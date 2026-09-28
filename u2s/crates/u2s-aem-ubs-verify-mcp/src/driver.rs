//! [`UbsDriver`]: the `u2s_aem_verify_core::driver::FormDriver` this binary
//! supplies -- how to open a UBS form (`crate::ubs_metadata`'s derived
//! `mandator`/`afAcceptLang`), how to recognise its terminal panel (the
//! summary panel, not a submit button -- see `crate::ubs_js`'s own module
//! doc for why UBS's toolbar hides `submit` entirely), and how to actually
//! submit it (`window.forms.ubs.navigation.submit(...)`, not a raw
//! `guideBridge.submit()`).

use serde_json::{Value, json};

use u2s_aem_verify_core::driver::FormDriver;
use u2s_aem_verify_core::package_check::PackageInspection;

use crate::ubs_js;
use crate::ubs_metadata::{self, UbsMetadataError};

pub struct UbsDriver {
    /// `U2S_AEM_VERIFY_UBS_MANDATOR`, when set -- overrides
    /// `UbsFormMetadata::select`'s default of "the first declared entity".
    /// See `crate::ubs_metadata::UbsFormMetadata::select`'s own doc for why
    /// an unknown override is a hard error rather than a silent fallback.
    mandator_override: Option<String>,
}

impl UbsDriver {
    pub fn new(mandator_override: Option<String>) -> Self {
        Self { mandator_override }
    }

    pub fn from_env() -> Self {
        Self::new(
            std::env::var("U2S_AEM_VERIFY_UBS_MANDATOR")
                .ok()
                .filter(|v| !v.is_empty()),
        )
    }
}

impl FormDriver for UbsDriver {
    fn form_url_query(&self, package: &PackageInspection) -> Result<Vec<(String, String)>, String> {
        let metadata = ubs_metadata::extract(package).map_err(|err| err.to_string())?;
        let params = metadata
            .select(self.mandator_override.as_deref())
            .map_err(|err| err.to_string())?;
        Ok(params.query())
    }

    fn terminal_panel_js(&self) -> String {
        ubs_js::IS_SUMMARY_PANEL.to_owned()
    }

    fn has_next_js(&self) -> String {
        ubs_js::HAS_NEXT.to_owned()
    }

    fn click_next_js(&self) -> String {
        ubs_js::CLICK_NEXT.to_owned()
    }

    fn click_prev_js(&self) -> String {
        ubs_js::CLICK_PREV.to_owned()
    }

    fn terminal_signal_label(&self) -> &'static str {
        "UBS's own summary panel (.summaryComponent)"
    }

    fn submit_js(&self) -> String {
        ubs_js::SUBMIT.to_owned()
    }

    fn submit_failed_message(&self) -> &'static str {
        "window.forms.ubs.navigation.submit(...) is missing or did not report success -- check \
         the UBS clientlib actually loaded (see this crate's module doc)"
    }

    fn last_panel_checks_js(&self, package: &PackageInspection) -> Option<String> {
        let metadata = ubs_metadata::extract(package).ok()?;
        let params = metadata.select(self.mandator_override.as_deref()).ok()?;
        Some(ubs_js::last_panel_checks(&params.mandator))
    }

    fn package_summary_extension(&self, package: &PackageInspection) -> Value {
        match ubs_metadata::extract(package) {
            Ok(metadata) => {
                let selected = metadata.select(self.mandator_override.as_deref());
                json!({
                    "ubs": {
                        "formcode": metadata.formcode,
                        "master_language": metadata.master_language,
                        "entities": metadata.entities.iter().map(|e| json!({
                            "entity": e.entity,
                            "languages": e.languages,
                            "cdoks": e.cdoks,
                        })).collect::<Vec<_>>(),
                        "dam_redacto_summary": metadata.dam_redacto_summary,
                        "selected_mandator": selected.as_ref().ok().map(|p| p.mandator.clone()),
                        "selected_language": selected.as_ref().ok().map(|p| p.language.clone()),
                        "selection_error": selected.err().map(|err: UbsMetadataError| err.to_string()),
                    }
                })
            }
            Err(err) => json!({ "ubs": { "error": err.to_string() } }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use u2s_aem_verify_core::package_check::PackageSummary;

    const AAOV_LIKE_XML: &str = r#"<jcr:root>
        <metadata
            sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/metadata"
            formrange_code="AAOV"
            formrange_afmasterlanguage="IT">
            <entities>
                <item0 formrange_entity="033" formrange_language="IT">
                    <cdoks><item0 formrange_cdokinfo="66830"/></cdoks>
                </item0>
            </entities>
        </metadata>
    </jcr:root>"#;

    fn package() -> PackageInspection {
        PackageInspection {
            summary: PackageSummary {
                form_jcr_path: "/content/forms/af/afforms_italy_all/af_aa/AF_AAOV".to_owned(),
                form_name: "AF_AAOV".to_owned(),
                looks_like_it_has_dor: false,
                looks_like_a_wizard: true,
            },
            form_content_xml: AAOV_LIKE_XML.as_bytes().to_vec(),
            dam_content_xml: None,
        }
    }

    #[test]
    fn form_url_query_derives_mandator_and_language_from_the_package() {
        let driver = UbsDriver::new(None);
        let query = driver.form_url_query(&package()).expect("ok");
        assert!(query.contains(&("mandator".to_owned(), "033".to_owned())));
        assert!(query.contains(&("afAcceptLang".to_owned(), "it".to_owned())));
        assert!(query.contains(&("wcmmode".to_owned(), "disabled".to_owned())));
    }

    #[test]
    fn form_url_query_fails_closed_on_an_unknown_mandator_override() {
        let driver = UbsDriver::new(Some("999".to_owned()));
        assert!(driver.form_url_query(&package()).is_err());
    }

    #[test]
    fn package_summary_extension_reports_the_selected_mandator() {
        let driver = UbsDriver::new(None);
        let extension = driver.package_summary_extension(&package());
        assert_eq!(extension["ubs"]["formcode"], "AAOV");
        assert_eq!(extension["ubs"]["selected_mandator"], "033");
        assert_eq!(extension["ubs"]["selected_language"], "it");
    }
}
