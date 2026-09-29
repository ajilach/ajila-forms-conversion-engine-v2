//! The verification flow: reuse (or boot, on first use) the profile's
//! persistent AEM + Chromium session (`crate::session`), install the
//! package under test, screenshot the rendered form, optionally fill and
//! submit it, capture whatever the submit produced, and uninstall the
//! package again -- leaving the session itself running for the next call.
//!
//! **Multi-step wizards.** A wizard-shaped Adaptive Form (rendered panels
//! advanced one at a time via a "next" button, `specs/AEM.md` §6.11's
//! `fd/af/components/nextitemnav`) is walked by [`walk_wizard_and_maybe_submit`]:
//! fill what's fillable on the current panel, screenshot it, look for a
//! visible "next" control, click it and confirm the panel actually
//! changed (`specs/AEM.md` §15.1 documents a panel's own "Step Completion"
//! validation can block navigation even after a successful click) or stop
//! if none is found. A single-panel form simply exits after one iteration,
//! so this needs no separate "is this a wizard" branch -- see
//! `crate::package_check::PackageSummary::looks_like_a_wizard` for an
//! offline signal that exists purely for a summary/dry-run, never to gate
//! this loop. The panel-navigation selectors in [`wizard_js`] are a best
//! guess from `specs/AEM.md`'s JCR-content-model documentation, not a
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
//! (`specs/AEM.md` §15.3) rather than a guessed CSS selector where one is
//! available: the global `guideBridge` object existing is the readiness
//! signal, and a field's own `name` is a global carrying `.value` (§15.2)
//! -- both are AEM's own contract for Adaptive Forms scripting, not
//! something this crate invented. GuideBridge has no documented panel-
//! navigation call, though (`specs/AEM.md` §15.3 lists `submit`,
//! `validate`, `reset`, `setFocus`, `on` -- no `gotoNextPanel`/`navigate`),
//! so panel advancement is the one place this module clicks a DOM element
//! instead.
//!
//! **Not handled**: a field living inside a repeatable subform (one that
//! needs `panel.instanceManager.addInstance()`, `specs/AEM.md` §15.2,
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
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;
use u2s_blob::{BlobRef, BlobStore};
use u2s_verify_core::browser::{self, BrowserSession};
use u2s_verify_core::docker::{DockerLifecycle, wait_for_http};
use u2s_verify_core::types::{
    Artefact, ArtefactKind, BlobDescriptor, ErrorKind, Finding, Step, VerifyError, VerifyReport,
};

use crate::aem_client::AemClient;
use crate::driver::FormDriver;
use crate::package_check::{self, PackageInspection};
use crate::profile::{Profile, SubmitArtefact};
use crate::session::{self, Instances, SessionPool};

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
pub mod wizard_js {
    /// The next/submit toolbar buttons are server-rendered with a stable
    /// `name` attribute matching `specs/AEM.md` §6.11's
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
    /// "Next").
    pub const HAS_VISIBLE_NEXT: &str = r#"(function() {
        var els = document.querySelectorAll('button.moveNext, .wizard-nav-next');
        for (var i = 0; i < els.length; i++) {
            if (els[i].offsetParent !== null) { return true; }
        }
        return false;
    })()"#;

    /// Clicks the first visible next-panel control (see
    /// [`HAS_VISIBLE_NEXT`] for why two different selectors are checked),
    /// returning whether one was found and clicked. Plain `.click()`
    /// first; if a real AEM instance's button binding turns out to need
    /// real mouse event semantics instead, swap this one expression for a
    /// `dispatchEvent(new MouseEvent('click', {bubbles:true,
    /// cancelable:true, view:window}))` call on the same element.
    pub const CLICK_NEXT: &str = r#"(function() {
        var els = document.querySelectorAll('button.moveNext, .wizard-nav-next');
        for (var i = 0; i < els.length; i++) {
            if (els[i].offsetParent !== null) {
                els[i].click();
                return true;
            }
        }
        return false;
    })()"#;

    /// Clicks the first visible previous-panel control. The generic
    /// counterpart to [`CLICK_NEXT`] -- `crate::interactive::prev` is the
    /// only caller `verify_run` never needed one before it (a one-shot
    /// wizard walk only ever moves forward).
    pub const CLICK_PREV: &str = r#"(function() {
        var els = document.querySelectorAll('button.movePrevious, .wizard-nav-prev');
        for (var i = 0; i < els.length; i++) {
            if (els[i].offsetParent !== null) {
                els[i].click();
                return true;
            }
        }
        return false;
    })()"#;

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
    pub fn fill_field(name: &str, value: &serde_json::Value) -> String {
        format!(
            r#"(function() {{
    var name = {name};
    var value = {value};

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
    // uses, matching on `.name` rather than `.id`. The root SOM is AEM's
    // own default naming for an adaptive form's container and root panel.
    try {{
        if (typeof guideBridge !== 'undefined') {{
            var root = guideBridge.resolveNode('guide[0].guide1[0].guideRootPanel[0]');
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
            name = serde_json::Value::String(name.to_owned()),
            value = value,
        )
    }

    /// Fills every *visible, required, still-empty* widget on the current
    /// panel with a type-appropriate placeholder, and reports back as JSON
    /// exactly what it touched:
    /// `{"filled":[{"name","kind","value"}],"skipped":[{"name","reason"}]}`.
    ///
    /// Why this is needed at all: an Adaptive Forms wizard validates the
    /// current panel before it will advance (`specs/AEM.md` §15.1's "Step
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
    pub const AUTO_FILL_REQUIRED: &str = r#"(function() {
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
                    var value;
                    if (type === 'date') { value = '2000-01-01'; }
                    else if (type === 'email') { value = 'verify@example.com'; }
                    else if (type === 'number' || type === 'tel') { value = '1'; }
                    else if (type === 'url') { value = 'https://example.com'; }
                    else { value = 'U2S'; }
                    el.value = value;
                    announce(el);
                    if (!el.value) { skipped.push({ name: name, reason: 'value did not stick' }); continue; }
                    filled.push({ name: name, kind: type, value: value });
                }
            } catch (e) {
                skipped.push({ name: name, reason: String(e) });
            }
        }
        return JSON.stringify({ filled: filled, skipped: skipped });
    })()"#;

    /// Registers a listener for GuideBridge's own `elementNavigationChanged`
    /// event -- fired by the stock AEM Forms wizard runtime's default
    /// next/previous-navigator handler whenever the active panel changes,
    /// `evnt.newText` carrying the newly active panel's SOM expression --
    /// and stashes the latest value on `window.__u2sPanelNav`. Idempotent
    /// (safe to evaluate more than once: re-registering the same handler
    /// only means duplicate assignments of the same value, never a
    /// double-advance). Call once, right after `guideBridge` is detected
    /// ready and before the wizard walk starts; [`PANEL_FINGERPRINT`]
    /// reads what this records.
    pub const OBSERVE_PANEL_NAVIGATION: &str = r#"(function() {
        if (!window.__u2sPanelNav) { window.__u2sPanelNav = ''; }
        window.guideBridge.on('elementNavigationChanged', function(name, evnt) {
            window.__u2sPanelNav = evnt && evnt.newText ? evnt.newText : '';
        });
        return true;
    })()"#;

    /// Evaluates to the latest panel SOM expression
    /// [`OBSERVE_PANEL_NAVIGATION`]'s listener has recorded -- empty
    /// string until the first navigation event fires. Used as a
    /// before/after diff around a "next" click: a click firing without
    /// error is not proof the panel advanced (`specs/AEM.md` §15.1's
    /// "Step Completion" validation can block navigation even then), so
    /// the diff -- a real navigation event actually firing -- is the
    /// actual advance signal, not a DOM visibility guess.
    pub const PANEL_FINGERPRINT: &str = "window.__u2sPanelNav || ''";

    /// The submit button is server-rendered `name="submit"`
    /// (`specs/AEM.md` §6.11) but, like the next button, loses that
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

/// Runs the whole flow for real: reuse or boot `session_id`'s session,
/// install the package under test, render, screenshot, optionally fill and
/// submit, uninstall the package again. `session_id` is what keeps two
/// concurrent callers from interfering (AGENTS.md's per-agent-session
/// requirement) -- each gets its own AEM+Chromium session, never sharing
/// one; see `crate::session`'s module doc. That session's containers are
/// never torn down here -- but whatever this call installed is always
/// cleaned up (unless `keep_on_failure` and this call failed, so the
/// failure state stays inspectable), regardless of how `execute` finished.
///
/// Refuses outright, before installing anything, if `session_id` already
/// has a form open for interaction (`crate::interactive`): the two paths
/// share the session's one `installed_package_path` slot, so a `verify_run`
/// racing an open interactive session would either install over it or have
/// its own package uninstalled out from under it by whichever side finishes
/// first. The caller's fix is simply to `verify_close` first, or use a
/// different `session_id`.
///
/// `driver` supplies the format-specific behaviour this crate itself never
/// names -- see this module's own doc and `crate::driver::FormDriver`.
pub async fn run(
    profile: &Profile,
    driver: &dyn FormDriver,
    blobs: &BlobStore,
    session: &SessionPool,
    session_id: &str,
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

    let (mut guard, session_findings) = session.ensure(session_id, &docker, profile).await?;
    let state = guard
        .as_mut()
        .expect("ensure always leaves Some on success");

    if let Some(form) = &state.open_form {
        return Err(VerifyError::new(
            ErrorKind::FormOpen,
            format!(
                "form {:?} is open on this session; call verify_close first, or use a \
                 different session_id for verify_run",
                form.handle
            ),
        ));
    }

    // Defensive: a prior call's uninstall may have crashed before running.
    // Clearing whatever it left installed here means this run's install
    // never collides with stale nodes, even though the persistent instance
    // is meant to already be clean between calls.
    uninstall_if_installed(
        profile,
        &state.instances,
        &mut state.installed_package_path,
        "a previous call's leftover package",
    )
    .await;

    let outcome = execute(
        profile,
        driver,
        blobs,
        &state.instances,
        &mut state.installed_package_path,
        package,
        &request,
        session_findings,
        started,
    )
    .await;

    if !(profile.keep_on_failure && outcome.is_err()) {
        uninstall_if_installed(
            profile,
            &state.instances,
            &mut state.installed_package_path,
            "this run's package",
        )
        .await;
    }

    outcome
}

/// Uninstalls whatever `installed_package_path` currently names, if
/// anything, logging (never failing) on error -- the "uninstall, log a
/// warning, clear the slot" sequence [`run`] needs twice (defensively
/// before installing its own package, and again once it finishes) and
/// [`crate::interactive::close`] needs a third time. `context` names the
/// package only for the log line; it never reaches a finding or a tool
/// result.
pub(crate) async fn uninstall_if_installed(
    profile: &Profile,
    instances: &Instances,
    installed_package_path: &mut Option<String>,
    context: &str,
) {
    let Some(path) = installed_package_path.take() else {
        return;
    };
    let aem_client = AemClient::new(
        &instances.aem_base_url,
        &profile.aem_user,
        &profile.aem_password,
    );
    if let Err(err) = aem_client.uninstall(&path).await {
        log::warn!(
            "{}: could not uninstall {context}: {err}",
            crate::LOG_PREFIX
        );
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

/// Installs `package_bytes` on `instances`' AEM, opens its form in a fresh
/// page in `instances`' Chromium, and waits for `guideBridge` -- everything
/// [`execute`] and `crate::interactive::open` both need before they can
/// start filling or walking a form. `installed_package_path` is written
/// the moment install succeeds, before anything below it can fail, so a
/// caller that only gets an `Err` back from a later step still knows to
/// uninstall this package (`crate::interactive::open` and [`run`] both
/// rely on this).
///
/// Registers [`wizard_js::OBSERVE_PANEL_NAVIGATION`] once `guideBridge` is
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
    instances: &Instances,
    installed_package_path: &mut Option<String>,
    package: PackageInspection,
    package_bytes: Vec<u8>,
) -> Result<OpenedForm, VerifyError> {
    let aem_base_url = &instances.aem_base_url;
    let cdp_url = &instances.cdp_url;
    let summary = &package.summary;

    let aem_client = AemClient::new(aem_base_url, &profile.aem_user, &profile.aem_password);
    let install_path = aem_client
        .upload_and_install(package_bytes, &summary.form_name)
        .await
        .map_err(|err| VerifyError::new(ErrorKind::InstallFailed, err.to_string()))?;
    // Recorded immediately, before anything below can fail: even if
    // rendering fails, this call's own package must still be uninstalled
    // afterward, not left on the persistent instance.
    *installed_package_path = Some(install_path);

    // The query string a real visitor's URL would carry -- `wcmmode=disabled`
    // for the generic driver, plus whatever a format-specific driver needs
    // to open its own form at all (UBS's `mandator`/`afAcceptLang`, derived
    // from the package's own metadata, say). An `Err` here means the
    // driver could not determine how to open this package's form -- a
    // package-level problem, not a Docker/AEM one.
    let query = driver
        .form_url_query(&package)
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
        instances.aem_network_url, summary.form_jcr_path
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
        .enable_downloads(session::CHROMIUM_DOWNLOAD_DIR)
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
    } else {
        // Idempotent (see the constant's own doc) -- safe to call every
        // time a form is opened, once `guideBridge` is confirmed present.
        let _ = page.evaluate_bool(wizard_js::OBSERVE_PANEL_NAVIGATION).await;
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
    instances: &Instances,
    installed_package_path: &mut Option<String>,
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
        instances,
        installed_package_path,
        package,
        request.package_bytes.clone(),
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
            instances,
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
/// `specs/AEM.md`'s own scripting-model doc (§15.3) is what makes this the
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
    instances: &Instances,
    blobs: &BlobStore,
    fill: &BTreeMap<String, Value>,
    should_submit: bool,
    steps: &mut Vec<Step>,
    artefacts: &mut Vec<Artefact>,
    findings: &mut Vec<Finding>,
) {
    let mut filled: BTreeSet<String> = BTreeSet::new();
    let mut reached_last_panel = false;

    // `wizard_js::OBSERVE_PANEL_NAVIGATION` is registered once, by
    // `open_live_form`, right after `guideBridge` is confirmed ready --
    // not here. See that function's own doc for why.

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

        match advance_panel(page, &click_next_js).await {
            PanelAdvance::Advanced { .. } => {}
            other => {
                if let Some(message) = other.failure_message(&format!("panel {panel_index}")) {
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
        page, session, profile, driver, package, instances, blobs, steps, artefacts, findings,
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
/// the reading of `specs/AEM.md` §15.2 that a field's own name is a global
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
        if set_control(page, name, value).await.set {
            filled.insert(name.clone());
        }
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SetReport {
    pub set: bool,
    pub via: Option<SetVia>,
}

fn parse_set_report(raw: &str) -> SetReport {
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return SetReport::default();
    };
    let set = parsed
        .get("set")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let via = match parsed.get("via").and_then(Value::as_str) {
        Some("model") => Some(SetVia::Model),
        Some("widget") => Some(SetVia::Widget),
        _ => None,
    };
    SetReport { set, via }
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
    name: &str,
    value: &Value,
) -> SetReport {
    let raw = page
        .evaluate_string(&wizard_js::fill_field(name, value))
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
            entry
                .get(value_key)
                .and_then(Value::as_str)
                .unwrap_or("?")
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

/// Polls the driver's own `has_next_js`/`terminal_panel_js` a few times,
/// scrolling to the bottom before each check (see
/// [`wizard_js::SCROLL_TO_BOTTOM`]), rather than a single check --
/// defensive insurance against a panel that gates its own
/// forward-navigation on being scrolled to its end (a real pattern for
/// long legal-disclosure panels, though not what `AAOV_033` turned out to
/// need live: see this module's own doc for the terminal-panel case that
/// motivated adding this), and against ordinary layout-settling races on
/// a heavy panel. Bounded by `PANEL_ADVANCE_POLL_ATTEMPTS` (reused rather
/// than a new constant: same "how long can one panel take to settle"
/// budget as confirming an advance actually happened).
pub(crate) async fn wait_for_navigation_controls(
    page: &browser::PageHandle,
    has_next_js: &str,
    terminal_panel_js: &str,
) -> NavState {
    for attempt in 0..PANEL_ADVANCE_POLL_ATTEMPTS {
        let _ = page.evaluate_bool(wizard_js::SCROLL_TO_BOTTOM).await;
        let has_next = page.evaluate_bool(has_next_js).await.unwrap_or(false);
        let is_terminal = page.evaluate_bool(terminal_panel_js).await.unwrap_or(false);
        if has_next || is_terminal || attempt == PANEL_ADVANCE_POLL_ATTEMPTS - 1 {
            return NavState {
                has_next,
                is_terminal,
            };
        }
        page.wait(READINESS_POLL_INTERVAL).await;
    }
    NavState {
        has_next: false,
        is_terminal: false,
    }
}

/// Polls [`wizard_js::PANEL_FINGERPRINT`] until it differs from `before`,
/// bounded by `PANEL_ADVANCE_POLL_ATTEMPTS`, returning the new fingerprint
/// -- the before/after diff that proves a "next"/"previous" click actually
/// changed the visible panel, since a click firing without error is not
/// proof of that on its own (this module's own doc: `specs/AEM.md` §15.1's
/// "Step Completion" validation can block navigation even after a
/// successful click).
async fn wait_for_panel_advance(page: &browser::PageHandle, before: &str) -> Option<String> {
    for _ in 0..PANEL_ADVANCE_POLL_ATTEMPTS {
        let current = page
            .evaluate_string(wizard_js::PANEL_FINGERPRINT)
            .await
            .unwrap_or_default();
        if current != before {
            return Some(current);
        }
        page.wait(READINESS_POLL_INTERVAL).await;
    }
    None
}

/// What clicking a panel-navigation control ([`wizard_js::CLICK_NEXT`] or
/// [`wizard_js::CLICK_PREV`]) accomplished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PanelAdvance {
    /// The panel fingerprint changed from `from` to `to`.
    Advanced { from: String, to: String },
    /// No visible control matched the click expression at all.
    NothingToClick,
    /// A control was clicked but the fingerprint never changed within
    /// `PANEL_ADVANCE_POLL_ATTEMPTS`.
    DidNotChange { before: String },
}

impl PanelAdvance {
    /// `None` for [`Self::Advanced`] -- nothing went wrong. The exact
    /// wording `walk_wizard_and_maybe_submit` has always used for the two
    /// ways a panel can fail to move on, kept in one place so
    /// `crate::interactive`'s `verify_next`/`verify_prev` report the same
    /// words a `verify_run` finding always has. `panel_label` names the
    /// panel for a human reading the message (`"panel 3"` for a
    /// `verify_run` finding; a caller with no panel index to give can pass
    /// anything descriptive instead).
    pub(crate) fn failure_message(&self, panel_label: &str) -> Option<String> {
        match self {
            PanelAdvance::Advanced { .. } => None,
            PanelAdvance::NothingToClick => {
                Some(format!("{panel_label}: no \"next\" control could be clicked"))
            }
            PanelAdvance::DidNotChange { before } => Some(format!(
                "{panel_label}: clicked \"next\" but the visible panel never changed within \
                 {PANEL_ADVANCE_POLL_ATTEMPTS} attempts (fingerprint stayed {before:?}); a \
                 validation script may be blocking navigation (specs/AEM.md §15.1's \"Step \
                 Completion\")"
            )),
        }
    }
}

/// Clicks `click_js` (either [`wizard_js::CLICK_NEXT`] or
/// [`wizard_js::CLICK_PREV`]) and confirms the panel actually moved,
/// exactly the "before fingerprint, click, wait for it to change" sequence
/// `walk_wizard_and_maybe_submit` has always used for "next" -- extracted
/// so `crate::interactive::next`/`prev` share it rather than reimplementing
/// the poll.
pub(crate) async fn advance_panel(page: &browser::PageHandle, click_js: &str) -> PanelAdvance {
    let before = page
        .evaluate_string(wizard_js::PANEL_FINGERPRINT)
        .await
        .unwrap_or_default();
    if !page.evaluate_bool(click_js).await.unwrap_or(false) {
        return PanelAdvance::NothingToClick;
    }
    match wait_for_panel_advance(page, &before).await {
        Some(to) => PanelAdvance::Advanced { from: before, to },
        None => PanelAdvance::DidNotChange { before },
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
    instances: &Instances,
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
                let path = browser::download_path(&instances.downloads_dir, &guid);
                read_downloaded_pdf(&path, blobs, artefacts, findings);
            }
            Err(err) => findings.push(Finding::error(
                ErrorKind::NoDownload.as_str(),
                err.to_string(),
            )),
        },
        SubmitArtefact::DocumentOfRecord => {
            let aem_client = AemClient::new(
                &instances.aem_base_url,
                &profile.aem_user,
                &profile.aem_password,
            );
            match aem_client
                .fetch_dor_pdf(&package.summary.form_jcr_path)
                .await
            {
                Ok(bytes) => match blobs.put(&bytes, "application/pdf", "pdf") {
                    Ok(blob) => artefacts.push(Artefact {
                        kind: ArtefactKind::Download,
                        label: "Document of Record".to_owned(),
                        blob: BlobDescriptor::from(&blob),
                    }),
                    Err(err) => findings.push(Finding::error(
                        ErrorKind::StorageFailed.as_str(),
                        storage_failed("storing the Document of Record", err).to_string(),
                    )),
                },
                Err(err) => findings.push(Finding::error(
                    ErrorKind::NoDownload.as_str(),
                    err.to_string(),
                )),
            }
        }
        SubmitArtefact::None => {}
    }
}

/// Reads, stores as a blob, and removes the file at `path` -- the last part
/// matters now that `path` lives in the session's shared, persistent
/// downloads directory (`crate::session::Instances::downloads_dir`) rather
/// than a per-run directory the caller deletes wholesale afterward; leaving
/// it behind would otherwise accumulate one file per call for as long as
/// the session stays up.
fn read_downloaded_pdf(
    path: &PathBuf,
    blobs: &BlobStore,
    artefacts: &mut Vec<Artefact>,
    findings: &mut Vec<Finding>,
) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            findings.push(Finding::error(
                ErrorKind::DownloadNotPdf.as_str(),
                format!(
                    "could not read the downloaded file at {}: {err}",
                    path.display()
                ),
            ));
            return;
        }
    };
    let _ = std::fs::remove_file(path);
    if !bytes.starts_with(b"%PDF") {
        findings.push(Finding::error(
            ErrorKind::DownloadNotPdf.as_str(),
            format!("the downloaded file at {} is not a PDF", path.display()),
        ));
        return;
    }
    let blob: BlobRef = match blobs.put(&bytes, "application/pdf", "pdf") {
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
        let js = wizard_js::fill_field("TXT_Cognome", &serde_json::json!("Rossi"));
        assert!(js.contains(r#"querySelector('[name="' + name + '"]')"#), "{js}");
        assert!(js.contains(r#""TXT_Cognome""#), "{js}");
        assert!(
            !js.contains("TXT_Cognome.value ="),
            "must not fall back to assigning a same-named global: {js}"
        );
    }

    #[test]
    fn fill_field_cannot_be_broken_out_of_by_a_quote_in_either_argument() {
        let js = wizard_js::fill_field("evil\"; alert(1); //", &serde_json::json!("v\"; alert(2)"));
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
        let js = wizard_js::fill_field("TXT_Cognome", &serde_json::json!("Rossi"));
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
        let js = wizard_js::fill_field("TXT_Cognome", &serde_json::json!("Rossi"));
        assert!(js.contains("offsetParent"), "{js}");
        assert!(js.contains("guideFieldNode"), "{js}");
    }

    #[test]
    fn fill_field_passes_a_non_string_value_through_as_its_json_literal() {
        let js = wizard_js::fill_field("count", &serde_json::json!(42));
        assert!(js.contains("var value = 42;"), "{js}");
    }

    /// `fill_current_panel`/`set_control` distinguish "set on the model"
    /// from "set on the widget" so a caller can tell which happened --
    /// this is the report `wizard_js::fill_field` must produce for each
    /// branch, and `parse_set_report` must read back correctly.
    #[test]
    fn fill_field_reports_which_strategy_set_the_value() {
        let js = wizard_js::fill_field("TXT_Cognome", &serde_json::json!("Rossi"));
        assert!(
            js.contains("JSON.stringify({set: true, via: 'model'})"),
            "{js}"
        );
        assert!(
            js.contains("JSON.stringify({set: true, via: 'widget'})"),
            "{js}"
        );
        assert!(js.contains("JSON.stringify({set: false})"), "{js}");
    }

    #[test]
    fn parse_set_report_reads_back_every_shape_fill_field_can_produce() {
        assert_eq!(
            parse_set_report(r#"{"set": true, "via": "model"}"#),
            SetReport {
                set: true,
                via: Some(SetVia::Model)
            }
        );
        assert_eq!(
            parse_set_report(r#"{"set": true, "via": "widget"}"#),
            SetReport {
                set: true,
                via: Some(SetVia::Widget)
            }
        );
        assert_eq!(
            parse_set_report(r#"{"set": false}"#),
            SetReport {
                set: false,
                via: None
            }
        );
        assert_eq!(parse_set_report("not json"), SetReport::default());
        assert_eq!(parse_set_report(""), SetReport::default());
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
                         {"name":"txtName","kind":"text","value":"U2S"}],
                "skipped":[]}"#,
        )
        .expect("a panel that was auto-filled must say so");

        assert_eq!(finding.kind, "auto_filled");
        // A caller auditing the run has to be able to see which fields
        // carried invented data, not just how many.
        assert!(finding.message.contains("RB_Group=1"), "{}", finding.message);
        assert!(finding.message.contains("txtName=U2S"), "{}", finding.message);
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

    /// The exact wording `walk_wizard_and_maybe_submit` has always used
    /// for the two ways a panel can fail to move on -- pinned here so a
    /// future edit to `advance_panel`'s caller cannot silently reword a
    /// finding a live-test doc comment already quotes.
    #[test]
    fn panel_advance_failure_messages_match_the_walks_own_established_wording() {
        assert_eq!(
            PanelAdvance::NothingToClick.failure_message("panel 3"),
            Some("panel 3: no \"next\" control could be clicked".to_owned())
        );
        let message = PanelAdvance::DidNotChange {
            before: "guide[0].panel1[0]".to_owned(),
        }
        .failure_message("panel 3");
        assert!(
            message
                .as_deref()
                .unwrap()
                .contains("clicked \"next\" but the visible panel never changed"),
            "{message:?}"
        );
        assert!(message.unwrap().contains("guide[0].panel1[0]"));
        assert_eq!(
            PanelAdvance::Advanced {
                from: "a".to_owned(),
                to: "b".to_owned()
            }
            .failure_message("panel 3"),
            None
        );
    }
}
