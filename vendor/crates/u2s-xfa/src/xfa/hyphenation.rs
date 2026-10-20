//! XFA-spec-compliant hyphenation support.
//!
//! Wraps the `hyphenation` crate (Knuth-Liang algorithm with TeX dictionaries)
//! and applies XFA filtering rules from the `<hyphenation>` element per XFA 3.3
//! §"Automatic Hyphenation" (spec page 65).

use hyphenation::{Hyphenator, Language, Load, Standard};
use std::sync::Mutex;

// ── Embedded dictionaries ────────────────────────────────────────────────────

/// The dictionaries loaded so far, by language. Interior mutability: a
/// cache of immutable, leaked dictionaries, filled on first use of each
/// language and never changed afterwards.
static DICTS: Mutex<Vec<(Language, &'static Standard)>> = Mutex::new(Vec::new());

/// The hyphenation dictionary for a locale tag (`de`, `de_CH`, `en-GB`),
/// `None` when the `hyphenation` crate embeds none for that language. The
/// country selects a variant where one exists (`de_CH`, `en_GB`, `nb`/`nn`
/// for Norwegian); otherwise the language's main dictionary is used.
pub fn dict_for_language(locale: &str) -> Option<&'static Standard> {
    let language = language_of(locale)?;
    let mut dicts = DICTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, dict)) = dicts.iter().find(|(l, _)| *l == language) {
        return Some(dict);
    }
    let dict: &'static Standard = Box::leak(Box::new(
        Standard::from_embedded(language).expect("embedded hyphenation dictionary is valid"),
    ));
    dicts.push((language, dict));
    Some(dict)
}

/// The `hyphenation` crate's language for a locale tag.
fn language_of(locale: &str) -> Option<Language> {
    let mut parts = locale.split(['_', '-']);
    let language = parts.next()?.to_ascii_lowercase();
    let country = parts.next().map(str::to_ascii_uppercase);
    Some(match (language.as_str(), country.as_deref()) {
        ("af", _) => Language::Afrikaans,
        ("sq", _) => Language::Albanian,
        ("hy", _) => Language::Armenian,
        ("as", _) => Language::Assamese,
        ("eu", _) => Language::Basque,
        ("be", _) => Language::Belarusian,
        ("bn", _) => Language::Bengali,
        ("bg", _) => Language::Bulgarian,
        ("ca", _) => Language::Catalan,
        ("zh", _) => Language::Chinese,
        ("cop", _) => Language::Coptic,
        ("hr", _) => Language::Croatian,
        ("cs", _) => Language::Czech,
        ("da", _) => Language::Danish,
        ("nl", _) => Language::Dutch,
        ("en", Some("GB" | "IE" | "AU" | "NZ" | "ZA" | "IN")) => Language::EnglishGB,
        ("en", _) => Language::EnglishUS,
        ("eo", _) => Language::Esperanto,
        ("et", _) => Language::Estonian,
        ("fi", _) => Language::Finnish,
        ("fr", _) => Language::French,
        ("fur", _) => Language::Friulan,
        ("gl", _) => Language::Galician,
        ("ka", _) => Language::Georgian,
        ("de", Some("CH" | "LI")) => Language::GermanSwiss,
        ("de", _) => Language::German1996,
        ("el", _) => Language::GreekMono,
        ("gu", _) => Language::Gujarati,
        ("hi", _) => Language::Hindi,
        ("hu", _) => Language::Hungarian,
        ("is", _) => Language::Icelandic,
        ("id", _) => Language::Indonesian,
        ("ia", _) => Language::Interlingua,
        ("ga", _) => Language::Irish,
        ("it", _) => Language::Italian,
        ("kn", _) => Language::Kannada,
        ("kmr" | "ku", _) => Language::Kurmanji,
        ("la", _) => Language::Latin,
        ("lv", _) => Language::Latvian,
        ("lt", _) => Language::Lithuanian,
        ("mk", _) => Language::Macedonian,
        ("ml", _) => Language::Malayalam,
        ("mr", _) => Language::Marathi,
        ("mn", _) => Language::Mongolian,
        ("nb" | "no", _) => Language::NorwegianBokmal,
        ("nn", _) => Language::NorwegianNynorsk,
        ("oc", _) => Language::Occitan,
        ("or", _) => Language::Oriya,
        ("pi", _) => Language::Pali,
        ("pa", _) => Language::Panjabi,
        ("pms", _) => Language::Piedmontese,
        ("pl", _) => Language::Polish,
        ("pt", _) => Language::Portuguese,
        ("ro", _) => Language::Romanian,
        ("rm", _) => Language::Romansh,
        ("ru", _) => Language::Russian,
        ("sa", _) => Language::Sanskrit,
        ("sr", _) => Language::SerbianCyrillic,
        ("sh", _) => Language::SerbocroatianLatin,
        ("cu", _) => Language::SlavonicChurch,
        ("sk", _) => Language::Slovak,
        ("sl", _) => Language::Slovenian,
        ("es", _) => Language::Spanish,
        ("sv", _) => Language::Swedish,
        ("ta", _) => Language::Tamil,
        ("te", _) => Language::Telugu,
        ("th", _) => Language::Thai,
        ("tr", _) => Language::Turkish,
        ("tk", _) => Language::Turkmen,
        ("uk", _) => Language::Ukrainian,
        ("hsb", _) => Language::Uppersorbian,
        ("cy", _) => Language::Welsh,
        _ => return None,
    })
}

// ── XFA Hyphenation element ──────────────────────────────────────────────────

/// XFA `<hyphenation>` element — controls auto-hyphenation per XFA 3.3 §17.
///
/// Attributes and defaults follow the spec verbatim:
/// - `hyphenate`: 0 (disabled) or 1 (enabled). Default: 0.
/// - `wordCharacterCount`: minimum grapheme clusters in word. Default: 7.
/// - `remainCharacterCount`: min clusters before break. Default: 3.
/// - `pushCharacterCount`: min clusters after break. Default: 3.
/// - `excludeAllCaps`: skip all-caps words. Default: 0 (don't exclude).
/// - `excludeInitialCap`: skip initial-cap words. Default: 0 (don't exclude).
#[derive(Debug, Clone, PartialEq)]
pub struct XfaHyphenation {
    pub hyphenate: bool,
    pub word_character_count: usize,
    pub remain_character_count: usize,
    pub push_character_count: usize,
    pub exclude_all_caps: bool,
    pub exclude_initial_cap: bool,
}

impl Default for XfaHyphenation {
    fn default() -> Self {
        Self {
            hyphenate: false,
            word_character_count: 7,
            remain_character_count: 3,
            push_character_count: 3,
            exclude_all_caps: false,
            exclude_initial_cap: false,
        }
    }
}

impl XfaHyphenation {
    /// Parse from XFA XML attributes on a `<hyphenation>` element.
    pub fn from_attributes(attrs: &std::collections::HashMap<String, String>) -> Self {
        Self {
            hyphenate: attrs.get("hyphenate").map(|v| v == "1").unwrap_or(false),
            word_character_count: attrs
                .get("wordCharacterCount")
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            remain_character_count: attrs
                .get("remainCharacterCount")
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            push_character_count: attrs
                .get("pushCharacterCount")
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            exclude_all_caps: attrs
                .get("excludeAllCaps")
                .map(|v| v == "1")
                .unwrap_or(false),
            exclude_initial_cap: attrs
                .get("excludeInitialCap")
                .map(|v| v == "1")
                .unwrap_or(false),
        }
    }

    /// Find valid hyphenation break points for a word, applying all XFA filters.
    ///
    /// Returns byte indices within `word` where a hyphen + line break may occur.
    /// Empty vec means the word should not be hyphenated.
    ///
    /// Per XFA spec:
    /// 1. If `hyphenate` is false, return empty.
    /// 2. If word contains a digit, it is ineligible.
    /// 3. If word has fewer grapheme clusters than `word_character_count`, skip.
    /// 4. If `exclude_all_caps` and word is all uppercase, skip.
    /// 5. If `exclude_initial_cap` and word starts with uppercase + has other case, skip.
    /// 6. Get break points from dictionary.
    /// 7. Filter by `remain_character_count` (chars before break, excluding hyphen).
    /// 8. Filter by `push_character_count` (chars after break, excluding hyphen).
    pub fn break_points(&self, word: &str, dict: &Standard) -> Vec<usize> {
        if !self.hyphenate {
            return vec![];
        }

        // Words containing digits are ineligible per spec
        if word.chars().any(|c| c.is_ascii_digit()) {
            return vec![];
        }

        let char_count = word.chars().count();

        // Minimum word length check (grapheme clusters ≈ chars for Latin scripts)
        if char_count < self.word_character_count {
            return vec![];
        }

        // Exclude all-caps words
        if self.exclude_all_caps && word.chars().all(|c| !c.is_alphabetic() || c.is_uppercase()) {
            return vec![];
        }

        // Exclude initial-cap words (starts uppercase, has at least one other case)
        if self.exclude_initial_cap {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                if first.is_uppercase() && chars.any(|c| c.is_lowercase()) {
                    return vec![];
                }
            }
        }

        // Get break points from the Knuth-Liang dictionary
        let hyphenated = dict.hyphenate(word);
        let breaks: &[usize] = &hyphenated.breaks;

        // Filter by remain/push character counts
        // `breaks` contains byte indices in the word
        breaks
            .iter()
            .copied()
            .filter(|&byte_idx| {
                // Count chars before the break
                let chars_before = word[..byte_idx].chars().count();
                // Count chars after the break
                let chars_after = word[byte_idx..].chars().count();

                chars_before >= self.remain_character_count
                    && chars_after >= self.push_character_count
            })
            .collect()
    }

    /// Emergency hyphenation: relaxes constraints to try harder to break a word.
    ///
    /// Per XFA spec: "the processor performs emergency hyphenation, discarding
    /// controls as necessary to accomplish hyphenation as best it can."
    pub fn emergency_break_points(&self, word: &str, dict: &Standard) -> Vec<usize> {
        if !self.hyphenate {
            return vec![];
        }

        // Words with digits are still ineligible even in emergency
        if word.chars().any(|c| c.is_ascii_digit()) {
            return vec![];
        }

        // Get all dictionary break points without filtering
        let hyphenated = dict.hyphenate(word);
        hyphenated.breaks.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionaries_follow_the_locale_tag() {
        assert!(dict_for_language("de").is_some());
        assert!(dict_for_language("de_CH").is_some());
        assert!(dict_for_language("fr-FR").is_some());
        assert!(dict_for_language("es").is_some());
        assert!(dict_for_language("xx").is_none());
        assert!(dict_for_language("").is_none());
        assert_eq!(language_of("en_GB"), Some(Language::EnglishGB));
        assert_eq!(language_of("de_CH"), Some(Language::GermanSwiss));
        assert_eq!(language_of("DE"), Some(Language::German1996));
    }

    fn de_dict() -> &'static Standard {
        dict_for_language("de").expect("German is embedded")
    }

    /// `dict_for_language("it")`: the Italian forms in the corpus (AAOV,
    /// AAJB, AAOT, ...) need it to wrap the same way Acrobat does -- without
    /// it a long Italian word runs uncut to the column edge, which drifts
    /// the wrap points of every line after it away from Acrobat's.
    #[test]
    fn italian_is_a_supported_hyphenation_language() {
        assert!(
            dict_for_language("it").is_some(),
            "\"it\" must resolve to the embedded Italian dictionary"
        );

        let hyph = XfaHyphenation {
            hyphenate: true,
            word_character_count: 6,
            remain_character_count: 2,
            push_character_count: 3,
            ..Default::default()
        };
        let dict = dict_for_language("it").unwrap();
        let breaks = hyph.break_points("collocamento", dict);
        assert!(
            !breaks.is_empty(),
            "\"collocamento\" (AAOV's own recesso paragraph) should have break points: {:?}",
            breaks
        );
    }

    #[test]
    fn test_german_compound_word_hyphenation() {
        let hyph = XfaHyphenation {
            hyphenate: true,
            word_character_count: 6,
            remain_character_count: 2,
            push_character_count: 3,
            ..Default::default()
        };
        let dict = de_dict();

        // Long German compound words should have break points
        let breaks = hyph.break_points("Unternehmensinvestitionen", dict);
        assert!(
            !breaks.is_empty(),
            "Unternehmensinvestitionen should have break points: {:?}",
            breaks
        );

        let breaks = hyph.break_points("Investitionsstrukturen", dict);
        assert!(
            !breaks.is_empty(),
            "Investitionsstrukturen should have break points: {:?}",
            breaks
        );

        let breaks = hyph.break_points("Investmentfonds", dict);
        assert!(
            !breaks.is_empty(),
            "Investmentfonds should have break points: {:?}",
            breaks
        );
    }

    #[test]
    fn test_short_word_excluded() {
        let hyph = XfaHyphenation {
            hyphenate: true,
            word_character_count: 7,
            ..Default::default()
        };
        let dict = de_dict();

        // "Fonds" is only 5 chars — below threshold of 7
        let breaks = hyph.break_points("Fonds", dict);
        assert!(breaks.is_empty(), "Short word should not be hyphenated");
    }

    #[test]
    fn test_word_with_digits_ineligible() {
        let hyph = XfaHyphenation {
            hyphenate: true,
            word_character_count: 6,
            remain_character_count: 2,
            push_character_count: 3,
            ..Default::default()
        };
        let dict = de_dict();

        let breaks = hyph.break_points("Modell2025", dict);
        assert!(
            breaks.is_empty(),
            "Word with digits should not be hyphenated"
        );
    }

    #[test]
    fn test_hyphenation_disabled() {
        let hyph = XfaHyphenation {
            hyphenate: false,
            ..Default::default()
        };
        let dict = de_dict();

        let breaks = hyph.break_points("Unternehmensinvestitionen", dict);
        assert!(
            breaks.is_empty(),
            "Disabled hyphenation should return no breaks"
        );
    }

    #[test]
    fn test_remain_push_filtering() {
        let hyph = XfaHyphenation {
            hyphenate: true,
            word_character_count: 6,
            remain_character_count: 2,
            push_character_count: 3,
            ..Default::default()
        };
        let dict = de_dict();

        let breaks = hyph.break_points("Unternehmensinvestitionen", dict);
        for &byte_idx in &breaks {
            let chars_before = "Unternehmensinvestitionen"[..byte_idx].chars().count();
            let chars_after = "Unternehmensinvestitionen"[byte_idx..].chars().count();
            assert!(
                chars_before >= 2,
                "Break at byte {} leaves only {} chars before (need 2)",
                byte_idx,
                chars_before,
            );
            assert!(
                chars_after >= 3,
                "Break at byte {} leaves only {} chars after (need 3)",
                byte_idx,
                chars_after,
            );
        }
    }

    #[test]
    fn test_exclude_all_caps() {
        let hyph = XfaHyphenation {
            hyphenate: true,
            word_character_count: 6,
            remain_character_count: 2,
            push_character_count: 3,
            exclude_all_caps: true,
            ..Default::default()
        };
        let dict = de_dict();

        let breaks = hyph.break_points("INVESTITIONEN", dict);
        assert!(
            breaks.is_empty(),
            "All-caps word should be excluded when excludeAllCaps=1"
        );
    }

    #[test]
    fn test_exclude_initial_cap() {
        let hyph = XfaHyphenation {
            hyphenate: true,
            word_character_count: 6,
            remain_character_count: 2,
            push_character_count: 3,
            exclude_initial_cap: true,
            ..Default::default()
        };
        let dict = de_dict();

        let breaks = hyph.break_points("Investitionen", dict);
        assert!(
            breaks.is_empty(),
            "Initial-cap word should be excluded when excludeInitialCap=1"
        );
    }

    #[test]
    fn test_emergency_hyphenation() {
        let hyph = XfaHyphenation {
            hyphenate: true,
            word_character_count: 100, // extreme threshold
            remain_character_count: 2,
            push_character_count: 3,
            ..Default::default()
        };
        let dict = de_dict();

        // Normal break_points should return nothing (word too short for threshold)
        let normal = hyph.break_points("Unternehmensinvestitionen", dict);
        assert!(normal.is_empty());

        // Emergency should still find breaks
        let emergency = hyph.emergency_break_points("Unternehmensinvestitionen", dict);
        assert!(!emergency.is_empty());
    }
}
