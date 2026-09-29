//! `AemConfig`/`AemNode` writer tests ported from `blueprint` (the deleted
//! `core` crate)'s `tests/mod.rs` (`ajilach/ajila-forms-conversion-engine` at
//! commit `f5f596a`, the last commit before `core/` was deleted).
//!
//! Every test here builds its `AemNode` tree by hand or from a small inline
//! XML fixture and renders it through the real UBS profile templates -- none
//! of them go through the mechanical PDF/XFA -> `StructuredNode` pipeline,
//! which no longer exists in this crate.

#[path = "support/mod.rs"]
mod support;

use std::collections::HashMap;

use u2s_aem_ubs_mcp::aem::{
    AemAttrs, AemConfig, AemNode, AemOption, ConditionRule, OptionAlignment, TextFieldKind,
    generate_aem_xml, header_slot_text, parse_aem_zip,
};
use u2s_aem_ubs_mcp::context::Context;
use u2s_aem_ubs_mcp::value::InputValue;
use uuid::Uuid;

/// A hand-built `AemNode` tree, covering a `Panel` holding a `TextField` and a
/// `RadioButton`, serialises and deserialises to the identical JSON -- the
/// shape the edit-history store and the agent's tool calls both rely on.
#[test]
fn aem_node_json_round_trips() {
    let root = AemNode::Root {
        title: "Test Form".into(),
        children: vec![AemNode::Panel {
            uuid: Uuid::nil(),
            name: "p1".into(),
            title: "Panel 1".into(),
            children: vec![
                AemNode::TextField {
                    attrs: AemAttrs::default(),
                    uuid: Uuid::nil(),
                    name: "tf".into(),
                    label: "Name".into(),
                    mandatory: true,
                    visible: true,
                    max_chars: Some(50),
                    colspan: 6,
                    dor_colspan: None,
                    bind_ref: None,
                    kind: TextFieldKind::Plain,
                },
                AemNode::RadioButton {
                    attrs: AemAttrs::default(),
                    uuid: Uuid::nil(),
                    name: "rb".into(),
                    label: "Type".into(),
                    options: vec![AemOption {
                        label: "A".into(),
                        value: "a".into(),
                    }],
                    alignment: OptionAlignment::Vertical,
                    mandatory: false,
                    visible: true,
                    colspan: 12,
                    dor_colspan: None,
                    conditions: vec![],
                    bind_ref: None,
                },
            ],
            is_page: true,
            attrs: AemAttrs::default(),
            visible: true,
            is_conditional: false,
            dor_num_cols: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            frag_ref: None,
        }],
    };

    let json = serde_json::to_string(&root).expect("serialize AemNode");
    let back: AemNode = serde_json::from_str(&json).expect("deserialize AemNode");
    assert_eq!(
        json,
        serde_json::to_string(&back).expect("re-serialize AemNode"),
        "AemNode JSON should round-trip"
    );
}

/// `form_code`/`form_title`/`form_path`/`form_dir()`/`xsd_path` are all
/// derived from the profile's templates against the source's XFA variables.
#[test]
fn test_aaab_aem_config_form_path_title_code() {
    let mut variables = HashMap::new();
    variables.insert("formrange_code".to_string(), "AAAB".to_string());
    variables.insert("formrange_entity".to_string(), "019".to_string());
    let ctx = Context::new("de".to_string(), variables);

    let (profile, templates) = support::ubs_profile();
    let config = AemConfig::from_profile(&profile, templates, &ctx)
        .expect("Failed to create AemConfig");

    assert_eq!(config.form_code, "AAAB", "form_code should be 'AAAB'");
    assert_eq!(
        config.form_title, "AAAB",
        "form_title should equal form_code"
    );
    assert_eq!(
        config.form_path, "afforms_germany_all/af_aa",
        "form_path should be 'afforms_germany_all/af_aa'"
    );
    assert_eq!(
        config.form_dir(),
        "AF_AAAB",
        "form_dir() should be 'AF_AAAB'"
    );
    assert_eq!(
        config.xsd_path.as_deref(),
        Some("/content/dam/formsanddocuments/afforms_xsd/AFForms/AF_AAAB.xsd"),
        "xsd_path should be rendered from the profile with the form code"
    );
    assert!(
        !config.bind_to_xsd,
        "binding to the schema must stay opt-in"
    );
}

/// The metadata control masters the issuing region's own language -- German
/// for Germany, Italian for Italy, English elsewhere -- and never a language
/// the form does not ship.
///
/// It used to be `master_language` from the profile, a flat `en`. That field
/// is the authoring master the dictionaries are keyed in, which is a
/// different question: a German form's keys are English while the form
/// itself was issued in German. UBS reads this attribute as the latter
/// (feedback PROBLEM-metadata-languages, the master half).
#[test]
fn the_metadata_control_masters_the_issuing_regions_language() {
    let master_for = |entity: &str, languages: &[&str]| {
        let (profile, templates) = support::ubs_profile();
        let mut vars = HashMap::new();
        vars.insert("formrange_code".into(), "TEST".into());
        vars.insert("formrange_entity".into(), entity.to_string());
        let ctx = Context::new(languages[0].to_string(), vars);
        let mut config = AemConfig::from_profile(&profile, templates, &ctx)
            .expect("profile config");
        config.languages = languages.iter().map(|l| l.to_string()).collect();

        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![],
        };
        let xml = generate_aem_xml(&root, &config);
        // The Tera block that computes the value must not eat the whitespace
        // separating this attribute from the one before it.
        assert!(
            xml.contains(" formrange_afmasterlanguage=\""),
            "the master-language attribute must stay separated from its neighbour:\n{}",
            &xml[..xml.len().min(4000)]
        );
        xml.split("formrange_afmasterlanguage=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("the metadata control must carry a master language")
            .to_string()
    };

    // Germany, issued in three languages: German masters it.
    assert_eq!(master_for("019", &["de", "en", "es"]), "DE");
    // Italy: Italian.
    assert_eq!(master_for("033", &["de", "en", "it"]), "IT");
    // A region whose language the form does not ship falls back to English --
    // a master the form does not carry is never right.
    assert_eq!(master_for("019", &["en", "it"]), "EN");
    // One language is its own master, whatever the region.
    assert_eq!(master_for("019", &["en"]), "EN");
    assert_eq!(master_for("001", &["de"]), "DE");
    // Anywhere else: English.
    assert_eq!(master_for("001", &["de", "en", "fr"]), "EN");
}

/// The profile's own setting: Redacto renders the DoR from the summary, and
/// renders the preview its own way, so the carousel step and the button that
/// opens it are obsolete (PROBLEM-preview-step-removed). Without the summary
/// the preview step is the only preview there is, so it stays.
#[test]
fn a_form_that_renders_its_dor_through_redacto_has_no_preview_step() {
    let (profile, templates) = support::ubs_profile();
    let mut vars = HashMap::new();
    vars.insert("formrange_code".into(), "TEST".into());
    vars.insert("formrange_entity".into(), "033".into());
    let ctx = Context::new("it".to_string(), vars);
    let mut config = AemConfig::from_profile(&profile, templates, &ctx)
        .expect("profile config");

    let root = AemNode::Root {
        title: "TEST".into(),
        children: vec![],
    };

    assert_eq!(
        config.user_vars.get("use_summary").map(String::as_str),
        Some("true"),
        "the UBS profile is expected to run with the summary enabled"
    );
    let xml = generate_aem_xml(&root, &config);
    assert!(
        xml.contains("<summarypanel"),
        "the summary step must be there. Got:\n{}",
        xml
    );
    for gone in ["<previewpanel", "carouselPreview", "initializeForPreview"] {
        assert!(
            !xml.contains(gone),
            "{gone} belongs to the obsolete preview step. Got:\n{}",
            xml
        );
    }
    assert_eq!(
        xml.matches("name=\"submitErrorMessage\"").count(),
        1,
        "exactly one submitErrorMessage, in the summary panel. Got:\n{}",
        xml
    );
    assert_eq!(
        xml.matches("name=\"messagebox_ElsigCheck\"").count(),
        1,
        "exactly one messagebox_ElsigCheck. Got:\n{}",
        xml
    );

    config
        .user_vars
        .insert("use_summary".into(), "false".into());
    let xml = generate_aem_xml(&root, &config);
    assert!(
        xml.contains("<previewpanel") && xml.contains("initializeForPreview"),
        "a profile without the summary keeps the preview step. Got:\n{}",
        xml
    );
}

/// The metadata control has to name a language by the code the platform files
/// it under, not by the code language detection produced: a Spanish source is
/// detected as `es`; the profile keys Spanish `sp` and declares `es` its
/// synonym.
#[test]
fn the_metadata_control_names_languages_by_their_canonical_codes() {
    let (profile, templates) = support::ubs_profile();
    let mut vars = HashMap::new();
    vars.insert("formrange_code".into(), "TEST".into());
    vars.insert("formrange_entity".into(), "019".into());
    let ctx = Context::new("de".to_string(), vars);
    let mut config = AemConfig::from_profile(&profile, templates, &ctx)
        .expect("profile config");
    // As the merge hands them over: ISO codes, Spanish among them.
    config.languages = vec!["de".into(), "en".into(), "es".into()];

    let root = AemNode::Root {
        title: "TEST".into(),
        children: vec![],
    };
    let xml = generate_aem_xml(&root, &config);

    let value = xml
        .split("formrange_language=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the metadata control must name the form's languages");
    let listed: std::collections::BTreeSet<&str> = value.split(',').collect();
    assert_eq!(
        listed,
        ["DE", "EN", "SP"].into_iter().collect(),
        "expected the canonical codes, got {value}"
    );
}

#[test]
fn test_ubs_profile_entity_folder_mapping() {
    let (profile, _) = support::ubs_profile();

    let config_for = |code: &str, entity: &str, lang: &str| {
        let mut vars = HashMap::new();
        vars.insert("formrange_code".into(), code.to_string());
        vars.insert("formrange_entity".into(), entity.to_string());
        let ctx = Context::new(lang.to_string(), vars);
        AemConfig::from_profile(&profile, HashMap::new(), &ctx).unwrap()
    };

    assert_eq!(
        config_for("AAEI", "019", "de").form_path,
        "afforms_germany_all/af_aa"
    );
    assert_eq!(
        config_for("AAOE", "033", "it").form_path,
        "afforms_italy_all/af_aa"
    );
    assert_eq!(
        config_for("ACAV", "001", "de").form_path,
        "afforms_ch_all/af_ac"
    );
    assert_eq!(
        config_for("TEST", "999", "en").form_path,
        "afforms_global_all/af_te"
    );
}

#[test]
fn test_aem_profile_allows_bind_to_xsd_without_xsd_path() {
    use u2s_aem_ubs_mcp::aem::AemProfile;

    let toml_str = r#"
title = "{{ xfa.formrange_code }}"
form_dir = "AF_{{ xfa.formrange_code }}"
bind_to_xsd = true
"#;

    let profile: AemProfile = toml::from_str(toml_str).expect("parse aem profile");
    let mut vars = HashMap::new();
    vars.insert("formrange_code".to_string(), "AAAB".to_string());
    let ctx = Context::new("en".to_string(), vars);

    let config = AemConfig::from_profile(&profile, HashMap::new(), &ctx)
        .expect("bind_to_xsd=true without xsd_path should succeed");

    assert!(config.bind_to_xsd);
    assert_eq!(
        config.xsd_path, None,
        "xsd_path should be None when not configured"
    );
}

/// [`header_slot_text`]: the entity is bolded, what qualifies it is not, and a
/// validity line is not the issuer.
#[test]
fn header_slot_text_keeps_the_entity_and_drops_the_validity_line() {
    assert_eq!(
        header_slot_text("Gültig ab 02.01.2018\nUBS Europe SE").as_deref(),
        Some("&lt;b>UBS Europe SE&lt;/b>")
    );
    assert_eq!(
        header_slot_text("UBS Europe SE (Succursale Italia)").as_deref(),
        Some("&lt;b>UBS Europe SE&lt;/b> (Succursale Italia)")
    );
    assert_eq!(
        header_slot_text("UBS Europe SE\nSuccursale Italia").as_deref(),
        Some("&lt;b>UBS Europe SE&lt;/b> Succursale Italia")
    );
    // Nothing but a date: no line to print, so no draw.
    assert_eq!(header_slot_text("02.01.2018").as_deref(), None);
    assert_eq!(header_slot_text("   ").as_deref(), None);
}

/// The wizard toolbar's Submit and Back buttons must run the UBS
/// navigation helpers rather than the rule editor's built-in Submit Form
/// action, so that submission goes through the UBS validation/error path
/// (feedback registry PROBLEM-toolbar-nav-handler, UBS directive 2026-07-28,
/// reference form AAOV).
#[test]
fn toolbar_submit_and_back_run_ubs_navigation_helpers() {
    let xml = support::ubs_root_xml();

    assert!(
        xml.contains("window.forms.ubs.navigation.submit(submitErrorMessage);"),
        "Submit must call the UBS navigation helper. Got:\n{}",
        xml
    );
    assert!(
        xml.contains("window.forms.ubs.navigation.previousStep(this);"),
        "Back must call the UBS navigation helper. Got:\n{}",
        xml
    );
    assert!(
        !xml.contains("SUBMIT_FORM"),
        "the built-in Submit Form visual rule must be gone. Got:\n{}",
        xml
    );
    assert!(
        xml.contains("this.visible=(!this.panel.navigationContext.hasNextItem);"),
        "fd:navigationChange must survive. Got:\n{}",
        xml
    );
}

/// Every form carries the global FormMetadata fragment as its first wizard
/// step -- hidden and DoR-excluded (feedback registry
/// PROBLEM-formmetadata-step, reference form AAOV_033).
#[test]
fn root_panel_starts_with_formmetadata_fragment_step() {
    let xml = support::ubs_root_xml();

    let frag = xml
        .find("/content/dam/formsanddocuments/afforms_global_fragmentlib/formmetadata")
        .unwrap_or_else(|| panic!("FormMetadata fragment step missing. Got:\n{}", xml));
    assert!(
        xml.contains("name=\"FormMetadata\""),
        "the FormMetadata step must be named FormMetadata. Got:\n{}",
        xml
    );
    // It is the first child of the root panel's items, i.e. before the toolbar.
    let toolbar = xml.find("name=\"toolbar\"").expect("toolbar missing");
    assert!(
        frag < toolbar,
        "FormMetadata must be the first step. Got:\n{}",
        xml
    );
}

/// The summary step carries the two attributes the Redacto rule requires.
///
/// `PROBLEM-summary-step-redacto`'s reference gained `dorFieldStyling="Default"`
/// and `visible="{Boolean}true"` on the panel itself. Both are inert
/// defaults, so nothing behaves differently -- but without them the detector
/// buckets every form the engine converts as `off-shape`.
#[test]
fn the_summary_step_carries_the_redacto_panel_attributes() {
    let xml = support::ubs_root_xml();
    let panel = xml
        .split("<summarypanel")
        .nth(1)
        .expect("no summary panel");
    let open_tag = panel.split('>').next().unwrap_or_default();
    for attr in ["dorFieldStyling=\"Default\"", "visible=\"{Boolean}true\""] {
        assert!(
            open_tag.contains(attr),
            "the summary panel must carry {attr}, got:\n{open_tag}"
        );
    }
}

/// PROBLEM-nav-save-progress-required: the toolbar carries the Save Progress
/// button, visible, as its LAST child -- last is what keeps
/// PROBLEM-nav-button-order (Next, Submit, Back first) green.
#[test]
fn toolbar_ends_with_the_save_progress_button() {
    let xml = support::ubs_root_xml();

    assert!(
        xml.contains("name=\"fwbSaveProgress\""),
        "the toolbar has no Save Progress button"
    );
    assert!(
        xml.contains("window.forms.ubs.fwb.saveFormData();"),
        "the Save Progress button has no click handler"
    );
    let toolbar = xml
        .find("name=\"toolbar\"")
        .map(|at| &xml[at..])
        .expect("toolbar");
    let items_end = toolbar.find("</items>").expect("toolbar items");
    let last_button = toolbar[..items_end]
        .rfind("name=\"")
        .map(|at| &toolbar[at..][..40])
        .unwrap_or("");
    assert!(
        last_button.starts_with("name=\"fwbSaveProgress\""),
        "Save Progress must be the toolbar's last child, found {last_button}"
    );
    let button = support::open_tags(&xml)
        .into_iter()
        .find(|(_, tag)| tag.contains("name=\"fwbSaveProgress\""))
        .map(|(_, tag)| tag)
        .expect("the Save Progress tag");
    assert!(
        !button.contains("visible=\"{Boolean}false\""),
        "the Save Progress button is hidden:\n{button}"
    );
}

/// PROBLEM-panel-type-ubs: every panel is the UBS custom panel. The default
/// AEM panel has no Summary authoring section, so options that live there
/// (the jump-to-field button) cannot be set or rendered on it.
#[test]
fn rendered_form_uses_the_ubs_panel_everywhere() {
    let page = AemNode::Panel {
        uuid: Uuid::new_v4(),
        name: "PN_Page".into(),
        title: "A page".into(),
        children: vec![text_field("TXT_a", "Name")],
        is_page: true,
        attrs: AemAttrs::default(),
        visible: true,
        is_conditional: false,
        dor_num_cols: None,
        colspan: 12,
        dor_colspan: None,
        bind_ref: None,
        frag_ref: None,
    };
    let xml = support::ubs_xml(vec![page]);

    let offenders: Vec<_> = support::open_tags(&xml)
        .into_iter()
        .filter(|(_, tag)| tag.contains("sling:resourceType=\"fd/af/components/panel\""))
        .map(|(name, _)| name)
        .collect();
    assert!(
        offenders.is_empty(),
        "{} node(s) still use the default AEM panel: {}",
        offenders.len(),
        offenders.join(", ")
    );
}

/// PROBLEM-dor-exclusion-implies-summary: the UBS DoR is rendered by Redacto
/// from the summary data, so a node kept out of the DoR but left in the
/// summary still reaches the reader. Owner directive 2026-08-26: no
/// exceptions by component type.
#[test]
fn dor_excluded_nodes_are_summary_excluded_too() {
    let mut dor_excluded_field = text_field("TXT_a", "Name");
    if let AemNode::TextField { attrs, .. } = &mut dor_excluded_field {
        attrs.dor_exclude = true;
    }
    let mut dor_excluded_panel = AemNode::Panel {
        uuid: Uuid::new_v4(),
        name: "PN_Hidden".into(),
        title: "Hidden".into(),
        children: vec![text_field("TXT_b", "Other")],
        is_page: false,
        attrs: AemAttrs::default(),
        visible: true,
        is_conditional: false,
        dor_num_cols: None,
        colspan: 12,
        dor_colspan: None,
        bind_ref: None,
        frag_ref: None,
    };
    if let AemNode::Panel { attrs, .. } = &mut dor_excluded_panel {
        attrs.dor_exclude = true;
    }
    let xml = support::ubs_xml(vec![dor_excluded_field, dor_excluded_panel]);

    let offenders: Vec<_> = support::open_tags(&xml)
        .into_iter()
        .filter(|(_, tag)| {
            tag.contains("dorExclusion=\"true\"") && !tag.contains("summaryExclusion=\"true\"")
        })
        .map(|(name, _)| name)
        .collect();
    assert!(
        offenders.is_empty(),
        "{} node(s) are excluded from the DoR but not from the summary: {}",
        offenders.len(),
        offenders.join(", ")
    );
}

/// PROBLEM-visual-editor-rules: a rule lives in the code editor as JavaScript
/// (a SCRIPTMODEL on `fd:scripts`), and the `fd:rules` node stays empty.
#[test]
fn rendered_form_has_no_visual_editor_rules() {
    let xml = support::ubs_xml(vec![text_field("TXT_a", "Name")]);
    let offenders = support::open_tags(&xml)
        .into_iter()
        .filter(|(name, tag)| name == "fd:rules" && tag.contains("fd:"))
        .filter(|(_, tag)| {
            tag.contains("fd:visible=")
                || tag.contains("fd:click=")
                || tag.contains("fd:valueCommit=")
                || tag.contains("fd:init=")
                || tag.contains("fd:calculate=")
                || tag.contains("fd:validate=")
        })
        .count();
    assert_eq!(offenders, 0, "{offenders} visual-editor rule(s) left");
}

/// On the first page -- the one carrying the banking relationship, marked by
/// a `Preface` child -- the heading is a `subtitle-after-form-title` static
/// text, not an `h2` step title, because an `h2` does not appear in the
/// finished DoR. Every other page keeps its step title.
#[test]
fn the_first_page_heading_is_a_subtitle_not_a_step_title() {
    let first_page = AemNode::Panel {
        uuid: Uuid::new_v4(),
        name: "PN_BR".into(),
        title: "Banking relationship".into(),
        children: vec![
            AemNode::Preface {
                uuid: Uuid::new_v4(),
                name: "PN_BR".into(),
            },
            text_field("TXT_a", "Name"),
        ],
        is_page: true,
        attrs: AemAttrs::default(),
        visible: true,
        is_conditional: false,
        dor_num_cols: None,
        colspan: 12,
        dor_colspan: None,
        bind_ref: None,
        frag_ref: None,
    };
    let second_page = AemNode::Panel {
        uuid: Uuid::new_v4(),
        name: "PN_Step2".into(),
        title: "Step two".into(),
        children: vec![text_field("TXT_b", "Other")],
        is_page: true,
        attrs: AemAttrs::default(),
        visible: true,
        is_conditional: false,
        dor_num_cols: None,
        colspan: 12,
        dor_colspan: None,
        bind_ref: None,
        frag_ref: None,
    };
    let xml = support::ubs_xml(vec![first_page, second_page]);

    let subtitles: Vec<_> = support::open_tags(&xml)
        .into_iter()
        .filter(|(_, tag)| tag.contains("css=\"subtitle-after-form-title\""))
        .collect();
    assert_eq!(
        subtitles.len(),
        1,
        "expected exactly one first-page subtitle, found {}",
        subtitles.len()
    );
    let (_, subtitle) = &subtitles[0];
    assert!(
        subtitle.contains("controls/textdraw") && !subtitle.contains("headingLevel"),
        "the subtitle must be a plain static text:\n{subtitle}"
    );
    assert!(
        subtitle.contains("name=\"ST_"),
        "a static text is named ST_:\n{subtitle}"
    );
    assert!(
        !subtitle.contains("dorExclusion=") && !subtitle.contains("summaryExclusion="),
        "the subtitle is excluded from the DoR it exists for:\n{subtitle}"
    );

    // The panel around it keeps the Edit button and carries no title.
    let at = xml.find("css=\"subtitle-after-form-title\"").unwrap();
    let wrapper = support::open_tags(&xml[..at])
        .into_iter()
        .rev()
        .find(|(name, _)| name.starts_with("panel_title_"))
        .map(|(_, tag)| tag)
        .expect("no title panel around the subtitle");
    assert!(
        !wrapper.contains("jcr:title="),
        "the wrapper repeats the subtitle as its own title:\n{wrapper}"
    );
    assert!(
        wrapper.contains("jumpToFieldButtonVisible=\"true\""),
        "the first page's wrapper keeps the Edit button:\n{wrapper}"
    );

    // Every other page keeps its step title.
    assert!(
        xml.contains("css=\"stepTitle\""),
        "the other pages lost their step titles"
    );
}

/// The legal-entity line under the banking relationship goes to the DoR's
/// second header slot, hidden on screen and off the summary step. Its text
/// is the source document's own master-page header, so a form without one
/// gets no draw.
#[test]
fn the_banking_preface_carries_the_dor_header_slot_text() {
    let (profile, templates) = support::ubs_profile();
    let mut vars = HashMap::new();
    vars.insert("formrange_code".into(), "AAOS".into());
    vars.insert("formrange_entity".into(), "033".into());
    let mut ctx = Context::new("it".to_string(), vars);
    ctx.header = Some("UBS Europe SE (Succursale Italia)".to_string());
    let config = AemConfig::from_profile(&profile, templates, &ctx)
        .expect("profile config");

    let root = AemNode::Root {
        title: "AAOS".into(),
        children: vec![AemNode::Preface {
            uuid: Uuid::new_v4(),
            name: "PN_BR".into(),
        }],
    };
    let xml = generate_aem_xml(&root, &config);

    let draw = support::open_tags(&xml)
        .into_iter()
        .find(|(_, tag)| tag.contains("dorHeaderSlot=\"slot2\""))
        .map(|(_, tag)| tag)
        .unwrap_or_else(|| panic!("no slot-2 header draw"));
    for attr in [
        "alwaysInPdf=\"true\"",
        "showIfHidden=\"true\"",
        "summaryExclusion=\"true\"",
        "visible=\"{Boolean}false\"",
        "name=\"ST_HeaderSlot2\"",
    ] {
        assert!(
            draw.contains(attr),
            "the slot-2 draw lacks {attr}:\n{draw}"
        );
    }
    assert!(
        !draw.contains("dorExclusion="),
        "the slot-2 draw exists to reach the DoR:\n{draw}"
    );
    assert!(
        draw.contains("&lt;b>UBS Europe SE&lt;/b> (Succursale Italia)"),
        "the slot-2 text is not the source's own header:\n{draw}"
    );
}

/// The configurator reset carries the archetype the sweep recognises it by: an
/// `_archetype` field and a canonical first-line comment. Built by hand here --
/// an approved-wording radio whose `ConditionRule`s decide two named panels --
/// rather than through a whole form, so the mechanism is pinned directly.
#[test]
fn the_configurator_reset_carries_its_archetype() {
    let panel = |name: &str| AemNode::Panel {
        uuid: Uuid::new_v4(),
        name: name.into(),
        title: name.into(),
        children: vec![text_field(&format!("TXT_{name}"), "Field")],
        is_page: false,
        attrs: AemAttrs::default(),
        visible: true,
        is_conditional: true,
        dor_num_cols: None,
        colspan: 12,
        dor_colspan: None,
        bind_ref: None,
        frag_ref: None,
    };
    let choice = AemNode::RadioButton {
        attrs: AemAttrs::default(),
        uuid: Uuid::new_v4(),
        name: "RB_Kind".into(),
        label: "Art".into(),
        options: vec![
            AemOption {
                label: "Individual".into(),
                value: "1".into(),
            },
            AemOption {
                label: "Company/Entity".into(),
                value: "2".into(),
            },
        ],
        alignment: OptionAlignment::Vertical,
        mandatory: true,
        visible: true,
        colspan: 12,
        dor_colspan: None,
        conditions: vec![
            ConditionRule {
                value: InputValue::Text("1".into()),
                target_panel_name: "PN_Option1".into(),
                show: true,
            },
            ConditionRule {
                value: InputValue::Text("2".into()),
                target_panel_name: "PN_Option2".into(),
                show: true,
            },
        ],
        bind_ref: None,
    };
    let xml = support::ubs_xml(vec![choice, panel("PN_Option1"), panel("PN_Option2")]);

    assert!(
        xml.contains("configurator-reset-on-change"),
        "the approved-wording radio must carry a reset:\n{xml}"
    );
    assert!(
        xml.contains(
            "// [configurator-reset-on-change] Generated automatically. \
             Do not edit: will be overwritten. Create your own different script."
        ),
        "the reset must open with the canonical archetype comment"
    );
    assert!(
        xml.contains("_archetype") && xml.contains("configurator-reset-on-change"),
        "the reset document must carry the _archetype field"
    );
    assert!(
        !xml.contains("emptied on change"),
        "the pre-2026-08-19 marker must be gone"
    );
}

/// A profile that ships no `email`/`telephone` template must still emit the
/// field. Dropping it is the failure mode this guards: `render_node` returns
/// an empty string for a missing template, so without the fallback an email
/// field would vanish from the package with nothing but a log line to show
/// for it.
#[test]
fn a_profile_without_contact_templates_falls_back_to_the_text_box() {
    let (profile, mut templates) = support::ubs_profile();
    templates.remove("email");
    templates.remove("telephone");
    let mut vars = HashMap::new();
    vars.insert("formrange_code".into(), "AAEI".into());
    vars.insert("formrange_entity".into(), "019".into());
    let ctx = Context::new("de".to_string(), vars);
    let config = AemConfig::from_profile(&profile, templates, &ctx)
        .expect("build AemConfig from the UBS profile");

    let root = AemNode::Root {
        title: "Contact".into(),
        children: vec![AemNode::TextField {
            attrs: AemAttrs::default(),
            uuid: Uuid::new_v5(&Uuid::NAMESPACE_URL, b"EML_Email"),
            name: "EML_Email".into(),
            label: "E-Mail".into(),
            mandatory: false,
            visible: true,
            max_chars: None,
            colspan: 6,
            dor_colspan: None,
            bind_ref: None,
            kind: TextFieldKind::Email,
        }],
    };
    let xml = generate_aem_xml(&root, &config);
    assert!(
        xml.contains("name=\"EML_Email\"") && xml.contains("controls/textbox"),
        "the field must survive as a plain text box:\n{xml}"
    );
}

/// The form configurator opens on the individual option whichever path
/// emitted the radio, recognised by its option labels, not by one field name.
#[test]
fn an_authored_configurator_radio_also_preselects_private_person() {
    let (profile, templates) = support::ubs_profile();
    let mut vars = HashMap::new();
    vars.insert("formrange_code".into(), "TEST".into());
    vars.insert("formrange_entity".into(), "019".into());
    let ctx = Context::new("de".to_string(), vars);
    let config = AemConfig::from_profile(&profile, templates, &ctx)
        .expect("profile config");

    let choice = |name: &str, labels: &[&str], first_key: usize| AemNode::RadioButton {
        attrs: AemAttrs::default(),
        uuid: Uuid::from_u128(1),
        name: name.into(),
        label: "Formular Adressat".into(),
        options: labels
            .iter()
            .enumerate()
            .map(|(i, label)| AemOption {
                label: (*label).into(),
                value: (i + first_key).to_string(),
            })
            .collect(),
        alignment: OptionAlignment::Horizontal,
        mandatory: true,
        visible: true,
        colspan: 12,
        dor_colspan: None,
        conditions: vec![],
        bind_ref: None,
    };
    let radio = |name: &str| {
        choice(
            name,
            &["Private Person", "Minderjährige", "Firma", "GbR"],
            1,
        )
    };
    let render = |node: AemNode| {
        generate_aem_xml(
            &AemNode::Root {
                title: "TEST".into(),
                children: vec![node],
            },
            &config,
        )
    };

    let tag_of = |node: AemNode, name: &str| {
        let xml = render(node);
        let at = xml
            .find(&format!("name=\"{name}\""))
            .unwrap_or_else(|| panic!("the radio must be emitted:\n{xml}"));
        let start = xml[..at].rfind('<').expect("inside a tag");
        let end = start + xml[start..].find('>').expect("the tag must close");
        xml[start..end].to_string()
    };
    let radio_tag = |name: &str| tag_of(radio(name), name);

    assert!(
        radio_tag("RB_FormularAdressat").contains(r#"_value="1""#),
        "the configurator radio must open preselected:\n{}",
        radio_tag("RB_FormularAdressat")
    );
    let ordinary = choice("RB_Something_Else", &["Yes", "No"], 1);
    let tag = tag_of(ordinary, "RB_Something_Else");
    assert!(
        !tag.contains("_value="),
        "an ordinary radio must not be preselected:\n{tag}"
    );

    let italian = choice("RB_Tipo", &["Individuo", "Entità giuridica"], 3);
    let tag = tag_of(italian, "RB_Tipo");
    assert!(
        tag.contains(r#"_value="3""#),
        "the individual option's own key must be written:\n{tag}"
    );
}

/// A message box loaded back out of a package stays a message box.
///
/// Before it had its own node it degraded to a `TextDraw` on load, which
/// loses what makes it a notice: it came back as ordinary static text and
/// would land in the DoR on the next write.
#[test]
fn a_loaded_message_box_round_trips_as_a_notice() {
    let node = AemNode::MessageBox {
        uuid: Uuid::new_v4(),
        name: "TB_Info".into(),
        content: "SEC-SH-Dauerauftrag-DE: notice text".into(),
        attrs: AemAttrs::dor_excluded(),
        visible: true,
        colspan: 12,
        dor_colspan: None,
    };
    let config = support::ubs_config("de", "AABF", "019");
    let root = AemNode::Root {
        title: "AABF".into(),
        children: vec![node],
    };
    let xml = generate_aem_xml(&root, &config);
    let zip = support::aem_zip_from_form_xml(&config.form_code, &xml);
    let parsed = parse_aem_zip(&zip).expect("parse the package back");

    let mut kinds = Vec::new();
    support::walk_aem_nodes(&parsed.root, &mut |node| {
        if let AemNode::MessageBox { name, content, .. } = node {
            kinds.push(format!("{name}|{content}"));
        }
    });

    assert!(
        kinds.iter().any(|k| k.contains("SEC-SH-Dauerauftrag-DE")),
        "the notice must come back as a MessageBox, got {kinds:?}"
    );
}

fn text_field(name: &str, label: &str) -> AemNode {
    AemNode::TextField {
        attrs: AemAttrs::default(),
        uuid: Uuid::new_v4(),
        name: name.into(),
        label: label.into(),
        mandatory: false,
        visible: true,
        max_chars: None,
        colspan: 12,
        dor_colspan: None,
        bind_ref: None,
        kind: TextFieldKind::Plain,
    }
}

