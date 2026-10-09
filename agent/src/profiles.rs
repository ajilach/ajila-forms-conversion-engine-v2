//! The conversion profiles, embedded at compile time from `profiles/`.
//!
//! A profile names a customer: its reference store (see [`crate::references`])
//! and the fonts its source forms are drawn in, which the u2s renderer needs to
//! lay out their text. The output format itself is the vendored UBS layer
//! (`u2s-aem-ubs-mcp`).

use include_dir::{Dir, include_dir};

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
    fn the_ubs_profile_ships_its_fonts() {
        assert!(list_profiles().contains(&"ubs".to_string()));
        assert!(!profile_font_files("ubs").is_empty());
        assert!(profile_font_files("missing").is_empty());
    }
}
