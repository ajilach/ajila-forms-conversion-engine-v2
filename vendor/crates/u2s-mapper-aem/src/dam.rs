//! The DAM asset `.content.xml` (AEM.md §9): the Forms Manager UI's
//! metadata entry. Its own typed attributes (`allowedRenderFormat`,
//! `dorType`, `formmodel`, `hasCustomThumbnail`, `title`) are mechanically
//! derived from the same `ValidForm` the form page itself comes from --
//! never invented. Everything else the real DAM asset's own `<metadata>`
//! node and its `jcr:content` siblings carry (`author`,
//! `availableStylings`, `dorTemplateRef`, `themeRef`, a `<dictionary>`
//! child, ...) is real per-deployment DAM authoring content this crate
//! cannot derive, so it round-trips through
//! [`u2s_aem::model::FormMetadata::dam_chrome`] the same way
//! `guideContainer`'s own remainder does through
//! [`u2s_aem::model::FormMetadata::chrome`].

use std::io::Cursor;

use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::writer::Writer;

use u2s_aem::model::{DataModel, Language, ValidForm};

use crate::xml_writer::{XmlResult, push_passthrough_attributes, write_raw_children};

pub fn write_dam_xml(form: &ValidForm, master: &Language) -> XmlResult<String> {
    let inner = form.form();
    let title = inner
        .metadata
        .title
        .as_ref()
        .and_then(|t| t.get(master))
        .map(|t| t.as_str().to_owned())
        .unwrap_or_else(|| inner.metadata.form_name.as_str().to_owned());

    // AEM.md §9's own attribute reference: `"none"`, `"xsd"`, `"xdp"`.
    // `u2s-aem` models only the first two.
    let formmodel = match inner.metadata.data_model {
        DataModel::Unbound => "none",
        DataModel::XmlSchema { .. } => "xsd",
    };
    let dor_type = match inner.metadata.dor {
        u2s_aem::model::DorMode::Generate => "generate",
        u2s_aem::model::DorMode::None => "none",
    };

    let mut w = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);
    w.write_event(Event::Text(quick_xml::events::BytesText::from_escaped(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    )))?;

    let mut root = BytesStart::new("jcr:root");
    root.push_attribute(crate::jcr::xml_attribute("xmlns:sling", crate::jcr::ns::SLING));
    root.push_attribute(crate::jcr::xml_attribute("xmlns:fd", crate::jcr::ns::FD));
    root.push_attribute(crate::jcr::xml_attribute("xmlns:dam", crate::jcr::ns::DAM));
    root.push_attribute(crate::jcr::xml_attribute("xmlns:jcr", crate::jcr::ns::JCR));
    // Standard JCR namespace, declared unconditionally rather than only
    // when `dam_chrome` happens to reference it (`jcr:mixinTypes="[mix:
    // created,mix:lastModified]"`, in the real fixture): the same
    // unconditional-declaration convention `i18n::write_dictionary_xml`
    // already uses for this exact namespace.
    root.push_attribute(crate::jcr::xml_attribute("xmlns:mix", crate::jcr::ns::MIX));
    root.push_attribute(crate::jcr::xml_attribute("xmlns:nt", crate::jcr::ns::NT));
    root.push_attribute(crate::jcr::xml_attribute("jcr:primaryType", "dam:Asset"));
    w.write_event(Event::Start(root))?;

    let mut jcr_content = BytesStart::new("jcr:content");
    jcr_content.push_attribute(crate::jcr::xml_attribute("jcr:primaryType", "dam:AssetContent"));
    jcr_content.push_attribute(crate::jcr::xml_attribute("sling:resourceType", "fd/fm/af/render"));
    jcr_content.push_attribute(crate::jcr::xml_attribute("guide", "1"));
    jcr_content.push_attribute(crate::jcr::xml_attribute("type", "guide"));
    w.write_event(Event::Start(jcr_content))?;

    // `dam_chrome`'s own raw children (the real fixture's own
    // `<dictionary>`, say) precede `<metadata>` -- confirmed against the
    // real fixture's own child order, which `canonical.rs` treats as
    // semantic.
    write_raw_children(&mut w, &inner.metadata.dam_chrome.raw_children)?;

    let mut metadata = BytesStart::new("metadata");
    metadata.push_attribute(crate::jcr::xml_attribute("fd:version", "1.1"));
    metadata.push_attribute(crate::jcr::xml_attribute("jcr:primaryType", "nt:unstructured"));
    metadata.push_attribute(crate::jcr::xml_attribute("allowedRenderFormat", "HTML"));
    metadata.push_attribute(crate::jcr::xml_attribute("dorType", dor_type));
    metadata.push_attribute(crate::jcr::xml_attribute("formmodel", formmodel));
    metadata.push_attribute(crate::jcr::xml_attribute("hasCustomThumbnail", "{Boolean}false"));
    metadata.push_attribute(crate::jcr::xml_attribute("title", title.as_str()));
    push_passthrough_attributes(&mut metadata, &inner.metadata.dam_chrome);
    w.write_event(Event::Empty(metadata))?;

    w.write_event(Event::End(BytesEnd::new("jcr:content")))?;
    w.write_event(Event::End(BytesEnd::new("jcr:root")))?;

    let mut xml = String::from_utf8(w.into_inner().into_inner()).expect("quick-xml writes valid UTF-8");
    xml.push('\n');
    Ok(xml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn the_dam_asset_carries_the_forms_title_and_data_model() {
        let form = build_form(xml_schema("Root"), vec![a_page_with(vec![a_text_field("Name")])]);
        let xml = write_dam_xml(&form, &lang("en")).expect("writes");
        assert!(xml.contains(r#"jcr:primaryType="dam:Asset""#));
        assert!(xml.contains(r#"formmodel="xsd""#));
        assert!(xml.contains(r#"title="TestForm""#));
    }

    #[test]
    fn an_unbound_form_reports_no_data_model() {
        let form = build_form(
            u2s_aem::model::DataModel::Unbound,
            vec![a_page_with(vec![a_text_field("Name")])],
        );
        let xml = write_dam_xml(&form, &lang("en")).expect("writes");
        assert!(xml.contains(r#"formmodel="none""#));
    }

    #[test]
    fn dam_chrome_attributes_and_children_round_trip_into_the_written_xml() {
        let mut form = build_form(
            u2s_aem::model::DataModel::Unbound,
            vec![a_page_with(vec![a_text_field("Name")])],
        );
        let mut inner = form.into_form();
        inner.metadata.dam_chrome = u2s_aem::model::Passthrough {
            raw_attributes: [("themeRef".to_owned(), "/content/dam/example/theme".to_owned())]
                .into_iter()
                .collect(),
            raw_children: vec![u2s_aem::model::RawJcrNode {
                tag_name: "dictionary".to_owned(),
                attributes: [("jcr:primaryType".to_owned(), "nt:unstructured".to_owned())]
                    .into_iter()
                    .collect(),
                children: Vec::new(),
            }],
        };
        form = inner.validate().expect("still valid");
        let xml = write_dam_xml(&form, &lang("en")).expect("writes");
        assert!(xml.contains(r#"themeRef="/content/dam/example/theme""#));
        assert!(xml.contains("<dictionary"));
    }
}
