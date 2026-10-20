//! The quotation marks a form prints, by its language and the country of its
//! issuing entity.
//!
//! The profile's own texts (the default translations of the toolbar and its
//! messages) write a quotation as `«…»`, a marker rather than a typographic
//! choice: German as Germany writes it is `„Vorschau“`, as Switzerland writes
//! it `«Vorschau»`, and English `“Preview”`. [`quoted`] prints the marker as
//! the form's language and entity country write a quotation, so one profile
//! text serves every entity.

/// The entity code (`formrange_entity`) of UBS Switzerland. Every other entity
/// writes its languages as their own country does.
const SWISS_ENTITY: &str = "001";

/// The opening and closing marks of a quotation in `lang` (the profile's
/// language code: `de`, `en`, `fr`, `it`, `sp`, or a synonym such as `de-ch`
/// and `es`) for a form of `entity`.
///
/// | Language | Outside Switzerland | Switzerland |
/// |---|---|---|
/// | German | „…“ | «…» |
/// | Italian | «…» | „…“ |
/// | French | « … » | «…» |
/// | Spanish | «…» | «…» |
/// | English, any other | “…” | “…” |
pub fn quote_marks(lang: &str, entity: Option<&str>) -> (&'static str, &'static str) {
    let swiss = entity.is_some_and(|e| e.trim() == SWISS_ENTITY);
    let language = lang.split(['-', '_']).next().unwrap_or(lang).to_ascii_lowercase();
    match (language.as_str(), swiss) {
        ("de", false) => ("„", "“"),
        ("de", true) => ("«", "»"),
        ("it", false) => ("«", "»"),
        ("it", true) => ("„", "“"),
        // French sets its guillemets off with a no-break space, except in
        // Switzerland.
        ("fr", false) => ("«\u{a0}", "\u{a0}»"),
        ("fr", true) => ("«", "»"),
        ("sp" | "es", _) => ("«", "»"),
        _ => ("“", "”"),
    }
}

/// `text` with each quotation marked `«…»` printed with the marks of `lang`
/// and `entity` ([`quote_marks`]).
pub fn quoted(text: &str, lang: &str, entity: Option<&str>) -> String {
    let (open, close) = quote_marks(lang, entity);
    text.replace('«', open).replace('»', close)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn german_quotes_follow_the_entity_country() {
        assert_eq!(quoted("Klicken Sie auf «Vorschau».", "de", Some("019")), "Klicken Sie auf „Vorschau“.");
        assert_eq!(quoted("Klicken Sie auf «Vorschau».", "de", Some("001")), "Klicken Sie auf «Vorschau».");
        assert_eq!(quoted("«Vorschau»", "de-ch", Some("019")), "„Vorschau“");
        assert_eq!(quoted("«Vorschau»", "de", None), "„Vorschau“");
    }

    #[test]
    fn every_language_has_its_marks() {
        assert_eq!(quoted("«Preview»", "en", Some("019")), "“Preview”");
        assert_eq!(quoted("«Anteprima»", "it", Some("033")), "«Anteprima»");
        assert_eq!(quoted("«Anteprima»", "it", Some("001")), "„Anteprima“");
        assert_eq!(quoted("«Aperçu»", "fr", None), "«\u{a0}Aperçu\u{a0}»");
        assert_eq!(quoted("«Aperçu»", "fr", Some("001")), "«Aperçu»");
        assert_eq!(quoted("«Vista previa»", "sp", Some("019")), "«Vista previa»");
        assert_eq!(quoted("«Vista previa»", "es", Some("001")), "«Vista previa»");
    }

    #[test]
    fn a_text_without_a_quotation_is_unchanged() {
        assert_eq!(quoted("Zurück", "de", Some("019")), "Zurück");
    }
}
