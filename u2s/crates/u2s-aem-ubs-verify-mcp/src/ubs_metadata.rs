//! Extracts a UBS form's own authored `mandator`/language entities from its
//! metadata component (`sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/metadata"`) --
//! the same node `ajila-forms-ubs`'s own `FormMetadataService.getEntityForMandator`
//! looks an entity up in server-side, at submit time. Reading it offline
//! from the package, before ever opening the form in a browser, is what
//! lets [`crate::driver::UbsDriver`] open the form with URL parameters
//! (`mandator`, `afAcceptLang`) that actually resolve to a real entity --
//! without them, `FormMetadataService` throws `FormMetadataException: No
//! metadata information for mandator . Formcode: <code>`, the exact 500
//! this crate exists to avoid.
//!
//! A metadata component looks like this (from a real `AAOV_033` package):
//!
//! ```xml
//! <metadata
//!     sling:resourceType=".../components/controls/metadata"
//!     formrange_code="AAOV"
//!     formrange_afmasterlanguage="IT">
//!     <entities>
//!         <item0 formrange_entity="033" formrange_language="IT">
//!             <cdoks>
//!                 <item0 formrange_cdokinfo="66830" .../>
//!             </cdoks>
//!         </item0>
//!     </entities>
//! </metadata>
//! ```

use u2s_mapper_aem::jcr::tree::{JcrXmlError, find_node_by_resource_type, parse_jcr_xml};

use u2s_aem_verify_core::package_check::PackageInspection;

const METADATA_RESOURCE_TYPE: &str =
    "ajila-forms-customers/ajila-forms-ubs/components/controls/metadata";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UbsEntity {
    pub entity: String,
    /// From `formrange_language`, comma-separated in the source attribute
    /// (`"EN,DE,SP"` on `AF_AABF`) -- split here so a caller never
    /// re-parses the same convention.
    pub languages: Vec<String>,
    pub cdoks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UbsFormMetadata {
    pub formcode: String,
    pub master_language: String,
    pub entities: Vec<UbsEntity>,
    /// The DAM asset's own `jcr:content/metadata/redactoSummary` property
    /// -- one of the three conditions `ajila-forms-ubs`'s own
    /// `DorRenderingExecutor.isSummaryOutput()` checks before routing
    /// submit through Redacto rather than AEM's native (and, on this
    /// workspace's ARM Docker image, non-functional) XDP rendering path.
    /// Informational for `verify_package_check`'s own structured result --
    /// `crate::driver::UbsDriver` never branches on it, since the actual
    /// routing decision happens server-side regardless of what this crate
    /// reports about it beforehand.
    pub dam_redacto_summary: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum UbsMetadataError {
    #[error(
        "no metadata component ({METADATA_RESOURCE_TYPE}) found in this package's form page -- \
         a UBS form cannot be opened without one, since its mandator/language come from there"
    )]
    NoMetadataComponent,
    #[error("the metadata component's {0:?} attribute is missing")]
    MissingAttribute(&'static str),
    #[error("the metadata component declares no entities under its own `entities` node")]
    NoEntities,
    #[error(
        "U2S_AEM_VERIFY_UBS_MANDATOR={requested:?} does not match any entity this package's \
         metadata component declares (known: {known:?})"
    )]
    UnknownMandator {
        requested: String,
        known: Vec<String>,
    },
    #[error(transparent)]
    Xml(#[from] JcrXmlError),
}

/// The query parameters `crate::driver::UbsDriver::form_url_query` appends
/// to the form's own `.html` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenParams {
    pub mandator: String,
    pub language: String,
    pub formcode: String,
}

impl OpenParams {
    pub fn query(&self) -> Vec<(String, String)> {
        vec![
            ("wcmmode".to_owned(), "disabled".to_owned()),
            ("afAcceptLang".to_owned(), self.language.clone()),
            ("mandator".to_owned(), self.mandator.clone()),
            ("formcode".to_owned(), self.formcode.clone()),
        ]
    }
}

/// Parses the form page's own raw `.content.xml` (already read once,
/// offline, by `u2s_aem_verify_core::package_check::inspect`) for its
/// metadata component, and, when the package declares one, the DAM asset's
/// own `redactoSummary` flag.
pub fn extract(package: &PackageInspection) -> Result<UbsFormMetadata, UbsMetadataError> {
    let form_xml = String::from_utf8_lossy(&package.form_content_xml);
    let form_root = parse_jcr_xml(&form_xml)?;
    let metadata_node = find_node_by_resource_type(&form_root, METADATA_RESOURCE_TYPE)
        .ok_or(UbsMetadataError::NoMetadataComponent)?;

    let formcode = metadata_node
        .attr("formrange_code")
        .ok_or(UbsMetadataError::MissingAttribute("formrange_code"))?
        .to_owned();
    let master_language = metadata_node
        .attr("formrange_afmasterlanguage")
        .ok_or(UbsMetadataError::MissingAttribute(
            "formrange_afmasterlanguage",
        ))?
        .to_owned();

    let entities_node = metadata_node
        .child("entities")
        .ok_or(UbsMetadataError::NoEntities)?;
    let entities: Vec<UbsEntity> = entities_node
        .children
        .iter()
        .filter_map(|item| {
            let entity = item.attr("formrange_entity")?.to_owned();
            let languages = item
                .attr("formrange_language")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|lang| !lang.is_empty())
                .map(str::to_owned)
                .collect();
            let cdoks = item
                .child("cdoks")
                .map(|cdoks| {
                    cdoks
                        .children
                        .iter()
                        .filter_map(|cdok| cdok.attr("formrange_cdokinfo").map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            Some(UbsEntity {
                entity,
                languages,
                cdoks,
            })
        })
        .collect();
    if entities.is_empty() {
        return Err(UbsMetadataError::NoEntities);
    }

    let dam_redacto_summary = package
        .dam_content_xml
        .as_deref()
        .map(String::from_utf8_lossy)
        .is_some_and(|dam_xml| dam_xml.contains("redactoSummary=\"true\""));

    Ok(UbsFormMetadata {
        formcode,
        master_language,
        entities,
        dam_redacto_summary,
    })
}

impl UbsFormMetadata {
    /// Picks which entity's mandator/language to open the form with:
    /// `mandator_override` (from `U2S_AEM_VERIFY_UBS_MANDATOR`) when set --
    /// and it must name a real entity, a hard error otherwise, since a
    /// silently-ignored override would open the form as whichever entity
    /// happens to be first, not the one an operator explicitly asked for
    /// -- else the first declared entity. `language` is that entity's own
    /// first declared language, falling back to the form's master
    /// language if the entity declares none (lowercased: `esignature.js`
    /// and `ElectronicSignatureRdsServlet` both build a `Locale` from the
    /// first two characters, and every real example is already
    /// lowercase-safe -- `"IT"`, `"EN"`, `"DE"`).
    pub fn select(&self, mandator_override: Option<&str>) -> Result<OpenParams, UbsMetadataError> {
        let entity = match mandator_override {
            Some(requested) => self
                .entities
                .iter()
                .find(|e| e.entity == requested)
                .ok_or_else(|| UbsMetadataError::UnknownMandator {
                    requested: requested.to_owned(),
                    known: self.entities.iter().map(|e| e.entity.clone()).collect(),
                })?,
            None => &self.entities[0],
        };
        let language = entity
            .languages
            .first()
            .cloned()
            .unwrap_or_else(|| self.master_language.clone())
            .to_lowercase();
        Ok(OpenParams {
            mandator: entity.entity.clone(),
            language,
            formcode: self.formcode.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use u2s_aem_verify_core::package_check::PackageSummary;

    fn package_with(form_xml: &str, dam_xml: Option<&str>) -> PackageInspection {
        PackageInspection {
            summary: PackageSummary {
                form_jcr_path: "/content/forms/af/afforms_italy_all/af_aa/AF_AAOV".to_owned(),
                form_name: "AF_AAOV".to_owned(),
                looks_like_it_has_dor: false,
                looks_like_a_wizard: true,
            },
            form_content_xml: form_xml.as_bytes().to_vec(),
            dam_content_xml: dam_xml.map(|xml| xml.as_bytes().to_vec()),
        }
    }

    const AAOV_LIKE_XML: &str = r#"<jcr:root>
        <metadata
            sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/metadata"
            formrange_code="AAOV"
            formrange_afmasterlanguage="IT">
            <entities>
                <item0 formrange_entity="033" formrange_language="IT">
                    <cdoks>
                        <item0 formrange_cdokinfo="66830"/>
                    </cdoks>
                </item0>
            </entities>
        </metadata>
    </jcr:root>"#;

    #[test]
    fn extracts_the_aaov_like_metadata_component() {
        let package = package_with(AAOV_LIKE_XML, None);
        let metadata = extract(&package).expect("must parse");
        assert_eq!(metadata.formcode, "AAOV");
        assert_eq!(metadata.master_language, "IT");
        assert_eq!(metadata.entities.len(), 1);
        assert_eq!(metadata.entities[0].entity, "033");
        assert_eq!(metadata.entities[0].languages, vec!["IT".to_owned()]);
        assert_eq!(metadata.entities[0].cdoks, vec!["66830".to_owned()]);
        assert!(!metadata.dam_redacto_summary);
    }

    #[test]
    fn detects_the_dam_redacto_summary_flag() {
        let package = package_with(
            AAOV_LIKE_XML,
            Some(r#"<jcr:root><metadata redactoSummary="true"/></jcr:root>"#),
        );
        let metadata = extract(&package).expect("must parse");
        assert!(metadata.dam_redacto_summary);
    }

    #[test]
    fn a_missing_metadata_component_is_a_clear_error() {
        let package = package_with("<jcr:root/>", None);
        assert!(matches!(
            extract(&package),
            Err(UbsMetadataError::NoMetadataComponent)
        ));
    }

    #[test]
    fn select_with_no_override_uses_the_first_entity() {
        let metadata = extract(&package_with(AAOV_LIKE_XML, None)).expect("must parse");
        let params = metadata.select(None).expect("must select");
        assert_eq!(
            params,
            OpenParams {
                mandator: "033".to_owned(),
                language: "it".to_owned(),
                formcode: "AAOV".to_owned(),
            }
        );
    }

    #[test]
    fn select_with_an_unknown_override_is_a_hard_error() {
        let metadata = extract(&package_with(AAOV_LIKE_XML, None)).expect("must parse");
        let err = metadata
            .select(Some("999"))
            .expect_err("999 is not a declared entity");
        assert!(matches!(
            err,
            UbsMetadataError::UnknownMandator { requested, known }
                if requested == "999" && known == vec!["033".to_owned()]
        ));
    }

    #[test]
    fn open_params_query_carries_wcmmode_disabled_too() {
        let params = OpenParams {
            mandator: "033".to_owned(),
            language: "it".to_owned(),
            formcode: "AAOV".to_owned(),
        };
        assert_eq!(
            params.query(),
            vec![
                ("wcmmode".to_owned(), "disabled".to_owned()),
                ("afAcceptLang".to_owned(), "it".to_owned()),
                ("mandator".to_owned(), "033".to_owned()),
                ("formcode".to_owned(), "AAOV".to_owned()),
            ]
        );
    }
}
