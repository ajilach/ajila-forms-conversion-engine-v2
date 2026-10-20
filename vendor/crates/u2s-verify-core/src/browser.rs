//! A CDP browser driver over `chromiumoxide`, connecting to an
//! already-running headless Chromium container rather than launching one --
//! see `u2s-aem-verify-core::flow` for where that container comes from.
//!
//! **Interior mutability, documented.** [`PageHandle`]'s console/network
//! buffers are `Arc<Mutex<Vec<_>>>`, written to by background tasks that
//! drain `chromiumoxide`'s event streams for as long as the page lives.
//! There is no way around this: the events arrive on their own schedule
//! from a stream nothing here can poll synchronously, so something has to
//! own them between "the browser reported it" and "the caller asked for
//! it". The mutex is held only for the length of a `Vec::push`/`drain`,
//! never across an `.await`. The dialog buffer is the same shape.
//!
//! **Native dialogs are answered, not left open.** While an `alert`,
//! `confirm`, `prompt` or `beforeunload` dialog is open in a headless page
//! with no browser-side handler, the page's script execution stalls, so
//! every later `evaluate` would hang. Each page therefore answers every
//! dialog the moment it opens, accepting it the way a visitor clicking OK
//! would (a `prompt` gets its own default text), and records it so a caller
//! can report what the form said. The handler is attached before the page
//! navigates, so a dialog raised while the form loads is answered too.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::browser::{
    EventDownloadProgress, SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use chromiumoxide::cdp::browser_protocol::network::{
    EventLoadingFailed, EventResponseReceived, Headers, SetExtraHttpHeadersParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    ConsoleApiCalledType, EventConsoleApiCalled, EventExceptionThrown,
};
use chromiumoxide::cdp::browser_protocol::page::{
    EventJavascriptDialogOpening, HandleJavaScriptDialogParams, Viewport,
};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::{Page, ScreenshotParams};
use futures::StreamExt;

use crate::types::{Dialog, ErrorKind, FailedRequest, VerifyError};

/// The encoded image format a screenshot is captured in -- mirrors the two
/// formats `u2s-render-core::encode` already standardises on for a
/// rendered page, so a verifier's screenshot and a renderer's page image
/// use the same vocabulary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageFormat {
    Png,
    /// `quality` is clamped to `0..=100` by the caller building the CDP
    /// params; chromiumoxide itself does not validate it.
    Jpeg { quality: u8 },
}

impl ImageFormat {
    pub fn mime(&self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg { .. } => "image/jpeg",
        }
    }

    pub fn ext(&self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg { .. } => "jpg",
        }
    }

    fn cdp_format(&self) -> CaptureScreenshotFormat {
        match self {
            Self::Png => CaptureScreenshotFormat::Png,
            Self::Jpeg { .. } => CaptureScreenshotFormat::Jpeg,
        }
    }
}

/// A rectangular clip, in CSS pixels with the origin at the page's
/// top-left -- the same coordinate convention
/// `u2s-aem-verify-core::interactive`'s control inventory reports a
/// control's own `position` in (it adds `window.scrollX`/`scrollY` to a
/// `getBoundingClientRect()` result for exactly this reason: with
/// `capture_beyond_viewport`, CDP's own `Viewport` clip is in *document*
/// coordinates, not viewport-relative ones).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// What region of the page a screenshot covers. `FullPage` and a `Clip`
/// are mutually exclusive in chromiumoxide's own builder --
/// `ScreenshotParamsBuilder::full_page(true)` overwrites any `clip` it was
/// given (confirmed against `chromiumoxide-0.9.1`'s handler, which derives
/// the clip from the content size and applies a device-metrics override
/// once `full_page` is set) -- so this type makes that exclusivity
/// unrepresentable instead of leaving it as an unchecked combination of
/// booleans.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScreenshotArea {
    FullPage,
    Clip(ClipRect),
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("cannot connect to Chromium at {url}: {source}")]
    Connect {
        url: String,
        #[source]
        source: chromiumoxide::error::CdpError,
    },
    #[error("cannot open a page at {url}: {source}")]
    OpenPage {
        url: String,
        #[source]
        source: chromiumoxide::error::CdpError,
    },
    #[error("cannot set the download behaviour: {0}")]
    DownloadBehavior(String),
    #[error("cannot take a screenshot: {0}")]
    Screenshot(#[source] chromiumoxide::error::CdpError),
    #[error("cannot set the authentication header: {0}")]
    SetAuthHeader(#[source] chromiumoxide::error::CdpError),
    #[error("cannot listen for {what}: {source}")]
    Listen {
        what: &'static str,
        #[source]
        source: chromiumoxide::error::CdpError,
    },
    #[error("no download completed within {0:?}")]
    DownloadTimedOut(Duration),
    /// The CDP connection itself is gone ([`session_lost`]): nothing sent
    /// over it will ever answer again.
    #[error("the browser session was lost: {0}")]
    SessionLost(#[source] chromiumoxide::error::CdpError),
    /// The page answered, but evaluating the expression failed (a thrown
    /// exception, a CDP error, a timeout).
    #[error("evaluating a script on the page failed: {0}")]
    Evaluate(#[source] chromiumoxide::error::CdpError),
    /// The expression evaluated to something other than what the caller
    /// asked for (`undefined` from a script that never returned, say).
    #[error("the script evaluated to {got}, not a {expected}")]
    UnexpectedResult {
        expected: &'static str,
        got: String,
    },
}

impl BrowserError {
    /// Whether this error means the CDP connection is gone, so the page
    /// (and every other page on the same connection) is unusable.
    pub fn is_session_lost(&self) -> bool {
        matches!(self, Self::SessionLost(_))
    }

    /// This error as a tool error a caller can act on: a lost session is
    /// [`ErrorKind::BrowserSessionLost`] with the remedy, anything else is
    /// `kind`, naming `what` was being done. Pure. Unit-tested.
    pub fn into_verify_error(self, what: &str, kind: ErrorKind) -> VerifyError {
        if self.is_session_lost() {
            VerifyError::new(
                ErrorKind::BrowserSessionLost,
                format!("the browser session was lost: close it and open the form again ({what}: {self})"),
            )
        } else {
            VerifyError::new(kind, format!("{what}: {self}"))
        }
    }
}

/// Whether `err` means the CDP connection is gone rather than that one
/// command failed: the websocket broke or closed, or `chromiumoxide`'s
/// handler task (which owns the websocket) stopped, so the command never
/// reached the browser or its answer can never arrive. A `Timeout` is not
/// counted: a page busy in a long script also times out, and stays usable.
/// Pure. Unit-tested.
pub fn session_lost(err: &chromiumoxide::error::CdpError) -> bool {
    use chromiumoxide::error::CdpError;
    matches!(
        err,
        CdpError::Ws(_) | CdpError::Io(_) | CdpError::ChannelSendError(_) | CdpError::NoResponse
    )
}

fn evaluate_failed(source: chromiumoxide::error::CdpError) -> BrowserError {
    if session_lost(&source) {
        BrowserError::SessionLost(source)
    } else {
        BrowserError::Evaluate(source)
    }
}

/// `value` as the `expected` JSON type `pick` extracts, or
/// [`BrowserError::UnexpectedResult`] naming what it was instead. `None` is
/// a result CDP returned no value for (`undefined`). Pure. Unit-tested.
fn expect_result<T>(
    value: Option<&serde_json::Value>,
    expected: &'static str,
    pick: impl FnOnce(&serde_json::Value) -> Option<T>,
) -> Result<T, BrowserError> {
    match value {
        Some(value) => pick(value).ok_or_else(|| BrowserError::UnexpectedResult {
            expected,
            got: describe_value(value),
        }),
        None => Err(BrowserError::UnexpectedResult {
            expected,
            got: "undefined".to_owned(),
        }),
    }
}

/// A short rendering of an unexpected evaluate result for an error message.
fn describe_value(value: &serde_json::Value) -> String {
    let rendered = value.to_string();
    let mut shown: String = rendered.chars().take(80).collect();
    if shown.len() < rendered.len() {
        shown.push_str("...");
    }
    shown
}

/// A live connection to the Chromium container's CDP endpoint. Owns the
/// background task that must continuously drive `chromiumoxide`'s
/// `Handler` -- without it, nothing chromiumoxide does resolves at all,
/// which is why this type cannot be constructed any other way than
/// [`BrowserSession::connect`].
pub struct BrowserSession {
    browser: Browser,
    handler_task: tokio::task::JoinHandle<()>,
    download_task: Option<tokio::task::JoinHandle<()>>,
    downloads: Arc<Mutex<Vec<EventDownloadProgress>>>,
}

impl BrowserSession {
    /// `cdp_url` is the container's published CDP port as an `http://`
    /// URL (e.g. `http://127.0.0.1:49222`) -- `chromiumoxide` resolves the
    /// actual websocket URL from `/json/version` itself.
    pub async fn connect(cdp_url: &str) -> Result<Self, BrowserError> {
        let (browser, mut handler) =
            Browser::connect(cdp_url)
                .await
                .map_err(|source| BrowserError::Connect {
                    url: cdp_url.to_owned(),
                    source,
                })?;

        let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        Ok(Self {
            browser,
            handler_task,
            download_task: None,
            downloads: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Enables named downloads into `host_visible_dir` (the *container-side*
    /// path a bind mount makes visible on the host too -- see
    /// `u2s-aem-verify-core::flow`, which is what lets [`Self::wait_for_download`]
    /// hand back a path this process can simply read) and starts buffering
    /// `Browser.downloadProgress` events. Call once per session, before
    /// whatever action triggers the download.
    pub async fn enable_downloads(&mut self, container_side_dir: &str) -> Result<(), BrowserError> {
        let params = SetDownloadBehaviorParams::builder()
            .behavior(SetDownloadBehaviorBehavior::AllowAndName)
            .download_path(container_side_dir)
            .events_enabled(true)
            .build()
            .map_err(BrowserError::DownloadBehavior)?;
        self.browser
            .execute(params)
            .await
            .map_err(|source| BrowserError::DownloadBehavior(source.to_string()))?;

        let mut stream = self
            .browser
            .event_listener::<EventDownloadProgress>()
            .await
            .map_err(|source| BrowserError::Listen {
                what: "download progress",
                source,
            })?;
        let downloads = Arc::clone(&self.downloads);
        self.download_task = Some(tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                downloads
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push((*event).clone());
            }
        }));
        Ok(())
    }

    /// Blocks until a download this session observed reaches `completed` or
    /// `canceled`, or `timeout` elapses. Returns the download's guid --
    /// `AllowAndName` names the file on disk after it, so the caller
    /// already knows the path: `<download_dir>/<guid>`.
    pub async fn wait_for_download(&self, timeout: Duration) -> Result<String, BrowserError> {
        use chromiumoxide::cdp::browser_protocol::browser::DownloadProgressState;

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let downloads = self
                    .downloads
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(done) = downloads
                    .iter()
                    .find(|d| d.state == DownloadProgressState::Completed)
                {
                    return Ok(done.guid.clone());
                }
                if downloads
                    .iter()
                    .any(|d| d.state == DownloadProgressState::Canceled)
                {
                    return Err(BrowserError::Listen {
                        what: "a download that was canceled rather than completed",
                        source: chromiumoxide::error::CdpError::NoResponse,
                    });
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(BrowserError::DownloadTimedOut(timeout));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Opens a page, injects `cookies` (an AEM author login token, in
    /// `u2s-aem-verify-core`'s usage) before navigating so the first request
    /// already carries them, navigates to `url`, and starts buffering
    /// console/exception/network events for the page's lifetime.
    ///
    /// `basic_auth`, when given, is injected as an `Authorization: Basic`
    /// header on every request this page makes -- the same authentication
    /// scheme `u2s-aem-verify-core::aem_client` already uses for the Package
    /// Manager and DoR endpoints, so the flow authenticates one way
    /// throughout rather than a header for HTTP calls and a separate,
    /// AEM-version-specific login-token cookie for the browser.
    pub async fn open(
        &self,
        url: &str,
        basic_auth: Option<(&str, &str)>,
    ) -> Result<PageHandle, BrowserError> {
        let page = self
            .browser
            .new_page("about:blank")
            .await
            .map_err(|source| BrowserError::OpenPage {
                url: url.to_owned(),
                source,
            })?;
        // Before anything navigates: a dialog raised while the form loads
        // would otherwise stall the navigation itself (module doc).
        let dialogs = answer_dialogs(&page).await?;

        if let Some((user, password)) = basic_auth {
            use base64::Engine;
            let credentials =
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
            let params = SetExtraHttpHeadersParams::new(Headers::new(serde_json::json!({
                "Authorization": format!("Basic {credentials}"),
            })));
            page.execute(params)
                .await
                .map_err(BrowserError::SetAuthHeader)?;
        }

        page.goto(url)
            .await
            .map_err(|source| BrowserError::OpenPage {
                url: url.to_owned(),
                source,
            })?;
        page.wait_for_navigation()
            .await
            .map_err(|source| BrowserError::OpenPage {
                url: url.to_owned(),
                source,
            })?;

        PageHandle::new(page, dialogs).await
    }

    /// Drops the connection and leaves the browser running.
    ///
    /// Never `Browser::close`: that is CDP's `Browser.close`, which ends the
    /// Chromium process itself, and the browser this session connected to is
    /// not its own -- it is the verifier session's container, shared by every
    /// form a caller opens on it, so closing it left the next `verify_open`
    /// with nothing to connect to. A caller closes its own pages first
    /// ([`PageHandle::close`]); the container's lifecycle is the session's.
    pub async fn disconnect(mut self) {
        if let Some(task) = self.download_task.take() {
            task.abort();
        }
        self.handler_task.abort();
    }
}

/// One open page plus the console/exception/network events observed on it
/// since it opened. Scoped to a page, not the whole session, so a step's
/// findings never bleed into the next step's screenshot.
pub struct PageHandle {
    page: Page,
    console_errors: Arc<Mutex<Vec<String>>>,
    failed_requests: Arc<Mutex<Vec<FailedRequest>>>,
    dialogs: Arc<Mutex<Vec<Dialog>>>,
    _dialog_task: tokio::task::JoinHandle<()>,
    _console_task: tokio::task::JoinHandle<()>,
    _exception_task: tokio::task::JoinHandle<()>,
    _response_task: tokio::task::JoinHandle<()>,
    _loading_failed_task: tokio::task::JoinHandle<()>,
}

/// Starts answering `page`'s native dialogs (module doc), returning the
/// buffer they are recorded in and the task that drains the event stream.
async fn answer_dialogs(
    page: &Page,
) -> Result<(Arc<Mutex<Vec<Dialog>>>, tokio::task::JoinHandle<()>), BrowserError> {
    let dialogs = Arc::new(Mutex::new(Vec::new()));
    let mut dialog_stream = page
        .event_listener::<EventJavascriptDialogOpening>()
        .await
        .map_err(|source| BrowserError::Listen {
            what: "JS dialogs",
            source,
        })?;
    let dialog_sink = Arc::clone(&dialogs);
    let answering_page = page.clone();
    let task = tokio::spawn(async move {
        while let Some(event) = dialog_stream.next().await {
            dialog_sink
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(Dialog {
                    kind: event.r#type.as_ref().to_owned(),
                    message: event.message.clone(),
                });
            let mut answer = HandleJavaScriptDialogParams::new(true);
            answer.prompt_text = event.default_prompt.clone();
            if let Err(err) = answering_page.execute(answer).await {
                // Nothing to return this to: the dialog stays open and the
                // next evaluate fails or times out with its own error.
                log::warn!(
                    "{}: could not answer a {} dialog: {err}",
                    crate::LOG_PREFIX,
                    event.r#type.as_ref()
                );
            }
        }
    });
    Ok((dialogs, task))
}

impl PageHandle {
    async fn new(
        page: Page,
        (dialogs, dialog_task): (Arc<Mutex<Vec<Dialog>>>, tokio::task::JoinHandle<()>),
    ) -> Result<Self, BrowserError> {
        let console_errors = Arc::new(Mutex::new(Vec::new()));
        let failed_requests = Arc::new(Mutex::new(Vec::new()));

        let mut console_stream = page
            .event_listener::<EventConsoleApiCalled>()
            .await
            .map_err(|source| BrowserError::Listen {
                what: "console messages",
                source,
            })?;
        let console_sink = Arc::clone(&console_errors);
        let console_task = tokio::spawn(async move {
            while let Some(event) = console_stream.next().await {
                if event.r#type == ConsoleApiCalledType::Error {
                    let text = event
                        .args
                        .iter()
                        .filter_map(|arg| {
                            arg.description
                                .clone()
                                .or_else(|| arg.value.as_ref().map(|v| v.to_string()))
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    console_sink
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(text);
                }
            }
        });

        let mut exception_stream = page
            .event_listener::<EventExceptionThrown>()
            .await
            .map_err(|source| BrowserError::Listen {
                what: "JS exceptions",
                source,
            })?;
        let exception_sink = Arc::clone(&console_errors);
        let exception_task = tokio::spawn(async move {
            while let Some(event) = exception_stream.next().await {
                exception_sink
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(event.exception_details.text.clone());
            }
        });

        let mut response_stream = page
            .event_listener::<EventResponseReceived>()
            .await
            .map_err(|source| BrowserError::Listen {
                what: "network responses",
                source,
            })?;
        let response_sink = Arc::clone(&failed_requests);
        let response_task = tokio::spawn(async move {
            while let Some(event) = response_stream.next().await {
                if event.response.status >= 400 {
                    response_sink
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(FailedRequest {
                            url: event.response.url.clone(),
                            status: Some(event.response.status),
                        });
                }
            }
        });

        let mut loading_failed_stream =
            page.event_listener::<EventLoadingFailed>()
                .await
                .map_err(|source| BrowserError::Listen {
                    what: "loading failures",
                    source,
                })?;
        let loading_failed_sink = Arc::clone(&failed_requests);
        let loading_failed_task = tokio::spawn(async move {
            while let Some(event) = loading_failed_stream.next().await {
                loading_failed_sink
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(FailedRequest {
                        url: event.error_text.clone(),
                        status: None,
                    });
            }
        });

        Ok(Self {
            page,
            console_errors,
            failed_requests,
            dialogs,
            _dialog_task: dialog_task,
            _console_task: console_task,
            _exception_task: exception_task,
            _response_task: response_task,
            _loading_failed_task: loading_failed_task,
        })
    }

    pub async fn wait(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    pub async fn screenshot_full_page(&self) -> Result<Vec<u8>, BrowserError> {
        self.screenshot(ScreenshotArea::FullPage, ImageFormat::Png)
            .await
    }

    /// Captures `area` in `format`. A [`ScreenshotArea::Clip`] is captured
    /// with `capture_beyond_viewport(true)` so the rect is honoured in
    /// document coordinates rather than clamped to whatever is currently
    /// scrolled into view -- see [`ScreenshotArea`]'s own doc for why
    /// `FullPage` and `Clip` cannot both be requested at once.
    pub async fn screenshot(
        &self,
        area: ScreenshotArea,
        format: ImageFormat,
    ) -> Result<Vec<u8>, BrowserError> {
        let mut builder = ScreenshotParams::builder().format(format.cdp_format());
        if let ImageFormat::Jpeg { quality } = format {
            builder = builder.quality(i64::from(quality));
        }
        builder = match area {
            ScreenshotArea::FullPage => builder.full_page(true),
            ScreenshotArea::Clip(rect) => builder.capture_beyond_viewport(true).clip(Viewport {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                scale: 1.0,
            }),
        };
        self.page
            .screenshot(builder.build())
            .await
            .map_err(BrowserError::Screenshot)
    }

    /// Evaluates `expression`, which must produce a boolean: anything else
    /// is [`BrowserError::UnexpectedResult`], never read as `false`.
    pub async fn evaluate_bool(&self, expression: &str) -> Result<bool, BrowserError> {
        let result = self
            .page
            .evaluate(expression)
            .await
            .map_err(evaluate_failed)?;
        expect_result(result.value(), "boolean", serde_json::Value::as_bool)
    }

    /// Same as [`Self::evaluate_bool`] but for an expression that returns a
    /// string (a `JSON.stringify(...)` result, typically). A result that is
    /// not a string is [`BrowserError::UnexpectedResult`], never `""`: an
    /// empty string read as "nothing there" hid a script that never ran.
    pub async fn evaluate_string(&self, expression: &str) -> Result<String, BrowserError> {
        let result = self
            .page
            .evaluate(expression)
            .await
            .map_err(evaluate_failed)?;
        expect_result(result.value(), "string", |value| {
            value.as_str().map(str::to_owned)
        })
    }

    /// Every console-error or uncaught-exception message observed since
    /// this page opened, in the order they arrived.
    pub fn console_errors(&self) -> Vec<String> {
        self.console_errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Every response with a 4xx/5xx status, or a request that failed to
    /// load at all, observed since this page opened.
    pub fn failed_requests(&self) -> Vec<FailedRequest> {
        self.failed_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Every native dialog this page raised since it opened, in order.
    /// Each was already answered when it opened (module doc).
    pub fn dialogs(&self) -> Vec<Dialog> {
        self.dialogs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub async fn close(self) {
        let _ = self.page.close().await;
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// No Chromium instance is assumed reachable for this crate's own test
    /// run -- see the crate's module doc and `u2s-aem-verify-core`'s
    /// `#[ignore]`d live tests for the CDP-backed coverage. This only
    /// exercises the pure path composition.

    #[test]
    fn only_a_broken_connection_counts_as_a_lost_session() {
        use chromiumoxide::error::CdpError;
        assert!(session_lost(&CdpError::NoResponse));
        assert!(session_lost(&CdpError::from(futures::channel::oneshot::Canceled)));
        assert!(session_lost(&CdpError::Io(std::io::Error::other("broken pipe"))));
        assert!(!session_lost(&CdpError::Timeout), "a busy page also times out");
        assert!(!session_lost(&CdpError::msg("Cannot find context with specified id")));
        assert!(evaluate_failed(CdpError::NoResponse).is_session_lost());
        assert!(!evaluate_failed(CdpError::Timeout).is_session_lost());
    }

    #[test]
    fn a_lost_session_becomes_its_own_actionable_error() {
        let lost = BrowserError::SessionLost(chromiumoxide::error::CdpError::NoResponse)
            .into_verify_error("reading the controls", ErrorKind::FormNotFound);
        assert_eq!(lost.kind, ErrorKind::BrowserSessionLost);
        assert!(
            lost.message.starts_with("the browser session was lost: close it and open the form again"),
            "{}",
            lost.message
        );

        let other = BrowserError::UnexpectedResult {
            expected: "string",
            got: "undefined".to_owned(),
        }
        .into_verify_error("reading the controls", ErrorKind::FormNotFound);
        assert_eq!(other.kind, ErrorKind::FormNotFound);
        assert_eq!(
            other.message,
            "reading the controls: the script evaluated to undefined, not a string"
        );
    }

    #[test]
    fn an_evaluate_result_of_the_wrong_type_is_an_error_not_a_default() {
        let string = serde_json::json!("{}");
        assert_eq!(
            expect_result(Some(&string), "string", |v| v.as_str().map(str::to_owned)).unwrap(),
            "{}"
        );
        let err = expect_result(None, "string", |v| v.as_str().map(str::to_owned)).unwrap_err();
        assert_eq!(err.to_string(), "the script evaluated to undefined, not a string");
        let number = serde_json::json!(3);
        let err = expect_result(Some(&number), "boolean", serde_json::Value::as_bool).unwrap_err();
        assert_eq!(err.to_string(), "the script evaluated to 3, not a boolean");
        let long = serde_json::Value::String("x".repeat(200));
        let err = expect_result(Some(&long), "boolean", serde_json::Value::as_bool).unwrap_err();
        assert!(err.to_string().len() < 140, "a long result is shortened: {err}");
    }

    #[test]
    fn image_format_reports_its_own_mime_and_extension() {
        assert_eq!(ImageFormat::Png.mime(), "image/png");
        assert_eq!(ImageFormat::Png.ext(), "png");
        assert_eq!(ImageFormat::Jpeg { quality: 82 }.mime(), "image/jpeg");
        assert_eq!(ImageFormat::Jpeg { quality: 82 }.ext(), "jpg");
    }

    /// Awaits `future`, failing the test instead of hanging when a dialog
    /// was left open and stalled the page.
    async fn bounded<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(20), future)
            .await
            .unwrap_or_else(|_| panic!("{what} hung: a dialog was left open"))
    }

    /// Native dialogs never stall the page: one raised while the page
    /// loads, one raised later from a timer, and one raised synchronously
    /// inside an evaluate are all answered and recorded, and the evaluate
    /// that raised the `prompt` gets the prompt's own default text back.
    /// Needs a headless Chromium, e.g.
    /// `docker run -d -p 127.0.0.1::9222 chromedp/headless-shell:stable`,
    /// with `U2S_VERIFY_LIVE_CDP_URL=http://127.0.0.1:<port>`.
    #[tokio::test]
    #[ignore = "needs a real headless Chromium at U2S_VERIFY_LIVE_CDP_URL"]
    async fn native_dialogs_are_answered_and_recorded() {
        let cdp_url = std::env::var("U2S_VERIFY_LIVE_CDP_URL")
            .expect("U2S_VERIFY_LIVE_CDP_URL must name a headless Chromium's CDP endpoint");
        let session = BrowserSession::connect(&cdp_url).await.expect("connect");

        let page = bounded(
            "opening a page that alerts on load",
            session.open("data:text/html,<script>alert('on load')</script><p>form</p>", None),
        )
        .await
        .expect("the page opens");

        let answered = bounded(
            "an evaluate that raised a prompt",
            page.evaluate_string("prompt('your name?', 'the default')"),
        )
        .await
        .expect("the evaluate answers");
        assert_eq!(answered, "the default");

        page.evaluate_bool("setTimeout(function() { confirm('later'); }, 0); true")
            .await
            .expect("scheduling the confirm");
        page.wait(Duration::from_millis(500)).await;
        let still = bounded("an evaluate after a deferred confirm", page.evaluate_bool("true"))
            .await
            .expect("the evaluate answers");
        assert!(still);

        let kinds: Vec<(String, String)> = page
            .dialogs()
            .into_iter()
            .map(|dialog| (dialog.kind, dialog.message))
            .collect();
        assert_eq!(
            kinds,
            [
                ("alert".to_owned(), "on load".to_owned()),
                ("prompt".to_owned(), "your name?".to_owned()),
                ("confirm".to_owned(), "later".to_owned()),
            ]
        );

        page.close().await;
        session.disconnect().await;
    }

    #[test]
    fn screenshot_area_full_page_and_clip_are_distinct_values() {
        let clip = ScreenshotArea::Clip(ClipRect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        });
        assert_ne!(clip, ScreenshotArea::FullPage);
    }
}
