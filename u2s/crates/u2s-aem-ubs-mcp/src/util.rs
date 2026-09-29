//! Shared utility functions used across multiple modules.

/// The time every package this writer builds records as its creation and
/// modification, the same instant the templates stamp on every node. A fixed
/// one keeps `encode` a function of the document: the same document builds the
/// same bytes, which is what a content-addressed encoder promises.
pub const PACKAGE_TIMESTAMP: &str = "2025-01-01T00:00:00.000+00:00";

/// Escape HTML special characters in a string.
///
/// Replaces `&`, `<`, `>`, `"`, and `'` with their HTML entity equivalents.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
