//! Whether a rendered PDF shows anything at all. A rendering dependency
//! that could not style or fill a document (a stylesheet it does not ship,
//! a summary payload that arrived empty) answers with a *valid* PDF of
//! blank pages, which a magic-number check waves through. This is the one
//! judgement every verifier applies to the PDF it got back, so the words
//! "blank" mean the same thing in each report.

use lopdf::content::Content;
use lopdf::{Document, Object};

/// Whether `pdf` has no page that draws anything: no page at all, or no
/// page whose content stream shows text (`Tj`, `TJ`, `'`, `"` with a
/// non-empty string) or paints an XObject (`Do`: an image, or a form
/// XObject carrying its own content). Judged on the operators rather than
/// on extracted text or declared fonts: text extraction depends on the
/// fonts' encodings, and a page may declare fonts it never uses. Pure.
///
/// An error when the bytes are not a PDF this crate can read, or a page's
/// content stream cannot be decoded: the caller reports that, since an unreadable
/// render is a finding of its own, not a blank one.
pub fn blank_pdf(pdf: &[u8]) -> Result<bool, String> {
    let doc =
        Document::load_mem(pdf).map_err(|e| format!("the rendered PDF cannot be read: {e}"))?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Ok(true);
    }
    for (number, page) in &pages {
        let raw = doc.get_page_content(*page);
        let content = Content::decode(&raw).map_err(|e| {
            format!("the rendered PDF's page {number} content cannot be decoded: {e}")
        })?;
        if content
            .operations
            .iter()
            .any(|op| draws_something(&op.operator, &op.operands))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The number of pages `pdf` declares, for a report that names it.
pub fn page_count(pdf: &[u8]) -> Result<usize, String> {
    let doc =
        Document::load_mem(pdf).map_err(|e| format!("the rendered PDF cannot be read: {e}"))?;
    Ok(doc.get_pages().len())
}

/// Whether one content-stream operation puts marks on the page.
fn draws_something(operator: &str, operands: &[Object]) -> bool {
    match operator {
        "Do" => true,
        "Tj" | "'" | "\"" => operands.last().is_some_and(shows_text),
        "TJ" => operands.last().is_some_and(|array| match array {
            Object::Array(items) => items.iter().any(shows_text),
            _ => false,
        }),
        _ => false,
    }
}

fn shows_text(operand: &Object) -> bool {
    matches!(operand, Object::String(bytes, _) if !bytes.is_empty())
}

/// PDFs built to order for tests, here rather than in each crate's test
/// module so a verifier's own tests and the live e2e suites judge the same
/// shapes [`blank_pdf`] is specified against.
#[doc(hidden)]
pub mod fixtures {
    use lopdf::{Document, Object, Stream, dictionary};

    /// A one-page PDF whose page declares `fonts` and draws `content`.
    pub fn one_page_pdf(fonts: lopdf::Dictionary, content: &[u8]) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => dictionary! { "Font" => fonts },
            "Contents" => content_id,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("the PDF saves");
        bytes
    }

    /// A one-page PDF with Helvetica declared and nothing drawn.
    pub fn blank_page() -> Vec<u8> {
        one_page_pdf(helvetica(), b"")
    }

    /// A one-page PDF showing `text` in Helvetica.
    pub fn page_showing(text: &str) -> Vec<u8> {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        one_page_pdf(
            helvetica(),
            format!("BT /F1 12 Tf 72 700 Td ({escaped}) Tj ET").as_bytes(),
        )
    }

    pub fn helvetica() -> lopdf::Dictionary {
        let font =
            dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" };
        dictionary! { "F1" => font }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{blank_page, helvetica, one_page_pdf, page_showing};
    use super::*;

    /// A page with nothing in its content stream is blank whether or not
    /// it declares a font: a declared font draws nothing by itself.
    #[test]
    fn a_page_that_draws_nothing_is_blank() {
        assert_eq!(
            blank_pdf(&one_page_pdf(lopdf::Dictionary::new(), b"")),
            Ok(true)
        );
        assert_eq!(blank_pdf(&blank_page()), Ok(true));
        assert_eq!(
            blank_pdf(&page_showing("")),
            Ok(true),
            "an empty string shows no text"
        );
    }

    #[test]
    fn a_page_that_shows_text_or_an_xobject_is_not_blank() {
        assert_eq!(blank_pdf(&page_showing("U2S")), Ok(false));
        assert_eq!(
            blank_pdf(&one_page_pdf(
                helvetica(),
                b"BT /F1 12 Tf [(U) -20 (2S)] TJ ET"
            )),
            Ok(false)
        );
        assert_eq!(
            blank_pdf(&one_page_pdf(lopdf::Dictionary::new(), b"q /Im1 Do Q")),
            Ok(false)
        );
    }

    #[test]
    fn bytes_that_are_not_a_pdf_are_an_error_not_a_verdict() {
        assert!(blank_pdf(b"%PDF-1.7 not really").is_err());
    }

    #[test]
    fn page_count_counts_the_pages() {
        assert_eq!(page_count(&blank_page()), Ok(1));
    }

    /// The text a fixture shows must come back out, since the live tests
    /// assert on extracted text.
    #[test]
    fn a_fixture_s_text_is_extractable() {
        let doc = Document::load_mem(&page_showing("U2S (ok)")).unwrap();
        assert!(doc.extract_text(&[1]).unwrap().contains("U2S (ok)"));
    }
}
