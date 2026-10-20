//! Reading why AEM failed to render a form out of its own error log
//! (`crx-quickstart/logs/error.log`), so a form page that answers 5xx is
//! reported with the exception that broke it rather than as a page that
//! "did not become ready".
//!
//! The log is Sling's: every entry starts with a timestamped header line,
//! `20.10.2026 11:49:04.175 *ERROR* [<thread>] <logger> <message>`, followed
//! by untimestamped continuation lines (a Java stack trace, `Caused by:`
//! chains). For a request thread the thread name is
//! `<client ip> [<start millis>] GET <path> HTTP/1.1`, which is what ties an
//! entry to one request of one form.
//!
//! Pure: [`render_root_cause`] takes the log text and returns the entry, the
//! caller reads the log (`crate::flow`, through the container's own `tail`).

/// The most lines [`render_root_cause`] returns for one entry. A rendering
/// failure's stack runs to hundreds of frames; the exception and the first
/// frames name the cause, and `Caused by:` lines further down are kept on
/// top of this.
pub const ROOT_CAUSE_MAX_LINES: usize = 25;

/// How many lines of the entry are kept before only `Caused by:` lines are.
const ROOT_CAUSE_HEAD_LINES: usize = 15;

/// One log entry: its header line and the continuation lines after it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry<'a> {
    header: &'a str,
    continuation: Vec<&'a str>,
}

/// Whether `line` starts a new entry: `dd.mm.yyyy hh:mm:ss.mmm *LEVEL*`.
fn is_header(line: &str) -> bool {
    let bytes = line.as_bytes();
    let shape = b"00.00.0000 00:00:00.000 *";
    bytes.len() > shape.len()
        && shape.iter().zip(bytes).all(|(want, got)| match want {
            b'0' => got.is_ascii_digit(),
            other => other == got,
        })
}

fn entries(log: &str) -> Vec<Entry<'_>> {
    let mut out: Vec<Entry<'_>> = Vec::new();
    for line in log.lines() {
        if is_header(line) {
            out.push(Entry {
                header: line,
                continuation: Vec::new(),
            });
        } else if let Some(last) = out.last_mut() {
            last.continuation.push(line);
        }
        // Continuation lines before the first header belong to an entry the
        // `tail` cut off; there is nothing to attach them to.
    }
    out
}

/// The entry's level (`ERROR`, `INFO`, ...) and its request tag
/// (`<ip> [<millis>] <request line>`), when its thread is a request thread.
fn level_and_thread(header: &str) -> Option<(&str, &str)> {
    // `<date> <time> *LEVEL* [<thread>] <logger> ...`
    let after_time = header.get(24..)?;
    let after_star = after_time.strip_prefix('*')?;
    let (level, rest) = after_star.split_once('*')?;
    let thread = rest.strip_prefix(" [")?;
    // The thread name holds one nested `[millis]`, so its end is the second
    // `]` (the first one closes the millis).
    let millis_end = thread.find(']')?;
    let end = millis_end + 1 + thread[millis_end + 1..].find(']')?;
    Some((level, &thread[..end]))
}

/// `<ip> [<millis>]`: what tells one request apart from another of the same
/// page.
fn request_id(thread: &str) -> Option<&str> {
    thread.find(']').map(|end| &thread[..=end])
}

/// Whether `thread` is a request for `form_path`'s own page:
/// `GET <form_path>.html` followed by the protocol or a query string.
fn requests_form(thread: &str, form_path: &str) -> bool {
    let target = format!("GET {form_path}.html");
    thread.match_indices(&target).any(|(at, _)| {
        matches!(
            thread[at + target.len()..].chars().next(),
            Some(' ') | Some('?') | None
        )
    })
}

/// Whether `line` is the first line of a Java exception: a qualified class
/// name ending in `Exception`/`Error`/`Throwable`, optionally followed by
/// `: message`, or a stack frame.
fn is_exception_line(line: &str) -> bool {
    if line.starts_with("\tat ") || line.starts_with("Caused by: ") {
        return true;
    }
    let class = line.split(':').next().unwrap_or_default();
    !class.is_empty()
        && !class.contains(char::is_whitespace)
        && class.contains('.')
        && ["Exception", "Error", "Throwable"]
            .iter()
            .any(|suffix| class.ends_with(suffix))
}

/// The entry, capped: the header and the first lines, then only the
/// `Caused by:` lines (each with the frame right after it), up to
/// [`ROOT_CAUSE_MAX_LINES`] in all, with how many lines were left out.
fn render(entry: &Entry<'_>) -> String {
    let mut kept: Vec<&str> = vec![entry.header];
    let head = ROOT_CAUSE_HEAD_LINES.saturating_sub(1).min(entry.continuation.len());
    kept.extend(&entry.continuation[..head]);
    let mut index = head;
    while index < entry.continuation.len() && kept.len() < ROOT_CAUSE_MAX_LINES {
        if entry.continuation[index].starts_with("Caused by: ") {
            kept.push(entry.continuation[index]);
            if let Some(frame) = entry.continuation.get(index + 1)
                && kept.len() < ROOT_CAUSE_MAX_LINES
            {
                kept.push(frame);
                index += 1;
            }
        }
        index += 1;
    }
    let total = 1 + entry.continuation.len();
    let mut out = kept.join("\n");
    if kept.len() < total {
        out.push_str(&format!("\n\t... {} more line(s) in the AEM error log", total - kept.len()));
    }
    out
}

/// Why the latest request for `form_path`'s page failed, from `log` (AEM's
/// `error.log`, or its tail): among the entries that request logged at
/// ERROR, the first one that carries a Java exception, since AEM logs benign
/// ERROR lines for working forms too (`FormsRelationServiceImpl`'s "Invalid
/// XSD path"), which carry none. Falls back to the request's first ERROR
/// entry when none carries an exception; `None` when the log holds no
/// request for the page, or none of its entries is an ERROR.
///
/// `form_path` is the form's JCR path without extension
/// (`/content/forms/af/.../AF_AAEV`). Pure. Unit-tested.
pub fn render_root_cause(log: &str, form_path: &str) -> Option<String> {
    let entries = entries(log);
    let tagged: Vec<(&str, &str, &Entry<'_>)> = entries
        .iter()
        .filter_map(|entry| {
            let (level, thread) = level_and_thread(entry.header)?;
            requests_form(thread, form_path).then_some((level, request_id(thread)?, entry))
        })
        .collect();
    let (_, latest, _) = tagged.last()?;
    let errors: Vec<&Entry<'_>> = tagged
        .iter()
        .filter(|(level, id, _)| id == latest && *level == "ERROR")
        .map(|(_, _, entry)| *entry)
        .collect();
    let chosen = errors
        .iter()
        .find(|entry| entry.continuation.first().is_some_and(|line| is_exception_line(line)))
        .or_else(|| errors.first())?;
    Some(render(chosen))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORM: &str = "/content/forms/af/afforms_germany_all/af_aa/AF_AAEV";

    /// An excerpt of a real AEM 6.5 error log (the `u2s-aem-ubs:2026-10-10`
    /// image), recorded while a form whose heading referenced footnote `fn3`
    /// without its description answered 500 twice: a working request before
    /// them, then both failing requests with their stacks shortened.
    fn fixture() -> &'static str {
        include_str!("../tests/fixtures/error-log-footnote-npe.log")
    }

    #[test]
    fn the_latest_requests_first_exception_is_the_root_cause() {
        let cause = render_root_cause(fixture(), FORM).expect("the failing request is found");
        let first = cause.lines().next().unwrap();
        assert!(first.contains("[1792497052593]"), "the latest request: {first}");
        assert!(first.contains("JsonObjectCreatorImpl Could not emit JSON"), "{first}");
        assert!(cause.contains("java.lang.NullPointerException"), "{cause}");
        assert!(cause.contains("createMapForFootnotes"), "{cause}");
        assert!(
            !cause.contains("FormsRelationServiceImpl"),
            "the benign ERROR without a stack is skipped: {cause}"
        );
        assert!(cause.lines().count() <= ROOT_CAUSE_MAX_LINES + 1, "{cause}");
        assert!(cause.ends_with("more line(s) in the AEM error log"), "{cause}");
    }

    #[test]
    fn another_forms_requests_are_not_read() {
        assert_eq!(render_root_cause(fixture(), "/content/forms/af/other/AF_AAEW"), None);
        assert_eq!(
            render_root_cause(fixture(), "/content/forms/af/afforms_germany_all/af_aa/AF_AAE"),
            None,
            "a path that is a prefix of the form's is another form"
        );
    }

    fn line(millis: u64, level: &str, path: &str, rest: &str) -> String {
        format!("20.10.2026 11:49:04.175 *{level}* [172.18.0.1 [{millis}] GET {path}.html HTTP/1.1] {rest}")
    }

    #[test]
    fn a_request_with_no_error_has_no_root_cause() {
        let log = [
            line(1, "ERROR", FORM, "x.Logger broke"),
            "java.lang.IllegalStateException: old".to_owned(),
            line(2, "INFO", FORM, "x.Logger rendering started"),
        ]
        .join("\n");
        assert_eq!(render_root_cause(&log, FORM), None, "only the latest request counts");
        assert_eq!(render_root_cause("", FORM), None);
    }

    #[test]
    fn without_any_exception_the_first_error_is_reported() {
        let log = [
            line(7, "ERROR", FORM, "a.Service Invalid XSD path"),
            line(7, "ERROR", FORM, "b.Service gave up"),
        ]
        .join("\n");
        let cause = render_root_cause(&log, FORM).unwrap();
        assert!(cause.ends_with("a.Service Invalid XSD path"), "{cause}");
    }

    #[test]
    fn caused_by_lines_past_the_head_are_kept() {
        let mut log = vec![
            line(9, "ERROR", FORM, "c.Tag Error while executing script"),
            "org.apache.sling.api.SlingException: wrapped".to_owned(),
        ];
        log.extend((0..40).map(|i| format!("\tat a.B.c(B.java:{i})")));
        log.push("Caused by: java.lang.NullPointerException: null".to_owned());
        log.push("\tat x.Y.createMapForFootnotes(Y.java:910)".to_owned());
        log.extend((0..40).map(|i| format!("\tat d.E.f(E.java:{i})")));
        let cause = render_root_cause(&log.join("\n"), FORM).unwrap();
        assert!(cause.contains("Caused by: java.lang.NullPointerException"), "{cause}");
        assert!(cause.contains("createMapForFootnotes"), "{cause}");
        assert!(!cause.contains("E.java"), "{cause}");
        assert!(cause.lines().count() <= ROOT_CAUSE_MAX_LINES + 1);
    }

    #[test]
    fn a_query_string_on_the_logged_request_still_matches() {
        let log = [
            format!("20.10.2026 11:49:04.175 *ERROR* [10.0.0.1 [5] GET {FORM}.html?wcmmode=disabled HTTP/1.1] a.B boom"),
            "java.lang.RuntimeException: boom".to_owned(),
        ]
        .join("\n");
        assert!(render_root_cause(&log, FORM).unwrap().contains("RuntimeException"));
    }

    #[test]
    fn exception_lines_are_recognised() {
        assert!(is_exception_line("java.lang.NullPointerException: null"));
        assert!(is_exception_line("org.apache.sling.api.SlingException"));
        assert!(is_exception_line("\tat a.B.c(B.java:1)"));
        assert!(is_exception_line("Caused by: x.YError: z"));
        assert!(!is_exception_line("Finding dependency for Guide : /content/dam/x"));
        assert!(!is_exception_line(""));
    }
}
