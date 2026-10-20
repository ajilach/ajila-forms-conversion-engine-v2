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
use std::sync::OnceLock;

use crate::XfaError;
use crate::xfa::font_manager::{RegisteredFace, get_font_manager, register_profile_font_data};

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

    let manager = get_font_manager();
    let mut manager = manager
        .lock()
        .map_err(|e| XfaError::Font(format!("font manager lock: {e}")))?;

    let mut registered = Vec::new();
    let mut faces: Vec<(PathBuf, &'static [u8], u16, bool)> = Vec::new();

    for path in &files {
        let bytes = std::fs::read(path)
            .map_err(|e| XfaError::Font(format!("cannot read {}: {e}", path.display())))?;
        // The manager's API takes `&'static [u8]`. Leaking here is bounded:
        // fonts are registered once at boot, not per document.
        let data: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        match RegisteredFace::parse(data) {
            Ok(face) => faces.push((path.clone(), data, face.weight, face.italic)),
            Err(e) => {
                log::warn!("{} is not a usable font file: {e}", path.display());
                continue;
            }
        }
        register_profile_font_data(&mut manager, data);
        registered.push(path.clone());
    }

    let candidates: Vec<(PathBuf, u16, bool)> =
        faces.iter().map(|(p, _, w, i)| (p.clone(), *w, *i)).collect();
    let fallback = choose_fallback(&candidates, fallback_stem)
        .ok_or_else(|| XfaError::Font("no fallback font chosen".into()))?;
    let data = faces
        .iter()
        .find(|(p, ..)| *p == fallback)
        .map(|(_, data, ..)| *data)
        .ok_or_else(|| XfaError::Font("fallback font not loaded".into()))?;
    manager.set_fallback(data);

    Ok(RegisteredFonts {
        registered,
        fallback,
    })
}

/// Pick the fallback deterministically, from what the faces themselves
/// say: the preferred file stem when one is named, else the upright face
/// whose weight is closest to regular (400), by `OS/2` weight and style.
/// Never directory order: every unresolvable typeface in the document ends
/// up wearing the fallback, and a bold or italic one would make that
/// obvious everywhere.
fn choose_fallback(faces: &[(PathBuf, u16, bool)], preferred_stem: Option<&str>) -> Option<PathBuf> {
    let stem_of = |p: &PathBuf| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
    };

    if let Some(want) = preferred_stem {
        let want = want.to_ascii_lowercase();
        if let Some((hit, ..)) = faces.iter().find(|(p, ..)| stem_of(p) == want) {
            return Some(hit.clone());
        }
    }

    faces
        .iter()
        .min_by_key(|(p, weight, italic)| (u8::from(*italic), weight.abs_diff(400), stem_of(p)))
        .map(|(p, ..)| p.clone())
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

/// The one registration this process makes: what was asked for, and what
/// came of it. The error is kept as a string because [`XfaError`] is not
/// `Clone` and a memoised failure is handed out on every later call.
struct Registration {
    dir: PathBuf,
    fallback_stem: Option<String>,
    outcome: Result<RegisteredFonts, String>,
}

/// Interior mutability, deliberately: the font manager is process-global, so
/// the record of having filled it has to be too. Set once, never changed.
static REGISTRATION: OnceLock<Registration> = OnceLock::new();

/// [`register_dir`], at most once per process.
///
/// Every later call with the same arguments returns the first outcome,
/// including a failure, without touching the filesystem again. This is the
/// entry point for anything that may run more than once (a request handler,
/// a test): `register_dir` leaks each font file, which is bounded only if it
/// runs once.
///
/// A later call naming a different directory or fallback is an error, not a
/// second registration: the manager would end up holding both sets and a
/// fallback that depends on call order.
pub fn register_dir_once(
    dir: impl AsRef<Path>,
    fallback_stem: Option<&str>,
) -> Result<&'static RegisteredFonts, XfaError> {
    let dir = dir.as_ref();
    // Compared canonically, so `./fonts` and its absolute spelling are one
    // directory. A directory that cannot be canonicalised does not exist, and
    // `register_dir` reports that.
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let registration = REGISTRATION.get_or_init(|| Registration {
        dir: key.clone(),
        fallback_stem: fallback_stem.map(str::to_owned),
        outcome: register_dir(dir, fallback_stem).map_err(|e| e.to_string()),
    });
    if registration.dir != key || registration.fallback_stem.as_deref() != fallback_stem {
        return Err(XfaError::Font(format!(
            "fonts were already registered from {} (fallback {:?}); refusing to also \
             register {} (fallback {:?})",
            registration.dir.display(),
            registration.fallback_stem,
            key.display(),
            fallback_stem,
        )));
    }
    registration
        .outcome
        .as_ref()
        .map_err(|e| XfaError::Font(e.clone()))
}

/// Register from `U2S_FONT_DIR`, which is how the server is configured.
pub fn register_from_env() -> Result<&'static RegisteredFonts, XfaError> {
    let dir = std::env::var("U2S_FONT_DIR").map_err(|_| {
        XfaError::Font(
            "U2S_FONT_DIR is not set — XFA rendering cannot lay text out without fonts, \
             and would silently produce wrong page breaks if it tried"
                .into(),
        )
    })?;
    register_dir_once(dir, std::env::var("U2S_FONT_FALLBACK").ok().as_deref())
}

/// Register the repository's test fonts for this crate's unit tests.
///
/// Panics rather than reporting: the fonts are committed, so a failure means a
/// broken checkout, and a test that skipped instead would count as a pass.
#[cfg(test)]
pub(crate) fn register_test_fonts() {
    register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_upright_face_closest_to_regular_is_the_fallback() {
        let faces: Vec<(PathBuf, u16, bool)> = vec![
            (PathBuf::from("frutiger-bold.ttf"), 700, false),
            (PathBuf::from("frutiger-italic.ttf"), 300, true),
            (PathBuf::from("frutiger-light.ttf"), 300, false),
        ];
        // Alphabetically first is bold; the light upright face is closer to
        // regular, and upright beats italic at any weight.
        assert_eq!(
            choose_fallback(&faces, None),
            Some(PathBuf::from("frutiger-light.ttf"))
        );
        let with_regular: Vec<(PathBuf, u16, bool)> =
            vec![(PathBuf::from("a-bold.ttf"), 700, false), (PathBuf::from("z-regular.ttf"), 400, false)];
        assert_eq!(
            choose_fallback(&with_regular, None),
            Some(PathBuf::from("z-regular.ttf")),
            "a regular face must win over an alphabetically earlier bold one"
        );
    }
    #[test]
    fn an_explicit_preference_wins() {
        let faces: Vec<(PathBuf, u16, bool)> =
            vec![(PathBuf::from("a-regular.ttf"), 400, false), (PathBuf::from("dejavu-sans.ttf"), 400, false)];
        assert_eq!(
            choose_fallback(&faces, Some("dejavu-sans")),
            Some(PathBuf::from("dejavu-sans.ttf"))
        );
    }
}
