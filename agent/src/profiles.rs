//! The conversion profiles, embedded at compile time from `profiles/`.
//!
//! A profile names a customer: its reference store (see [`crate::references`])
//! and the fonts its source forms are drawn in, which the u2s renderer needs to
//! lay out their text. The output formats themselves are the vendored UBS
//! layers (`u2s-aem-ubs-mcp`, `u2s-redacto-ubs-mcp`), so every profile offers
//! both targets.

use include_dir::{Dir, include_dir};

use crate::OutputTarget;

static PROFILES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../profiles");

/// Every embedded profile's name.
pub fn list_profiles() -> Vec<String> {
    let mut names: Vec<String> = PROFILES_DIR
        .dirs()
        .filter_map(|d| d.path().file_name()?.to_str().map(String::from))
        .collect();
    names.sort();
    names
}

/// The output targets `name` can produce, in the order they are offered. Empty
/// for a profile that does not exist.
pub fn profile_targets(name: &str) -> Vec<OutputTarget> {
    if PROFILES_DIR.get_dir(name).is_some() {
        OutputTarget::ALL.to_vec()
    } else {
        Vec::new()
    }
}

/// The `.ttf` / `.otf` files in `{profile}/parser/fonts/`, as (file stem,
/// bytes). A profile without the directory has none.
pub fn profile_font_files(name: &str) -> Vec<(String, &'static [u8])> {
    let Some(fonts) = PROFILES_DIR.get_dir(format!("{name}/parser/fonts")) else {
        return Vec::new();
    };
    fonts
        .files()
        .filter(|file| {
            let ext = file
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            matches!(ext.as_deref(), Some("ttf") | Some("otf"))
        })
        .map(|file| {
            let stem = file
                .path()
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            (stem, file.contents())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ubs_profile_ships_its_fonts_and_both_targets() {
        assert!(list_profiles().contains(&"ubs".to_string()));
        assert_eq!(profile_targets("ubs"), OutputTarget::ALL.to_vec());
        assert!(!profile_font_files("ubs").is_empty());
        assert!(profile_targets("missing").is_empty());
    }
}
