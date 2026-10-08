//! The single pdfium thread.
//!
//! pdfium is not thread-safe, so every call into it happens on one owned
//! thread; public methods send a request and await a reply. Concurrency
//! serializes here by construction rather than by discipline. Scaling out is
//! "run more server processes", which the u2s client pool already does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

use image::RgbaImage;
use pdfium_render::prelude::*;

use u2s_render_core::encode::encode;
use u2s_render_core::{
    BatchLimits, Grep, ImageFormat, Limits, PageGeometry, PageText, Pattern, RectPt, RenderError,
    RenderedPage, RenderedPages, TextMatch, TextSearch, render_batch, window_chars,
};

use crate::error::{ENGINE, from_load};
use crate::types::{DocumentInfo, FormType};

/// The library a host process supplies itself (for example one it embeds and
/// extracts), set once before the first render. Set-once, so it cannot change
/// under a worker that already bound pdfium.
static HOST_LIBRARY: OnceLock<PathBuf> = OnceLock::new();

/// Names the pdfium library to bind, taking precedence over every search path
/// except `PDFIUM_LIB_PATH`. Only the first call has an effect; it returns
/// whether this call was it.
pub fn set_library_path(path: PathBuf) -> bool {
    HOST_LIBRARY.set(path).is_ok()
}

/// Where to look for the pdfium dynamic library, in order:
/// `PDFIUM_LIB_PATH`, then the directory of the library the host set with
/// [`set_library_path`], then next to the running binary, then a `vendor/pdfium/lib`
/// directory in any ancestor of the binary or the working directory. The
/// ancestor walk is what makes `cargo test` work without configuration: test
/// binaries live in `target/debug/deps`, several levels below the vendored
/// library at the workspace root.
fn library_search_paths() -> Vec<PathBuf> {
    // An explicit setting is authoritative: if the operator names a directory
    // and the library is not there, that is an error, not a reason to quietly
    // load a different pdfium from somewhere else.
    if let Ok(p) = std::env::var("PDFIUM_LIB_PATH") {
        return vec![PathBuf::from(p)];
    }

    let mut paths = Vec::new();
    if let Some(dir) = HOST_LIBRARY.get().and_then(|lib| lib.parent()) {
        paths.push(dir.to_path_buf());
    }

    let mut roots = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        paths.push(dir.to_path_buf());
        roots.push(dir.to_path_buf());
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }

    for root in roots {
        for ancestor in root.ancestors() {
            let candidate = ancestor.join("vendor/pdfium/lib");
            if !paths.contains(&candidate) {
                paths.push(candidate);
            }
        }
    }
    paths
}

/// Bind pdfium, or fail with every path we looked in. Called once at startup:
/// a render server that starts without its renderer produces confusing
/// per-call errors forever, so we refuse to start instead.
fn bind() -> Result<Pdfium, RenderError> {
    let mut searched = Vec::new();
    for dir in library_search_paths() {
        let candidate = Pdfium::pdfium_platform_library_name_at_path(&dir);
        if !candidate.exists() {
            searched.push(format!("{} (absent)", candidate.display()));
            continue;
        }
        match Pdfium::bind_to_library(&candidate) {
            Ok(bindings) => {
                log::info!("bound pdfium at {}", candidate.display());
                return Ok(Pdfium::new(bindings));
            }
            Err(e) => searched.push(format!("{} ({e})", candidate.display())),
        }
    }
    if let Ok(bindings) = Pdfium::bind_to_system_library() {
        log::info!("bound system pdfium");
        return Ok(Pdfium::new(bindings));
    }
    searched.push("<system library path>".to_string());
    Err(RenderError::EngineUnavailable {
        engine: ENGINE,
        searched,
    })
}

enum Job {
    Info {
        path: PathBuf,
        limits: Limits,
        reply: Sender<Result<DocumentInfo, RenderError>>,
    },
    RenderPages {
        path: PathBuf,
        pages: Vec<u32>,
        dpi: Option<f32>,
        max_edge: Option<u32>,
        format: ImageFormat,
        /// `None` for single-page renders: no batching, no byte budget.
        batch: Option<BatchLimits>,
        limits: Limits,
        reply: Sender<Result<RenderedPages, RenderError>>,
    },
    RenderRegion {
        path: PathBuf,
        page: u32,
        rect: RectPt,
        dpi: Option<f32>,
        format: ImageFormat,
        limits: Limits,
        reply: Sender<Result<RenderedPage, RenderError>>,
    },
    PageText {
        path: PathBuf,
        page: u32,
        offset: usize,
        limit: usize,
        reply: Sender<Result<PageText, RenderError>>,
    },
    SearchText {
        path: PathBuf,
        from: u32,
        pattern: Pattern,
        limit: usize,
        limits: Limits,
        reply: Sender<Result<TextSearch, RenderError>>,
    },
}

/// Cache key: a document is the same document only if path, size and mtime all
/// match. Rendering pages 1..n must not reopen the file n times, but a file
/// replaced under us must not be served from cache.
#[derive(PartialEq, Eq, Hash, Clone, Debug)]
struct DocKey {
    path: PathBuf,
    len: u64,
    mtime: Option<std::time::SystemTime>,
}

impl DocKey {
    fn of(path: &Path) -> Result<Self, RenderError> {
        let meta = std::fs::metadata(path).map_err(|source| RenderError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Ok(DocKey {
            path: path.to_path_buf(),
            len: meta.len(),
            mtime: meta.modified().ok(),
        })
    }
}

/// The process-wide pdfium worker. pdfium's bindings are a global singleton —
/// a second attempt to bind in the same process fails — so the thread that owns
/// them is a singleton too. `Renderer` is a cheap handle onto it, carrying only
/// the limits a particular caller wants.
static WORKER: OnceLock<Result<WorkerHandle, String>> = OnceLock::new();

/// How many times each document has been opened. Keyed by path so a test can
/// assert on its own fixture while the worker is shared with everything else.
type OpenCounts = Arc<Mutex<HashMap<PathBuf, u64>>>;

#[derive(Clone)]
struct WorkerHandle {
    tx: Sender<Job>,
    opens: OpenCounts,
}

fn worker() -> Result<WorkerHandle, RenderError> {
    WORKER
        .get_or_init(|| spawn_worker().map_err(|e| e.to_string()))
        .clone()
        .map_err(|e| {
            // The message is preserved verbatim so a missing library still
            // reports every path that was searched.
            if e.contains("not available") {
                RenderError::EngineUnavailable {
                    engine: ENGINE,
                    searched: vec![e.clone()],
                }
            } else {
                RenderError::Worker(e)
            }
        })
}

fn spawn_worker() -> Result<WorkerHandle, RenderError> {
    let (ready_tx, ready_rx) = channel::<Result<(), RenderError>>();
    let (tx, rx) = channel::<Job>();
    let opens: OpenCounts = Arc::new(Mutex::new(HashMap::new()));
    let opens_worker = Arc::clone(&opens);
    let cache_size = doc_cache_size();

    std::thread::Builder::new()
        .name("pdfium".into())
        .spawn(move || match bind() {
            Ok(pdfium) => {
                let _ = ready_tx.send(Ok(()));
                run(pdfium, rx, cache_size, opens_worker);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        })
        .map_err(|e| RenderError::Worker(format!("cannot spawn pdfium thread: {e}")))?;

    ready_rx
        .recv()
        .map_err(|_| RenderError::Worker("pdfium thread died during startup".into()))??;
    Ok(WorkerHandle { tx, opens })
}

/// Handle to the shared worker. Cheap to clone; all clones address one thread.
#[derive(Clone)]
pub struct Renderer {
    handle: WorkerHandle,
    limits: Limits,
}

impl Renderer {
    /// Bind pdfium (once per process) and return a handle. Returns an error —
    /// rather than a half-working server — when the library is missing, so a
    /// caller can refuse to start.
    pub fn start(limits: Limits) -> Result<Self, RenderError> {
        Ok(Renderer {
            handle: worker()?,
            limits,
        })
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// How many times this document has been opened by the worker. Used to
    /// prove the metadata cache actually elides re-opens.
    pub fn open_count_for(&self, path: impl AsRef<Path>) -> u64 {
        self.handle
            .opens
            .lock()
            .map(|m| m.get(path.as_ref()).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    fn submit<T>(
        &self,
        job: Job,
        reply: Receiver<Result<T, RenderError>>,
    ) -> Result<T, RenderError> {
        self.handle
            .tx
            .send(job)
            .map_err(|_| RenderError::Worker("pdfium thread is gone".into()))?;
        reply
            .recv()
            .map_err(|_| RenderError::Worker("pdfium thread died mid-request".into()))?
    }

    pub fn info(&self, path: impl AsRef<Path>) -> Result<DocumentInfo, RenderError> {
        let (reply, rx) = channel();
        self.submit(
            Job::Info {
                path: path.as_ref().to_path_buf(),
                limits: self.limits.clone(),
                reply,
            },
            rx,
        )
    }

    /// Render exactly one page. No batching, no byte budget — the caller
    /// decides inline vs blob from the returned size.
    pub fn render_page(
        &self,
        path: impl AsRef<Path>,
        page: u32,
        dpi: Option<f32>,
        max_edge: Option<u32>,
        format: ImageFormat,
    ) -> Result<(RenderedPage, Option<String>), RenderError> {
        let (reply, rx) = channel();
        let out = self.submit(
            Job::RenderPages {
                path: path.as_ref().to_path_buf(),
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

    /// Cursor-paginated batch. Either an explicit page list or a window
    /// starting at `from`. Stops on whichever cap binds first and always says
    /// which, so the caller can walk `next_from` to exhaustion.
    // The parameters mirror the tool schema one-for-one and are mutually
    // independent; bundling them into a struct would only move the width.
    #[allow(clippy::too_many_arguments)]
    pub fn render_pages(
        &self,
        path: impl AsRef<Path>,
        pages: Option<Vec<u32>>,
        from: Option<u32>,
        limit: Option<usize>,
        dpi: Option<f32>,
        max_edge: Option<u32>,
        format: ImageFormat,
    ) -> Result<RenderedPages, RenderError> {
        let path = path.as_ref().to_path_buf();
        let page_count = self.info(&path)?.page_count;

        let requested: Vec<u32> = match pages {
            Some(explicit) => explicit,
            None => {
                let start = from.unwrap_or(1).max(1);
                (start..=page_count).collect()
            }
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
                path,
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
        path: impl AsRef<Path>,
        page: u32,
        rect: RectPt,
        dpi: Option<f32>,
        format: ImageFormat,
    ) -> Result<RenderedPage, RenderError> {
        let (reply, rx) = channel();
        self.submit(
            Job::RenderRegion {
                path: path.as_ref().to_path_buf(),
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
        path: impl AsRef<Path>,
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
                path: path.as_ref().to_path_buf(),
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
        path: impl AsRef<Path>,
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
                path: path.as_ref().to_path_buf(),
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

fn doc_cache_size() -> usize {
    std::env::var("DOC_CACHE_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4)
}

/// The worker loop. Owns the `Pdfium` instance for the process lifetime.
fn run(pdfium: Pdfium, rx: Receiver<Job>, cache_size: usize, opens: OpenCounts) {
    // Documents are re-opened per job rather than held across jobs: PdfDocument
    // borrows from Pdfium, and holding them in a struct field alongside the
    // owner would be self-referential. The cache below memoises the *metadata*
    // (page count, geometry, form type) which is what repeated calls actually
    // re-derive, and the open itself is cheap because pdfium loads lazily.
    let mut meta_cache: HashMap<DocKey, DocumentInfo> = HashMap::new();
    let mut order: Vec<DocKey> = Vec::new();

    while let Ok(job) = rx.recv() {
        match job {
            Job::Info {
                path,
                limits,
                reply,
            } => {
                let r = with_info(
                    &pdfium,
                    &path,
                    &limits,
                    &mut meta_cache,
                    &mut order,
                    cache_size,
                    &opens,
                );
                let _ = reply.send(r);
            }
            Job::RenderPages {
                path,
                pages,
                dpi,
                max_edge,
                format,
                batch,
                limits,
                reply,
            } => {
                let r = do_render_pages(
                    &pdfium, &path, &pages, dpi, max_edge, format, batch, &limits, &opens,
                );
                let _ = reply.send(r);
            }
            Job::RenderRegion {
                path,
                page,
                rect,
                dpi,
                format,
                limits,
                reply,
            } => {
                let r = do_render_region(&pdfium, &path, page, rect, dpi, format, &limits, &opens);
                let _ = reply.send(r);
            }
            Job::PageText {
                path,
                page,
                offset,
                limit,
                reply,
            } => {
                let r = do_page_text(&pdfium, &path, page, offset, limit, &opens);
                let _ = reply.send(r);
            }
            Job::SearchText {
                path,
                from,
                pattern,
                limit,
                limits,
                reply,
            } => {
                let r = do_search_text(&pdfium, &path, from, pattern, limit, &limits, &opens);
                let _ = reply.send(r);
            }
        }
    }
}

fn open<'a>(
    pdfium: &'a Pdfium,
    path: &Path,
    opens: &OpenCounts,
) -> Result<PdfDocument<'a>, RenderError> {
    if let Ok(mut m) = opens.lock() {
        *m.entry(path.to_path_buf()).or_insert(0) += 1;
    }
    pdfium
        .load_pdf_from_file(path, None)
        .map_err(|e| from_load(path, e))
}

fn xfa_warning(form_type: FormType) -> Option<String> {
    form_type.is_xfa().then(|| {
        format!(
            "form_type is {} — pdfium renders XFA documents as a static shim page \
             ('please update your reader'); use the XFA renderer for this document",
            match form_type {
                FormType::XfaFull => "xfa_full",
                _ => "xfa_foreground",
            }
        )
    })
}

fn read_info(
    pdfium: &Pdfium,
    path: &Path,
    limits: &Limits,
    opens: &OpenCounts,
) -> Result<DocumentInfo, RenderError> {
    let doc = open(pdfium, path, opens)?;
    let page_count = doc.pages().len() as u32;

    let form_type = match doc.form().map(|f| f.form_type()) {
        Some(PdfFormType::Acrobat) => FormType::Acroform,
        Some(PdfFormType::XfaFull) => FormType::XfaFull,
        Some(PdfFormType::XfaForeground) => FormType::XfaForeground,
        Some(PdfFormType::None) | None => FormType::None,
    };

    let listed = (page_count as usize).min(limits.max_page_entries);
    let mut pages = Vec::with_capacity(listed);
    for idx in 0..listed {
        let page = doc
            .pages()
            .get(idx as i32)
            .map_err(|e| RenderError::backend(ENGINE, format!("page {}: {e:?}", idx + 1)))?;
        pages.push(PageGeometry {
            page: idx as u32 + 1,
            width_pt: page.width().value,
            height_pt: page.height().value,
            rotation: match page.rotation() {
                Ok(PdfPageRenderRotation::Degrees90) => 90,
                Ok(PdfPageRenderRotation::Degrees180) => 180,
                Ok(PdfPageRenderRotation::Degrees270) => 270,
                _ => 0,
            },
        });
    }

    let meta = doc.metadata();
    let get = |t| meta.get(t).map(|tag| tag.value().to_string());

    Ok(DocumentInfo {
        page_count,
        pages,
        pages_truncated: listed < page_count as usize,
        form_type,
        title: get(PdfDocumentMetadataTagType::Title),
        producer: get(PdfDocumentMetadataTagType::Producer),
        warning: xfa_warning(form_type),
    })
}

fn with_info(
    pdfium: &Pdfium,
    path: &Path,
    limits: &Limits,
    cache: &mut HashMap<DocKey, DocumentInfo>,
    order: &mut Vec<DocKey>,
    cache_size: usize,
    opens: &OpenCounts,
) -> Result<DocumentInfo, RenderError> {
    let key = DocKey::of(path)?;
    if let Some(hit) = cache.get(&key) {
        return Ok(hit.clone());
    }
    let info = read_info(pdfium, path, limits, opens)?;
    cache.insert(key.clone(), info.clone());
    order.push(key);
    while order.len() > cache_size {
        let evicted = order.remove(0);
        cache.remove(&evicted);
    }
    Ok(info)
}

fn render_one(
    doc: &PdfDocument<'_>,
    page_number: u32,
    dpi: Option<f32>,
    max_edge: Option<u32>,
    format: ImageFormat,
    limits: &Limits,
) -> Result<RenderedPage, RenderError> {
    let page_count = doc.pages().len() as u32;
    if page_number == 0 || page_number > page_count {
        return Err(RenderError::PageOutOfRange {
            page: page_number,
            page_count,
        });
    }
    let page = doc
        .pages()
        .get(page_number as i32 - 1)
        .map_err(|e| RenderError::backend(ENGINE, format!("page {page_number}: {e:?}")))?;

    let (scale, dpi_effective) =
        limits.effective_scale(page.width().value, page.height().value, dpi, max_edge);

    let config = PdfRenderConfig::new().scale_page_by_factor(scale);
    let bitmap = page
        .render_with_config(&config)
        .map_err(|e| RenderError::backend(ENGINE, format!("render page {page_number}: {e:?}")))?;
    let img: RgbaImage = bitmap
        .as_image()
        .map_err(|e| RenderError::backend(ENGINE, format!("bitmap page {page_number}: {e:?}")))?
        .into_rgba8();

    let (width_px, height_px) = (img.width(), img.height());
    let data = encode(&img, format, limits.jpeg_quality)?;

    Ok(RenderedPage {
        page: page_number,
        width_px,
        height_px,
        dpi_effective,
        mime: format.mime(),
        data,
    })
}

#[allow(clippy::too_many_arguments)]
fn do_render_pages(
    pdfium: &Pdfium,
    path: &Path,
    pages: &[u32],
    dpi: Option<f32>,
    max_edge: Option<u32>,
    format: ImageFormat,
    batch: Option<BatchLimits>,
    limits: &Limits,
    opens: &OpenCounts,
) -> Result<RenderedPages, RenderError> {
    let doc = open(pdfium, path, opens)?;
    let page_count = doc.pages().len() as u32;
    let form_type = match doc.form().map(|f| f.form_type()) {
        Some(PdfFormType::XfaFull) => FormType::XfaFull,
        Some(PdfFormType::XfaForeground) => FormType::XfaForeground,
        _ => FormType::None,
    };

    let Some(batch) = batch else {
        // Single-page path: no caps, the caller wanted exactly this page.
        let page = render_one(&doc, pages[0], dpi, max_edge, format, limits)?;
        return Ok(RenderedPages {
            pages: vec![page],
            next_from: None,
            budget_hit: None,
            warning: xfa_warning(form_type),
        });
    };

    let mut batch = render_batch(pages, batch, xfa_warning(form_type), |page_number| {
        render_one(&doc, page_number, dpi, max_edge, format, limits)
    })?;

    // Only a contiguous window has a meaningful cursor; an explicit page list
    // that was cut short still reports where it stopped.
    if batch.next_from == Some(page_count + 1) {
        batch.next_from = None;
    }

    Ok(batch)
}

#[allow(clippy::too_many_arguments)]
fn do_render_region(
    pdfium: &Pdfium,
    path: &Path,
    page_number: u32,
    rect: RectPt,
    dpi: Option<f32>,
    format: ImageFormat,
    limits: &Limits,
    opens: &OpenCounts,
) -> Result<RenderedPage, RenderError> {
    let doc = open(pdfium, path, opens)?;
    let page_count = doc.pages().len() as u32;
    if page_number == 0 || page_number > page_count {
        return Err(RenderError::PageOutOfRange {
            page: page_number,
            page_count,
        });
    }
    let page = doc
        .pages()
        .get(page_number as i32 - 1)
        .map_err(|e| RenderError::backend(ENGINE, format!("page {page_number}: {e:?}")))?;

    let (width_pt, height_pt) = (page.width().value, page.height().value);
    let out_of_bounds = rect.width <= 0.0
        || rect.height <= 0.0
        || rect.x < -0.5
        || rect.y < -0.5
        || rect.x + rect.width > width_pt + 0.5
        || rect.y + rect.height > height_pt + 0.5;
    if out_of_bounds {
        return Err(RenderError::RegionOutOfBounds {
            page: page_number,
            x: rect.x,
            y: rect.y,
            w: rect.width,
            h: rect.height,
            width_pt,
            height_pt,
        });
    }

    // Scale is chosen from the *region*, not the page: a zoom is about
    // resolving detail in the crop.
    let (scale, dpi_effective) =
        limits.effective_scale(rect.width, rect.height, Some(dpi.unwrap_or(300.0)), None);

    // Render the whole page at the zoom scale, then crop. pdfium's clipping API
    // works in device space and interacts with rotation; cropping the rendered
    // page is unambiguous and the memory cost is bounded by the edge clamp.
    let page_scale = limits
        .effective_scale(width_pt, height_pt, Some(dpi_effective), None)
        .0;
    let config = PdfRenderConfig::new().scale_page_by_factor(page_scale);
    let bitmap = page
        .render_with_config(&config)
        .map_err(|e| RenderError::backend(ENGINE, format!("render page {page_number}: {e:?}")))?;
    let full: RgbaImage = bitmap
        .as_image()
        .map_err(|e| RenderError::backend(ENGINE, format!("bitmap page {page_number}: {e:?}")))?
        .into_rgba8();

    let x = (rect.x * page_scale).round().max(0.0) as u32;
    let y = (rect.y * page_scale).round().max(0.0) as u32;
    let w = ((rect.width * page_scale).round() as u32)
        .min(full.width().saturating_sub(x))
        .max(1);
    let h = ((rect.height * page_scale).round() as u32)
        .min(full.height().saturating_sub(y))
        .max(1);
    let cropped = image::imageops::crop_imm(&full, x, y, w, h).to_image();

    let _ = scale;
    let (width_px, height_px) = (cropped.width(), cropped.height());
    let data = encode(&cropped, format, limits.jpeg_quality)?;

    Ok(RenderedPage {
        page: page_number,
        width_px,
        height_px,
        dpi_effective: page_scale * 72.0,
        mime: format.mime(),
        data,
    })
}

fn do_page_text(
    pdfium: &Pdfium,
    path: &Path,
    page_number: u32,
    offset: usize,
    limit: usize,
    opens: &OpenCounts,
) -> Result<PageText, RenderError> {
    let doc = open(pdfium, path, opens)?;
    let page_count = doc.pages().len() as u32;
    if page_number == 0 || page_number > page_count {
        return Err(RenderError::PageOutOfRange {
            page: page_number,
            page_count,
        });
    }
    let page = doc
        .pages()
        .get(page_number as i32 - 1)
        .map_err(|e| RenderError::backend(ENGINE, format!("page {page_number}: {e:?}")))?;
    let all = page
        .text()
        .map_err(|e| RenderError::backend(ENGINE, format!("text page {page_number}: {e:?}")))?
        .all();

    // Windowing is by character, not byte, so a window boundary can never split
    // a UTF-8 sequence. Shared with every other text-returning tool.
    let w = window_chars(&all, offset, limit);

    Ok(PageText {
        page: page_number,
        text: w.text,
        offset: w.offset,
        total_chars: w.total_chars,
        truncated: w.truncated,
    })
}

/// Grep the text of pages `from` onwards.
///
/// Opens the document once for the whole scan and reads `page_count` from it
/// directly, rather than going through `Renderer::info` the way
/// `render_pages` does — that would be a second worker round trip for
/// something already in hand.
fn do_search_text(
    pdfium: &Pdfium,
    path: &Path,
    from: u32,
    pattern: Pattern,
    limit: usize,
    limits: &Limits,
    opens: &OpenCounts,
) -> Result<TextSearch, RenderError> {
    let doc = open(pdfium, path, opens)?;
    let pages = doc.pages();
    let page_count = pages.len() as u32;
    if from == 0 || from > page_count {
        return Err(RenderError::PageOutOfRange {
            page: from,
            page_count,
        });
    }

    let mut grep = Grep::new(pattern, limit, limits.search_context_radius);
    let mut matches = Vec::new();
    let mut through = from.saturating_sub(1);
    let mut budget_hit = None;
    let mut next_from = None;

    // `scanned` is the enumerate index rather than its own counter: it is read
    // before each page is scanned, so the index is exactly how many came before.
    for (scanned, page_number) in (from..=page_count).enumerate() {
        // Both caps are checked before the page, never inside it: a page is
        // scanned whole or not at all, so `next_from` names a page nobody has
        // looked at and a resumed walk cannot repeat a match.
        if scanned >= limits.search_max_pages {
            budget_hit = Some("pages");
            next_from = Some(page_number);
            break;
        }
        if grep.is_full() {
            budget_hit = Some("matches");
            next_from = Some(page_number);
            break;
        }

        let page = pages
            .get(page_number as i32 - 1)
            .map_err(|e| RenderError::backend(ENGINE, format!("page {page_number}: {e:?}")))?;
        let text = page
            .text()
            .map_err(|e| RenderError::backend(ENGINE, format!("text page {page_number}: {e:?}")))?
            .all();

        matches.extend(grep.scan(&text).into_iter().map(|m| TextMatch {
            page: page_number,
            offset: m.offset,
            length: m.length,
            context: m.context,
        }));
        through = page_number;
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
