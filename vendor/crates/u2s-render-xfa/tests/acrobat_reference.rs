//! Where Acrobat puts each form's content, from the screenshots in
//! `pdf-screenshots/` (taken of the PDF stored next to them, in its default
//! state). These are the only reference renderings in the project, so they
//! pin what a spec-conformant layout must reproduce: how many pages a form
//! has and which text opens and closes each page.
//!
//! A reference the engine does not meet yet is listed in `KNOWN_GAPS`; the
//! test fails both when a listed gap is still open and expected to be closed
//! and when an unlisted reference breaks, so the list only ever shrinks.

use std::path::PathBuf;

use u2s_render_xfa::states::StateSpec;
use u2s_render_xfa::{Limits, Renderer, Target, fonts};

struct Reference {
    /// The PDF, relative to `pdf-screenshots/`.
    pdf: &'static str,
    pages: u32,
    /// For each page, in order: text Acrobat shows near its top and text it
    /// shows near its bottom (body content, not the repeated master page).
    first_and_last: &'static [(&'static str, &'static str)],
}

const REFERENCES: &[Reference] = &[
    Reference {
        pdf: "aaep/AAEP_019_DE.pdf",
        pages: 1,
        first_and_last: &[("Kundendaten", "Unterschrift Compliance")],
    },
    Reference {
        pdf: "aakf/AAKF_019_DE.pdf",
        pages: 5,
        first_and_last: &[
            ("Europäische Standardinformationen", "Laufzeit des Kreditvertrags"),
            ("Teilzahlungen und gegebenenfalls", "Effektiver Jahreszins"),
            ("Leistungen", "Entschädigung"),
            ("Datenbankabfrage", "Widerrufsfolgen"),
            ("Recht, dass der Kreditgeber", "Außergerichtlicher Beschwerde"),
        ],
    },
    Reference {
        pdf: "aalc/AALC_019_DE.pdf",
        pages: 1,
        first_and_last: &[("Form configurator", "Freundliche Grüße")],
    },
    Reference {
        pdf: "aaot/AAOT_033_IT.pdf",
        pages: 3,
        first_and_last: &[
            ("Documento di Sintesi", "Ordine permanente di Bonifico"),
            ("Addebito diretto", "Operazioni di giroconto"),
            ("Per i conti correnti denominati", "Richiesta rilascio certificazione"),
        ],
    },
    Reference {
        pdf: "aaov/AAOV_033_IT.pdf",
        pages: 3,
        first_and_last: &[
            ("Dati del titolare del conto", "Dichiarazione"),
            ("Firma/e", "Nome del titolare del conto"),
            ("Informazioni sulla banca", "non può utilizzare i codici"),
        ],
    },
    Reference {
        pdf: "aajb/AAJB_033_IT.pdf",
        pages: 3,
        first_and_last: &[
            ("Form configurator", "50:50 di capitale"),
            ("B2)", ""),
            ("Letto, confermato e sottoscritto", "Membri del Supervisory Board"),
        ],
    },
];

/// References the engine does not meet yet, by PDF, with what is wrong.
const KNOWN_GAPS: &[(&str, &str)] = &[(
    "aaot/AAOT_033_IT.pdf",
    "pages 1 and 2 each end one table row later than in Acrobat",
)];

fn screenshots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../pdf-screenshots")
}

/// What the engine does differently from Acrobat on one reference, if
/// anything.
fn mismatches(renderer: &Renderer, reference: &Reference) -> Vec<String> {
    let path = screenshots_dir().join(reference.pdf);
    let target = Target::doc(&path, &StateSpec::default());
    let info = renderer.info(&target).expect("info");
    let mut out = Vec::new();
    if info.page_count != reference.pages {
        out.push(format!("{} pages, Acrobat has {}", info.page_count, reference.pages));
    }
    for (i, (first, last)) in reference.first_and_last.iter().enumerate() {
        let page = i as u32 + 1;
        let on_page = |text: &str| -> Option<u32> {
            (1..=info.page_count).find(|&p| {
                renderer
                    .page_text(&target, p, None, Some(usize::MAX))
                    .expect("page text")
                    .text
                    .contains(text)
            })
        };
        for text in [first, last].into_iter().filter(|t| !t.is_empty()) {
            match on_page(text) {
                Some(p) if p == page => {}
                Some(p) => out.push(format!("{text:?} is on page {p}, Acrobat has page {page}")),
                None => out.push(format!("{text:?} is on no page, Acrobat has page {page}")),
            }
        }
    }
    out
}

#[test]
fn layout_matches_the_acrobat_screenshots() {
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
    let renderer = Renderer::new(Limits::default());
    let mut failures = Vec::new();
    for reference in REFERENCES {
        let found = mismatches(&renderer, reference);
        let known = KNOWN_GAPS.iter().any(|(pdf, _)| *pdf == reference.pdf);
        match (found.is_empty(), known) {
            (true, false) | (false, true) => {}
            (true, true) => failures.push(format!(
                "{} now matches Acrobat; remove it from KNOWN_GAPS",
                reference.pdf
            )),
            (false, false) => failures.push(format!("{}:\n  {}", reference.pdf, found.join("\n  "))),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
