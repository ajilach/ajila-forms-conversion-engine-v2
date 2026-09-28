//! `xsd::` config and AEM -> XSD walk tests ported from `blueprint` (the
//! deleted `core` crate)'s `tests/mod.rs` (`ajilach/ajila-forms-conversion-engine`
//! at commit `f5f596a`, the last commit before `core/` was deleted).
//!
//! The fixture-based tests parse a bare form `.content.xml` from
//! `tests/fixtures/ubs-xsd/` (the UBS `af-xsd-automation` reference fixtures;
//! see that directory's README for provenance) rather than a source PDF, and
//! the `ubs_xsd_naming` tests build their `AemNode` trees by hand -- neither
//! touches the mechanical PDF/XFA -> `StructuredNode` pipeline, which no
//! longer exists in this crate.

#[path = "support/mod.rs"]
mod support;

use u2s_aem_ubs_mcp::xsd::{XsdConfig, XsdProfile, to_snake_case};

#[test]
fn test_xsd_snake_case_conversion() {
    assert_eq!(to_snake_case("Date of Birth"), "date_of_birth");
    assert_eq!(to_snake_case("Phone Number"), "phone_number");
    assert_eq!(to_snake_case("IBAN"), "iban");
    assert_eq!(to_snake_case("first name"), "first_name");
    assert_eq!(
        to_snake_case("Account Details (Primary)"),
        "account_details_primary"
    );
    assert_eq!(to_snake_case(""), "unknown");
    assert_eq!(to_snake_case("single"), "single");
}

#[test]
fn test_xsd_profile_master_language_from_toml_is_applied() {
    let toml_str = r#"
schemaLocationPrefix = "../"
masterLanguage = "en"
"#;

    let profile: XsdProfile = toml::from_str(toml_str).expect("parse xsd profile");
    let config = XsdConfig::from_profile(profile);

    assert_eq!(config.master_language.as_deref(), Some("en"));
}

#[test]
fn test_xsd_profile_master_language_defaults_to_none() {
    let profile = XsdProfile::default();
    let config = XsdConfig::from_profile(profile);

    assert_eq!(config.master_language.as_deref(), None);
}

// ============================================================================
// AEM -> XSD fixture tests
// ============================================================================

/// Tests driven by the UBS `af-xsd-automation` reference fixtures under
/// `tests/fixtures/ubs-xsd/`. See that directory's README for provenance and
/// for which parts of the reference are derivable versus config-supplied.
mod aem_xsd_fixtures {
    use super::support;

    /// The AEM -> XSD walk needs each fragment panel's `fragRef` to pick the
    /// right global element or complex type.
    ///
    /// `convert_fragment` inlines every fragment it can resolve -- from the
    /// ZIP or from `profiles/ubs/aem/fragments/` -- into a plain `Panel`, so
    /// the panel has to carry `frag_ref` for the reference to be
    /// reproducible at all: four of AF_ABFA's fourteen XSD elements come
    /// from such fragments.
    #[test]
    fn parsed_abfa_retains_every_fragment_ref() {
        let root = support::parse_fixture_form("AF_ABFA");
        let mut found = Vec::new();
        support::walk_aem_nodes(&root, &mut |node| {
            use u2s_aem_ubs_mcp::aem::AemNode;
            match node {
                AemNode::Fragment { frag_ref, .. } => found.push(frag_ref.clone()),
                AemNode::Panel {
                    frag_ref: Some(fr), ..
                }
                | AemNode::Repeatable {
                    frag_ref: Some(fr), ..
                } => found.push(fr.clone()),
                _ => {}
            }
        });

        for expected in [
            "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_BankingRelationship1",
            "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_ContractualPartnerGeneric1",
            "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1",
        ] {
            assert!(
                found.iter().any(|f| f == expected),
                "fragRef {expected} was lost during parse.\nFound: {found:#?}"
            );
        }

        let signature_refs = found
            .iter()
            .filter(|f| f.ends_with("affrg_SignatureGeneric1"))
            .count();
        assert_eq!(
            signature_refs, 2,
            "expected both SignatureGeneric fragments, found {signature_refs}"
        );
    }

    /// AF_ABFA's two repeating panels use `maxOccur="-1"` (unbounded). They
    /// must survive as `Repeatable`, because they are what produce the
    /// `EmailAddressInstruction` and `DomainInstruction` elements with
    /// `maxOccurs="50"` -- a plain `Panel` would emit no grouping element at
    /// all.
    #[test]
    fn parsed_abfa_recognises_unbounded_repeat_panels() {
        use u2s_aem_ubs_mcp::aem::AemNode;

        let root = support::parse_fixture_form("AF_ABFA");
        let mut repeats = Vec::new();
        support::walk_aem_nodes(&root, &mut |node| {
            if let AemNode::Repeatable {
                name,
                title,
                max_occur,
                ..
            } = node
            {
                repeats.push((name.clone(), title.clone(), *max_occur));
            }
        });

        for expected in ["PN_RepeatEmailAddress", "PN_RepeatDomain"] {
            let found = repeats.iter().find(|(n, _, _)| n == expected);
            let (_, _, max_occur) = found.unwrap_or_else(|| {
                panic!("{expected} was not parsed as a Repeatable.\nFound: {repeats:#?}")
            });
            assert_eq!(
                *max_occur,
                AemNode::UNBOUNDED_OCCUR,
                "{expected} has maxOccur=\"-1\" and must round-trip as unbounded"
            );
        }
    }

    /// Compare a generated schema with the UBS reference **structurally**:
    /// the element tree, names, `ref` versus `name`/`type`, occurrence
    /// attributes and the ordered include list. Our formatting (2-space
    /// indent, no XMLSpy header comment) deliberately differs from theirs,
    /// so a byte comparison would test the wrong thing.
    fn xsd_shape(xsd: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        for raw in xsd.lines() {
            let line = raw.trim();
            if line.starts_with("<xs:include") {
                let loc = attr(line, "schemaLocation").unwrap_or_default();
                out.push(format!("include {loc}"));
            } else if line.starts_with("<xs:element") {
                let mut parts = vec![format!("{}element", "  ".repeat(depth))];
                for key in ["name", "ref", "type", "minOccurs", "maxOccurs"] {
                    if let Some(v) = attr(line, key) {
                        parts.push(format!("{key}={v}"));
                    }
                }
                out.push(parts.join(" "));
                if !line.ends_with("/>") {
                    depth += 1;
                }
            } else if line.starts_with("</xs:element") {
                depth = depth.saturating_sub(1);
            }
        }
        out
    }

    fn attr(line: &str, key: &str) -> Option<String> {
        let needle = format!("{key}=\"");
        let start = line.find(&needle)? + needle.len();
        let rest = &line[start..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }

    /// AF_BBEO is an *old-model* form: its fragments live in
    /// `afforms_ch_fragmentlib`, and its reference is the output of UBS's
    /// own tool rather than a hand-curated schema. We reproduce its element
    /// names, types and includes exactly.
    ///
    /// Two differences are artefacts of that reference and are normalised
    /// away: it predates `minOccurs="0"` (the curated AF_ABFA reference has
    /// it), and it was generated with an empty form code so its root is a
    /// bare `UBSAF`.
    #[test]
    fn bbeo_matches_the_ubs_tool_modulo_known_artefacts() {
        let root = support::parse_fixture_form("AF_BBEO");
        let mut config = support::ubs_xsd_config();
        config.form_code = Some("BBEO".to_string());
        let fragments = support::ubs_fragments();

        let normalise = |xsd: &str| -> Vec<String> {
            xsd_shape(xsd)
                .into_iter()
                .map(|line| {
                    line.replace(" minOccurs=0", "")
                        .replace("name=UBSAF_BBEO", "name=UBSAF")
                })
                .collect()
        };

        let ours = normalise(&u2s_aem_ubs_mcp::xsd::generate_xsd_string_from_aem(
            &root, &config, &fragments,
        ));
        let theirs = normalise(&support::read_fixture("ubs-xsd/AF_BBEO/reference.schema.xsd"));

        assert_eq!(
            ours, theirs,
            "our AF_BBEO schema no longer matches UBS's.\nours:   {ours:#?}\ntheirs: {theirs:#?}"
        );
    }

    /// Diff our schema against every UBS fixture that has a source/reference
    /// pair, so the gap to their tool is measured rather than assumed.
    #[test]
    #[ignore = "diagnostic"]
    fn diff_every_ubs_fixture() {
        for code in ["AF_ABFA", "AF_BBRR", "AF_ABSD", "AF_BBEO"] {
            let root = support::parse_fixture_form(code);
            let mut config = support::ubs_xsd_config();
            config.form_code = Some(code.trim_start_matches("AF_").to_string());
            let fragments = support::ubs_fragments();
            let ours = xsd_shape(&u2s_aem_ubs_mcp::xsd::generate_xsd_string_from_aem(
                &root, &config, &fragments,
            ));
            let theirs = xsd_shape(&support::read_fixture(&format!(
                "ubs-xsd/{code}/reference.schema.xsd"
            )));

            let ours_set: std::collections::BTreeSet<&String> = ours.iter().collect();
            let theirs_set: std::collections::BTreeSet<&String> = theirs.iter().collect();
            let only_theirs: Vec<&&String> = theirs_set.difference(&ours_set).collect();
            let only_ours: Vec<&&String> = ours_set.difference(&theirs_set).collect();

            println!(
                "\n=== {code}: ours {} lines, theirs {} lines, {} missing, {} extra{}",
                ours.len(),
                theirs.len(),
                only_theirs.len(),
                only_ours.len(),
                if ours == theirs { "  [IDENTICAL]" } else { "" }
            );
            for l in only_theirs.iter().take(14) {
                println!("   - only UBS: {l}");
            }
            for l in only_ours.iter().take(14) {
                println!("   + only us : {l}");
            }
        }
    }

    /// Load the fixture form and generate its schema through the real UBS
    /// profile, exactly as production would.
    fn abfa_generated_xsd() -> String {
        let root = support::parse_fixture_form("AF_ABFA");
        let mut config = support::ubs_xsd_config();
        config.form_code = Some("ABFA".to_string());
        let fragments = support::ubs_fragments();
        u2s_aem_ubs_mcp::xsd::generate_xsd_string_from_aem(&root, &config, &fragments)
    }

    /// The headline acceptance check: our AEM -> XSD walk reproduces the
    /// schema UBS derives for the same form.
    #[test]
    fn abfa_aem_to_xsd_matches_ubs_reference_shape() {
        let actual = xsd_shape(&abfa_generated_xsd());
        let expected = xsd_shape(&support::read_fixture("ubs-xsd/AF_ABFA/reference.schema.xsd"));

        if actual != expected {
            let mut msg = String::from("generated schema differs from the UBS reference\n");
            for (i, line) in expected.iter().enumerate() {
                let got = actual.get(i).map(String::as_str).unwrap_or("<missing>");
                let mark = if got == line { " " } else { "!" };
                msg.push_str(&format!("{mark} want: {line}\n{mark}  got: {got}\n"));
            }
            for extra in actual.iter().skip(expected.len()) {
                msg.push_str(&format!("! unexpected: {extra}\n"));
            }
            panic!("{msg}");
        }
    }

    /// Config may name nodes the AEM tree cannot name on its own; it must
    /// never reorder or re-nest what the tree already determines.
    ///
    /// Regenerate AF_ABFA with every naming action (`element` / `type`)
    /// stripped from the `[[aemElements]]` rules -- keeping `ignore` and
    /// `occurs`, which are shape rules by design -- and assert the result is
    /// a strict *subsequence* of the full schema's skeleton.
    #[test]
    fn abfa_shape_is_derivable_without_config_names() {
        let root = support::parse_fixture_form("AF_ABFA");
        let fragments = support::ubs_fragments();

        let mut with_names = support::ubs_xsd_config();
        with_names.form_code = Some("ABFA".to_string());

        let mut without_names = with_names.clone();
        for rule in &mut without_names.profile.aem_elements {
            if !rule.ignore {
                rule.element = None;
                rule.type_ref = None;
            }
        }

        let full = skeleton(
            &u2s_aem_ubs_mcp::xsd::generate_xsd_from_aem(&root, &with_names, &fragments)
                .schema
                .to_xml(),
        );
        let stripped = skeleton(
            &u2s_aem_ubs_mcp::xsd::generate_xsd_from_aem(&root, &without_names, &fragments)
                .schema
                .to_xml(),
        );

        let mut next = 0usize;
        for entry in &stripped {
            match full[next..].iter().position(|e| e == entry) {
                Some(offset) => next += offset + 1,
                None => panic!(
                    "stripping the naming rules changed the schema shape: {entry} is out of \
                     order or missing.\nfull:     {full:#?}\nstripped: {stripped:#?}"
                ),
            }
        }

        assert!(
            full.len() - stripped.len() <= 2,
            "config added {} elements; it should only name what the tree cannot",
            full.len() - stripped.len()
        );
    }

    /// The nesting depth and occurrence attributes of every element, with
    /// names, refs and types blanked.
    fn skeleton(xsd: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        for raw in xsd.lines() {
            let line = raw.trim();
            if line.starts_with("<xs:element") {
                let min = attr(line, "minOccurs").unwrap_or_else(|| "-".into());
                let max = attr(line, "maxOccurs").unwrap_or_else(|| "-".into());
                let kind = if line.ends_with("/>") { "leaf" } else { "group" };
                out.push(format!("{depth} {kind} min={min} max={max}"));
                if !line.ends_with("/>") {
                    depth += 1;
                }
            } else if line.starts_with("</xs:element") {
                depth = depth.saturating_sub(1);
            }
        }
        out
    }

    /// The bindRefs our walk assigns must be the ones UBS injected into
    /// `reference.content.xml` -- same paths, same order.
    ///
    /// Two of UBS's sixteen bindRefs are rooted at the generic `/UBSAF/`
    /// prefix and address the *global fragment library's* schema, not this
    /// form's, so they are excluded here (see the fixture README).
    #[test]
    fn abfa_bind_refs_match_the_ubs_reference_form() {
        let root = support::parse_fixture_form("AF_ABFA");
        let mut config = support::ubs_xsd_config();
        config.form_code = Some("ABFA".to_string());
        let fragments = support::ubs_fragments();

        let result = u2s_aem_ubs_mcp::xsd::generate_xsd_from_aem(&root, &config, &fragments);
        let mut ours: Vec<&str> = result.bind_refs.values().map(String::as_str).collect();
        ours.sort_unstable();

        let reference = support::read_fixture("ubs-xsd/AF_ABFA/reference.content.xml");
        let mut theirs: Vec<&str> = reference
            .match_indices("bindRef=\"")
            .filter_map(|(i, m)| {
                let rest = &reference[i + m.len()..];
                rest.find('"').map(|end| &rest[..end])
            })
            .filter(|b| b.starts_with("/UBSAF_ABFA/"))
            .collect();
        theirs.sort_unstable();

        assert_eq!(
            ours, theirs,
            "our bindRefs differ from the ones UBS injected into the same form"
        );
    }

    /// Element names that appear twice inside the same `xs:sequence`.
    fn duplicate_sibling_names(xsd: &str) -> Vec<String> {
        let mut stack: Vec<Vec<String>> = vec![Vec::new()];
        let mut dupes = Vec::new();
        for raw in xsd.lines() {
            let line = raw.trim();
            if line.starts_with("<xs:sequence") {
                stack.push(Vec::new());
            } else if line.starts_with("</xs:sequence") {
                stack.pop();
            } else if line.starts_with("<xs:element") {
                let name = attr(line, "name").or_else(|| attr(line, "ref"));
                if let (Some(name), Some(top)) = (name, stack.last_mut()) {
                    if top.contains(&name) {
                        dupes.push(name.clone());
                    }
                    top.push(name);
                }
            }
        }
        dupes
    }

    /// The structural contract, over the four UBS reference fixture forms
    /// (`tests/fixtures/ubs-xsd/`) instead of a full PDF/XFA pipeline run:
    /// because `generate_xsd_from_aem` derives the schema from the finished
    /// AEM tree and writes each node's `bind_ref` during that same walk,
    /// these must hold for every tree it is given, source PDF or hand-built
    /// alike.
    #[test]
    fn aem_bind_refs_are_exact_xsd_paths() {
        use std::collections::{HashMap, HashSet};
        use u2s_aem_ubs_mcp::aem::AemNode;

        for code in ["AF_ABFA", "AF_ABSD", "AF_BBEO", "AF_BBRR"] {
            let mut root = support::parse_fixture_form(code);
            let mut config = support::ubs_xsd_config();
            config.form_code = Some(code.trim_start_matches("AF_").to_string());
            let fragments = support::ubs_fragments();
            let result = u2s_aem_ubs_mcp::xsd::generate_xsd_from_aem(&root, &config, &fragments);
            // `generate_xsd_from_aem` returns the bind_refs as a side map
            // keyed by uuid; write them onto the tree's own `bind_ref`
            // fields the way `document::lower` does, so `node_bind_ref` (and
            // hence assertion (4) below) sees them.
            u2s_aem_ubs_mcp::xsd::apply_bind_refs(&mut root, &result.bind_refs);
            let schema = result.schema;

            let xsd_paths = support::xsd_element_paths_in_order(&schema);
            let known: HashSet<&str> = xsd_paths.iter().map(String::as_str).collect();

            let mut bound: Vec<(String, String)> = Vec::new();
            support::walk_aem_nodes(&root, &mut |node| {
                if let Some(br) = support::node_bind_ref(node) {
                    bound.push((node.element_name(), br.to_string()));
                }
            });

            assert!(!bound.is_empty(), "{code}: no node was bound at all");

            // (1) Every bindRef is an exact element path -- never a prefix
            //     match.
            for (owner, br) in &bound {
                assert!(
                    known.contains(br.as_str()),
                    "{code}: {owner} is bound to {br}, which is not an element in the generated XSD"
                );
            }

            // (2) No path is bound twice.
            let mut seen: HashMap<&str, &str> = HashMap::new();
            for (owner, br) in &bound {
                if let Some(prev) = seen.insert(br.as_str(), owner.as_str()) {
                    panic!("{code}: {br} is bound by both {prev} and {owner}");
                }
            }

            // (3) Document order of the bindRefs follows the schema's own
            //     order (a subsequence, not equality).
            let mut next = 0usize;
            for (owner, br) in &bound {
                match xsd_paths[next..].iter().position(|p| p == br) {
                    Some(offset) => next += offset + 1,
                    None => panic!(
                        "{code}: {owner} bound to {br} appears out of order relative to the schema"
                    ),
                }
            }

            // (4) Every visible data field is bound, unless it sits inside a
            //     panel the walk has already resolved to a fragment `ref=`:
            //     such a panel's own inner fields are the fragment library's
            //     schema to bind, not this form's, and the walk does not
            //     descend into them for that reason (the same reason a field
            //     inside an opaque `Custom` node is exempt). An invisible
            //     field is exempt too: `node_visible` in `xsd/from_aem.rs`
            //     documents that an invisible field "carries no data worth
            //     binding" -- AF_ABFA's `TXT_Format` is one, a permanently
            //     hidden scripting helper for a print-blank URL.
            fn assert_fields_bound(code: &str, node: &AemNode, inside_fragment: bool) {
                let is_fragment_panel = matches!(
                    node,
                    AemNode::Panel { frag_ref: Some(_), .. }
                        | AemNode::Repeatable { frag_ref: Some(_), .. }
                );
                let field = match node {
                    AemNode::TextField { name, visible, .. }
                    | AemNode::NumberField { name, visible, .. }
                    | AemNode::DatePicker { name, visible, .. }
                    | AemNode::Dropdown { name, visible, .. }
                    | AemNode::Checkbox { name, visible, .. }
                    | AemNode::RadioButton { name, visible, .. } => Some((name, *visible)),
                    _ => None,
                };
                if let Some((name, visible)) = field {
                    assert!(
                        inside_fragment || !visible || support::node_bind_ref(node).is_some(),
                        "{code}: field {name} is unbound, so it has no element in the schema"
                    );
                }
                let children = match node {
                    AemNode::Root { children, .. }
                    | AemNode::Panel { children, .. }
                    | AemNode::Repeatable { children, .. } => Some(children),
                    _ => None,
                };
                if let Some(children) = children {
                    for child in children {
                        assert_fields_bound(code, child, inside_fragment || is_fragment_panel);
                    }
                }
            }
            assert_fields_bound(code, &root, false);

            // (5) Element names are unique within each sequence, or the
            //     schema would not validate.
            let dupes = duplicate_sibling_names(&schema.to_xml());
            assert!(
                dupes.is_empty(),
                "{code}: duplicate sibling element names in the generated XSD: {dupes:?}"
            );
        }
    }
}

// ============================================================================
// AEM -> XSD naming and classification rules
// ============================================================================

/// Unit coverage for the rules that turn one AEM node into one XSD element,
/// driven off the real `profiles/ubs/xsd/types/` registry rather than a
/// hand-written table.
mod ubs_xsd_naming {
    use super::support;
    use u2s_aem_ubs_mcp::aem::{AemAttrs, AemNode, AemOption, OptionAlignment};
    use u2s_aem_ubs_mcp::xsd::{XsdConfig, XsdSchema};
    use uuid::Uuid;

    fn config() -> XsdConfig {
        let mut cfg = support::ubs_xsd_config();
        cfg.form_code = Some("TEST".to_string());
        cfg
    }

    fn page(title: &str, children: Vec<AemNode>) -> AemNode {
        AemNode::Panel {
            uuid: Uuid::nil(),
            name: "PN_Page".into(),
            title: title.into(),
            children,
            is_page: true,
            attrs: AemAttrs::default(),
            visible: true,
            is_conditional: false,
            dor_num_cols: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            frag_ref: None,
        }
    }

    fn layout(children: Vec<AemNode>) -> AemNode {
        AemNode::Panel {
            uuid: Uuid::new_v4(),
            name: "PN_Layout".into(),
            title: String::new(),
            children,
            is_page: false,
            attrs: AemAttrs::default(),
            visible: true,
            is_conditional: false,
            dor_num_cols: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            frag_ref: None,
        }
    }

    fn frag(name: &str, title: &str, frag_ref: &str) -> AemNode {
        AemNode::Fragment {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: Uuid::new_v4(),
            name: name.into(),
            title: title.into(),
            frag_ref: frag_ref.into(),
            bind_ref: None,
        }
    }

    fn textbox(name: &str, label: &str) -> AemNode {
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
            kind: u2s_aem_ubs_mcp::aem::TextFieldKind::Plain,
        }
    }

    fn radio(name: &str, label: &str, options: &[&str]) -> AemNode {
        AemNode::RadioButton {
            attrs: AemAttrs::default(),
            uuid: Uuid::new_v4(),
            name: name.into(),
            label: label.into(),
            options: options
                .iter()
                .enumerate()
                .map(|(i, l)| AemOption {
                    label: (*l).to_string(),
                    value: (i + 1).to_string(),
                })
                .collect(),
            alignment: OptionAlignment::Vertical,
            mandatory: false,
            visible: true,
            colspan: 12,
            dor_colspan: None,
            conditions: vec![],
            bind_ref: None,
        }
    }

    fn root(children: Vec<AemNode>) -> AemNode {
        AemNode::Root {
            title: "Test".into(),
            children,
        }
    }

    fn schema_for(children: Vec<AemNode>) -> XsdSchema {
        let fragments = support::ubs_fragments();
        u2s_aem_ubs_mcp::xsd::generate_xsd_from_aem(&root(children), &config(), &fragments).schema
    }

    fn xml_for(children: Vec<AemNode>) -> String {
        schema_for(children).to_xml()
    }

    const SIGNATURE: &str =
        "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1";
    const BANKING: &str =
        "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_BankingRelationship1";

    #[test]
    fn field_label_becomes_the_element_name() {
        let xml = xml_for(vec![textbox("TXT_x", "Email address")]);
        assert!(
            xml.contains(r#"<xs:element name="EmailAddress" type="xs:string" minOccurs="0"/>"#),
            "got:\n{xml}"
        );
    }

    #[test]
    fn field_type_comes_from_the_component_kind() {
        let xml = xml_for(vec![AemNode::DatePicker {
            attrs: AemAttrs::default(),
            uuid: Uuid::new_v4(),
            name: "DATE_x".into(),
            label: "Date of birth".into(),
            mandatory: false,
            visible: true,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
        }]);
        assert!(xml.contains(r#"type="xs:date""#), "got:\n{xml}");
    }

    /// A fragment whose type has a global element declaration is
    /// referenced, never re-declared.
    #[test]
    fn fragment_with_a_global_element_is_emitted_as_a_ref() {
        let xml = xml_for(vec![frag("PN_BR", "", BANKING)]);
        assert!(
            xml.contains(r#"<xs:element ref="BankingRelationship""#),
            "got:\n{xml}"
        );
        assert!(
            xml.contains(r#"schemaLocation="../AFFragments/BankingRelationship.xsd""#),
            "the include for the referenced type must be emitted. got:\n{xml}"
        );
    }

    /// One fragRef, two elements: only the panel title tells them apart.
    #[test]
    fn one_fragment_ref_yields_two_names_disambiguated_by_title() {
        let xml = xml_for(vec![
            frag("PN_A", "Client", SIGNATURE),
            frag("PN_B", "Authorized representative", SIGNATURE),
        ]);
        assert!(
            xml.contains(
                r#"<xs:element name="AccountHolderSignature" type="SignatureType" minOccurs="0"/>"#
            ),
            "got:\n{xml}"
        );
        assert!(
            xml.contains(r#"<xs:element name="AuthRepSignature" type="SignatureType""#),
            "got:\n{xml}"
        );
    }

    /// Option-set matching ignores order, case, numeric prefixes and markup.
    #[test]
    fn option_set_rule_selects_the_partner_class_ref() {
        for options in [
            vec!["Individual", "Company/Entity"],
            vec!["Company/Entity", "Individual"],
            vec!["individual", "COMPANY/ENTITY"],
            vec!["<p>Individual</p>", "<b>Company/Entity</b>"],
        ] {
            let xml = xml_for(vec![radio("RB_CPType", "", &options)]);
            assert!(
                xml.contains(r#"<xs:element ref="AccountHolderPartnerClass""#),
                "options {options:?} should match the partner-class rule. got:\n{xml}"
            );
        }
    }

    #[test]
    fn non_matching_option_set_falls_back_to_a_typed_element() {
        let xml = xml_for(vec![radio("RB_x", "Delivery", &["Post", "Email"])]);
        assert!(
            xml.contains(r#"<xs:element name="Delivery" type="xs:string""#),
            "got:\n{xml}"
        );
    }

    /// A layout panel contributes no level; a titled page panel does, so
    /// that two sections holding the same field cannot collide.
    #[test]
    fn layout_panels_are_transparent_but_titled_pages_group() {
        let xml = xml_for(vec![page(
            "Personal Data",
            vec![layout(vec![textbox("TXT_a", "Last name")])],
        )]);

        let paths = support::xsd_element_paths_in_order(&schema_for(vec![page(
            "Personal Data",
            vec![layout(vec![textbox("TXT_a", "Last name")])],
        )]));
        assert_eq!(
            paths,
            vec![
                "/UBSAF_TEST".to_string(),
                "/UBSAF_TEST/PersonalData".to_string(),
                "/UBSAF_TEST/PersonalData/LastName".to_string(),
            ],
            "got:\n{xml}"
        );
    }

    /// `groupPagePanels = false` follows UBS's rule exactly: even a titled
    /// page is transparent, and colliding names fall back to ordinal
    /// suffixes.
    #[test]
    fn group_page_panels_false_flattens_and_falls_back_to_suffixes() {
        let mut cfg = config();
        cfg.profile.group_page_panels = false;

        let tree = root(vec![
            page("Client", vec![textbox("a", "Last name")]),
            page("Authorized representative", vec![textbox("b", "Last name")]),
        ]);
        let flat = u2s_aem_ubs_mcp::xsd::generate_xsd_from_aem(&tree, &cfg, &[]);
        let mut paths = support::xsd_element_paths_in_order(&flat.schema);
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "/UBSAF_TEST".to_string(),
                "/UBSAF_TEST/LastName".to_string(),
                "/UBSAF_TEST/LastName2".to_string(),
            ],
            "with grouping off the sections vanish and the counter disambiguates"
        );

        // With grouping on, the same tree keeps both names meaningful.
        let grouped = u2s_aem_ubs_mcp::xsd::generate_xsd_from_aem(&tree, &config(), &[]);
        let mut grouped_paths = support::xsd_element_paths_in_order(&grouped.schema);
        grouped_paths.sort();
        assert_eq!(
            grouped_paths,
            vec![
                "/UBSAF_TEST".to_string(),
                "/UBSAF_TEST/AuthRep".to_string(),
                "/UBSAF_TEST/AuthRep/LastName".to_string(),
                "/UBSAF_TEST/Client".to_string(),
                "/UBSAF_TEST/Client/LastName".to_string(),
            ]
        );

        for result in [&flat, &grouped] {
            let mut bound: Vec<&str> = result.bind_refs.values().map(String::as_str).collect();
            let before = bound.len();
            bound.sort_unstable();
            bound.dedup();
            assert_eq!(before, bound.len(), "a path was bound twice");
        }
    }

    /// A titleless, non-page panel: pure layout, no XSD level and no
    /// binding.
    #[test]
    fn an_untitled_layout_panel_adds_no_level_and_no_binding() {
        let result = u2s_aem_ubs_mcp::xsd::generate_xsd_from_aem(
            &root(vec![layout(vec![textbox("TXT_a", "Street")])]),
            &config(),
            &[],
        );
        let paths = support::xsd_element_paths_in_order(&result.schema);
        assert_eq!(
            paths,
            vec!["/UBSAF_TEST".to_string(), "/UBSAF_TEST/Street".to_string()]
        );
        assert_eq!(
            result.bind_refs.len(),
            1,
            "only the field is bound, not the layout panel"
        );
    }

    /// Repeated sibling names would make the schema invalid and bind two
    /// nodes to one path, so the second occurrence is suffixed.
    #[test]
    fn repeated_sibling_names_are_disambiguated() {
        let result = u2s_aem_ubs_mcp::xsd::generate_xsd_from_aem(
            &root(vec![textbox("a", "Widget"), textbox("b", "Widget")]),
            &config(),
            &[],
        );
        let xml = result.schema.to_xml();
        assert!(xml.contains(r#"name="Widget""#), "got:\n{xml}");
        assert!(xml.contains(r#"name="Widget2""#), "got:\n{xml}");

        let mut paths: Vec<&str> = result.bind_refs.values().map(String::as_str).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["/UBSAF_TEST/Widget", "/UBSAF_TEST/Widget2"]);
    }

    /// The generated schema never contains the constructs the UBS format
    /// does not use -- they are not even representable any more.
    #[test]
    fn generated_schema_uses_only_the_ubs_vocabulary() {
        let xml = xml_for(vec![
            page("Section", vec![textbox("TXT_a", "Street")]),
            frag("PN_BR", "", BANKING),
        ]);
        for forbidden in [
            "<xs:choice",
            "<xs:simpleType",
            "<xs:restriction",
            "<xs:annotation",
            "<xs:import",
            "<xs:enumeration",
            "<xs:pattern",
        ] {
            assert!(
                !xml.contains(forbidden),
                "{forbidden} must never be emitted. got:\n{xml}"
            );
        }
    }

    /// The always-include entry is emitted first, ahead of anything the walk
    /// discovers, and regardless of whether a type from it is used.
    #[test]
    fn simple_elements_include_is_always_first() {
        let schema = schema_for(vec![frag("PN_BR", "", BANKING)]);
        assert_eq!(
            schema.includes.first().map(String::as_str),
            Some("../AFSimpleTypeElements/AFSimpleElements.xsd")
        );
    }

    /// `[elements]` normalises a label to a canonical English name and
    /// supplies the type, so the same concept gets the same element across
    /// language variants of a form.
    #[test]
    fn elements_table_normalises_a_foreign_label_and_its_type() {
        let cases = [
            ("Straße", "Street", "xs:string"),
            ("Cognome", "LastName", "xs:string"),
            ("PLZ", "PostalCode", "xs:string"),
            ("Data", "Date", "xs:date"),
        ];
        for (label, element, type_ref) in cases {
            let xml = xml_for(vec![textbox("TXT_x", label)]);
            assert!(
                xml.contains(&format!(
                    r#"<xs:element name="{element}" type="{type_ref}" minOccurs="0"/>"#
                )),
                "{label:?} should resolve to {element}/{type_ref}. got:\n{xml}"
            );
        }
    }

    /// Matching is on the whole label. The table holds two-letter synonyms,
    /// so substring matching turned sentence-length labels -- which these
    /// forms use freely -- into nonsense.
    #[test]
    fn elements_table_does_not_fire_on_sentence_labels() {
        for label in [
            "Nota: Barrare una sola casella salvo indicazione diversa.",
            "UNKNOWN",
            "– ha presentato un piano di liquidazione, di ristrutturazione in data ;",
        ] {
            let xml = xml_for(vec![textbox("TXT_x", label)]);
            for wrong in ["StreetNumber", r#"type="xs:date""#] {
                assert!(
                    !xml.contains(wrong),
                    "the table must not fire on {label:?} (matched {wrong}). got:\n{xml}"
                );
            }
        }
    }

    /// `[sections]` regex patterns name a section across languages before
    /// the title is used verbatim.
    #[test]
    fn sections_table_names_a_page_panel_across_languages() {
        for title in ["Signature of company", "Unterschrift der Firma"] {
            let xml = xml_for(vec![page(title, vec![textbox("TXT_a", "Place")])]);
            assert!(
                xml.contains(r#"<xs:element name="CompanySignature""#),
                "{title:?} should resolve to CompanySignature. got:\n{xml}"
            );
        }
    }

    /// The Individual / Company-Entity radio classifies whichever partner
    /// follows it, so the fragment after it picks the element. Without the
    /// lookahead every form got `AccountHolderPartnerClass`, which is right
    /// only for an account-holder form.
    #[test]
    fn the_partner_class_radio_is_resolved_by_the_following_fragment() {
        let cases = [
            (
                "affrg_ContractualPartnerGeneric1",
                "AccountHolderPartnerClass",
            ),
            ("affrg_BeneficialOwnerGeneric1", "BOPartnerClass"),
            ("affrg_PowerofAttorneyGeneric1", "POAPartnerClass"),
            ("affrg_PartnertoPartnerGeneric1", "PToPPartnerClass"),
            // Old model
            ("affrg_Attorney1", "POAPartnerClass"),
            ("affrg_Payee1", "PayeePartnerClass"),
            ("affrg_FIM2", "FIMPartnerClass"),
            // A longer key must not be shadowed by its prefix.
            ("affrg_AccountHolderDepot1", "AccountHolderPartnerClass"),
        ];
        for (fragment, expected) in cases {
            let xml = xml_for(vec![
                radio("RB_CPType", "", &["Individual", "Company/Entity"]),
                frag(
                    "PN_Partner",
                    "",
                    &format!("/content/dam/formsanddocuments/afforms_ch_fragmentlib/{fragment}"),
                ),
            ]);
            assert!(
                xml.contains(&format!(r#"<xs:element ref="{expected}""#)),
                "a radio followed by {fragment} should classify as {expected}. got:\n{xml}"
            );
        }
    }

    /// With nothing recognisable after it, the radio falls back to the
    /// rule's own element rather than dropping out of the schema.
    #[test]
    fn the_partner_class_radio_falls_back_without_a_following_fragment() {
        let xml = xml_for(vec![radio(
            "RB_CPType",
            "",
            &["Individual", "Company/Entity"],
        )]);
        assert!(
            xml.contains(r#"<xs:element ref="AccountHolderPartnerClass""#),
            "got:\n{xml}"
        );
    }

    /// Two schemas declare a global `AccountHolder`: `ContractualPartner.xsd`
    /// types it `AccountHolderType`, `ContractualPartnerGeneric.xsd` types it
    /// `ContractualPartnerGenericType`. Resolving the include by element
    /// name alone picks whichever the index happened to keep, leaving a
    /// `ref=` that points at an element of the wrong type -- so the
    /// fragment's own model root decides which file is included.
    #[test]
    fn the_include_for_a_ref_is_resolved_by_type_not_element_name() {
        let cases = [
            // old model: AccountHolderType, in the CH library
            (
                "afforms_ch_fragmentlib/affrg_Client1",
                "../AFFragments/ContractualPartner.xsd",
            ),
            // new model: ContractualPartnerGenericType, in the UBS library
            (
                "afforms_ubs_fragmentlib/affrg_ContractualPartnerGeneric1",
                "../AFFragments/ContractualPartnerGeneric.xsd",
            ),
        ];
        for (fragment, expected_include) in cases {
            let schema = schema_for(vec![frag(
                "PN_AH",
                "",
                &format!("/content/dam/formsanddocuments/{fragment}"),
            )]);
            let xml = schema.to_xml();
            assert!(
                xml.contains(r#"<xs:element ref="AccountHolder""#),
                "{fragment} should reference AccountHolder. got:\n{xml}"
            );
            assert!(
                schema.includes.iter().any(|i| i == expected_include),
                "{fragment} must include {expected_include}, got {:?}",
                schema.includes
            );
        }
    }
}
