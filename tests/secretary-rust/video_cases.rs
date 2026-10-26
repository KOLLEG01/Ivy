#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::{
    IvyResult,
    hash::{digest, hash_json},
    json::canonical,
};
use ivy_secretary::{
    media_input::{Prepared, Source},
    media_video::{VideoCommand, VideoCommands, VideoFuture, VideoPreparer},
    video_schema,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::sync::watch;
fn num(v: &IvyJson, key: &str, default: usize) -> usize {
    v.get(key)
        .and_then(IvyJson::as_f64)
        .map(|n| n as usize)
        .unwrap_or(default)
}
fn yes(v: &IvyJson, key: &str) -> bool {
    v.get(key) == Some(&true.into())
}
fn string(v: &IvyJson, key: &str, default: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str().ok())
        .unwrap_or_else(|| default.into())
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("secretary-video-rust-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
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
                .to_str()
                .unwrap()
                .starts_with("secretary-video-rust-")
        );
        if let Err(error) = std::fs::remove_dir_all(actual) {
            if !std::thread::panicking() {
                panic!("Owned video cleanup: {error}")
            }
        }
    }
}
fn normalize(value: &IvyJson, root: &Path) -> IvyJson {
    match value {
        IvyJson::String(s) => String::from_utf16_lossy(&s.0)
            .replace(&root.to_string_lossy().to_string(), "<root>")
            .into(),
        IvyJson::Array(v) => IvyJson::Array(v.iter().map(|v| normalize(v, root)).collect()),
        IvyJson::Object(v) => IvyJson::Object(
            v.iter()
                .map(|(k, v)| (k.clone(), normalize(v, root)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
#[tokio::test]
async fn original_video_protected_configuration_and_both_executable_identities() {
    let output = reference_data::read(include_bytes!("video-config-cases.json.gz"));
    let rows = parse(std::str::from_utf8(&output).unwrap(), 1024 * 1024).unwrap();
    for row in rows.as_array().unwrap() {
        let root = Directory::new();
        let kind = row.required("kind").unwrap().as_str().unwrap();
        std::fs::write(
            root.0.join("decoder.exe"),
            if kind == "empty" {
                b"".as_slice()
            } else {
                b"decoder".as_slice()
            },
        )
        .unwrap();
        std::fs::write(root.0.join("probe.exe"), b"probe").unwrap();
        if kind == "directory" {
            std::fs::create_dir(root.0.join("directory")).unwrap();
        }
        let configuration = parse(
            &ivy_contracts::json::stringify(row.required("configuration").unwrap(), 1024 * 1024)
                .unwrap()
                .replace("<root>", &root.0.to_string_lossy().replace('\\', "\\\\")),
            1024 * 1024,
        )
        .unwrap();
        let path = root.0.join("config.json");
        if kind != "missing" {
            std::fs::write(
                &path,
                ivy_contracts::json::stringify(&configuration, 1024 * 1024).unwrap(),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        let actual = match ivy_secretary::media_video::load_video_configuration(path).await {
            Ok(value) => IvyJson::object([("value", value.unwrap_or(IvyJson::Null))]),
            Err(error) => IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("outcome", error.outcome.to_json()),
            ]),
        };
        assert_eq!(
            normalize(&actual, &root.0),
            *row.required("expected").unwrap(),
            "configuration {kind}"
        );
    }
    println!(
        "video configuration: 12 frozen-original protected-envelope/executable-identity comparisons"
    );
}
struct Driver {
    options: IvyJson,
    trace: Mutex<Vec<IvyJson>>,
    calls: AtomicUsize,
}
struct Controlled {
    driver: Driver,
    hold: std::sync::atomic::AtomicBool,
    unknown: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: watch::Sender<bool>,
}
impl VideoCommands for Controlled {
    fn run<'a>(
        &'a self,
        command: VideoCommand,
        mut cancel: watch::Receiver<bool>,
    ) -> VideoFuture<'a, Vec<u8>> {
        Box::pin(async move {
            if self.unknown.load(Ordering::SeqCst) {
                return Err(IvyError::new(
                    "outcome_unknown",
                    "Simulated missing Host retirement receipt.",
                )
                .with_outcome(ivy_contracts::Outcome::Unknown));
            }
            if self.hold.load(Ordering::SeqCst) && command.args.iter().any(|a| a == "-show_entries")
            {
                self.entered.notify_one();
                let mut release = self.release.subscribe();
                tokio::select! {_ = async{while !*release.borrow_and_update(){release.changed().await.unwrap();}}=>(),_ = async{while !*cancel.borrow_and_update(){cancel.changed().await.unwrap();}}=>return Err(IvyError::new("media_video_cancelled","The bounded local video decoder did not complete."))}
            }
            self.driver.run(command, cancel).await
        })
    }
}
fn configured(root: &Path, commands: Arc<dyn VideoCommands>) -> VideoPreparer {
    std::fs::write(root.join("decoder.exe"), b"decoder").unwrap();
    std::fs::write(root.join("probe.exe"), b"probe").unwrap();
    let configuration = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        (
            "ffmpeg",
            IvyJson::object([
                (
                    "path",
                    root.join("decoder.exe")
                        .to_string_lossy()
                        .to_string()
                        .into(),
                ),
                ("hash", digest(b"decoder").into()),
            ]),
        ),
        (
            "ffprobe",
            IvyJson::object([
                (
                    "path",
                    root.join("probe.exe").to_string_lossy().to_string().into(),
                ),
                ("hash", digest(b"probe").into()),
            ]),
        ),
    ]);
    VideoPreparer::new(root.join("work"), configuration, commands).unwrap()
}
fn controlled() -> Arc<Controlled> {
    let (release, _) = watch::channel(false);
    Arc::new(Controlled {
        driver: Driver {
            options: parse(r#"{"silent":true}"#, 1024).unwrap(),
            trace: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
        },
        hold: std::sync::atomic::AtomicBool::new(false),
        unknown: std::sync::atomic::AtomicBool::new(false),
        entered: tokio::sync::Notify::new(),
        release,
    })
}
struct NoDocument;
impl ivy_secretary::media_document::DocumentWorker for NoDocument {
    fn extract<'a>(
        &'a self,
        _: ivy_secretary::document::Format,
        _: &'a [u8],
        _: f64,
    ) -> ivy_secretary::media_document::DocumentFuture<'a> {
        Box::pin(async { panic!("Video dispatch must not invoke the document worker") })
    }
}
#[tokio::test]
async fn video_caller_loss_busy_cache_close_and_unknown_retirement_are_owned() {
    let root = Directory::new();
    let driver = controlled();
    let worker = Arc::new(configured(&root.0, driver.clone()));
    let options = IvyJson::object(Vec::<(String, IvyJson)>::new());
    let (source0, bytes0) = source(&options, 0);
    let original = worker
        .prepare(&source0, &bytes0, 5.0 * 1024.0 * 1024.0)
        .await
        .unwrap();
    assert!(matches!(original, Prepared::Video { .. }));
    let acquired =
        ivy_secretary::media_document::AcquiredInput::new(&NoDocument, Some(worker.as_ref()));
    assert_eq!(
        acquired
            .prepare(&source0, &bytes0, 5.0 * 1024.0 * 1024.0)
            .await
            .unwrap(),
        original
    );
    drop(acquired);
    driver.hold.store(true, Ordering::SeqCst);
    let (source1, bytes1) = source(&options, 1);
    let caller_worker = worker.clone();
    let caller_source = source1.clone();
    let caller_bytes = bytes1.clone();
    let caller = tokio::spawn(async move {
        caller_worker
            .prepare(&caller_source, &caller_bytes, 5.0 * 1024.0 * 1024.0)
            .await
    });
    driver.entered.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let (source2, bytes2) = source(&options, 2);
    assert_eq!(
        worker
            .prepare(&source2, &bytes2, 1024.0)
            .await
            .err()
            .unwrap()
            .code,
        "media_video_busy"
    );
    assert_eq!(
        worker
            .prepare(&source0, &bytes0, 5.0 * 1024.0 * 1024.0)
            .await
            .unwrap(),
        original
    );
    driver.release.send_replace(true);
    let completed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match worker
                .prepare(&source1, &bytes1, 5.0 * 1024.0 * 1024.0)
                .await
            {
                Err(e) if e.code == "media_video_busy" => {
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await
                }
                value => break value,
            }
        }
    })
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(completed, Prepared::Video { .. }));
    assert_eq!(driver.driver.calls.load(Ordering::SeqCst), 4);
    driver.release.send_replace(false);
    let next_worker = worker.clone();
    let caller = tokio::spawn(async move { next_worker.prepare(&source2, &bytes2, 1024.0).await });
    driver.entered.notified().await;
    worker.close().await;
    assert_eq!(
        caller.await.unwrap().err().unwrap().code,
        "media_video_cancelled"
    );
    assert_eq!(std::fs::read_dir(root.0.join("work")).unwrap().count(), 0);
    assert_eq!(
        worker
            .prepare(&source0, &bytes0, 1024.0)
            .await
            .err()
            .unwrap()
            .code,
        "internal_error"
    );
    drop(worker);
    let second = Directory::new();
    let driver = controlled();
    driver.unknown.store(true, Ordering::SeqCst);
    let worker = configured(&second.0, driver);
    let error = worker
        .prepare(&source0, &bytes0, 1024.0)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "outcome_unknown");
    assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
    let dirs: Vec<_> = std::fs::read_dir(second.0.join("work"))
        .unwrap()
        .map(|v| v.unwrap().path())
        .collect();
    assert_eq!(dirs.len(), 1);
    assert!(dirs[0].join("source.media").is_file());
    assert_eq!(
        worker
            .prepare(&source1, &bytes1, 1024.0)
            .await
            .err()
            .unwrap()
            .code,
        "media_video_busy"
    );
    worker.close().await;
    println!(
        "video ownership: dropped waiter retains active work/cache; busy admission and cached reads; close drains cancellation; simulated missing retirement keeps scratch and forbids another decoder"
    );
}
impl VideoCommands for Driver {
    fn run<'a>(
        &'a self,
        command: VideoCommand,
        _: watch::Receiver<bool>,
    ) -> VideoFuture<'a, Vec<u8>> {
        Box::pin(async move {
            assert!(
                command.cwd.parent().unwrap().file_name() == Some(std::ffi::OsStr::new("work"))
            );
            let expected: std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString> = [
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
                    .map(|v| (key.into(), v))
            })
            .collect();
            assert_eq!(command.environment, expected);
            let args = command
                .args
                .iter()
                .map(|arg| {
                    arg.replace(&command.cwd.to_string_lossy().to_string(), "<scratch>")
                        .into()
                })
                .collect();
            self.trace.lock().unwrap().push(IvyJson::object([
                (
                    "executable",
                    command.executable.to_string_lossy().to_string().into(),
                ),
                ("args", IvyJson::Array(args)),
                ("cwd", "<scratch>".into()),
                (
                    "maximumBytes",
                    IvyJson::Number(command.maximum_bytes as f64),
                ),
                ("timeoutMs", IvyJson::Number(command.timeout_ms as f64)),
            ]));
            let index = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::task::yield_now().await;
            if num(&self.options, "failAt", 0) == index {
                return Err(IvyError::new(
                    string(&self.options, "failure", "media_video_timeout"),
                    "The bounded local video decoder did not complete.",
                ));
            }
            if command.args.iter().any(|a| a == "-show_entries") {
                if yes(&self.options, "invalidJson") {
                    return Ok(b"{broken".to_vec());
                }
                if yes(&self.options, "bomJson") {
                    return Ok("\u{feff}{}".as_bytes().to_vec());
                }
                let fallback = parse(
                    if yes(&self.options, "silent") {
                        r#"{"format":{"duration":2,"start_time":"0"},"streams":[{"index":0,"codec_type":"video","width":320,"height":240}]}"#
                    } else {
                        r#"{"format":{"duration":2,"start_time":"0"},"streams":[{"index":0,"codec_type":"video","width":320,"height":240},{"index":1,"codec_type":"audio","duration":0.02}]}"#
                    },
                    32768,
                )?;
                return Ok(ivy_contracts::json::stringify(
                    self.options.get("probe").unwrap_or(&fallback),
                    usize::MAX,
                )?
                .into_bytes());
            }
            if command.args.iter().any(|a| a == "image2") {
                for n in 0..num(&self.options, "frames", 8) {
                    let mut frame = vec![0; num(&self.options, "frameSize", 4)];
                    let len = frame.len();
                    frame[0] = 255;
                    frame[1] = 216;
                    frame[len - 2] = 255;
                    frame[len - 1] = if yes(&self.options, "badFrame") {
                        0
                    } else {
                        217
                    };
                    let index = n + 1 + usize::from(yes(&self.options, "frameGap") && n > 0);
                    tokio::fs::write(command.cwd.join(format!("frame-{index:02}.jpg")), frame)
                        .await
                        .map_err(|_| IvyError::internal())?;
                }
                return Ok(vec![]);
            }
            assert!(command.args.iter().any(|a| a == "s16le"));
            Ok(vec![
                num(&self.options, "pcmByte", 255) as u8;
                num(&self.options, "pcmSize", 640)
            ])
        })
    }
}
fn source(options: &IvyJson, id: usize) -> (Source, Vec<u8>) {
    let mut bytes = vec![0; num(options, "length", 16)];
    let media_type = string(options, "type", "video/mp4");
    if bytes.len() >= 16 {
        match media_type.as_str() {
            "video/x-msvideo" => {
                bytes[..4].copy_from_slice(b"RIFF");
                bytes[8..12].copy_from_slice(b"AVI ")
            }
            "video/webm" | "video/x-matroska" => {
                bytes[..4].copy_from_slice(&[0x1a, 0x45, 0xdf, 0xa3])
            }
            _ => bytes[4..8].copy_from_slice(b"ftyp"),
        };
        bytes[15] = id as u8;
    }
    if yes(options, "badSignature") {
        bytes[4] = 0;
    }
    if yes(options, "highAscii") {
        let indices: &[usize] = if media_type == "video/x-msvideo" {
            &[0, 1, 2, 3, 8, 9, 10, 11]
        } else {
            &[4, 5, 6, 7]
        };
        for index in indices {
            bytes[*index] |= 128;
        }
    }
    let source = Source {
        name: string(options, "name", "../../outside; $(inert).mp4").into(),
        media_type: Some(media_type.into()),
        byte_length: bytes.len() + usize::from(yes(options, "wrongLength")),
        content_hash: if yes(options, "wrongHash") {
            digest(b"other")
        } else {
            digest(&bytes)
        },
    };
    (source, bytes)
}
fn observed(value: IvyResult<Prepared>, compact: bool) -> IvyJson {
    match value {
        Ok(Prepared::Video { source, inputs }) if compact => IvyJson::object([(
            "value",
            IvyJson::object([
                ("kind", "video".into()),
                ("source", source.to_json()),
                (
                    "inputsHash",
                    hash_json(&IvyJson::Array(inputs.clone())).unwrap().into(),
                ),
                (
                    "inputBytes",
                    IvyJson::Number(
                        canonical(&IvyJson::Array(inputs.clone()), usize::MAX)
                            .unwrap()
                            .len() as f64,
                    ),
                ),
                (
                    "projection",
                    parse(
                        &inputs[0].required("text").unwrap().as_str().unwrap(),
                        1024 * 1024,
                    )
                    .unwrap(),
                ),
            ]),
        )]),
        Ok(value) => IvyJson::object([("value", value.to_json())]),
        Err(e) => IvyJson::object([
            ("code", IvyJson::String(e.code_js())),
            ("outcome", e.outcome.to_json()),
        ]),
    }
}
fn corpus() -> IvyJson {
    let out = reference_data::read(include_bytes!("video-cases.json.gz"));
    parse(std::str::from_utf8(&out).unwrap(), 32 * 1024 * 1024).unwrap()
}
#[tokio::test]
async fn original_video_policy_projection_argv_scratch_and_cache() {
    let corpus = corpus();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    let mut actions = 0;
    for row in rows {
        let input = row.required("input").unwrap();
        let label = input.required("label").unwrap().as_str().unwrap();
        let options = input.required("options").unwrap();
        let directory = Directory::new();
        let root = &directory.0;
        let work = root.join("work");
        std::fs::write(root.join("decoder.exe"), b"decoder").unwrap();
        std::fs::write(root.join("probe.exe"), b"probe").unwrap();
        let configuration = parse(
            &canonical(input.required("configuration").unwrap(), usize::MAX)
                .unwrap()
                .replace("<root>", &root.to_string_lossy().replace('\\', "\\\\")),
            1024 * 1024,
        )
        .unwrap();
        if let Some(scratch) = options.get("scratch") {
            std::fs::create_dir(&work).unwrap();
            for n in 0..num(scratch, "entries", 1) {
                let path = work.join(format!(
                    "{}-{n}",
                    if yes(scratch, "badName") {
                        "foreign"
                    } else {
                        "video-retained"
                    }
                ));
                if yes(scratch, "entryFile") {
                    std::fs::write(path, b"x").unwrap();
                    continue;
                }
                std::fs::create_dir(&path).unwrap();
                for k in 0..num(scratch, "files", 0) {
                    let file = path.join(format!("file-{k}"));
                    std::fs::write(&file, b"x").unwrap();
                    if let Some(size) = scratch.get("large").and_then(IvyJson::as_f64) {
                        std::fs::OpenOptions::new()
                            .write(true)
                            .open(file)
                            .unwrap()
                            .set_len(size as u64)
                            .unwrap();
                    }
                }
            }
        }
        let driver = Arc::new(Driver {
            options: options.clone(),
            trace: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
        });
        let worker = VideoPreparer::new(work.clone(), configuration, driver.clone()).unwrap();
        let mut results = vec![];
        for action in input.required("actions").unwrap().as_array().unwrap() {
            actions += 1;
            let (mut source, bytes) = source(options, num(action, "id", 0));
            if let Some(name) = action.get("name").and_then(IvyJson::as_js_string) {
                source.name = name.clone();
            }
            let value = worker
                .prepare(
                    &source,
                    &bytes,
                    action.required("budget").unwrap().as_f64().unwrap(),
                )
                .await;
            results.push(observed(value, yes(options, "compact")));
        }
        worker.close().await;
        let entries = |path: &Path| {
            let mut names: Vec<_> = std::fs::read_dir(path)
                .unwrap()
                .map(|v| v.unwrap().file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            IvyJson::Array(names.into_iter().map(Into::into).collect())
        };
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            (
                "trace",
                IvyJson::Array(driver.trace.lock().unwrap().clone()),
            ),
            ("retained", entries(root)),
            (
                "scratch",
                if work.exists() {
                    entries(&work)
                } else {
                    IvyJson::Array(vec![])
                },
            ),
        ]);
        let actual = normalize(&actual, root);
        let expected = row.required("expected").unwrap();
        for field in ["results", "trace", "retained", "scratch"] {
            assert_eq!(
                canonical(actual.required(field).unwrap(), 32 * 1024 * 1024).unwrap(),
                canonical(expected.required(field).unwrap(), 32 * 1024 * 1024).unwrap(),
                "{label}/{field}"
            );
        }
    }
    for row in corpus.required("schema").unwrap().as_array().unwrap() {
        let actual = video_schema::validate("VideoProjection", row.required("value").unwrap())
            .err()
            .map(|e| {
                IvyJson::object([
                    ("code", IvyJson::String(e.code_js())),
                    ("outcome", e.outcome.to_json()),
                ])
            })
            .unwrap_or(IvyJson::Null);
        assert_eq!(actual, *row.required("result").unwrap());
    }
    println!(
        "video policy original comparisons: {} scenarios / {actions} attempts / {} schema cases; exact argv, JPEG/WAV bytes, frame/audio limits, scratch and FIFO/byte cache",
        rows.len(),
        corpus.required("schema").unwrap().as_array().unwrap().len()
    );
}
