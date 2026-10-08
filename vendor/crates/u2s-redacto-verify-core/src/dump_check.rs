//! Offline structural validation of a dump -- the check `verify_run`'s own
//! `dry_run: true` performs internally, and the whole job of the
//! `verify_dump_check` tool. Never touches Docker or a network, mirroring
//! `u2s-aem-verify-core`'s own `package_check` module.
//!
//! Reuses [`u2s_mapper_redacto::decode::decode`] rather than re-implementing
//! a second structural check: decode is already lossless-or-error and
//! already calls [`u2s_redacto::RedactoDocument::validate`] internally, so
//! a dump that decodes cleanly has already been proven well-formed SQL,
//! referentially closed, and semantically valid all at once.

use u2s_mapper_redacto::decode::DecodeError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpCheckReport {
    pub ok: bool,
    /// The recovered `document_id`, when the dump decoded successfully.
    pub document_id: Option<String>,
    pub languages: Vec<String>,
    pub asset_count: usize,
    /// Present iff `ok` is false -- what could not be represented or
    /// validated, in the decoder's own words.
    pub problem: Option<String>,
}

pub fn check(bytes: &[u8]) -> DumpCheckReport {
    match u2s_mapper_redacto::decode::decode(bytes) {
        Ok(valid) => {
            let doc = valid.document();
            DumpCheckReport {
                ok: true,
                document_id: Some(doc.metadata.document_id.to_string()),
                languages: doc.metadata.languages.iter().map(|l| l.to_string()).collect(),
                asset_count: doc.assets.len(),
                problem: None,
            }
        }
        Err(err) => DumpCheckReport {
            ok: false,
            document_id: None,
            languages: Vec::new(),
            asset_count: 0,
            problem: Some(describe(&err)),
        },
    }
}

fn describe(err: &DecodeError) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_fixture_checks_out() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../u2s-mapper-redacto/tests/fixtures/redacto-AAEV_019.sql"
        ))
        .expect("fixture reads");
        let report = check(&bytes);
        assert!(report.ok, "{report:?}");
        assert_eq!(report.document_id.as_deref(), Some("aaev_019"));
        assert_eq!(report.asset_count, 18);
        assert_eq!(report.languages, vec!["en"]);
    }

    #[test]
    fn garbage_bytes_are_reported_as_a_problem_not_a_panic() {
        let report = check(b"not a dump at all");
        assert!(!report.ok);
        assert!(report.problem.is_some());
    }

    #[test]
    fn an_empty_body_dump_is_reported_as_a_problem() {
        // A dump with a document row but no body assetContainer -- the
        // encoder never produces this (validate() refuses it upstream),
        // but a hand-crafted or corrupted dump might.
        let sql = format!(
            "{}\n{}\n{}\n{}\n",
            "BEGIN;",
            "INSERT INTO app_redacto.documents (id, created, document_id, form_path, configuration) VALUES ('d', '1970-01-01 00:00:00.000', 'empty_doc', '/content/forms/af/redacto-documents/empty_doc', '{\"$schema\":\"redacto-document/v2\",\"document\":{\"id\":\"empty_doc\",\"title\":\"Empty\"},\"header\":[],\"body\":[],\"footer\":[]}');",
            "INSERT INTO app_redacto.document_version (id, created, language, version, status, document_fk_id) VALUES ('dv', '1970-01-01 00:00:00.000', 'en', 1, 'DRAFT', 'd');",
            "INSERT INTO app_redacto.ownerships (id, created, owner_id, owner_type, ownership_type, object_id, object_type) VALUES ('o', '1970-01-01 00:00:00.000', 'admin', 'USER', 'OWNER', 'empty_doc', 'DOCUMENT');"
        ) + "COMMIT;\n";
        let report = check(sql.as_bytes());
        assert!(!report.ok);
        assert!(
            report.problem.as_deref().unwrap_or_default().contains("body"),
            "{report:?}"
        );
    }
}
