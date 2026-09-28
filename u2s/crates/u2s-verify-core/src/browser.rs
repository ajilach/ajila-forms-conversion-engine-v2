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
//! never across an `.await`.

use std::path::Path;
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
use chromiumoxide::cdp::browser_protocol::page::Viewport;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::{Page, ScreenshotParams};
use futures::StreamExt;

use crate::types::FailedRequest;

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

        PageHandle::new(page).await
    }

    /// Closes every page and the connection. Aborts the handler task last
    /// so an in-flight `close()` call still has somewhere to send its
    /// command.
    pub async fn close(mut self) {
        if let Some(task) = self.download_task.take() {
            task.abort();
        }
        let _ = self.browser.close().await;
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
    _console_task: tokio::task::JoinHandle<()>,
    _exception_task: tokio::task::JoinHandle<()>,
    _response_task: tokio::task::JoinHandle<()>,
    _loading_failed_task: tokio::task::JoinHandle<()>,
}

impl PageHandle {
    async fn new(page: Page) -> Result<Self, BrowserError> {
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

    pub async fn evaluate_bool(&self, expression: &str) -> Result<bool, BrowserError> {
        let result =
            self.page
                .evaluate(expression)
                .await
                .map_err(|source| BrowserError::Listen {
                    what: "an evaluate result",
                    source,
                })?;
        Ok(result.value().and_then(|v| v.as_bool()).unwrap_or(false))
    }

    /// Same as [`Self::evaluate_bool`] but for an expression that returns a
    /// string -- `flow.rs` uses this for diagnostics (e.g. reporting why a
    /// readiness signal never appeared) that a caller wants to read, not
    /// just branch on.
    pub async fn evaluate_string(&self, expression: &str) -> Result<String, BrowserError> {
        let result =
            self.page
                .evaluate(expression)
                .await
                .map_err(|source| BrowserError::Listen {
                    what: "an evaluate result",
                    source,
                })?;
        Ok(result
            .value()
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned())
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

    pub async fn close(self) {
        let _ = self.page.close().await;
    }
}

/// The path a completed download landed at, given the host-visible bind
/// mount directory `u2s-aem-verify-core::flow` configured and the guid
/// [`BrowserSession::wait_for_download`] returned.
pub fn download_path(host_downloads_dir: &Path, guid: &str) -> std::path::PathBuf {
    host_downloads_dir.join(guid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No Chromium instance is assumed reachable for this crate's own test
    /// run -- see the crate's module doc and `u2s-aem-verify-core`'s
    /// `#[ignore]`d live tests for the CDP-backed coverage. This only
    /// exercises the pure path composition.
    #[test]
    fn download_path_joins_the_directory_and_guid() {
        let dir = Path::new("/downloads");
        let path = download_path(dir, "abcd-1234");
        assert_eq!(path, std::path::PathBuf::from("/downloads/abcd-1234"));
    }

    #[test]
    fn image_format_reports_its_own_mime_and_extension() {
        assert_eq!(ImageFormat::Png.mime(), "image/png");
        assert_eq!(ImageFormat::Png.ext(), "png");
        assert_eq!(ImageFormat::Jpeg { quality: 82 }.mime(), "image/jpeg");
        assert_eq!(ImageFormat::Jpeg { quality: 82 }.ext(), "jpg");
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
