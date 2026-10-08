//! An open document, live, held for one agent's interactions.
//!
//! [`Sessions`] is the table this holds: a plain `HashMap`, no lock, no
//! `Send` bound, because the value it stores -- a live [`XfaForm`] -- owns a
//! `boa_engine::Context` and is not `Send` at all. It is meant to be a local
//! inside [`crate::renderer::run`]'s loop, on the one dedicated worker
//! thread every request already funnels through (the font manager is
//! process-global, per that module's own doc), never a table shared across
//! an async runtime behind a `Mutex`.
//!
//! `u2s-mcp` already has a generic session store with the same open/read/
//! mutate/close shape (`u2s_mcp::session::SessionStore`); this does not
//! reuse it. Doing so would make this crate -- a synchronous rendering
//! library with no network or async dependency today -- pull in `tokio` and
//! `rmcp` for roughly fifty lines of hashmap-with-a-TTL logic, which is a
//! worse trade than the small, disclosed duplication below.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use u2s_render_core::RenderError;
use u2s_xfa::states::{Controls, controls_of_form};
use u2s_xfa::xfa::script_executor::LayoutScripts;
use u2s_xfa::xfa::scripting::{SomPath, XfaForm};
use u2s_xfa::{Fidelity, XfaNode, extract_xfa_packets};

use crate::renderer::Prepared;

const ENGINE: &str = "xfa";
const SESSION_TTL: Duration = Duration::from_secs(600);
const SESSION_CAP: usize = 32;

/// One field's value before and after an interaction -- what the form's own
/// scripts changed as a side effect, distinct from the field the caller
/// actually set.
#[derive(Debug, Clone, Serialize)]
pub struct FieldChange {
    pub field: String,
    pub from: String,
    pub to: String,
}

/// What a single interaction did, reported back rather than left for the
/// caller to infer from a re-render. `revision` is the session's new
/// revision after this call, to carry forward into the next one.
#[derive(Debug, Clone, Serialize)]
pub struct Interaction {
    pub revision: u64,
    pub field: String,
    pub value: String,
    pub values_changed: bool,
    /// Other fields the form's scripts changed as a consequence, excluding
    /// `field` itself.
    pub side_effects: Vec<FieldChange>,
    /// Controls that became visible as a consequence -- the only way an
    /// agent learns the form grew, since a control a script hides is
    /// unaddressable until it appears here or in a fresh `xfa_controls`.
    pub appeared: Vec<String>,
    pub disappeared: Vec<String>,
    pub page_count: u32,
    pub fidelity: Fidelity,
    pub warning: Option<String>,
}

/// One live form, mid-interaction.
struct LiveSession {
    /// The document's own nodes, as parsed, kept so `reset` can rebuild the
    /// form without re-reading the file or re-extracting its XFA packets.
    original_nodes: Vec<XfaNode>,
    form: XfaForm,
    /// The master-page engine from the same document-wide pass that seeded
    /// `form`, kept alive so every refresh can re-evaluate page-dependent
    /// master content against it -- see `XfaForm::refresh_paged`'s own doc
    /// for why `refresh` alone cannot do this. Absent only when this
    /// document's scripts could not be executed at all.
    layout: Option<LayoutScripts>,
    revision: u64,
    /// The one renderable view, at `revision`. Replaced wholesale by every
    /// interaction: no raster survives the revision it was drawn from.
    view: Arc<Prepared>,
    last_used: Instant,
}

fn packets_of(path: &Path) -> Result<(Vec<u8>, Vec<String>), RenderError> {
    let bytes = std::fs::read(path).map_err(|source| RenderError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let packets = extract_xfa_packets(&bytes)
        .map_err(|e| RenderError::UnsupportedInput {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?
        .ok_or_else(|| RenderError::UnsupportedInput {
            path: path.display().to_string(),
            detail: "not an XFA form — this engine can only render XFA".to_string(),
        })?;
    let names: Vec<String> = packets.iter().map(|p| p.name.clone()).collect();
    let xfa: Vec<u8> = packets.into_iter().flat_map(|p| p.content).collect();
    Ok((xfa, names))
}

/// Every visible, addressable control's field path -- what "a control
/// appeared" or "disappeared" is measured against.
fn visible_field_set(form: &mut XfaForm) -> Result<std::collections::HashSet<String>, RenderError> {
    let controls = controls_of_form(form).map_err(|e| RenderError::backend(ENGINE, e.to_string()))?;
    Ok(controls
        .controls
        .into_iter()
        .filter(|c| c.visible)
        .map(|c| c.field)
        .collect())
}

impl LiveSession {
    fn open(path: &Path) -> Result<Self, RenderError> {
        let (xfa, packet_names) = packets_of(path)?;
        let original_nodes = XfaNode::parse(&xfa).map_err(|e| RenderError::backend(ENGINE, e))?;

        let (mut form, mut layout) = XfaForm::new_with_layout(original_nodes.clone())
            .map_err(|e| RenderError::backend(ENGINE, e))?;

        // One view-producing path: even the opening view goes through the
        // paged refresh, so it never disagrees with what an interaction
        // later produces from the same engine.
        let (fidelity, warning) = refresh(&mut form, &mut layout)?;
        let view = Arc::new(view_of(&form, &fidelity, &warning, &packet_names));

        Ok(LiveSession {
            original_nodes,
            form,
            layout,
            revision: 0,
            view,
            last_used: Instant::now(),
        })
    }

    fn set(&mut self, field: &str, value: &str) -> Result<Interaction, RenderError> {
        let before_values = self.form.current_field_values();
        let before_visible = visible_field_set(&mut self.form)?;

        let result = self
            .form
            .interact(field, value)
            .map_err(|e| RenderError::backend(ENGINE, e))?;

        let (fidelity, warning) = refresh(&mut self.form, &mut self.layout)?;

        let after_values = self.form.current_field_values();
        let after_visible = visible_field_set(&mut self.form)?;

        let target = SomPath::new(field);
        let side_effects: Vec<FieldChange> = after_values
            .iter()
            // `current_field_values()` stores each field under more than one
            // alias (its full SOM path and its bare leaf name at least), all
            // pointing at the same value -- so excluding only the exact
            // string the caller passed would let the target's own change
            // back in under its other alias. Every alias of one field shares
            // its leaf name, which is what this actually excludes.
            .filter(|(path, _)| path.name() != target.name())
            .filter_map(|(path, to)| {
                let from = before_values.get(path).cloned().unwrap_or_default();
                (from != *to).then(|| FieldChange {
                    field: path.as_str().to_string(),
                    from,
                    to: to.clone(),
                })
            })
            .collect();

        let appeared: Vec<String> = after_visible.difference(&before_visible).cloned().collect();
        let disappeared: Vec<String> = before_visible.difference(&after_visible).cloned().collect();

        let packet_names = self.view.packets.clone();
        let view = view_of(&self.form, &fidelity, &warning, &packet_names);
        let page_count = crate::bands::bands(&view.flattened).len() as u32;

        self.revision += 1;
        self.view = Arc::new(view);
        self.last_used = Instant::now();

        Ok(Interaction {
            revision: self.revision,
            field: field.to_string(),
            value: value.to_string(),
            values_changed: result.values_changed,
            side_effects,
            appeared,
            disappeared,
            page_count,
            fidelity,
            warning,
        })
    }

    fn reset(&mut self) -> Result<Interaction, RenderError> {
        let before_visible = visible_field_set(&mut self.form)?;

        let (mut form, mut layout) = XfaForm::new_with_layout(self.original_nodes.clone())
            .map_err(|e| RenderError::backend(ENGINE, e))?;
        let (fidelity, warning) = refresh(&mut form, &mut layout)?;

        let after_visible = visible_field_set(&mut form)?;
        let appeared: Vec<String> = after_visible.difference(&before_visible).cloned().collect();
        let disappeared: Vec<String> = before_visible.difference(&after_visible).cloned().collect();

        let packet_names = self.view.packets.clone();
        let view = view_of(&form, &fidelity, &warning, &packet_names);
        let page_count = crate::bands::bands(&view.flattened).len() as u32;

        self.form = form;
        self.layout = layout;
        self.revision += 1;
        self.view = Arc::new(view);
        self.last_used = Instant::now();

        Ok(Interaction {
            revision: self.revision,
            field: String::new(),
            value: String::new(),
            values_changed: true,
            side_effects: Vec::new(),
            appeared,
            disappeared,
            page_count,
            fidelity,
            warning,
        })
    }

    fn controls(&mut self) -> Result<Controls, RenderError> {
        controls_of_form(&mut self.form).map_err(|e| RenderError::backend(ENGINE, e.to_string()))
    }
}

/// `refresh_paged` when this document's scripts actually ran (the ordinary
/// case), `refresh` otherwise -- there is no master-page engine to page
/// through if script execution itself failed, matching `prepare_default`'s
/// own fallback to level 2 rather than failing the whole open outright.
fn refresh(
    form: &mut XfaForm,
    layout: &mut Option<LayoutScripts>,
) -> Result<(Fidelity, Option<String>), RenderError> {
    match layout {
        Some(l) => {
            form.refresh_paged(l).map_err(|e| RenderError::backend(ENGINE, e))?;
            Ok((Fidelity::Scripts, None))
        }
        None => {
            form.refresh().map_err(|e| RenderError::backend(ENGINE, e))?;
            Ok((
                Fidelity::FormDom,
                Some(
                    "this form's scripts could not be executed; script-computed values are \
                     missing and script-hidden sections may still be visible"
                        .to_string(),
                ),
            ))
        }
    }
}

fn view_of(
    form: &XfaForm,
    fidelity: &Fidelity,
    warning: &Option<String>,
    packet_names: &[String],
) -> Prepared {
    let flattened = form.flattened().clone();
    let language = flattened.language.clone();
    Prepared::fresh(
        flattened,
        *fidelity,
        warning.clone(),
        packet_names.to_vec(),
        language,
    )
}

/// The table every live session lives in. See the module doc for why this
/// carries no lock and no `Send` bound.
pub(crate) struct Sessions {
    table: HashMap<String, LiveSession>,
}

impl Sessions {
    pub(crate) fn new() -> Self {
        Sessions {
            table: HashMap::new(),
        }
    }

    fn sweep(&mut self) {
        let now = Instant::now();
        self.table
            .retain(|_, s| now.duration_since(s.last_used) < SESSION_TTL);
    }

    fn get_mut(&mut self, handle: &str) -> Result<&mut LiveSession, RenderError> {
        self.sweep();
        self.table.get_mut(handle).ok_or_else(|| {
            RenderError::invalid_argument(
                "session",
                format!(
                    "{handle:?} is not open; it may never have existed or may have gone idle \
                     too long — open a new one and replay your interactions"
                ),
            )
        })
    }

    fn check_revision(current: u64, asked: u64, handle: &str) -> Result<(), RenderError> {
        if asked == current {
            return Ok(());
        }
        let relation = if asked < current { "behind" } else { "ahead of" };
        Err(RenderError::invalid_argument(
            "revision",
            format!(
                "session {handle:?} is at revision {current}; you asked for {asked}, which is \
                 {relation} it — use revision {current}"
            ),
        ))
    }

    pub(crate) fn open(&mut self, path: &Path) -> Result<(String, u64, Arc<Prepared>), RenderError> {
        self.sweep();
        if self.table.len() >= SESSION_CAP {
            return Err(RenderError::invalid_argument(
                "session",
                format!(
                    "{SESSION_CAP} sessions are already open in this server; close one you \
                     have finished with before opening another"
                ),
            ));
        }
        let session = LiveSession::open(path)?;
        let handle = format!("sess_{}", uuid_like());
        let view = Arc::clone(&session.view);
        self.table.insert(handle.clone(), session);
        Ok((handle, 0, view))
    }

    pub(crate) fn view(&mut self, handle: &str, revision: u64) -> Result<Arc<Prepared>, RenderError> {
        let session = self.get_mut(handle)?;
        Self::check_revision(session.revision, revision, handle)?;
        session.last_used = Instant::now();
        Ok(Arc::clone(&session.view))
    }

    pub(crate) fn controls(&mut self, handle: &str, revision: u64) -> Result<Controls, RenderError> {
        let session = self.get_mut(handle)?;
        Self::check_revision(session.revision, revision, handle)?;
        session.controls()
    }

    pub(crate) fn set(
        &mut self,
        handle: &str,
        expected_revision: u64,
        field: &str,
        value: &str,
    ) -> Result<(Arc<Prepared>, Interaction), RenderError> {
        let session = self.get_mut(handle)?;
        Self::check_revision(session.revision, expected_revision, handle)?;
        let interaction = session.set(field, value)?;
        Ok((Arc::clone(&session.view), interaction))
    }

    pub(crate) fn reset(
        &mut self,
        handle: &str,
        expected_revision: u64,
    ) -> Result<(Arc<Prepared>, Interaction), RenderError> {
        let session = self.get_mut(handle)?;
        Self::check_revision(session.revision, expected_revision, handle)?;
        let interaction = session.reset()?;
        Ok((Arc::clone(&session.view), interaction))
    }

    pub(crate) fn close(&mut self, handle: &str) -> Result<(), RenderError> {
        self.sweep();
        self.table.remove(handle).map(|_| ()).ok_or_else(|| {
            RenderError::invalid_argument(
                "session",
                format!("{handle:?} is not open; nothing to close"),
            )
        })
    }
}

/// An unguessable, unique-enough handle suffix. Not a cryptographic
/// requirement -- the handle only ever appears in one run's transcript, and
/// this server has no route that lets a caller enumerate live handles -- so
/// a wide random integer is enough, without pulling in a UUID dependency
/// this crate otherwise has no use for.
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{time:016x}{counter:08x}")
}
