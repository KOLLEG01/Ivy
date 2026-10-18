use base64::{Engine, prelude::BASE64_STANDARD};
use chromiumoxide::{Browser, browser::BrowserConfig};
use ivy_contracts::{IvyJson, json::parse, limits::MANAGEMENT_FRAME_BYTES};
use ivy_dashboards::{
    cdp::{CdpRenderer, RenderError, RenderInput},
    raster::ColorMode,
};
use std::{io::Cursor, time::Duration};
use tokio_util::sync::CancellationToken;

struct TestRenderer {
    renderer: std::sync::Arc<CdpRenderer>,
    browser: Browser,
    profile: std::path::PathBuf,
}

impl TestRenderer {
    async fn close(&mut self) {
        let closed = self.renderer.close().await;
        if tokio::time::timeout(Duration::from_secs(10), self.browser.wait())
            .await
            .is_err()
        {
            tokio::time::timeout(Duration::from_secs(5), self.browser.kill())
                .await
                .expect("owned test browser did not stop")
                .expect("test browser owns its process")
                .expect("owned test browser kill failed");
        }
        assert!(self.browser.try_wait().unwrap().is_some());
        std::fs::remove_dir_all(&self.profile).expect("remove exited test browser profile");
        closed.unwrap();
    }
}

async fn renderer() -> TestRenderer {
    let executable = std::env::var("DASHBOARDS_TEST_BROWSER")
        .expect("set DASHBOARDS_TEST_BROWSER to the pinned isolated headless shell");
    let profile =
        std::env::temp_dir().join(format!("ivy-dashboards-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&profile).unwrap();
    let (browser, handler) = Browser::launch(
        BrowserConfig::builder()
            .chrome_executable(executable)
            .user_data_dir(&profile)
            .viewport(None)
            .respect_https_errors()
            .request_timeout(Duration::from_secs(15))
            .build()
            .unwrap(),
    )
    .await
    .unwrap();
    let endpoint = browser.websocket_address().clone();
    // Keep the launched process handle for exit verification, but use only the
    // renderer's CDP connection so two auto-attaching drivers cannot interfere.
    drop(handler);
    let renderer = std::sync::Arc::new(CdpRenderer::connect(endpoint).await.unwrap());
    TestRenderer {
        renderer,
        browser,
        profile,
    }
}

fn input(html: &IvyJson, data: &IvyJson, mode: ColorMode) -> RenderInput {
    RenderInput::new(html.as_js_string().unwrap(), data, 120, 100, mode).unwrap()
}

fn pixels(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    let rgba = buffer[..info.buffer_size()]
        .chunks_exact(info.color_type.samples())
        .flat_map(|p| match info.color_type {
            png::ColorType::Rgb => [p[0], p[1], p[2], 255],
            png::ColorType::Rgba => [p[0], p[1], p[2], p[3]],
            png::ColorType::Grayscale => [p[0], p[0], p[0], 255],
            png::ColorType::GrayscaleAlpha => [p[0], p[0], p[0], p[1]],
            _ => panic!("unexpected palette output"),
        })
        .collect();
    (info.width, info.height, rgba)
}

#[test]
fn callback_wire_errors_keep_lone_surrogates_and_existing_error_codes() {
    let error = RenderError::Callback(ivy_contracts::JsString(vec![0xd800])).into_ivy();
    assert_eq!(error.code, "dashboard_render_failed");
    assert_eq!(
        error
            .to_wire()
            .required("message")
            .unwrap()
            .as_js_string()
            .unwrap()
            .0,
        [0xd800]
    );
    assert_eq!(RenderError::Busy.into_ivy().code, "resource_busy");
    assert_eq!(
        RenderError::Png(ivy_dashboards::png::PngError::TooLarge)
            .into_ivy()
            .code,
        "resource_exhausted"
    );
    assert_eq!(
        RenderError::Timeout.into_ivy().outcome,
        ivy_contracts::Outcome::Unknown
    );
}

#[tokio::test]
#[ignore = "requires pinned Chromium, Windows job launcher and TypeScript test dependencies"]
async fn matches_playwright_pixels_for_async_assets_sandbox_animations_and_carets() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut oracle_command = tokio::process::Command::new("node");
    if let Some(bundle) = std::env::var_os("DASHBOARDS_TEST_ORACLE") {
        oracle_command.arg(bundle);
    } else {
        oracle_command
            .args(["--import", "tsx"])
            .arg(root.join("tests/fixtures/dashboards-render-oracle.mjs"));
    }
    let output = oracle_command.current_dir(&root).output().await.unwrap();
    assert!(
        output.status.success(),
        "oracle: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let fixtures = parse(
        include_str!("../fixtures/dashboards/render.json"),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let profile_root =
        std::env::temp_dir().join(format!("ivy-dashboards-pixels-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&profile_root).unwrap();
    let runtime =
        ivy_dashboards::browser::BrowserOwner::new(ivy_dashboards::browser::BrowserOptions {
            executable: std::env::var_os("DASHBOARDS_TEST_BROWSER").unwrap().into(),
            artifact_root: profile_root.clone(),
            temporary_root: profile_root.clone(),
            job_launcher: std::env::var_os("DASHBOARDS_TEST_JOB").map(Into::into),
        });
    runtime.start().await.unwrap();
    let renderer = runtime.clone();
    let cancel = CancellationToken::new();
    for fixture in fixtures.as_array().unwrap() {
        let name = fixture.required("name").unwrap().as_str().unwrap();
        for (mode, text) in [
            (ColorMode::Color, "color"),
            (ColorMode::Grayscale, "grayscale"),
            (ColorMode::Monochrome, "monochrome"),
        ] {
            let request = input(
                fixture.required("html").unwrap(),
                fixture.required("data").unwrap(),
                mode,
            );
            let actual = renderer.render(request, cancel.clone()).await.expect(&name);
            let expected = oracle
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["name"] == name && v["colorMode"] == text)
                .unwrap();
            let expected = BASE64_STANDARD
                .decode(expected["data"].as_str().unwrap())
                .unwrap();
            let actual_pixels = pixels(&actual);
            let expected_pixels = pixels(&expected);
            if actual_pixels != expected_pixels {
                let first = actual_pixels
                    .2
                    .iter()
                    .zip(&expected_pixels.2)
                    .position(|(a, b)| a != b);
                runtime.close().await.unwrap();
                panic!("pixel mismatch {name}/{text}: first differing byte {first:?}");
            }
        }
    }
    runtime.close().await.unwrap();
    std::fs::remove_dir(profile_root).unwrap();
}

#[tokio::test]
#[ignore = "requires pinned Chromium"]
async fn abandoned_requests_release_contexts_and_browser_loss_clears_readiness() {
    let mut runtime = renderer().await;
    let data = IvyJson::Object(vec![]);
    let slow = input(
        &"<script>dashboard.onUpdate(()=>new Promise(()=>{}))</script>".into(),
        &data,
        ColorMode::Color,
    );
    let render = runtime.renderer.clone();
    let request = tokio::spawn(async move { render.render(slow, &CancellationToken::new()).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    request.abort();
    let _ = request.await;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let result = runtime
                .renderer
                .render(
                    input(&"<p>recovered</p>".into(), &data, ColorMode::Color),
                    &CancellationToken::new(),
                )
                .await;
            match result {
                Err(RenderError::Busy) => tokio::time::sleep(Duration::from_millis(20)).await,
                other => {
                    other.expect("abandoned request must release render admission");
                    break;
                }
            }
        }
    })
    .await
    .expect("context cleanup deadline");
    runtime
        .browser
        .kill()
        .await
        .expect("owned browser process")
        .expect("kill test browser");
    tokio::time::timeout(Duration::from_secs(5), async {
        while runtime.renderer.ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("browser loss must clear readiness");
    assert!(matches!(
        runtime
            .renderer
            .render(
                input(&"<p>unavailable</p>".into(), &data, ColorMode::Color),
                &CancellationToken::new(),
            )
            .await,
        Err(RenderError::Unavailable)
    ));
    assert!(runtime.browser.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(&runtime.profile).expect("remove crashed browser profile");
}

#[tokio::test]
#[ignore = "requires pinned Chromium"]
async fn authored_network_attempts_do_not_reach_a_loopback_listener() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    let server = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::Relaxed);
            drop(socket);
        }
    });
    let mut runtime = renderer().await;
    let html = format!(
        r#"<img src="http://{address}/image"><iframe src="http://{address}/frame"></iframe>
      <script>dashboard.onUpdate(async()=>{{
        try{{await fetch('http://{address}/fetch')}}catch{{}}
        try{{new Worker('http://{address}/worker')}}catch{{}}
        try{{window.open('http://{address}/popup')}}catch{{}}
        try{{await new Promise((ok,no)=>{{const s=new WebSocket('ws://{address}/socket');s.onopen=ok;s.onerror=no}})}}catch{{}}
      }});</script>"#
    );
    let result = runtime
        .renderer
        .render(
            input(&html.into(), &IvyJson::Object(vec![]), ColorMode::Color),
            &CancellationToken::new(),
        )
        .await;
    runtime.close().await;
    server.abort();
    result.expect("blocked network attempts must not break rendering");
    assert_eq!(connections.load(Ordering::Relaxed), 0);
}

#[tokio::test]
#[ignore = "requires pinned Chromium"]
async fn cancellation_busy_callback_errors_and_fresh_context_recovery() {
    let mut runtime = renderer().await;
    let renderer = runtime.renderer.clone();
    let data = IvyJson::Object(vec![]);
    let cancel = CancellationToken::new();
    let r = renderer.clone();
    let c = cancel.clone();
    let slow = input(
        &"<script>dashboard.onUpdate(()=>new Promise(()=>{}))</script>".into(),
        &data,
        ColorMode::Color,
    );
    let task = tokio::spawn(async move { r.render(slow, &c).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let request = || input(&"<p>ok</p>".into(), &data, ColorMode::Color);
    assert!(matches!(
        renderer.render(request(), &CancellationToken::new()).await,
        Err(RenderError::Busy)
    ));
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(6), task)
            .await
            .unwrap()
            .unwrap(),
        Err(RenderError::Cancelled)
    ));
    assert!(
        renderer
            .render(request(), &CancellationToken::new())
            .await
            .is_ok()
    );
    let bad = input(
        &"<script>dashboard.onUpdate(()=>{throw ''})</script>".into(),
        &data,
        ColorMode::Color,
    );
    assert!(
        matches!(renderer.render(bad,&CancellationToken::new()).await,Err(RenderError::Callback(message)) if message.to_utf8().unwrap()=="Dashboard rendering failed.")
    );
    let surrogate = input(
        &"<script>dashboard.onUpdate(()=>{throw String.fromCharCode(0xd800)})</script>".into(),
        &data,
        ColorMode::Color,
    );
    assert!(
        matches!(renderer.render(surrogate,&CancellationToken::new()).await,Err(RenderError::Callback(message)) if message.0==[0xd800])
    );
    assert!(
        renderer
            .render(request(), &CancellationToken::new())
            .await
            .is_ok()
    );
    runtime.close().await;
    assert!(!renderer.ready());
}
