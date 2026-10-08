//! MCP stdio server verifying an AEM Adaptive Forms package against a real
//! AEM instance running UBS's own `ajila-forms-ubs` platform.
//!
//! A UBS Adaptive Form needs behaviour the generic `u2s-aem-verify-mcp`
//! never has to know about (see `u2s_aem_verify_core`'s own module doc for
//! the `FormDriver` seam this crate plugs into):
//!
//! - **Opening the form at all.** A UBS form's server-side prefill/DoR
//!   metadata service (`FormMetadataService`) needs `mandator` and
//!   `afAcceptLang` URL parameters that resolve to a real entity the
//!   package's own metadata component declares -- otherwise it throws
//!   `FormMetadataException: No metadata information for mandator .`.
//!   `u2s_aem_ubs_verify_mcp::ubs_metadata` extracts that entity offline, from the package
//!   itself, before ever opening a browser.
//! - **Recognising the wizard's terminal panel.** This branch of
//!   `ajila-forms-ubs` hardcodes `window.forms.ubs.isFWB()` to `true`,
//!   which hides the toolbar's `submit` button entirely on the summary
//!   panel (the Forms WorkBench host is normally what triggers submit
//!   instead). So the generic driver's "a visible submit button" signal
//!   never fires; this crate's own signal is "the summary panel
//!   (`.summaryComponent`) is showing" (`u2s_aem_ubs_verify_mcp::ubs_js::IS_SUMMARY_PANEL`).
//! - **Submitting.** A raw `guideBridge.submit()` takes AEM's native XDP
//!   rendering path, whose 32-bit x86 native services (`XMLForm.exe`,
//!   `convertpdf.exe`) cannot run on this workspace's own ARM Docker
//!   image. UBS's own `window.forms.ubs.navigation.submit(...)` routine
//!   (`u2s_aem_ubs_verify_mcp::ubs_js::SUBMIT`) populates `summaryComponent` first, which
//!   is what makes `ajila-forms-ubs`'s `DorRenderingExecutor` route the
//!   submission through the Redacto rendering dependency instead.
//!
//! See `specs.rs` for the exact tool contract and
//! `u2s_aem_verify_core::flow` for the orchestration this binary's own
//! `u2s_aem_ubs_verify_mcp::driver::UbsDriver` plugs into.

use std::sync::Arc;

use u2s_aem_ubs_verify_mcp::driver::UbsDriver;
use u2s_aem_ubs_verify_mcp::specs;
use u2s_aem_verify_core::server::{ServerConfig, run_main};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ServerConfig {
        name: "u2s-aem-ubs-verify-mcp",
        version: env!("CARGO_PKG_VERSION"),
        expected_format: Some("aem-ubs"),
        driver: Arc::new(UbsDriver::from_env()),
        tool_specs: specs::tool_specs(),
        manifest: Box::new(specs::manifest),
        instructions: Box::new(|profile| {
            format!(
                "Verifies an encoded {} package against a real AEM instance running UBS's own \
                 `ajila-forms-ubs` platform: installs it on this profile's persistent AEM \
                 session (booted from this profile's prebuilt image on first use, then reused \
                 across calls), opens the form with the `mandator`/`afAcceptLang` URL \
                 parameters its own metadata component resolves to, walks its wizard, and -- \
                 unless told not to -- fills and submits it through UBS's own \
                 `window.forms.ubs.navigation.submit(...)` routine, capturing the Redacto \
                 rendering dependency's PDF as a browser download. Pass `session_id` (any \
                 stable string identifying the calling agent/run) so concurrent callers each \
                 get their own AEM instance rather than sharing and blocking on one; omit it to \
                 use a single shared default session. `verify_run` is side-effecting and \
                 offered to the Output Review Agent alone; `verify_status` and \
                 `verify_package_check` are plain reads. Read `u2s://manifest` for the exact \
                 `verify_run` contract.",
                profile.format
            )
        }),
    };
    run_main(config).await
}
