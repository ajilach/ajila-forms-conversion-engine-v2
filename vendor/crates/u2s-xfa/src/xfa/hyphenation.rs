//! XFA-spec-compliant hyphenation support.
//!
//! Wraps the `hyphenation` crate (Knuth-Liang algorithm with TeX dictionaries)
//! and applies XFA filtering rules from the `<hyphenation>` element per XFA 3.3
//! §"Automatic Hyphenation" (spec page 65).

use hyphenation::{Hyphenator, Language, Load, Standard};
use std::sync::OnceLock;

// ── Embedded dictionaries ────────────────────────────────────────────────────

static DE_DICT: OnceLock<Standard> = OnceLock::new();
static EN_DICT: OnceLock<Standard> = OnceLock::new();
static IT_DICT: OnceLock<Standard> = OnceLock::new();

/// Return a reference to the German hyphenation dictionary (lazy-loaded).
fn german_dict() -> &'static Standard {
    DE_DICT.get_or_init(|| {
        Standard::from_embedded(Language::German1996)
            .expect("embedded German hyphenation dictionary is valid")
    })
}

/// Return a reference to the English hyphenation dictionary (lazy-loaded).
fn english_dict() -> &'static Standard {
    EN_DICT.get_or_init(|| {
        Standard::from_embedded(Language::EnglishUS)
            .expect("embedded English hyphenation dictionary is valid")
    })
}

/// Return a reference to the Italian hyphenation dictionary (lazy-loaded).
fn italian_dict() -> &'static Standard {
    IT_DICT.get_or_init(|| {
        Standard::from_embedded(Language::Italian)
            .expect("embedded Italian hyphenation dictionary is valid")
    })
}

/// Return the dictionary for a language code, or `None` if unsupported.
pub fn dict_for_language(lang: &str) -> Option<&'static Standard> {
    match lang {
        "de" => Some(german_dict()),
        "en" => Some(english_dict()),
        "it" => Some(italian_dict()),
        _ => None,
    }
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

    fn de_dict() -> &'static Standard {
        german_dict()
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
