use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyJson,
    hash::{digest, hash_json},
    json::{canonical, parse},
};
use ivy_secretary::{
    media_input::Source,
    media_video::{VideoCommand, VideoCommands, VideoFuture, VideoPreparer},
    video_commands::HostVideoCommands,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

struct Directory(PathBuf);
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
                .to_string_lossy()
                .starts_with("secretary-real-codec-")
        );
        std::fs::remove_dir_all(actual).unwrap();
    }
}
struct Commands {
    host: HostVideoCommands,
    trace: Mutex<Vec<IvyJson>>,
}
impl VideoCommands for Commands {
    fn run<'a>(
        &'a self,
        command: VideoCommand,
        cancel: watch::Receiver<bool>,
    ) -> VideoFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let cwd = command.cwd.to_string_lossy().to_string();
            let mut entry = IvyJson::object([
                (
                    "executable",
                    command.executable.to_string_lossy().to_string().into(),
                ),
                (
                    "args",
                    IvyJson::Array(
                        command
                            .args
                            .iter()
                            .map(|arg| arg.replace(&cwd, "<scratch>").into())
                            .collect(),
                    ),
                ),
                ("cwd", "<scratch>".into()),
                (
                    "maximumBytes",
                    IvyJson::Number(command.maximum_bytes as f64),
                ),
                ("timeoutMs", IvyJson::Number(command.timeout_ms as f64)),
            ]);
            let result = self.host.run(command, cancel).await;
            let outcome = match &result {
                Ok(bytes) => IvyJson::object([
                    ("bytes", IvyJson::Number(bytes.len() as f64)),
                    ("hash", digest(bytes).into()),
                ]),
                Err(error) => IvyJson::object([
                    ("code", error.code.clone().into()),
                    ("outcome", error.outcome.to_json()),
                ]),
            };
            entry.insert("result", outcome);
            self.trace.lock().unwrap().push(entry);
            result
        })
    }
}
fn scratch(root: &Path) -> IvyJson {
    if !root.exists() {
        return IvyJson::Null;
    }
    let mut names = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    names.sort();
    IvyJson::Array(names.into_iter().map(Into::into).collect())
}

#[tokio::test]
#[ignore = "Requires the pinned real-codec original corpus and qualified Host Job helper."]
async fn windows_real_codecs_match_original_fixture_cases() {
    let path = std::env::var_os("IVY_SECRETARY_CODEC_CORPUS")
        .expect("Actual original codec corpus required");
    let corpus = parse(&std::fs::read_to_string(path).unwrap(), 64 * 1024 * 1024).unwrap();
    let directory = Directory(
        std::env::temp_dir().join(format!("secretary-real-codec-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&directory.0).unwrap();
    let helper = PathBuf::from(
        std::env::var_os("IVY_SECRETARY_JOB_HELPER").expect("Qualified Host helper required"),
    );
    assert_eq!(
        digest(&std::fs::read(&helper).unwrap()),
        "sha256:c380c679cdbe22e883fa1450a9fba2c6c91d5344c756d62af3bc4e3e3df84b27"
    );
    let mut receipts = vec![];
    let mut commands = 0;
    let instances = corpus.required("instances").unwrap().as_array().unwrap();
    for (index, instance) in instances.iter().enumerate() {
        let parent = directory.0.join(index.to_string());
        std::fs::create_dir(&parent).unwrap();
        let root = parent.join(
            instance
                .required("directory")
                .unwrap()
                .as_str()
                .unwrap()
                .as_str(),
        );
        let driver = Arc::new(Commands {
            host: HostVideoCommands::new(parent, helper.clone()),
            trace: Mutex::new(vec![]),
        });
        let worker = VideoPreparer::new(
            root.clone(),
            instance.required("configuration").unwrap().clone(),
            driver.clone(),
        )
        .unwrap();
        for (number, action) in instance
            .required("actions")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let selected = action.required("source").unwrap();
            let source = Source {
                name: selected
                    .required("name")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone(),
                media_type: Some(
                    selected
                        .required("mediaType")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .clone(),
                ),
                byte_length: selected.required("byteLength").unwrap().as_f64().unwrap() as usize,
                content_hash: selected
                    .required("contentHash")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string(),
            };
            let bytes = STANDARD
                .decode(
                    action
                        .required("base64")
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .as_bytes(),
                )
                .unwrap();
            assert_eq!(digest(&bytes), source.content_hash);
            assert_eq!(bytes.len(), source.byte_length);
            let start = driver.trace.lock().unwrap().len();
            let result = match worker
                .prepare(
                    &source,
                    &bytes,
                    action.required("budget").unwrap().as_f64().unwrap(),
                )
                .await
            {
                Ok(value) => IvyJson::object([("value", value.to_json())]),
                Err(error) => IvyJson::object([
                    ("code", error.code.into()),
                    ("outcome", error.outcome.to_json()),
                ]),
            };
            let trace = driver.trace.lock().unwrap()[start..].to_vec();
            commands += trace.len();
            let actual = IvyJson::object([
                ("result", result),
                ("trace", IvyJson::Array(trace)),
                ("scratch", scratch(&root)),
            ]);
            let expected = IvyJson::object([
                ("result", action.required("expected").unwrap().clone()),
                ("trace", action.required("trace").unwrap().clone()),
                ("scratch", action.required("scratch").unwrap().clone()),
            ]);
            let same = canonical(&actual, 16 * 1024 * 1024).unwrap()
                == canonical(&expected, 16 * 1024 * 1024).unwrap();
            receipts.push(IvyJson::object([
                ("instance", IvyJson::Number(index as f64)),
                ("action", IvyJson::Number(number as f64)),
                ("sourceHash", source.content_hash.into()),
                ("actualHash", hash_json(&actual).unwrap().into()),
                ("expectedHash", hash_json(&expected).unwrap().into()),
                ("equal", same.into()),
            ]));
            if let Some(path) = std::env::var_os("IVY_SECRETARY_CODEC_ACTUAL") {
                std::fs::write(
                    path,
                    canonical(
                        &IvyJson::object([
                            ("receipts", IvyJson::Array(receipts.clone())),
                            ("latestActual", actual),
                            ("latestExpected", expected),
                        ]),
                        32 * 1024 * 1024,
                    )
                    .unwrap(),
                )
                .unwrap();
            }
            assert!(
                same,
                "Real codec instance {index} action {number}: compare retained latestActual/latestExpected"
            );
        }
        worker.close().await;
        if root.exists() {
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        }
    }
    assert!(commands > 10);
    assert!(receipts.len() > 10);
    println!(
        "actual Windows FFmpeg/FFprobe: {} instances,{} exact original/native actions,{} real owned commands; full JPEG/PCM input bytes and ordering,cache,budget,identity,duration,dimensions,corruption and scratch agree",
        instances.len(),
        receipts.len(),
        commands
    );
}
