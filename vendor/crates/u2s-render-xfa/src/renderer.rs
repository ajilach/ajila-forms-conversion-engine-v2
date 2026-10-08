//! The renderer facade: one worker thread, a cache, and the five operations.
//!
//! Same architecture as the PDF renderer and for a related reason. There the
//! constraint was that pdfium is not thread-safe; here it is that the font
//! manager is process-global mutable state, so concurrent rendering of
//! documents with different fonts would cross-contaminate. Serializing on one
//! thread makes that impossible by construction rather than by discipline.
//!
//! ## Memory
//!
//! Two caches live on the worker, both bounded and both env-tunable:
//!
//! * `DOC_CACHE_SIZE` (default 4) — how many *prepared* documents (parsed,
//!   scripted, laid out — no pixels) are held at once, per `(path, state)`.
//!   Cheap relative to rasters: a `Flattened` tree, not an image.
//! * `RASTER_CACHE_SIZE` (default 2, per document) — how many *rasterized
//!   columns* one prepared document holds, keyed by scale. This is the
//!   expensive one: an RGBA buffer covering every page stacked into one
//!   column. See [`raster_cache_size`] for the worked-out ceiling.
//!
//! Neither cache is reconfigurable per call — both are read once, from the
//! environment, the first time the worker thread spawns — so set them before
//! the first request if the defaults do not fit a deployment's memory budget.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

use image::RgbaImage;
use u2s_render_core::encode::encode;
use u2s_render_core::{
    BatchLimits, Grep, ImageFormat, Limits, PageText, Pattern, RectPt, RenderError, RenderedPage,
    RenderedPages, TextMatch, TextSearch, render_batch, window_chars,
};
use u2s_xfa::states::{StateSpec, materialize};
use u2s_xfa::{Fidelity, Flattened, XfaNode, extract_xfa_packets, prepare_default};

use crate::bands::{Band, bands, geometry, max_page_height};
use crate::text::{page_text, page_texts};
use crate::types::{DocumentInfo, DocumentKind};

const ENGINE: &str = "xfa";

/// What the caller should do instead when this is not an XFA document.
const NOT_XFA: &str = "not an XFA form — this engine can only render XFA; use the PDF renderer (pdf_info, \
     pdf_render_page) for this document";

/// A document's prepared layout, cached: reaching it costs an XML parse, a
/// JavaScript run and a full layout pass, and every operation needs it.
///
/// `pub(crate)` (fields included) because [`crate::session`] builds one
/// afresh after every interaction -- a session's view is not looked up by
/// [`DocKey`] the way a plain `doc_path` render is, but it is exactly the
/// same shape of thing once built, and every render/text/search function
/// below takes it by reference regardless of which path produced it.
pub(crate) struct Prepared {
    pub(crate) flattened: Flattened,
    pub(crate) fidelity: Fidelity,
    pub(crate) warning: Option<String>,
    pub(crate) packets: Vec<String>,
    pub(crate) language: String,
    /// The rasterized tall column, cached by scale (as bits, so it can be a
    /// plain map key). Rasterizing is the expensive step; cropping a page out
    /// of an already-rendered column is cheap. Bounded to
    /// `RASTER_CACHE_SIZE` entries (default 2) so a caller alternating between
    /// two resolutions — a thumbnail pass and a detail pass, say — does not
    /// keep re-rendering either one.
    pub(crate) rasters: Mutex<Vec<(u32, Arc<RgbaImage>)>>,
    /// How many times this document (in this state) has actually been
    /// rasterized. Lives on the document rather than as one process-wide
    /// counter, so a test asserting on it is not racing every other test that
    /// happens to run concurrently and touch a *different* document.
    pub(crate) raster_count: Mutex<u64>,
}

impl Prepared {
    /// A session's view, freshly built after an interaction: no raster is
    /// carried over from the revision it replaces, since a stale raster must
    /// never be servable once the layout it was drawn from is gone.
    pub(crate) fn fresh(
        flattened: Flattened,
        fidelity: Fidelity,
        warning: Option<String>,
        packets: Vec<String>,
        language: String,
    ) -> Self {
        Prepared {
            flattened,
            fidelity,
            warning,
            packets,
            language,
            rasters: Mutex::new(Vec::new()),
            raster_count: Mutex::new(0),
        }
    }
}

/// Rasters per document, bounded — see the module doc for the full memory
/// picture. `RASTER_CACHE_SIZE` (default 2) times `DOC_CACHE_SIZE` (default 4)
/// times an RGBA column is the ceiling on this cache alone: an 8-page A4 form
/// at scale 2 is roughly 8 * 595 * 842 * 4 * 4 bytes ~= 64 MB per rasterized
/// column, so the default configuration bounds this cache at roughly 8 * 64 MB
/// = 512 MB in the worst case (every cached document large and at its largest
/// cached scale). Lower both for a memory-constrained deployment; each is read
/// once, at first use, so set them before the first request.
fn raster_cache_size() -> usize {
    std::env::var("RASTER_CACHE_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2)
}

/// A prepared layout is identified by the document *and* the state it is in.
/// The state part is the canonical key, so selections written in a different
/// order address the same cache entry rather than doing the work twice.
#[derive(PartialEq, Eq, Hash, Clone, Debug)]
struct DocKey {
    path: PathBuf,
    len: u64,
    mtime: Option<std::time::SystemTime>,
    state: String,
}

impl DocKey {
    fn of(path: &Path, state: &StateSpec) -> Result<Self, RenderError> {
        let meta = std::fs::metadata(path).map_err(|source| RenderError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Ok(DocKey {
            path: path.to_path_buf(),
            len: meta.len(),
            mtime: meta.modified().ok(),
            state: state.key(),
        })
    }
}

/// What one call addresses: a document on disk in a requested state, or one
/// frozen revision of a session opened earlier with [`Renderer::open`].
///
/// Additive, not a replacement: a `doc_path`-addressed render is still what
/// the normalizer's stateless probes and a single-call conformance vector
/// use, and it keeps going through the `(path, state)`-keyed [`DocKey`]
/// cache exactly as before. A session-addressed read goes through
/// [`crate::session::Sessions`] instead, whose cache key is the session
/// itself: every interaction replaces its view wholesale, so there is
/// nothing to key by state at all.
#[derive(Clone)]
pub enum Target {
    Doc { path: PathBuf, state: StateSpec },
    View { handle: String, revision: u64 },
}

impl Target {
    pub fn doc(path: impl AsRef<Path>, state: &StateSpec) -> Self {
        Target::Doc {
            path: path.as_ref().to_path_buf(),
            state: state.clone(),
        }
    }

    pub fn view(handle: impl Into<String>, revision: u64) -> Self {
        Target::View {
            handle: handle.into(),
            revision,
        }
    }
}

enum Job {
    Info {
        target: Target,
        limits: Limits,
        reply: Sender<Result<DocumentInfo, RenderError>>,
    },
    RenderPages {
        target: Target,
        pages: Vec<u32>,
        dpi: Option<f32>,
        max_edge: Option<u32>,
        format: ImageFormat,
        batch: Option<BatchLimits>,
        limits: Limits,
        reply: Sender<Result<RenderedPages, RenderError>>,
    },
    RenderRegion {
        target: Target,
        page: u32,
        rect: RectPt,
        dpi: Option<f32>,
        format: ImageFormat,
        limits: Limits,
        reply: Sender<Result<RenderedPage, RenderError>>,
    },
    PageText {
        target: Target,
        page: u32,
        offset: usize,
        limit: usize,
        reply: Sender<Result<PageText, RenderError>>,
    },
    SearchText {
        target: Target,
        from: u32,
        pattern: Pattern,
        limit: usize,
        limits: Limits,
        reply: Sender<Result<TextSearch, RenderError>>,
    },
    RasterCount {
        target: Target,
        reply: Sender<Result<u64, RenderError>>,
    },
    Controls {
        target: Target,
        reply: Sender<Result<u2s_xfa::states::Controls, RenderError>>,
    },
    Open {
        path: PathBuf,
        reply: Sender<Result<OpenedSession, RenderError>>,
    },
    Set {
        handle: String,
        expected_revision: u64,
        field: String,
        value: String,
        reply: Sender<Result<InteractionOutcome, RenderError>>,
    },
    Reset {
        handle: String,
        expected_revision: u64,
        reply: Sender<Result<InteractionOutcome, RenderError>>,
    },
    Close {
        handle: String,
        reply: Sender<Result<(), RenderError>>,
    },
}

type Prepares = Arc<Mutex<HashMap<PathBuf, u64>>>;

/// A freshly opened session: its handle, starting revision (always 0), and
/// the view at that revision.
type OpenedSession = (String, u64, Arc<Prepared>);

/// What an interaction (`xfa_set` or `xfa_reset`) leaves behind: the
/// session's new view, and the report of what happened.
type InteractionOutcome = (Arc<Prepared>, crate::session::Interaction);

static WORKER: OnceLock<WorkerHandle> = OnceLock::new();

#[derive(Clone)]
struct WorkerHandle {
    tx: Sender<Job>,
    prepares: Prepares,
}

fn worker() -> WorkerHandle {
    WORKER
        .get_or_init(|| {
            let (tx, rx) = channel::<Job>();
            let prepares: Prepares = Arc::new(Mutex::new(HashMap::new()));
            let p = Arc::clone(&prepares);
            std::thread::Builder::new()
                .name("xfa-render".into())
                .spawn(move || run(rx, p))
                .expect("spawn xfa worker");
            WorkerHandle { tx, prepares }
        })
        .clone()
}

/// Handle onto the shared worker. Cheap to clone.
#[derive(Clone)]
pub struct Renderer {
    handle: WorkerHandle,
    limits: Limits,
}

impl Renderer {
    /// Fonts must already be registered — see `u2s_xfa::fonts`. Without them
    /// layout degrades silently into wrong page breaks, so a caller that has
    /// not registered any should refuse to start rather than call this.
    pub fn new(limits: Limits) -> Self {
        Renderer {
            handle: worker(),
            limits,
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// How many times this document (in the default state) has actually been
    /// rasterized. Proves the raster cache elides the expensive step of a
    /// cursor walk, not just the parse.
    pub fn raster_count_for(&self, path: impl AsRef<Path>) -> Result<u64, RenderError> {
        self.raster_count_for_state(path, &StateSpec::default())
    }

    /// As `raster_count_for`, for a specific state.
    pub fn raster_count_for_state(
        &self,
        path: impl AsRef<Path>,
        state: &StateSpec,
    ) -> Result<u64, RenderError> {
        let (reply, rx) = channel();
        self.submit(
            Job::RasterCount {
                target: Target::doc(path, state),
                reply,
            },
            rx,
        )
    }

    /// Every interactive control of a document addressed by `target`, as it
    /// stands: the document's default controls for [`Target::Doc`], or the
    /// controls of a live session as its interactions have left them for
    /// [`Target::View`].
    pub fn controls(&self, target: &Target) -> Result<u2s_xfa::states::Controls, RenderError> {
        let (reply, rx) = channel();
        self.submit(
            Job::Controls {
                target: target.clone(),
                reply,
            },
            rx,
        )
    }

    /// Open a session on this document, returning its handle, its starting
    /// revision (always 0) and the view at that revision.
    pub fn open(&self, path: impl AsRef<Path>) -> Result<(String, u64, DocumentInfo), RenderError> {
        let (reply, rx) = channel();
        let (handle, revision, view) = self.submit(
            Job::Open {
                path: path.as_ref().to_path_buf(),
                reply,
            },
            rx,
        )?;
        Ok((handle, revision, document_info(&view, &self.limits)))
    }

    /// Set one field the way a person would: focus in, change, focus out --
    /// see [`u2s_xfa::xfa::scripting::XfaForm::interact`]. `expected_revision`
    /// must be the session's current revision; a stale or future one is
    /// refused, naming the current one.
    pub fn set(
        &self,
        handle: impl Into<String>,
        expected_revision: u64,
        field: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<crate::session::Interaction, RenderError> {
        let (reply, rx) = channel();
        let (_, interaction) = self.submit(
            Job::Set {
                handle: handle.into(),
                expected_revision,
                field: field.into(),
                value: value.into(),
                reply,
            },
            rx,
        )?;
        Ok(interaction)
    }

    /// Put a session's form back the way it opened, keeping the session and
    /// its handle.
    pub fn reset(
        &self,
        handle: impl Into<String>,
        expected_revision: u64,
    ) -> Result<crate::session::Interaction, RenderError> {
        let (reply, rx) = channel();
        let (_, interaction) = self.submit(
            Job::Reset {
                handle: handle.into(),
                expected_revision,
                reply,
            },
            rx,
        )?;
        Ok(interaction)
    }

    /// Release a session. Not required before ending a run: an unused
    /// session is reclaimed on its own after its idle TTL.
    pub fn close(&self, handle: impl Into<String>) -> Result<(), RenderError> {
        let (reply, rx) = channel();
        self.submit(
            Job::Close {
                handle: handle.into(),
                reply,
            },
            rx,
        )
    }

    /// How many times a document has been prepared — parsed, scripted and laid
    /// out. Proves the cache elides the expensive work.
    pub fn prepare_count_for(&self, path: impl AsRef<Path>) -> u64 {
        self.handle
            .prepares
            .lock()
            .map(|m| m.get(path.as_ref()).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    fn submit<T>(&self, job: Job, rx: Receiver<Result<T, RenderError>>) -> Result<T, RenderError> {
        self.handle
            .tx
            .send(job)
            .map_err(|_| RenderError::Worker("xfa worker is gone".into()))?;
        rx.recv()
            .map_err(|_| RenderError::Worker("xfa worker died mid-request".into()))?
    }

    pub fn info(&self, target: &Target) -> Result<DocumentInfo, RenderError> {
        let (reply, rx) = channel();
        self.submit(
            Job::Info {
                target: target.clone(),
                limits: self.limits.clone(),
                reply,
            },
            rx,
        )
    }

    pub fn render_page(
        &self,
        target: &Target,
        page: u32,
        dpi: Option<f32>,
        max_edge: Option<u32>,
        format: ImageFormat,
    ) -> Result<(RenderedPage, Option<String>), RenderError> {
        let (reply, rx) = channel();
        let out = self.submit(
            Job::RenderPages {
                target: target.clone(),
                pages: vec![page],
                dpi,
                max_edge,
                format,
                batch: None,
                limits: self.limits.clone(),
                reply,
            },
            rx,
        )?;
        let warning = out.warning.clone();
        out.pages
            .into_iter()
            .next()
            .map(|p| (p, warning))
            .ok_or_else(|| RenderError::backend(ENGINE, "renderer returned no page"))
    }

    // The parameters mirror the tool schema one-for-one and are mutually
    // independent; bundling them into a struct would only move the width.
    #[allow(clippy::too_many_arguments)]
    pub fn render_pages(
        &self,
        target: &Target,
        pages: Option<Vec<u32>>,
        from: Option<u32>,
        limit: Option<usize>,
        dpi: Option<f32>,
        max_edge: Option<u32>,
        format: ImageFormat,
    ) -> Result<RenderedPages, RenderError> {
        let page_count = self.info(target)?.page_count;

        let requested: Vec<u32> = match pages {
            Some(explicit) => explicit,
            None => (from.unwrap_or(1).max(1)..=page_count).collect(),
        };
        for &p in &requested {
            if p == 0 || p > page_count {
                return Err(RenderError::PageOutOfRange {
                    page: p,
                    page_count,
                });
            }
        }

        let max_images = limit
            .unwrap_or(self.limits.max_images_per_call)
            .clamp(1, self.limits.max_images_per_call);

        let (reply, rx) = channel();
        self.submit(
            Job::RenderPages {
                target: target.clone(),
                pages: requested,
                dpi,
                max_edge,
                format,
                batch: Some(BatchLimits {
                    max_images,
                    max_bytes: self.limits.max_response_bytes,
                }),
                limits: self.limits.clone(),
                reply,
            },
            rx,
        )
    }

    pub fn render_region(
        &self,
        target: &Target,
        page: u32,
        rect: RectPt,
        dpi: Option<f32>,
        format: ImageFormat,
    ) -> Result<RenderedPage, RenderError> {
        let (reply, rx) = channel();
        self.submit(
            Job::RenderRegion {
                target: target.clone(),
                page,
                rect,
                dpi,
                format,
                limits: self.limits.clone(),
                reply,
            },
            rx,
        )
    }

    pub fn page_text(
        &self,
        target: &Target,
        page: u32,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<PageText, RenderError> {
        let limit = limit
            .unwrap_or(self.limits.text_limit_default)
            .min(self.limits.text_limit_max);
        let (reply, rx) = channel();
        self.submit(
            Job::PageText {
                target: target.clone(),
                page,
                offset: offset.unwrap_or(0),
                limit,
                reply,
            },
            rx,
        )
    }

    /// Search the text of pages `from` onwards. `pattern` is compiled by the
    /// caller, at its own edge, so a bad query never reaches the worker.
    ///
    /// Scans at most `Limits::search_max_pages` pages and returns a cursor for
    /// the rest; a page is always scanned whole, so the walk reports every
    /// match exactly once.
    pub fn search_text(
        &self,
        target: &Target,
        from: Option<u32>,
        pattern: Pattern,
        limit: Option<usize>,
    ) -> Result<TextSearch, RenderError> {
        let limit = limit
            .unwrap_or(self.limits.search_limit_default)
            .clamp(1, self.limits.search_limit_max);
        let (reply, rx) = channel();
        self.submit(
            Job::SearchText {
                target: target.clone(),
                from: from.unwrap_or(1).max(1),
                pattern,
                limit,
                limits: self.limits.clone(),
                reply,
            },
            rx,
        )
    }
}

fn run(rx: Receiver<Job>, prepares: Prepares) {
    let mut cache: HashMap<DocKey, Arc<Prepared>> = HashMap::new();
    let mut order: Vec<DocKey> = Vec::new();
    let mut sessions = crate::session::Sessions::new();
    let cache_size = std::env::var("DOC_CACHE_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4usize);

    while let Ok(job) = rx.recv() {
        match job {
            Job::Info {
                target,
                limits,
                reply,
            } => {
                let r = resolve(&target, &mut cache, &mut order, cache_size, &prepares, &mut sessions)
                    .map(|p| document_info(&p, &limits));
                let _ = reply.send(r);
            }
            Job::RenderPages {
                target,
                pages,
                dpi,
                max_edge,
                format,
                batch,
                limits,
                reply,
            } => {
                let r = resolve(&target, &mut cache, &mut order, cache_size, &prepares, &mut sessions)
                    .and_then(|p| {
                        do_render_pages(&p, &pages, dpi, max_edge, format, batch, &limits)
                    });
                let _ = reply.send(r);
            }
            Job::RenderRegion {
                target,
                page,
                rect,
                dpi,
                format,
                limits,
                reply,
            } => {
                let r = resolve(&target, &mut cache, &mut order, cache_size, &prepares, &mut sessions)
                    .and_then(|p| do_render_region(&p, page, rect, dpi, format, &limits));
                let _ = reply.send(r);
            }
            Job::PageText {
                target,
                page,
                offset,
                limit,
                reply,
            } => {
                let r = resolve(&target, &mut cache, &mut order, cache_size, &prepares, &mut sessions)
                    .and_then(|p| do_page_text(&p, page, offset, limit));
                let _ = reply.send(r);
            }
            Job::SearchText {
                target,
                from,
                pattern,
                limit,
                limits,
                reply,
            } => {
                let r = resolve(&target, &mut cache, &mut order, cache_size, &prepares, &mut sessions)
                    .and_then(|p| do_search_text(&p, from, pattern, limit, &limits));
                let _ = reply.send(r);
            }
            Job::RasterCount { target, reply } => {
                let r = resolve(&target, &mut cache, &mut order, cache_size, &prepares, &mut sessions)
                    .map(|p| *p.raster_count.lock().expect("raster_count lock"));
                let _ = reply.send(r);
            }
            Job::Controls { target, reply } => {
                let r = match &target {
                    Target::Doc { path, .. } => nodes_of(path).and_then(|(nodes, _names)| {
                        u2s_xfa::states::controls(&nodes)
                            .map_err(|e| RenderError::backend(ENGINE, e.to_string()))
                    }),
                    Target::View { handle, revision } => sessions.controls(handle, *revision),
                };
                let _ = reply.send(r);
            }
            Job::Open { path, reply } => {
                let r = sessions.open(&path);
                let _ = reply.send(r);
            }
            Job::Set {
                handle,
                expected_revision,
                field,
                value,
                reply,
            } => {
                let r = sessions.set(&handle, expected_revision, &field, &value);
                let _ = reply.send(r);
            }
            Job::Reset {
                handle,
                expected_revision,
                reply,
            } => {
                let r = sessions.reset(&handle, expected_revision);
                let _ = reply.send(r);
            }
            Job::Close { handle, reply } => {
                let r = sessions.close(&handle);
                let _ = reply.send(r);
            }
        }
    }
}

/// The XFA node tree of a document on disk, plus its packet names -- for
/// the tools that work below a full prepare, such as `Job::Controls`'s
/// `Target::Doc` arm, which needs the nodes but nothing else a render does.
fn nodes_of(path: &Path) -> Result<(Vec<XfaNode>, Vec<String>), RenderError> {
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
            detail: NOT_XFA.to_string(),
        })?;
    let names: Vec<String> = packets.iter().map(|p| p.name.clone()).collect();
    let xfa: Vec<u8> = packets.into_iter().flat_map(|p| p.content).collect();
    let nodes = XfaNode::parse(&xfa).map_err(|e| RenderError::backend(ENGINE, e))?;
    Ok((nodes, names))
}

fn prepare(path: &Path, state: &StateSpec, prepares: &Prepares) -> Result<Prepared, RenderError> {
    if let Ok(mut m) = prepares.lock() {
        *m.entry(path.to_path_buf()).or_insert(0) += 1;
    }
    let (nodes, names) = nodes_of(path)?;

    // The default state goes through the fidelity ladder, which can degrade
    // with a warning if the form's scripts fail. A requested state goes through
    // `materialize`, which applies the selections and refreshes the form.
    let (flattened, fidelity, warning) = if state.is_default() {
        let prepared =
            prepare_default(&nodes).map_err(|e| RenderError::backend(ENGINE, e.to_string()))?;
        (prepared.flattened, prepared.fidelity, prepared.warning)
    } else {
        let m =
            materialize(&nodes, state).map_err(|e| RenderError::backend(ENGINE, e.to_string()))?;
        (m.flattened, m.fidelity, m.warning)
    };
    let language = flattened.language.clone();

    Ok(Prepared::fresh(flattened, fidelity, warning, names, language))
}

/// Resolve `target` to a prepared view, whichever of the two addressing
/// modes it names -- see [`Target`]'s own doc for how the two differ.
fn resolve(
    target: &Target,
    cache: &mut HashMap<DocKey, Arc<Prepared>>,
    order: &mut Vec<DocKey>,
    cache_size: usize,
    prepares: &Prepares,
    sessions: &mut crate::session::Sessions,
) -> Result<Arc<Prepared>, RenderError> {
    match target {
        Target::Doc { path, state } => {
            with_prepared(path, state, cache, order, cache_size, prepares)
        }
        Target::View { handle, revision } => sessions.view(handle, *revision),
    }
}

fn with_prepared(
    path: &Path,
    state: &StateSpec,
    cache: &mut HashMap<DocKey, Arc<Prepared>>,
    order: &mut Vec<DocKey>,
    cache_size: usize,
    prepares: &Prepares,
) -> Result<Arc<Prepared>, RenderError> {
    let key = DocKey::of(path, state)?;
    if let Some(hit) = cache.get(&key) {
        return Ok(Arc::clone(hit));
    }
    let prepared = Arc::new(prepare(path, state, prepares)?);
    cache.insert(key.clone(), Arc::clone(&prepared));
    order.push(key);
    while order.len() > cache_size {
        let evicted = order.remove(0);
        cache.remove(&evicted);
    }
    Ok(prepared)
}

fn document_info(p: &Prepared, limits: &Limits) -> DocumentInfo {
    let mut pages = geometry(&p.flattened);
    let page_count = pages.len() as u32;
    pages.truncate(limits.max_page_entries);

    DocumentInfo {
        kind: DocumentKind::Xfa,
        page_count,
        pages,
        language: p.language.clone(),
        fidelity: p.fidelity,
        packets: p.packets.clone(),
        warning: p.warning.clone(),
    }
}

fn band_for(p: &Prepared, page: u32) -> Result<Band, RenderError> {
    let all = bands(&p.flattened);
    let page_count = all.len() as u32;
    all.into_iter()
        .find(|b| b.page == page)
        .ok_or(RenderError::PageOutOfRange { page, page_count })
}

/// Render the whole column once, then crop one band out of it.
///
/// The engine has no per-page render; this is the crop that makes pages exist.
fn render_band(
    p: &Prepared,
    band: &Band,
    all_bands: &[Band],
    dpi: Option<f32>,
    max_edge: Option<u32>,
    format: ImageFormat,
    limits: &Limits,
) -> Result<RenderedPage, RenderError> {
    // The clamp is measured against the tallest *page*, not the whole column:
    // a caller asking for one page of a ten-page form must not be silently
    // downscaled because the document is long. Using the same reference for
    // every page in a walk is also what lets them share one rasterized buffer.
    let (scale, dpi_effective) =
        limits.effective_scale(band.width, max_page_height(all_bands), dpi, max_edge);

    let full = rasterize_cached(p, scale)?;

    let top = (band.top * scale).round().max(0.0) as u32;
    let height = ((band.height() * scale).round() as u32)
        .min(full.height().saturating_sub(top))
        .max(1);
    let cropped = image::imageops::crop_imm(full.as_ref(), 0, top, full.width(), height).to_image();

    let (width_px, height_px) = (cropped.width(), cropped.height());
    let data = encode(&cropped, format, limits.jpeg_quality)?;

    Ok(RenderedPage {
        page: band.page,
        width_px,
        height_px,
        dpi_effective,
        mime: format.mime(),
        data,
    })
}

/// Rasterize the tall column at `scale`, or return the cached buffer if this
/// document (in this state) was already rasterized at that scale.
fn rasterize_cached(p: &Prepared, scale: f32) -> Result<Arc<RgbaImage>, RenderError> {
    let key = scale.to_bits();
    if let Ok(cache) = p.rasters.lock()
        && let Some((_, img)) = cache.iter().find(|(k, _)| *k == key)
    {
        return Ok(Arc::clone(img));
    }

    let rendered = p
        .flattened
        .render_to_image_buffer_plain(scale)
        .map_err(|e| RenderError::backend(ENGINE, e))?;
    if let Ok(mut n) = p.raster_count.lock() {
        *n += 1;
    }
    let img = Arc::new(rendered);

    if let Ok(mut cache) = p.rasters.lock() {
        cache.retain(|(k, _)| *k != key); // this scale may have raced in
        cache.push((key, Arc::clone(&img)));
        let limit = raster_cache_size();
        while cache.len() > limit {
            cache.remove(0);
        }
    }
    Ok(img)
}

#[allow(clippy::too_many_arguments)]
fn do_render_pages(
    p: &Prepared,
    pages: &[u32],
    dpi: Option<f32>,
    max_edge: Option<u32>,
    format: ImageFormat,
    batch: Option<BatchLimits>,
    limits: &Limits,
) -> Result<RenderedPages, RenderError> {
    let all = bands(&p.flattened);
    let page_count = all.len() as u32;

    let render_one = |page: u32| -> Result<RenderedPage, RenderError> {
        let band = all
            .iter()
            .find(|b| b.page == page)
            .ok_or(RenderError::PageOutOfRange { page, page_count })?;
        render_band(p, band, &all, dpi, max_edge, format, limits)
    };

    let Some(batch) = batch else {
        let page = render_one(pages[0])?;
        return Ok(RenderedPages {
            pages: vec![page],
            next_from: None,
            budget_hit: None,
            warning: p.warning.clone(),
        });
    };

    let mut out = render_batch(pages, batch, p.warning.clone(), render_one)?;
    if out.next_from == Some(page_count + 1) {
        out.next_from = None;
    }
    Ok(out)
}

fn do_render_region(
    p: &Prepared,
    page: u32,
    rect: RectPt,
    dpi: Option<f32>,
    format: ImageFormat,
    limits: &Limits,
) -> Result<RenderedPage, RenderError> {
    let band = band_for(p, page)?;

    let out_of_bounds = rect.width <= 0.0
        || rect.height <= 0.0
        || rect.x < -0.5
        || rect.y < -0.5
        || rect.x + rect.width > band.width + 0.5
        || rect.y + rect.height > band.height() + 0.5;
    if out_of_bounds {
        return Err(RenderError::RegionOutOfBounds {
            page,
            x: rect.x,
            y: rect.y,
            w: rect.width,
            h: rect.height,
            width_pt: band.width,
            height_pt: band.height(),
        });
    }

    // Resolution is chosen from the region, since a zoom is about detail there —
    // but once we have a target dpi, the actual rasterization scale is picked
    // the same way every page render picks it, against the tallest page, so a
    // region crop can share the walk's cached column.
    let (_, dpi_effective) =
        limits.effective_scale(rect.width, rect.height, Some(dpi.unwrap_or(300.0)), None);
    let all = bands(&p.flattened);
    let (scale, _) =
        limits.effective_scale(band.width, max_page_height(&all), Some(dpi_effective), None);

    let full = rasterize_cached(p, scale)?;

    // The rect is page-local; the column is not. XFA is natively top-left, so
    // this is an offset with no flip.
    let x = (rect.x * scale).round().max(0.0) as u32;
    let y = ((band.top + rect.y) * scale).round().max(0.0) as u32;
    let w = ((rect.width * scale).round() as u32)
        .min(full.width().saturating_sub(x))
        .max(1);
    let h = ((rect.height * scale).round() as u32)
        .min(full.height().saturating_sub(y))
        .max(1);
    let cropped = image::imageops::crop_imm(full.as_ref(), x, y, w, h).to_image();

    let (width_px, height_px) = (cropped.width(), cropped.height());
    let data = encode(&cropped, format, limits.jpeg_quality)?;

    Ok(RenderedPage {
        page,
        width_px,
        height_px,
        dpi_effective: scale * 72.0,
        mime: format.mime(),
        data,
    })
}

fn do_page_text(
    p: &Prepared,
    page: u32,
    offset: usize,
    limit: usize,
) -> Result<PageText, RenderError> {
    let band = band_for(p, page)?;
    let all = page_text(&p.flattened, &band);

    // Windowed by character, so a boundary can never split a UTF-8 sequence.
    // Shared with every other text-returning tool.
    let w = window_chars(&all, offset, limit);

    Ok(PageText {
        page,
        text: w.text,
        offset: w.offset,
        total_chars: w.total_chars,
        truncated: w.truncated,
    })
}

/// Grep the text of pages `from` onwards.
///
/// `bands` and `page_texts` are each called once for the whole scan: both walk
/// every node in the document, so calling them per page would make this
/// quadratic in a long form.
fn do_search_text(
    p: &Prepared,
    from: u32,
    pattern: Pattern,
    limit: usize,
    limits: &Limits,
) -> Result<TextSearch, RenderError> {
    let all_bands = bands(&p.flattened);
    let page_count = all_bands.len() as u32;
    if from == 0 || from > page_count {
        return Err(RenderError::PageOutOfRange {
            page: from,
            page_count,
        });
    }

    let texts = page_texts(&p.flattened, &all_bands);
    let mut grep = Grep::new(pattern, limit, limits.search_context_radius);

    let mut matches = Vec::new();
    let mut through = from.saturating_sub(1);
    let mut budget_hit = None;
    let mut next_from = None;

    // `scanned` is the enumerate index rather than its own counter: it is read
    // before each page is scanned, so the index is exactly how many came before.
    for (scanned, page) in (from..=page_count).enumerate() {
        // Both caps are checked before the page, never inside it: a page is
        // scanned whole or not at all, so `next_from` names a page nobody has
        // looked at and a resumed walk cannot repeat a match.
        if scanned >= limits.search_max_pages {
            budget_hit = Some("pages");
            next_from = Some(page);
            break;
        }
        if grep.is_full() {
            budget_hit = Some("matches");
            next_from = Some(page);
            break;
        }

        let text = texts
            .get((page - 1) as usize)
            .map(String::as_str)
            .unwrap_or("");
        matches.extend(grep.scan(text).into_iter().map(|m| TextMatch {
            page,
            offset: m.offset,
            length: m.length,
            context: m.context,
        }));
        through = page;
    }

    Ok(TextSearch {
        matches,
        total_matches: grep.total_matches(),
        truncated: grep.truncated(),
        through,
        next_from,
        budget_hit,
    })
}
