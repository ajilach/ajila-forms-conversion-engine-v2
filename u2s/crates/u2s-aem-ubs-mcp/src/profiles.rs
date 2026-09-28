//! Embedded profile loading for the AEM and XSD outputs.
//!
//! The entire `profiles/` directory is baked into this crate at compile
//! time. Consumers (CLI, app, server) should load profile data through this
//! module instead of duplicating profile I/O logic.

use crate::aem::{AemConfig, AemProfile, ParsedFragment, parse_fragment_content};
use crate::context::Context;
use crate::xsd::{XsdConfig, XsdProfile, build_xsd_config_from_type_sources};
use include_dir::{Dir, include_dir};
use serde::de::DeserializeOwned;
use std::collections::HashMap;

static PROFILES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/profiles");

/// Result of loading an AEM profile: the parsed profile, its component
/// templates, and its translations.
pub type AemProfileLoad = (AemProfile, HashMap<String, String>, HashMap<String, String>);

/// Return all embedded profile names (top-level profile directories).
pub fn list_profiles() -> Vec<String> {
    PROFILES_DIR
        .dirs()
        .filter_map(|d| d.path().file_name())
        .filter_map(|n| n.to_str())
        .map(String::from)
        .collect()
}

/// Return whether `{profile}/aem/config.toml` exists.
pub fn has_aem_config(name: &str) -> bool {
    has_profile_config(name, "aem")
}

/// Return whether `{profile}/xsd/config.toml` exists.
pub fn has_xsd_config(name: &str) -> bool {
    has_profile_config(name, "xsd")
}

/// Load and parse `{profile}/aem/config.toml` and all top-level `*.xml`
/// component templates from `{profile}/aem/`.
pub fn load_aem_profile(name: &str) -> Result<AemProfileLoad, String> {
    let aem_dir = PROFILES_DIR
        .get_dir(format!("{name}/aem"))
        .ok_or_else(|| format!("Profile '{name}' has no aem/ subdirectory"))?;

    let mut profile: AemProfile = read_profile_config_toml(name, "aem")?;

    // Load translations from the `translations/` directory (per-language TOML files).
    if let Some(translations_dir) = aem_dir.get_dir(format!("{name}/aem/translations")) {
        for entry in translations_dir.files() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml")
                && let Some(lang) = path.file_stem().and_then(|s| s.to_str())
                && let Some(content) = entry.contents_utf8()
            {
                parse_translation_toml(content, lang, &mut profile.default_translations)
                    .map_err(|e| format!("Failed to parse translations/{lang}.toml: {e}"))?;
            }
        }
    }

    let mut templates = HashMap::new();
    for entry in aem_dir.files() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("xml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && let Some(content) = entry.contents_utf8()
        {
            templates.insert(stem.to_string(), content.to_string());
        }
    }

    // Load custom element templates from the `custom/` subdirectory.
    let mut custom_templates = HashMap::new();
    if let Some(custom_dir) = aem_dir.get_dir(format!("{name}/aem/custom")) {
        for entry in custom_dir.files() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("xml")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && let Some(content) = entry.contents_utf8()
            {
                custom_templates.insert(stem.to_string(), content.to_string());
            }
        }
    }

    Ok((profile, templates, custom_templates))
}

/// Build a full `AemConfig` for an embedded profile.
///
/// This includes:
/// - AEM profile + templates
/// - optional XSD binding (requires embedded xsd config when bind_to_xsd=true)
/// - optional embedded fragment scan
pub fn load_aem_config(name: &str, ctx: &Context) -> Result<AemConfig, String> {
    let (profile, templates, custom_templates) = load_aem_profile(name)?;

    let mut config = AemConfig::from_profile(&profile, templates, custom_templates, ctx)
        .map_err(|e| format!("Failed to build AEM config: {e}"))?;

    if config.bind_to_xsd || config.use_fragments {
        if has_xsd_config(name) {
            let mut xsd_config = load_xsd_config(name)?;
            xsd_config.form_code = Some(config.form_code.clone());
            config.xsd_config = Some(xsd_config);
        } else if config.bind_to_xsd {
            return Err(format!(
                "bind_to_xsd=true requires profile '{name}' to provide xsd/config.toml"
            ));
        }
    }

    if config.use_fragments {
        config.fragments =
            load_aem_fragments(name, &config.fragment_ref_prefix, &config.fragment_paths)?;
    }

    Ok(config)
}

/// Load XSD config for an embedded profile.
pub fn load_xsd_config(name: &str) -> Result<XsdConfig, String> {
    let xsd_dir = PROFILES_DIR
        .get_dir(format!("{name}/xsd"))
        .ok_or_else(|| format!("Profile '{name}' has no xsd/ subdirectory"))?;

    let profile: XsdProfile = read_profile_config_toml(name, "xsd")?;

    let mut type_sources: Vec<(String, String)> = Vec::new();
    if let Some(types_dir) = xsd_dir.get_dir(format!("{name}/xsd/types")) {
        let types_root = types_dir.path();
        walk_embedded_dirs(types_dir, &mut |embedded_dir| {
            for file in embedded_dir.files() {
                if file.path().extension().and_then(|e| e.to_str()) == Some("xsd")
                    && let Some(content) = file.contents_utf8()
                {
                    let rel = relative_embedded_path(file.path(), types_root);
                    type_sources.push((rel, content.to_string()));
                }
            }
        });
        type_sources.sort_by(|a, b| a.0.cmp(&b.0));
    }

    let mut config = build_xsd_config_from_type_sources(profile, &type_sources);

    // Index every fragment library in the profile, not just the ones the AEM
    // profile lets the converter substitute: a form may already reference a
    // fragment from any of them, and without its `fragmentModelRoot` the schema
    // would silently omit that element.
    let prefix = load_aem_profile(name)
        .ok()
        .and_then(|(p, _, _)| p.fragment_ref_prefix.clone())
        .unwrap_or_else(|| "/content/dam/formsanddocuments/".to_string());
    if let Ok(fragments) = load_aem_fragments(name, &prefix, &[]) {
        config.fragment_types = fragments
            .into_iter()
            .map(|f| (f.frag_ref, f.xsd_type_name))
            .collect();
    }

    Ok(config)
}

/// Load parsed AEM fragments from `{profile}/aem/fragments`.
///
/// When `fragment_paths` is non-empty, only those specific paths (relative to
/// `fragments/`) are scanned. Each path can be:
/// - A single fragment directory (e.g. `"afforms_ubs_fragmentlib/affrg_Address1"`)
/// - A parent directory to scan recursively (e.g. `"afforms_ubs_fragmentlib"`)
///
/// When empty, all subdirectories are scanned (backward-compatible).
pub fn load_aem_fragments(
    name: &str,
    fragment_ref_prefix: &str,
    fragment_paths: &[String],
) -> Result<Vec<ParsedFragment>, String> {
    let fragments_root = PROFILES_DIR
        .get_dir(format!("{name}/aem/fragments"))
        .ok_or_else(|| format!("Profile '{name}' has no aem/fragments directory"))?;

    let base = fragments_root.path();
    let prefix = fragment_ref_prefix.trim_end_matches('/');
    let mut fragments = Vec::new();

    // If specific paths are requested, iterate over just those.
    // Otherwise, scan all subdirectories (backward-compatible behavior).
    if fragment_paths.is_empty() {
        walk_embedded_dirs(fragments_root, &mut |embedded_dir| {
            if let Some(content_file) = embedded_dir
                .files()
                .find(|f| f.path().file_name().and_then(|n| n.to_str()) == Some(".content.xml"))
                && let Some(content) = content_file.contents_utf8()
                && let Some(fragment) =
                    parse_embedded_fragment(embedded_dir.path(), base, prefix, content)
            {
                fragments.push(fragment);
            }
        });
    } else {
        for path in fragment_paths {
            let path = path.trim_matches('/');
            let full_path = format!("{name}/aem/fragments/{path}");

            // If this resolves to a directory, always scan it recursively.
            // Some fragment libraries have a root `.content.xml` that is NOT
            // a fragment (no fragmentModelRoot). In that case we still must
            // descend into child fragment directories.
            if let Some(subdir) = PROFILES_DIR.get_dir(&full_path) {
                walk_embedded_dirs(subdir, &mut |embedded_dir| {
                    if let Some(content_file) = embedded_dir.files().find(|f| {
                        f.path().file_name().and_then(|n| n.to_str()) == Some(".content.xml")
                    }) && let Some(content) = content_file.contents_utf8()
                        && let Some(fragment) =
                            parse_embedded_fragment(embedded_dir.path(), base, prefix, content)
                    {
                        fragments.push(fragment);
                    }
                });
            }
        }
    }

    fragments.sort_by(|a, b| a.frag_ref.cmp(&b.frag_ref));
    Ok(fragments)
}

fn has_profile_config(name: &str, section: &str) -> bool {
    PROFILES_DIR
        .get_file(format!("{name}/{section}/config.toml"))
        .is_some()
}

fn read_profile_config_toml<T>(name: &str, section: &str) -> Result<T, String>
where
    T: DeserializeOwned,
{
    let config_path = format!("{name}/{section}/config.toml");
    let config_file = PROFILES_DIR
        .get_file(&config_path)
        .ok_or_else(|| format!("Profile '{name}/{section}' has no config.toml"))?;
    let toml_str = config_file
        .contents_utf8()
        .ok_or_else(|| format!("{section}/config.toml is not valid UTF-8"))?;
    toml::from_str::<T>(toml_str).map_err(|e| format!("Failed to parse {section}/config.toml: {e}"))
}

fn walk_embedded_dirs(dir: &Dir<'_>, visit: &mut impl FnMut(&Dir<'_>)) {
    visit(dir);
    for child in dir.dirs() {
        walk_embedded_dirs(child, visit);
    }
}

fn parse_embedded_fragment(
    current_path: &std::path::Path,
    base_path: &std::path::Path,
    fragment_ref_prefix: &str,
    content: &str,
) -> Option<ParsedFragment> {
    let rel = relative_embedded_path(current_path, base_path);
    parse_fragment_content(&rel, fragment_ref_prefix, content)
}

fn normalize_embedded_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn relative_embedded_path(path: &std::path::Path, root: &std::path::Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    normalize_embedded_path(rel)
        .trim_start_matches('/')
        .to_string()
}

/// Resolve a `fragRef` path to the fragment's `.content.xml` content from
/// the embedded profiles.
///
/// Strips known JCR path prefixes (`/content/dam/formsanddocuments/` or
/// `/content/forms/af/`) and looks up the fragment under each profile's
/// `aem/fragments/` directory.  Returns the XML as a `String` if found.
pub fn resolve_embedded_fragment_xml(frag_ref: &str) -> Option<String> {
    let (_, fragment_xml_path) = resolve_embedded_fragment_paths(frag_ref)?;
    PROFILES_DIR
        .get_file(&fragment_xml_path)
        .and_then(|f| f.contents_utf8())
        .map(|s| s.to_string())
}

/// Load Sling i18n dictionary files for an embedded fragment.
///
/// Returns a map of `(language, xml_content)` pairs from the fragment's
/// `_jcr_content/guideContainer/assets/dictionary/` directory.
pub fn resolve_embedded_fragment_dictionaries(frag_ref: &str) -> Vec<(String, String)> {
    let Some((profile_and_relative, _)) = resolve_embedded_fragment_paths(frag_ref) else {
        return Vec::new();
    };

    let dict_dir_path =
        format!("{profile_and_relative}/_jcr_content/guideContainer/assets/dictionary");

    let Some(dict_dir) = PROFILES_DIR.get_dir(&dict_dir_path) else {
        return Vec::new();
    };

    dict_dir
        .files()
        .filter_map(|f| {
            let filename = f.path().file_name()?.to_str()?;
            if !filename.ends_with(".xml") {
                return None;
            }
            let lang = filename.strip_suffix(".xml")?;
            let content = f.contents_utf8()?;
            Some((lang.to_string(), content.to_string()))
        })
        .collect()
}

/// Load all dictionaries from every fragment in the same library as the given fragRef.
///
/// In AEM, Sling dictionaries are shared across a content subtree. Fragments in the same
/// library (e.g. `afforms_italy_fragmentlib`) share dictionaries even if they don't
/// directly reference each other. This function loads all `dictionary/*.xml` files
/// from all sibling fragments to enable correct translation resolution.
pub fn resolve_embedded_library_dictionaries(frag_ref: &str) -> Vec<(String, String)> {
    let relative = frag_ref
        .strip_prefix("/content/dam/formsanddocuments/")
        .or_else(|| frag_ref.strip_prefix("/content/forms/af/"));

    let Some(relative) = relative else {
        return Vec::new();
    };

    // Extract the library path (first segment of the relative path)
    // e.g. "afforms_italy_fragmentlib/affrg_ClientSignature1" → "afforms_italy_fragmentlib"
    let library = relative.split('/').next().unwrap_or(relative);

    let mut results = Vec::new();
    for profile_dir in PROFILES_DIR.dirs() {
        let Some(profile_name) = profile_dir.path().file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let lib_path = format!("{profile_name}/aem/fragments/{library}");
        let Some(lib_dir) = PROFILES_DIR.get_dir(&lib_path) else {
            continue;
        };

        // Recursively collect all dictionary XML files from the library
        collect_dictionary_files(lib_dir, &mut results);
    }

    results
}

/// Recursively collect dictionary XML files from a directory tree.
fn collect_dictionary_files(dir: &Dir<'_>, results: &mut Vec<(String, String)>) {
    let dir_name = dir
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    if dir_name == "dictionary" {
        // This is a dictionary directory — collect its XML files
        for file in dir.files() {
            if let Some(filename) = file.path().file_name().and_then(|n| n.to_str())
                && let Some(lang) = filename.strip_suffix(".xml")
                && let Some(content) = file.contents_utf8()
            {
                results.push((lang.to_string(), content.to_string()));
            }
        }
    }

    // Recurse into subdirectories
    for subdir in dir.dirs() {
        collect_dictionary_files(subdir, results);
    }
}

/// Resolve a fragRef to (profile_name/aem/fragments/relative, xml_path).
fn resolve_embedded_fragment_paths(frag_ref: &str) -> Option<(String, String)> {
    let relative = frag_ref
        .strip_prefix("/content/dam/formsanddocuments/")
        .or_else(|| frag_ref.strip_prefix("/content/forms/af/"))?;

    for profile_dir in PROFILES_DIR.dirs() {
        let profile_name = profile_dir.path().file_name()?.to_str()?;
        let base = format!("{profile_name}/aem/fragments/{relative}");
        let xml_path = format!("{base}/.content.xml");
        if PROFILES_DIR.get_file(&xml_path).is_some() {
            return Some((base, xml_path));
        }
    }

    None
}

/// Parse a translation TOML file (from the `translations/` directory) and merge
/// entries into the `default_translations` map.
///
/// Expected format:
/// ```toml
/// [translations]
/// "Master text" = "Translated text"
/// ```
pub fn parse_translation_toml(
    toml_str: &str,
    lang: &str,
    translations: &mut HashMap<String, HashMap<String, String>>,
) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct TranslationFile {
        #[serde(default)]
        translations: HashMap<String, String>,
    }

    let file: TranslationFile = toml::from_str(toml_str).map_err(|e| e.to_string())?;

    for (key, message) in file.translations {
        translations
            .entry(key)
            .or_default()
            .insert(lang.to_string(), message);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ubs_profile_has_configs() {
        assert!(has_aem_config("ubs"));
        assert!(has_xsd_config("ubs"));
        assert!(!has_aem_config("missing-profile"));
        assert!(!has_xsd_config("missing-profile"));
    }

    #[test]
    fn embedded_xsd_loader_fails_without_profile_config() {
        let err = load_xsd_config("missing-profile").expect_err("missing profile");
        assert!(
            err.contains("has no xsd/ subdirectory") || err.contains("has no config.toml"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn embedded_xsd_loader_discovers_nested_type_files() {
        let cfg = load_xsd_config("ubs").expect("load ubs xsd config");

        assert!(
            cfg.type_to_file.contains_key("AddressType"),
            "Expected AddressType from nested xsd/types/** files"
        );
        assert!(
            cfg.registered_types.contains_key("AddressType"),
            "Expected parsed registered type AddressType"
        );
    }

    #[test]
    fn embedded_fragment_loader_parses_known_fragments() {
        let fragments =
            load_aem_fragments("ubs", "/content/forms/af/", &[]).expect("load embedded fragments");

        assert!(
            !fragments.is_empty(),
            "Expected at least one embedded AEM fragment"
        );
        assert!(
            fragments.iter().any(|f| f.xsd_type_name == "AddressType"),
            "Expected AddressType fragment in embedded profile"
        );
    }

    #[test]
    fn embedded_fragment_loader_scans_fragment_library_path_recursively() {
        // Regression: when fragment_paths points to a fragment library directory
        // that also contains its own `.content.xml`, we must still recurse into
        // child fragment directories.
        let fragments = load_aem_fragments(
            "ubs",
            "/content/dam/formsanddocuments/",
            &["afforms_ubs_fragmentlib".to_string()],
        )
        .expect("load embedded fragments from explicit library path");

        assert!(
            !fragments.is_empty(),
            "Expected at least one fragment from afforms_ubs_fragmentlib"
        );
        assert!(
            fragments.iter().any(|f| f.xsd_type_name == "AddressType"),
            "Expected AddressType fragment when scanning afforms_ubs_fragmentlib recursively"
        );
    }

    #[test]
    fn embedded_fragment_loader_supports_explicit_fragment_directory_path() {
        let fragments = load_aem_fragments(
            "ubs",
            "/content/dam/formsanddocuments/",
            &["afforms_ubs_fragmentlib/affrg_AddressGeneric1".to_string()],
        )
        .expect("load embedded explicit fragment directory");

        assert_eq!(
            fragments.len(),
            1,
            "Expected exactly one fragment for explicit directory path"
        );
        assert_eq!(
            fragments[0].xsd_type_name, "AddressType",
            "Explicit fragment directory should resolve to AddressType"
        );
    }
}
