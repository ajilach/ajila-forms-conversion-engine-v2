//! The verification flow: reuse (or boot, on first use) the profile's
//! persistent AEM + Chromium session (`crate::instance`), install the
//! package under test, screenshot the rendered form, optionally fill and
//! submit it, capture whatever the submit produced, and uninstall the
//! package again -- leaving the session itself running for the next call.
//!
//! **Multi-step wizards.** A wizard-shaped Adaptive Form (rendered panels
//! advanced one at a time via a "next" button, `specs/aem/aem-xml-spec.md` §6.11's
//! `fd/af/components/nextitemnav`) is walked by [`walk_wizard_and_maybe_submit`]:
//! fill what's fillable on the current panel, screenshot it, look for a
//! visible "next" control, click it and confirm the panel actually
//! changed (`specs/aem/aem-xml-spec.md` §15.1 documents a panel's own "Step Completion"
//! validation can block navigation even after a successful click) or stop
//! if none is found. A single-panel form simply exits after one iteration,
//! so this needs no separate "is this a wizard" branch -- see
//! `crate::package_check::PackageSummary::looks_like_a_wizard` for an
//! offline signal that exists purely for a summary/dry-run, never to gate
//! this loop. The panel-navigation selectors in [`wizard_js`] are a best
//! guess from `specs/aem/aem-xml-spec.md`'s JCR-content-model documentation, not a
//! rendered-DOM contract, and were only ever exercised against one real
//! wizard form (`AAOV_033`, see `aaov-output/README.md` at the repo root)
//! on one machine -- correct them there first if a real wizard form's
//! panels are not being detected or advanced.
//!
//! **What is format-specific, and how it plugs in.** How a form's URL is
//! opened, how the wizard's terminal panel is recognised, and how submit
//! is actually triggered are not decided in this module at all -- they
//! come from a `crate::driver::FormDriver`, supplied by whichever thin
//! binary is hosting this crate right now (the generic `aem` format's
//! `crate::driver::GenericDriver`, or a format-specific one such as UBS's
//! own driver in `u2s-aem-ubs-verify-mcp`). This module only ever calls
//! the trait's methods and evaluates the JS strings they hand back; it
//! never itself names a specific customer's platform.
//!
//! Readiness and field access lean on the documented `GuideBridge` API
//! (`specs/aem/aem-xml-spec.md` §15.3) rather than a guessed CSS selector where one is
//! available: the global `guideBridge` object existing is the readiness
//! signal, and a field's own `name` is a global carrying `.value` (§15.2)
//! -- both are AEM's own contract for Adaptive Forms scripting, not
//! something this crate invented. GuideBridge has no documented panel-
//! navigation call, though (`specs/aem/aem-xml-spec.md` §15.3 lists `submit`,
//! `validate`, `reset`, `setFocus`, `on` -- no `gotoNextPanel`/`navigate`),
//! so panel advancement is the one place this module clicks a DOM element
//! instead.
//!
//! **Not handled**: a field living inside a repeatable subform (one that
//! needs `panel.instanceManager.addInstance()`, `specs/aem/aem-xml-spec.md` §15.2,
//! before its GuideBridge global even exists) is not instantiated by this
//! loop -- a `fill` key meant for a second repeatable instance will still
//! show up as `fill_failed` even once its panel is reached. A known,
//! intentional gap, not a bug.
//!
//! **A genuine terminal state with no submit control at all, observed
//! live**: walked against `AAOV_033`, this loop correctly advances
//! through every real content panel and reaches the wizard's actual last
//! panel (a "Summary of form information" review screen, confirmed by its
//! progress indicator reading 100% and offering only a "Back" button) --
//! but that panel never renders *any* submit control, because UBS's own
//! digital-signing capability check itself reports it "cannot currently
//! determine whether this form can be signed digitally" in this
//! environment (no real UBS signing backend behind the local Redacto
//! stack) and falls back to a "print and post" message instead. Neither
//! `.moveNext` nor `.wizard-nav-next` nor `button.submit` exist there, so
//! [`walk_wizard_and_maybe_submit`] correctly reports
//! `wizard_navigation_not_found`/`wizard_incomplete` and refuses to
//! submit -- there is genuinely nothing to click, not a selector miss.
//! (This specific case is what `u2s-aem-ubs-verify-mcp`'s own driver
//! exists to handle: UBS's terminal panel is the *summary* panel, not a
//! submit button, and UBS's own submit routine is what actually gets the
//! form to Redacto -- see that crate's module doc.) See
//! `crates/u2s-aem-ubs-verify-mcp/tests/e2e.rs`'s
//! `verify_run_submits_a_ubs_wizard_and_downloads_the_redacto_pdf` doc
//! comment for the full account, including how this was told apart from an earlier,
//! wrong assumption (screenshots were first matched to panel names by
//! file size, which pointed at the wrong panel; the actual visit order,
//! recovered from blob creation timestamps, showed the true last panel is
//! the small "Summary" screenshot, not the largest one).
//!
//! **`verify_run` versus the interactive tools.** Everything below this
//! doc's own history was written for one-shot `verify_run`, which opens a
//! page, walks it, and closes it inside a single call. `crate::interactive`
//! drives the same form one step at a time instead (`verify_open`,
//! `verify_set`, `verify_next`, ...), so the pieces both paths need --
//! opening the page and waiting for `guideBridge` ([`open_live_form`]),
//! setting one field ([`set_control`]), clicking a panel-navigation
//! control ([`advance_panel`]), submitting ([`submit_and_capture_artefact`])
//! -- are `pub(crate)` functions `crate::interactive` calls directly rather
//! than a second implementation. `execute` and `walk_wizard_and_maybe_submit`
//! stay the orchestration `verify_run` alone uses.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use u2s_blob::{BlobRef, BlobStore};
use u2s_verify_core::browser::{self, BrowserSession};
use u2s_verify_core::docker::{DockerLifecycle, wait_for_http};
use u2s_verify_core::pdf_content;
use u2s_verify_core::types::{
    Artefact, ArtefactKind, BlobDescriptor, ErrorKind, FailedRequest, Finding, Step, VerifyError,
    VerifyReport,
};

use crate::aem_client::AemClient;
use crate::driver::FormDriver;
use crate::instance::{self, Containers, InstancePool};
use crate::package_check::{self, PackageInspection};
use crate::packages::{self, Acquired, FormPathInUse, PackageTable};
use crate::profile::{Profile, SubmitArtefact};

const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(500);
// 30s total -- generous headroom over what live testing needed once the
// real blocker was fixed (see `form_url`'s own comment: `wcmmode=disabled`,
// not a timing budget, was why `guideBridge` never appeared at all against
// a real form before). The original untested 15s (30 attempts) is kept
// doubled here rather than shrunk back down, since this number still has
// only ever been exercised against one real form on one machine.
const READINESS_POLL_ATTEMPTS: u32 = 60;
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(90);
/// A hard ceiling against a stuck loop if panel-advance detection
/// (`wizard_js::panel_fingerprint`) ever false-negatives -- not a real
/// expected panel count (7 observed live in `AAOV_033`). Untested above
/// that, same "generous headroom, exercised against one form" caveat
/// `READINESS_POLL_ATTEMPTS` already carries about itself.
const MAX_WIZARD_PANELS: u32 = 25;
/// Per-panel budget for confirming a "next" click actually changed the
/// visible panel -- same shape as `wait_for_guide_bridge`'s readiness
/// poll, scoped to one panel transition rather than initial page load.
const PANEL_ADVANCE_POLL_ATTEMPTS: u32 = 20;

/// Builds a JS expression that walks `selectors` in priority order and
/// runs `action` (which must `return`) on the first element that is
/// rendered (`offsetParent !== null`) and not `disabled`, evaluating to
/// `false` when none qualifies. One template for every navigation-control
/// lookup, so "which button counts as usable" is decided in one place.
macro_rules! first_usable_nav_control {
    ($action:literal, $($selector:literal),+ $(,)?) => {
        concat!(
            "(function() { var selectors = [",
            $("'", $selector, "', ",)+
            "]; for (var s = 0; s < selectors.length; s++) { ",
            "var els = document.querySelectorAll(selectors[s]); ",
            "for (var i = 0; i < els.length; i++) { var el = els[i]; ",
            "if (el.offsetParent !== null && !el.disabled) { ",
            $action,
            " } } } return false; })()"
        )
    };
}

/// A JS statement list that steps the root panel's wizard with
/// `guideBridge.setFocus(root, focus)` -- the call the step view's own
/// arrows make -- returning `false` when `blocked` (an expression over the
/// root's `context`, its `navigationContext`) says there is no step that
/// way. Confirmed live on `AF_AABF` for both directions.
macro_rules! bridge_step {
    ($focus:literal, $blocked:literal) => {
        concat!(
            "var root = ",
            crate::root_panel!(),
            "; var context = root && root.navigationContext; ",
            "if (!context || ",
            $blocked,
            ") { return false; } ",
            "guideBridge.setFocus(root.somExpression, '",
            $focus,
            "'); return true;"
        )
    };
}

/// The DOM/GuideBridge-event-facing JS this module needs for wizard
/// navigation, since GuideBridge itself has no documented *navigate* call
/// (see this module's own doc) -- only an event to observe navigation
/// after it happens. Confirmed live against a real rendered `AAOV_033`
/// page (both the static server-rendered HTML and, for the event, the
/// stock `/etc.clientlibs/fd/af/runtime/clientlibs/guideRuntime.js`
/// bundle's own OOTB next/previous-navigator handler -- not a UBS
/// customisation, so this should generalise across Adaptive Forms
/// wizards, not just this one customer's). Kept as named constants in one
/// place so there is exactly one spot to correct if a different real
/// wizard form's rendered markup or runtime version disagrees. `pub` so
/// `crate::driver::GenericDriver` can reuse [`HAS_VISIBLE_SUBMIT`] as its
/// own terminal-panel signal without this module needing to know that.
/// The text placeholder, [`wizard_js::PLACEHOLDER`].
macro_rules! placeholder_text {
    () => {
        "UtwoS"
    };
}

/// The placeholder logic of `wizard_js::AUTO_FILL_REQUIRED`, as a macro so it
/// is spliced into that script with `concat!` and run on its own by the tests.
///
/// `placeholderCandidates(kind, clauses)` lists what to type, best first: a
/// candidate the field's own regex pattern (`validatePictureClause`,
/// `displayPictureClause`, `validationPattern`) accepts comes before one it
/// refuses, and an AEM date picker gets 1 January 2000 written in its picture
/// clause (`date{D.M.YYYY}`) first, then the other usual spellings. The caller
/// types them in turn until one sticks. `modelOf(el)` is the field's guide
/// node, found by the id of its `.guideFieldNode` wrapper.
macro_rules! placeholder_js {
    () => {
        concat!(
            r#"
        var PLACEHOLDER_TEXT = '"#,
            placeholder_text!(),
            r#"';
        var guideModels = null;
        function modelOf(el) {
            var node = el.closest ? el.closest('.guideFieldNode') : null;
            if (!node || !node.id) { return null; }
            if (guideModels === null) {
                guideModels = {};
                try {
                    guideBridge.visit(function(m) { if (m && m.id) { guideModels[m.id] = m; } });
                } catch (e) {}
            }
            return guideModels[node.id] || guideModels[node.id.replace(/_guide-item$/, '')] || null;
        }
        function isDatePicker(el, model) {
            if (model && model.className === 'guideDatePicker') { return true; }
            var node = el.closest ? el.closest('.guideFieldNode') : null;
            return !!(node && /guideDatePicker|datepicker/i.test(node.className || ''));
        }
        function clausesOf(model) {
            var json = (model && model.jsonModel) || {};
            return [json.validatePictureClause, json.displayPictureClause, json.validationPattern]
                .filter(function(c) { return typeof c === 'string' && c.length > 0; });
        }
        function dateIn(pattern) {
            return pattern.replace(/YYYY|YY|MM|M|DD|D/g, function(token) {
                return { YYYY: '2000', YY: '00', MM: '01', M: '1', DD: '01', D: '1' }[token];
            });
        }
        function placeholderCandidates(kind, clauses) {
            var out = [];
            function add(v) { if (out.indexOf(v) === -1) { out.push(v); } }
            if (kind === 'datepicker' || kind === 'date') {
                clauses.forEach(function(c) {
                    var m = /^date\{(.*)\}$/.exec(c);
                    if (m) { add(dateIn(m[1])); }
                });
                if (kind === 'date') { add('2000-01-01'); }
                ['1.1.2000', '01.01.2000', '01/01/2000', '2000-01-01'].forEach(add);
            } else if (kind === 'email') { add('verify@example.com'); }
            else if (kind === 'number' || kind === 'tel') { add('1'); }
            else if (kind === 'url') { add('https://example.com'); }
            else { [PLACEHOLDER_TEXT, 'Verifier', 'X'].forEach(add); }
            var patterns = [];
            clauses.forEach(function(c) {
                if (/^[a-z]+\{/.test(c)) { return; }
                try { patterns.push(new RegExp(c)); } catch (e) {}
            });
            var accepted = out.filter(function(v) {
                return patterns.every(function(p) { return p.test(v); });
            });
            return accepted.concat(out.filter(function(v) { return accepted.indexOf(v) === -1; }));
        }
        "#
        )
    };
}

pub mod wizard_js {
    /// The next/submit toolbar buttons are server-rendered with a stable
    /// `name` attribute matching `specs/aem/aem-xml-spec.md` §6.11's
    /// `nextitemnav`/`submit` resource types directly (confirmed against
    /// the raw server HTML: `<button name="nextitemnav" ...>`), but the
    /// stock `guideRuntime.js` client-side runtime strips or rewrites that
    /// `name` attribute once it initialises the page (confirmed live:
    /// `document.querySelectorAll('button[name="nextitemnav"]')` matches
    /// zero elements on a fully-loaded page even though the button is
    /// visible on screen and functional). Its CSS class survives that
    /// rewrite (`button.moveNext`), but that specific control belongs to
    /// this runtime's "Scroll view" toolbar and is itself hidden
    /// (`display:none` on an `afToolbarButton hidden` ancestor, confirmed
    /// live) while the alternate "Step by Step view" mode is active --
    /// which is `AAOV_033`'s own default. That mode's own next control is
    /// `.wizard-nav-next` (also confirmed present in the rendered DOM), so
    /// this checks both class selectors and uses whichever is actually
    /// visible; a real wizard form may use either depending on which view
    /// mode it defaults to or the visitor toggled. Not dependent on button
    /// text (text is localized -- the Italian `AAOV_033` form reads
    /// "Next"). `.wizard-nav-next` is a `div` carrying
    /// `data-guide-wizard-nav='{"focusOption": "nextItem", ...}'`
    /// (confirmed live on `AF_AABF`, where it is hidden): the step view's
    /// arrow, which steps with `guideBridge.setFocus(..., 'nextItem')`.
    pub const HAS_VISIBLE_NEXT: &str =
        first_usable_nav_control!("return true;", "button.moveNext", ".wizard-nav-next");

    /// Moves to the next panel, evaluating to whether a move was started:
    /// the toolbar's usable `button.moveNext` when there is one (it runs the
    /// form's own click script), otherwise the step the wizard's own arrow
    /// takes ([`HAS_VISIBLE_NEXT`]'s doc), made directly through
    /// GuideBridge so it does not depend on which view mode shows the
    /// arrow. `false` on the last step.
    pub const CLICK_NEXT: &str = concat!(
        "(function() { if (",
        first_usable_nav_control!("el.click(); return true;", "button.moveNext"),
        ") { return true; } ",
        bridge_step!("nextItem", "!context.hasNextItem"),
        " })()"
    );

    /// Clicks the first usable previous-panel control. The toolbar's
    /// previous button is authored as `type="movePrev"`
    /// (`u2s-aem-ubs-mcp/src/aem/lower.rs`, and every fixture package),
    /// which the runtime renders as the class `movePrev` (confirmed in the
    /// live server-rendered HTML); `movePrevious` is kept as a fallback for
    /// runtimes that spell it out.
    ///
    /// Without a usable toolbar button it steps back through GuideBridge,
    /// like [`CLICK_NEXT`]; `false` on the first step.
    pub const CLICK_PREV: &str = concat!(
        "(function() { if (",
        first_usable_nav_control!(
            "el.click(); return true;",
            "button.movePrev",
            "button.movePrevious"
        ),
        ") { return true; } ",
        bridge_step!("prevItem", "context.isFirstItem"),
        " })()"
    );

    /// Jumps straight to the panel `som` names
    /// (`guideBridge.setFocus(som)`, confirmed live on `AF_AABF` to move a
    /// wizard forward or back by any number of steps and to fire one panel
    /// navigation event). It does not run the step validation a "next"
    /// click does.
    pub fn goto_panel(som: &str) -> String {
        format!(
            "(function(som) {{ guideBridge.setFocus(som); return true; }})({})",
            serde_json::Value::String(som.to_owned())
        )
    }

    /// Runs `guideBridge.validate` over `panel_som` (the whole form when
    /// `None`) and evaluates to a JSON array of `{"field", "message"}`
    /// objects, or `{"error": ...}` if validate itself threw. Used after a
    /// navigation click that did not move the panel, to name what blocked
    /// it (`specs/aem/aem-xml-spec.md` §15.3 documents the
    /// `validate(errorList, somExpression)` signature but not the shape of
    /// an entry, so every plausible property name is read and the raw
    /// entry kept as the message when none of them is present).
    pub fn validate(panel_som: Option<&str>) -> String {
        let som = serde_json::to_string(&panel_som).expect("an Option<&str> always serialises");
        format!(
            r#"(function(som) {{
        var errors = [];
        try {{
            if (som) {{ guideBridge.validate(errors, som); }} else {{ guideBridge.validate(errors); }}
        }} catch (e) {{ return JSON.stringify({{ error: String(e) }}); }}
        return JSON.stringify(errors.map(function(entry) {{
            var field = entry && (entry.som || entry.somExpression || entry.name) || '';
            var message = entry && (entry.errorText || entry.errorMessage || entry.message) || '';
            if (!message) {{
                try {{ message = JSON.stringify(entry); }} catch (e) {{ message = String(entry); }}
            }}
            return {{ field: String(field), message: String(message).trim() }};
        }}));
    }})({som})"#
        )
    }

    /// Sets one named field the way a person would, returning a JSON
    /// report of what happened: `{"set": true, "via": "model"}`,
    /// `{"set": true, "via": "widget"}`, or `{"set": false}`. See
    /// [`super::fill_current_panel`] and [`super::set_control`] for why
    /// this goes through the DOM widget rather than a same-named JS
    /// global, and why the model is tried first.
    ///
    /// `value` is injected as its JSON literal, so it carries its own
    /// quoting and a string value cannot break out of the expression. The
    /// name is matched as an exact attribute value, so a name containing a
    /// quote simply fails to match rather than changing what runs.
    ///
    /// A [`super::FieldRef::Som`] is resolved directly
    /// (`guideBridge.resolveNode`), which is how a field inside the second
    /// instance of a repeated panel is reached; it is model-only, since a
    /// SOM always names a model node. A [`super::FieldRef::Name`] keeps the
    /// first-match name walk `verify_run`'s `fill` relies on.
    pub fn fill_field(target: super::FieldRef<'_>, value: &serde_json::Value) -> String {
        let (som, name) = match target {
            super::FieldRef::Som(som) => (Some(som), None),
            super::FieldRef::Name(name) => (None, Some(name)),
        };
        format!(
            r#"(function() {{
    var som = {som};
    var name = {name};
    var value = {value};

    if (som) {{
        var target = null;
        try {{
            target = guideBridge.resolveNode(som);
            if (target) {{ target.value = value; return JSON.stringify({{set: true, via: 'model'}}); }}
        }} catch (e) {{
            // The model may take the value and the runtime then fail while
            // updating the widget (confirmed live on AAQR_033: guideRuntime's
            // `_setOption` throws, the model holds the value, the input stays
            // empty). The model is what validation and submit read, so that
            // counts as set, with the error reported.
            if (target && target.value == value) {{
                return JSON.stringify({{set: true, via: 'model', view_error: String(e)}});
            }}
        }}
        return JSON.stringify({{set: false}});
    }}

    // Preferred: set the value on the *model* node. Assigning to the widget
    // instead only changes the DOM, and AEM re-renders a panel from its
    // model when it becomes visible, so a DOM-only value is silently thrown
    // away -- confirmed live, by a panel screenshot that came back
    // byte-identical to the unfilled run. The model is also what a summary
    // component reads when it builds itself, which a DOM value never
    // reaches.
    //
    // The traversal (resolve the root panel, then walk `.items` recursively)
    // is the same one ajila-forms-ubs's own `getGuideNodeFromElementId`
    // uses, matching on `.name` rather than `.id`.
    try {{
        if (typeof guideBridge !== 'undefined') {{
            var root = {root};
            var find = function(items) {{
                if (!items) {{ return null; }}
                for (var i = 0; i < items.length; i++) {{
                    var item = items[i];
                    if (!item) {{ continue; }}
                    if (item.name === name) {{ return item; }}
                    var nested = find(item.items);
                    if (nested) {{ return nested; }}
                }}
                return null;
            }};
            var node = root && find(root.items);
            if (node) {{ node.value = value; return JSON.stringify({{set: true, via: 'model'}}); }}
        }}
    }} catch (e) {{ /* fall through to the widget */ }}

    // Fallback for anything the model walk cannot reach. Only accepted when
    // the widget is actually on the visible panel, since that is the one
    // case where AEM's own bindings are live and will carry the value back
    // into the model; a hidden one would be the silent no-op described
    // above. The caller retries an unfilled name on every later panel.
    var el = document.querySelector('[name="' + name + '"]');
    if (!el) {{ return JSON.stringify({{set: false}}); }}
    var field = el.closest ? el.closest('.guideFieldNode') : null;
    if (el.offsetParent === null && !(field && field.offsetParent !== null)) {{
        return JSON.stringify({{set: false}});
    }}
    var type = (el.getAttribute('type') || el.tagName).toLowerCase();
    if (type === 'radio' || type === 'checkbox') {{
        var match = document.querySelector(
            '[name="' + name + '"][value="' + value + '"]') || el;
        match.click();
        return JSON.stringify({{set: true, via: 'widget'}});
    }}
    el.value = value;
    ['input', 'change', 'blur'].forEach(function(event) {{
        el.dispatchEvent(new Event(event, {{ bubbles: true }}));
    }});
    return JSON.stringify({{set: true, via: 'widget'}});
}})()"#,
            som = serde_json::to_string(&som).expect("an Option<&str> always serialises"),
            name = serde_json::to_string(&name).expect("an Option<&str> always serialises"),
            value = value,
            root = crate::root_panel!(),
        )
    }

    /// Fills every *visible, required, still-empty* widget on the current
    /// panel with a type-appropriate placeholder, and reports back as JSON
    /// exactly what it touched:
    /// `{"filled":[{"name","kind","value"}],"skipped":[{"name","reason"}]}`.
    ///
    /// Why this is needed at all: an Adaptive Forms wizard validates the
    /// current panel before it will advance (`specs/aem/aem-xml-spec.md` §15.1's "Step
    /// Completion", and UBS's own `validation.js` calls
    /// `guideBridge.validate` from `nextStep`), so a panel carrying an
    /// unfilled required field simply refuses to move and the walk stalls
    /// with no error of its own.
    ///
    /// Deliberately DOM-driven rather than model-driven. Going through
    /// GuideBridge would mean resolving each field's SOM expression from
    /// the DOM first, and the DOM-to-model direction has no stable public
    /// accessor (`resolveNode` goes the other way). Setting the widget and
    /// dispatching `input`/`change`/`blur` is what a real visitor's typing
    /// does, and is what AEM's own widget bindings listen to.
    ///
    /// Everything it cannot confidently fill is reported in `skipped`
    /// rather than guessed at, so one live run is enough to see what a new
    /// widget type needs -- the same "make a miss correctable without a
    /// second run" reasoning as [`NAVIGATION_DIAGNOSTIC`].
    ///
    /// What it types is [`PLACEHOLDER`]'s: a value the field's own pattern
    /// accepts, so the wizard's step validation lets the panel go.
    pub const AUTO_FILL_REQUIRED: &str = concat!(
        r#"(function() {
        "#,
        placeholder_js!(),
        r#"
        var filled = [];
        var skipped = [];
        var seenRadioGroups = {};

        function visible(el) {
            return el && el.offsetParent !== null;
        }
        function label(el) {
            return el.getAttribute('name') || el.id ||
                el.getAttribute('aria-label') || el.tagName.toLowerCase();
        }
        function required(el) {
            if (el.getAttribute('aria-required') === 'true') { return true; }
            if (el.hasAttribute('required')) { return true; }
            var node = el.closest ? el.closest('.guideFieldNode') : null;
            return !!(node && node.className.indexOf('guideFieldRequired') !== -1);
        }
        function announce(el) {
            ['input', 'change', 'blur'].forEach(function(type) {
                el.dispatchEvent(new Event(type, { bubbles: true }));
            });
        }

        var widgets = document.querySelectorAll('input, select, textarea');
        for (var i = 0; i < widgets.length; i++) {
            var el = widgets[i];
            var type = (el.getAttribute('type') || el.tagName).toLowerCase();

            if (type === 'hidden' || el.disabled || el.readOnly) { continue; }
            if (!visible(el) || !required(el)) { continue; }

            var name = label(el);
            try {
                if (type === 'radio') {
                    var group = el.getAttribute('name') || name;
                    if (seenRadioGroups[group]) { continue; }
                    if (document.querySelector('input[type=radio][name="' + group + '"]:checked')) {
                        continue;
                    }
                    seenRadioGroups[group] = true;
                    el.click();
                    filled.push({ name: group, kind: 'radio', value: el.value || 'first option' });
                } else if (type === 'checkbox') {
                    if (el.checked) { continue; }
                    el.click();
                    filled.push({ name: name, kind: 'checkbox', value: 'checked' });
                } else if (type === 'select') {
                    if (el.value) { continue; }
                    var chosen = null;
                    for (var o = 0; o < el.options.length; o++) {
                        if (el.options[o].value) { chosen = el.options[o]; break; }
                    }
                    if (!chosen) { skipped.push({ name: name, reason: 'no non-empty option' }); continue; }
                    el.value = chosen.value;
                    announce(el);
                    filled.push({ name: name, kind: 'select', value: chosen.value });
                } else {
                    if (el.value) { continue; }
                    var model = modelOf(el);
                    var kind = isDatePicker(el, model) ? 'datepicker' : type;
                    // An AEM date picker's widget shows the date in its
                    // display pattern, but its model holds it as ISO, and
                    // typing into the widget does not always reach the model
                    // (the step then reports the field as empty).
                    if (kind === 'datepicker' && model) {
                        model.value = '2000-01-01';
                        if (model.value) {
                            filled.push({ name: name, kind: kind, value: '2000-01-01' });
                            continue;
                        }
                    }
                    var value = null;
                    var candidates = placeholderCandidates(kind, clausesOf(model));
                    for (var c = 0; c < candidates.length; c++) {
                        el.value = candidates[c];
                        announce(el);
                        if (el.value) { value = candidates[c]; break; }
                    }
                    if (value === null) { skipped.push({ name: name, reason: 'value did not stick' }); continue; }
                    filled.push({ name: name, kind: kind, value: value });
                }
            } catch (e) {
                skipped.push({ name: name, reason: String(e) });
            }
        }
        return JSON.stringify({ filled: filled, skipped: skipped });
    })()"#
    );

    /// The text [`AUTO_FILL_REQUIRED`] types into a text field whose pattern
    /// does not ask for something else: free of digits, since a name field
    /// may refuse them (the UBS letterhead's `^([^0-9]*)$`), and unlikely to
    /// be text the form prints of its own, so a rendered document showing it
    /// shows the submitted data.
    pub const PLACEHOLDER: &str = placeholder_text!();

    /// What the last submit's error callback recorded
    /// ([`crate::capture_submit_error_js`]), or an empty string.
    pub const SUBMIT_ERROR: &str = "window.__u2sSubmitError || ''";

    /// Evaluates to the SOM of the step the root wizard currently shows
    /// (`navigationContext.currentItem`), the before/after signal that a
    /// "next" or "previous" actually moved: a click firing without error
    /// is not proof of that, since the form's step validation can block
    /// it. Read from the wizard's own state, not from
    /// `elementNavigationChanged` events: confirmed live that a move does
    /// not always fire one (`BAZA_019` stepping back to its first step,
    /// `AABE_019` jumping to its summary step) while `currentItem` always
    /// follows, and that the event also fires for the toolbar when a button
    /// takes focus.
    pub const PANEL_FINGERPRINT: &str = concat!(
        "(function() { var root = ",
        crate::root_panel!(),
        "; var current = root && root.navigationContext && root.navigationContext.currentItem; ",
        "return current && current.somExpression ? current.somExpression : ''; })()"
    );

    /// [`PANEL_FINGERPRINT`] for one container, `container`: the SOM of
    /// the step it currently shows. What a jump into a nested accordion or
    /// wizard is confirmed against, since the root's step need not change.
    pub fn container_fingerprint(container: &str) -> String {
        format!(
            "(function(som) {{ var node = guideBridge.resolveNode(som); \
             var current = node && node.navigationContext && node.navigationContext.currentItem; \
             return current && current.somExpression ? current.somExpression : ''; }})({})",
            serde_json::Value::String(container.to_owned())
        )
    }

    /// The submit button is server-rendered `name="submit"`
    /// (`specs/aem/aem-xml-spec.md` §6.11) but, like the next button, loses that
    /// attribute once `guideRuntime.js` initialises the page -- see
    /// [`HAS_VISIBLE_NEXT`]'s doc for why this selects on the
    /// (server-rendered, and so far still present at runtime) CSS class
    /// `submit` instead. This is `crate::driver::GenericDriver`'s own
    /// terminal-panel signal; a format-specific driver may recognise its
    /// terminal panel a different way entirely (UBS's summary panel has
    /// no submit control at all -- see this module's own doc).
    pub const HAS_VISIBLE_SUBMIT: &str = r#"(function() {
        var els = document.querySelectorAll('button.submit');
        for (var i = 0; i < els.length; i++) {
            if (els[i].offsetParent !== null) { return true; }
        }
        return false;
    })()"#;

    /// Scrolls the window (and, for good measure, any panel-shaped
    /// element that itself scrolls) to the bottom. Some panels gate their
    /// own forward-navigation control on the visitor having scrolled to
    /// the end of the panel's content (a common pattern for a long legal
    /// disclosure panel specifically) -- a headless full-page screenshot
    /// never triggers that on its own, since it captures the whole page
    /// without any real scrolling. Cheap and harmless to run on every
    /// panel, not just ones that need it.
    pub const SCROLL_TO_BOTTOM: &str = r#"(function() {
        window.scrollTo(0, document.body.scrollHeight);
        var scrollables = document.querySelectorAll('.panel, [class*="guidePanel"]');
        for (var i = 0; i < scrollables.length; i++) {
            var el = scrollables[i];
            if (el.scrollHeight > el.clientHeight) {
                el.scrollTop = el.scrollHeight;
            }
        }
        return true;
    })()"#;

    /// A JSON diagnostic for when neither [`HAS_VISIBLE_NEXT`] nor a
    /// driver's own terminal-panel check found anything -- how many
    /// elements a handful of known selectors matched at all (regardless
    /// of visibility), so a selector miss can be corrected from the
    /// finding message alone rather than needing a second live run with a
    /// debugger attached.
    pub const NAVIGATION_DIAGNOSTIC: &str = r#"JSON.stringify((function() {
        function describe(selector) {
            var el = document.querySelector(selector);
            if (!el) { return null; }
            var rect = el.getBoundingClientRect();
            var cs = window.getComputedStyle(el);
            var ancestors = [];
            var p = el.parentElement;
            var depth = 0;
            while (p && depth < 8) {
                var pcs = window.getComputedStyle(p);
                ancestors.push({
                    tag: p.tagName,
                    cls: p.className,
                    display: pcs.display,
                    visibility: pcs.visibility,
                    position: pcs.position
                });
                p = p.parentElement;
                depth++;
            }
            return {
                tag: el.tagName,
                cls: el.className,
                disabled: el.disabled,
                display: cs.display,
                visibility: cs.visibility,
                position: cs.position,
                width: rect.width,
                height: rect.height,
                offsetParentTag: el.offsetParent ? el.offsetParent.tagName : null,
                ancestors: ancestors
            };
        }
        return {
            nextCount: document.querySelectorAll('button[name="nextitemnav"]').length,
            submitCount: document.querySelectorAll('button[name="submit"]').length,
            prevCount: document.querySelectorAll('button[name="previtemnav"]').length,
            moveNextCount: document.querySelectorAll('.moveNext').length,
            wizardNavNextCount: document.querySelectorAll('.wizard-nav-next').length,
            toolbarCount: document.querySelectorAll('.guideToolbar, [class*="toolbar"]').length,
            totalButtonCount: document.querySelectorAll('button').length,
            iframeCount: document.querySelectorAll('iframe').length,
            bodyLength: document.body ? document.body.innerHTML.length : -1,
            title: document.title,
            readyState: document.readyState,
            guideBridgeType: typeof window.guideBridge,
            moveNextElement: describe('.moveNext'),
            wizardNavNextElement: describe('.wizard-nav-next'),
            submitElement: describe('button.submit')
        };
    })())"#;
}

pub struct RunRequest {
    pub package_bytes: Vec<u8>,
    /// Field name (the guide node's own `name`, and so the global
    /// `GuideBridge` exposes it under) to the value to assign its `.value`.
    pub fill: BTreeMap<String, Value>,
    pub submit: bool,
    /// The language to open the form in, as the driver's
    /// [`FormDriver::form_url_query`] takes it; `None` leaves the choice to
    /// the driver.
    pub language: Option<String>,
}

fn storage_failed(action: &str, err: u2s_blob::BlobError) -> VerifyError {
    VerifyError::new(ErrorKind::StorageFailed, format!("{action}: {err}"))
}

/// A minimal `application/x-www-form-urlencoded` query-string encoder --
/// every value a `FormDriver` hands back is a short ASCII token (a
/// `wcmmode` flag, a mandator code, a two-letter language), so this is
/// deliberately not a general-purpose URL library dependency, just percent-
/// encoding for the handful of characters that would otherwise break the
/// query string (a space, `&`, `=`, `%`, `#`, `?`, `+`).
fn encode_query(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Validates `package_bytes` and reports whether Docker is reachable,
/// touching nothing else -- the contract [`u2s_mcp::manifest::VerifyCapability::Run`]
/// documents for `dry_run: true`, and what lets a conformance vector
/// exercise this tool without the side effect a real run performs.
pub async fn dry_run(package_bytes: &[u8]) -> VerifyReport {
    let started = Instant::now();
    let mut findings = Vec::new();

    if let Err(err) = package_check::check(package_bytes) {
        findings.push(Finding::error(
            ErrorKind::PackageInvalid.as_str(),
            err.to_string(),
        ));
    }

    match DockerLifecycle::connect().await {
        Ok(docker) if docker.is_reachable().await => {}
        _ => findings.push(Finding::warning(
            ErrorKind::DockerUnreachable.as_str(),
            "the Docker daemon is not reachable; a real verify_run would fail at the \
             acquire-instance step"
                .to_owned(),
        )),
    }

    VerifyReport::dry(findings, started.elapsed().as_millis() as u64)
}

/// Runs the whole flow for real: reuse or boot `instance_id`'s instance,
/// install the package under test, render, screenshot, optionally fill and
/// submit, uninstall the package again. `instance_id` is what keeps two
/// concurrent callers from interfering (AGENTS.md's per-agent-session
/// requirement) -- each gets its own AEM+Chromium instance, never sharing
/// one; see `crate::instance`'s module doc. That instance's containers are
/// never torn down here -- but whatever this call installed is always
/// cleaned up (unless `keep_on_failure` and this call failed, so the
/// failure state stays inspectable), regardless of how `execute` finished.
///
/// Runs beside the sessions open on the instance (`crate::interactive`),
/// which may belong to other callers sharing it: the walk opens a page of
/// its own, and the instance lock it holds keeps their calls out until it
/// is done. The package table is what keeps the two apart: a session's
/// package stays installed through this run's uninstall, and a different
/// build of a form a session shows is refused (`install_package`).
///
/// `driver` supplies the format-specific behaviour this crate itself never
/// names -- see this module's own doc and `crate::driver::FormDriver`.
pub async fn run(
    profile: &Profile,
    driver: &dyn FormDriver,
    blobs: &BlobStore,
    pool: &InstancePool,
    instance_id: &str,
    request: RunRequest,
) -> Result<VerifyReport, VerifyError> {
    let started = Instant::now();

    let package = package_check::inspect(&request.package_bytes)
        .map_err(|err| VerifyError::new(ErrorKind::PackageInvalid, err.to_string()))?;

    if request.submit && matches!(profile.submit, SubmitArtefact::Download) {
        check_redacto_reachable(profile).await?;
    }

    let docker = DockerLifecycle::connect()
        .await
        .map_err(|err| VerifyError::new(ErrorKind::DockerUnreachable, err.to_string()))?;

    let (mut guard, boot_findings) = instance::ensure(pool, instance_id, &docker, profile).await?;
    let state = guard
        .as_mut()
        .expect("ensure always leaves Some on success");

    // Defensive: a prior call may have kept its package (keep_on_failure)
    // or crashed before uninstalling. Clearing it first means this run's
    // install never collides with stale nodes.
    uninstall_unreferenced(
        profile,
        &state.containers,
        &mut state.packages,
        "a previous call's leftover package",
    )
    .await;

    let form_path = package.summary.form_jcr_path.clone();
    install_package(
        profile,
        &state.containers,
        &mut state.packages,
        &package,
        request.package_bytes.clone(),
    )
    .await?;

    let outcome = execute(
        profile,
        driver,
        blobs,
        &state.containers,
        package,
        &request,
        boot_findings,
        started,
    )
    .await;

    state.packages.release(&form_path);
    if !(profile.keep_on_failure && outcome.is_err()) {
        uninstall_unreferenced(
            profile,
            &state.containers,
            &mut state.packages,
            "this run's package",
        )
        .await;
    }

    outcome
}

/// Uninstalls every package `packages` records with no holder left,
/// logging (never failing) on error: a package that would not uninstall is
/// forgotten anyway, since a later install of the same form simply
/// replaces it. `context` names the package only for the log line.
pub(crate) async fn uninstall_unreferenced(
    profile: &Profile,
    containers: &Containers,
    packages: &mut PackageTable,
    context: &str,
) {
    let unreferenced = packages.take_unreferenced();
    if unreferenced.is_empty() {
        return;
    }
    let aem_client = AemClient::new(
        &containers.aem_base_url,
        &profile.aem_user,
        &profile.aem_password,
    );
    for path in unreferenced {
        if let Err(err) = aem_client.uninstall(&path).await {
            log::warn!(
                "{}: could not uninstall {context} ({path}): {err}",
                crate::LOG_PREFIX
            );
        }
    }
}

/// Takes a hold on `package` in `packages` (`crate::packages`), installing
/// it on `containers`' AEM first unless the identical package is already
/// installed and held. On success the caller holds it and must
/// [`PackageTable::release`] it by its form JCR path when done; on error
/// nothing is held. A different package for a form path another holder
/// still uses is refused rather than installed over it.
pub(crate) async fn install_package(
    profile: &Profile,
    containers: &Containers,
    packages: &mut PackageTable,
    package: &PackageInspection,
    package_bytes: Vec<u8>,
) -> Result<(), VerifyError> {
    let form_path = &package.summary.form_jcr_path;
    let fingerprint = packages::fingerprint(&package_bytes);
    match packages.acquire(form_path, fingerprint) {
        Ok(Acquired::Shared) => Ok(()),
        Ok(Acquired::MustInstall) => {
            let aem_client = AemClient::new(
                &containers.aem_base_url,
                &profile.aem_user,
                &profile.aem_password,
            );
            let install_path = aem_client
                .upload_and_install(package_bytes, &package.summary.form_name)
                .await
                .map_err(|err| VerifyError::new(ErrorKind::InstallFailed, err.to_string()))?;
            packages.installed(form_path, fingerprint, install_path);
            Ok(())
        }
        Err(FormPathInUse { form_path, holders }) => Err(VerifyError::new(
            ErrorKind::FormOpen,
            format!(
                "{form_path} is already open in {holders} session(s) from a different package; \
                 installing this one would change the form under them -- close those sessions \
                 first (verify_status lists them), or open them all from the same package"
            ),
        )),
    }
}

/// Checks `profile.redacto_url` (when configured) is reachable before a
/// submit that would depend on it -- refusing up front is a clearer failure
/// than letting `guideBridge.submit()` appear to succeed while AEM's own
/// server-side call to Redacto fails invisibly to the browser.
pub(crate) async fn check_redacto_reachable(profile: &Profile) -> Result<(), VerifyError> {
    let Some(url) = &profile.redacto_url else {
        return Ok(());
    };
    if u2s_verify_core::http::is_reachable(url, Duration::from_secs(5)).await {
        Ok(())
    } else {
        Err(VerifyError::new(
            ErrorKind::RedactoUnreachable,
            format!(
                "{url} is not reachable; refusing to submit a form whose rendering depends on it"
            ),
        ))
    }
}

/// A page open on a specific installed form, plus the browser session it
/// belongs to and the package inspection that opened it -- what
/// [`execute`] and every `crate::interactive` operation drive. Not `Clone`,
/// not `Copy`: exactly one of these exists per open form, and
/// [`Self::close`] consumes it so a caller cannot accidentally act on a
/// page it already tore down.
pub(crate) struct LiveForm {
    pub browser_session: BrowserSession,
    pub page: browser::PageHandle,
    pub package: PackageInspection,
    /// The URL the *browser* (inside its own container) opened -- kept so
    /// `crate::interactive::reset` can navigate back to it without
    /// re-deriving `driver.form_url_query` a second time.
    pub form_url_for_browser: String,
}

impl LiveForm {
    /// Closes the page, then disconnects from the browser, which stays up for
    /// the session's next form (see [`BrowserSession::disconnect`]).
    pub(crate) async fn close(self) {
        self.page.close().await;
        self.browser_session.disconnect().await;
    }
}

/// What [`open_live_form`] hands back: the opened [`LiveForm`], whether
/// `guideBridge` was actually observed ready, and whatever findings that
/// process itself produced (currently only `guide_bridge_not_detected`,
/// when it was not).
pub(crate) struct OpenedForm {
    pub form: LiveForm,
    pub bridge_ready: bool,
    pub findings: Vec<Finding>,
}

/// Opens `package`'s already installed form ([`install_package`]) in a
/// fresh page in `containers`' Chromium and waits for `guideBridge` --
/// everything [`execute`] and `crate::interactive::open` both need before
/// they can start filling or walking a form. Installing is the caller's
/// step, so the caller also owns releasing the package when this fails.
///
/// Formerly registered a navigation-event listener once `guideBridge` was
/// confirmed ready -- unconditionally, not only when a submit is about to
/// happen (a change from this crate's own history: `verify_run` used to
/// register it only inside [`walk_wizard_and_maybe_submit`], entered only
/// when `submit` was requested). Registering a no-op event listener has no
/// observable effect on a report's steps or findings either way, and doing
/// it here means `crate::interactive::open` gets the same panel-advance
/// signal `verify_next`/`verify_prev` need without a second registration
/// call.
pub(crate) async fn open_live_form(
    profile: &Profile,
    driver: &dyn FormDriver,
    containers: &Containers,
    package: PackageInspection,
    language: Option<&str>,
) -> Result<OpenedForm, VerifyError> {
    let aem_base_url = &containers.aem_base_url;
    let cdp_url = &containers.cdp_url;
    let summary = &package.summary;

    // The query string a real visitor's URL would carry -- `wcmmode=disabled`
    // for the generic driver, plus whatever a format-specific driver needs
    // to open its own form at all (UBS's `mandator`/`afAcceptLang`, derived
    // from the package's own metadata, say). An `Err` here means the
    // driver could not determine how to open this package's form -- a
    // package-level problem, not a Docker/AEM one.
    let query = driver
        .form_url_query(&package, language)
        .map_err(|err| VerifyError::new(ErrorKind::PackageInvalid, err))?;
    let query_string = encode_query(&query);

    // Two different URLs for the same form, deliberately: this process
    // (and so this readiness wait) runs on the host, but the browser it
    // hands the URL to afterward runs inside its own container on the same
    // Docker network as AEM -- confirmed live, `127.0.0.1:<host port>` is
    // simply unreachable from there (that loopback is Chromium's own
    // container, not AEM's), so the browser needs AEM's address as seen
    // from inside that network instead.
    // `wcmmode=disabled` is not decoration: without it, AEM's author
    // instance renders an Adaptive Form wrapped in its own authoring
    // chrome/iframe rather than the live runtime a real visitor (or the
    // wiki's own documented UBS form URLs) would see, and `guideBridge`
    // then never appears as a global on the *top* page at all -- confirmed
    // live, the page still rendered and screenshotted cleanly either way,
    // so this was silently wrong rather than loudly broken.
    let form_url = format!(
        "{aem_base_url}{}.html?{query_string}",
        summary.form_jcr_path
    );
    let form_url_for_browser = format!(
        "{}{}.html?{query_string}",
        containers.aem_network_url, summary.form_jcr_path
    );
    wait_for_http(
        &form_url,
        200,
        Duration::from_secs(60),
        Duration::from_secs(2),
        // Unlike the login page (deliberately anonymous, so there is
        // something to show *before* logging in), a form's own page is
        // protected content: confirmed live, AEM answers it 401 without
        // credentials, indistinguishable from "not ready yet" until this
        // wait simply timed out.
        Some((&profile.aem_user, &profile.aem_password)),
    )
    .await
    .map_err(|err| VerifyError::new(ErrorKind::FormNotFound, err.to_string()))?;

    let mut browser_session = BrowserSession::connect(cdp_url)
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ChromiumNotReady, err.to_string()))?;
    browser_session
        .enable_downloads(instance::CHROMIUM_DOWNLOAD_DIR)
        .await
        .map_err(|err| VerifyError::new(ErrorKind::ChromiumNotReady, err.to_string()))?;

    let page = browser_session
        .open(
            &form_url_for_browser,
            Some((&profile.aem_user, &profile.aem_password)),
        )
        .await
        .map_err(|err| VerifyError::new(ErrorKind::RenderTimeout, err.to_string()))?;

    let bridge_ready = wait_for_guide_bridge(&page).await;
    let mut findings = Vec::new();
    if !bridge_ready {
        let diagnostic = page
            .evaluate_string(
                "JSON.stringify({\
                   readyState: document.readyState,\
                   guidelibKeys: window.guidelib ? Object.keys(window.guidelib) : null,\
                   guidelibType: typeof window.guidelib,\
                   hasBridgeProp: window.guidelib && 'guideBridge' in window.guidelib,\
                 })",
            )
            .await
            .unwrap_or_default();
        findings.push(Finding::warning(
            "guide_bridge_not_detected",
            format!(
                "the global guideBridge object was not observed within {} attempts; the \
                 screenshot and any console/network findings below are still real, but a \
                 fill or submit was skipped. page diagnostic: {diagnostic}",
                READINESS_POLL_ATTEMPTS
            ),
        ));
    }

    Ok(OpenedForm {
        form: LiveForm {
            browser_session,
            page,
            package,
            form_url_for_browser,
        },
        bridge_ready,
        findings,
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    profile: &Profile,
    driver: &dyn FormDriver,
    blobs: &BlobStore,
    containers: &Containers,
    package: PackageInspection,
    request: &RunRequest,
    mut findings: Vec<Finding>,
    started: Instant,
) -> Result<VerifyReport, VerifyError> {
    let package_blob = blobs
        .put(&request.package_bytes, "application/zip", "zip")
        .map_err(|err| storage_failed("storing the package", err))?;

    let opened = open_live_form(
        profile,
        driver,
        containers,
        package,
        request.language.as_deref(),
    )
    .await?;
    findings.extend(opened.findings);
    let LiveForm {
        browser_session,
        page,
        package,
        ..
    } = opened.form;
    let ready = opened.bridge_ready;

    let screenshot = page
        .screenshot_full_page()
        .await
        .map_err(|err| VerifyError::new(ErrorKind::RenderTimeout, err.to_string()))?;
    let screenshot_blob = blobs
        .put(&screenshot, "image/png", "png")
        .map_err(|err| storage_failed("storing the screenshot", err))?;

    let mut steps = vec![Step {
        name: "form".to_owned(),
        screenshot: BlobDescriptor::from(&screenshot_blob),
        console_errors: page.console_errors(),
        failed_requests: page.failed_requests(),
        dialogs: page.dialogs(),
    }];

    let mut artefacts = vec![Artefact {
        kind: ArtefactKind::Package,
        label: "the package this run installed".to_owned(),
        blob: BlobDescriptor::from(&package_blob),
    }];

    if request.submit && ready {
        walk_wizard_and_maybe_submit(
            &page,
            &browser_session,
            profile,
            driver,
            &package,
            containers,
            blobs,
            &request.fill,
            request.submit,
            &mut steps,
            &mut artefacts,
            &mut findings,
        )
        .await;
    }

    page.close().await;
    browser_session.disconnect().await;

    Ok(VerifyReport {
        dry_run: false,
        steps,
        artefacts,
        findings,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Polls for `guideBridge` to exist in the page's global scope --
/// `specs/aem/aem-xml-spec.md`'s own scripting-model doc (§15.3) is what makes this the
/// documented readiness signal rather than a guess: every example there
/// calls a method on this same global, so its absence means the form has
/// not finished initialising (or this page is not an Adaptive Form at
/// all).
pub(crate) async fn wait_for_guide_bridge(page: &browser::PageHandle) -> bool {
    for _ in 0..READINESS_POLL_ATTEMPTS {
        if page
            .evaluate_bool("typeof guideBridge !== 'undefined'")
            .await
            .unwrap_or(false)
        {
            return true;
        }
        page.wait(READINESS_POLL_INTERVAL).await;
    }
    false
}

/// Walks a (possibly single-panel) wizard: on each panel, fills whatever
/// requested fields are fillable there, screenshots it (panel 1's
/// screenshot is the one `execute` already took as `steps[0]` ("form"),
/// never retaken here), and either advances to the next panel or -- once
/// no "next" control is visible and `driver`'s own terminal-panel signal
/// fires -- hands off to [`submit_and_capture_artefact`]. A field that
/// never becomes fillable on any panel it walks, or a wizard that never
/// confirms reaching its last panel while `should_submit` is set, is a
/// finding, not a hard failure -- whatever screenshots and artefacts were
/// already collected are still worth returning.
#[allow(clippy::too_many_arguments)]
async fn walk_wizard_and_maybe_submit(
    page: &browser::PageHandle,
    session: &BrowserSession,
    profile: &Profile,
    driver: &dyn FormDriver,
    package: &PackageInspection,
    containers: &Containers,
    blobs: &BlobStore,
    fill: &BTreeMap<String, Value>,
    should_submit: bool,
    steps: &mut Vec<Step>,
    artefacts: &mut Vec<Artefact>,
    findings: &mut Vec<Finding>,
) {
    let mut filled: BTreeSet<String> = BTreeSet::new();
    let mut reached_last_panel = false;

    let terminal_panel_js = driver.terminal_panel_js();
    let has_next_js = driver.has_next_js();
    let click_next_js = driver.click_next_js();

    for panel_index in 1..=MAX_WIZARD_PANELS {
        fill_current_panel(page, fill, &mut filled).await;

        // After the caller's own values, never before: an explicitly
        // supplied value must win over an invented one.
        if let Ok(report) = page.evaluate_string(wizard_js::AUTO_FILL_REQUIRED).await
            && let Some(finding) = auto_fill_finding(panel_index, &report)
        {
            findings.push(finding);
        }

        if panel_index > 1 {
            screenshot_panel(page, blobs, panel_index, steps, findings).await;
        }

        let nav = wait_for_navigation_controls(page, &has_next_js, &terminal_panel_js).await;

        if !nav.has_next && nav.is_terminal {
            reached_last_panel = true;
            break;
        }
        if !nav.has_next {
            // Neither a next control nor the wizard's terminal panel was
            // recognised -- fail closed (stop, don't submit) rather than
            // guess. Diagnostic mirrors `guide_bridge_not_detected`'s own
            // pattern above: the selectors in `wizard_js` (and a driver's
            // own terminal-panel check) are a best guess (see this
            // module's own doc), so a miss here needs enough detail to
            // correct them without a second live run.
            let diagnostic = page
                .evaluate_string(wizard_js::NAVIGATION_DIAGNOSTIC)
                .await
                .unwrap_or_default();
            findings.push(Finding::warning(
                "wizard_navigation_not_found",
                format!(
                    "panel {panel_index}: no visible \"next\" control, and this driver's own \
                     terminal panel ({}) was not recognised either; stopping. page diagnostic: \
                     {diagnostic}",
                    driver.terminal_signal_label()
                ),
            ));
            break;
        }

        match advance_panel(page, &click_next_js, wizard_js::PANEL_FINGERPRINT).await {
            PanelAdvance::Advanced { .. } => {}
            other => {
                if let Some(message) =
                    other.failure_message(&format!("panel {panel_index}"), NavDirection::Next)
                {
                    findings.push(Finding::warning("panel_advance_failed", message));
                }
                break;
            }
        }
    }

    for name in fill.keys() {
        if !filled.contains(name) {
            findings.push(Finding::warning(
                "fill_failed",
                format!("could not set the value of field {name:?}"),
            ));
        }
    }

    if !should_submit {
        return;
    }
    if !reached_last_panel {
        findings.push(Finding::error(
            "wizard_incomplete",
            "submit was requested but the wizard's last panel was never confirmed reached; \
             refusing to submit from a non-final panel"
                .to_owned(),
        ));
        return;
    }

    submit_and_capture_artefact(
        page, session, profile, driver, package, containers, blobs, steps, artefacts, findings,
    )
    .await;
}

/// Attempts to set every not-yet-`filled` field on whatever panel is
/// currently visible. A field not on this panel simply fails here -- that
/// is expected, not an error; only a name still missing from `filled`
/// after the whole wizard walk is a real `fill_failed` finding (pushed
/// once, by the caller).
///
/// **This used to evaluate `<name>.value = <value>` as a bare global**, on
/// the reading of `specs/aem/aem-xml-spec.md` §15.2 that a field's own name is a global
/// carrying `.value`. That holds inside the rule editor's own script scope
/// -- which is where the spec's examples live -- but not in the page's
/// global scope, which is what a CDP `evaluate` runs in. Confirmed live:
/// every single `fill` entry against a real AEM form came back
/// `fill_failed`, so the argument had never once set a value, and any
/// wizard walked with it was walking an empty form.
///
/// What works instead is [`set_control`], the same setter
/// `crate::interactive::set` uses.
async fn fill_current_panel(
    page: &browser::PageHandle,
    fill: &BTreeMap<String, Value>,
    filled: &mut BTreeSet<String>,
) {
    for (name, value) in fill {
        if filled.contains(name) {
            continue;
        }
        if set_control(page, FieldRef::Name(name), value).await.set {
            filled.insert(name.clone());
        }
    }
}

/// How [`set_control`] finds the control it sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldRef<'a> {
    /// A SOM expression, as `verify_controls` reports it -- exact, and the
    /// only way to reach one instance of a repeated panel.
    Som(&'a str),
    /// A guide node name, matched first-come across the whole form --
    /// `verify_run`'s `fill` keys, which predate SOM addressing.
    Name(&'a str),
}

/// Which strategy [`set_control`] actually used to set a value --
/// `crate::interactive::set` reports this back to the caller so an agent
/// can tell "this went through the model, and so a summary component will
/// see it" from "this only reached the widget".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetVia {
    Model,
    Widget,
}

/// [`wizard_js::fill_field`]'s parsed report.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SetReport {
    pub set: bool,
    pub via: Option<SetVia>,
    /// The error the form's runtime raised while showing a value its model
    /// had already taken; see [`wizard_js::fill_field`].
    pub view_error: Option<String>,
}

fn parse_set_report(raw: &str) -> SetReport {
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return SetReport::default();
    };
    let set = parsed.get("set").and_then(Value::as_bool).unwrap_or(false);
    let via = match parsed.get("via").and_then(Value::as_str) {
        Some("model") => Some(SetVia::Model),
        Some("widget") => Some(SetVia::Widget),
        _ => None,
    };
    let view_error = parsed
        .get("view_error")
        .and_then(Value::as_str)
        .map(str::to_owned);
    SetReport {
        set,
        via,
        view_error,
    }
}

/// Sets one field the way a person would ([`wizard_js::fill_field`]),
/// model-first with a DOM-widget fallback, and reports which strategy
/// actually worked. The one setter [`fill_current_panel`] (`verify_run`'s
/// own `fill`) and `crate::interactive::set` (`verify_set`) both call, so
/// there is exactly one place that knows how to address a field by name.
/// A CDP evaluation failure (an unreachable page, say) is reported the
/// same as the field simply not being found -- `SetReport::default()`,
/// `set: false` -- since either way nothing was set.
pub(crate) async fn set_control(
    page: &browser::PageHandle,
    target: FieldRef<'_>,
    value: &Value,
) -> SetReport {
    let raw = page
        .evaluate_string(&wizard_js::fill_field(target, value))
        .await
        .unwrap_or_default();
    parse_set_report(&raw)
}

/// Turns [`wizard_js::AUTO_FILL_REQUIRED`]'s JSON report into the finding
/// that records it, or `None` when it filled nothing (the common case for
/// a panel with no required fields -- saying so every time would bury the
/// panels where it did act).
///
/// A `Warning`, not a silent note: a run that only got through the wizard
/// because the verifier invented values is a materially weaker result than
/// one walked with real data, and whoever reads the report needs to see
/// which fields those were rather than infer it.
fn auto_fill_finding(panel_index: u32, raw: &str) -> Option<Finding> {
    let report: Value = serde_json::from_str(raw).ok()?;
    let filled = report.get("filled")?.as_array()?;
    let skipped = report.get("skipped").and_then(Value::as_array);

    if filled.is_empty() && skipped.is_none_or(|s| s.is_empty()) {
        return None;
    }

    let describe = |entry: &Value, value_key: &str| {
        format!(
            "{}={}",
            entry.get("name").and_then(Value::as_str).unwrap_or("?"),
            entry.get(value_key).and_then(Value::as_str).unwrap_or("?")
        )
    };

    let mut message = format!(
        "panel {panel_index}: filled {} required field(s) the caller did not supply, with \
         placeholder values, so the wizard would advance: [{}]",
        filled.len(),
        filled
            .iter()
            .map(|entry| describe(entry, "value"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(skipped) = skipped.filter(|s| !s.is_empty()) {
        message.push_str(&format!(
            "; could not fill [{}]",
            skipped
                .iter()
                .map(|entry| describe(entry, "reason"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Some(Finding::warning("auto_filled", message))
}

/// Screenshots the currently visible panel and pushes it as
/// `Step{name: "panel-<panel_index>"}`. Never called for `panel_index ==
/// 1` -- `execute` already took that screenshot before any fill/navigation
/// happened, and this function must not retake it (`steps[0].name ==
/// "form"` is an invariant nothing downstream has to special-case).
pub(crate) async fn screenshot_panel(
    page: &browser::PageHandle,
    blobs: &BlobStore,
    panel_index: u32,
    steps: &mut Vec<Step>,
    findings: &mut Vec<Finding>,
) {
    let screenshot = match page.screenshot_full_page().await {
        Ok(bytes) => bytes,
        Err(err) => {
            findings.push(Finding::warning(
                "panel_screenshot_failed",
                format!("panel {panel_index}: {err}"),
            ));
            return;
        }
    };
    match blobs.put(&screenshot, "image/png", "png") {
        Ok(blob) => steps.push(Step {
            name: format!("panel-{panel_index}"),
            screenshot: BlobDescriptor::from(&blob),
            console_errors: page.console_errors(),
            failed_requests: page.failed_requests(),
            dialogs: page.dialogs(),
        }),
        Err(err) => findings.push(Finding::warning(
            "panel_screenshot_failed",
            storage_failed(&format!("storing panel {panel_index}'s screenshot"), err).to_string(),
        )),
    }
}

/// What one poll of the driver's own navigation signals found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NavState {
    pub has_next: bool,
    pub is_terminal: bool,
}

/// One poll of the navigation signals plus the panel fingerprint, so
/// [`settled`] can tell a stable panel from one still rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NavSample {
    pub nav: NavState,
    pub fingerprint: String,
}

/// `true` iff `current` can be reported: it carries a signal (a next
/// control or the terminal panel) and the poll before it saw exactly the
/// same thing. Reporting the first poll with any signal was wrong: right
/// after an advance, a driver whose next control stays in the DOM until
/// the terminal panel renders (UBS's summary) reports `has_next: true`
/// for a moment, and the agent then calls `verify_next` on the last panel.
pub(crate) fn settled(previous: Option<&NavSample>, current: &NavSample) -> bool {
    (current.nav.has_next || current.nav.is_terminal) && previous == Some(current)
}

/// Polls the driver's own `has_next_js`/`terminal_panel_js` until two
/// consecutive polls agree ([`settled`]), scrolling to the bottom before
/// each check (see [`wizard_js::SCROLL_TO_BOTTOM`]) -- insurance against a
/// panel that gates its own forward-navigation on being scrolled to its end
/// and against layout-settling races on a heavy panel. Bounded by
/// `PANEL_ADVANCE_POLL_ATTEMPTS` (same "how long can one panel take to
/// settle" budget as confirming an advance); the last poll is reported as
/// is when nothing settled.
pub(crate) async fn wait_for_navigation_controls(
    page: &browser::PageHandle,
    has_next_js: &str,
    terminal_panel_js: &str,
) -> NavState {
    let mut previous: Option<NavSample> = None;
    for attempt in 0..PANEL_ADVANCE_POLL_ATTEMPTS {
        let _ = page.evaluate_bool(wizard_js::SCROLL_TO_BOTTOM).await;
        let current = NavSample {
            nav: NavState {
                has_next: page.evaluate_bool(has_next_js).await.unwrap_or(false),
                is_terminal: page.evaluate_bool(terminal_panel_js).await.unwrap_or(false),
            },
            fingerprint: page
                .evaluate_string(wizard_js::PANEL_FINGERPRINT)
                .await
                .unwrap_or_default(),
        };
        if settled(previous.as_ref(), &current) || attempt == PANEL_ADVANCE_POLL_ATTEMPTS - 1 {
            return current.nav;
        }
        previous = Some(current);
        page.wait(READINESS_POLL_INTERVAL).await;
    }
    unreachable!("the last attempt always returns")
}

/// Polls [`wizard_js::PANEL_FINGERPRINT`] until it differs from `before`,
/// bounded by `PANEL_ADVANCE_POLL_ATTEMPTS`, returning the new fingerprint
/// -- the before/after diff that proves a "next"/"previous" click actually
/// changed the visible panel, since a click firing without error is not
/// proof of that on its own (this module's own doc: `specs/aem/aem-xml-spec.md` §15.1's
/// "Step Completion" validation can block navigation even after a
/// successful click).
async fn wait_for_panel_advance(
    page: &browser::PageHandle,
    fingerprint_js: &str,
    before: &str,
) -> Option<String> {
    for _ in 0..PANEL_ADVANCE_POLL_ATTEMPTS {
        let current = page
            .evaluate_string(fingerprint_js)
            .await
            .unwrap_or_default();
        if current != before {
            return Some(current);
        }
        page.wait(READINESS_POLL_INTERVAL).await;
    }
    None
}

/// Which way a panel-navigation click was meant to move, so a failure
/// message names the control that was actually tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavDirection {
    Next,
    Prev,
    Goto,
}

impl NavDirection {
    /// What was attempted, as the failure messages phrase it.
    fn attempt(self) -> &'static str {
        match self {
            NavDirection::Next => "clicked \"next\"",
            NavDirection::Prev => "clicked \"previous\"",
            NavDirection::Goto => "jumped to it",
        }
    }

    fn missing(self) -> &'static str {
        match self {
            NavDirection::Next => "no \"next\" control could be clicked",
            NavDirection::Prev => "no \"previous\" control could be clicked",
            NavDirection::Goto => "the panel could not be focused",
        }
    }
}

/// One entry of `guideBridge.validate`'s error list, as
/// [`wizard_js::validate`] normalises it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

/// Parses [`wizard_js::validate`]'s output. `Err` carries why the list
/// could not be read (validate threw, or the output was not the expected
/// JSON), which the caller reports instead of an empty list so a missing
/// diagnosis is never mistaken for "nothing failed".
pub(crate) fn parse_validation_errors(raw: &str) -> Result<Vec<ValidationError>, String> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Output {
        Errors(Vec<ValidationError>),
        Threw { error: String },
    }
    match serde_json::from_str::<Output>(raw) {
        Ok(Output::Errors(errors)) => Ok(errors),
        Ok(Output::Threw { error }) => Err(format!("guideBridge.validate threw: {error}")),
        Err(err) => Err(format!("unreadable validation result {raw:?}: {err}")),
    }
}

/// `"N error(s): field: message; ..."` -- how every refusal names the
/// fields validation rejected. Pure.
pub(crate) fn describe_validation_errors(errors: &[ValidationError]) -> String {
    format!(
        "{} error(s): {}",
        errors.len(),
        errors
            .iter()
            .map(|error| format!("{}: {}", error.field, error.message))
            .collect::<Vec<_>>()
            .join("; ")
    )
}

/// Runs `guideBridge.validate` over `scope` (a panel's SOM, or the whole
/// form when `None`) and reads back what it rejected. Validating also
/// shows the form's own error styling on those fields, as it would for a
/// visitor. `Err` says why the result could not be read.
pub(crate) async fn validate(
    page: &browser::PageHandle,
    scope: Option<&str>,
) -> Result<Vec<ValidationError>, String> {
    match page.evaluate_string(&wizard_js::validate(scope)).await {
        Ok(raw) => parse_validation_errors(&raw),
        Err(err) => Err(format!("evaluating guideBridge.validate failed: {err}")),
    }
}

/// What clicking a panel-navigation control ([`wizard_js::CLICK_NEXT`] or
/// [`wizard_js::CLICK_PREV`]) accomplished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PanelAdvance {
    /// The panel fingerprint changed from `from` to `to`.
    Advanced { from: String, to: String },
    /// No usable control matched the click expression at all.
    NothingToClick,
    /// A control was clicked but the fingerprint never changed within
    /// `PANEL_ADVANCE_POLL_ATTEMPTS`. `errors` is what
    /// `guideBridge.validate` reported for the panel afterwards.
    DidNotChange {
        before: String,
        errors: Result<Vec<ValidationError>, String>,
    },
}

impl PanelAdvance {
    /// `None` for [`Self::Advanced`] -- nothing went wrong. The wording for
    /// the two ways a panel can fail to move, kept in one place so
    /// `crate::interactive`'s `verify_next`/`verify_prev` report the same
    /// words a `verify_run` finding does. `panel_label` names the panel for
    /// a human (`"panel 3"` for a `verify_run` finding).
    pub(crate) fn failure_message(
        &self,
        panel_label: &str,
        direction: NavDirection,
    ) -> Option<String> {
        match self {
            PanelAdvance::Advanced { .. } => None,
            PanelAdvance::NothingToClick => Some(format!("{panel_label}: {}", direction.missing())),
            PanelAdvance::DidNotChange { before, errors } => {
                let cause = match errors {
                    Ok(errors) if errors.is_empty() => {
                        "guideBridge.validate reports no errors on this panel, so a custom \
                         navigation script is the likely blocker"
                            .to_owned()
                    }
                    Ok(errors) => format!(
                        "it is blocked by the form's validation, {}",
                        describe_validation_errors(errors)
                    ),
                    Err(why) => format!(
                        "a validation script may be blocking navigation, but its errors \
                         could not be read ({why})"
                    ),
                };
                Some(format!(
                    "{panel_label}: {} but the visible panel never changed \
                     within {PANEL_ADVANCE_POLL_ATTEMPTS} attempts (fingerprint stayed \
                     {before:?}); {cause}",
                    direction.attempt()
                ))
            }
        }
    }
}

/// Clicks `click_js` (either [`wizard_js::CLICK_NEXT`] or
/// [`wizard_js::CLICK_PREV`], or a driver's own) and confirms the panel
/// actually moved: before fingerprint, click, wait for it to change. When
/// it does not change, the panel is validated so the result names what
/// blocked it. Shared by `walk_wizard_and_maybe_submit` and
/// `crate::interactive::next`/`prev`/`goto`. `fingerprint_js` is what is
/// expected to change: [`wizard_js::PANEL_FINGERPRINT`] for a wizard step,
/// [`wizard_js::container_fingerprint`] for a jump inside one container.
pub(crate) async fn advance_panel(
    page: &browser::PageHandle,
    click_js: &str,
    fingerprint_js: &str,
) -> PanelAdvance {
    let before = page
        .evaluate_string(fingerprint_js)
        .await
        .unwrap_or_default();
    if !page.evaluate_bool(click_js).await.unwrap_or(false) {
        return PanelAdvance::NothingToClick;
    }
    match wait_for_panel_advance(page, fingerprint_js, &before).await {
        Some(to) => PanelAdvance::Advanced { from: before, to },
        None => {
            let panel_som = Some(before.as_str()).filter(|som| !som.is_empty());
            let errors = validate(page, panel_som).await;
            PanelAdvance::DidNotChange { before, errors }
        }
    }
}

/// Submits (via `driver.submit_js()`) and captures whatever the profile's
/// submit artefact strategy says to expect. A submit call that did not
/// report success is a finding, not a hard failure -- the screenshots and
/// package artefact already collected are still worth returning. Called
/// from [`walk_wizard_and_maybe_submit`] once it has confirmed the
/// wizard's last panel was actually reached, and from
/// `crate::interactive::submit` once it has confirmed the same thing
/// through [`NavState::is_terminal`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn submit_and_capture_artefact(
    page: &browser::PageHandle,
    session: &BrowserSession,
    profile: &Profile,
    driver: &dyn FormDriver,
    package: &PackageInspection,
    containers: &Containers,
    blobs: &BlobStore,
    steps: &mut Vec<Step>,
    artefacts: &mut Vec<Artefact>,
    findings: &mut Vec<Finding>,
) {
    if let Some(checks_js) = driver.last_panel_checks_js(package) {
        let raw = page.evaluate_string(&checks_js).await.unwrap_or_default();
        if let Ok(entries) = serde_json::from_str::<Vec<Value>>(&raw) {
            for entry in entries {
                let kind = entry
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("driver_check")
                    .to_owned();
                let message = entry
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                findings.push(Finding::warning(kind, message));
            }
        }
    }

    let failed_before_submit = page.failed_requests().len();
    let submitted = page
        .evaluate_bool(&driver.submit_js())
        .await
        .unwrap_or(false);
    if !submitted {
        findings.push(Finding::warning(
            "submit_failed",
            driver.submit_failed_message().to_owned(),
        ));
        return;
    }

    let after_submit = match page.screenshot_full_page().await {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            findings.push(Finding::warning(
                "post_submit_screenshot_failed",
                err.to_string(),
            ));
            None
        }
    };
    if let Some(bytes) = after_submit {
        match blobs.put(&bytes, "image/png", "png") {
            Ok(blob) => steps.push(Step {
                name: "after-submit".to_owned(),
                screenshot: BlobDescriptor::from(&blob),
                console_errors: page.console_errors(),
                failed_requests: page.failed_requests(),
                dialogs: page.dialogs(),
            }),
            Err(err) => findings.push(Finding::warning(
                "post_submit_screenshot_failed",
                storage_failed("storing the post-submit screenshot", err).to_string(),
            )),
        }
    }

    match &profile.submit {
        SubmitArtefact::Download => match session.wait_for_download(SUBMIT_TIMEOUT).await {
            Ok(guid) => {
                read_downloaded_pdf(containers, &guid, blobs, artefacts, findings).await;
            }
            Err(err) => {
                let failed = page.failed_requests();
                let refusal = page
                    .evaluate_string(wizard_js::SUBMIT_ERROR)
                    .await
                    .unwrap_or_default();
                findings.push(Finding::error(
                    ErrorKind::NoDownload.as_str(),
                    no_download_message(
                        &err.to_string(),
                        failed.get(failed_before_submit..).unwrap_or_default(),
                        Some(refusal.as_str()).filter(|refusal| !refusal.is_empty()),
                    ),
                ))
            }
        },
        SubmitArtefact::DocumentOfRecord => {
            let aem_client = AemClient::new(
                &containers.aem_base_url,
                &profile.aem_user,
                &profile.aem_password,
            );
            match aem_client
                .fetch_dor_pdf(&package.summary.form_jcr_path)
                .await
            {
                Ok(bytes) => {
                    keep_pdf_artefact(&bytes, "Document of Record", blobs, artefacts, findings)
                }
                Err(err) => findings.push(Finding::error(
                    ErrorKind::NoDownload.as_str(),
                    err.to_string(),
                )),
            }
        }
        SubmitArtefact::None => {}
    }
}

/// The `no_download` finding's message: why no download arrived, with the
/// requests that failed after the submit started -- the difference between
/// "the server refused the submission" (confirmed live on `AF_AABF`: its
/// submit answered HTTP 500 when AEM's native PDF/A conversion failed) and
/// "the form's own submit script never posted" (no request failed). Pure.
pub(crate) fn no_download_message(
    reason: &str,
    failed_after_submit: &[FailedRequest],
    submit_refusal: Option<&str>,
) -> String {
    if let Some(refusal) = submit_refusal {
        return format!("{reason}; the server refused the submission: {refusal}");
    }
    if failed_after_submit.is_empty() {
        return format!(
            "{reason}; no request failed after the submit, so the form's own submit script \
             most likely stopped before posting (see the after-submit screenshot)"
        );
    }
    let failed = failed_after_submit
        .iter()
        .map(|request| match request.status {
            Some(status) => format!("{} answered HTTP {status}", request.url),
            None => format!("{} failed to load", request.url),
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("{reason}; after the submit: {failed}")
}

/// Copies the download named `guid` out of the session's Chromium
/// container, stores it as a blob, and removes it there. Removing matters:
/// the container lives as long as the session, so leaving each file behind
/// would accumulate one per call.
async fn read_downloaded_pdf(
    containers: &Containers,
    guid: &str,
    blobs: &BlobStore,
    artefacts: &mut Vec<Artefact>,
    findings: &mut Vec<Finding>,
) {
    let path = format!("{}/{guid}", crate::instance::CHROMIUM_DOWNLOAD_DIR);
    let copied = match DockerLifecycle::connect().await {
        Ok(docker) => {
            let copied = docker.copy_file_out(&containers.chromium.id, &path).await;
            if copied.is_ok()
                && let Err(err) = docker
                    .exec(
                        &containers.chromium.id,
                        vec!["rm".to_owned(), "-f".to_owned(), path.clone()],
                    )
                    .await
            {
                log::warn!(
                    "{}: could not remove {path} from the Chromium container: {err}",
                    crate::LOG_PREFIX
                );
            }
            copied.map_err(|err| err.to_string())
        }
        Err(err) => Err(err.to_string()),
    };
    let bytes = match copied {
        Ok(bytes) => bytes,
        Err(err) => {
            findings.push(Finding::error(
                ErrorKind::DownloadNotPdf.as_str(),
                format!("could not read the downloaded file {path}: {err}"),
            ));
            return;
        }
    };
    if !bytes.starts_with(b"%PDF") {
        findings.push(Finding::error(
            ErrorKind::DownloadNotPdf.as_str(),
            format!("the downloaded file {path} is not a PDF"),
        ));
        return;
    }
    keep_pdf_artefact(
        &bytes,
        "the form's submit download",
        blobs,
        artefacts,
        findings,
    );
}

/// Stores a PDF the submit produced as a `Download` artefact labelled
/// `label`, after judging it: a PDF that draws nothing on any page
/// (`pdf_content::blank_pdf`) is an `ErrorKind::DownloadBlank` finding. The
/// blank PDF is kept all the same, since its bytes are what a person
/// diagnoses the render from. One place for both the download and the
/// Document of Record path, so the two agree on what blank means.
fn keep_pdf_artefact(
    bytes: &[u8],
    label: &str,
    blobs: &BlobStore,
    artefacts: &mut Vec<Artefact>,
    findings: &mut Vec<Finding>,
) {
    match pdf_content::blank_pdf(bytes) {
        Ok(false) => {}
        Ok(true) => {
            let pages = pdf_content::page_count(bytes).unwrap_or(0);
            findings.push(Finding::error(
                ErrorKind::DownloadBlank.as_str(),
                format!(
                    "{label} is a PDF of {pages} page(s) that shows no text or image on any of \
                     them: the render came back empty (the AEM container's error.log has the cause)"
                ),
            ));
        }
        Err(message) => findings.push(Finding::error(
            ErrorKind::DownloadNotPdf.as_str(),
            format!("{label}: {message}"),
        )),
    }
    let blob: BlobRef = match blobs.put(bytes, "application/pdf", "pdf") {
        Ok(blob) => blob,
        Err(err) => {
            findings.push(Finding::error(
                ErrorKind::StorageFailed.as_str(),
                storage_failed("storing the downloaded form", err).to_string(),
            ));
            return;
        }
    };
    artefacts.push(Artefact {
        kind: ArtefactKind::Download,
        label: "the form's submit download".to_owned(),
        blob: BlobDescriptor::from(&blob),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A download that is a valid PDF of blank pages is the one shape the
    /// `%PDF` check waves through, and the one a person otherwise finds
    /// only by opening the review: it must be an error finding, with the
    /// bytes kept for the diagnosis.
    #[test]
    fn a_blank_download_is_an_error_finding_and_is_still_kept() {
        let dir =
            std::env::temp_dir().join(format!("u2s-aem-verify-blank-{}", uuid::Uuid::new_v4()));
        let blobs = BlobStore::in_folder(&dir);
        let mut artefacts = Vec::new();
        let mut findings = Vec::new();

        keep_pdf_artefact(
            &pdf_content::fixtures::blank_page(),
            "the form's submit download",
            &blobs,
            &mut artefacts,
            &mut findings,
        );
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].kind, ErrorKind::DownloadBlank.as_str());
        assert!(
            findings[0].message.contains("1 page(s)"),
            "{}",
            findings[0].message
        );
        assert_eq!(artefacts.len(), 1, "the blank PDF is kept as evidence");

        keep_pdf_artefact(
            &pdf_content::fixtures::page_showing(wizard_js::PLACEHOLDER),
            "the form's submit download",
            &blobs,
            &mut artefacts,
            &mut findings,
        );
        assert_eq!(
            findings.len(),
            1,
            "a PDF that shows text adds no finding: {findings:?}"
        );
        assert_eq!(artefacts.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// No Docker or AEM is assumed reachable for this crate's own test
    /// run -- see the crate's `#[ignore]`d live tests for the
    /// Docker/AEM/Chromium-backed coverage. `dry_run` is the one entry
    /// point that must work without either, so it is what this module
    /// tests directly.
    #[tokio::test]
    async fn dry_run_reports_an_invalid_package_without_touching_docker() {
        let report = dry_run(b"not a zip").await;
        assert!(report.dry_run);
        assert!(report.steps.is_empty());
        assert!(report.artefacts.is_empty());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == ErrorKind::PackageInvalid.as_str())
        );
    }

    /// The bug this replaced: `<name>.value = ...` as a bare global, which
    /// silently set nothing on a real form. The widget's own `name`
    /// attribute is what AEM actually renders.
    #[test]
    fn fill_field_targets_the_widget_by_its_name_attribute() {
        let js = wizard_js::fill_field(FieldRef::Name("TXT_Cognome"), &serde_json::json!("Rossi"));
        assert!(
            js.contains(r#"querySelector('[name="' + name + '"]')"#),
            "{js}"
        );
        assert!(js.contains(r#""TXT_Cognome""#), "{js}");
        assert!(
            !js.contains("TXT_Cognome.value ="),
            "must not fall back to assigning a same-named global: {js}"
        );
    }

    #[test]
    fn fill_field_cannot_be_broken_out_of_by_a_quote_in_either_argument() {
        let js = wizard_js::fill_field(
            FieldRef::Name("evil\"; alert(1); //"),
            &serde_json::json!("v\"; alert(2)"),
        );
        // Both arguments arrive as JSON literals, so each embedded quote is
        // backslash-escaped and stays inside the string it was put in,
        // rather than closing it and starting new statements.
        assert!(
            js.contains(r#"var name = "evil\"; alert(1); //";"#),
            "the name must land as one escaped literal: {js}"
        );
        assert!(
            js.contains(r#"var value = "v\"; alert(2)";"#),
            "the value must land as one escaped literal: {js}"
        );
    }

    /// A DOM-only value is discarded when AEM re-renders the panel from its
    /// model, and never reaches a summary component, so the model node is
    /// what has to be set.
    #[test]
    fn fill_field_sets_the_model_node_before_falling_back_to_the_widget() {
        let js = wizard_js::fill_field(FieldRef::Name("TXT_Cognome"), &serde_json::json!("Rossi"));
        let model = js.find("guideBridge.resolveNode").expect("model strategy");
        let widget = js.find("document.querySelector").expect("widget fallback");
        assert!(model < widget, "the model must be tried first: {js}");
        assert!(js.contains("item.name === name"), "{js}");
    }

    /// A wizard keeps every panel in the DOM, so the widget fallback must
    /// not "succeed" against an unbound widget on a panel the walk has not
    /// reached -- the value would be silently lost.
    #[test]
    fn the_widget_fallback_refuses_a_field_that_is_not_currently_visible() {
        let js = wizard_js::fill_field(FieldRef::Name("TXT_Cognome"), &serde_json::json!("Rossi"));
        assert!(js.contains("offsetParent"), "{js}");
        assert!(js.contains("guideFieldNode"), "{js}");
    }

    /// A SOM reaches one exact model node, including a field inside the
    /// second instance of a repeated panel, which a name walk never could
    /// (every instance repeats the name and the walk stops at the first).
    #[test]
    fn fill_field_by_som_resolves_that_exact_node_and_never_walks_by_name() {
        let som = "guide[0].guide1[0].guideRootPanel[0].p[0].Row[1].Amount[0]";
        let js = wizard_js::fill_field(FieldRef::Som(som), &serde_json::json!("12"));
        assert!(js.contains(&format!("var som = \"{som}\";")), "{js}");
        assert!(js.contains("var name = null;"), "{js}");
        let resolve = js
            .find("guideBridge.resolveNode(som)")
            .expect("direct resolve");
        let walk = js
            .find("item.name === name")
            .expect("name walk stays for names");
        assert!(
            resolve < walk,
            "a SOM must be handled before the name walk: {js}"
        );
    }

    #[test]
    fn fill_field_by_som_cannot_be_broken_out_of_by_a_quote() {
        let js = wizard_js::fill_field(
            FieldRef::Som("p[0].x\"); alert(1); //"),
            &serde_json::json!(1),
        );
        assert!(
            js.contains(r#"var som = "p[0].x\"); alert(1); //";"#),
            "{js}"
        );
    }

    #[test]
    fn fill_field_passes_a_non_string_value_through_as_its_json_literal() {
        let js = wizard_js::fill_field(FieldRef::Name("count"), &serde_json::json!(42));
        assert!(js.contains("var value = 42;"), "{js}");
    }

    /// `fill_current_panel`/`set_control` distinguish "set on the model"
    /// from "set on the widget" so a caller can tell which happened --
    /// this is the report `wizard_js::fill_field` must produce for each
    /// branch, and `parse_set_report` must read back correctly.
    #[test]
    fn fill_field_reports_which_strategy_set_the_value() {
        let js = wizard_js::fill_field(FieldRef::Name("TXT_Cognome"), &serde_json::json!("Rossi"));
        assert!(
            js.contains("JSON.stringify({set: true, via: 'model'})"),
            "{js}"
        );
        assert!(
            js.contains("JSON.stringify({set: true, via: 'widget'})"),
            "{js}"
        );
        assert!(js.contains("JSON.stringify({set: false})"), "{js}");
        assert!(
            js.contains("target.value == value"),
            "a model that took the value counts: {js}"
        );
    }

    #[test]
    fn parse_set_report_reads_back_every_shape_fill_field_can_produce() {
        assert_eq!(
            parse_set_report(r#"{"set": true, "via": "model"}"#),
            SetReport {
                set: true,
                via: Some(SetVia::Model),
                view_error: None
            }
        );
        assert_eq!(
            parse_set_report(r#"{"set": true, "via": "widget"}"#),
            SetReport {
                set: true,
                via: Some(SetVia::Widget),
                view_error: None
            }
        );
        assert_eq!(
            parse_set_report(r#"{"set": false}"#),
            SetReport {
                set: false,
                via: None,
                view_error: None
            }
        );
        assert_eq!(
            parse_set_report(r#"{"set": true, "via": "model", "view_error": "TypeError: x"}"#),
            SetReport {
                set: true,
                via: Some(SetVia::Model),
                view_error: Some("TypeError: x".to_owned())
            }
        );
        assert_eq!(parse_set_report("not json"), SetReport::default());
        assert_eq!(parse_set_report(""), SetReport::default());
    }

    /// What `placeholderCandidates(kind, clauses)` lists first, run as the
    /// browser runs it.
    fn first_candidate(kind: &str, clauses: &[&str]) -> String {
        let source = format!(
            "{}\nplaceholderCandidates({}, {})[0];",
            placeholder_js!(),
            serde_json::to_string(kind).unwrap(),
            serde_json::to_string(clauses).unwrap()
        );
        let mut context = boa_engine::Context::default();
        let value = context
            .eval(boa_engine::Source::from_bytes(source.as_bytes()))
            .unwrap_or_else(|e| panic!("the placeholder logic runs: {e}"));
        value
            .to_string(&mut context)
            .unwrap()
            .to_std_string_escaped()
    }

    /// A placeholder the field's own pattern refuses blocks the wizard: a
    /// letter's name fields allow no digits, and its date picker reads its
    /// picture clause.
    #[test]
    fn the_placeholder_satisfies_the_fields_own_pattern() {
        assert_eq!(first_candidate("text", &[]), wizard_js::PLACEHOLDER);
        assert!(!wizard_js::PLACEHOLDER.chars().any(|c| c.is_ascii_digit()));
        assert_eq!(
            first_candidate("text", &["^([^0-9]*)$"]),
            wizard_js::PLACEHOLDER
        );
        assert_eq!(first_candidate("text", &["^[A-Z]+$"]), "X");
        assert_eq!(
            first_candidate("datepicker", &["date{D.M.YYYY}"]),
            "1.1.2000"
        );
        assert_eq!(
            first_candidate("datepicker", &["date{DD.MM.YYYY}", "date{DD.MM.YYYY}"]),
            "01.01.2000"
        );
        assert_eq!(
            first_candidate("datepicker", &["date{YYYY-MM-DD}"]),
            "2000-01-01"
        );
        assert_eq!(first_candidate("datepicker", &[]), "1.1.2000");
        assert_eq!(first_candidate("date", &[]), "2000-01-01");
        assert_eq!(
            first_candidate("email", &["^not(a regex"]),
            "verify@example.com"
        );
    }

    #[test]
    fn a_panel_needing_no_auto_fill_produces_no_finding() {
        assert!(auto_fill_finding(1, r#"{"filled":[],"skipped":[]}"#).is_none());
    }

    #[test]
    fn auto_filled_fields_are_named_and_valued_in_the_finding() {
        let finding = auto_fill_finding(
            3,
            r#"{"filled":[{"name":"RB_Group","kind":"radio","value":"1"},
                         {"name":"txtName","kind":"text","value":"UtwoS"}],
                "skipped":[]}"#,
        )
        .expect("a panel that was auto-filled must say so");

        assert_eq!(finding.kind, "auto_filled");
        // A caller auditing the run has to be able to see which fields
        // carried invented data, not just how many.
        assert!(
            finding.message.contains("RB_Group=1"),
            "{}",
            finding.message
        );
        assert!(
            finding.message.contains("txtName=UtwoS"),
            "{}",
            finding.message
        );
        assert!(finding.message.contains("panel 3"), "{}", finding.message);
    }

    #[test]
    fn a_widget_the_auto_fill_could_not_handle_is_reported_too() {
        let finding = auto_fill_finding(
            2,
            r#"{"filled":[],"skipped":[{"name":"weirdWidget","reason":"no non-empty option"}]}"#,
        )
        .expect("a panel with an unfillable required widget must say so");

        assert!(
            finding.message.contains("weirdWidget=no non-empty option"),
            "{}",
            finding.message
        );
    }

    #[test]
    fn a_malformed_auto_fill_report_is_ignored_rather_than_panicking() {
        assert!(auto_fill_finding(1, "not json").is_none());
        assert!(auto_fill_finding(1, r#"{"unexpected":true}"#).is_none());
    }

    #[test]
    fn encode_query_percent_encodes_only_what_needs_it() {
        assert_eq!(encode_query(&[]), "");
        assert_eq!(
            encode_query(&[("wcmmode".to_owned(), "disabled".to_owned())]),
            "wcmmode=disabled"
        );
        assert_eq!(
            encode_query(&[
                ("entity".to_owned(), "033".to_owned()),
                ("lang".to_owned(), "it".to_owned()),
            ]),
            "entity=033&lang=it"
        );
        assert_eq!(
            encode_query(&[("formTitle".to_owned(), "a b&c".to_owned())]),
            "formTitle=a%20b%26c"
        );
    }

    /// The wording of the two ways a panel can fail to move on, pinned so
    /// an edit cannot silently reword a finding a live-test doc comment
    /// quotes, and so a failed "previous" never blames the "next" control.
    #[test]
    fn panel_advance_failure_messages_name_the_control_that_was_tried() {
        assert_eq!(
            PanelAdvance::NothingToClick.failure_message("panel 3", NavDirection::Next),
            Some("panel 3: no \"next\" control could be clicked".to_owned())
        );
        assert_eq!(
            PanelAdvance::NothingToClick.failure_message("this panel", NavDirection::Prev),
            Some("this panel: no \"previous\" control could be clicked".to_owned())
        );
        let message = PanelAdvance::DidNotChange {
            before: "guide[0].panel1[0]".to_owned(),
            errors: Ok(vec![]),
        }
        .failure_message("panel 3", NavDirection::Prev)
        .unwrap();
        assert!(
            message.contains("clicked \"previous\" but the visible panel never changed"),
            "{message}"
        );
        assert!(message.contains("guide[0].panel1[0]"), "{message}");
        assert_eq!(
            PanelAdvance::Advanced {
                from: "a".to_owned(),
                to: "b".to_owned()
            }
            .failure_message("panel 3", NavDirection::Next),
            None
        );
    }

    /// A blocked advance names every field validate reported, so the agent
    /// knows what to fill instead of guessing at "a validation script".
    #[test]
    fn a_blocked_advance_names_the_fields_validate_reported() {
        let message = PanelAdvance::DidNotChange {
            before: "p".to_owned(),
            errors: Ok(vec![
                ValidationError {
                    field: "guide[0].guide1[0].guideRootPanel[0].p1[0].lastName[0]".to_owned(),
                    message: "This field is required".to_owned(),
                },
                ValidationError {
                    field: "guide[0].guide1[0].guideRootPanel[0].p1[0].zip[0]".to_owned(),
                    message: "Invalid".to_owned(),
                },
            ]),
        }
        .failure_message("this panel", NavDirection::Next)
        .unwrap();
        assert!(
            message.contains("blocked by the form's validation, 2 error(s): "),
            "{message}"
        );
        assert!(
            message.contains("lastName[0]: This field is required"),
            "{message}"
        );
        assert!(message.contains("zip[0]: Invalid"), "{message}");

        let unreadable = PanelAdvance::DidNotChange {
            before: "p".to_owned(),
            errors: Err("boom".to_owned()),
        }
        .failure_message("this panel", NavDirection::Next)
        .unwrap();
        assert!(
            unreadable.contains("could not be read (boom)"),
            "{unreadable}"
        );
    }

    #[test]
    fn validation_errors_parse_from_the_list_and_the_threw_shape() {
        assert_eq!(
            parse_validation_errors(r#"[{"field":"a[0]","message":"required"}]"#),
            Ok(vec![ValidationError {
                field: "a[0]".to_owned(),
                message: "required".to_owned()
            }])
        );
        assert_eq!(parse_validation_errors("[]"), Ok(vec![]));
        assert_eq!(
            parse_validation_errors(r#"{"error":"TypeError: x"}"#),
            Err("guideBridge.validate threw: TypeError: x".to_owned())
        );
        assert!(parse_validation_errors("not json").is_err());
    }

    fn sample(has_next: bool, is_terminal: bool, fingerprint: &str) -> NavSample {
        NavSample {
            nav: NavState {
                has_next,
                is_terminal,
            },
            fingerprint: fingerprint.to_owned(),
        }
    }

    /// The navigation state is only reported once two polls agree: a
    /// single `has_next: true` right after an advance is exactly the
    /// transient that made the agent click next on the last panel.
    #[test]
    fn navigation_state_settles_only_on_two_identical_polls_with_a_signal() {
        let first = sample(true, false, "p2");
        assert!(!settled(None, &first));
        assert!(settled(Some(&first), &sample(true, false, "p2")));
        assert!(!settled(Some(&first), &sample(false, true, "p2")));
        assert!(!settled(Some(&first), &sample(true, false, "p3")));
        let nothing = sample(false, false, "p2");
        assert!(!settled(Some(&nothing), &nothing.clone()));
    }

    /// The toolbar button is clicked first (it runs the form's own click
    /// script); without one the wizard steps through GuideBridge, the call
    /// its own step arrows make. The arrows still count as "there is a
    /// next step" for the terminal-panel check, but are never clicked.
    #[test]
    fn navigation_clicks_the_toolbar_button_then_steps_through_guide_bridge() {
        let next = wizard_js::CLICK_NEXT;
        assert!(
            next.find("button.moveNext").unwrap() < next.find("setFocus").unwrap(),
            "{next}"
        );
        assert!(
            next.contains("'nextItem'") && next.contains("hasNextItem"),
            "{next}"
        );
        let prev = wizard_js::CLICK_PREV;
        assert!(
            prev.find("'button.movePrev'").unwrap() < prev.find("button.movePrevious").unwrap()
        );
        assert!(
            prev.contains("'prevItem'") && prev.contains("isFirstItem"),
            "{prev}"
        );
        for js in [next, prev] {
            assert!(!js.contains("wizard-nav"), "{js}");
            assert!(js.contains("!el.disabled"), "{js}");
            assert!(js.contains(crate::root_panel!()), "{js}");
        }
        assert!(wizard_js::HAS_VISIBLE_NEXT.contains(".wizard-nav-next"));
    }

    #[test]
    fn goto_panel_focuses_that_panel_and_escapes_its_som() {
        let js = wizard_js::goto_panel("g[0].p\"); alert(1); //[0]");
        assert!(js.contains("guideBridge.setFocus(som)"), "{js}");
        assert!(js.ends_with(r#"})("g[0].p\"); alert(1); //[0]")"#), "{js}");
    }

    #[test]
    fn validate_js_interpolates_the_panel_som_as_a_json_literal() {
        let js = wizard_js::validate(Some("guide[0].p'\"x[0]"));
        assert!(js.ends_with(r#"})("guide[0].p'\"x[0]")"#), "{js}");
        assert!(wizard_js::validate(None).ends_with("})(null)"));
    }

    #[test]
    fn a_missing_download_names_the_requests_that_failed_after_submit() {
        let refused = no_download_message(
            "no download completed within 90s",
            &[FailedRequest {
                url: "http://aem/x/guideContainer.af.submit.jsp".to_owned(),
                status: Some(500),
            }],
            None,
        );
        assert!(
            refused.contains("guideContainer.af.submit.jsp answered HTTP 500"),
            "{refused}"
        );
        let silent = no_download_message("no download completed within 90s", &[], None);
        assert!(silent.contains("stopped before posting"), "{silent}");
        let refused = no_download_message(
            "no download completed within 90s",
            &[],
            Some("Error in reading XSD from node: /content/dam/x.xsd"),
        );
        assert!(
            refused.contains("the server refused the submission: Error in reading XSD"),
            "{refused}"
        );
    }
}
