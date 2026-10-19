use ivy_agent_manager::process_evidence::{OwnedProcessEvidence, StopClock};
use ivy_contracts::{
    IvyJson, JsString,
    hash::digest,
    json::{canonical, parse, stringify},
};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-agent-evidence-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Clock {
    text: JsString,
    now: Vec<i64>,
    position: Mutex<usize>,
    trace: Arc<Mutex<IvyJson>>,
}
fn append(trace: &Mutex<IvyJson>, key: &str, value: IvyJson) {
    let mut trace = trace.lock().unwrap();
    let mut values = trace.required(key).unwrap().as_array().unwrap().to_vec();
    values.push(value);
    trace.insert(key, IvyJson::Array(values));
}
impl StopClock for Clock {
    fn observed_at(&self) -> JsString {
        append(
            &self.trace,
            "observedAt",
            IvyJson::String(self.text.clone()),
        );
        self.text.clone()
    }
    fn millis(&self) -> i64 {
        let mut position = self.position.lock().unwrap();
        let value = self.now[(*position).min(self.now.len() - 1)];
        *position += 1;
        append(&self.trace, "now", IvyJson::Number(value as f64));
        value
    }
}
#[tokio::test]
async fn owned_stop_file_validation_retention_and_clock_order_match_actual_original() {
    let root = Root::new();
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/agent-process-evidence-oracle.mjs",
        ])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = IvyJson::object([(
        "root",
        IvyJson::String(ivy_host_runtime::node_path::from_native(
            &root.0.join("original"),
        )),
    )]);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for row in oracle.required("parsed").unwrap().as_array().unwrap() {
        let date = row.required("date").unwrap().as_js_string().unwrap();
        let actual = ivy_sdk::clock::parse_date(date);
        assert_eq!(
            actual.is_finite().then_some(actual as i64),
            row.required("millis").unwrap().as_f64().map(|n| n as i64),
            "{date:?}"
        );
    }
    let cases = oracle.required("cases").unwrap().as_array().unwrap();
    assert!(cases.len() > 35);
    for case in cases {
        let name = case.required("name").unwrap().as_str().unwrap();
        let directory = root.0.join("rust").join(&name);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("owned-process-stops.json");
        let hex = case.required("bytes").unwrap().as_str().unwrap();
        let bytes: Vec<u8> = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
            .collect();
        match case.required("kind").unwrap().as_str().unwrap().as_str() {
            "missing" => {}
            "directory" => fs::create_dir(&path).unwrap(),
            "hard-link" => {
                let original = directory.join("original");
                fs::write(&original, &bytes).unwrap();
                fs::hard_link(original, &path).unwrap();
            }
            _ => fs::write(&path, &bytes).unwrap(),
        }
        let trace = Arc::new(Mutex::new(IvyJson::object([
            ("observedAt", IvyJson::Array(Vec::new())),
            ("now", IvyJson::Array(Vec::new())),
        ])));
        let clock = Arc::new(Clock {
            text: oracle
                .required("clock")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            now: case
                .required("now")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap() as i64)
                .collect(),
            position: Mutex::new(0),
            trace: trace.clone(),
        });
        let evidence = OwnedProcessEvidence::with_clock(directory, clock);
        let result = if case.required("operation").unwrap() == &IvyJson::from("read") {
            evidence.stops().await.map(IvyJson::Array)
        } else {
            evidence
                .retain(
                    case.required("epoch")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .clone(),
                )
                .await
                .map(|()| IvyJson::Null)
        };
        let reply = match result {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
            Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
        };
        let file_hash = fs::read(path)
            .ok()
            .map(|bytes| IvyJson::from(digest(bytes)))
            .unwrap_or(IvyJson::Null);
        let actual = IvyJson::object([
            ("reply", reply),
            ("fileHash", file_hash),
            ("trace", trace.lock().unwrap().clone()),
        ]);
        assert_eq!(
            canonical(&actual, usize::MAX).unwrap(),
            canonical(case.required("expected").unwrap(), usize::MAX).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn evidence_child_fixture() {}

#[cfg(windows)]
#[tokio::test]
async fn production_stop_callback_does_not_promote_without_job_and_persists_unknown_fences() {
    use ivy_agent_manager::native_handshake::NativeMode;
    use ivy_host_runtime::{process, process_fence::process_stop_fence};
    use std::{collections::BTreeMap, time::Duration};
    let root = Root::new();
    let child = process::spawn(
        &process::Command {
            executable: "fixture".into(),
            args: vec![
                "--exact".into(),
                "process_evidence_cases::evidence_child_fixture".into(),
            ],
            cwd: None,
            timeout_ms: 5000,
        },
        &root.0,
        &BTreeMap::from([("fixture".into(), std::env::current_exe().unwrap())]),
        process::LaunchContext {
            without_job: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(6), child.completion.wait())
        .await
        .unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.error_code, None);
    assert!(!process::owned_process_tree_stopped(&child).await.unwrap());
    let trace = Arc::new(Mutex::new(IvyJson::object([
        ("observedAt", IvyJson::Array(Vec::new())),
        ("now", IvyJson::Array(Vec::new())),
    ])));
    let clock = Arc::new(Clock {
        text: "2026-10-19T12:34:56.789Z".into(),
        now: vec![0],
        position: Mutex::new(0),
        trace: trace.clone(),
    });
    let callback = OwnedProcessEvidence::with_clock(root.0.clone(), clock).callback();
    // Poisoned retained evidence would fail if a proxy or unproved launch tried
    // to read it. Successful completion still supplies no positive Job proof.
    fs::create_dir(root.0.join("owned-process-stops.json")).unwrap();
    for mode in [
        NativeMode::OwnedStdio,
        NativeMode::ExternalProxy,
        NativeMode::ClaudeAdapter,
    ] {
        callback("unproved-epoch".into(), &child, mode, result.clone())
            .await
            .unwrap();
    }
    assert!(process_stop_fence(root.0.clone()).await.unwrap().is_none());
    assert!(
        trace
            .lock()
            .unwrap()
            .required("observedAt")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    // The injected result exercises the durable policy only. This is not an
    // assertion that the actual completed fixture had an uncertain outcome.
    let mut unknown = (*result).clone();
    unknown.error_code = Some("outcome_unknown".into());
    callback(
        "first-epoch".into(),
        &child,
        NativeMode::ExternalProxy,
        Arc::new(unknown.clone()),
    )
    .await
    .unwrap();
    let first = process_stop_fence(root.0.clone()).await.unwrap().unwrap();
    assert_eq!(first.get("ownerId"), Some(&"first-epoch".into()));
    assert_eq!(
        first.get("processId"),
        Some(&IvyJson::Number(child.pid as f64))
    );
    callback(
        "second-epoch".into(),
        &child,
        NativeMode::OwnedStdio,
        Arc::new(unknown),
    )
    .await
    .unwrap();
    assert_eq!(
        process_stop_fence(root.0.clone()).await.unwrap(),
        Some(first)
    );
}
