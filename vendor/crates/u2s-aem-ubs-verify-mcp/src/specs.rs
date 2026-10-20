//! The tool schemas and the manifest for the UBS-specific verifier.
//!
//! `manifest` takes the profile the same way the generic verifier's own
//! `specs.rs` does (so both binaries share the same `ServerConfig` shape),
//! but this binary's own `main.rs` passes `expected_format: Some("aem-ubs")`
//! to `u2s_aem_verify_core::server::run_main`, so `profile.format` is
//! always `"aem-ubs"` here in practice -- checked at startup, not assumed.

use serde_json::{Value, json};

use u2s_aem_verify_core::args::{LanguageArg, PackageCheckArgs, StatusArgs, VerifyRunArgs};
use u2s_aem_verify_core::profile::Profile;
use u2s_aem_verify_core::specs_shared::FORM_RENDER_FAILED_NOTE;
use u2s_schema::tool_input_schema;

pub const CONTRACT_VERSION: &str = "1.0.0";

pub fn tool_specs() -> Vec<Value> {
    let mut specs = vec![
        json!({
            "name": "verify_status",
            "description":
                "Reports this server's configured profile (the AEM and Chromium images, the \
                 submit artefact strategy, the Redacto rendering dependency UBS's own submit \
                 path calls out to), whether Docker and Redacto are currently reachable, and \
                 whether `instance_id`'s own persistent AEM instance is up and for how long (plus \
                 how many instances are active across every `instance_id`), and which sessions (open forms) it holds. Touches nothing -- \
                 safe to call at any time, including before ever running Docker, and is how a \
                 caller distinguishes \"this server is misconfigured\" from \"Docker is not \
                 running right now\".",
            "input_schema": tool_input_schema::<StatusArgs>()
        }),
        json!({
            "name": "verify_package_check",
            "description":
                "Validates a FileVault package offline: is it a well-formed ZIP, does its \
                 META-INF/vault/filter.xml resolve to exactly one form, what JCR path does it \
                 render at, and (UBS-specific) what mandator/language entities its own metadata \
                 component authors and which one this server would open the form as. Exactly \
                 one of `package` (a u2s blob handle) or `package_path` (a filesystem path, for \
                 conformance test vectors) must be given. Never touches Docker, AEM or a \
                 network -- this is the check `verify_run`'s own `dry_run: true` performs \
                 internally before anything with a side effect runs.",
            "input_schema": tool_input_schema::<PackageCheckArgs>()
        }),
        json!({
            "name": "verify_run",
            "description":
                format!(
                    "Installs the package on this profile's persistent AEM instance (booted from \
                     this profile's prebuilt image on first use, then reused across calls), opens \
                     the form with the `mandator`/`afAcceptLang` URL parameters this package's own \
                     metadata component resolves to (a UBS form otherwise fails server-side with \
                     \"No metadata information for mandator\"), walks its wizard panel by panel, \
                     and -- unless `submit: false` -- fills the fields named in `fill` and submits \
                     via UBS's own `window.forms.ubs.navigation.submit(...)` routine (not a raw \
                     `guideBridge.submit()`, which would take AEM's native XDP rendering path -- \
                     unavailable on this workspace's own ARM Docker image), capturing the Redacto \
                     rendering dependency's PDF as a browser download; the package is uninstalled \
                     again afterward so the instance stays clean for the next call. Every screenshot \
                     and artefact travels as a blob, never inline: a real run's images would not \
                     fit a tool result. `dry_run: true` validates the package and reports Docker \
                     reachability without starting anything. This tool is side-effecting: it \
                     installs software on an instance it does not tear down and, unless `dry_run` or \
                     `submit: false`, submits the rendered form. It is never offered to the \
                     Conversion Agent. {}",
                    FORM_RENDER_FAILED_NOTE
                ),
            "input_schema": tool_input_schema::<VerifyRunArgs<LanguageArg>>()
        }),
    ];
    specs.extend(u2s_aem_verify_core::specs_shared::tool_specs());
    specs
}

/// The `u2s://manifest` resource. `profile.format` is always `"aem-ubs"`
/// for this binary (`main.rs`'s `expected_format` check enforces it at
/// startup), but this still reads it from the profile rather than a
/// hardcoded literal so this function's shape matches the generic
/// verifier's own `specs::manifest` exactly.
pub fn manifest(profile: &Profile) -> Value {
    let mut tools = vec![
        json!({
            "tool": "verify_status",
            "role": "query",
            "scope": { "output_formats": [profile.format] }
        }),
        json!({
            "tool": "verify_package_check",
            "role": "query",
            "scope": { "output_formats": [profile.format] }
        }),
        json!({
            "tool": "verify_run",
            "role": "query",
            "side_effecting": true,
            "verify": "run",
            "scope": { "output_formats": [profile.format] }
        }),
    ];
    tools.extend(u2s_aem_verify_core::specs_shared::manifest_tools(
        &profile.format,
    ));

    let mut test_vectors = vec![
        json!({
            "tool": "verify_status",
            "args": {},
            "expect": { "structured": { "format": profile.format } }
        }),
        json!({
            "tool": "verify_package_check",
            "args": { "package_path": "$FIXTURES/AF_AABF.zip" },
            "expect": {
                "structured": {
                    "form_jcr_path": "/content/forms/af/afforms_germany_all/af_aa/AF_AABF",
                    "form_name": "AF_AABF"
                }
            }
        }),
        json!({
            "tool": "verify_run",
            "args": { "package_path": "$FIXTURES/AF_AABF.zip", "dry_run": true },
            "expect": { "structured": { "dry_run": true } }
        }),
    ];
    test_vectors.extend(u2s_aem_verify_core::specs_shared::test_vectors());

    json!({
        "contract": CONTRACT_VERSION,
        "server": {
            "name": "u2s-aem-ubs-verify-mcp",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "tools": tools,
        // No `format` module: the output format this profile verifies is
        // already owned by its own encoder server (`u2s-aem-ubs-mcp`).
        // Registering this server never registers a format.
        "format": null,
        "test_vectors": test_vectors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every optional argument refuses an empty value in the schema itself,
    /// the same check `u2s_mcp::conformance` runs against a live server.
    #[test]
    fn no_optional_argument_accepts_an_empty_value() {
        let found: Vec<String> = tool_specs()
            .iter()
            .flat_map(|spec| {
                u2s_mcp::conformance::empty_optionals(&spec["input_schema"])
                    .into_iter()
                    .map(|path| format!("{}{path}", spec["name"].as_str().unwrap_or("?")))
            })
            .collect();
        assert!(found.is_empty(), "{found:#?}");
    }

    fn profile() -> Profile {
        Profile {
            format: "aem-ubs".to_owned(),
            aem_image: "ajila.azurecr.io/aemforms-arm:6.5.17.0".to_owned(),
            registry_credentials: None,
            aem_user: "admin".to_owned(),
            aem_password: "admin".to_owned(),
            submit: u2s_aem_verify_core::profile::SubmitArtefact::Download,
            chromium_image: "chromedp/headless-shell:stable".to_owned(),
            platform: "linux/arm64".to_owned(),
            boot_timeout: std::time::Duration::from_secs(900),
            idle_timeout: std::time::Duration::from_secs(1800),
            keep_on_failure: false,
            redacto_url: Some(
                "http://host.docker.internal:18080/bin/redacto/summary/generatepdf".to_owned(),
            ),
            aem_data_volume: "u2s-aem-ubs-data".to_owned(),
            self_container: None,
            aem_container_port: 8080,
            max_inline_bytes: 1536 * 1024,
        }
    }

    #[test]
    fn there_are_fifteen_tools_scoped_to_aem_ubs() {
        let specs = tool_specs();
        // verify_status, verify_package_check, verify_run, plus the twelve
        // shared interactive tools (`u2s_aem_verify_core::specs_shared`).
        assert_eq!(specs.len(), 15);
        assert_eq!(specs[0]["name"], "verify_status");
        assert_eq!(specs[1]["name"], "verify_package_check");
        assert_eq!(specs[2]["name"], "verify_run");

        let manifest = manifest(&profile());
        assert!(manifest["format"].is_null());
        for tool in manifest["tools"].as_array().expect("array") {
            assert_eq!(tool["scope"]["output_formats"][0], "aem-ubs");
        }
    }

    #[test]
    fn only_verify_run_declares_the_verify_capability_and_side_effecting_matches_specs_shared() {
        let manifest = manifest(&profile());
        let tools = manifest["tools"].as_array().expect("array");
        assert_eq!(tools[0]["side_effecting"], Value::Null);
        assert_eq!(tools[1]["side_effecting"], Value::Null);
        assert_eq!(tools[2]["side_effecting"], json!(true));
        assert_eq!(tools[2]["verify"], json!("run"));
        for tool in &tools[3..] {
            assert_eq!(tool["verify"], Value::Null, "{tool}");
        }
    }

    #[test]
    fn every_tool_has_a_test_vector() {
        let manifest = manifest(&profile());
        let vectors = manifest["test_vectors"].as_array().expect("array");
        let vector_tools: std::collections::BTreeSet<&str> = vectors
            .iter()
            .map(|v| v["tool"].as_str().expect("a tool name"))
            .collect();
        assert_eq!(
            vector_tools,
            std::collections::BTreeSet::from([
                "verify_status",
                "verify_package_check",
                "verify_run",
                "verify_open",
                "verify_controls",
                "verify_field",
                "verify_set",
                "verify_click",
                "verify_next",
                "verify_prev",
                "verify_goto",
                "verify_reset",
                "verify_screenshot",
                "verify_submit",
                "verify_close",
            ]),
            "every declared tool should have at least one test vector"
        );
    }

    #[test]
    fn every_test_vector_is_answerable_without_docker() {
        let manifest = manifest(&profile());
        let vectors = manifest["test_vectors"].as_array().unwrap();
        let run_vector = vectors
            .iter()
            .find(|v| v["tool"] == "verify_run")
            .expect("verify_run has a vector");
        assert_eq!(run_vector["args"]["dry_run"], json!(true));

        for tool in [
            "verify_open",
            "verify_controls",
            "verify_field",
            "verify_set",
            "verify_click",
            "verify_next",
            "verify_prev",
            "verify_goto",
            "verify_reset",
            "verify_screenshot",
            "verify_submit",
            "verify_close",
        ] {
            let vector = vectors
                .iter()
                .find(|v| v["tool"] == tool)
                .unwrap_or_else(|| panic!("{tool} has a vector"));
            assert_eq!(vector["expect"]["error"], json!(true), "{tool}");
        }
    }

    #[test]
    fn every_tool_description_is_real_prompt_surface() {
        for spec in tool_specs() {
            let description = spec["description"].as_str().expect("a description");
            assert!(
                description.len() > 60,
                "u2s-mcp's conformance suite fails an empty description: {description}"
            );
        }
    }
}
