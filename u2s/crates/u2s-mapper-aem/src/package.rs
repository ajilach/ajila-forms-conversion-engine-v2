//! FileVault content package assembly (AEM.md §2, §11, §12): the ZIP/
//! META-INF boilerplate plus the JCR paths this form and its DAM asset
//! live at. Every byte here is either a fixed structural constant AEM.md
//! documents for every package, or derived mechanically from
//! `metadata.form_name` -- nothing is invented per form beyond that one
//! name.
//!
//! **Reproducibility.** `zip` is pulled in with `default-features = false`
//! (this crate's `Cargo.toml`), which excludes the `time` feature and with
//! it the convenience of stamping each entry with the current wall clock.
//! Without it, every entry gets the crate's own fixed default timestamp,
//! so the same `ValidForm` always produces byte-identical package bytes --
//! the property any golden-file comparison needs to not flap on every run.
//!
//! **Simplification from the reference.** The reference stores a form's
//! XSD as its own `dam:Asset` (a full binary-rendition wrapper) requiring
//! its own filter root. This module instead writes the XSD as a plain
//! file *inside* the form's own JCR subtree
//! (`.../<form_name>/schema.xsd`) -- a legitimate FileVault file aggregate
//! (AEM.md §12.4 lists `"file"` as its own aggregate type) that needs no
//! extra filter root, because it already sits under one the form's own
//! entry declares.

use std::io::Write as _;

use u2s_aem::model::ValidForm;

use crate::i18n::Dictionary;

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("could not write the package archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("could not write to the archive buffer: {0}")]
    Io(#[from] std::io::Error),
}

pub struct PackageInput<'a> {
    pub form: &'a ValidForm,
    pub form_xml: &'a str,
    pub dam_xml: &'a str,
    /// `None` for [`u2s_aem::model::DataModel::Unbound`].
    pub xsd_xml: Option<&'a str>,
    pub dictionaries: &'a std::collections::BTreeMap<u2s_aem::model::Language, Dictionary>,
}

/// Everything this module needs to know about one form's JCR paths, computed
/// once so no path is assembled two different ways in two different
/// functions. `folder_path` (`FormMetadata.folder_path`) is empty for
/// every green-field form this system's own Conversion Agent produces,
/// reproducing the flat path this crate always wrote before that field
/// existed; a real package's own nested path
/// (`afforms_germany_all/af_aa/AF_AABF`) round-trips through it instead.
struct Paths {
    form_name: String,
    /// The folder segments between `af/`/`formsanddocuments/` and this
    /// form's own name -- shared by both the forms and DAM trees, since a
    /// real package's own `filter.xml` always names the same relative
    /// suffix under both roots (checked by `crate::decode::zip`'s own
    /// root discovery).
    folder_segments: Vec<String>,
    form_root: String,
    dam_root: String,
    dictionary_dir: String,
}

impl Paths {
    fn new(form: &ValidForm) -> Self {
        let form_name = form.form().metadata.form_name.as_str().to_owned();
        let folder_segments: Vec<String> = form
            .form()
            .metadata
            .folder_path
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect();
        let relative = folder_segments
            .iter()
            .map(|s| format!("{s}/"))
            .collect::<String>()
            + &form_name;
        Paths {
            form_root: format!("/content/forms/af/{relative}"),
            dam_root: format!("/content/dam/formsanddocuments/{relative}"),
            // A ZIP entry path, not a JCR path: relative (no leading slash),
            // under `jcr_root/`, and the JCR name `jcr:content` renders as
            // the filesystem-safe `_jcr_content` FileVault always uses on
            // disk -- confirmed against the real fixture package
            // (`tests/fixtures/AF_AABF.zip`), whose own dictionaries live at
            // `jcr_root/content/forms/af/.../_jcr_content/guideContainer/assets/dictionary/`.
            // The previous value (`/content/forms/af/{form}/jcr:content/...`,
            // an absolute JCR path with the unencoded name) produced a ZIP
            // entry nothing in FileVault would ever look for.
            dictionary_dir: format!(
                "jcr_root/content/forms/af/{relative}/_jcr_content/guideContainer/assets/dictionary"
            ),
            folder_segments,
            form_name,
        }
    }
}

pub fn assemble(input: PackageInput) -> Result<Vec<u8>, PackageError> {
    let paths = Paths::new(input.form);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));

    // -------------------------------------------------------------- META-INF
    zip.start_file("META-INF/MANIFEST.MF", options)?;
    write!(
        zip,
        "Manifest-Version: 1.0\nContent-Package-Id: fd/export:{}\nContent-Package-Roots: {},{}\nContent-Package-Type: mixed\n",
        paths.form_name, paths.form_root, paths.dam_root
    )?;

    zip.start_file("META-INF/vault/filter.xml", options)?;
    write!(zip, "{}", filter_xml(&paths))?;

    zip.start_file("META-INF/vault/properties.xml", options)?;
    write!(zip, "{}", properties_xml(&paths))?;

    zip.start_file("META-INF/vault/config.xml", options)?;
    write!(zip, "{}", CONFIG_XML)?;

    zip.start_file("META-INF/vault/nodetypes.cnd", options)?;
    write!(zip, "{}", NODETYPES_CND)?;

    zip.start_file("META-INF/vault/definition/.content.xml", options)?;
    write!(zip, "{}", definition_xml(&paths))?;

    // ---------------------------------------------------------------- jcr_root
    zip.start_file("jcr_root/.content.xml", options)?;
    write!(zip, "{REP_ROOT_XML}")?;

    zip.start_file("jcr_root/content/.content.xml", options)?;
    write!(zip, "{CONTENT_FOLDER_XML}")?;

    zip.start_file("jcr_root/content/forms/.content.xml", options)?;
    write!(zip, "{ORDERED_FOLDER_XML}")?;

    zip.start_file("jcr_root/content/forms/af/.content.xml", options)?;
    write!(zip, "{HIDDEN_FOLDER_XML}")?;

    // One `.content.xml` per intermediate folder segment (empty for every
    // green-field form, which writes nothing extra here) -- see `Paths`'s
    // own doc.
    let mut forms_prefix = "jcr_root/content/forms/af".to_owned();
    let mut dam_prefix = "jcr_root/content/dam/formsanddocuments".to_owned();
    for segment in &paths.folder_segments {
        forms_prefix.push('/');
        forms_prefix.push_str(segment);
        zip.start_file(format!("{forms_prefix}/.content.xml"), options)?;
        write!(zip, "{ORDERED_FOLDER_XML}")?;

        dam_prefix.push('/');
        dam_prefix.push_str(segment);
        zip.start_file(format!("{dam_prefix}/.content.xml"), options)?;
        write!(zip, "{ORDERED_FOLDER_XML}")?;
    }

    zip.start_file(
        format!("{forms_prefix}/{}/.content.xml", paths.form_name),
        options,
    )?;
    write!(zip, "{}", input.form_xml)?;

    if let Some(xsd_xml) = input.xsd_xml {
        zip.start_file(
            format!("{forms_prefix}/{}/schema.xsd", paths.form_name),
            options,
        )?;
        write!(zip, "{xsd_xml}")?;
    }

    for (language, dictionary) in input.dictionaries {
        zip.start_file(format!("{}/{language}.xml", paths.dictionary_dir), options)?;
        write!(zip, "{}", crate::i18n::write_dictionary_xml(language, dictionary))?;
    }

    zip.start_file("jcr_root/content/dam/.content.xml", options)?;
    write!(zip, "{HIDDEN_FOLDER_XML}")?;

    zip.start_file(
        "jcr_root/content/dam/formsanddocuments/.content.xml",
        options,
    )?;
    write!(zip, "{HIDDEN_FOLDER_XML}")?;

    zip.start_file(
        format!("{dam_prefix}/{}/.content.xml", paths.form_name),
        options,
    )?;
    write!(zip, "{}", input.dam_xml)?;

    Ok(zip.finish()?.into_inner())
}

fn filter_xml(paths: &Paths) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<workspaceFilter version="1.0">
    <filter root="{}"/>
    <filter root="{}"/>
</workspaceFilter>
"#,
        paths.form_root, paths.dam_root
    )
}

fn definition_xml(paths: &Paths) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:vlt="{vlt}"
          xmlns:jcr="{jcr}"
          xmlns:nt="{nt}"
          jcr:primaryType="vlt:PackageDefinition"
          buildCount="1"
          group="fd/export"
          name="{name}"
          version="">
    <filter jcr:primaryType="nt:unstructured">
        <f0 jcr:primaryType="nt:unstructured" mode="replace" root="{form_root}" rules="[]"/>
        <f1 jcr:primaryType="nt:unstructured" mode="replace" root="{dam_root}" rules="[]"/>
    </filter>
</jcr:root>
"#,
        vlt = crate::jcr::ns::VLT,
        jcr = crate::jcr::ns::JCR,
        nt = crate::jcr::ns::NT,
        name = paths.form_name,
        form_root = paths.form_root,
        dam_root = paths.dam_root,
    )
}

fn properties_xml(paths: &Paths) -> String {
    // A fixed timestamp, not `now()` -- see the module doc on
    // reproducibility. `1970-01-01T00:00:00.000+00:00` is the epoch, not a
    // wall-clock reading anyone should mistake for a real build time.
    const FIXED_TIMESTAMP: &str = "1970-01-01T00:00:00.000+00:00";
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE properties SYSTEM "http://java.sun.com/dtd/properties.dtd">
<properties>
    <comment>FileVault Package Properties</comment>
    <entry key="packageType">mixed</entry>
    <entry key="group">fd/export</entry>
    <entry key="name">{name}</entry>
    <entry key="version"></entry>
    <entry key="created">{ts}</entry>
    <entry key="createdBy">u2s-mapper-aem</entry>
    <entry key="packageFormatVersion">2</entry>
</properties>
"#,
        name = paths.form_name,
        ts = FIXED_TIMESTAMP,
    )
}

const REP_ROOT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0"
          xmlns:jcr="http://www.jcp.org/jcr/1.0"
          xmlns:rep="internal"
          jcr:mixinTypes="[rep:AccessControllable]"
          jcr:primaryType="rep:root"/>
"#;

const CONTENT_FOLDER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:jcr="http://www.jcp.org/jcr/1.0"
          jcr:primaryType="sling:OrderedFolder"/>
"#;

const ORDERED_FOLDER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:jcr="http://www.jcp.org/jcr/1.0"
          jcr:primaryType="sling:OrderedFolder"/>
"#;

const HIDDEN_FOLDER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:jcr="http://www.jcp.org/jcr/1.0"
          jcr:primaryType="sling:Folder"
          hidden="true"/>
"#;

/// FileVault aggregation config (AEM.md §12.4). Fixed across every
/// package this crate emits -- it describes how the *format* serializes,
/// not anything about a specific form.
const CONFIG_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<vaultfs version="1.1">
    <aggregates>
        <aggregate type="file" title="File Aggregate"/>
        <aggregate type="filefolder" title="File/Folder Aggregate"/>
        <aggregate type="nodetype" title="Node Type Aggregate"/>
        <aggregate type="full" title="Full Coverage Aggregate"/>
    </aggregates>
    <handlers>
        <handler type="folder"/>
        <handler type="file"/>
        <handler type="nodetype"/>
        <handler type="generic"/>
    </handlers>
</vaultfs>
"#;

/// A CND transcription of AEM.md §4's own "Primary Node Types" and "Mixin
/// Types" tables -- not invented, since those tables are the one place
/// this crate has real, spec-grounded content for a node-type file.
const NODETYPES_CND: &str = r#"<'jcr'='http://www.jcp.org/jcr/1.0'>
<'nt'='http://www.jcp.org/jcr/nt/1.0'>
<'sling'='http://sling.apache.org/jcr/sling/1.0'>
<'cq'='http://www.day.com/jcr/cq/1.0'>
<'dam'='http://www.day.com/dam/1.0'>
<'mix'='http://www.jcp.org/jcr/mix/1.0'>
<'vlt'='http://www.day.com/jcr/vault/1.0'>

[cq:Page] > nt:hierarchyNode
[cq:PageContent] > nt:unstructured
  mixin
[nt:unstructured]
[sling:Folder] > nt:folder
[sling:OrderedFolder] > sling:Folder
[dam:Asset] > nt:hierarchyNode
[dam:AssetContent] > nt:unstructured
[vlt:PackageDefinition]
[rep:root]

[mix:language]
  mixin
  - jcr:language (string)
[mix:created]
  mixin
[mix:title]
  mixin
  - jcr:title (string)
  - jcr:description (string)
[sling:Resource]
  mixin
  - sling:resourceType (string)
[sling:Message]
  mixin
  - sling:key (string)
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    

    fn zip_entries(bytes: &[u8]) -> Vec<String> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("valid zip");
        (0..archive.len())
            .map(|i| archive.by_index(i).expect("entry").name().to_owned())
            .collect()
    }

    #[test]
    fn the_package_contains_every_required_meta_inf_entry() {
        let form = build_form(xml_schema("Root"), vec![a_page_with(vec![a_text_field("Name")])]);
        let xsd = crate::xsd::generate(&form);
        let master = lang("en");
        let ctx = crate::xml_writer::WriteCtx {
            master: &master,
            bind_refs: &xsd.bind_refs,
        };
        let form_xml = crate::xml_writer::write_form_xml(&form, &ctx).expect("writes");
        let dam_xml = crate::dam::write_dam_xml(&form, &master).expect("writes");
        let dictionaries = crate::i18n::collect_dictionaries(&form);

        let bytes = assemble(PackageInput {
            form: &form,
            form_xml: &form_xml,
            dam_xml: &dam_xml,
            xsd_xml: xsd.schema_xml.as_deref(),
            dictionaries: &dictionaries,
        })
        .expect("assembles");

        let entries = zip_entries(&bytes);
        for expected in [
            "META-INF/MANIFEST.MF",
            "META-INF/vault/filter.xml",
            "META-INF/vault/properties.xml",
            "META-INF/vault/config.xml",
            "META-INF/vault/nodetypes.cnd",
            "META-INF/vault/definition/.content.xml",
            "jcr_root/.content.xml",
            "jcr_root/content/forms/af/TestForm/.content.xml",
            "jcr_root/content/forms/af/TestForm/schema.xsd",
            "jcr_root/content/dam/formsanddocuments/TestForm/.content.xml",
        ] {
            assert!(entries.contains(&expected.to_owned()), "missing {expected}: {entries:?}");
        }
    }

    #[test]
    fn the_filter_covers_both_the_form_and_the_dam_asset() {
        let form = build_form(xml_schema("Root"), vec![a_page_with(vec![a_text_field("Name")])]);
        let paths = Paths::new(&form);
        let xml = filter_xml(&paths);
        assert!(xml.contains("/content/forms/af/TestForm"));
        assert!(xml.contains("/content/dam/formsanddocuments/TestForm"));
    }

    #[test]
    fn assembling_the_same_form_twice_produces_identical_bytes() {
        let form = build_form(xml_schema("Root"), vec![a_page_with(vec![a_text_field("Name")])]);
        let xsd = crate::xsd::generate(&form);
        let master = lang("en");
        let ctx = crate::xml_writer::WriteCtx {
            master: &master,
            bind_refs: &xsd.bind_refs,
        };
        let form_xml = crate::xml_writer::write_form_xml(&form, &ctx).expect("writes");
        let dam_xml = crate::dam::write_dam_xml(&form, &master).expect("writes");
        let dictionaries = crate::i18n::collect_dictionaries(&form);

        let build = || {
            assemble(PackageInput {
                form: &form,
                form_xml: &form_xml,
                dam_xml: &dam_xml,
                xsd_xml: xsd.schema_xml.as_deref(),
                dictionaries: &dictionaries,
            })
            .expect("assembles")
        };
        assert_eq!(build(), build(), "reproducible: no wall-clock timestamps");
    }

    #[test]
    fn dictionaries_are_written_one_file_per_language() {
        let form = build_form(
            u2s_aem::model::DataModel::Unbound,
            vec![a_page_with(vec![a_text_field("Name")])],
        );
        let master = lang("en");
        let ctx = crate::xml_writer::WriteCtx {
            master: &master,
            bind_refs: &std::collections::HashMap::new(),
        };
        let form_xml = crate::xml_writer::write_form_xml(&form, &ctx).expect("writes");
        let dam_xml = crate::dam::write_dam_xml(&form, &master).expect("writes");
        let dictionaries = crate::i18n::collect_dictionaries(&form);

        let bytes = assemble(PackageInput {
            form: &form,
            form_xml: &form_xml,
            dam_xml: &dam_xml,
            xsd_xml: None,
            dictionaries: &dictionaries,
        })
        .expect("assembles");
        let entries = zip_entries(&bytes);
        // The full path, not just the suffix: a relative `jcr_root/`-rooted
        // ZIP entry with `_jcr_content` (the filesystem-safe spelling
        // FileVault always uses), not the absolute JCR path with the
        // unencoded `jcr:content` name the entry used to be built from,
        // which produced a path nothing in FileVault would ever look for.
        assert!(
            entries.contains(
                &"jcr_root/content/forms/af/TestForm/_jcr_content/guideContainer/assets/dictionary/en.xml"
                    .to_owned()
            ),
            "expected the dictionary entry at its real FileVault path: {entries:?}"
        );
    }

}
