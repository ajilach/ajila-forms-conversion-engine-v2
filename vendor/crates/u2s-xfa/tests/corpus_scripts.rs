//! The UBS corpus forms' own scripts run without failing: every form is
//! opened the way a session opens it (the load pass, then a paged refresh,
//! which re-runs master-page scripts per page), and every script failure
//! the engine records is counted against the form's baseline.
//!
//! The baseline is the number of failures each form still has. It may only
//! go down: a form that fails more than its baseline is a regression, and
//! one that fails less means the baseline should be lowered to match.

use std::collections::BTreeMap;

use u2s_xfa::xfa::script_executor::LayoutScripts;
use u2s_xfa::xfa::scripting::{ScriptReport, XfaForm};
use u2s_xfa::{XfaNode, fonts};

/// Script failures each form may still have; a form not listed must have
/// none.
const BASELINE: &[(&str, usize)] = &[
];

/// The nodes of a corpus form.
pub fn corpus_nodes(form: &str) -> Vec<XfaNode> {
    let bytes = std::fs::read(u2s_test_assets::corpus_form(form)).expect("read");
    let xfa = u2s_xfa::extract_xfa_from_pdf_bytes(&bytes)
        .expect("extract")
        .expect("xfa");
    XfaNode::parse(&xfa).expect("parse")
}

/// A corpus form opened as a session opens it, and everything its scripts
/// reported while it did.
pub fn opened(form: &str) -> (XfaForm, ScriptReport) {
    let (mut opened, _) = opened_with_layout(form);
    let report = opened.take_script_report();
    (opened, report)
}

/// As [`opened`], keeping the master-page scripts for further
/// `refresh_paged` calls after interactions.
pub fn opened_with_layout(form: &str) -> (XfaForm, Option<LayoutScripts>) {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
    let (mut opened, mut layout) = XfaForm::new_with_layout(corpus_nodes(form)).expect("form");
    if let Some(layout) = layout.as_mut() {
        opened.refresh_paged(layout).expect("refresh");
    }
    (opened, layout)
}

#[test]
fn every_corpus_form_stays_within_its_script_failure_baseline() {
    let baseline: BTreeMap<&str, usize> = BASELINE.iter().copied().collect();
    let mut forms: Vec<String> = std::fs::read_dir(u2s_test_assets::corpus_dir())
        .expect("corpus dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".pdf"))
        .collect();
    forms.sort();

    // Each form in its own thread: a script engine is per form, and the
    // corpus takes minutes in a debug build one form at a time.
    let reports: Vec<(String, Vec<String>, usize)> = std::thread::scope(|scope| {
        let handles: Vec<_> = forms
            .iter()
            .map(|form| {
                scope.spawn(move || {
                    let (_, report) = opened(form);
                    let errors: Vec<String> = report.errors.iter().map(|e| e.to_string()).collect();
                    (form.clone(), errors, report.dropped_errors)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("form thread")).collect()
    });

    let mut regressions = Vec::new();
    let mut improvements = Vec::new();
    for (form, errors, dropped) in &reports {
        let failures = errors.len() + dropped;
        let allowed = baseline.get(form.as_str()).copied().unwrap_or(0);
        for error in errors {
            println!("{form}: {error}");
        }
        if failures > allowed {
            regressions.push(format!("{form}: {failures} failures, baseline {allowed}"));
        } else if failures < allowed {
            improvements.push(format!("{form}: {failures} failures, baseline {allowed}"));
        }
    }
    assert!(regressions.is_empty(), "regressions: {regressions:?}");
    assert!(
        improvements.is_empty(),
        "fewer failures than the baseline; lower it: {improvements:?}"
    );
}

/// The value of the field at `path` in `form`.
fn value(form: &mut XfaForm, path: &str) -> Option<String> {
    form.current_field_values().get(path).cloned()
}

/// BBDO_019_DE copies the client's name into the signature block with
/// `Signature.Signature.FullName`, an unqualified name that must resolve by
/// the XFA 3.3 §3 scope walk to the body's `Signature`, not the master page
/// field of that name.
#[test]
fn bbdo_copies_the_clients_name_into_its_signature_block() {
    let (mut form, _) = opened("BBDO_019_DE.pdf");
    let name = "UBSForms_64468.Page.Client_Details.FamilyName_FirstName";
    form.interact(&format!("{name}.TF_FirstName"), "Hans").expect("first name");
    form.interact(&format!("{name}.TF_FamilyName"), "Muster").expect("family name");
    form.refresh().expect("refresh");
    assert_eq!(
        value(&mut form, "UBSForms_64468.Page.Signature.Signature.FullName").as_deref(),
        Some("Hans Muster")
    );
}

/// AAGS_019_DE numbers its signature rows with `parent.index + 1` (plus
/// the rows of the tables before), a bare `parent` that is the row's own:
/// its four tables open with one row each, numbered 1 to 4.
#[test]
fn aags_numbers_its_signature_rows() {
    let (mut form, _) = opened("AAGS_019_DE.pdf");
    for table in 1..=4 {
        let path = format!("UBSForms_66352.Page.SpecimenSignatures.Subtable_{table}.DYN_Row.ffIndex");
        assert_eq!(value(&mut form, &path), Some(table.to_string()), "{path}");
    }
}

/// AAGZ_019_DE hides its provision section on a blank form: its initialize
/// script asks the form root for `getDeltas()`, and a form nobody has
/// filled in has none.
#[test]
fn aagz_hides_its_provision_section_on_a_blank_form() {
    let (form, report) = opened("AAGZ_019_DE.pdf");
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    for page in 1..=3 {
        let path = format!("UBSForms_66359.Page_66359_{page}.Authorised_User.STP_Provision");
        let node = form.resolve(&path).unwrap_or_else(|| panic!("{path}"));
        assert!(!node.is_visible(), "{path} is hidden");
    }
}

/// ACAV_001_DE numbers its pages with a FormCalc calculate on the master
/// page, `$ = $.parent.index + 1` (the invisible `MyPresentPageCount`),
/// which the footer's `PageNumbering` reads, prefixed by the word its
/// language draw (`txtlanguage`, "D") selects: "Seite ".
#[test]
fn acav_numbers_its_pages() {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
    let flat = u2s_xfa::prepare_default(&corpus_nodes("ACAV_001_DE.pdf")).expect("prepare").flattened;
    let page_h: f64 = flat.page.height.to_string().parse().expect("height");
    let mut numbers: Vec<(usize, String)> = flat
        .iter_nodes()
        .filter(|n| n.som_path().is_some_and(|p| p.as_str().ends_with("PageNumbering")))
        .filter_map(|n| {
            let y: f64 = n.y.to_string().parse().ok()?;
            Some(((y / page_h).floor() as usize, n.shown_value()?))
        })
        .collect();
    numbers.sort();
    let count = flat.page.page_count as usize;
    let expected: Vec<(usize, String)> =
        (0..count).map(|p| (p, format!("Seite {}/{count}", p + 1))).collect();
    assert_eq!(numbers, expected);
}

/// AALU_019_DE: the account number typed into the header is copied by its
/// exit script into `Global_AccountNumber`, a global binding every table
/// row shares, including a row added afterwards; and choosing "Bereich"
/// rebuilds the "Objektart" list through `soLocalLabelDefinition.onChangeArea`,
/// unlocking it, or, for "Access", leaving it one preselected item, locked.
#[test]
fn aalu_carries_the_account_number_into_new_rows_and_builds_the_object_list() {
    const HEADER: &str = "UBSForms_66492.MPs.MP.STP_Header.BankingRelationship.BC_AccountNumber";
    const PRODUCT: &str = "UBSForms_66492.Page.Product.Product";
    let (mut form, layout) = opened_with_layout("AALU_019_DE.pdf");
    let mut layout = layout.expect("the scripts ran");
    let row_number = |i: usize| {
        let row = if i == 0 {
            "Table_Row_DYN".to_string()
        } else {
            format!("Table_Row_DYN[{i}]")
        };
        format!("{PRODUCT}.Table_Optional.{row}.Row.CustodyAccount.Global_AccountNumber")
    };

    form.interact(HEADER, "123456").expect("header");
    form.refresh_paged(&mut layout).expect("refresh");
    assert_eq!(
        form.current_field_values()
            .get(row_number(0).as_str())
            .map(String::as_str),
        Some("123456")
    );
    assert_eq!(
        form.current_field_values().get(HEADER).map(String::as_str),
        Some("123456"),
        "the typed header value survives the page pass"
    );

    form.click(&format!(
        "{PRODUCT}.Table_Optional.Table_Row_DYN.STP_PlusMinus.Button_Add"
    ))
    .expect("add");
    form.refresh_paged(&mut layout).expect("refresh");
    assert_eq!(
        form.current_field_values()
            .get(row_number(1).as_str())
            .map(String::as_str),
        Some("123456")
    );

    let object = format!("{PRODUCT}.Product.Product");
    assert_eq!(
        form.effective_access(&object).map(|a| a.access.as_str()),
        Some("protected")
    );
    form.interact(&format!("{PRODUCT}.Bereich.Bereich"), "Markt")
        .expect("area");
    form.refresh_paged(&mut layout).expect("refresh");
    let options = form.resolve(&object).expect("Objektart").dropdown_options();
    assert_eq!(options.len(), 5, "{options:?}");
    assert_eq!(
        options[1],
        ("601  UBS Depot".to_string(), "601".to_string())
    );
    assert_eq!(
        form.effective_access(&object).map(|a| a.access.as_str()),
        Some("open")
    );

    form.interact(&format!("{PRODUCT}.Bereich.Bereich"), "Access")
        .expect("area");
    form.refresh_paged(&mut layout).expect("refresh");
    let options = form.resolve(&object).expect("Objektart").dropdown_options();
    assert_eq!(
        options,
        vec![(
            "609  Depot mit Vermögensverwaltungsvertrag".to_string(),
            "609 RA".to_string()
        )]
    );
    assert_eq!(
        form.current_field_values()
            .get(object.as_str())
            .map(String::as_str),
        Some("609 RA")
    );
    assert_eq!(
        form.effective_access(&object).map(|a| a.access.as_str()),
        Some("protected")
    );
    let report = form.take_script_report();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

/// AALP_019_EN fills its branch dropdown in an initialize script that first
/// tries `xfa.form.UBSForms.UBSForms_<id>` inside `try`: reading a child
/// the form does not have throws, as in Acrobat, so the script falls back
/// to the root and lists the branches.
#[test]
fn aalp_lists_its_branches() {
    let (mut form, report) = opened("AALP_019_EN.pdf");
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let controls = u2s_xfa::states::controls_of_form(&mut form).expect("controls");
    let branch = controls
        .controls
        .iter()
        .find(|c| c.field == "UBSForms.Page.FormConfigurator_Branch.LocationShortCode.CL_ShortCode")
        .expect("the branch dropdown is listed");
    assert!(branch.options.len() > 1, "{:?}", branch.options);
}
