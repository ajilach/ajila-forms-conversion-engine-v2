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

use super::fragment_parser::{FragmentSubPanel, ParsedFragment};
use super::{ComponentPath, FragmentInit};

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

/// The sub-panels a partner generic shows once its Initialize rule ran: those
/// it ships shown and does not hide, and those it ships hidden and shows.
pub fn shown_sub_panels<'a>(sub_panels: &'a [FragmentSubPanel], init: &FragmentInit) -> Vec<&'a str> {
    let listed = |list: &[ComponentPath], name: &str| list.iter().any(|p| p.as_str() == name);
    sub_panels
        .iter()
        .filter(|panel| {
            listed(&init.init_show, &panel.name) || (panel.ships_shown && !listed(&init.init_hide, &panel.name))
        })
        .map(|panel| panel.name.as_str())
        .collect()
}

/// One copy of a partner generic in a party row.
pub struct PartyPart<'a> {
    pub name: &'a str,
    pub frag_ref: &'a str,
    pub init: &'a FragmentInit,
}

/// What is wrong with a party row holding more than one partner generic: a
/// party the source splits around fields of its own (UBS-PTY-5), say its
/// name, then a field, then its address. Each part is a copy of the same
/// fragment, the one party's role, cut down by its Initialize rule to the
/// sub-panels it shows, and no two parts show the same sub-panel. The copies
/// bind to the one schema element, so AEM keeps their fields in one party.
pub fn split_problems(row: &str, parts: &[PartyPart<'_>], library: &[ParsedFragment]) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(first) = parts.first() else {
        return problems;
    };
    if let Some(other) = parts.iter().find(|p| p.frag_ref != first.frag_ref) {
        problems.push(format!(
            "`{row}` holds partner generics of two roles ({} and {}): a row is one party, and \
             the parts of a split party are copies of its one fragment",
            first.frag_ref, other.frag_ref
        ));
        return problems;
    }
    let Some(sub_panels) = sub_panels(library, first.frag_ref) else {
        problems.push(format!(
            "`{row}` references {}, which the fragment library does not hold",
            first.frag_ref
        ));
        return problems;
    };
    let mut owner: Vec<(&str, &str)> = Vec::new();
    for part in parts {
        for panel in shown_sub_panels(sub_panels, part.init) {
            match owner.iter().find(|(p, _)| *p == panel) {
                Some((_, by)) => problems.push(format!(
                    "`{row}`: `{by}` and `{}` both show `{panel}`: each part of a split party \
                     shows its own sub-panels, hiding the others with `init_hide`",
                    part.name
                )),
                None => owner.push((panel, part.name)),
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init(hide: &[&str], show: &[&str]) -> FragmentInit {
        let paths = |p: &[&str]| p.iter().map(|n| ComponentPath::try_from(n.to_string()).unwrap()).collect();
        FragmentInit { init_hide: paths(hide), init_show: paths(show), ..Default::default() }
    }

    fn generic() -> Vec<FragmentSubPanel> {
        [
            ("PN_EntityBasic", false),
            ("PN_FormAddress", false),
            ("PN_IndividualBasic", true),
            ("PN_Address", true),
            ("PN_DOBNationality", false),
            ("PN_DateIncorporation", false),
        ]
        .iter()
        .map(|(name, ships_shown)| FragmentSubPanel { name: name.to_string(), ships_shown: *ships_shown })
        .collect()
    }

    #[test]
    fn what_shows_is_what_ships_shown_and_is_not_hidden_and_what_is_shown() {
        let generic = generic();
        let shown = shown_sub_panels(&generic, &init(&["PN_Address"], &["PN_DOBNationality"]));
        assert_eq!(shown, ["PN_IndividualBasic", "PN_DOBNationality"]);
    }
}
