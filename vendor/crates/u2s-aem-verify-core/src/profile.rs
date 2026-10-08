//! The one AEM profile this server process serves, read once from
//! environment at startup into a value that cannot hold an invalid
//! configuration -- the untrusted-input-at-the-edge discipline this
//! workspace uses throughout, applied to process environment rather than a
//! request body.
//!
//! Every variable is `U2S_*`-prefixed because `u2s-mcp`'s registration
//! allowlist (`crates/u2s-mcp/src/registration.rs`) only ever forwards
//! `U2S_*` (plus a short fixed list) to a registered stdio server's
//! environment -- an unprefixed variable this server needed would simply
//! never arrive. Credentials are env, never `args`: `args` are stored in
//! `mcp_servers` and shown back in the registration UI, `env` is not.
//!
//! One profile per *process*, not per format: the format-specific
//! *behaviour* a binary needs (how a form's URL is built, how its terminal
//! panel and submit call are recognised) is that binary's own
//! `crate::driver::FormDriver`, never a branch in this struct or in
//! `crate::flow`. Two AEM profiles (`aem-ubs`, `aem`) therefore need two
//! different binaries (`u2s-aem-ubs-verify-mcp`, `u2s-aem-verify-mcp`),
//! each configured by this same `Profile` shape from its own environment
//! -- `Profile` itself stays format-blind.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitArtefact {
    /// The UBS overlay's submit action triggers a browser download.
    Download,
    /// No overlay-specific submit action is configured; fetch the
    /// Document of Record instead (`.../jcr:content/guideContainer.af.dor.pdf`).
    DocumentOfRecord,
    /// Neither is configured for this profile -- `verify_run` renders and
    /// screenshots but never fills or submits.
    None,
}

impl SubmitArtefact {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "download" => Some(Self::Download),
            "dor" => Some(Self::DocumentOfRecord),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The output format key this registration serves -- `"aem-ubs"` or
    /// `"aem"` -- becomes the manifest's `FormatScope`.
    pub format: String,
    pub aem_image: String,
    pub aem_user: String,
    pub aem_password: String,
    pub submit: SubmitArtefact,
    pub chromium_image: String,
    pub platform: String,
    pub boot_timeout: Duration,
    /// How long a session may go with no interaction -- a `verify_run`
    /// call, or any `verify_open`/`verify_controls`/`verify_set`/`verify_next`/
    /// `verify_prev`/`verify_reset`/`verify_screenshot`/`verify_submit`/
    /// `verify_close` call touching it -- before the persistent session
    /// (`crate::session`) tears itself down. Not a per-run timeout -- a
    /// forgotten dev environment safety net, since the session otherwise
    /// runs until the process exits.
    pub idle_timeout: Duration,
    pub keep_on_failure: bool,
    /// The already-running rendering dependency a UBS form's submit path
    /// calls out to (e.g. `ajila-forms-ubs-redacto-summary`'s
    /// `/bin/redacto/summary/generatepdf`) -- not a container this crate
    /// starts, stops, or bakes into an image, only one `verify_status`
    /// checks reachability of and `flow::run` refuses to submit against
    /// when it is configured but unreachable. `None` for a profile whose
    /// submit strategy does not depend on it (`SubmitArtefact::DocumentOfRecord`
    /// or `SubmitArtefact::None`).
    pub redacto_url: Option<String>,
    /// The named Docker volume mounted at `/aem/crx-quickstart` (the JCR
    /// repository and quickstart install) on every AEM container this
    /// profile boots: the AEM images declare that path a `VOLUME`, so an
    /// instance's state lives in the volume, never in an image layer. A new
    /// volume is seeded from the image on first boot (see
    /// `docker/aem/README.md`); every later boot reuses it.
    ///
    /// `U2S_AEM_VERIFY_DATA_VOLUME` when set, otherwise derived from the
    /// format and the image reference ([`default_data_volume`]), so a new
    /// image gets a fresh volume instead of booting on an older image's
    /// state.
    pub aem_data_volume: String,
    /// `U2S_VERIFY_SELF_CONTAINER`: the name of the container this process
    /// runs in, when it runs in one. Set, each session's network is joined
    /// and siblings are reached by container address
    /// ([`crate::session::Reach::SessionNetwork`]); unset, through ports
    /// published on this host's loopback ([`crate::session::Reach::Published`]).
    pub self_container: Option<String>,
    /// The port AEM listens on *inside* its own container -- not
    /// necessarily 4502. A vanilla Adobe quickstart jar defaults to 4502,
    /// but `ajila.azurecr.io/aemforms-arm` runs its own entrypoint script
    /// with an explicit `-p 8080` (confirmed via `docker inspect`'s
    /// `ExposedPorts`, and live: this crate previously hardcoded 4502
    /// unconditionally, which published a port nothing inside ajila's
    /// image was listening on -- every live `verify_run` timed out waiting
    /// for a login page that was never going to answer on that port, while
    /// the same page answered instantly on 8080 from inside the same
    /// container). Defaults to 4502 for a profile that never overrides it.
    pub aem_container_port: u16,
    /// A screenshot this large or smaller travels inline in a tool result;
    /// larger goes to the blob store instead. Same default (1'572'864
    /// bytes, 1536 KiB) as `u2s-render-core::limits::Limits`'s own
    /// `max_inline_bytes` -- this crate does not depend on that one (it
    /// would drag in an `image` dependency for a single env read), but the
    /// value is worth keeping identical across every u2s server that makes
    /// this same inline-or-blob decision. Read from
    /// `U2S_AEM_VERIFY_MAX_INLINE_BYTES`, not a bare `MAX_INLINE_BYTES`
    /// like the render crates use -- this module's own doc explains why
    /// every variable here is `U2S_AEM_VERIFY_`-prefixed.
    pub max_inline_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("{0} is set but empty")]
    Empty(&'static str),
    #[error("U2S_AEM_VERIFY_SUBMIT={0:?} is not one of: download, dor, none")]
    InvalidSubmit(String),
    #[error("U2S_AEM_VERIFY_BOOT_TIMEOUT_SECS={0:?} is not a positive integer")]
    InvalidBootTimeout(String),
    #[error("U2S_AEM_VERIFY_IDLE_TIMEOUT_SECS={0:?} is not a positive integer")]
    InvalidIdleTimeout(String),
    #[error("U2S_AEM_VERIFY_CONTAINER_PORT={0:?} is not a valid port number")]
    InvalidContainerPort(String),
    #[error("MAX_INLINE_BYTES={0:?} is not a positive integer")]
    InvalidMaxInlineBytes(String),
}

const DEFAULT_CHROMIUM_IMAGE: &str = "chromedp/headless-shell:stable";
/// Empty: the daemon runs the image for its own architecture. The AEM images
/// are published for both arm64 and amd64.
const DEFAULT_PLATFORM: &str = "";
const DEFAULT_BOOT_TIMEOUT_SECS: u64 = 900;
/// 30 minutes: long enough that a developer stepping away between calls
/// during a debugging session does not eat a cold reboot, short enough that
/// a forgotten session does not occupy AEM's fairly heavy memory footprint
/// indefinitely.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 1800;
/// A vanilla Adobe AEM quickstart jar's own default port -- what a profile
/// gets unless it overrides `U2S_AEM_VERIFY_CONTAINER_PORT` for an image
/// (like ajila's) whose entrypoint runs AEM on a different one.
const DEFAULT_AEM_CONTAINER_PORT: u16 = 4502;
/// 1536 KiB -- see [`Profile::max_inline_bytes`]'s own doc for why this
/// matches `u2s-render-core::limits::Limits`'s default exactly.
const DEFAULT_MAX_INLINE_BYTES: usize = 1536 * 1024;

impl Profile {
    /// Reads every `U2S_AEM_VERIFY_*` variable this process needs. Fails
    /// closed: a missing or malformed required variable is a startup
    /// error, never a default that would silently target the wrong image
    /// or skip authentication.
    pub fn from_env() -> Result<Self, ProfileError> {
        Self::from_reader(|key| std::env::var(key).ok())
    }

    /// The actual parser, taking a variable reader so it is testable
    /// without mutating the real process environment -- `std::env::set_var`
    /// is unsound to call from multiple threads, which is exactly the
    /// situation a test binary running in parallel is in.
    pub fn from_reader(read: impl Fn(&str) -> Option<String>) -> Result<Self, ProfileError> {
        let required = |key: &'static str| match read(key) {
            None => Err(ProfileError::Missing(key)),
            Some(value) if value.is_empty() => Err(ProfileError::Empty(key)),
            Some(value) => Ok(value),
        };

        let format = required("U2S_AEM_VERIFY_FORMAT")?;
        let aem_image = required("U2S_AEM_VERIFY_IMAGE")?;
        let aem_user = required("U2S_AEM_VERIFY_USER")?;
        let aem_password = required("U2S_AEM_VERIFY_PASSWORD")?;

        let submit = match read("U2S_AEM_VERIFY_SUBMIT") {
            None => SubmitArtefact::None,
            Some(raw) => SubmitArtefact::parse(&raw).ok_or(ProfileError::InvalidSubmit(raw))?,
        };

        let chromium_image = read("U2S_AEM_VERIFY_CHROMIUM_IMAGE")
            .unwrap_or_else(|| DEFAULT_CHROMIUM_IMAGE.to_owned());
        let platform =
            read("U2S_AEM_VERIFY_PLATFORM").unwrap_or_else(|| DEFAULT_PLATFORM.to_owned());

        let boot_timeout = match read("U2S_AEM_VERIFY_BOOT_TIMEOUT_SECS") {
            None => Duration::from_secs(DEFAULT_BOOT_TIMEOUT_SECS),
            Some(raw) => {
                let secs: u64 = raw
                    .parse()
                    .ok()
                    .filter(|&secs| secs > 0)
                    .ok_or(ProfileError::InvalidBootTimeout(raw))?;
                Duration::from_secs(secs)
            }
        };

        let idle_timeout = match read("U2S_AEM_VERIFY_IDLE_TIMEOUT_SECS") {
            None => Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            Some(raw) => {
                let secs: u64 = raw
                    .parse()
                    .ok()
                    .filter(|&secs| secs > 0)
                    .ok_or(ProfileError::InvalidIdleTimeout(raw))?;
                Duration::from_secs(secs)
            }
        };

        let keep_on_failure = read("U2S_AEM_VERIFY_KEEP_ON_FAILURE").as_deref() == Some("1");
        let redacto_url = read("U2S_AEM_VERIFY_REDACTO_URL").filter(|v| !v.is_empty());
        let aem_data_volume = read("U2S_AEM_VERIFY_DATA_VOLUME")
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default_data_volume(&format, &aem_image));
        let self_container = read("U2S_VERIFY_SELF_CONTAINER").filter(|v| !v.is_empty());

        let aem_container_port = match read("U2S_AEM_VERIFY_CONTAINER_PORT") {
            None => DEFAULT_AEM_CONTAINER_PORT,
            Some(raw) => raw
                .parse()
                .ok()
                .filter(|&port: &u16| port > 0)
                .ok_or(ProfileError::InvalidContainerPort(raw))?,
        };

        let max_inline_bytes = match read("U2S_AEM_VERIFY_MAX_INLINE_BYTES") {
            None => DEFAULT_MAX_INLINE_BYTES,
            Some(raw) => raw
                .parse()
                .ok()
                .filter(|&bytes: &usize| bytes > 0)
                .ok_or(ProfileError::InvalidMaxInlineBytes(raw))?,
        };

        Ok(Self {
            format,
            aem_image,
            aem_user,
            aem_password,
            submit,
            chromium_image,
            platform,
            boot_timeout,
            idle_timeout,
            keep_on_failure,
            redacto_url,
            aem_data_volume,
            self_container,
            aem_container_port,
            max_inline_bytes,
        })
    }

    /// The Docker label filter matching every container this profile
    /// created (`u2s-verify-core::docker::DockerLifecycle::find_by_label`'s
    /// argument), so a leftover from a crashed process is discoverable by
    /// format alone. `crate::flow::acquire` writes the label this matches
    /// against via [`Self::owner_labels`], so the two cannot drift apart.
    pub fn owner_label(&self) -> String {
        u2s_verify_core::session::owner_label(&self.format)
    }

    /// The label(s) every container and network this profile creates
    /// carries -- the write side of [`Self::owner_label`]'s read-side
    /// filter.
    pub fn owner_labels(&self) -> std::collections::HashMap<String, String> {
        u2s_verify_core::session::owner_labels(&self.format)
    }
}

/// `u2s-aem-<format>-<first 12 hex digits of sha256(image)>`: one volume per
/// image reference, stable across restarts. Pure function. Unit-tested.
pub fn default_data_volume(format: &str, image: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(image.as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    let format: String = format
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    format!("u2s-aem-{format}-{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn minimal_env() -> Vec<(&'static str, &'static str)> {
        vec![
            ("U2S_AEM_VERIFY_FORMAT", "aem-ubs"),
            ("U2S_AEM_VERIFY_IMAGE", "registry.example/aem-ubs:sp1"),
            ("U2S_AEM_VERIFY_USER", "admin"),
            ("U2S_AEM_VERIFY_PASSWORD", "admin"),
        ]
    }

    #[test]
    fn a_minimal_environment_parses_with_defaults() {
        let profile = Profile::from_reader(env(&minimal_env())).expect("parses");
        assert_eq!(profile.format, "aem-ubs");
        assert_eq!(profile.submit, SubmitArtefact::None);
        assert_eq!(profile.chromium_image, DEFAULT_CHROMIUM_IMAGE);
        assert_eq!(profile.platform, DEFAULT_PLATFORM);
        assert_eq!(profile.boot_timeout, Duration::from_secs(900));
        assert_eq!(profile.idle_timeout, Duration::from_secs(1800));
        assert!(!profile.keep_on_failure);
        assert_eq!(profile.redacto_url, None);
        assert_eq!(
            profile.aem_data_volume,
            default_data_volume("aem-ubs", "registry.example/aem-ubs:sp1")
        );
        assert_eq!(profile.self_container, None);
        assert_eq!(profile.aem_container_port, 4502);
        assert_eq!(profile.max_inline_bytes, DEFAULT_MAX_INLINE_BYTES);
    }

    #[test]
    fn a_max_inline_bytes_override_is_carried_through() {
        let mut pairs = minimal_env();
        pairs.push(("U2S_AEM_VERIFY_MAX_INLINE_BYTES", "4096"));
        let profile = Profile::from_reader(env(&pairs)).expect("parses");
        assert_eq!(profile.max_inline_bytes, 4096);
    }

    #[test]
    fn a_zero_or_non_numeric_max_inline_bytes_is_rejected() {
        for bad in ["0", "-5", "notanumber", ""] {
            let mut pairs = minimal_env();
            pairs.push(("U2S_AEM_VERIFY_MAX_INLINE_BYTES", bad));
            assert!(matches!(
                Profile::from_reader(env(&pairs)),
                Err(ProfileError::InvalidMaxInlineBytes(_))
            ));
        }
    }

    #[test]
    fn a_container_port_override_is_carried_through() {
        let mut pairs = minimal_env();
        pairs.push(("U2S_AEM_VERIFY_CONTAINER_PORT", "8080"));
        let profile = Profile::from_reader(env(&pairs)).expect("parses");
        assert_eq!(profile.aem_container_port, 8080);
    }

    #[test]
    fn a_zero_or_non_numeric_container_port_is_rejected() {
        for bad in ["0", "-5", "notaport", "70000", ""] {
            let mut pairs = minimal_env();
            pairs.push(("U2S_AEM_VERIFY_CONTAINER_PORT", bad));
            assert!(matches!(
                Profile::from_reader(env(&pairs)),
                Err(ProfileError::InvalidContainerPort(_))
            ));
        }
    }

    #[test]
    fn each_required_variable_is_named_when_missing() {
        for (missing, _) in minimal_env() {
            let mut pairs = minimal_env();
            pairs.retain(|(k, _)| *k != missing);
            let err = Profile::from_reader(env(&pairs)).expect_err("must fail closed");
            assert_eq!(err, ProfileError::Missing(missing));
        }
    }

    #[test]
    fn an_empty_required_variable_is_distinct_from_a_missing_one() {
        let mut pairs = minimal_env();
        pairs.retain(|(k, _)| *k != "U2S_AEM_VERIFY_USER");
        pairs.push(("U2S_AEM_VERIFY_USER", ""));
        let err = Profile::from_reader(env(&pairs)).expect_err("empty must still fail");
        assert_eq!(err, ProfileError::Empty("U2S_AEM_VERIFY_USER"));
    }

    #[test]
    fn submit_download_and_dor_and_none_all_parse() {
        for (raw, expected) in [
            ("download", SubmitArtefact::Download),
            ("dor", SubmitArtefact::DocumentOfRecord),
            ("none", SubmitArtefact::None),
        ] {
            let mut pairs = minimal_env();
            pairs.push(("U2S_AEM_VERIFY_SUBMIT", raw));
            let profile = Profile::from_reader(env(&pairs)).expect("parses");
            assert_eq!(profile.submit, expected);
        }
    }

    #[test]
    fn an_unknown_submit_value_is_rejected() {
        let mut pairs = minimal_env();
        pairs.push(("U2S_AEM_VERIFY_SUBMIT", "email"));
        assert_eq!(
            Profile::from_reader(env(&pairs)),
            Err(ProfileError::InvalidSubmit("email".to_owned()))
        );
    }

    #[test]
    fn a_zero_or_non_numeric_boot_timeout_is_rejected() {
        for bad in ["0", "-5", "soon", ""] {
            let mut pairs = minimal_env();
            pairs.push(("U2S_AEM_VERIFY_BOOT_TIMEOUT_SECS", bad));
            assert!(matches!(
                Profile::from_reader(env(&pairs)),
                Err(ProfileError::InvalidBootTimeout(_))
            ));
        }
    }

    #[test]
    fn keep_on_failure_requires_exactly_the_string_one() {
        for value in ["true", "yes", "2", ""] {
            let mut pairs = minimal_env();
            pairs.push(("U2S_AEM_VERIFY_KEEP_ON_FAILURE", value));
            let profile = Profile::from_reader(env(&pairs)).expect("parses");
            assert!(
                !profile.keep_on_failure,
                "{value:?} must not enable keep_on_failure, only \"1\" may"
            );
        }
        let mut pairs = minimal_env();
        pairs.push(("U2S_AEM_VERIFY_KEEP_ON_FAILURE", "1"));
        assert!(
            Profile::from_reader(env(&pairs))
                .expect("parses")
                .keep_on_failure
        );
    }

    #[test]
    fn a_zero_or_non_numeric_idle_timeout_is_rejected() {
        for bad in ["0", "-5", "soon", ""] {
            let mut pairs = minimal_env();
            pairs.push(("U2S_AEM_VERIFY_IDLE_TIMEOUT_SECS", bad));
            assert!(matches!(
                Profile::from_reader(env(&pairs)),
                Err(ProfileError::InvalidIdleTimeout(_))
            ));
        }
    }

    #[test]
    fn an_empty_redacto_url_is_treated_as_unset() {
        let mut pairs = minimal_env();
        pairs.push(("U2S_AEM_VERIFY_REDACTO_URL", ""));
        let profile = Profile::from_reader(env(&pairs)).expect("parses");
        assert_eq!(profile.redacto_url, None);
    }

    #[test]
    fn a_redacto_url_is_carried_through_verbatim() {
        let mut pairs = minimal_env();
        pairs.push((
            "U2S_AEM_VERIFY_REDACTO_URL",
            "http://host.docker.internal:18080/bin/redacto/summary/generatepdf",
        ));
        let profile = Profile::from_reader(env(&pairs)).expect("parses");
        assert_eq!(
            profile.redacto_url.as_deref(),
            Some("http://host.docker.internal:18080/bin/redacto/summary/generatepdf")
        );
    }

    #[test]
    fn an_explicit_data_volume_wins_and_an_empty_one_falls_back() {
        let mut pairs = minimal_env();
        pairs.push(("U2S_AEM_VERIFY_DATA_VOLUME", "u2s-aem-ubs-data"));
        let profile = Profile::from_reader(env(&pairs)).expect("parses");
        assert_eq!(profile.aem_data_volume, "u2s-aem-ubs-data");
        let mut empty = minimal_env();
        empty.push(("U2S_AEM_VERIFY_DATA_VOLUME", ""));
        let profile = Profile::from_reader(env(&empty)).expect("parses");
        assert_eq!(
            profile.aem_data_volume,
            default_data_volume("aem-ubs", "registry.example/aem-ubs:sp1")
        );
    }

    #[test]
    fn the_default_data_volume_follows_the_image() {
        let a = default_data_volume("aem-ubs", "ajila.azurecr.io/u2s-aem-ubs:1");
        let b = default_data_volume("aem-ubs", "ajila.azurecr.io/u2s-aem-ubs:2");
        assert_ne!(a, b);
        assert_eq!(a, default_data_volume("aem-ubs", "ajila.azurecr.io/u2s-aem-ubs:1"));
        assert!(a.starts_with("u2s-aem-aem-ubs-") && a.len() == "u2s-aem-aem-ubs-".len() + 12, "{a}");
    }

    #[test]
    fn the_owner_label_names_the_format() {
        let profile = Profile::from_reader(env(&minimal_env())).expect("parses");
        assert_eq!(profile.owner_label(), "u2s.verify.format=aem-ubs");
    }

    #[test]
    fn owner_labels_and_owner_label_agree_on_the_same_key_and_value() {
        let profile = Profile::from_reader(env(&minimal_env())).expect("parses");
        let labels = profile.owner_labels();
        let owner_label = profile.owner_label();
        let (key, value) = owner_label
            .split_once('=')
            .expect("owner_label is always key=value");
        assert_eq!(labels.get(key), Some(&value.to_owned()));
    }
}
