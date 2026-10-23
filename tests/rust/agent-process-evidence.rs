use ivy_agent_manager::process_evidence::{OwnedProcessEvidence, StopClock};
use ivy_contracts::{IvyJson, JsString, json::stringify};
use std::{
    fs,
    path::PathBuf,
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
async fn native_stop_records_validate_storage_and_retain_recent_observations() {
    let root = Root::new();
    let path = root.0.join("owned-process-stops.json");
    let now = 1_792_413_296_789i64;
    let iso = |ms| {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    let row = |epoch: &str, at: &str| {
        IvyJson::object([("epoch", epoch.into()), ("observedAt", at.into())])
    };
    let record = row("prior", &iso(now));
    let clock = Arc::new(Clock {
        text: iso(now).into(),
        now: vec![now],
        position: Mutex::new(0),
        trace: Arc::new(Mutex::new(IvyJson::object([
            ("observedAt", IvyJson::Array(vec![])),
            ("now", IvyJson::Array(vec![])),
        ]))),
    });
    let evidence = OwnedProcessEvidence::with_clock(root.0.clone(), clock);
    assert!(evidence.stops().await.unwrap().is_empty());
    for records in [
        IvyJson::Array(vec![]),
        IvyJson::Array(vec![record.clone()]),
        IvyJson::Array(vec![row("offset", "2026-10-19T14:34:56.789+02:00")]),
        IvyJson::Array(vec![row("date-only", "2026-10-19")]),
        IvyJson::Array(vec![row("rfc", "Mon, 19 Oct 2026 12:34:56 GMT")]),
        IvyJson::Array(vec![record.clone(); 256]),
    ] {
        fs::write(&path, stringify(&records, 65536).unwrap()).unwrap();
        assert_eq!(IvyJson::Array(evidence.stops().await.unwrap()), records);
    }
    for invalid in [
        IvyJson::Null,
        IvyJson::object([] as [(&str, IvyJson); 0]),
        "text".into(),
        IvyJson::Number(1.0),
        IvyJson::Array(vec![IvyJson::Null]),
        IvyJson::Array(vec![record.clone(); 257]),
        IvyJson::Array(vec![row("", &iso(now))]),
        IvyJson::Array(vec![row(&"x".repeat(257), &iso(now))]),
        IvyJson::Array(vec![row("bad", "invalid")]),
        IvyJson::Array(vec![IvyJson::object([
            ("epoch", IvyJson::Number(1.0)),
            ("observedAt", iso(now).into()),
        ])]),
        IvyJson::Array(vec![IvyJson::object([
            ("epoch", "prior".into()),
            ("observedAt", iso(now).into()),
            ("extra", true.into()),
        ])]),
    ] {
        let bytes = stringify(&invalid, 65536).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            evidence.stops().await.unwrap_err().code,
            "native_stop_evidence_invalid"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
    }
    for (bytes, valid) in [
        (format!("[]{}", " ".repeat(65536 - 2)), true),
        (format!("[]{}", " ".repeat(65536 - 1)), false),
        ("{".into(), false),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert_eq!(evidence.stops().await.is_ok(), valid);
        assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
    }
    let cutoff = now - 14 * 24 * 60 * 60 * 1000;
    let records = IvyJson::Array(vec![
        row("before", &iso(cutoff - 1)),
        row("equal", &iso(cutoff)),
        row("after", &iso(cutoff + 1)),
        row("future", &iso(now + 1)),
        row("new-epoch", &iso(now - 1)),
    ]);
    fs::write(&path, stringify(&records, 65536).unwrap()).unwrap();
    evidence.retain("new-epoch".into()).await.unwrap();
    assert_eq!(
        evidence.stops().await.unwrap(),
        vec![
            row("after", &iso(cutoff + 1)),
            row("future", &iso(now + 1)),
            row("new-epoch", &iso(now))
        ]
    );
    let records = IvyJson::Array(
        (0..256)
            .map(|n| row(&format!("epoch-{n}"), &iso(now)))
            .collect(),
    );
    fs::write(&path, stringify(&records, 65536).unwrap()).unwrap();
    evidence.retain("last".into()).await.unwrap();
    let retained = evidence.stops().await.unwrap();
    assert_eq!(retained.len(), 256);
    assert_eq!(retained.first(), Some(&row("epoch-1", &iso(now))));
    assert_eq!(retained.last(), Some(&row("last", &iso(now))));
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
