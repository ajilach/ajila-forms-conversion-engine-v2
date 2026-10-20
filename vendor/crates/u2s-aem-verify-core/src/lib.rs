//! The AEM verification runtime shared by every AEM-verifying MCP server:
//! reuse (or boot) a profile's persistent AEM + Chromium session, install a
//! FileVault package, walk a (possibly wizard-shaped) Adaptive Form,
//! optionally submit it, and capture whatever the submit produced.
//!
//! **Format-specific behaviour is a [`driver::FormDriver`], not a branch in
//! here.** How a form's URL query is built (a generic form needs none; a
//! UBS form needs `mandator`/`afAcceptLang`), how the wizard's terminal
//! panel is recognised (a visible submit button; or, for UBS, a summary
//! panel with no submit button at all), and how submit is actually
//! triggered (`guideBridge.submit()`; or UBS's own
//! `window.forms.ubs.navigation.submit(...)`) are all supplied by the
//! binary on top through that trait. This crate names AEM and FileVault
//! vocabulary, but never a specific customer's platform -- that split is
//! enforced by `tests/no_customer_terms.rs` -- the same "library has the
//! logic, binaries are thin" doctrine `u2s-mapper-aem` already gives the
//! two AEM *encoder* binaries (`u2s-aem-mcp`, `u2s-aem-ubs-mcp`), applied
//! here to the two AEM *verifier* binaries (`u2s-aem-verify-mcp`,
//! `u2s-aem-ubs-verify-mcp`).
//!
//! - [`aem_client`] -- the CRX Package Manager HTTP client.
//! - [`profile`] -- the one AEM profile a process serves, read from
//!   `U2S_AEM_VERIFY_*` environment.
//! - [`package_check`] -- offline FileVault package validation.
//! - [`instance`] -- the persistent per-`instance_id` AEM+Chromium instances.
//! - [`driver`] -- the [`driver::FormDriver`] seam and the generic
//!   implementation every non-format-specific binary uses.
//! - [`flow`] -- the verification flow itself: install, render, walk,
//!   submit, capture.
//! - [`interactive`] -- the one-step-at-a-time control tools
//!   (`verify_open`/`verify_controls`/`verify_set`/...) built on the same
//!   `flow` primitives `verify_run` uses.
//! - [`server`] -- the rmcp `ServerHandler` glue shared by every binary,
//!   parameterised by a [`server::ServerConfig`].

/// A JS expression evaluating to the Adaptive Form's root panel model
/// node, or `undefined` when the form has none. Every script that walks
/// the form starts here, so there is one place that knows how the root is
/// found: the guide container (`guideBridge.resolveNode('guide[0]')`)
/// carries it as `rootPanel` (confirmed live on `AF_AABF`), which does not
/// depend on the root panel's own name the way a hard-coded
/// `guide[0].guide1[0].guideRootPanel[0]` did. A macro rather than a
/// `const` so `concat!` can splice it into the `const` scripts.
#[macro_export]
macro_rules! root_panel {
    () => {
        "(window.guideBridge.resolveNode('guide[0]') || {}).rootPanel"
    };
}

/// A JS statement list that wraps the next `guideBridge.submit` call so
/// its `error` callback records what the server answered on
/// `window.__u2sSubmitError` (tags stripped, at most 400 characters)
/// before running the form's own handler. Every driver's submit script
/// starts with it, so a submission the server refused is reported even
/// when the refusal arrives with HTTP 200 (confirmed live on `BBPT_019`:
/// `internalsubmit.jsp` answered an error page with status 200 because the
/// form's XSD is missing, and only the error callback saw it).
#[macro_export]
macro_rules! capture_submit_error_js {
    () => {
        "window.__u2sSubmitError = ''; \
         (function() { var original = guideBridge.submit; \
         guideBridge.submit = function(options) { guideBridge.submit = original; \
         var wrapped = Object.assign({}, options || {}); var own = wrapped.error; \
         wrapped.error = function(result) { try { var data = result && result.data; \
         var text = data && data.responseJSON ? JSON.stringify(data.responseJSON) \
         : String((data && data.responseText) || JSON.stringify(result) || 'error'); \
         window.__u2sSubmitError = text.replace(/<[^>]*>/g, ' ').replace(/\\s+/g, ' ').trim().slice(0, 400); \
         } catch (e) { window.__u2sSubmitError = String(e); } \
         if (own) { return own.apply(this, arguments); } }; \
         return original.call(guideBridge, wrapped); }; })();"
    };
}

pub mod aem_client;
pub mod args;
pub mod driver;
pub mod flow;
pub mod interactive;
pub mod package_check;
pub mod profile;
pub mod server;
pub mod instance;
mod packages;
pub mod specs_shared;

/// Prefix every log line this crate's own modules emit -- one shared
/// string rather than each binary's own name, since a log line from this
/// crate is about the shared runtime (session boot, wizard walk)
/// regardless of which thin binary is hosting it right now.
pub const LOG_PREFIX: &str = "u2s-aem-verify-core";
