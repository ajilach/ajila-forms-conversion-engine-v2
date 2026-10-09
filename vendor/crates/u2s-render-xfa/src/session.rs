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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use u2s_render_core::RenderError;
use u2s_xfa::flattened::FieldAccess;
use u2s_xfa::states::{Control, Controls, access_by_field, controls_of_form, drawn_field_set, field_of_form};
use u2s_xfa::xfa::script_executor::LayoutScripts;
use u2s_xfa::xfa::scripting::{EventResult, SomPath, XfaForm};
use u2s_xfa::{Fidelity, XfaNode, extract_xfa_packets};

use crate::renderer::Prepared;

const ENGINE: &str = "xfa";
const SESSION_TTL: Duration = Duration::from_secs(600);
const SESSION_CAP: usize = 32;

/// One field's value before and after an interaction -- what the form's own
/// scripts changed as a side effect, distinct from the field the caller
/// actually set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldChange {
    pub field: String,
    pub from: String,
    pub to: String,
}

/// One field's effective access before and after an interaction: a field the
/// form's own scripts locked or unlocked (XFA 3.3 §17 lets a script assign
/// `access`), the way AAGS locks its sheet number once a radio button
/// prefills it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccessChange {
    pub field: String,
    pub from: FieldAccess,
    pub to: FieldAccess,
}

/// Which interaction produced an [`Interaction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    Set,
    Click,
    Reset,
}

/// What a single interaction did, reported back rather than left for the
/// caller to infer from a re-render. `revision` is the session's new
/// revision after this call, to carry forward into the next one.
#[derive(Debug, Clone, Serialize)]
pub struct Interaction {
    pub revision: u64,
    pub kind: InteractionKind,
    /// The field set or pressed; empty for a reset.
    pub field: String,
    /// The value set; empty for a click or a reset.
    pub value: String,
    pub values_changed: bool,
    /// Other fields the form's scripts changed as a consequence, excluding
    /// `field` itself.
    pub side_effects: Vec<FieldChange>,
    /// Fields that became drawn as a consequence, sorted -- the way an agent
    /// learns the form grew: a new instance of a repeated section shows up
    /// here as every field in it (`form1.Body.Row[1].Amount`), and a field a
    /// script reveals shows up the same way.
    pub appeared: Vec<String>,
    pub disappeared: Vec<String>,
    /// Fields whose effective access changed as a consequence, sorted by
    /// field: locked by a script (`open` to `protected`, say) or unlocked.
    /// Only fields present both before and after are compared; a field that
    /// appeared is reported with its access by `xfa_controls`.
    pub access_changes: Vec<AccessChange>,
    /// Whether an instance of a repeatable section was added, removed or
    /// moved (XFA 3.3 §9).
    pub instances_changed: bool,
    pub page_count: u32,
    pub fidelity: Fidelity,
    /// Why the render is incomplete, and/or why a press did nothing (a
    /// repeatable section already at its `<occur>` limit), joined by "; ".
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

/// The form as an interaction finds or leaves it: every field's value, every
/// drawn field, and every field's effective access.
struct Snapshot {
    values: HashMap<SomPath, String>,
    drawn: BTreeSet<String>,
    access: BTreeMap<String, FieldAccess>,
}

impl Snapshot {
    fn of(form: &mut XfaForm) -> Snapshot {
        Snapshot {
            values: form.canonical_field_values(),
            drawn: drawn_field_set(form.flattened()),
            access: access_by_field(form),
        }
    }
}

/// Fields whose access differs between two snapshots, in field order. A
/// field missing from either side (a new or removed instance) is not a
/// change of access.
fn access_diff(before: &Snapshot, after: &Snapshot) -> Vec<AccessChange> {
    after
        .access
        .iter()
        .filter_map(|(field, to)| {
            let from = before.access.get(field)?;
            (from != to).then(|| AccessChange {
                field: field.clone(),
                from: *from,
                to: *to,
            })
        })
        .collect()
}

/// What changed between two snapshots: values other than `target`'s, and the
/// fields that started or stopped being drawn. Snapshots hold one entry per
/// field, under its canonical path, so a change is reported once and one
/// instance's change never hides behind another's.
fn diff(
    before: &Snapshot,
    after: &Snapshot,
    target: Option<&str>,
) -> (Vec<FieldChange>, Vec<String>, Vec<String>) {
    let mut side_effects: Vec<FieldChange> = after
        .values
        .iter()
        .filter(|(path, _)| Some(path.as_str()) != target)
        .filter_map(|(path, to)| {
            let from = before.values.get(path).cloned().unwrap_or_default();
            (from != *to).then(|| FieldChange {
                field: path.as_str().to_string(),
                from,
                to: to.clone(),
            })
        })
        .collect();
    side_effects.sort_by(|a, b| a.field.cmp(&b.field));
    let appeared = after.drawn.difference(&before.drawn).cloned().collect();
    let disappeared = before.drawn.difference(&after.drawn).cloned().collect();
    (side_effects, appeared, disappeared)
}

/// `a` and `b` joined by "; ", whichever are present.
fn join_warnings(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, b) => a.or(b),
    }
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

    /// Refuse to address a field the wrong way: a button is pressed, not
    /// set, and anything else is set, not pressed.
    fn check_field(&self, field: &str, kind: InteractionKind) -> Result<(), RenderError> {
        let Some(node) = self.form.resolve(field) else {
            return Err(RenderError::invalid_argument(
                "field",
                format!("no field {field} on this form; call xfa_controls for the available ones"),
            ));
        };
        let is_button = node.is_button();
        // Refused before anything runs, as an argument error naming why: the
        // form would refuse the same thing (`XfaForm::interact` and `click`
        // check it too), but a caller addressing a locked field made a
        // mistake about the form, not hit a failure of the engine.
        if let Some(refusal) = self.form.access_refusal(field) {
            return Err(RenderError::invalid_argument("field", refusal));
        }
        match (kind, is_button) {
            (InteractionKind::Set, true) => Err(RenderError::invalid_argument(
                "field",
                format!("{field} is a button; it has no value to set. Press it with xfa_click"),
            )),
            (InteractionKind::Click, false) => Err(RenderError::invalid_argument(
                "field",
                format!("{field} is not a button; set it to a value with xfa_set"),
            )),
            _ => Ok(()),
        }
    }

    fn set(&mut self, field: &str, value: &str) -> Result<Interaction, RenderError> {
        self.check_field(field, InteractionKind::Set)?;
        self.interaction_around(InteractionKind::Set, field, value, |form| {
            form.interact(field, value)
        })
    }

    fn click(&mut self, field: &str) -> Result<Interaction, RenderError> {
        self.check_field(field, InteractionKind::Click)?;
        self.interaction_around(InteractionKind::Click, field, "", |form| form.click(field))
    }

    /// Run one interaction `op` on the form, re-lay it out, and report what
    /// changed.
    fn interaction_around(
        &mut self,
        kind: InteractionKind,
        field: &str,
        value: &str,
        op: impl FnOnce(&mut XfaForm) -> Result<EventResult, String>,
    ) -> Result<Interaction, RenderError> {
        let before = Snapshot::of(&mut self.form);
        let result = op(&mut self.form).map_err(|e| RenderError::backend(ENGINE, e))?;
        let (fidelity, warning) = refresh(&mut self.form, &mut self.layout)?;
        Ok(self.finish(kind, field, value, before, result, fidelity, warning))
    }

    fn reset(&mut self) -> Result<Interaction, RenderError> {
        let before = Snapshot::of(&mut self.form);

        let (mut form, mut layout) = XfaForm::new_with_layout(self.original_nodes.clone())
            .map_err(|e| RenderError::backend(ENGINE, e))?;
        let (fidelity, warning) = refresh(&mut form, &mut layout)?;
        self.form = form;
        self.layout = layout;

        let result = EventResult {
            values_changed: true,
            ..EventResult::default()
        };
        let mut interaction = self.finish(
            InteractionKind::Reset,
            "",
            "",
            before,
            result,
            fidelity,
            warning,
        );
        // A reset rebuilds the form from the template, so any instance added
        // or removed since opening is undone.
        interaction.instances_changed = interaction
            .appeared
            .iter()
            .chain(&interaction.disappeared)
            .any(|p| p.contains('['));
        Ok(interaction)
    }

    /// The common tail of every interaction: diff against `before`, install
    /// the new view, bump the revision.
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        kind: InteractionKind,
        field: &str,
        value: &str,
        before: Snapshot,
        result: EventResult,
        fidelity: Fidelity,
        warning: Option<String>,
    ) -> Interaction {
        let after = Snapshot::of(&mut self.form);
        let target = (!field.is_empty()).then_some(field);
        let (side_effects, appeared, disappeared) = diff(&before, &after, target);
        let access_changes = access_diff(&before, &after);

        let packet_names = self.view.packets.clone();
        let view = view_of(&self.form, &fidelity, &warning, &packet_names);
        let page_count = crate::bands::bands(&view.flattened).len() as u32;

        self.revision += 1;
        self.view = Arc::new(view);
        self.last_used = Instant::now();

        Interaction {
            revision: self.revision,
            kind,
            field: field.to_string(),
            value: value.to_string(),
            values_changed: result.values_changed,
            side_effects,
            appeared,
            disappeared,
            access_changes,
            instances_changed: result.instances_changed,
            page_count,
            fidelity,
            warning: join_warnings(warning, result.instance_limit_hit),
        }
    }

    fn controls(&mut self) -> Result<Controls, RenderError> {
        controls_of_form(&mut self.form).map_err(|e| RenderError::backend(ENGINE, e.to_string()))
    }

    fn field(&mut self, field: &str) -> Result<Option<Control>, RenderError> {
        field_of_form(&mut self.form, field).map_err(|e| RenderError::backend(ENGINE, e.to_string()))
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

    pub(crate) fn field(
        &mut self,
        handle: &str,
        revision: u64,
        field: &str,
    ) -> Result<Option<Control>, RenderError> {
        let session = self.get_mut(handle)?;
        Self::check_revision(session.revision, revision, handle)?;
        session.field(field)
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

    pub(crate) fn click(
        &mut self,
        handle: &str,
        expected_revision: u64,
        field: &str,
    ) -> Result<(Arc<Prepared>, Interaction), RenderError> {
        let session = self.get_mut(handle)?;
        Self::check_revision(session.revision, expected_revision, handle)?;
        let interaction = session.click(field)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(values: &[(&str, &str)], drawn: &[&str]) -> Snapshot {
        Snapshot {
            values: values
                .iter()
                .map(|(k, v)| (SomPath::new(*k), v.to_string()))
                .collect(),
            drawn: drawn.iter().map(|s| s.to_string()).collect(),
            access: BTreeMap::new(),
        }
    }

    fn with_access(mut snapshot: Snapshot, access: &[(&str, FieldAccess)]) -> Snapshot {
        snapshot.access = access.iter().map(|(f, a)| (f.to_string(), *a)).collect();
        snapshot
    }

    #[test]
    fn access_diff_reports_locks_and_unlocks_of_fields_on_both_sides() {
        let before = with_access(
            snapshot(&[], &[]),
            &[
                ("f.Sheet", FieldAccess::Open),
                ("f.Name", FieldAccess::ReadOnly),
                ("f.Same", FieldAccess::Protected),
                ("f.Gone", FieldAccess::Open),
            ],
        );
        let after = with_access(
            snapshot(&[], &[]),
            &[
                ("f.Sheet", FieldAccess::Protected),
                ("f.Name", FieldAccess::Open),
                ("f.Same", FieldAccess::Protected),
                ("f.Row[1].New", FieldAccess::Protected),
            ],
        );
        assert_eq!(
            access_diff(&before, &after),
            vec![
                AccessChange {
                    field: "f.Name".into(),
                    from: FieldAccess::ReadOnly,
                    to: FieldAccess::Open,
                },
                AccessChange {
                    field: "f.Sheet".into(),
                    from: FieldAccess::Open,
                    to: FieldAccess::Protected,
                },
            ]
        );
    }

    #[test]
    fn diff_excludes_exactly_the_target() {
        let before = snapshot(
            &[
                ("f.Row.Amount", "1"),
                ("f.Row[1].Amount", "1"),
                ("f.Total", "2"),
            ],
            &["f.Row.Amount"],
        );
        let after = snapshot(
            &[
                ("f.Row.Amount", "1"),
                ("f.Row[1].Amount", "5"),
                ("f.Total", "6"),
            ],
            &["f.Row.Amount", "f.Row[1].Amount", "f.Row[1].Remove"],
        );
        let (side_effects, appeared, disappeared) = diff(&before, &after, Some("f.Row[1].Amount"));
        // The target's own change is not a side effect; the other row's
        // same-named field would still be reported if it had changed.
        assert_eq!(
            side_effects,
            vec![FieldChange {
                field: "f.Total".into(),
                from: "2".into(),
                to: "6".into()
            }]
        );
        assert_eq!(appeared, ["f.Row[1].Amount", "f.Row[1].Remove"]);
        assert!(disappeared.is_empty());

        let (side_effects, _, disappeared) = diff(&after, &before, None);
        assert_eq!(side_effects.len(), 2, "{side_effects:?}");
        assert_eq!(disappeared, ["f.Row[1].Amount", "f.Row[1].Remove"]);
    }

    #[test]
    fn warnings_join_with_a_semicolon() {
        assert_eq!(join_warnings(None, None), None);
        assert_eq!(join_warnings(Some("a".into()), None).as_deref(), Some("a"));
        assert_eq!(join_warnings(None, Some("b".into())).as_deref(), Some("b"));
        assert_eq!(
            join_warnings(Some("a".into()), Some("b".into())).as_deref(),
            Some("a; b")
        );
    }
}
