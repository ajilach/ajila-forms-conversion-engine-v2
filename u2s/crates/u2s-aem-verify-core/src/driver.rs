//! The seam between this crate's format-agnostic verification flow and a
//! specific Adaptive Forms platform's behaviour. [`FormDriver`] is a pure
//! value -- everything it hands back is a URL query, a JS expression
//! string, or a JSON blob to merge into a tool result -- never something
//! that itself touches the page or the network. `crate::flow` is the only
//! thing that evaluates a driver's JS against a real page; a driver can
//! therefore be unit-tested with no browser at all.
//!
//! [`GenericDriver`] is what `u2s-aem-verify-mcp` (the generic `aem`
//! format) uses: exactly the behaviour this crate had before it grew a
//! seam at all, moved here unchanged. A format-specific binary (UBS's
//! `u2s-aem-ubs-verify-mcp`, today) implements its own driver instead.

use serde_json::{Value, json};

use crate::package_check::PackageInspection;

/// Format-specific behaviour `crate::flow`'s wizard walk needs but must
/// not itself know about. Every method is synchronous and side-effect
/// free: it either inspects `package` (already read once, offline, by
/// `crate::package_check::inspect`) or hands back a JS expression for
/// `crate::flow` to evaluate against the page.
pub trait FormDriver: Send + Sync {
    /// Query parameters appended to `<form_jcr_path>.html?...` -- both the
    /// host-side readiness-wait URL and the browser-side navigation URL
    /// use the same query. `Err` fails the whole run closed (mapped to
    /// `ErrorKind::PackageInvalid` by the caller): a format that cannot
    /// determine how to open its own form at all must not fall back to
    /// opening it wrong.
    fn form_url_query(&self, package: &PackageInspection) -> Result<Vec<(String, String)>, String>;

    /// A JS expression evaluating to `true` iff the currently visible
    /// panel is the wizard's terminal panel -- the panel from which
    /// `submit_js` should be called. Combined with "no next control" by
    /// the caller; a driver only needs to recognise its own terminal
    /// state (a visible submit button; a summary panel with none at all;
    /// whatever the platform actually does).
    fn terminal_panel_js(&self) -> String;

    /// A JS expression evaluating to `true` iff a "next panel" control
    /// this driver is willing to click exists on the current panel.
    ///
    /// `crate::flow` treats a panel as terminal only when this is `false`
    /// *and* [`Self::terminal_panel_js`] is `true`, so a driver whose
    /// platform leaves a next control in the DOM on the last panel must
    /// return `false` there itself rather than relying on the caller to
    /// break the tie.
    fn has_next_js(&self) -> String;

    /// A JS expression that clicks that control, evaluating to `true` iff
    /// one was found and clicked.
    fn click_next_js(&self) -> String;

    /// The "previous panel" counterpart to [`Self::click_next_js`], used
    /// only by `crate::interactive::prev` (`verify_run` only ever walks
    /// forward, so it never needed one). Required rather than defaulted to
    /// [`crate::flow::wizard_js::CLICK_PREV`]: a driver whose platform
    /// hides its toolbar the way UBS's does (see
    /// `u2s-aem-ubs-verify-mcp::ubs_js`'s own module doc) must supply its
    /// own selector, the same way it already must for
    /// [`Self::has_next_js`]/[`Self::click_next_js`] -- a silent generic
    /// default here would look like it worked while clicking nothing.
    fn click_prev_js(&self) -> String;

    /// What the "neither a next control nor the terminal panel was
    /// recognised" finding calls the thing it was looking for -- purely
    /// for a human reading the finding message.
    fn terminal_signal_label(&self) -> &'static str;

    /// A JS expression evaluating to `true` iff the submit routine was
    /// actually invoked (not that it necessarily succeeded -- the caller
    /// still waits for a download/artefact afterward).
    fn submit_js(&self) -> String;

    /// The `submit_failed` finding's message when [`Self::submit_js`]
    /// evaluates to anything other than `true`.
    fn submit_failed_message(&self) -> &'static str;

    /// An optional JS expression, evaluated once on the terminal panel
    /// before [`Self::submit_js`], returning a JSON array of
    /// `{"kind": ..., "message": ...}` objects -- each becomes a
    /// `Finding::warning` before submit is attempted. `None` for a driver
    /// with nothing extra to check.
    fn last_panel_checks_js(&self, package: &PackageInspection) -> Option<String>;

    /// An optional JS expression evaluating to a JSON string that describes
    /// the server's answer to the submit [`Self::submit_js`] started:
    /// `{"done": bool, "ok": bool, "data": {...}, "logs": [...]}`, or
    /// `{"done": false}` while it is still outstanding. Lets the flow stop
    /// waiting for a download that is not coming and say why. `None` for a
    /// driver that cannot observe its own submit.
    fn submit_result_js(&self) -> Option<String> {
        None
    }

    /// Where the submitted artefact is stored in the repository, given the
    /// `data` of a successful [`Self::submit_result_js`] answer: a path
    /// (percent-encoded, starting with `/`) the flow reads over HTTP when the
    /// browser download did not complete. `None` when the platform stores
    /// nothing readable.
    fn stored_artefact_path(&self, _submit_data: &Value) -> Option<String> {
        None
    }

    /// AEM log files (as the Sling log tailer names them) and the substrings
    /// of their lines worth attaching to a submit's findings, so an agent
    /// reads the server's own account of what happened. Empty for none.
    fn submit_log_filters(&self) -> &'static [(&'static str, &'static [&'static str])] {
        &[]
    }

    /// Extra keys merged into `verify_package_check`'s structured result,
    /// under this driver's own namespace (e.g. `{"ubs": {...}}`). The
    /// generic driver contributes nothing.
    fn package_summary_extension(&self, package: &PackageInspection) -> Value;
}

/// The generic driver: no URL query beyond `wcmmode=disabled`, a visible
/// submit button is the terminal-panel signal, and submit is a plain
/// `guideBridge.submit()` call -- exactly this crate's behaviour before it
/// grew a `FormDriver` seam at all.
pub struct GenericDriver;

impl FormDriver for GenericDriver {
    fn form_url_query(
        &self,
        _package: &PackageInspection,
    ) -> Result<Vec<(String, String)>, String> {
        Ok(vec![("wcmmode".to_owned(), "disabled".to_owned())])
    }

    fn terminal_panel_js(&self) -> String {
        // The submit button is server-rendered `name="submit"`
        // (`specs/AEM.md` §6.11) but, like the next button, loses that
        // attribute once `guideRuntime.js` initialises the page -- see
        // `crate::flow::wizard_js::HAS_VISIBLE_NEXT`'s doc for why this
        // selects on the (server-rendered, and so far still present at
        // runtime) CSS class `submit` instead.
        crate::flow::wizard_js::HAS_VISIBLE_SUBMIT.to_owned()
    }

    fn has_next_js(&self) -> String {
        crate::flow::wizard_js::HAS_VISIBLE_NEXT.to_owned()
    }

    fn click_next_js(&self) -> String {
        crate::flow::wizard_js::CLICK_NEXT.to_owned()
    }

    fn click_prev_js(&self) -> String {
        crate::flow::wizard_js::CLICK_PREV.to_owned()
    }

    fn terminal_signal_label(&self) -> &'static str {
        "\"submit\""
    }

    fn submit_js(&self) -> String {
        "guideBridge.submit(); true".to_owned()
    }

    fn submit_failed_message(&self) -> &'static str {
        "guideBridge.submit() did not report success"
    }

    fn last_panel_checks_js(&self, _package: &PackageInspection) -> Option<String> {
        None
    }

    fn package_summary_extension(&self, _package: &PackageInspection) -> Value {
        json!({})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspection() -> PackageInspection {
        PackageInspection {
            summary: crate::package_check::PackageSummary {
                form_jcr_path: "/content/forms/af/ConformanceForm".to_owned(),
                form_name: "ConformanceForm".to_owned(),
                looks_like_it_has_dor: false,
                looks_like_a_wizard: false,
            },
            form_content_xml: Vec::new(),
            dam_content_xml: None,
        }
    }

    #[test]
    fn the_generic_driver_asks_for_exactly_wcmmode_disabled() {
        let query = GenericDriver.form_url_query(&inspection()).expect("ok");
        assert_eq!(query, vec![("wcmmode".to_owned(), "disabled".to_owned())]);
    }

    #[test]
    fn the_generic_driver_has_no_last_panel_checks_or_summary_extension() {
        assert!(GenericDriver.last_panel_checks_js(&inspection()).is_none());
        assert_eq!(
            GenericDriver.package_summary_extension(&inspection()),
            json!({})
        );
    }
}
