//! A fragment's Initialize rule: what it hides, shows and makes mandatory or
//! optional inside the fragment when the form starts.
//!
//! Two sources decide it. The profile does the same for every fragment of a
//! kind: the generic address fragment's unused fields hidden and its city and
//! country not mandatory, wherever it is used, on its own or as a partner
//! generic's `PN_Address`. The document says the rest per fragment
//! ([`FragmentInit`]): a partner generic's sub-panels, an address field the
//! source shows after all, a heading the fragment prints that the source does
//! not, an input the source requires or does not, and a signature block whose
//! rows share one place and date. Every path is checked against the fragment
//! library the profile embeds, never assumed, so a rule never names a
//! component AEM cannot find.
//!
//! AEM's Expression Editor hands back ONE SCRIPTMODEL per field and event, so
//! all of this is one rule, however many statements it carries.

use super::fragment_parser::{FragmentComponent, ParsedFragment};
use super::partner;
use super::{ComponentPath, FragmentInit};

const HIDE: &str = "window.forms.ubs.hideAFHideDor";
const SHOW: &str = "window.forms.ubs.showAFShowDor";
const SET_MANDATORY: &str = "window.com.ajila.forms.ubs.components.setMandatory";

/// The generic address fragment.
pub const ADDRESS_GENERIC: &str = "affrg_AddressGeneric1";
/// The fields of the generic address fragment the profile hides, by their path
/// inside it: the source's address is street, number, postal code, city and
/// country unless the form shows another field (`init_show`).
pub const ADDRESS_HIDDEN: &[&str] = &[
    "PN_AddressBlock.TXT_StreetNumber",
    "PN_AddressBlock.TXT_AdditionalDetails_AddressBlock",
    "PN_AddressBlock.TXT_PostalCodeCity",
    "PN_AddressBlock.TXT_State_AddressBlock",
    "PN_AddressBlock.TXT_District_AddressBlock_APAC",
];
/// The fields of an address fragment the profile makes optional (feedback
/// #102): the fragment requires them, the sources do not, unless the form
/// requires them (`init_required`).
pub const ADDRESS_OPTIONAL: &[&str] = &[
    "PN_AddressBlock.TXT_City_AddressBlock",
    "PN_AddressBlock.DD_Country_AddressBlock",
];
/// The country list the generic address fragment's own rule loaded, kept in
/// the one rule that replaces it.
const ADDRESS_COUNTRY_LIST: &str = "var formMetadata = window.forms.ubs.getFormMetadata();\nwindow.forms.ubs.referencedata.getCountryList(formMetadata.mandator, formMetadata.language, 'DOMICILE_INDIVIDUAL', DD_Country_AddressBlock);";

/// The signature generic, and its place and date, which a block whose rows
/// share them asks in the first row only.
pub const SIGNATURE_GENERIC: &str = "affrg_SignatureGeneric1";
const SIGNATURE_PLACE_DATE: &[&str] = &["PN_GenericSignature.TXT_Place_Generic", "PN_GenericSignature.DATE_Sign_Generic"];
/// A row after the first, seen from the fragment: it sits in its repeatable's
/// row panel, whose parent is the instance.
const SHARED_PLACE_DATE_TEST: &str = "this.parent.parent.instanceIndex > 0";

/// The last segment of a fragment reference: the fragment's directory name.
pub fn file_of(frag_ref: &str) -> &str {
    frag_ref.rsplit('/').next().unwrap_or(frag_ref)
}

fn fragment<'a>(library: &'a [ParsedFragment], frag_ref: &str) -> Option<&'a ParsedFragment> {
    library.iter().find(|fragment| fragment.frag_ref == frag_ref)
}

/// The component at `path` inside the fragment at `frag_ref`, looking into the
/// fragments it references; `None` when it has none there.
pub fn component<'a>(library: &'a [ParsedFragment], frag_ref: &str, path: &str) -> Option<&'a FragmentComponent> {
    let fragment = fragment(library, frag_ref)?;
    if let Some(found) = fragment.components.iter().find(|c| c.path == path) {
        return Some(found);
    }
    fragment.components.iter().find_map(|c| {
        let inner = c.frag_ref.as_deref()?;
        let rest = path.strip_prefix(c.path.as_str())?.strip_prefix('.')?;
        component(library, inner, rest)
    })
}

fn contains(list: &[ComponentPath], path: &str) -> bool {
    list.iter().any(|p| p.as_str() == path)
}

fn join(prefix: &str, path: &str) -> String {
    if prefix.is_empty() { path.to_owned() } else { format!("{prefix}.{path}") }
}

/// Where the profile's address handling applies inside the fragment: the
/// generic address fragment itself (`""`), and each sub-panel that references
/// it and is shown when the form starts (a partner generic's `PN_Address`).
fn address_prefixes(library: &[ParsedFragment], frag_ref: &str, init: &FragmentInit) -> Vec<String> {
    if file_of(frag_ref) == ADDRESS_GENERIC {
        return vec![String::new()];
    }
    let Some(fragment) = fragment(library, frag_ref) else {
        return Vec::new();
    };
    fragment
        .components
        .iter()
        .filter(|c| !c.path.contains('.') && c.frag_ref.as_deref().map(file_of) == Some(ADDRESS_GENERIC))
        .filter(|c| contains(&init.init_show, &c.path) || (c.ships_shown && !contains(&init.init_hide, &c.path)))
        .map(|c| c.path.clone())
        .collect()
}

/// The components the profile hides inside the fragment, by path, before the
/// form's own `init_show` takes any of them back.
fn default_hidden(library: &[ParsedFragment], frag_ref: &str, init: &FragmentInit) -> Vec<String> {
    address_prefixes(library, frag_ref, init)
        .iter()
        .flat_map(|prefix| ADDRESS_HIDDEN.iter().map(move |path| join(prefix, path)))
        .collect()
}

/// What is wrong with a fragment's [`FragmentInit`]: every path names a
/// component of the fragment, once per list; `init_show` takes only a
/// component hidden when the form starts (the fragment ships it hidden, or the
/// profile hides it); nothing is both hidden and shown, or both required and
/// optional; only an input is made mandatory or optional; and
/// `shared_place_date` is the signature generic's, in a repeatable
/// (`in_repeatable`).
pub fn problems(
    name: &str,
    frag_ref: &str,
    init: &FragmentInit,
    in_repeatable: bool,
    library: &[ParsedFragment],
) -> Vec<String> {
    let mut problems = Vec::new();
    if init.is_empty() {
        return problems;
    }
    if init.shared_place_date {
        if file_of(frag_ref) != SIGNATURE_GENERIC {
            problems.push(format!(
                "`{name}` shares place and date, which only the signature generic ({SIGNATURE_GENERIC}) takes"
            ));
        } else if !in_repeatable {
            problems.push(format!(
                "`{name}` shares place and date between its rows, so it is the child of a Repeatable"
            ));
        }
    }
    if fragment(library, frag_ref).is_none() {
        if !(init.init_hide.is_empty()
            && init.init_show.is_empty()
            && init.init_required.is_empty()
            && init.init_optional.is_empty())
        {
            problems.push(format!("`{name}` references {frag_ref}, which the fragment library does not hold"));
        }
        return problems;
    }
    let hidden = default_hidden(library, frag_ref, init);
    let lists: [(&str, &[ComponentPath], &str); 4] = [
        ("init_hide", &init.init_hide, "hides"),
        ("init_show", &init.init_show, "shows"),
        ("init_required", &init.init_required, "requires"),
        ("init_optional", &init.init_optional, "frees"),
    ];
    for (list, paths, verb) in lists {
        for (i, path) in paths.iter().enumerate() {
            if paths[..i].contains(path) {
                problems.push(format!("`{name}` lists `{}` twice in `{list}`", path.as_str()));
            }
            let Some(found) = component(library, frag_ref, path.as_str()) else {
                let have = partner::sub_panels(library, frag_ref)
                    .unwrap_or(&[])
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                problems.push(format!(
                    "`{name}` {verb} `{}`, which it does not have; its sub-panels are {have}, and a component \
                     inside one is named by its path (`PN_Address.PN_AddressBlock.TXT_City_AddressBlock`)",
                    path.as_str()
                ));
                continue;
            };
            match list {
                "init_show" if found.ships_shown && !hidden.iter().any(|h| h == path.as_str()) => {
                    problems.push(format!(
                        "`{name}` shows `{}`, which it already shows: `init_show` takes only what the fragment \
                         ships hidden or the profile hides",
                        path.as_str()
                    ))
                }
                "init_required" | "init_optional" if !found.is_input() => problems.push(format!(
                    "`{name}` {verb} `{}`, which is no input: only an input is mandatory or not",
                    path.as_str()
                )),
                _ => {}
            }
        }
    }
    for path in &init.init_show {
        if init.init_hide.contains(path) {
            problems.push(format!("`{name}` both hides and shows `{}`", path.as_str()));
        }
    }
    for path in &init.init_required {
        if init.init_optional.contains(path) {
            problems.push(format!("`{name}` both requires and frees `{}`", path.as_str()));
        }
    }
    problems
}

/// The statements of the fragment's Initialize rule, in this order: the
/// sub-panels it hides and shows, in the order the fragment holds them (the
/// feedback repository's fixer writes the same order, so a fixed form and a
/// written one agree); the address fields the profile hides; the other
/// components the form hides and shows; the address fragment's country list;
/// what is mandatory and what is not; and the shared place and date. `this.`
/// scopes each call to the fragment instance, so every row of a repeating
/// block runs it on its own components. Empty when there is nothing to do.
pub fn statements(library: &[ParsedFragment], frag_ref: &str, init: &FragmentInit) -> Vec<String> {
    let file = file_of(frag_ref);
    let standalone_address = file == ADDRESS_GENERIC;
    let mut out = Vec::new();

    // Sub-panels, in the fragment's order; one the library does not know last.
    let sub_panels = partner::sub_panels(library, frag_ref).unwrap_or(&[]);
    let rank = |path: &ComponentPath| {
        sub_panels.iter().position(|sub| sub.name == path.as_str()).unwrap_or(sub_panels.len())
    };
    let mut direct: Vec<(usize, usize, String)> = init
        .init_hide
        .iter()
        .map(|p| (p, HIDE))
        .chain(init.init_show.iter().map(|p| (p, SHOW)))
        .filter(|(p, _)| p.is_direct())
        .enumerate()
        .map(|(given, (path, call))| (rank(path), given, format!("{call}(this.{});", path.as_str())))
        .collect();
    direct.sort();
    out.extend(direct.into_iter().map(|(_, _, call)| call));

    // The address fields the profile hides and the form does not show. The
    // generic address fragment on its own keeps the calls its own rule made,
    // by the fields' names.
    let prefixes = address_prefixes(library, frag_ref, init);
    let hidden = default_hidden(library, frag_ref, init);
    for path in &hidden {
        if contains(&init.init_show, path) {
            continue;
        }
        if standalone_address {
            let leaf = path.rsplit('.').next().unwrap_or(path);
            out.push(format!("{HIDE}({leaf});"));
        } else {
            out.push(format!("{HIDE}(this.{path});"));
        }
    }

    // The form's other hides and shows, as authored. A hide the profile
    // already makes is not made twice, and showing a field the profile hides
    // is not hiding it, plus a show where the fragment ships it hidden.
    for (paths, call) in [(&init.init_hide, HIDE), (&init.init_show, SHOW)] {
        for path in paths.iter().filter(|p| !p.is_direct()) {
            if hidden.iter().any(|h| h == path.as_str()) {
                let ships_hidden = component(library, frag_ref, path.as_str()).is_some_and(|c| !c.ships_shown);
                if call == HIDE || !ships_hidden {
                    continue;
                }
            }
            out.push(format!("{call}(this.{});", path.as_str()));
        }
    }

    if standalone_address {
        out.push(ADDRESS_COUNTRY_LIST.to_owned());
    }

    // Mandatory or not: the address fields the profile frees unless the form
    // requires them, then the form's own.
    let address_like = !prefixes.is_empty()
        || (file.to_lowercase().contains("address")
            && !file.to_lowercase().contains("formofaddress")
            && !file.to_lowercase().contains("form_address"));
    let optional: Vec<String> = if prefixes.is_empty() && address_like {
        ADDRESS_OPTIONAL.iter().map(|p| (*p).to_owned()).collect()
    } else {
        prefixes
            .iter()
            .flat_map(|prefix| ADDRESS_OPTIONAL.iter().map(move |path| join(prefix, path)))
            .collect()
    };
    for path in &optional {
        let required = contains(&init.init_required, path);
        out.push(format!("{SET_MANDATORY}(this.{path}, {required});"));
    }
    for (paths, required) in [(&init.init_required, true), (&init.init_optional, false)] {
        for path in paths.iter().filter(|p| !optional.iter().any(|o| o == p.as_str())) {
            out.push(format!("{SET_MANDATORY}(this.{}, {required});", path.as_str()));
        }
    }

    // A signature block's rows share one place and date: the first row asks
    // them, the others hide them. The fragment sits in its repeatable's row
    // panel, whose parent is the instance.
    if init.shared_place_date && file == SIGNATURE_GENERIC {
        let hides = SIGNATURE_PLACE_DATE
            .iter()
            .map(|path| format!("  {HIDE}(this.{path});"))
            .collect::<Vec<_>>()
            .join("\n");
        out.push(format!("if ({SHARED_PLACE_DATE_TEST}) {{\n{hides}\n}}"));
    }
    out
}

/// What of a fragment's Initialize rule its document keeps: the sub-panels it
/// hides and shows, each `hideAFHideDor(this.X)` / `showAFShowDor(this.X)` in
/// the order written, and whether its rows share one place and date. A call on
/// anything but a direct sub-panel (`this.X.Y`, no `this.`) is the profile's
/// own or not one of these, and is left out: the writer makes the profile's
/// again.
pub fn parse_init_rule(content: &str) -> FragmentInit {
    use regex_lite::Regex;
    let call = Regex::new(r"(hideAFHideDor|showAFShowDor)\(this\.([A-Za-z_][A-Za-z0-9_]*)\)").unwrap();
    let mut init = FragmentInit::default();
    for c in call.captures_iter(content) {
        let Ok(path) = ComponentPath::try_from(c[2].to_string()) else {
            continue;
        };
        if &c[1] == "hideAFHideDor" { &mut init.init_hide } else { &mut init.init_show }.push(path);
    }
    init.shared_place_date = content.contains(SHARED_PLACE_DATE_TEST);
    init
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aem::fragment_parser::FragmentSubPanel;

    fn paths(paths: &[&str]) -> Vec<ComponentPath> {
        paths.iter().map(|p| ComponentPath::try_from(p.to_string()).unwrap()).collect()
    }

    fn component(path: &str, class: &str, ships_shown: bool, frag_ref: Option<&str>) -> FragmentComponent {
        FragmentComponent {
            path: path.into(),
            class: class.into(),
            ships_shown,
            frag_ref: frag_ref.map(str::to_owned),
        }
    }

    const LIB: &str = "/content/dam/formsanddocuments/afforms_ubs_fragmentlib";

    fn library() -> Vec<ParsedFragment> {
        let address = format!("{LIB}/{ADDRESS_GENERIC}");
        let mut address_components = vec![component("PN_AddressBlock", "guidePanel", true, None)];
        for field in [
            "TXT_Street_AddressBlock",
            "TXT_StreetNumber",
            "TXT_AdditionalDetails_AddressBlock",
            "TXT_PostalCode_AddressBlock",
            "TXT_City_AddressBlock",
            "TXT_PostalCodeCity",
            "TXT_State_AddressBlock",
            "TXT_District_AddressBlock_APAC",
        ] {
            address_components.push(component(&format!("PN_AddressBlock.{field}"), "guideTextBox", true, None));
        }
        address_components.push(component("PN_AddressBlock.DD_Country_AddressBlock", "guideDropDownList", true, None));
        let generic_subs = [
            ("PN_EntityBasic", false, None),
            ("PN_IndividualBasic", true, None),
            ("PN_Address", true, Some(address.as_str())),
            ("PN_DOBNationality", false, None),
        ];
        let fragment = |dir: &str, sub_panels: Vec<FragmentSubPanel>, components: Vec<FragmentComponent>| {
            ParsedFragment {
                dir_name: dir.into(),
                frag_ref: format!("{LIB}/{dir}"),
                name: format!("PN_{dir}"),
                xsd_type_name: "T".into(),
                bound_elements: Vec::new(),
                sub_panels,
                components,
            }
        };
        vec![
            fragment(
                ADDRESS_GENERIC,
                vec![FragmentSubPanel { name: "PN_AddressBlock".into(), ships_shown: true }],
                address_components,
            ),
            fragment(
                "affrg_ContractualPartnerGeneric1",
                generic_subs
                    .iter()
                    .map(|(n, s, _)| FragmentSubPanel { name: (*n).into(), ships_shown: *s })
                    .collect(),
                generic_subs
                    .iter()
                    .map(|(n, s, r)| component(n, "guidePanel", *s, *r))
                    .collect(),
            ),
            fragment(
                "affrg_Account1",
                vec![FragmentSubPanel { name: "PN_AccountNo".into(), ships_shown: true }],
                vec![
                    component("PN_AccountNo", "guidePanel", true, None),
                    component("PN_AccountNo.ST_AccountNo", "guideTextDraw", true, None),
                    component("PN_AccountNo.TXT_Account3", "guideTextBox", true, None),
                ],
            ),
        ]
    }

    fn partner() -> String {
        format!("{LIB}/affrg_ContractualPartnerGeneric1")
    }

    #[test]
    fn the_standalone_address_rule_is_the_profiles_when_nothing_is_authored() {
        let lib = library();
        let calls = statements(&lib, &format!("{LIB}/{ADDRESS_GENERIC}"), &FragmentInit::default()).join("\n");
        assert_eq!(
            calls,
            "window.forms.ubs.hideAFHideDor(TXT_StreetNumber);\nwindow.forms.ubs.hideAFHideDor(TXT_AdditionalDetails_AddressBlock);\nwindow.forms.ubs.hideAFHideDor(TXT_PostalCodeCity);\nwindow.forms.ubs.hideAFHideDor(TXT_State_AddressBlock);\nwindow.forms.ubs.hideAFHideDor(TXT_District_AddressBlock_APAC);\nvar formMetadata = window.forms.ubs.getFormMetadata();\nwindow.forms.ubs.referencedata.getCountryList(formMetadata.mandator, formMetadata.language, 'DOMICILE_INDIVIDUAL', DD_Country_AddressBlock);\nwindow.com.ajila.forms.ubs.components.setMandatory(this.PN_AddressBlock.TXT_City_AddressBlock, false);\nwindow.com.ajila.forms.ubs.components.setMandatory(this.PN_AddressBlock.DD_Country_AddressBlock, false);"
        );
    }

    #[test]
    fn an_address_shows_the_fields_its_source_shows_and_requires_what_it_requires() {
        let lib = library();
        let init = FragmentInit {
            init_show: paths(&["PN_AddressBlock.TXT_State_AddressBlock"]),
            init_required: paths(&["PN_AddressBlock.TXT_City_AddressBlock"]),
            ..Default::default()
        };
        let address = format!("{LIB}/{ADDRESS_GENERIC}");
        assert!(problems("A", &address, &init, false, &lib).is_empty());
        let calls = statements(&lib, &address, &init);
        assert!(!calls.iter().any(|c| c.contains("TXT_State_AddressBlock")), "{calls:?}");
        assert!(calls.iter().any(|c| c.ends_with("TXT_City_AddressBlock, true);")), "{calls:?}");
        assert!(calls.iter().any(|c| c.ends_with("DD_Country_AddressBlock, false);")), "{calls:?}");
    }

    #[test]
    fn a_partner_generic_with_its_address_hides_and_frees_the_address_fields() {
        let lib = library();
        let init = FragmentInit { init_hide: paths(&["PN_EntityBasic"]), ..Default::default() };
        let calls = statements(&lib, &partner(), &init);
        assert_eq!(calls[0], "window.forms.ubs.hideAFHideDor(this.PN_EntityBasic);");
        assert_eq!(calls[1], "window.forms.ubs.hideAFHideDor(this.PN_Address.PN_AddressBlock.TXT_StreetNumber);");
        assert_eq!(
            calls.last().unwrap(),
            "window.com.ajila.forms.ubs.components.setMandatory(this.PN_Address.PN_AddressBlock.DD_Country_AddressBlock, false);"
        );
        // Without its address the partner generic has no address fields to touch.
        let without = FragmentInit { init_hide: paths(&["PN_Address"]), ..Default::default() };
        assert_eq!(statements(&lib, &partner(), &without), ["window.forms.ubs.hideAFHideDor(this.PN_Address);"]);
    }

    #[test]
    fn a_fragment_heading_can_be_hidden_and_an_input_freed() {
        let lib = library();
        let account = format!("{LIB}/affrg_Account1");
        let init = FragmentInit {
            init_hide: paths(&["PN_AccountNo.ST_AccountNo"]),
            init_optional: paths(&["PN_AccountNo.TXT_Account3"]),
            ..Default::default()
        };
        assert!(problems("A", &account, &init, false, &lib).is_empty());
        assert_eq!(
            statements(&lib, &account, &init),
            [
                "window.forms.ubs.hideAFHideDor(this.PN_AccountNo.ST_AccountNo);",
                "window.com.ajila.forms.ubs.components.setMandatory(this.PN_AccountNo.TXT_Account3, false);",
            ]
        );
        let heading = FragmentInit { init_required: paths(&["PN_AccountNo.ST_AccountNo"]), ..Default::default() };
        assert!(problems("A", &account, &heading, false, &lib)[0].contains("no input"));
    }

    #[test]
    fn what_a_fragment_ships_and_the_profile_hides_decide_what_it_can_show() {
        let lib = library();
        let show = |p: &[&str]| FragmentInit { init_show: paths(p), ..Default::default() };
        assert!(problems("P", &partner(), &show(&["PN_DOBNationality"]), false, &lib).is_empty());
        assert!(problems("P", &partner(), &show(&["PN_Address.PN_AddressBlock.TXT_State_AddressBlock"]), false, &lib).is_empty());
        let shown = problems("P", &partner(), &show(&["PN_IndividualBasic"]), false, &lib);
        assert!(shown[0].contains("already shows"), "{shown:?}");
        let missing = problems("P", &partner(), &FragmentInit { init_hide: paths(&["PN_Nothing"]), ..Default::default() }, false, &lib);
        assert!(missing[0].contains("does not have"), "{missing:?}");
        let twice = problems("P", &partner(), &FragmentInit { init_hide: paths(&["PN_Address", "PN_Address"]), ..Default::default() }, false, &lib);
        assert!(twice[0].contains("twice"), "{twice:?}");
        let both = FragmentInit { init_hide: paths(&["PN_EntityBasic"]), init_show: paths(&["PN_EntityBasic"]), ..Default::default() };
        assert!(problems("P", &partner(), &both, false, &lib).iter().any(|p| p.contains("both hides and shows")));
    }

    #[test]
    fn a_shared_place_and_date_is_the_signature_generics_in_a_repeatable() {
        let lib = library();
        let shared = FragmentInit { shared_place_date: true, ..Default::default() };
        let signature = format!("{LIB}/{SIGNATURE_GENERIC}");
        assert!(problems("S", &signature, &shared, true, &lib).is_empty());
        assert!(problems("S", &signature, &shared, false, &lib)[0].contains("child of a Repeatable"));
        assert!(problems("A", &format!("{LIB}/affrg_Account1"), &shared, true, &lib)[0].contains("only the signature generic"));
        assert_eq!(
            statements(&lib, &signature, &shared),
            ["if (this.parent.parent.instanceIndex > 0) {\n  window.forms.ubs.hideAFHideDor(this.PN_GenericSignature.TXT_Place_Generic);\n  window.forms.ubs.hideAFHideDor(this.PN_GenericSignature.DATE_Sign_Generic);\n}"]
        );
    }

    #[test]
    fn parsing_the_sub_panel_calls_gives_back_what_was_written() {
        let lib = library();
        let init = FragmentInit {
            init_hide: paths(&["PN_EntityBasic", "PN_Address"]),
            init_show: paths(&["PN_DOBNationality"]),
            ..Default::default()
        };
        let content = statements(&lib, &partner(), &init).join("\n");
        assert_eq!(parse_init_rule(&content), init);
        let deep = parse_init_rule(
            "window.forms.ubs.hideAFHideDor(this.PN_Address.TXT_District);\nwindow.forms.ubs.hideAFHideDor(PN_CPGRP.PN_Address);",
        );
        assert_eq!(deep, FragmentInit::default());
    }
}
