//! Desktop file handoff: save an artefact to the user's Downloads folder and
//! show it to them, either revealed in the file manager or opened directly.

use std::path::{Path, PathBuf};

/// The folder artefacts are saved to. The one place that decides.
pub fn downloads_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine the home directory.")?;
    Ok(home.join("Downloads"))
}

/// Where `filename` should actually land in `dir`.
///
/// Artefact names are keyed only by the form code, so two conversions of the
/// same form produce identical names — and the second download used to destroy
/// the first without a word. The rule is the one browsers use, and that users
/// therefore already expect: never overwrite somebody else's file, but do
/// overwrite your own.
///
/// `previous` is where this same tab last wrote this same artefact. Replacing it
/// in place is what stops a feedback re-run, or a second press of the same
/// button, from leaving a trail of `(2)`, `(3)`, `(4)`.
///
/// `exists` is injected so the rule can be tested without touching a disk.
pub fn download_target(
    dir: &Path,
    filename: &str,
    previous: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let direct = dir.join(filename);

    // Our own file from last time — replace it rather than pile up beside it.
    if let Some(previous) = previous
        && previous.parent() == Some(dir)
        && exists(previous)
    {
        return previous.to_path_buf();
    }
    if !exists(&direct) {
        return direct;
    }

    let (stem, extension) = match filename.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        // No extension, or a leading-dot name like `.profile`.
        _ => (filename, String::new()),
    };
    // Bounded so a directory full of collisions cannot spin forever.
    for n in 2..1000 {
        let candidate = dir.join(format!("{stem} ({n}){extension}"));
        if !exists(&candidate) {
            return candidate;
        }
    }
    direct
}

/// Write `data` to the user's Downloads folder and return where it landed.
///
/// `previous` is where this tab last wrote this artefact, if anywhere — see
/// [`download_target`].
pub fn save_to_downloads(
    filename: &str,
    data: &[u8],
    previous: Option<&Path>,
) -> Result<PathBuf, String> {
    let dir = downloads_dir()?;
    let path = download_target(&dir, filename, previous, |p| p.exists());
    std::fs::write(&path, data).map_err(|e| format!("Could not save {}: {e}", path.display()))?;
    Ok(path)
}

/// Save an artefact and reveal it in the file manager.
///
/// A file manager that refuses to open is not worth failing over — the file the
/// user asked for is on disk either way — so only the save can fail here.
pub fn download_file(
    data: &[u8],
    filename: &str,
    previous: Option<&Path>,
) -> Result<PathBuf, String> {
    let path = save_to_downloads(filename, data, previous)?;
    reveal_in_file_explorer(&path);
    Ok(path)
}

/// Reveal an existing file in the platform's file manager.
///
/// Best-effort: a file manager that refuses to open is not worth reporting, the
/// file is on disk either way.
pub fn reveal_in_file_explorer(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();

    // xdg-open cannot select a file, so revealing means opening its folder.
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .spawn();

    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer")
        .args(["/select,", &path.to_string_lossy()])
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        PathBuf::from("/downloads")
    }

    #[test]
    fn a_free_name_is_used_as_it_is() {
        let path = download_target(&dir(), "forms-package-AAOV.zip", None, |_| false);
        assert_eq!(path, dir().join("forms-package-AAOV.zip"));
    }

    /// Two tabs converting the same form produce the same artefact name. The
    /// second must not destroy the first.
    #[test]
    fn a_name_another_tab_already_took_is_suffixed() {
        let taken = |p: &Path| p == dir().join("forms-package-AAOV.zip");
        assert_eq!(
            download_target(&dir(), "forms-package-AAOV.zip", None, taken),
            dir().join("forms-package-AAOV (2).zip")
        );

        // A run of collisions keeps counting rather than giving up.
        let mut taken_names = vec!["forms-package-AAOV.zip".to_string()];
        taken_names.extend((2..5).map(|n| format!("forms-package-AAOV ({n}).zip")));
        let taken = move |p: &Path| {
            taken_names
                .iter()
                .any(|n| p == dir().join(n).as_path())
        };
        assert_eq!(
            download_target(&dir(), "forms-package-AAOV.zip", None, taken),
            dir().join("forms-package-AAOV (5).zip")
        );
    }

    /// Pressing the same button twice, or downloading again after a feedback
    /// re-run, should replace the file rather than pile up beside it.
    #[test]
    fn a_tab_overwrites_its_own_previous_download() {
        let previous = dir().join("forms-package-AAOV (2).zip");
        let taken = |_: &Path| true;
        assert_eq!(
            download_target(&dir(), "forms-package-AAOV.zip", Some(&previous), taken),
            previous
        );
    }

    /// The user may have moved or deleted it in the meantime.
    #[test]
    fn a_previous_download_that_is_gone_is_recreated() {
        let previous = dir().join("forms-package-AAOV.zip");
        assert_eq!(
            download_target(&dir(), "forms-package-AAOV.zip", Some(&previous), |_| false),
            previous
        );
    }

    /// A remembered path from another folder is not this folder's business.
    #[test]
    fn a_previous_download_elsewhere_is_ignored() {
        let previous = PathBuf::from("/somewhere/else/forms-package-AAOV.zip");
        let taken = |p: &Path| p == dir().join("forms-package-AAOV.zip");
        assert_eq!(
            download_target(&dir(), "forms-package-AAOV.zip", Some(&previous), taken),
            dir().join("forms-package-AAOV (2).zip")
        );
    }

    /// The suffix goes before the extension, not after it, or the file stops
    /// opening in the tool that made it.
    #[test]
    fn the_suffix_keeps_the_extension_last() {
        let taken = |p: &Path| !p.to_string_lossy().contains('(');
        assert_eq!(
            download_target(&dir(), "schema-AAOV.xsd", None, taken),
            dir().join("schema-AAOV (2).xsd")
        );
        // A name with no extension still gets a usable suffix.
        assert_eq!(
            download_target(&dir(), "agent-log", None, taken),
            dir().join("agent-log (2)")
        );
    }
}
