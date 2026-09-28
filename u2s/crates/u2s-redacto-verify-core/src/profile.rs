//! A Redacto verification profile: what makes one binary's checks differ
//! from another's, without a branch in [`crate::flow`] or [`crate::session`]
//! -- the same `FormDriver`/tenant-seam split `u2s-aem-verify-core` already
//! uses, scaled down to what this format's verifier actually needs to vary.
//!
//! **Deliberately narrower than AEM's own profile.** This crate never boots
//! a custom, privately-registry-hosted image the way `u2s-aem-verify-core`
//! does: the one container it starts is a public `postgres` image, always
//! available, needing nothing baked. What a profile actually varies is
//! whether (and where) an *already-running* platform's rendering endpoint
//! is reachable -- see [`RenderProfile::rendering_base_url`]'s own doc for
//! why importing the dump under test into that external platform's own
//! database is the operator's responsibility, not this crate's.

use std::env;

pub struct RenderProfile {
    pub name: &'static str,
    /// A public image, needing no registry credentials or baking --
    /// always available, unlike every AEM verify profile's own `_IMAGE`.
    pub postgres_image: String,
    /// Base URL of an *already-running* platform's rendering endpoint
    /// (`POST {base}/bin/redacto/rendering/integration`), e.g.
    /// `http://localhost:8082`. `None` means this deployment has no
    /// platform to render against yet -- `verify_run` still performs the
    /// self-contained Postgres import check and reports rendering as
    /// skipped, not as failed.
    ///
    /// **Precondition this crate does not enforce**: the dump under test
    /// must already be imported into *that* platform's own database before
    /// calling `verify_run` -- this crate's own throwaway Postgres import
    /// (see [`crate::session`]) is a separate database used only to prove
    /// the dump is well-formed, never the one the render call reads from.
    /// The same "kept up independently, never started by this crate"
    /// relationship `u2s-aem-verify-core`'s own `U2S_AEM_VERIFY_UBS_REDACTO_URL`
    /// already has with its Redacto Summary dependency.
    pub rendering_base_url: Option<String>,
    pub basic_auth: Option<(String, String)>,
    /// `"pdf"` or `"pdf-ua"` -- the `format` query parameter the rendering
    /// endpoint takes. A UBS profile requests `"pdf-ua"`; a generic one
    /// would request plain `"pdf"`.
    pub render_format: &'static str,
}

impl RenderProfile {
    /// Reads `{PREFIX}_POSTGRES_IMAGE` (default `postgres:16-alpine`),
    /// `{PREFIX}_RENDERING_URL`, `{PREFIX}_USER`/`{PREFIX}_PASSWORD`
    /// (default `admin`/`admin`, the platform's own local-dev default).
    pub fn from_env(name: &'static str, prefix: &str, render_format: &'static str) -> Self {
        let postgres_image = env::var(format!("{prefix}_POSTGRES_IMAGE"))
            .unwrap_or_else(|_| "postgres:16-alpine".to_owned());
        let rendering_base_url = env::var(format!("{prefix}_RENDERING_URL")).ok();
        let user = env::var(format!("{prefix}_USER")).unwrap_or_else(|_| "admin".to_owned());
        let password = env::var(format!("{prefix}_PASSWORD")).unwrap_or_else(|_| "admin".to_owned());
        Self {
            name,
            postgres_image,
            rendering_base_url,
            basic_auth: Some((user, password)),
            render_format,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_defaults_when_nothing_is_set() {
        // A prefix guaranteed unused by any real environment variable.
        let profile = RenderProfile::from_env("test", "U2S_REDACTO_VERIFY_TEST_NONCE_XYZ", "pdf");
        assert_eq!(profile.postgres_image, "postgres:16-alpine");
        assert_eq!(profile.rendering_base_url, None);
        assert_eq!(profile.basic_auth, Some(("admin".to_owned(), "admin".to_owned())));
    }
}
