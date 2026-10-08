//! The UBS partner generics (`afforms_ubs_fragmentlib`): the fragments a party
//! is authored from, and the one Initialize rule that sets which of their
//! sub-panels a form shows.
//!
//! A partner generic ships some sub-panels shown and some hidden, and the host
//! form hides the shown ones it does not need (`init_hide`) and shows the
//! hidden ones it does (`init_show`). Which are which differs between the
//! generics and their `Basic` variants (a `Basic` has no address, and some
//! ship their company name shown), so both are read from the fragment library
//! the profile embeds ([`super::fragment_parser::FragmentSubPanel`]), never
//! assumed. Leaving a sub-panel hidden whose fields the source asks for loses
//! data: the form renders and prints, without them.

use super::ComponentName;
use super::fragment_parser::{FragmentSubPanel, ParsedFragment};

/// The fragment stems of the partner generics. Their `Basic` variants count
/// too.
pub const PARTNER_GENERICS: &[&str] = &[
    "affrg_ContractualPartnerGeneric",
    "affrg_PartnertoPartnerGeneric",
    "affrg_BeneficialOwnerGeneric",
    "affrg_PowerofAttorneyGeneric",
];

/// Is the fragment at `frag_ref` a partner generic?
pub fn is_partner_generic(frag_ref: &str) -> bool {
    let fragment = frag_ref.rsplit('/').next().unwrap_or(frag_ref);
    PARTNER_GENERICS.iter().any(|stem| fragment.starts_with(stem))
}

/// The sub-panels of the fragment at `frag_ref`, in the order it holds them,
/// as the fragment library records them; `None` when the library has no such
/// fragment.
pub fn sub_panels<'a>(library: &'a [ParsedFragment], frag_ref: &str) -> Option<&'a [FragmentSubPanel]> {
    library
        .iter()
        .find(|fragment| fragment.frag_ref == frag_ref)
        .map(|fragment| fragment.sub_panels.as_slice())
}

/// What is wrong with a partner generic's `init_hide` and `init_show`, given
/// its sub-panels: each name must be one of them, appear once, and not be
/// both hidden and shown; a shown one must be one the fragment ships hidden,
/// since showing a shown one does nothing. Hiding one the fragment already
/// hides is allowed: the reviewed forms state it.
pub fn init_problems(
    name: &str,
    sub_panels: &[FragmentSubPanel],
    init_hide: &[ComponentName],
    init_show: &[ComponentName],
) -> Vec<String> {
    let mut problems = Vec::new();
    let find = |sub: &ComponentName| sub_panels.iter().find(|p| p.name == sub.as_str());
    let have = sub_panels
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    for (list, subs) in [("init_hide", init_hide), ("init_show", init_show)] {
        for (i, sub) in subs.iter().enumerate() {
            if subs[..i].contains(sub) {
                problems.push(format!("`{name}` lists `{}` twice in `{list}`", sub.as_str()));
            }
            match find(sub) {
                None => problems.push(format!(
                    "`{name}` {} `{}`, which it does not have; its sub-panels are {have}",
                    if list == "init_hide" { "hides" } else { "shows" },
                    sub.as_str()
                )),
                Some(panel) if list == "init_show" && panel.ships_shown => problems.push(format!(
                    "`{name}` shows `{}`, which it already shows: `init_show` takes only the sub-panels it ships hidden",
                    sub.as_str()
                )),
                Some(_) => {}
            }
        }
    }
    for sub in init_show {
        if init_hide.contains(sub) {
            problems.push(format!("`{name}` both hides and shows `{}`", sub.as_str()));
        }
    }
    problems
}

/// The statements of a partner generic's Initialize rule: one `hideAFHideDor`
/// per hidden and one `showAFShowDor` per shown sub-panel, in the order the
/// fragment holds them (`sub_panels`; the feedback repository's fixer writes
/// the same order, so a fixed form and a written one agree). `this.` scopes
/// each call to the fragment instance, so every row of a repeating party runs
/// it on its own sub-panels.
pub fn init_calls(
    sub_panels: &[FragmentSubPanel],
    hide: &[ComponentName],
    show: &[ComponentName],
) -> Vec<String> {
    let rank = |name: &ComponentName| {
        sub_panels
            .iter()
            .position(|sub| sub.name == name.as_str())
            .unwrap_or(sub_panels.len())
    };
    let mut calls: Vec<(usize, usize, String)> = hide
        .iter()
        .map(|n| (n, "window.forms.ubs.hideAFHideDor"))
        .chain(show.iter().map(|n| (n, "window.forms.ubs.showAFShowDor")))
        .enumerate()
        .map(|(given, (name, call))| (rank(name), given, format!("{call}(this.{});", name.as_str())))
        .collect();
    calls.sort();
    calls.into_iter().map(|(_, _, call)| call).collect()
}

/// Which sub-panels of a partner generic its Initialize rule hides and shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubPanelVisibility {
    pub hide: Vec<ComponentName>,
    pub show: Vec<ComponentName>,
}

/// The sub-panels an Initialize rule's JavaScript hides and shows, each
/// `hideAFHideDor(this.X)` / `showAFShowDor(this.X)` in the order written.
/// A call on anything but a direct sub-panel (`this.X.Y`, no `this.`) is not
/// one of these and is left out.
pub fn parse_init_calls(content: &str) -> SubPanelVisibility {
    use regex_lite::Regex;
    let call =
        Regex::new(r"(hideAFHideDor|showAFShowDor)\(this\.([A-Za-z_][A-Za-z0-9_]*)\)").unwrap();
    let (mut hide, mut show) = (Vec::new(), Vec::new());
    for c in call.captures_iter(content) {
        let Ok(name) = ComponentName::try_from(c[2].to_string()) else {
            continue;
        };
        if &c[1] == "hideAFHideDor" { &mut hide } else { &mut show }.push(name);
    }
    SubPanelVisibility { hide, show }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<ComponentName> {
        names
            .iter()
            .map(|n| ComponentName::try_from(n.to_string()).unwrap())
            .collect()
    }

    fn panels(spec: &[(&str, bool)]) -> Vec<FragmentSubPanel> {
        spec.iter()
            .map(|(name, ships_shown)| FragmentSubPanel {
                name: name.to_string(),
                ships_shown: *ships_shown,
            })
            .collect()
    }

    fn generic() -> Vec<FragmentSubPanel> {
        panels(&[
            ("PN_EntityBasic", false),
            ("PN_FormAddress", false),
            ("PN_IndividualBasic", true),
            ("PN_Address", true),
            ("PN_DOBNationality", false),
            ("PN_DateIncorporation", false),
        ])
    }

    #[test]
    fn the_calls_follow_the_fragment_order_whatever_order_they_were_authored_in() {
        let calls = init_calls(
            &generic(),
            &names(&["PN_Address", "PN_EntityBasic", "PN_Extra"]),
            &names(&["PN_DOBNationality"]),
        );
        assert_eq!(
            calls,
            [
                "window.forms.ubs.hideAFHideDor(this.PN_EntityBasic);",
                "window.forms.ubs.hideAFHideDor(this.PN_Address);",
                "window.forms.ubs.showAFShowDor(this.PN_DOBNationality);",
                "window.forms.ubs.hideAFHideDor(this.PN_Extra);",
            ]
        );
    }

    #[test]
    fn parsing_the_calls_gives_back_what_was_written() {
        let (hide, show) = (names(&["PN_EntityBasic", "PN_Address"]), names(&["PN_DOBNationality"]));
        let content = init_calls(&generic(), &hide, &show).join("\n");
        assert_eq!(parse_init_calls(&content), SubPanelVisibility { hide, show });
    }

    #[test]
    fn a_call_on_a_field_inside_a_sub_panel_is_not_a_sub_panel() {
        let parsed = parse_init_calls(
            "window.forms.ubs.hideAFHideDor(this.PN_Address.TXT_District);\nwindow.forms.ubs.hideAFHideDor(PN_CPGRP.PN_Address);",
        );
        assert_eq!(parsed, SubPanelVisibility::default());
    }

    #[test]
    fn what_a_fragment_ships_decides_what_it_can_show() {
        // A `Basic` variant that ships its company name shown and has no address.
        let basic = panels(&[("PN_EntityBasic", true), ("PN_IndividualBasic", true), ("PN_DOBNationality", false)]);
        assert!(init_problems("P", &basic, &[], &names(&["PN_DOBNationality"])).is_empty());
        let shown = init_problems("P", &basic, &[], &names(&["PN_EntityBasic"]));
        assert!(shown[0].contains("already shows"), "{shown:?}");
        let missing = init_problems("P", &basic, &names(&["PN_Address"]), &[]);
        assert!(missing[0].contains("does not have"), "{missing:?}");
        let twice = init_problems("P", &generic(), &names(&["PN_Address", "PN_Address"]), &[]);
        assert!(twice[0].contains("twice"), "{twice:?}");
        let both = init_problems("P", &generic(), &names(&["PN_EntityBasic"]), &names(&["PN_EntityBasic"]));
        assert!(both.iter().any(|p| p.contains("both hides and shows")), "{both:?}");
    }
}
