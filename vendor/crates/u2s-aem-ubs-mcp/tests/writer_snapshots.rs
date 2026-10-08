//! The writer's whole output, held to a snapshot of itself.
//!
//! Golden parity (`golden_parity.rs`) holds the writer to the retired engine,
//! but it has to leave out the account-holder cluster and the signature block,
//! which documents now author differently. This test covers everything,
//! including those: each case's package (and bound package and schema) is read
//! the way AEM reads it (`support::package::structure_without`) and compared
//! with `tests/fixtures/writer_snapshots/<case>.snap`.
//!
//! The cases are the golden documents and the form page of every deployed
//! package in `tests/fixtures/ubs-packages/`, loaded and saved again, which
//! carries the passthrough of a real package through the writer.
//!
//! A snapshot is not a reference the way a golden package is: when the output
//! is meant to change, regenerate them with `UPDATE_SNAPSHOTS=1` and say in the
//! commit why the output changed.

use std::path::{Path, PathBuf};

use u2s_aem_ubs_mcp::aem::{AemNode, generate_aem_xml_with_passthrough, parse_aem_zip};
use u2s_aem_ubs_mcp::{UbsAemBuild, UbsAemDocument, encode};

mod support;
use support::package::{canonical, dictionary, first_difference, structure_without, unzip};

const GOLDEN: &[&str] = &["AAOS_033_IT", "AAEV_019_EN", "AABF_019"];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A package as one text: each file under its name, a form file as AEM reads
/// it, a dictionary as its sorted entries.
fn package_text(label: &str, bytes: &[u8]) -> String {
    let mut out = String::new();
    for (name, text) in unzip(bytes) {
        out.push_str(&format!("== {label} {name}\n"));
        if name.contains("/assets/dictionary/") {
            for (key, message) in dictionary(&text) {
                out.push_str(&format!("{key:?} = {message:?}\n"));
            }
        } else if name.ends_with(".xml") {
            out.push_str(&structure_without(&text, &[]));
        } else {
            out.push_str(&canonical(&text));
            out.push('\n');
        }
    }
    out
}

fn build_text(build: &UbsAemBuild) -> String {
    let mut out = package_text("package", &build.package);
    if let Some(bound) = &build.bound_package {
        out.push_str(&package_text("bound", bound));
    }
    if let Some(xsd) = &build.xsd {
        out.push_str("== schema\n");
        out.push_str(xsd);
        out.push('\n');
    }
    out
}

/// The form page of a deployed package, loaded into the working tree and
/// written again: these packages predate the form metadata `decode` reads, so
/// they go through the same load and save as `aem_translated.rs`.
fn reloaded_form(zip: &[u8]) -> String {
    let package = parse_aem_zip(zip).expect("a deployed package parses");
    let languages = support::lift_languages(&package);
    let lifted = support::lift_package(&package);
    let (lowered, _dictionary) = lifted.lower(&package.language, &languages);
    let form_code = match &lowered {
        AemNode::Root { title, .. } => title.clone(),
        _ => String::new(),
    };
    let config = support::ubs_config_for(&package.language, &languages, &form_code);
    let xml = generate_aem_xml_with_passthrough(&lowered, &config, &lifted.passthrough_map());
    format!("== form\n{}", structure_without(&xml, &[]))
}

fn cases() -> Vec<(String, String)> {
    let mut cases = Vec::new();
    for form in GOLDEN {
        let json = std::fs::read_to_string(fixtures().join("golden").join(form).join("document.json"))
            .unwrap();
        let doc = UbsAemDocument::from_json(&serde_json::from_str(&json).unwrap())
            .unwrap_or_else(|e| panic!("{form}: document.json is a document: {e}"));
        let build = encode(&doc).unwrap_or_else(|e| panic!("{form} encodes: {e}"));
        cases.push((format!("golden-{form}"), build_text(&build)));
    }
    let mut packages: Vec<PathBuf> = std::fs::read_dir(fixtures().join("ubs-packages"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "zip"))
        .collect();
    packages.sort();
    for path in packages {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        cases.push((format!("reload-{stem}"), reloaded_form(&std::fs::read(&path).unwrap())));
    }
    cases
}

#[test]
fn the_writer_output_matches_its_snapshots() {
    let dir = fixtures().join("writer_snapshots");
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let mut failures = Vec::new();
    let cases = cases();
    // A snapshot whose case is gone is stale, not a reference.
    if !update {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            let case = name.trim_end_matches(".snap");
            if !cases.iter().any(|(c, _)| c == case) {
                failures.push(format!("{name}: no case writes this snapshot; delete it"));
            }
        }
    }
    for (case, actual) in cases {
        // Some package files end their lines in CRLF; git would rewrite them.
        let actual = actual.replace("\r\n", "\n");
        let path = dir.join(format!("{case}.snap"));
        if update {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, &actual).unwrap();
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(expected) if expected == actual => {}
            Ok(expected) => failures.push(format!(
                "{case}: {}",
                first_difference(&expected, &actual)
            )),
            Err(_) => failures.push(format!(
                "{case}: no snapshot at {}; run with UPDATE_SNAPSHOTS=1",
                path.display()
            )),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
