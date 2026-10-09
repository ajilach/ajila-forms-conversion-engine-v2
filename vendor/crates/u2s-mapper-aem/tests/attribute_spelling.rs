//! Every attribute of a real package, decoded and encoded again, is the same
//! bytes. AEM (FileVault) spells an attribute one way: `&amp;` `&lt;`
//! `&quot;` and `&#xa;` `&#xd;` `&#x9;`, with `>` and `'` left as they are.
//! A writer that spells a character any other way still writes valid XML,
//! but a package it rewrites no longer matches the file it read, and tools
//! that rewrite rules in place (the feedback repo's sweeps) can no longer
//! edit it byte for byte.
//!
//! The packages are the ones AEM itself wrote, in this repository: this
//! crate's AABF and the UBS layer's deployed packages. Forms and their
//! translation dictionaries both count. The UBS layer's golden packages are
//! not among them: the retired engine wrote those, with its own spelling.

use std::io::Read;

use u2s_mapper_aem::jcr::escape_attribute_value;

/// Every zip under `dir`, recursively.
fn zips(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            zips(&path, out);
        } else if path.extension().is_some_and(|e| e == "zip") {
            out.push(path);
        }
    }
}

fn packages() -> Vec<std::path::PathBuf> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    zips(&manifest.join("tests/fixtures"), &mut found);
    zips(&manifest.join("../u2s-aem-ubs-mcp/tests/fixtures/ubs-packages"), &mut found);
    found.retain(|p| !p.ends_with("script-corpus.zip"));
    found.sort();
    found
}

#[test]
fn every_attribute_of_a_real_package_round_trips_to_its_own_bytes() {
    let attribute = regex::Regex::new(r#"\s([\w:.\-]+)="([^"]*)""#).unwrap();
    let (mut files, mut attributes, mut failures) = (0, 0, Vec::new());
    for package in packages() {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&package).unwrap()).unwrap();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().to_owned();
            if !name.ends_with(".xml") || name.starts_with("__MACOSX/") || !name.contains("jcr_root/") {
                continue;
            }
            let mut xml = String::new();
            if entry.read_to_string(&mut xml).is_err() {
                continue;
            }
            files += 1;
            for capture in attribute.captures_iter(&xml) {
                attributes += 1;
                let raw = &capture[2];
                let value = quick_xml::escape::unescape(raw)
                    .unwrap_or_else(|e| panic!("{name}: {} does not decode: {e}", &capture[1]));
                if escape_attribute_value(&value) != raw {
                    failures.push(format!("{}: {name}: {}", package.display(), &capture[1]));
                }
            }
        }
    }
    assert!(files > 50 && attributes > 10_000, "{files} files, {attributes} attributes");
    assert!(
        failures.is_empty(),
        "{} of {attributes} attributes are spelled differently once written again:\n{}",
        failures.len(),
        failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );
}
