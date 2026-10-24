use super::*;
use ivy_contracts::json::canonical;
use ivy_sdk::service::{ConnectionOptions, InvocationContext, ServiceConnection};
use ivy_secretary::{
    generation::ServiceEvent,
    journal::{Journal, Limits},
    reconcile::{Readiness, ReadinessState, ReconcileConfig, Reconciliation, retry_delay},
    service_handlers::registered_handlers,
};
use std::{
    future::{Future, poll_fn},
    io::{BufRead, BufReader},
    path::PathBuf,
    pin::Pin,
    process::{Child, Command, Stdio},
    task::Poll,
    time::Duration,
};

#[test]
fn original_main_readiness_and_bounded_initialization_retry_policy() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/reconcile-oracle.mjs")
        .current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(&String::from_utf8(output.stdout).unwrap(), 1024 * 1024).unwrap();
    let optional_string = |value: &IvyJson| value.as_js_string().cloned();
    let optional_number = |value: &IvyJson| value.as_f64().map(|n| n as u64);
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 11);
    for row in rows {
        let input = row.required("input").unwrap();
        let state = Readiness {
            generation: optional_number(input.required("engineGeneration").unwrap()),
            recovering: input.required("recovering").unwrap().as_bool().unwrap(),
            failure: optional_string(input.required("failure").unwrap()),
            issue: optional_string(input.required("issue").unwrap()),
        };
        let observed = state.observe(
            input.required("generation").unwrap().as_f64().unwrap() as u64,
            input
                .required("serviceNodeId")
                .unwrap()
                .as_js_string()
                .unwrap(),
            input.required("at").unwrap().as_js_string().unwrap(),
        );
        let actual = IvyJson::object([
            ("ready", observed.ready.into()),
            ("detail", observed.detail.into()),
            ("heartbeat", observed.heartbeat),
        ]);
        assert_eq!(
            canonical(&actual, 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 1024 * 1024).unwrap(),
            "{}",
            row.required("label").unwrap().as_str().unwrap()
        );
    }
    let retries = corpus.required("retryRows").unwrap().as_array().unwrap();
    assert_eq!(retries.len(), 9);
    for row in retries {
        let input = row.required("input").unwrap();
        let expected = row.required("expected").unwrap();
        let failures = input.required("failures").unwrap().as_f64().unwrap() as usize;
        let error = IvyError::new(
            &input.required("code").unwrap().as_str().unwrap(),
            "Fixture initialization failure.",
        );
        let cancelled = input.required("abort").unwrap().as_bool().unwrap();
        let mut delays = vec![];
        let mut attempts = 0;
        loop {
            let attempt = attempts;
            attempts += 1;
            if attempts > failures {
                break;
            }
            match retry_delay(&error, attempt, cancelled) {
                Some(delay) => delays.push(IvyJson::Number(delay.as_millis() as f64)),
                None => break,
            }
        }
        assert_eq!(
            attempts,
            expected.required("attempts").unwrap().as_f64().unwrap() as usize
        );
        assert_eq!(
            IvyJson::Array(delays),
            *expected.required("delays").unwrap()
        );
    }
}

struct SocketFixture(Child);
impl Drop for SocketFixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct DataDirectory(PathBuf);
impl Drop for DataDirectory {
    fn drop(&mut self) {
        for name in [
            "secretary.sqlite",
            "secretary.sqlite-wal",
            "secretary.sqlite-shm",
        ] {
            let _ = std::fs::remove_file(self.0.join(name));
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}
async fn drive_until<F: Future>(mut work: Pin<&mut F>, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            assert!(
                poll_fn(|cx| Poll::Ready(work.as_mut().poll(cx)))
                    .await
                    .is_pending(),
                "Service exited before the expected lifecycle boundary"
            );
            if ready() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Lifecycle fixture did not reach its expected boundary");
}
async fn lifecycle_case(gated: bool, failures: usize, stop_initializing: bool) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = Command::new("node");
    command.args([
        "tests/secretary-rust/generation-fixture.mjs",
        "--reconcile",
        "--failures",
        &failures.to_string(),
    ]);
    if gated {
        command.arg("--gate-stat");
    }
    command
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut socket = SocketFixture(command.spawn().unwrap());
    let mut line = String::new();
    BufReader::new(socket.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let fixture = parse(&line, 1024 * 1024).unwrap();
    let address = format!(
        "http://127.0.0.1:{}",
        fixture.required("port").unwrap().as_f64().unwrap() as u16
    );
    let connection = ServiceConnection::connect(
        &address,
        "fixture",
        ConnectionOptions::new("fixture.secretary".into()),
        RequestOptions::default().cancellation,
    )
    .await
    .unwrap();
    connection.set_generation(7);
    let data = DataDirectory(
        std::env::temp_dir().join(format!("secretary-reconcile-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&data.0).unwrap();
    {
        let journal = ivy_secretary::import_state::runtime_journal(
            &data.0.join("secretary.sqlite"),
            Limits::default(),
            &ivy_secretary::import_state::import_identity(
                fixture.required("settings").unwrap(),
                &"fixture.secretary".into(),
            )
            .unwrap(),
        )
        .unwrap();
        journal.db.execute_batch("PRAGMA user_version=37;CREATE TABLE retained_fixture(value BLOB);INSERT INTO retained_fixture VALUES(x'00ff00d8');").unwrap();
    }
    let readiness = ReadinessState::default();
    let owner = Reconciliation {
        readiness: readiness.clone(),
        config: ReconcileConfig {
            settings: fixture.required("settings").unwrap().clone(),
            service_node_id: "fixture.secretary".into(),
            host_id: "fixture-host".into(),
            data_root: data.0.clone(),
            distribution_root: root,
            job_launcher: None,
            minimum_message_age_minutes: 0.0,
            notification_policy: "Fixture policy".into(),
        },
    };
    let (sender, mut invocations) = tokio::sync::mpsc::unbounded_channel();
    let handlers = registered_handlers(sender);
    let (event, mut events) = tokio::sync::mpsc::unbounded_channel();
    let mut work = Box::pin(owner.run(&mut invocations, &mut events));
    assert!(
        event
            .send(ServiceEvent::Reconcile(connection.clone()))
            .is_ok()
    );
    let at = "2027-01-15T08:00:00.000Z".into();
    let node = "fixture.secretary".into();
    drive_until(work.as_mut(), || readiness.current().recovering).await;
    if gated {
        let observed = connection.request(
            "fixture.waitStat",
            IvyJson::object(Vec::<(String, IvyJson)>::new()),
            RequestOptions::default(),
        );
        tokio::time::timeout(Duration::from_secs(5),async {
            tokio::select!{_=&mut work=>panic!("Initializing service exited"),result=observed=>{result.unwrap();}}
        }).await.unwrap();
        let binding = handlers.get(&"secretary.binding".into()).unwrap();
        let reply = binding(
            IvyJson::Null,
            InvocationContext::detached("caller".into(),None,7,connection.signal(),IvyError::new("service_not_ready","Service generation ended.")),
        );
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {_=&mut work=>panic!("Initializing service exited"),result=reply=>result}
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().code, "service_not_ready");
        assert!(!readiness.current().observe(7, &node, &at).ready);
        if !stop_initializing {
            let release = connection.request(
                "fixture.release",
                IvyJson::object(Vec::<(String, IvyJson)>::new()),
                RequestOptions::default(),
            );
            tokio::select! {_=&mut work=>panic!("Initializing service exited"),result=release=>{result.unwrap();}}
        }
    }
    if !stop_initializing {
        drive_until(work.as_mut(), || {
            readiness.current().observe(7, &node, &at).ready
        })
        .await;
        let trace = connection.request(
            "fixture.trace",
            IvyJson::object(Vec::<(String, IvyJson)>::new()),
            RequestOptions::default(),
        );
        let trace = tokio::select! {_=&mut work=>panic!("Reconciled service exited"),result=trace=>result.unwrap()};
        let calls = trace.as_array().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|v| v.get("method") == Some(&"serviceNodes.get".into()))
                .count(),
            failures + 1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|v| v.get("method") == Some(&"system.status".into()))
                .count(),
            1,
            "Retrying initialization retains the connection's epoch lookup"
        );
    }
    assert!(event.send(ServiceEvent::Stop).is_ok());
    tokio::time::timeout(Duration::from_secs(5), work)
        .await
        .unwrap();
    assert!(connection.signal().is_cancelled());
    assert!(!readiness.current().observe(7, &node, &at).ready);
    let journal = Journal::open(&data.0.join("secretary.sqlite"), Limits::default()).unwrap();
    assert_eq!(
        journal
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        37
    );
    assert_eq!(
        journal
            .db
            .query_row("SELECT value FROM retained_fixture", [], |r| r
                .get::<_, Vec<u8>>(0))
            .unwrap(),
        vec![0, 255, 0, 216]
    );
}

#[tokio::test]
async fn actual_connection_initialization_admission_retry_and_journal_lifetime() {
    lifecycle_case(true, 0, false).await;
    lifecycle_case(false, 1, false).await;
    lifecycle_case(true, 0, true).await;
}
