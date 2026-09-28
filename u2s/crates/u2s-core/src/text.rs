//! Character-offset windowing and text search, shared by every text-returning
//! tool: a window boundary must never split a UTF-8 sequence, a caller must
//! always be able to tell a partial read from a whole one, and every offset
//! this module hands out is a *character* offset, so a search hit composes
//! directly with a windowed read.
//!
//! Promoted here from `u2s-render-core` once a second, unrelated consumer
//! needed it (`u2s-jsondoc`'s windowed `get`) — the same "a second consumer
//! means promote" move already applied to `u2s-blob`
//! ([`crate::format`]'s sibling story). `u2s-render-core::text` re-exports
//! this unchanged so the renderers need no change.
//!
//! [`Grep`] arrived the same way: it was `u2s_xfa::query::search`, serving one
//! caller, when the two render servers needed the same thing over extracted
//! page text. It is not XFA-specific, so it moved rather than being copied.

use std::iter::Peekable;
use std::str::CharIndices;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

/// A windowed slice of text: what was returned, where it started, how much
/// text existed in total, and whether the window cut it short.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Windowed {
    pub text: String,
    pub offset: usize,
    pub total_chars: usize,
    pub truncated: bool,
}

/// Take `limit` characters of `text` starting at `offset`, never splitting a
/// UTF-8 sequence. An `offset` past the end yields an empty, non-truncated
/// window reporting the real `total_chars`.
pub fn window_chars(text: &str, offset: usize, limit: usize) -> Windowed {
    let chars: Vec<char> = text.chars().collect();
    let total_chars = chars.len();
    let start = offset.min(total_chars);
    let end = start.saturating_add(limit).min(total_chars);
    Windowed {
        text: chars[start..end].iter().collect(),
        offset: start,
        total_chars,
        truncated: end < total_chars,
    }
}

// ------------------------------------------------------------------- search

/// One match: where it starts and how long it is, both as **character**
/// offsets so they feed straight back into [`window_chars`], plus a bounded
/// window of surrounding text for context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepMatch {
    pub offset: usize,
    pub length: usize,
    pub context: String,
}

/// A compiled query: a literal substring or a regex.
///
/// Both modes compile to one [`Regex`], which is what keeps every offset an
/// index into the *original* text. The predecessor of this type searched a
/// `to_lowercase()` copy for the literal case and then indexed the original
/// with the offsets it found — wrong whenever case folding changes length, and
/// unusable with a character-windowed read either way.
///
/// A literal is matched case-insensitively, as the predecessor did. A regex is
/// matched case-sensitively; a caller who wants otherwise writes `(?i)`.
#[derive(Debug, Clone)]
pub struct Pattern(Regex);

impl Pattern {
    /// Compile `query`, treating it as a regex when `regex` is set and as a
    /// literal substring otherwise.
    ///
    /// Rejects any pattern that matches the empty string — `""`, `a*`, `(?:)`.
    /// Such a pattern matches at every position in the text, so it reports one
    /// match per character rather than the nothing a caller means by it. A
    /// hard failure naming the reason beats a result that looks like a hit
    /// count.
    ///
    /// A pattern that matches empty only in *places*, such as `\b`, compiles:
    /// it does not match `""` itself, and no compile-time check can see the
    /// difference. [`Grep::scan`] drops its zero-length matches instead, so
    /// such a query reports nothing rather than one hit per word boundary.
    pub fn parse(query: &str, regex: bool) -> Result<Self, String> {
        let (source, case_insensitive) = if regex {
            (query.to_string(), false)
        } else {
            (regex::escape(query), true)
        };

        let compiled = RegexBuilder::new(&source)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|e| format!("invalid regex: {e}"))?;

        if compiled.is_match("") {
            return Err(
                "a query that can match the empty string would match at every position: \
                 give a non-empty query, or narrow the pattern"
                    .to_string(),
            );
        }

        Ok(Pattern(compiled))
    }
}

/// Searches one text after another under a single match limit and a single
/// running total — XFA packets, or the pages of a document.
///
/// The limit is shared across every [`scan`](Grep::scan): once it is spent,
/// later texts are still counted into [`total_matches`](Grep::total_matches)
/// but contribute no further entries, so a caller can always tell how much it
/// did not see. That bookkeeping lived at each call site before this type did.
#[derive(Debug, Clone)]
pub struct Grep {
    pattern: Pattern,
    limit: usize,
    context_radius: usize,
    total: usize,
    kept: usize,
}

impl Grep {
    pub fn new(pattern: Pattern, limit: usize, context_radius: usize) -> Self {
        Grep {
            pattern,
            limit,
            context_radius,
            total: 0,
            kept: 0,
        }
    }

    /// Search one text. Every match found bumps [`total_matches`](Grep::total_matches);
    /// only those the limit still had room for are returned.
    pub fn scan(&mut self, text: &str) -> Vec<GrepMatch> {
        // Byte ranges first, so the borrow of `self.pattern` ends before the
        // counters are touched. There are at most as many as the text has
        // characters, and each is two words.
        let found: Vec<(usize, usize)> = self
            .pattern
            .0
            .find_iter(text)
            .map(|m| (m.start(), m.end()))
            .collect();
        if found.is_empty() {
            return Vec::new();
        }

        let chars: Vec<char> = text.chars().collect();
        let total_chars = chars.len();
        // `find_iter` yields matches in increasing byte order, so one forward
        // cursor converts every byte offset to a character offset in a single
        // pass over the text rather than a rescan per match.
        let mut cursor = CharCursor::new(text, total_chars);

        let mut kept = Vec::new();
        for (start_byte, end_byte) in found {
            // A zero-length match has no extent, so `offset` + `length` cannot
            // address it and a caller cannot read it back. `Pattern::parse`
            // refuses the patterns that match empty *everywhere*; this catches
            // the ones that do it only in places, such as `\b`.
            if start_byte == end_byte {
                continue;
            }
            self.total += 1;
            if self.kept >= self.limit {
                continue;
            }

            let start = cursor.char_offset_of(start_byte);
            let end = cursor.char_offset_of(end_byte);
            let context_start = start.saturating_sub(self.context_radius);
            let context_end = end.saturating_add(self.context_radius).min(total_chars);

            kept.push(GrepMatch {
                offset: start,
                length: end - start,
                // Slicing a `Vec<char>` cannot split a UTF-8 sequence, so the
                // context is safe by construction.
                context: chars[context_start..context_end].iter().collect(),
            });
            self.kept += 1;
        }
        kept
    }

    /// Every match found across every scan, which may exceed the number
    /// returned — see [`truncated`](Grep::truncated).
    pub fn total_matches(&self) -> usize {
        self.total
    }

    /// True when the limit dropped matches. Never silent: a truncated result
    /// must be distinguishable from a complete one.
    pub fn truncated(&self) -> bool {
        self.total > self.kept
    }

    /// True when the limit is spent, so a caller walking pages can stop
    /// instead of scanning text it cannot report.
    pub fn is_full(&self) -> bool {
        self.kept >= self.limit
    }
}

/// A one-way cursor from byte offsets to character offsets over a single text.
/// Only correct for queries in non-decreasing order, which is what
/// `Regex::find_iter` produces.
struct CharCursor<'t> {
    indices: Peekable<CharIndices<'t>>,
    at: usize,
    total_chars: usize,
}

impl<'t> CharCursor<'t> {
    fn new(text: &'t str, total_chars: usize) -> Self {
        CharCursor {
            indices: text.char_indices().peekable(),
            at: 0,
            total_chars,
        }
    }

    fn char_offset_of(&mut self, byte: usize) -> usize {
        while let Some(&(index, _)) = self.indices.peek() {
            if index >= byte {
                return self.at;
            }
            self.indices.next();
            self.at += 1;
        }
        self.total_chars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_without_splitting_utf8() {
        let text = "a\u{1F600}b\u{1F600}c"; // multibyte emoji either side
        let w = window_chars(text, 1, 1);
        assert_eq!(w.text, "\u{1F600}");
        assert_eq!(w.total_chars, 5);
        assert!(w.truncated);
    }

    #[test]
    fn offset_past_end_is_empty_not_truncated() {
        let w = window_chars("abc", 10, 5);
        assert_eq!(w.text, "");
        assert_eq!(w.offset, 3);
        assert_eq!(w.total_chars, 3);
        assert!(!w.truncated);
    }

    #[test]
    fn a_window_covering_everything_is_not_truncated() {
        let w = window_chars("hello", 0, 100);
        assert_eq!(w.text, "hello");
        assert!(!w.truncated);
    }

    // -------------------------------------------------------------- search

    /// The whole point of the type: an offset it reports is a character
    /// offset, so feeding it back to `window_chars` lands on the match. This
    /// is the assertion the predecessor could not pass — it reported byte
    /// offsets found in a lowercased copy.
    #[test]
    fn an_offset_and_length_feed_window_chars_back_onto_the_match() {
        // Multibyte either side, and a case difference, so byte offsets and
        // lowercased-copy offsets both disagree with the right answer.
        let text = "\u{1F600}\u{00E4}\u{00F6} ALPHA \u{1F600}beta";
        let mut grep = Grep::new(Pattern::parse("alpha", false).unwrap(), 10, 4);
        let found = grep.scan(text);

        assert_eq!(found.len(), 1);
        let m = &found[0];
        let w = window_chars(text, m.offset, m.length);
        assert_eq!(w.text, "ALPHA", "the offset must be a character offset");
    }

    #[test]
    fn a_literal_is_matched_case_insensitively() {
        let mut grep = Grep::new(Pattern::parse("Alpha", false).unwrap(), 10, 8);
        let found = grep.scan("alpha ALPHA aLpHa");
        assert_eq!(found.len(), 3);
        assert_eq!(grep.total_matches(), 3);
    }

    #[test]
    fn a_literal_with_regex_metacharacters_is_matched_literally() {
        let mut grep = Grep::new(Pattern::parse("a.c", false).unwrap(), 10, 8);
        let found = grep.scan("abc a.c axc");
        assert_eq!(found.len(), 1, "the dot must not match any character");
        let w = window_chars("abc a.c axc", found[0].offset, found[0].length);
        assert_eq!(w.text, "a.c");
    }

    #[test]
    fn search_finds_every_match_and_reports_offsets() {
        let text = "alpha beta alpha gamma alpha";
        let mut grep = Grep::new(Pattern::parse("alpha", false).unwrap(), 100, 80);
        let found = grep.scan(text);

        assert_eq!(grep.total_matches(), 3);
        assert_eq!(found.len(), 3);
        assert!(!grep.truncated());
        assert_eq!(found[0].offset, 0);
        assert_eq!(
            window_chars(text, found[1].offset, found[1].length).text,
            "alpha"
        );
    }

    #[test]
    fn search_on_a_single_line_document_still_finds_every_match() {
        // The upstream bug this exists to not repeat: line-based matching is
        // meaningless when the whole document is one line.
        let minified = format!(
            "<a>{}</a>",
            "x".repeat(10_000) + "NEEDLE" + &"y".repeat(10_000)
        );
        let mut grep = Grep::new(Pattern::parse("NEEDLE", false).unwrap(), 10, 80);
        let found = grep.scan(&minified);

        assert_eq!(grep.total_matches(), 1);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn search_truncation_is_reported_not_silent() {
        let text = "x ".repeat(50);
        let mut grep = Grep::new(Pattern::parse("x", false).unwrap(), 5, 80);
        let found = grep.scan(&text);

        assert_eq!(found.len(), 5);
        assert!(grep.total_matches() > 5);
        assert!(grep.truncated());
        assert!(grep.is_full());
    }

    /// The limit and the total are shared across scans, which is what lets a
    /// caller walking packets or pages report an honest total.
    #[test]
    fn the_limit_and_total_span_every_scan() {
        let mut grep = Grep::new(Pattern::parse("x", false).unwrap(), 3, 4);

        assert_eq!(grep.scan("x x").len(), 2);
        assert_eq!(grep.scan("x x").len(), 1, "the limit carries across scans");
        assert!(grep.is_full());
        assert_eq!(grep.scan("x x").len(), 0, "spent, but still counted");

        assert_eq!(grep.total_matches(), 6, "the total is of every match seen");
        assert!(grep.truncated());
    }

    #[test]
    fn an_invalid_regex_is_a_clean_error_not_a_silent_empty_result() {
        let err = Pattern::parse("(unclosed", true).unwrap_err();
        assert!(err.contains("invalid regex"), "got {err:?}");
    }

    #[test]
    fn regex_search_works() {
        let mut grep = Grep::new(Pattern::parse(r"field\d+", true).unwrap(), 100, 80);
        assert_eq!(grep.scan("field1 field2 field99").len(), 3);
    }

    /// An empty query used to mean "no matches"; compiled as a regex it means
    /// "match at every position". Neither is what a caller wants, so it is
    /// refused rather than answered.
    #[test]
    fn an_empty_query_is_refused() {
        let err = Pattern::parse("", false).unwrap_err();
        assert!(err.contains("empty string"), "got {err:?}");
    }

    #[test]
    fn a_pattern_that_matches_everywhere_is_refused() {
        for pattern in ["a*", "(?:)", "x?"] {
            let err = Pattern::parse(pattern, true)
                .expect_err("a pattern matching the empty string must be refused");
            assert!(err.contains("empty string"), "{pattern}: got {err:?}");
        }
    }

    /// `\b` does not match `""`, so it compiles; its matches are all
    /// zero-length, which `offset` + `length` cannot address. Reporting one
    /// hit per word boundary would be worse than reporting none.
    #[test]
    fn zero_length_matches_are_not_reported() {
        let mut grep = Grep::new(Pattern::parse(r"\b", true).unwrap(), 100, 4);
        assert!(grep.scan("alpha beta gamma").is_empty());
        assert_eq!(grep.total_matches(), 0, "and they are not counted either");
    }

    #[test]
    fn context_is_bounded_and_never_splits_utf8() {
        let text = format!(
            "{}\u{1F600}NEEDLE\u{1F600}{}",
            "a".repeat(500),
            "b".repeat(500)
        );
        let mut grep = Grep::new(Pattern::parse("NEEDLE", false).unwrap(), 10, 5);
        let found = grep.scan(&text);

        assert_eq!(found.len(), 1);
        // 6 characters of match plus 5 either side, emoji included whole.
        assert_eq!(found[0].context, "aaaa\u{1F600}NEEDLE\u{1F600}bbbb");
    }
}
