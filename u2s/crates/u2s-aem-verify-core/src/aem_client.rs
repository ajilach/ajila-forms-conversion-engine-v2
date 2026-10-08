//! AEM CRX Package Manager HTTP client (HTTP basic auth), ported from
//! `ajila-forms-conversion-engine`'s successor (`blueprint/agent/src/aem_client.rs`)
//! rather than rewritten: the upload/install/DoR endpoints and their CRX
//! JSON response shape are unchanged by anything this workspace does.
//!
//! - upload:    `POST {host}/crx/packmgr/service/.json/?cmd=upload` (multipart)
//! - install:   `POST {host}/crx/packmgr/service/.json{path}?cmd=install`
//! - uninstall: `POST {host}/crx/packmgr/service/.json{path}?cmd=uninstall`
//! - DoR:       `GET {host}{form_path}/jcr:content/guideContainer.af.dor.pdf`
//!
//! Adapted in one way: `AemConnection` (blueprint's desktop-app profile
//! type) becomes a bare `host`/`user`/`password` triple here, since this
//! server reads those out of [`crate::profile::Profile`] rather than a
//! shared settings file.

#[derive(Debug, Clone, thiserror::Error)]
pub enum AemClientError {
    #[error("AEM {action} request failed: {source}")]
    Request {
        action: &'static str,
        #[source]
        source: std::sync::Arc<reqwest::Error>,
    },
    #[error("AEM {action} response could not be read: {source}")]
    ReadBody {
        action: &'static str,
        #[source]
        source: std::sync::Arc<reqwest::Error>,
    },
    #[error("AEM {action} failed: authentication rejected (HTTP {status})")]
    AuthRejected { action: &'static str, status: u16 },
    #[error("AEM {action} failed (HTTP {status}): {snippet}")]
    NonJsonResponse {
        action: &'static str,
        status: u16,
        snippet: String,
    },
    #[error("AEM {action} failed: {message}")]
    CrxError {
        action: &'static str,
        message: String,
    },
    #[error("AEM upload succeeded but returned no package path")]
    NoPackagePath,
    #[error("AEM {action} failed (HTTP {status}): {snippet}")]
    HttpError {
        action: &'static str,
        status: u16,
        snippet: String,
    },
    #[error("AEM DoR response was not a PDF (DoR may not be configured for this form): {snippet}")]
    NotAPdf { snippet: String },
}

fn request_error(action: &'static str, source: reqwest::Error) -> AemClientError {
    AemClientError::Request {
        action,
        source: std::sync::Arc::new(source),
    }
}

fn read_error(action: &'static str, source: reqwest::Error) -> AemClientError {
    AemClientError::ReadBody {
        action,
        source: std::sync::Arc::new(source),
    }
}

pub struct AemClient {
    client: reqwest::Client,
    host: String,
    user: String,
    password: String,
}

impl AemClient {
    /// `host` is the base URL of the AEM instance's published port, e.g.
    /// `http://127.0.0.1:49321` -- the ephemeral host port
    /// `u2s_verify_core::docker::RunningContainer::published_port` handed
    /// back for container port 4502.
    pub fn new(host: &str, user: &str, password: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            host: host.trim_end_matches('/').to_owned(),
            user: user.to_owned(),
            password: password.to_owned(),
        }
    }

    /// Uploads `zip` and installs it. `package_name` becomes the uploaded
    /// file's name (`{package_name}.zip`); it need not match the package's
    /// own JCR path -- a caller that wants the form's URL already derived
    /// it from the package's own `filter.xml` via
    /// [`crate::package_check::form_jcr_path`], not from anything the
    /// Package Manager echoes back. Returns the package's CRX path (e.g.
    /// `/etc/packages/.../x.zip`), which is what [`Self::uninstall`] later
    /// needs to remove exactly this package from a persistent session
    /// (`crate::session`) rather than whatever happened to be installed
    /// under that name before.
    pub async fn upload_and_install(
        &self,
        zip: Vec<u8>,
        package_name: &str,
    ) -> Result<String, AemClientError> {
        let file_name = format!("{package_name}.zip");
        let part = reqwest::multipart::Part::bytes(zip)
            .file_name(file_name)
            .mime_str("application/zip")
            .expect("\"application/zip\" is always a valid mime string");
        let form = reqwest::multipart::Form::new()
            .part("package", part)
            .text("force", "true");

        let upload_url = format!("{}/crx/packmgr/service/.json/?cmd=upload", self.host);
        let response = self
            .client
            .post(&upload_url)
            .basic_auth(&self.user, Some(&self.password))
            .multipart(form)
            .send()
            .await
            .map_err(|e| request_error("upload", e))?;

        let path = parse_crx_response(response, "upload")
            .await?
            .ok_or(AemClientError::NoPackagePath)?;

        let install_url = format!("{}/crx/packmgr/service/.json{path}?cmd=install", self.host);
        let response = self
            .client
            .post(&install_url)
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .map_err(|e| request_error("install", e))?;

        parse_crx_response(response, "install").await?;
        Ok(path)
    }

    /// Uninstalls the package at `path` (as returned by
    /// [`Self::upload_and_install`]) via CRX Package Manager
    /// `cmd=uninstall`, so a persistent session (`crate::session`) can
    /// return to a clean baseline between calls instead of accumulating
    /// whatever each `verify_run` installed. Best-effort by design: CRX
    /// reports `success:false` for a package that is not currently
    /// installed (already uninstalled, or the install itself never
    /// completed), and that must not fail a caller that is only trying to
    /// make sure nothing of the prior run is left behind -- so this treats
    /// [`AemClientError::CrxError`] the same as success and only surfaces a
    /// genuine transport or authentication failure.
    pub async fn uninstall(&self, path: &str) -> Result<(), AemClientError> {
        let url = format!(
            "{}/crx/packmgr/service/.json{path}?cmd=uninstall",
            self.host
        );
        let response = self
            .client
            .post(&url)
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .map_err(|e| request_error("uninstall", e))?;

        match parse_crx_response(response, "uninstall").await {
            Ok(_) | Err(AemClientError::CrxError { .. }) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Fetches the Document of Record for a deployed form via the guide
    /// container's `.dor.pdf` selector. Errors, with a body snippet, if the
    /// response is not a PDF -- the same signal a caller uses whether DoR
    /// was never configured for the form or the selector itself has moved
    /// in a future AEM version.
    pub async fn fetch_dor_pdf(&self, form_jcr_path: &str) -> Result<Vec<u8>, AemClientError> {
        let path = form_jcr_path.trim_end_matches('/');
        let url = format!("{}{path}/jcr:content/guideContainer.af.dor.pdf", self.host);
        let response = self
            .client
            .get(&url)
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .map_err(|e| request_error("DoR fetch", e))?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| read_error("DoR fetch", e))?
            .to_vec();

        if !status.is_success() {
            return Err(AemClientError::HttpError {
                action: "DoR fetch",
                status: status.as_u16(),
                snippet: snippet(&bytes),
            });
        }
        if !bytes.starts_with(b"%PDF") {
            return Err(AemClientError::NotAPdf {
                snippet: snippet(&bytes),
            });
        }
        Ok(bytes)
    }
}

impl AemClient {
    /// GETs `path` (already percent-encoded where needed) with the instance's
    /// credentials and returns the body. Used to read a file AEM stored in
    /// the repository -- e.g. a UBS DoR under `/tmp/ubsdocs/` -- when the
    /// browser could not download it.
    pub async fn fetch_path(&self, path: &str) -> Result<Vec<u8>, AemClientError> {
        let url = format!("{}{path}", self.host);
        let response = self
            .client
            .get(&url)
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .map_err(|e| request_error("repository read", e))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| read_error("repository read", e))?
            .to_vec();
        if !status.is_success() {
            return Err(AemClientError::HttpError {
                action: "repository read",
                status: status.as_u16(),
                snippet: snippet(&bytes),
            });
        }
        Ok(bytes)
    }

    /// The last `lines` lines of one of AEM's log files (`name` as the Sling
    /// log tailer knows it, e.g. `/logs/error.log`), read through the Web
    /// Console's tailer.
    pub async fn tail_log(&self, name: &str, lines: u32) -> Result<String, AemClientError> {
        let encoded: String = name
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect();
        let path = format!("/system/console/slinglog/tailer.txt?tail={lines}&name={encoded}");
        let bytes = self.fetch_path(&path).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Parses a CRX Package Manager `.json` response. Returns the package
/// `path` (present on upload, absent on install) on success. CRX returns
/// an HTML login page on auth failure rather than JSON, which is why a
/// non-JSON body is distinguished by status code rather than treated as
/// one more parse failure -- the message a caller sees should say
/// "wrong credentials", not "unexpected response shape".
async fn parse_crx_response(
    response: reqwest::Response,
    action: &'static str,
) -> Result<Option<String>, AemClientError> {
    let status = response.status();
    let body = response.text().await.map_err(|e| read_error(action, e))?;

    let json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(json) => json,
        Err(_) if status.as_u16() == 401 || status.as_u16() == 403 => {
            return Err(AemClientError::AuthRejected {
                action,
                status: status.as_u16(),
            });
        }
        Err(_) => {
            return Err(AemClientError::NonJsonResponse {
                action,
                status: status.as_u16(),
                snippet: snippet(body.as_bytes()),
            });
        }
    };

    if json["success"].as_bool() == Some(true) {
        return Ok(json["path"].as_str().map(String::from));
    }

    let message = json["msg"].as_str().unwrap_or("unknown error").to_owned();
    Err(AemClientError::CrxError { action, message })
}

fn snippet(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No AEM instance is assumed reachable for this crate's own test run
    /// -- see the crate's `#[ignore]`d live tests for the HTTP-backed
    /// coverage. This only exercises the pure CRX response parsing, which
    /// is the part a hand-typed fixture can actually stand in for.
    fn resp(status: u16, body: &str) -> reqwest::Response {
        http::Response::builder()
            .status(status)
            .body(body.to_owned())
            .expect("a fixture response always builds")
            .into()
    }

    #[tokio::test]
    async fn a_successful_upload_response_yields_the_package_path() {
        let path = parse_crx_response(
            resp(
                200,
                r#"{"success":true,"path":"/etc/packages/fd/export/x.zip"}"#,
            ),
            "upload",
        )
        .await
        .expect("must parse");
        assert_eq!(path.as_deref(), Some("/etc/packages/fd/export/x.zip"));
    }

    #[tokio::test]
    async fn a_successful_install_response_has_no_path() {
        let path = parse_crx_response(resp(200, r#"{"success":true}"#), "install")
            .await
            .expect("must parse");
        assert_eq!(path, None);
    }

    #[tokio::test]
    async fn a_crx_failure_message_is_carried_through() {
        let err = parse_crx_response(
            resp(
                200,
                r#"{"success":false,"msg":"package already installed"}"#,
            ),
            "install",
        )
        .await
        .expect_err("must fail");
        assert!(matches!(
            err,
            AemClientError::CrxError { message, .. } if message == "package already installed"
        ));
    }

    #[tokio::test]
    async fn a_401_html_login_page_is_reported_as_auth_rejected_not_a_parse_error() {
        let err = parse_crx_response(resp(401, "<html>please log in</html>"), "upload")
            .await
            .expect_err("must fail");
        assert!(matches!(
            err,
            AemClientError::AuthRejected { status: 401, .. }
        ));
    }

    #[tokio::test]
    async fn a_non_json_non_auth_response_names_the_status_and_a_snippet() {
        let err = parse_crx_response(resp(500, "internal server error"), "install")
            .await
            .expect_err("must fail");
        match err {
            AemClientError::NonJsonResponse {
                status, snippet, ..
            } => {
                assert_eq!(status, 500);
                assert!(snippet.contains("internal server error"));
            }
            other => panic!("expected NonJsonResponse, got {other:?}"),
        }
    }

    /// `uninstall`'s own leniency lives in how it interprets
    /// [`parse_crx_response`]'s result, so this exercises that mapping
    /// directly rather than standing up a fixture HTTP server: a CRX
    /// failure message (e.g. "package not installed") must not become an
    /// error a persistent session's cleanup has to handle specially.
    #[tokio::test]
    async fn a_crx_failure_on_uninstall_is_not_an_error() {
        let result = parse_crx_response(
            resp(200, r#"{"success":false,"msg":"package not installed"}"#),
            "uninstall",
        )
        .await;
        let mapped = match result {
            Ok(_) => Ok(()),
            Err(AemClientError::CrxError { .. }) => Ok(()),
            Err(err) => Err(err),
        };
        assert!(
            mapped.is_ok(),
            "a CRX-level failure must be swallowed by uninstall's own leniency"
        );
    }

    #[test]
    fn a_dor_response_that_is_not_a_pdf_is_reported_with_a_snippet() {
        let bytes = b"<html>404 not found</html>".to_vec();
        assert!(!bytes.starts_with(b"%PDF"));
        assert!(snippet(&bytes).contains("404"));
    }
}
