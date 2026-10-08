//! The error taxonomy, shared by every renderer.
//!
//! One type rather than one per backend, because the MCP layer maps all of
//! these to `CallToolResult::error` identically and a shared mapping cannot be
//! written against a per-crate enum. Backend-specific failures that need no
//! structured matching travel in [`RenderError::Backend`]; anything a caller
//! might branch on gets its own variant.

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// The file is not something this renderer can open at all.
    #[error("unsupported input: {path} ({detail})")]
    UnsupportedInput { path: String, detail: String },

    #[error("password-protected PDFs are not supported: {path}")]
    Encrypted { path: String },

    #[error("page {page} of {page_count} — valid range 1..{page_count}")]
    PageOutOfRange { page: u32, page_count: u32 },

    /// The caller's argument is bad — a malformed regex, a value out of
    /// range. Its own variant rather than [`RenderError::Backend`] because a
    /// caller's typo must not be reported as an engine failure: routing one
    /// through `Backend` produces "pdfium: invalid regex", which blames the
    /// wrong party and sends a reader looking in the wrong place.
    #[error("invalid argument {field}: {detail}")]
    InvalidArgument { field: &'static str, detail: String },

    #[error("region [{x}, {y}, {w}, {h}] pt is outside page {page} ({width_pt} x {height_pt} pt)")]
    RegionOutOfBounds {
        page: u32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        width_pt: f32,
        height_pt: f32,
    },

    /// A failure from the rendering engine itself, named so the message says
    /// which engine produced it.
    #[error("{engine}: {detail}")]
    Backend {
        engine: &'static str,
        detail: String,
    },

    /// The engine could not be initialised — the fail-fast case at boot.
    #[error("{engine} not available; searched: {searched:?}")]
    EngineUnavailable {
        engine: &'static str,
        searched: Vec<String>,
    },

    #[error("render worker unavailable: {0}")]
    Worker(String),

    #[error("blob store: {0}")]
    Blob(String),
}

impl RenderError {
    /// Shorthand for a backend failure.
    pub fn backend(engine: &'static str, detail: impl Into<String>) -> Self {
        RenderError::Backend {
            engine,
            detail: detail.into(),
        }
    }

    /// Shorthand for a bad argument from the caller.
    pub fn invalid_argument(field: &'static str, detail: impl Into<String>) -> Self {
        RenderError::InvalidArgument {
            field,
            detail: detail.into(),
        }
    }
}
