use ivy_contracts::{IvyJson, Outcome, hash::digest};
use ivy_secretary::{
    media_input::{Prepared, Source},
    media_video::{VideoCommand, VideoCommands, VideoPreparer},
    video_commands::HostVideoCommands,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("secretary-video-command-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn binary(&self) -> PathBuf {
        let path = self.0.join(if cfg!(windows) {
            "decoder.exe"
        } else {
            "decoder"
        });
        std::fs::copy(
            std::env::var_os("IVY_SECRETARY_VIDEO_FIXTURE")
                .expect("Selected native fixture is required"),
            &path,
        )
        .unwrap();
        path
    }
    fn driver(&self) -> HostVideoCommands {
        HostVideoCommands::new(
            self.0.clone(),
            std::env::var_os("IVY_SECRETARY_JOB_HELPER")
                .expect("Qualified shared Job helper is required")
                .into(),
        )
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let actual = std::fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(
                std::fs::canonicalize(std::env::temp_dir())
                    .unwrap()
                    .as_path()
            )
        );
        assert!(
            actual
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("secretary-video-command-")
        );
        if let Err(error) = std::fs::remove_dir_all(actual) {
            if !std::thread::panicking() {
                panic!("Owned command cleanup: {error}");
            }
        }
    }
}
fn environment() -> BTreeMap<OsString, OsString> {
    [
        "SystemRoot",
        "WINDIR",
        "PATH",
        "TEMP",
        "TMP",
        "TMPDIR",
        "LANG",
    ]
    .into_iter()
    .filter_map(|key| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(|value| (key.into(), value))
    })
    .collect()
}
fn command(
    root: &Root,
    binary: &Path,
    args: Vec<String>,
    maximum: usize,
    timeout: u32,
) -> VideoCommand {
    VideoCommand {
        executable: binary.into(),
        args,
        cwd: root.0.clone(),
        maximum_bytes: maximum,
        timeout_ms: timeout,
        environment: environment(),
    }
}
async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn actual_video_command_binary_caps_environment_eof_exit_timeout_and_cancel() {
    let root = Root::new();
    let binary = root.binary();
    let driver = root.driver();
    let (_, cancel) = watch::channel(false);
    for maximum in [1024, 32768, ivy_secretary::media_video::MAXIMUM_AUDIO] {
        for extra in [0, 1] {
            let report = root.0.join(format!("report-{maximum}-{extra}"));
            let result = driver
                .run(
                    command(
                        &root,
                        &binary,
                        vec![
                            "bytes".into(),
                            (maximum + extra).to_string(),
                            report.to_string_lossy().into(),
                        ],
                        maximum,
                        15000,
                    ),
                    cancel.clone(),
                )
                .await;
            if extra == 0 {
                let value = result.unwrap();
                assert_eq!(value.len(), maximum);
                assert!(value.iter().enumerate().all(|(i, b)| *b == (i % 256) as u8));
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.code, "media_native_input_too_large");
                assert_ne!(error.outcome, Outcome::Unknown);
            }
            assert_eq!(
                std::fs::read_to_string(report).unwrap(),
                format!("{:?}\nstdin=0\ncwd={}\n", environment(), root.0.display())
            );
        }
    }
    let failed = driver
        .run(
            command(&root, &binary, vec!["fail".into()], 1024, 15000),
            cancel.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(failed.code, "media_video_decode_failed");
    assert_ne!(failed.outcome, Outcome::Unknown);
    let missing = driver
        .run(
            command(&root, &root.0.join("missing.exe"), vec![], 1024, 15000),
            cancel.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(missing.code, "media_video_decoder_failed");
    // Preserve the Host's conservative admission I/O receipt. A missing path
    // must not be used to infer nonexecution from an unavailable owner proof.
    assert_eq!(missing.outcome, Outcome::Unknown);
    let report = root.0.join("timeout.ready");
    let timeout = driver
        .run(
            command(
                &root,
                &binary,
                vec!["hold".into(), report.to_string_lossy().into()],
                1024,
                250,
            ),
            cancel,
        )
        .await
        .unwrap_err();
    assert_eq!(timeout.code, "media_video_timeout");
    assert_ne!(timeout.outcome, Outcome::Unknown);
    assert!(report.is_file());
    let (stop, cancel) = watch::channel(false);
    let report = root.0.join("cancel.ready");
    let running_driver = driver.clone();
    let selected = command(
        &root,
        &binary,
        vec!["hold".into(), report.to_string_lossy().into()],
        1024,
        15000,
    );
    let running = tokio::spawn(async move { running_driver.run(selected, cancel).await });
    ready(&report).await;
    stop.send_replace(true);
    let error = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "media_video_cancelled");
    assert_ne!(error.outcome, Outcome::Unknown);
    println!(
        "actual video Host binding: 3 exact caps + 3 overflow retirements; exact environment/cwd/stdin EOF; discarded 256KiB private stderr; binary PCM; exit7/timeout/cancel distinguished with retirement confirmed; missing-path Host Unknown preserved"
    );
}
#[tokio::test]
async fn actual_video_preparer_composes_host_commands_and_retains_abandoned_waiter_until_close() {
    let root = Root::new();
    let binary = root.binary();
    let hash = digest(&std::fs::read(&binary).unwrap());
    let configuration = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        (
            "ffmpeg",
            IvyJson::object([
                ("path", binary.to_string_lossy().to_string().into()),
                ("hash", hash.clone().into()),
            ]),
        ),
        (
            "ffprobe",
            IvyJson::object([
                ("path", binary.to_string_lossy().to_string().into()),
                ("hash", hash.into()),
            ]),
        ),
    ]);
    let worker = Arc::new(
        VideoPreparer::new(root.0.join("work"), configuration, Arc::new(root.driver())).unwrap(),
    );
    let bytes = b"0000ftyp00000000";
    let source = Source {
        name: "private.mov".into(),
        media_type: Some("video/quicktime".into()),
        byte_length: bytes.len(),
        content_hash: digest(bytes),
    };
    let result = worker
        .prepare(&source, bytes, 6.0 * 1024.0 * 1024.0)
        .await
        .unwrap();
    let Prepared::Video { inputs, .. } = &result else {
        panic!("Expected video, got {result:?}")
    };
    assert_eq!(inputs.len(), 3);
    assert!(
        std::fs::read_to_string(root.0.join("probe.ready"))
            .unwrap()
            .starts_with(&format!("{:?}\nstdin=0\n", environment()))
    );
    assert_eq!(std::fs::read_dir(root.0.join("work")).unwrap().count(), 0);
    std::fs::write(root.0.join("mode"), "hold-probe").unwrap();
    std::fs::remove_file(root.0.join("probe.ready")).unwrap();
    let other = b"0000ftyp00000001";
    let next = Source {
        content_hash: digest(other),
        ..source.clone()
    };
    let caller_worker = worker.clone();
    let caller = tokio::spawn(async move { caller_worker.prepare(&next, other, 1024.0).await });
    ready(&root.0.join("probe.ready")).await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert_eq!(
        worker
            .prepare(&source, bytes, 6.0 * 1024.0 * 1024.0)
            .await
            .unwrap(),
        result
    );
    tokio::time::timeout(Duration::from_secs(5), worker.close())
        .await
        .unwrap();
    assert_eq!(std::fs::read_dir(root.0.join("work")).unwrap().count(), 0);
    assert_eq!(
        worker
            .prepare(&source, bytes, 1024.0)
            .await
            .unwrap_err()
            .code,
        "internal_error"
    );
    println!(
        "actual video composition: hash-pinned inert probe/frames/PCM processes, owned scratch retirement, cached bytes during abandoned active waiter, close cancels and drains real Host owner before cleanup"
    );
}
