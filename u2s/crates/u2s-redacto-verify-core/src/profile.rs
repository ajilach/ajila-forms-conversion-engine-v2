//! A Redacto verification profile: what makes one binary's checks differ
//! from another's, without a branch in [`crate::flow`] -- the same
//! tenant-seam split `u2s-aem-verify-core` uses, scaled down to what this
//! format's verifier varies.
//!
//! The profile names the platform images a session boots
//! ([`crate::session`]), not a running platform: each `session_id` gets its
//! own.

use std::time::Duration;

/// The prefix-relative variable names, for error messages and docs.
const MIGRATION_IMAGE: &str = "MIGRATION_IMAGE";
const CORE_IMAGE: &str = "CORE_IMAGE";
const RENDERING_IMAGE: &str = "RENDERING_IMAGE";

const DEFAULT_POSTGRES_IMAGE: &str = "postgres:16-alpine";
/// The platform's `core` service checks its Flyway schema and loads its
/// Sling bundles on start; compose gave it 90 s plus 30 health retries.
const DEFAULT_BOOT_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(1800);

/// The four images one platform is booted from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformImages {
    pub postgres: String,
    /// The platform's Flyway migrations, run once per boot.
    pub migration: String,
    pub core: String,
    pub rendering: String,
}

impl PlatformImages {
    pub fn all(&self) -> [&str; 4] {
        [&self.postgres, &self.migration, &self.core, &self.rendering]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderProfile {
    /// Also the owner label every container and network of this profile
    /// carries (`u2s_verify_core::session::owner_labels`).
    pub name: &'static str,
    pub images: PlatformImages,
    /// `docker run --platform` for the platform images; empty lets the
    /// daemon decide.
    pub platform: String,
    pub boot_timeout: Duration,
    pub idle_timeout: Duration,
    /// The `rendering` service's basic auth.
    pub basic_auth: (String, String),
    /// `"pdf"` or `"pdf-ua"` -- the `format` query parameter the rendering
    /// endpoint takes. A UBS profile requests `"pdf-ua"`.
    pub render_format: &'static str,
    /// `U2S_VERIFY_SELF_CONTAINER`: the container this process runs in, if
    /// any (`u2s_verify_core::session::Reach`).
    pub self_container: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    #[error("{0} is not set")]
    Missing(String),
    #[error("{key}={value:?} is invalid: {reason}")]
    Invalid {
        key: String,
        value: String,
        reason: &'static str,
    },
}

impl RenderProfile {
    /// Reads, under `prefix`:
    /// - `_MIGRATION_IMAGE`, `_CORE_IMAGE`, `_RENDERING_IMAGE` (required),
    /// - `_POSTGRES_IMAGE` (default `postgres:16-alpine`), `_PLATFORM`,
    /// - `_BOOT_TIMEOUT_SECS` (600), `_IDLE_TIMEOUT_SECS` (1800),
    /// - `_USER`/`_PASSWORD` (`admin`/`admin`, the platform's own default),
    ///
    /// and `U2S_VERIFY_SELF_CONTAINER`, shared with the AEM verifiers.
    pub fn from_env(
        name: &'static str,
        prefix: &str,
        render_format: &'static str,
    ) -> Result<Self, ProfileError> {
        Self::from_reader(name, prefix, render_format, |key| std::env::var(key).ok())
    }

    /// The parser, taking a variable reader so it is testable without
    /// mutating the process environment.
    pub fn from_reader(
        name: &'static str,
        prefix: &str,
        render_format: &'static str,
        read: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ProfileError> {
        let optional = |suffix: &str| read(&format!("{prefix}_{suffix}")).filter(|v| !v.is_empty());
        let required = |suffix: &str| {
            optional(suffix).ok_or_else(|| ProfileError::Missing(format!("{prefix}_{suffix}")))
        };
        let seconds = |suffix: &str, default: Duration| match optional(suffix) {
            None => Ok(default),
            Some(value) => match value.parse::<u64>() {
                Ok(secs) if secs > 0 => Ok(Duration::from_secs(secs)),
                _ => Err(ProfileError::Invalid {
                    key: format!("{prefix}_{suffix}"),
                    value,
                    reason: "expected a positive number of seconds",
                }),
            },
        };

        let images = PlatformImages {
            migration: required(MIGRATION_IMAGE)?,
            core: required(CORE_IMAGE)?,
            rendering: required(RENDERING_IMAGE)?,
            postgres: optional("POSTGRES_IMAGE").unwrap_or_else(|| DEFAULT_POSTGRES_IMAGE.to_owned()),
        };
        Ok(Self {
            name,
            images,
            platform: optional("PLATFORM").unwrap_or_default(),
            boot_timeout: seconds("BOOT_TIMEOUT_SECS", DEFAULT_BOOT_TIMEOUT)?,
            idle_timeout: seconds("IDLE_TIMEOUT_SECS", DEFAULT_IDLE_TIMEOUT)?,
            basic_auth: (
                optional("USER").unwrap_or_else(|| "admin".to_owned()),
                optional("PASSWORD").unwrap_or_else(|| "admin".to_owned()),
            ),
            render_format,
            self_container: read("U2S_VERIFY_SELF_CONTAINER").filter(|v| !v.is_empty()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn reader(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| map.get(key).cloned()
    }

    const IMAGES: [(&str, &str); 3] = [
        ("P_MIGRATION_IMAGE", "registry/migration:1"),
        ("P_CORE_IMAGE", "registry/core:1"),
        ("P_RENDERING_IMAGE", "registry/rendering:1"),
    ];

    #[test]
    fn every_platform_image_is_required() {
        assert_eq!(
            RenderProfile::from_reader("t", "P", "pdf", reader(&[])),
            Err(ProfileError::Missing("P_MIGRATION_IMAGE".to_owned()))
        );
        for missing in 0..IMAGES.len() {
            let mut pairs = IMAGES.to_vec();
            let (key, _) = pairs.remove(missing);
            assert_eq!(
                RenderProfile::from_reader("t", "P", "pdf", reader(&pairs)),
                Err(ProfileError::Missing(key.to_owned()))
            );
            // An empty value is the same as an unset one.
            pairs.insert(missing, (key, ""));
            assert_eq!(
                RenderProfile::from_reader("t", "P", "pdf", reader(&pairs)),
                Err(ProfileError::Missing(key.to_owned()))
            );
        }
    }

    #[test]
    fn a_minimal_environment_parses_with_defaults() {
        let profile = RenderProfile::from_reader("t", "P", "pdf-ua", reader(&IMAGES)).unwrap();
        assert_eq!(profile.images.postgres, DEFAULT_POSTGRES_IMAGE);
        assert_eq!(profile.images.core, "registry/core:1");
        assert_eq!(profile.platform, "");
        assert_eq!(profile.boot_timeout, DEFAULT_BOOT_TIMEOUT);
        assert_eq!(profile.idle_timeout, DEFAULT_IDLE_TIMEOUT);
        assert_eq!(profile.basic_auth, ("admin".to_owned(), "admin".to_owned()));
        assert_eq!(profile.self_container, None);
    }

    #[test]
    fn optional_settings_override_the_defaults() {
        let mut pairs = IMAGES.to_vec();
        pairs.extend([
            ("P_POSTGRES_IMAGE", "postgres:17"),
            ("P_PLATFORM", "linux/amd64"),
            ("P_BOOT_TIMEOUT_SECS", "120"),
            ("P_IDLE_TIMEOUT_SECS", "60"),
            ("P_USER", "u"),
            ("P_PASSWORD", "p"),
            ("U2S_VERIFY_SELF_CONTAINER", "u2s-server"),
        ]);
        let profile = RenderProfile::from_reader("t", "P", "pdf", reader(&pairs)).unwrap();
        assert_eq!(profile.images.postgres, "postgres:17");
        assert_eq!(profile.platform, "linux/amd64");
        assert_eq!(profile.boot_timeout, Duration::from_secs(120));
        assert_eq!(profile.idle_timeout, Duration::from_secs(60));
        assert_eq!(profile.basic_auth, ("u".to_owned(), "p".to_owned()));
        assert_eq!(profile.self_container.as_deref(), Some("u2s-server"));
    }

    #[test]
    fn a_zero_or_non_numeric_timeout_is_rejected() {
        for bad in ["0", "ten", "-5"] {
            let mut pairs = IMAGES.to_vec();
            pairs.push(("P_IDLE_TIMEOUT_SECS", bad));
            assert!(
                matches!(
                    RenderProfile::from_reader("t", "P", "pdf", reader(&pairs)),
                    Err(ProfileError::Invalid { .. })
                ),
                "{bad}"
            );
        }
    }
}
