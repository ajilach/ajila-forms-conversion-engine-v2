//! The UBS-specific JS this crate's own `crate::driver::UbsDriver`
//! evaluates against the page -- kept as named constants in one place for
//! the same reason `u2s_aem_verify_core::flow::wizard_js` is: exactly one
//! spot to correct if a real UBS instance's rendered DOM or client-side
//! runtime disagrees with what these were developed against
//! (`ajila-forms-ubs`, branch `local-setup/redacto-summary`).
//!
//! **Why this exists at all.** UBS's own toolbar hides its `submit`
//! button on the summary panel entirely (`navigation.js`'s
//! `_configureToolbar("summaryPage")`, live for any form since
//! `window.forms.ubs.isFWB()` is hardcoded `true` on this branch -- the
//! Forms WorkBench host is normally what triggers submit instead). So
//! `u2s_aem_verify_core::flow`'s generic terminal-panel signal (a visible
//! submit button) never fires for a UBS form; this crate's own terminal
//! signal is instead "the summary panel is showing", and its own submit
//! call is UBS's routine, not a raw `guideBridge.submit()` -- the routine
//! that actually populates `summaryComponent` before submitting, which is
//! what makes `ajila-forms-ubs`'s own `DorRenderingExecutor` route the
//! submission through Redacto instead of AEM's native (and, on this
//! workspace's ARM Docker image, non-functional) XDP rendering path.

/// `true` iff there is a next-panel control to click.
///
/// The same `isFWB()`-is-hardcoded-`true` behaviour that hides `submit`
/// (see this module's own doc) hides the toolbar's *next* button too:
/// `_hideToolbarElement` puts `hidden fwbHidden` on its parent, so the
/// button renders `display: none` while staying in the DOM with its
/// handler bound. `u2s_aem_verify_core::flow::wizard_js::HAS_VISIBLE_NEXT`
/// therefore never fires for a UBS form and the walk stops on panel 1.
/// Visibility is simply not a usable signal here, so this does not check
/// it.
///
/// Confirmed live, and worth stating because it is counter-intuitive:
/// this only became true once the Adaptive Forms *fragment libraries* were
/// installed. Without them the form's own metadata fragment is
/// unresolvable, the toolbar-configuring path never runs, and the next
/// button is left visible -- so the walk appeared to work while the form
/// was rendering with every fragment reference resolving to nothing.
///
/// Returns `false` on the summary panel even though a hidden `.moveNext`
/// is still in the DOM there: `u2s_aem_verify_core::flow` only treats a
/// panel as terminal when there is no next control, so without this the
/// walk would keep clicking past the end instead of submitting.
pub const HAS_NEXT: &str = r#"(function() {
    var summary = document.querySelector('.summaryComponent');
    if (summary && summary.offsetParent !== null) { return false; }
    return !!document.querySelector('button.moveNext');
})()"#;

/// Clicks that control. A plain `.click()` reaches a `display: none`
/// button's bound handler perfectly well, which is the whole point: the
/// handler is UBS's own `window.forms.ubs.navigation.nextStep(container)`,
/// already bound with the toolbar button's own `container`. Calling
/// `nextStep` directly instead would mean reconstructing that argument
/// (it needs `container.panel` with a live `somExpression` and
/// `navigationContext`), and getting it wrong would skip the validation,
/// panel renumbering and `setSummaryData` bookkeeping that routine does on
/// the way.
///
/// The summary build is kicked off here too, and it has to be. AAOV_033
/// hangs two `fd:click` scripts on this button --
/// `summary.setSummaryData(guideRootPanel)` and `navigation.nextStep(this)`
/// -- but the panel advances on AEM's own built-in toolbar navigation and
/// neither custom script observably runs from a synthetic click. Removing
/// this call was tried, once DOMPurify was installed, on the theory that
/// the form's own script would cover it: the walk immediately stopped at
/// the summary panel with an empty summary and refused to submit. So both
/// were needed -- the library *and* this call.
///
/// It returns a promise and nothing awaits it (`navigation.submit` does not
/// either), which is why [`IS_SUMMARY_PANEL`] waits for the value to show
/// up instead of trusting that the panel is visible.
pub const CLICK_NEXT: &str = r#"(function() {
    var next = document.querySelector('button.moveNext');
    if (!next) { return false; }
    next.click();
    try {
        var summary = window.ajila && window.ajila.forms && window.ajila.forms.ubs
            && window.ajila.forms.ubs.control && window.ajila.forms.ubs.control.summary;
        if (summary && typeof summary.setSummaryData === 'function'
                && typeof guideBridge !== 'undefined') {
            var root = guideBridge.resolveNode('guide[0].guide1[0].guideRootPanel[0]');
            if (root) { summary.setSummaryData(root); }
        }
    } catch (e) { /* the click already happened; the poll reports the rest */ }
    return true;
})()"#;

/// Clicks the toolbar's "previous panel" control -- the same
/// `isFWB()`-hides-the-toolbar situation [`HAS_NEXT`]'s own doc describes
/// for "next": `button.movePrevious` renders `display: none` with its
/// handler still bound, so this does not gate on visibility either. Unlike
/// [`CLICK_NEXT`], going backward never needs to kick off
/// `setSummaryData` -- that only matters on the way *into* the summary
/// panel, never on the way back out of it.
pub const CLICK_PREV: &str = r#"(function() {
    var prev = document.querySelector('button.movePrevious');
    if (!prev) { return false; }
    prev.click();
    return true;
})()"#;

/// `true` iff UBS's own summary panel is the visible one.
///
/// Visibility is the whole test, deliberately. Requiring the summary
/// component to already hold a value was tried and is wrong: UBS writes it
/// during submit, not before -- `navigation.submit` resolves the component
/// and calls `summary.submit(summaryComponent)` itself -- so the value is
/// null right up until the thing this check gates on has happened. That
/// version never recognised the terminal panel and never submitted at all.
///
/// What the summary actually needs beforehand is `setSummaryData` having
/// been kicked off (see [`CLICK_NEXT`]) and its prerequisites being present
/// on the page, which [`last_panel_checks`] verifies and reports instead.
pub const IS_SUMMARY_PANEL: &str = r#"(function() {
    var summary = document.querySelector('.summaryComponent');
    return !!summary && summary.offsetParent !== null;
})()"#;

/// Calls UBS's own submit routine
/// (`window.forms.ubs.navigation.submit(submitErrorMessage)`), guarded by
/// a `typeof` check so a real instance whose clientlib failed to load (or
/// a future branch that renames the routine) produces a clear
/// `submit_failed` finding instead of a JS exception this crate cannot
/// distinguish from any other evaluation failure. `submitErrorMessage` is
/// the form's own `messagebox_SubmissionError` guide node
/// (`specs/AEM.md`'s scripting model: a field's own `name` is a global),
/// resolved via `guideBridge.resolveNode` rather than assumed to exist as
/// a bare global, since not every UBS form is guaranteed to author one
/// under that exact name.
pub const SUBMIT: &str = r#"(function() {
    if (typeof window.forms === 'undefined'
        || typeof window.forms.ubs === 'undefined'
        || typeof window.forms.ubs.navigation === 'undefined'
        || typeof window.forms.ubs.navigation.submit !== 'function') {
        return false;
    }
    // Record the server's answer and the page's own log lines, so the
    // verifier can tell "the server answered but the download never came"
    // from "still waiting" (see SUBMIT_RESULT).
    window.__u2sSubmit = { done: false, logs: [] };
    if (!window.__u2sConsoleHooked) {
        ['log', 'warn', 'error'].forEach(function (level) {
            var original = console[level];
            console[level] = function () {
                try {
                    var text = Array.prototype.map.call(arguments, function (a) {
                        if (typeof a === 'string') { return a; }
                        try { return JSON.stringify(a); } catch (e) { return String(a); }
                    }).join(' ');
                    if (window.__u2sSubmit && window.__u2sSubmit.logs.length < 50) {
                        window.__u2sSubmit.logs.push(level + ': ' + text.slice(0, 500));
                    }
                } catch (e) {}
                return original.apply(console, arguments);
            };
        });
        window.__u2sConsoleHooked = true;
    }
    if (!window.__u2sSubmitHooked && typeof guideBridge !== 'undefined') {
        var originalSubmit = guideBridge.submit;
        guideBridge.submit = function (options) {
            options = options || {};
            var success = options.success, error = options.error;
            options.success = function (result) {
                window.__u2sSubmit.done = true;
                window.__u2sSubmit.ok = true;
                window.__u2sSubmit.data = result && result.data;
                if (success) { return success.apply(this, arguments); }
            };
            options.error = function (result) {
                window.__u2sSubmit.done = true;
                window.__u2sSubmit.ok = false;
                window.__u2sSubmit.data = result && (result.data || result);
                if (error) { return error.apply(this, arguments); }
            };
            return originalSubmit.call(guideBridge, options);
        };
        window.__u2sSubmitHooked = true;
    }
    var errorBox = null;
    try { errorBox = guideBridge.resolveNode('submitErrorMessage'); } catch (e) {}
    window.forms.ubs.navigation.submit(errorBox);
    return true;
})()"#;

/// The answer [`SUBMIT`] recorded, as a JSON string:
/// `{"done", "ok", "data", "logs"}`. `data` is the guide's submit result;
/// on the `local-setup/redacto-summary` branch its `form` names the stored
/// DoR (`<uuid>/<file name>.pdf` under `/tmp/ubsdocs/`).
pub const SUBMIT_RESULT: &str = r#"(function() {
    try { return JSON.stringify(window.__u2sSubmit || { done: false }); }
    catch (e) { return JSON.stringify({ done: false, error: String(e) }); }
})()"#;

/// Evaluated once on the summary panel before [`SUBMIT`], returning a JSON
/// array of `{"kind", "message"}` findings for anything worth flagging
/// before attempting submit -- not fatal on its own, since the actual
/// submit attempt (or its absence) is the real signal. `mandator` is
/// interpolated (a plain digit string, never attacker-controlled JS) so
/// the check compares against exactly the entity this run opened the form
/// with.
pub fn last_panel_checks(mandator: &str) -> String {
    format!(
        r#"(function() {{
            var findings = [];
            try {{
                var metadata = window.forms.ubs.getFormMetadata();
                if (metadata && metadata.mandator !== '{mandator}') {{
                    findings.push({{
                        kind: 'ubs_mandator_mismatch',
                        message: 'window.forms.ubs.getFormMetadata().mandator is ' +
                            JSON.stringify(metadata.mandator) + ' but this run opened the form ' +
                            'with mandator {mandator}'
                    }});
                }}
            }} catch (e) {{
                findings.push({{kind: 'ubs_mandator_mismatch', message: 'could not read window.forms.ubs.getFormMetadata(): ' + e}});
            }}
            if (!document.querySelector('.summaryComponent')) {{
                findings.push({{
                    kind: 'ubs_summary_component_missing',
                    message: 'no .summaryComponent element on the summary panel; Redacto \
                        rendering depends on this field being populated before submit'
                }});
            }}
            // The summary is the entirety of what Redacto renders, so its
            // prerequisites are worth checking before submitting rather
            // than discovering an empty PDF afterwards. Its *value* is not
            // checkable here -- UBS writes that during submit -- so check
            // what has to be true for that write to produce anything.
            //
            // DOMPurify is the one that actually bit: `summary.js` calls
            // `DOMPurify.sanitize(..)` but its clientlib neither ships the
            // library nor declares a dependency on one that does, so on a
            // bare AEM instance `setSummaryData` threw on its first call and
            // every rendered PDF came back holding only a header. See
            // docker/aem/dompurify/README.md.
            try {{
                var control = window.ajila && window.ajila.forms && window.ajila.forms.ubs
                    && window.ajila.forms.ubs.control && window.ajila.forms.ubs.control.summary;
                var missing = [];
                if (!control) {{ missing.push('window.ajila.forms.ubs.control.summary'); }}
                else if (typeof control.setSummaryData !== 'function') {{
                    missing.push('control.summary.setSummaryData');
                }}
                if (typeof DOMPurify === 'undefined') {{ missing.push('DOMPurify'); }}
                if (typeof guideBridge === 'undefined'
                        || !guideBridge.resolveNode('summaryComponent')) {{
                    missing.push('guideBridge.resolveNode("summaryComponent")');
                }}
                if (missing.length > 0) {{
                    findings.push({{
                        kind: 'ubs_summary_prerequisites_missing',
                        message: 'the summary cannot be built, so Redacto would render an '
                            + 'empty document: missing ' + missing.join(', ')
                    }});
                }}
            }} catch (e) {{
                findings.push({{kind: 'ubs_summary_prerequisites_missing', message: 'could not check the summary prerequisites: ' + e}});
            }}
            return JSON.stringify(findings);
        }})()"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_submit_js_names_the_real_ubs_navigation_routine() {
        assert!(SUBMIT.contains("window.forms.ubs.navigation.submit"));
    }

    /// The point of this crate's own next-control handling: UBS renders
    /// the toolbar's next button `display: none` under FWB mode, so
    /// anything that gates on visibility stops the walk on panel 1.
    #[test]
    fn the_next_control_checks_do_not_gate_on_visibility_of_the_button() {
        for js in [HAS_NEXT, CLICK_NEXT] {
            assert!(
                js.contains("button.moveNext"),
                "must still select UBS's own toolbar next button: {js}"
            );
            assert!(
                !js.contains("offsetParent !== null) { return true"),
                "must not require the next button itself to be visible: {js}"
            );
        }
    }

    #[test]
    fn click_prev_does_not_gate_on_visibility_of_the_button_either() {
        assert!(CLICK_PREV.contains("button.movePrevious"));
        assert!(!CLICK_PREV.contains("offsetParent"));
    }

    /// `u2s_aem_verify_core::flow` only treats a panel as terminal when
    /// there is no next control, and a hidden `.moveNext` survives onto
    /// the summary panel -- so this has to rule itself out there.
    #[test]
    fn has_next_rules_itself_out_on_the_summary_panel() {
        assert!(
            HAS_NEXT.contains(".summaryComponent"),
            "has-next must return false once the summary panel shows: {HAS_NEXT}"
        );
    }

    #[test]
    fn last_panel_checks_interpolates_the_mandator_verbatim() {
        let js = last_panel_checks("033");
        assert!(
            !js.contains("{mandator}"),
            "the {{mandator}} placeholder must be substituted, not left literal: {js}"
        );
        assert!(js.contains("!== '033'"));
        assert!(js.contains("mandator 033"));
    }
}
