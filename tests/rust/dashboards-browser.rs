use ivy_contracts::IvyJson;
use ivy_dashboards::{
    browser::{BrowserOptions, BrowserOwner},
    cdp::RenderInput,
    raster::ColorMode,
};
use std::{path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

fn input(script: &str) -> RenderInput {
    RenderInput::new(
        &script.into(),
        &IvyJson::object([] as [(&str, IvyJson); 0]),
        100,
        100,
        ColorMode::Color,
    )
    .unwrap()
}
fn fixture() -> (BrowserOwner, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("ivy-browser-owner-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let owner = BrowserOwner::new(BrowserOptions {
        executable: std::env::var_os("DASHBOARDS_TEST_BROWSER")
            .expect("explicit pinned browser")
            .into(),
        artifact_root: root.clone(),
        temporary_root: root.clone(),
        job_launcher: std::env::var_os("DASHBOARDS_TEST_JOB").map(Into::into),
    });
    (owner, root)
}

#[tokio::test]
async fn failed_launch_removes_only_its_profile_and_remains_unready() {
    let root =
        std::env::temp_dir().join(format!("ivy-browser-launch-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let owner = BrowserOwner::new(BrowserOptions {
        executable: root.join("missing-browser"),
        artifact_root: root.clone(),
        temporary_root: root.clone(),
        job_launcher: None,
    });
    for _ in 0..2 {
        let error = owner
            .render(input("<p>unavailable</p>"), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code, "dependency_unavailable");
        assert!(!owner.ready());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
    owner.close().await.unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[tokio::test]
#[ignore = "requires pinned isolated Chromium and the built Windows job launcher"]
async fn owned_browser_cancellation_busy_shutdown_and_restart() {
    let (owner, root) = fixture();
    assert!(!owner.ready());
    let clone = owner.clone();
    let request = tokio::spawn(async move {
        clone
            .render(
                input("<script>dashboard.onUpdate(()=>new Promise(()=>{}))</script>"),
                CancellationToken::new(),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        owner
            .render(input("<p>x</p>"), CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "resource_busy"
    );
    request.abort();
    let _ = request.await;
    let success = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match owner
                .render(
                    input("<p>after cancelled request</p>"),
                    CancellationToken::new(),
                )
                .await
            {
                Ok(bytes) => break bytes,
                Err(error) if error.code == "resource_busy" => {
                    tokio::time::sleep(Duration::from_millis(25)).await
                }
                Err(error) => panic!("{error:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(&success[..8], b"\x89PNG\r\n\x1a\n");

    assert!(!owner.ready());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    // Every following export starts and retires a fresh owned browser tree.
    let bytes = owner
        .render(input("<p>second export</p>"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert!(!owner.ready());
    owner.close().await.unwrap();
    owner.close().await.unwrap();
    assert!(!owner.ready());
    assert!(
        owner
            .render(input("<p>closed</p>"), CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir(root).unwrap();
}

#[tokio::test]
#[ignore = "requires pinned isolated Chromium and the built Windows job launcher"]
async fn shutdown_owns_an_abandoned_launch_and_active_render() {
    let (owner, root) = fixture();
    let clone = owner.clone();
    let start = tokio::spawn(async move {
        clone
            .render(input("<p>abandoned launch</p>"), CancellationToken::new())
            .await
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    start.abort();
    owner.close().await.unwrap();
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir(root).unwrap();

    let (owner, root) = fixture();
    let clone = owner.clone();
    let render = tokio::spawn(async move {
        clone
            .render(
                input("<script>dashboard.onUpdate(()=>new Promise(()=>{}))</script>"),
                CancellationToken::new(),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    tokio::time::timeout(Duration::from_secs(12), owner.close())
        .await
        .unwrap()
        .unwrap();
    assert!(render.await.unwrap().is_err());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir(root).unwrap();
}
