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
        js_protocol::runtime::EvaluateParams,
    },
    handler::HandlerConfig,
};
use futures_util::StreamExt;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinHandle};
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
    document_json: String,
    channel_json: String,
    data_json: String,
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
        use ivy_contracts::{IvyError, IvyJson, json::stringify, limits::MANAGEMENT_FRAME_BYTES};
        if !(100..=4096).contains(&width)
            || !(100..=4096).contains(&height)
            || u64::from(width) * u64::from(height) > png::MAX_PIXELS
        {
            return Err(IvyError::new(
                "invalid_arguments",
                "At most 8 million pixels; dimensions must be 100..4096.",
            ));
        }
        let channel = uuid::Uuid::new_v4().to_string();
        let document = crate::document::dashboard_document(html, &channel.as_str().into())?;
        Ok(Self {
            document_json: stringify(&IvyJson::String(document), MANAGEMENT_FRAME_BYTES)?,
            channel_json: stringify(&channel.into(), MANAGEMENT_FRAME_BYTES)?,
            // JSON text must be parsed, not evaluated as an object literal:
            // an authored __proto__ key is data, not a prototype setter.
            data_json: stringify(
                &IvyJson::from(stringify(&playwright_data(data), MANAGEMENT_FRAME_BYTES)?),
                MANAGEMENT_FRAME_BYTES,
            )?,
            width,
            height,
            color_mode,
        })
    }
}

// Playwright 1.63 reconstructs argument objects with property assignment. Its
// __proto__ setter drops that own key before postMessage's structured clone.
// Preserve this observed behavior until a data-preservation fix is approved.
// Walk iteratively so a valid deep JSON value does not consume the native stack.
fn playwright_data(data: &ivy_contracts::IvyJson) -> ivy_contracts::IvyJson {
    use ivy_contracts::{IvyJson, JsString};
    let mut data = data.clone();
    let key = JsString::from("__proto__");
    let mut pending = vec![&mut data];
    while let Some(value) = pending.pop() {
        match value {
            IvyJson::Object(fields) => {
                fields.retain(|(name, _)| name != &key);
                pending.extend(fields.iter_mut().map(|(_, value)| value));
            }
            IvyJson::Array(values) => pending.extend(values.iter_mut()),
            _ => {}
        }
    }
    data
}

pub struct CdpRenderer {
    browser: Arc<Browser>,
    pump: JoinHandle<()>,
    available: Arc<AtomicBool>,
    permit: Arc<Semaphore>,
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
            permit: Arc::new(Semaphore::new(1)),
            shutdown: CancellationToken::new(),
        }
    }

    pub fn ready(&self) -> bool {
        self.available.load(Ordering::Acquire) && !self.pump.is_finished()
    }

    pub async fn render(
        &self,
        input: RenderInput,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, RenderError> {
        let permit = self
            .permit
            .clone()
            .try_acquire_owned()
            .map_err(|_| RenderError::Busy)?;
        if !self.ready() {
            return Err(RenderError::Unavailable);
        }
        if cancel.is_cancelled() {
            return Err(RenderError::Cancelled);
        }
        let browser = self.browser.clone();
        let live = self.available.clone();
        // Spawn the lease owner: dropping the request future cannot abandon its
        // context or release admission before bounded disposal has completed.
        let cancel = cancel.clone();
        let shutdown = self.shutdown.clone();
        let (mut tx, mut rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            // Creation itself is owned even if the caller drops while CDP is
            // accepting it; after its bounded reply we still dispose the result.
            let context = match browser
                .create_browser_context(CreateBrowserContextParams::default())
                .await
            {
                Ok(context) => context,
                Err(error) => {
                    // A lost reply cannot establish that no context was made.
                    // Require the enclosing owner to stop/recover this browser.
                    live.store(false, Ordering::Release);
                    let _ = tx.send(Err(error.into()));
                    return;
                }
            };
            let mode = input.color_mode;
            let work = async {
                let target = CreateTargetParams::builder()
                    .url("about:blank")
                    .browser_context_id(context.clone())
                    .build()
                    .expect("fixed target parameters");
                let page = browser.new_page(target).await?;
                render_page(&page, &input).await
            };
            let result = tokio::select! {
                biased;
                _=tx.closed()=>Err(RenderError::Cancelled),
                _=cancel.cancelled()=>Err(RenderError::Cancelled),
                _=shutdown.cancelled()=>Err(RenderError::Cancelled),
                result=work=>result,
            };
            let cleanup =
                tokio::time::timeout(DISPOSE, browser.dispose_browser_context(context)).await;
            let disposed = matches!(cleanup, Ok(Ok(())));
            if !disposed {
                live.store(false, Ordering::Release);
            }
            // The original renderer's finally block rejects the request when
            // context disposal fails, even if screenshot capture succeeded.
            let result = if disposed {
                result
            } else {
                Err(RenderError::Unavailable)
            };
            // Keep the permit while native PNG work runs. A cancelled future
            // must not leave a blocking conversion behind and admit another.
            let result = match result {
                Ok(_) if cancel.is_cancelled() || shutdown.is_cancelled() || tx.is_closed() => {
                    Err(RenderError::Cancelled)
                }
                Ok(bytes) => {
                    match tokio::task::spawn_blocking(move || png::convert(bytes, mode)).await {
                        Ok(result) => result.map_err(RenderError::Png),
                        Err(_) => Err(RenderError::Unavailable),
                    }
                }
                error => error,
            };
            let result = if cancel.is_cancelled() || shutdown.is_cancelled() || tx.is_closed() {
                Err(RenderError::Cancelled)
            } else {
                result
            };
            drop(permit);
            let _ = tx.send(result);
        });
        (&mut rx).await.map_err(|_| RenderError::Unavailable)?
    }

    /// Cancel outstanding work and request cooperative browser shutdown. The
    /// enclosing process owner must still observe exit or perform forced stop.
    pub async fn close(&self) -> Result<(), RenderError> {
        self.available.store(false, Ordering::Release);
        self.shutdown.cancel();
        let _permit = tokio::time::timeout(DISPOSE, self.permit.acquire())
            .await
            .map_err(|_| RenderError::Timeout)?
            .map_err(|_| RenderError::Unavailable)?;
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

async fn render_page(page: &Page, input: &RenderInput) -> Result<Vec<u8>, RenderError> {
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
    let fonts: serde_json::Value = serde_json::from_str(include_str!("../assets/fonts.json"))
        .expect("embedded Playwright font families");
    let platform = if cfg!(target_os = "windows") {
        "win"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };
    let fonts: SetFontFamiliesParams =
        serde_json::from_value(fonts[platform].clone()).expect("pinned CDP font parameters");
    page.execute(fonts).await?;
    page.execute(SetDeviceMetricsOverrideParams::new(
        input.width as i64,
        input.height as i64,
        1.0,
        false,
    ))
    .await?;
    page.set_content(OUTER).await?;
    let mode = match input.color_mode {
        ColorMode::Color => "color",
        ColorMode::Grayscale => "grayscale",
        ColorMode::Monochrome => "monochrome",
    };
    let expression = format!(
        r#"(() => {{
      const frame=document.querySelector('iframe'), channel={channel};
      const data=JSON.parse({data}), output={{width:{width},height:{height},colorMode:'{mode}',mode:'image'}};
      return new Promise(resolve => {{
        window.addEventListener('message', event => {{
          if(event.source!==frame.contentWindow || event.data?.channel!==channel) return;
          if(event.data.type==='ready') frame.contentWindow.postMessage({{channel,type:'update',sequence:1,data,output}},'*');
          if(event.data.type==='rendered') resolve({{state:'done'}});
          if(event.data.type==='error') resolve({{state:'error',messageJson:JSON.stringify(event.data.message)}});
        }});
        frame.srcdoc={document};
      }});
    }})()"#,
        channel = input.channel_json,
        data = input.data_json,
        width = input.width,
        height = input.height,
        document = input.document_json
    );
    let evaluated = tokio::time::timeout(
        WAIT,
        page.evaluate_expression(
            EvaluateParams::builder()
                .expression(expression)
                .await_promise(true)
                .return_by_value(true)
                .build()
                .expect("fixed evaluation parameters"),
        ),
    )
    .await
    .map_err(|_| RenderError::Timeout)??;
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
