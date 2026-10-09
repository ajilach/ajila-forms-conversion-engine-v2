//! What the UBS profile derives from a form's own XFA variables: where the
//! form lives in AEM, what it is called, which DoR templates it uses and which
//! language its metadata names as its master.
//!
//! These were Tera expressions in the profile config; they are rules of the
//! UBS deployment, so they are Rust.

use std::collections::HashMap;

/// A form's identity, from its XFA variables.
pub struct FormIdentity {
    /// `formrange_code`, e.g. `AABF`.
    pub code: String,
    /// `formrange_entity`: `019` Germany, `033` Italy, `001` Switzerland, or
    /// another; `None` when the form names none.
    pub entity: Option<String>,
}

impl FormIdentity {
    /// The identity in `xfa`; an error when the form names no code, since
    /// every path and name of the package is built from it.
    pub fn from_xfa(xfa: &HashMap<String, String>) -> Result<Self, String> {
        let code = xfa
            .get("formrange_code")
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty())
            .ok_or("the form names no `formrange_code`, which every package path is built from")?;
        let entity = xfa
            .get("formrange_entity")
            .map(|e| e.trim().to_owned())
            .filter(|e| !e.is_empty());
        Ok(FormIdentity { code, entity })
    }

    fn entity(&self) -> &str {
        self.entity.as_deref().unwrap_or_default()
    }

    /// The region's form folder.
    pub fn entity_dir(&self) -> &'static str {
        match self.entity() {
            "019" => "afforms_germany_all",
            "033" => "afforms_italy_all",
            "001" => "afforms_ch_all",
            _ => "afforms_global_all",
        }
    }

    /// The folder of forms whose code starts the same: `af_` and the code's
    /// first two letters, lower-cased.
    pub fn prefix_dir(&self) -> String {
        let stem: String = self.code.chars().take(2).collect();
        format!("af_{}", stem.to_lowercase())
    }

    /// `entity_dir/prefix_dir`, the path under `content/forms/af/`.
    pub fn form_path(&self) -> String {
        format!("{}/{}", self.entity_dir(), self.prefix_dir())
    }

    /// The form's own folder, `AF_<code>`.
    pub fn form_dir(&self) -> String {
        format!("AF_{}", self.code)
    }

    /// The region's DoR template the print settings name.
    pub fn meta_template_ref(&self) -> String {
        let template = match self.entity() {
            "019" => "UBS_General_Germany_DOR.xdp",
            "033" => "UBS_General_Italy_DOR.xdp",
            _ => "UBS_Blank_DoR.xdp",
        };
        format!("/content/dam/formsanddocuments/reference-dor-templates/ajila-forms-ubs/02_forms/{template}")
    }

    /// The form's own DoR template rendition.
    pub fn dor_template_ref(&self) -> String {
        format!(
            "/content/dam/formsanddocuments/{}/{}/jcr:content/renditions/dorTemplate",
            self.form_path(),
            self.form_dir()
        )
    }

    /// The language the metadata control reports as the form's master: the
    /// issuing region's own.
    pub fn metadata_master_language(&self) -> &'static str {
        match self.entity() {
            "019" => "DE",
            "033" => "IT",
            _ => "EN",
        }
    }

    /// The profile variables this identity decides.
    pub fn variables(&self) -> HashMap<String, String> {
        [
            ("entity_dir", self.entity_dir().to_owned()),
            ("prefix_dir", self.prefix_dir()),
            ("form_code", self.code.clone()),
            ("meta_template_ref", self.meta_template_ref()),
            ("dor_template_ref", self.dor_template_ref()),
            ("metadata_master_language", self.metadata_master_language().to_owned()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(code: &str, entity: &str) -> FormIdentity {
        let xfa = HashMap::from([
            ("formrange_code".to_owned(), code.to_owned()),
            ("formrange_entity".to_owned(), entity.to_owned()),
        ]);
        FormIdentity::from_xfa(&xfa).unwrap()
    }

    #[test]
    fn an_italian_form_lives_in_the_italian_folders() {
        let id = identity("AAOS", "033");
        assert_eq!(id.form_path(), "afforms_italy_all/af_aa");
        assert_eq!(id.form_dir(), "AF_AAOS");
        assert_eq!(
            id.dor_template_ref(),
            "/content/dam/formsanddocuments/afforms_italy_all/af_aa/AF_AAOS/jcr:content/renditions/dorTemplate"
        );
        assert!(id.meta_template_ref().ends_with("UBS_General_Italy_DOR.xdp"));
        assert_eq!(id.metadata_master_language(), "IT");
    }

    #[test]
    fn a_form_of_another_region_falls_back_to_the_global_folders() {
        let id = identity("BXYZ", "");
        assert_eq!(id.form_path(), "afforms_global_all/af_bx");
        assert!(id.meta_template_ref().ends_with("UBS_Blank_DoR.xdp"));
        assert_eq!(id.metadata_master_language(), "EN");
    }

    #[test]
    fn a_form_without_a_code_is_refused() {
        assert!(FormIdentity::from_xfa(&HashMap::new()).is_err());
    }
}
