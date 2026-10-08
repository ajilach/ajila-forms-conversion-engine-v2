//! Font registration from the filesystem.
//!
//! DEVIATION from upstream: `core/src/profiles.rs` loads fonts out of a
//! `profiles/` tree compiled into the binary with `include_dir!`. Two reasons
//! that cannot come along:
//!
//! * **Licensing.** The only profile upstream ships is UBS, whose Frutiger
//!   faces are a commercial Linotype typeface. A licence covering
//!   redistribution within this workspace has since been confirmed, and the
//!   three needed files are vendored at `vendor/fonts/ubs-frutiger/` (see that
//!   directory's README) — used *and* redistributed, but only there, and that
//!   confirmation does not extend to any other use.
//! * **Weight.** The tree is ~9 MB, of which ~300 KB is font data; the rest is
//!   AEM fragment XML that rendering never reads.
//!
//! So fonts are loaded at runtime from a configured directory rather than
//! compiled in. `U2S_FONT_DIR` can still point at any other checkout for
//! fidelity work.
//!
//! **Fonts are not optional.** Rendering fails outright without a registered
//! fallback, and — worse — layout *silently* degrades: when a metrics lookup
//! fails the flattener falls back to `approximate_text_bounds`, producing wrong
//! heights and therefore wrong page breaks. A caller must register fonts before
//! doing anything, which is why the server refuses to boot without them.

use std::path::{Path, PathBuf};

use crate::XfaError;
use crate::xfa::font_manager::{get_font_manager, register_profile_font_data};

/// Register every `.ttf`/`.otf` in `dir`, and set the fallback.
///
/// DEVIATION: upstream sets the fallback to whichever file the directory
/// happens to yield first — alphabetically `frutiger-bold.ttf`, so every
/// unresolvable typeface renders **bold**. Here the fallback is chosen
/// explicitly: `fallback_family` if it matches a file stem, else the
/// lexicographically first *regular-looking* face, else the first file. The
/// choice is returned so a caller can log or assert on it.
pub fn register_dir(
    dir: impl AsRef<Path>,
    fallback_stem: Option<&str>,
) -> Result<RegisteredFonts, XfaError> {
    let dir = dir.as_ref();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| XfaError::Font(format!("cannot read font dir {}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            matches!(
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("ttf") | Some("otf")
            )
        })
        .collect();
    // Sorted so registration order — and therefore any order-dependent
    // behaviour left in the manager — is stable across filesystems.
    files.sort();

    if files.is_empty() {
        return Err(XfaError::Font(format!(
            "no .ttf or .otf files in {}",
            dir.display()
        )));
    }

    let fallback = choose_fallback(&files, fallback_stem);

    let manager = get_font_manager();
    let mut manager = manager
        .lock()
        .map_err(|e| XfaError::Font(format!("font manager lock: {e}")))?;

    let mut registered = Vec::new();
    let mut fallback_data: Option<&'static [u8]> = None;

    for path in &files {
        let bytes = std::fs::read(path)
            .map_err(|e| XfaError::Font(format!("cannot read {}: {e}", path.display())))?;
        // The manager's API takes `&'static [u8]`, and it leaks internally
        // anyway when resolving embedded fonts. Leaking here is bounded: fonts
        // are registered once at boot, not per document.
        let data: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        register_profile_font_data(&mut manager, data);
        if Some(path) == fallback.as_ref() {
            fallback_data = Some(data);
        }
        registered.push(path.clone());
    }

    let fallback = fallback.ok_or_else(|| XfaError::Font("no fallback font chosen".into()))?;
    let data = fallback_data.ok_or_else(|| XfaError::Font("fallback font not loaded".into()))?;
    manager.set_fallback(data);

    Ok(RegisteredFonts {
        registered,
        fallback,
    })
}

/// Pick the fallback deterministically. Never directory order.
fn choose_fallback(files: &[PathBuf], preferred_stem: Option<&str>) -> Option<PathBuf> {
    let stem_of = |p: &PathBuf| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
    };

    if let Some(want) = preferred_stem {
        let want = want.to_ascii_lowercase();
        if let Some(hit) = files.iter().find(|p| stem_of(p) == want) {
            return Some(hit.clone());
        }
    }

    // A regular face makes a far better fallback than a bold or italic one:
    // every unresolvable typeface in the document ends up wearing it.
    const STYLED: [&str; 6] = ["bold", "italic", "oblique", "light", "thin", "black"];
    if let Some(regular) = files
        .iter()
        .find(|p| !STYLED.iter().any(|s| stem_of(p).contains(s)))
    {
        return Some(regular.clone());
    }

    files.first().cloned()
}

/// What `register_dir` actually did — returned rather than logged so a test can
/// assert the fallback is the intended one.
#[derive(Debug, Clone)]
pub struct RegisteredFonts {
    pub registered: Vec<PathBuf>,
    pub fallback: PathBuf,
}

/// Whether fonts are registered: the font manager has a fallback, which
/// [`register_dir`] always sets. A caller that registers fonts its own way
/// checks this before rendering anything.
pub fn fallback_registered() -> bool {
    get_font_manager()
        .lock()
        .is_ok_and(|manager| manager.has_fallback())
}

/// Register from `U2S_FONT_DIR`, which is how the server is configured.
pub fn register_from_env() -> Result<RegisteredFonts, XfaError> {
    let dir = std::env::var("U2S_FONT_DIR").map_err(|_| {
        XfaError::Font(
            "U2S_FONT_DIR is not set — XFA rendering cannot lay text out without fonts, \
             and would silently produce wrong page breaks if it tried"
                .into(),
        )
    })?;
    register_dir(dir, std::env::var("U2S_FONT_FALLBACK").ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_regular_face_is_preferred_over_a_styled_one() {
        let files: Vec<PathBuf> = [
            "frutiger-bold.ttf",
            "frutiger-italic.ttf",
            "frutiger-light.ttf",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        // Upstream would take the alphabetically first — bold. All three of
        // these are styled, so we fall back to the first, but the *rule* is
        // what matters and the next case proves it.
        assert_eq!(
            choose_fallback(&files, None),
            Some(PathBuf::from("frutiger-bold.ttf"))
        );

        let with_regular: Vec<PathBuf> = ["a-bold.ttf", "z-regular.ttf"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            choose_fallback(&with_regular, None),
            Some(PathBuf::from("z-regular.ttf")),
            "a regular face must win over an alphabetically earlier bold one"
        );
    }

    #[test]
    fn an_explicit_preference_wins() {
        let files: Vec<PathBuf> = ["a-regular.ttf", "dejavu-sans.ttf"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            choose_fallback(&files, Some("dejavu-sans")),
            Some(PathBuf::from("dejavu-sans.ttf"))
        );
    }
}

/// Font bootstrap for tests.
///
/// Fonts are process-global and must be registered exactly once. Tests that lay
/// text out call [`ensure_registered`].
#[doc(hidden)]
pub mod test_support {
    use std::sync::OnceLock;

    static REGISTERED: OnceLock<bool> = OnceLock::new();

    /// A directory to take fonts from, in order: `U2S_FONT_DIR`, then the
    /// vendored, licenced UBS Frutiger set (real metrics for the vendored
    /// UBS corpus), then this workspace's plain `vendor/fonts` (DejaVu).
    ///
    /// Both vendored paths are resolved against `CARGO_MANIFEST_DIR` rather
    /// than the process's working directory, which for a test is the crate
    /// root, not the workspace root. Both are committed, so — unlike the
    /// external-checkout fallback this once had — their absence means a
    /// broken checkout, not a machine this suite silently skips on.
    pub fn font_dir() -> Option<std::path::PathBuf> {
        if let Ok(d) = std::env::var("U2S_FONT_DIR") {
            let p = std::path::PathBuf::from(d);
            if p.is_dir() {
                return Some(p);
            }
        }
        let frutiger = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/fonts/ubs-frutiger");
        if frutiger.is_dir() {
            return Some(frutiger);
        }
        let vendored =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/fonts");
        vendored.is_dir().then_some(vendored)
    }

    /// Register once; report whether fonts are available at all.
    pub fn ensure_registered() -> bool {
        *REGISTERED.get_or_init(|| {
            font_dir()
                .map(|d| super::register_dir(d, None).is_ok())
                .unwrap_or(false)
        })
    }
}
