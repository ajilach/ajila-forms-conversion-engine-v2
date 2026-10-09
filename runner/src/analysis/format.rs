//! Small text helpers shared by the analysis files: durations, numbers,
//! excerpts and file-name slugs, all written for a human reader first.

/// `1h 02m 13s`, `4m 05s`, `12.3s`, `850ms`.
pub fn duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let secs = ms / 1_000;
    if secs < 60 {
        return format!("{:.1}s", ms as f64 / 1_000.0);
    }
    let (h, m, s) = (secs / 3_600, (secs % 3_600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else {
        format!("{m}m {s:02}s")
    }
}

/// Elapsed time since the run started, as a fixed-width clock: `+01:02:13`.
pub fn clock(ms: u64) -> String {
    let secs = ms / 1_000;
    format!("+{:02}:{:02}:{:02}", secs / 3_600, (secs % 3_600) / 60, secs % 60)
}

/// `1,234,567`.
pub fn number(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A share of a whole, `42%`; `-` when the whole is zero.
pub fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        "-".into()
    } else {
        format!("{:.0}%", part as f64 * 100.0 / whole as f64)
    }
}

/// `USD 1.23`, or `-` when nothing could price it.
pub fn usd(cost: Option<f64>) -> String {
    match cost {
        Some(c) => format!("USD {c:.2}"),
        None => "-".into(),
    }
}

/// At most `max` characters of `text`, with a marker saying how much was cut.
pub fn excerpt(text: &str, max: usize) -> String {
    let total = text.chars().count();
    if total <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}… [{} more characters]", number((total - max) as u64))
}

/// `text` on one line: newlines and runs of whitespace collapsed, for table
/// cells and single-line log entries.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A table cell or inline excerpt: one line, cut to `max`, with the pipes
/// that would break a Markdown table escaped and `<` defused, so the XFA rich
/// text and XML that tool results are full of shows as text instead of being
/// rendered as HTML.
pub fn cell(text: &str, max: usize) -> String {
    excerpt(&one_line(text), max)
        .replace('|', "\\|")
        .replace('<', "&lt;")
}

/// The first non-empty line of `text` — what an error is usually identified by.
pub fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// A form of an error line stable across occurrences: digits folded to `#`,
/// quoted strings to `"…"`, so "field 12 'IBAN' missing" and "field 7 'BIC'
/// missing" group as one recurring problem.
pub fn normalize_error(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_quote: Option<char> = None;
    let mut last_digit = false;
    for c in line.chars() {
        if let Some(q) = in_quote {
            if c == q {
                in_quote = None;
                out.push('…');
                out.push(q);
            }
            continue;
        }
        // An apostrophe inside a word ("can't", "field's") is not a quote.
        let opens_quote = match c {
            '"' | '`' => true,
            '\'' => !out.chars().last().is_some_and(char::is_alphanumeric),
            _ => false,
        };
        if opens_quote {
            in_quote = Some(c);
            out.push(c);
            last_digit = false;
            continue;
        }
        if c.is_ascii_digit() {
            if !last_digit {
                out.push('#');
            }
            last_digit = true;
            continue;
        }
        last_digit = false;
        out.push(c);
    }
    excerpt(out.trim(), 200)
}

/// A file-name-safe slug of `text`: ASCII letters, digits, `-` and `_`.
pub fn slug(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
        if out.len() >= max {
            break;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "run".into()
    } else {
        trimmed.to_string()
    }
}

/// A Markdown code fence long enough that `body` cannot close it early.
pub fn fence(body: &str, lang: &str) -> String {
    let mut ticks = 3;
    let mut run = 0;
    for c in body.chars() {
        if c == '`' {
            run += 1;
            ticks = ticks.max(run + 1);
        } else {
            run = 0;
        }
    }
    let marker = "`".repeat(ticks);
    format!("{marker}{lang}\n{}\n{marker}\n", body.trim_end_matches('\n'))
}

/// `text` as a Markdown block quote.
pub fn quote(text: &str) -> String {
    text.trim()
        .lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally_at_every_scale() {
        assert_eq!(duration(850), "850ms");
        assert_eq!(duration(12_340), "12.3s");
        assert_eq!(duration(245_000), "4m 05s");
        assert_eq!(duration(3_733_000), "1h 02m 13s");
        assert_eq!(clock(3_733_999), "+01:02:13");
    }

    #[test]
    fn numbers_get_thousands_separators() {
        assert_eq!(number(0), "0");
        assert_eq!(number(999), "999");
        assert_eq!(number(1_000), "1,000");
        assert_eq!(number(1_234_567), "1,234,567");
    }

    #[test]
    fn excerpts_say_how_much_was_cut() {
        assert_eq!(excerpt("short", 10), "short");
        assert_eq!(excerpt("abcdefghij", 4), "abcd… [6 more characters]");
    }

    #[test]
    fn errors_that_differ_only_in_numbers_and_names_group_together() {
        assert_eq!(
            normalize_error("field 12 'IBAN' is missing a bindRef"),
            normalize_error("field 7 'BIC' is missing a bindRef")
        );
        assert_ne!(normalize_error("field is missing"), normalize_error("field is hidden"));
    }

    /// An apostrophe in a word must not swallow the rest of the line as a
    /// quoted name, or unrelated errors would fold into one.
    #[test]
    fn apostrophes_in_words_are_not_quotes() {
        assert_eq!(
            normalize_error("can't find 'IBAN' in the tree"),
            normalize_error("can't find 'BIC' in the tree")
        );
        assert_ne!(
            normalize_error("doesn't exist: panel"),
            normalize_error("doesn't render: panel")
        );
    }

    #[test]
    fn slugs_are_file_name_safe() {
        assert_eq!(slug("AAOS_033_IT.pdf, AAOS_033_DE.pdf", 40), "AAOS_033_IT_pdf_AAOS_033_DE_pdf");
        assert_eq!(slug("äöü", 10), "run");
    }

    #[test]
    fn a_fence_outgrows_the_backticks_inside_it() {
        let fenced = fence("a ``` b", "");
        assert!(fenced.starts_with("````\n"), "{fenced}");
    }

    #[test]
    fn table_cells_stay_on_one_line_and_escape_pipes() {
        assert_eq!(cell("a |\n b", 50), "a \\| b");
        assert_eq!(cell("<p>x</p>", 50), "&lt;p>x&lt;/p>");
    }
}
