//! Dashboard-specific CDP rendering. The host runtime owns the browser process;
//! this module owns its connection, temporary contexts and screenshot lifecycle.
use crate::{png, raster::ColorMode};
use chromiumoxide::{
    Browser, Page,
    cdp::{
        browser_protocol::{
            emulation::{SetDeviceMetricsOverrideParams, SetFocusEmulationEnabledParams},
            fetch::{EventRequestPaused, FailRequestParams},
            network::{ErrorReason, SetBypassServiceWorkerParams},
            page::SetFontFamiliesParams,
            target::{CreateBrowserContextParams, CreateTargetParams},
        },
        js_protocol::runtime::{CallArgument, CallFunctionOnParams, EvaluateParams},
    },
    handler::HandlerConfig,
};
use futures_util::StreamExt;
use std::{
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(15);
const DISPOSE: Duration = Duration::from_secs(5);
const OUTER: &str = "<html><body style=\"margin:0\"><iframe sandbox=\"allow-scripts\" style=\"border:0;width:100vw;height:100vh;display:block\"></iframe></body></html>";
const PREPARE: &str = include_str!("../assets/screenshot.js");

#[derive(Debug)]
pub enum RenderError {
    Driver(chromiumoxide::error::CdpError),
    Png(png::PngError),
    Busy,
    Cancelled,
    Timeout,
    Callback(ivy_contracts::JsString),
    Unavailable,
}
impl From<chromiumoxide::error::CdpError> for RenderError {
    fn from(value: chromiumoxide::error::CdpError) -> Self {
        Self::Driver(value)
    }
}

impl RenderError {
    /// Preserve the original service's explicit errors. Driver/timeout/codec
    /// failures remain internal_error/unknown rather than inventing wire codes.
    pub fn into_ivy(self) -> ivy_contracts::IvyError {
        use ivy_contracts::IvyError;
        match self {
            Self::Busy => IvyError::new(
                "resource_busy",
                "An image is already rendering; retry shortly.",
            ),
            Self::Callback(message) => {
                IvyError::new("dashboard_render_failed", "Dashboard rendering failed.")
                    .with_js_message(message)
            }
            Self::Png(png::PngError::TooLarge) => IvyError::new(
                "resource_exhausted",
                "PNG exceeds 8 MiB; reduce dimensions.",
            ),
            _ => IvyError::internal(),
        }
    }
}

/// Authored values cross CDP as shared-serializer expressions, retaining UTF-16.
pub struct RenderInput {
    payload_json: String,
    width: u32,
    height: u32,
    color_mode: ColorMode,
}
impl RenderInput {
    pub fn new(
        html: &ivy_contracts::JsString,
        data: &ivy_contracts::IvyJson,
        width: u32,
        height: u32,
        color_mode: ColorMode,
    ) -> ivy_contracts::IvyResult<Self> {
        use ivy_contracts::IvyError;
        if !(100..=4096).contains(&width)
            || !(100..=4096).contains(&height)
            || u64::from(width) * u64::from(height) > png::MAX_PIXELS
        {
            return Err(IvyError::new(
                "invalid_arguments",
                "At most 8 million pixels; dimensions must be 100..4096.",
            ));
        }
        Self::owned(
            html.clone(),
            data.clone(),
            crate::validation::OutputOptions {
                width,
                height,
                mode: color_mode,
            },
        )
    }
    pub fn owned(
        html: ivy_contracts::JsString,
        data: ivy_contracts::IvyJson,
        output: crate::validation::OutputOptions,
    ) -> ivy_contracts::IvyResult<Self> {
        use ivy_contracts::{IvyJson, json::stringify, limits::MANAGEMENT_FRAME_BYTES};
        let channel = uuid::Uuid::new_v4().to_string();
        let document = crate::document::dashboard_document(&html, &channel.as_str().into())?;
        Ok(Self {
            payload_json: stringify(
                &IvyJson::object([
                    ("channel", channel.into()),
                    ("document", IvyJson::String(document)),
                    ("data", data),
                    (
                        "output",
                        IvyJson::object([
                            ("width", IvyJson::Number(output.width.into())),
                            ("height", IvyJson::Number(output.height.into())),
                            ("colorMode", output.color_mode().into()),
                            ("mode", "image".into()),
                        ]),
                    ),
                ]),
                MANAGEMENT_FRAME_BYTES,
            )?,
            width: output.width,
            height: output.height,
            color_mode: output.mode,
        })
    }
}

pub struct CdpRenderer {
    browser: Arc<Browser>,
    pump: JoinHandle<()>,
    available: Arc<AtomicBool>,
    shutdown: CancellationToken,
}

impl CdpRenderer {
    /// Endpoint must belong to the service's newly launched, local-only browser.
    pub async fn connect(endpoint: String) -> Result<Self, RenderError> {
        let (browser, handler) = Browser::connect_with_config(
            endpoint,
            HandlerConfig {
                ignore_https_errors: false,
                request_intercept: true,
                cache_enabled: false,
                request_timeout: WAIT,
                viewport: None,
                ..Default::default()
            },
        )
        .await?;
        Ok(Self::from_connection(browser, handler))
    }

    pub fn from_connection(browser: Browser, mut handler: chromiumoxide::Handler) -> Self {
        let available = Arc::new(AtomicBool::new(true));
        let live = available.clone();
        let pump = tokio::spawn(async move {
            while let Some(result) = handler.next().await {
                if result.is_err() {
                    break;
                }
            }
            live.store(false, Ordering::Release);
        });
        Self {
            browser: Arc::new(browser),
            pump,
            available,
            shutdown: CancellationToken::new(),
        }
    }

    pub fn ready(&self) -> bool {
        self.available.load(Ordering::Acquire) && !self.pump.is_finished()
    }

    /// Called by the process owner, which retains request admission through
    /// disposal and conversion even when the observing client disappears.
    pub async fn render(
        &self,
        input: RenderInput,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, RenderError> {
        if !self.ready() {
            return Err(RenderError::Unavailable);
        }
        if cancel.is_cancelled() {
            return Err(RenderError::Cancelled);
        }
        let context = match self
            .browser
            .create_browser_context(CreateBrowserContextParams::default())
            .await
        {
            Ok(context) => context,
            Err(error) => {
                self.available.store(false, Ordering::Release);
                return Err(error.into());
            }
        };
        let mode = input.color_mode;
        let work = async {
            let target = CreateTargetParams::builder()
                .url("about:blank")
                .browser_context_id(context.clone())
                .build()
                .expect("fixed target parameters");
            let page = self.browser.new_page(target).await?;
            render_page(&page, input).await
        };
        let result = tokio::select! {
            biased;
            _=cancel.cancelled()=>Err(RenderError::Cancelled),
            _=self.shutdown.cancelled()=>Err(RenderError::Cancelled),
            result=work=>result,
        };
        if !matches!(
            tokio::time::timeout(DISPOSE, self.browser.dispose_browser_context(context)).await,
            Ok(Ok(()))
        ) {
            self.available.store(false, Ordering::Release);
            return Err(RenderError::Unavailable);
        }
        let bytes = result?;
        if cancel.is_cancelled() || self.shutdown.is_cancelled() {
            return Err(RenderError::Cancelled);
        }
        tokio::task::spawn_blocking(move || png::convert(bytes, mode))
            .await
            .map_err(|_| RenderError::Unavailable)?
            .map_err(RenderError::Png)
    }
    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    /// Cancel outstanding work and request cooperative browser shutdown. The
    /// enclosing process owner must still observe exit or perform forced stop.
    pub async fn close(&self) -> Result<(), RenderError> {
        self.available.store(false, Ordering::Release);
        self.shutdown.cancel();
        self.browser
            .execute(chromiumoxide::cdp::browser_protocol::browser::CloseParams::default())
            .await?;
        Ok(())
    }
}

impl Drop for CdpRenderer {
    fn drop(&mut self) {
        self.available.store(false, Ordering::Release);
        self.shutdown.cancel();
        self.pump.abort();
        // The containing HostExecutor process lease still stops/reaps Chromium.
    }
}

async fn render_page(page: &Page, input: RenderInput) -> Result<Vec<u8>, RenderError> {
    let mut requests = page.event_listener::<EventRequestPaused>().await?;
    let intercept_page = page.clone();
    let intercept = tokio::spawn(async move {
        while let Some(event) = requests.next().await {
            if intercept_page
                .execute(FailRequestParams::new(
                    event.request_id.clone(),
                    ErrorReason::Aborted,
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    // Abort the listener on every return/cancel path. Context disposal is owned
    // by render's lease task and occurs after this future drops.
    struct Listener(JoinHandle<()>);
    impl Drop for Listener {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _listener = Listener(intercept);
    page.execute(SetBypassServiceWorkerParams::new(true))
        .await?;
    page.execute(SetFocusEmulationEnabledParams::new(true))
        .await?;
    // Playwright's headless defaults differ from Chromium's platform defaults
    // (notably Consolas versus Courier New on Windows). Preserve the pinned
    // families, including script-specific overrides. Apache-2.0 attribution is
    // retained alongside the screenshot preparation asset.
    static FONTS: LazyLock<SetFontFamiliesParams> = LazyLock::new(|| {
        let fonts: serde_json::Value = serde_json::from_str(include_str!("../assets/fonts.json"))
            .expect("embedded Playwright font families");
        let platform = if cfg!(target_os = "windows") {
            "win"
        } else if cfg!(target_os = "macos") {
            "mac"
        } else {
            "linux"
        };
        serde_json::from_value(fonts[platform].clone()).expect("pinned CDP font parameters")
    });
    page.execute(FONTS.clone()).await?;
    page.execute(SetDeviceMetricsOverrideParams::new(
        input.width as i64,
        input.height as i64,
        1.0,
        false,
    ))
    .await?;
    page.set_content(OUTER).await?;
    // Only fixed code is evaluated. Authored HTML/data cross CDP as a typed
    // argument containing lossless JSON; JSON.parse keeps __proto__ as data.
    let evaluated = tokio::time::timeout(WAIT, page.evaluate_function(
        CallFunctionOnParams::builder()
            .function_declaration(r#"function(payload) {
                const {channel, document, data, output} = JSON.parse(payload);
                const frame = window.document.querySelector('iframe');
                return new Promise(resolve => {
                    window.addEventListener('message', event => {
                        if(event.source!==frame.contentWindow || event.data?.channel!==channel) return;
                        if(event.data.type==='ready') frame.contentWindow.postMessage({channel,type:'update',sequence:1,data,output},'*');
                        if(event.data.type==='rendered') resolve({state:'done'});
                        if(event.data.type==='error') resolve({state:'error',messageJson:JSON.stringify(event.data.message)});
                    });
                    frame.srcdoc=document;
                });
            }"#)
            .argument(CallArgument::builder().value(serde_json::Value::String(input.payload_json)).build())
            .await_promise(true).return_by_value(true).build().expect("fixed render call")
    )).await.map_err(|_| RenderError::Timeout)??;
    let state: serde_json::Value = evaluated
        .into_value()
        .map_err(|_| RenderError::Unavailable)?;
    if state["state"] != "done" {
        let message = state["messageJson"]
            .as_str()
            .and_then(|s| {
                ivy_contracts::json::parse(s, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES).ok()
            })
            .and_then(|v| v.as_js_string().cloned())
            .filter(|s| !s.0.is_empty())
            .unwrap_or_else(|| "Dashboard rendering failed.".into());
        return Err(RenderError::Callback(message));
    }
    // Match Playwright's utility world: authored replacements of DOM APIs in
    // the main world must not affect screenshot preparation. This also reaches
    // the opaque sandbox without exposing it to the parent page.
    for frame in page.frames().await? {
        if let Some(context) = page.frame_secondary_execution_context(frame).await? {
            page.execute(
                EvaluateParams::builder()
                    .expression(format!(
                        "{PREPARE}\ninPagePrepareForScreenshots(undefined,true,true,false)"
                    ))
                    .context_id(context)
                    .build()
                    .expect("fixed screenshot preparation"),
            )
            .await?;
        }
    }
    // Playwright waits for the screenshot frame's fonts after preparing all
    // frames. A page screenshot uses the main frame.
    if let Some(context) = page.secondary_execution_context().await? {
        page.execute(
            EvaluateParams::builder()
                .expression("document.fonts.ready")
                .context_id(context)
                .await_promise(true)
                .build()
                .expect("fixed font readiness expression"),
        )
        .await?;
    }
    let bytes = tokio::time::timeout(
        WAIT,
        page.screenshot(
            chromiumoxide::page::ScreenshotParams::builder()
                .format(chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat::Png)
                .full_page(false)
                .omit_background(false)
                .build(),
        ),
    )
    .await
    .map_err(|_| RenderError::Timeout)??;
    Ok(bytes)
}
