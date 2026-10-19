use super::{ServiceFixture, assert_json_eq, host_configuration};
use ivy_contracts::{IvyJson as J, hash::hash_json, json};
use ivy_host_runtime::{
    IvyError, IvyResult, config,
    configuration_updater::{self, ConfigurationUpdater},
    host_config,
    journal::HostJournal,
    node_path,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
static CORPUS: Mutex<()> = Mutex::new(());
fn path(v: &J) -> PathBuf {
    node_path::to_native(v.as_js_string().unwrap())
}
fn result(v: IvyResult<J>) -> J {
    match v {
        Ok(v) => J::object([("ok", true.into()), ("value", v)]),
        Err(e) => J::object([("ok", false.into()), ("error", e.to_wire())]),
    }
}
fn call(input: &J) -> J {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/host-configuration-updater-oracle.mjs");
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
        ])
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    json::parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap()
}
fn pair(case: &str, input: &J, actual: &J, original: &J) {
    if let Some(file) = std::env::var_os("IVY_HOST_CONFIGURATION_UPDATER_CORPUS") {
        let _lock = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(
                &J::object([
                    ("case", case.into()),
                    ("input", input.clone()),
                    ("actual", actual.clone()),
                    ("original", original.clone())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_json_eq(actual, original);
}
#[derive(Default)]
struct Client {
    remote: Mutex<Option<J>>,
    error: Mutex<Option<String>>,
    calls: Mutex<Vec<J>>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    block: std::sync::atomic::AtomicBool,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: J,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(J::object([
                ("method", method.into()),
                ("params", params),
                ("timeoutMs", J::Number(options.timeout.as_millis() as f64)),
            ]));
            self.entered.notify_one();
            if self.block.load(std::sync::atomic::Ordering::SeqCst) {
                self.release.notified().await;
            }
            if let Some(code) = self.error.lock().unwrap().clone() {
                return Err(IvyError::new(code, "Fixture remote failure."));
            }
            Ok(self.remote.lock().unwrap().clone().unwrap())
        })
    }
}
async fn initial(f: &ServiceFixture) -> J {
    let mut c = host_configuration(f);
    c.insert(
        "configPath",
        f.0.join("host.json").to_string_lossy().to_string().into(),
    );
    let node = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    c.insert(
        "executables",
        J::object([(
            "node",
            String::from_utf8(node.stdout).unwrap().trim().into(),
        )]),
    );
    let mut a = c
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    a[0].insert("settings", J::object([("value", J::Number(1.0))]));
    let mut second = a[0].clone();
    second.insert("instanceId", "second".into());
    second.insert("serviceNodeId", "second-node".into());
    a.push(second);
    c.insert("instances", J::Array(a));
    host_config::checked_host_config(c, f.0.join("host.json"))
        .await
        .unwrap()
}
fn remote(c: J, revision: f64) -> J {
    J::object([
        ("hostId", c.required("hostId").unwrap().clone()),
        ("revision", J::Number(revision)),
        ("contentHash", hash_json(&c).unwrap().into()),
        ("configuration", c),
    ])
}
fn installed(c: &J) -> Vec<J> {
    c.required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            J::object([
                ("instanceId", v.required("instanceId").unwrap().clone()),
                ("candidateId", format!("sha256:{}", "a".repeat(64)).into()),
                ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
                ("enabled", true.into()),
                ("installedAt", "2026-01-01T00:00:00.000Z".into()),
            ])
        })
        .collect()
}
fn sql(c: &J) -> Connection {
    Connection::open(path(c.required("runtimeRoot").unwrap()).join("deployments.sqlite")).unwrap()
}
async fn native(input: &J) -> J {
    let config_path = path(input.required("configPath").unwrap());
    let c = host_config::checked_host_config(
        input.required("config").unwrap().clone(),
        config_path.clone(),
    )
    .await
    .unwrap();
    config::atomic_json(config_path.clone(), c.clone())
        .await
        .unwrap();
    let mut journal = HostJournal::open(c.clone()).await.unwrap();
    journal
        .save_candidate(
            input.required("candidate").unwrap().clone(),
            input.required("manifest").unwrap().clone(),
        )
        .await
        .unwrap();
    for v in input.required("installed").unwrap().as_array().unwrap() {
        journal.set_installed(v.clone()).await.unwrap();
    }
    if let Some(v) = input.get("busy") {
        journal.accept(v.clone(), None).await.unwrap();
    }
    let client = Arc::new(Client::default());
    *client.remote.lock().unwrap() = Some(input.required("remote").unwrap().clone());
    *client.error.lock().unwrap() = input.get("remoteError").map(|v| v.as_str().unwrap());
    let mut updater =
        ConfigurationUpdater::with_client(config_path.clone(), journal.clone(), client.clone());
    let mut values = Vec::new();
    for step in input.required("steps").unwrap().as_array().unwrap() {
        if let Some(v) = step.get("remote") {
            *client.remote.lock().unwrap() = Some(v.clone());
        }
        if let Some(id) = step.get("failInstance") {
            let id = id.as_str().unwrap();
            assert_eq!(id, "second");
            sql(&c).execute_batch("CREATE TRIGGER fixture_fail_accept BEFORE INSERT ON operations WHEN json_extract(NEW.entry_json,'$.request.instanceId')='second' BEGIN SELECT RAISE(ABORT,'fixture acceptance interruption'); END").unwrap();
        }
        if step.get("removeFailure") == Some(&true.into()) {
            sql(&c)
                .execute_batch("DROP TRIGGER fixture_fail_accept")
                .unwrap();
        }
        if step.get("reopen") == Some(&true.into()) {
            journal.close().await.unwrap();
            journal = HostJournal::open(
                config::json_file(config_path.clone(), config::CONFIGURATION_BYTES)
                    .await
                    .unwrap(),
            )
            .await
            .unwrap();
            updater = ConfigurationUpdater::with_client(
                config_path.clone(),
                journal.clone(),
                client.clone(),
            );
        }
        let value = result(updater.sync().await.map(|v| v.to_json()));
        let mut requests = journal
            .list(200.0)
            .await
            .unwrap()
            .iter()
            .map(|e| e.required("request").unwrap().clone())
            .collect::<Vec<_>>();
        requests.sort_by_key(|r| r.required("operationId").unwrap().as_str().unwrap());
        values.push(J::object([
            ("result", value),
            ("config", journal.config().await.unwrap()),
            (
                "file",
                config::json_file(config_path.clone(), config::CONFIGURATION_BYTES)
                    .await
                    .unwrap(),
            ),
            (
                "pending",
                journal
                    .configuration_adoption()
                    .await
                    .unwrap()
                    .unwrap_or(J::Null),
            ),
            ("requests", J::Array(requests)),
            ("calls", J::Array(client.calls.lock().unwrap().clone())),
        ]));
    }
    journal.close().await.unwrap();
    J::Array(values)
}
#[tokio::test]
async fn configuration_updater_matches_original_admission_actions_hashes_and_recovery() {
    for case in [
        "same",
        "settings",
        "enabled",
        "uninstalled",
        "host",
        "hash",
        "identity",
        "busy",
        "remove-enabled",
        "remove-disabled",
        "bootstrap-settings",
        "bootstrap-enabled",
        "partial-reopen",
        "remote-error",
        "public-url",
    ] {
        let f = ServiceFixture::new();
        let c = initial(&f).await;
        let mut next = c.clone();
        let mut a = next
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        let mut retained = installed(&c);
        let mut steps = vec![J::object([] as [(&str, J); 0])];
        match case {
            "settings" | "busy" | "partial-reopen" => {
                for i in &mut a {
                    i.insert("settings", J::object([("value", J::Number(2.0))]));
                }
            }
            "enabled" => a[0].insert("enabled", false.into()),
            "uninstalled" => {
                retained.clear();
                a[0].insert("settings", J::object([("value", J::Number(2.0))]));
            }
            "identity" => next.insert(
                "artifactRoot",
                f.0.join("other-artifacts")
                    .to_string_lossy()
                    .to_string()
                    .into(),
            ),
            "remove-enabled" | "remove-disabled" => {
                a.remove(1);
                if case == "remove-disabled" {
                    retained[1].insert("enabled", false.into());
                }
            }
            "bootstrap-settings" | "bootstrap-enabled" => {
                let mut before = c
                    .required("instances")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .to_vec();
                before[0].insert("componentId", "host-executor".into());
                next.insert("instances", J::Array(before));
            }
            "public-url" => next.insert("publicBaseUrl", "http://127.0.0.1:1/another".into()),
            _ => {}
        }
        let mut c = c;
        if case.starts_with("bootstrap-") {
            let mut before = c
                .required("instances")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec();
            before[0].insert("componentId", "host-executor".into());
            c.insert("instances", J::Array(before.clone()));
            a = before;
            if case == "bootstrap-settings" {
                a[0].insert("settings", J::object([("value", J::Number(2.0))]));
            } else {
                a[0].insert("enabled", false.into());
            }
        }
        next.insert("instances", J::Array(a));
        let mut r = remote(next, 3.0);
        if case == "host" {
            r.insert("hostId", "another-host".into());
        }
        if case == "hash" {
            r.insert("contentHash", format!("sha256:{}", "f".repeat(64)).into());
        }
        if case == "partial-reopen" {
            steps = vec![
                J::object([("failInstance", "second".into())]),
                J::object([("removeFailure", true.into()), ("reopen", true.into())]),
                J::object([] as [(&str, J); 0]),
            ];
        }
        let mut input = J::object([
            ("config", c.clone()),
            (
                "configPath",
                f.0.join("host.json").to_string_lossy().to_string().into(),
            ),
            ("remote", r),
            ("candidate", super::journal_cases::candidate()),
            ("manifest", super::journal_cases::manifest()),
            ("installed", J::Array(retained)),
            ("steps", J::Array(steps)),
        ]);
        if case == "busy" {
            input.insert(
                "busy",
                J::object([
                    ("instanceId", "fixture".into()),
                    ("action", "restart".into()),
                    ("operationId", "busy".into()),
                ]),
            );
        }
        if case == "remote-error" {
            input.insert("remoteError", "permission_denied".into());
        }
        let expected = call(&input);
        let first = expected.as_array().unwrap()[0].required("result").unwrap();
        if [
            "same",
            "settings",
            "enabled",
            "uninstalled",
            "remove-disabled",
            "bootstrap-settings",
            "public-url",
        ]
        .contains(&case)
        {
            assert_eq!(first.get("ok"), Some(&true.into()), "{case}: {first:?}");
        }
        if case == "bootstrap-settings" {
            assert_eq!(
                first
                    .required("value")
                    .unwrap()
                    .required("bootstrapRequired")
                    .unwrap(),
                &J::Array(vec!["fixture".into()])
            );
        }
        if case == "partial-reopen" {
            let steps = expected.as_array().unwrap();
            assert!(!steps[0].required("pending").unwrap().is_null());
            assert_eq!(
                steps[0]
                    .required("requests")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert!(steps[1].required("pending").unwrap().is_null());
            assert_eq!(
                steps[1]
                    .required("requests")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                steps[2]
                    .required("result")
                    .unwrap()
                    .required("value")
                    .unwrap()
                    .get("changed"),
                Some(&false.into())
            );
        }
        assert_eq!(f.0.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            f.0.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-config-")
        );
        fs::remove_dir_all(&f.0).unwrap();
        fs::create_dir(&f.0).unwrap();
        let actual = native(&input).await;
        pair(case, &input, &actual, &expected);
    }
}

#[tokio::test]
async fn configuration_updater_dropped_observer_keeps_adoption_owned() {
    let f = ServiceFixture::new();
    let c = initial(&f).await;
    let journal = HostJournal::open(c.clone()).await.unwrap();
    config::atomic_json(f.0.join("host.json"), c.clone())
        .await
        .unwrap();
    let mut next = c.clone();
    next.insert(
        "configurationUpdates",
        J::object([("intervalSeconds", J::Number(2.0))]),
    );
    let client = Arc::new(Client::default());
    *client.remote.lock().unwrap() = Some(remote(next.clone(), 4.0));
    client
        .block
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let updater =
        ConfigurationUpdater::with_client(f.0.join("host.json"), journal.clone(), client.clone());
    let waiter = tokio::spawn(async move { updater.sync().await });
    tokio::time::timeout(Duration::from_secs(5), client.entered.notified())
        .await
        .unwrap();
    waiter.abort();
    client.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if journal.config().await.unwrap() == next
                && journal.configuration_adoption().await.unwrap().is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(client.calls.lock().unwrap().len(), 1);
    assert_json_eq(
        &config::json_file(f.0.join("host.json"), config::CONFIGURATION_BYTES)
            .await
            .unwrap(),
        &next,
    );
    journal.close().await.unwrap();
}

#[test]
fn configuration_updater_credential_selection_matches_original_configured_executor() {
    let config = J::object([(
        "instances",
        J::Array(vec![
            J::object([
                ("instanceId", "first".into()),
                ("componentId", "host-executor".into()),
                ("credential", "first-fixture-token".into()),
            ]),
            J::object([
                ("instanceId", "custom-executor".into()),
                ("componentId", "host-executor".into()),
                ("credential", "own-fixture-token".into()),
            ]),
        ]),
    )]);
    let cases = vec![
        J::object([] as [(&str, J); 0]),
        J::object([("own", "custom-executor".into())]),
        J::object([("own", "missing".into())]),
        J::object([("own", "".into())]),
        J::object([
            ("own", "custom-executor".into()),
            ("explicit", "override-fixture-token".into()),
        ]),
        J::object([("own", "custom-executor".into()), ("explicit", "".into())]),
    ];
    let input = J::object([
        ("kind", "credentials".into()),
        ("config", config.clone()),
        ("cases", J::Array(cases.clone())),
    ]);
    let actual = J::Array(
        cases
            .iter()
            .map(|c| {
                configuration_updater::update_credential(
                    &config,
                    c.get("own").map(|v| v.as_str().unwrap()).as_deref(),
                    c.get("explicit").and_then(J::as_js_string),
                )
                .unwrap()
                .map(J::String)
                .unwrap_or(J::Null)
            })
            .collect(),
    );
    pair("credential-selection", &input, &actual, &call(&input));
}

struct NoActions;
impl ivy_host_runtime::executor::ActivationRuntime for NoActions {
    fn prepare<'a>(
        &'a self,
        _: &'a ivy_host_runtime::executor::OperationContext,
        _: &'a J,
        _: &'a str,
    ) -> ivy_host_runtime::executor::ExecutorFuture<'a, J> {
        Box::pin(async { panic!("No source preparation admitted") })
    }
    fn compatibility<'a>(
        &'a self,
        _: &'a ivy_host_runtime::executor::OperationContext,
        _: &'a str,
        _: Option<J>,
    ) -> ivy_host_runtime::executor::ExecutorFuture<'a, ()> {
        Box::pin(async { panic!("No deployment admitted") })
    }
    fn reconcile<'a>(
        &'a self,
        _: &'a ivy_host_runtime::executor::OperationContext,
        _: Option<J>,
        _: bool,
    ) -> ivy_host_runtime::executor::ExecutorFuture<'a, ()> {
        Box::pin(async { panic!("No OS activation admitted") })
    }
    fn dispose(&self) -> ivy_host_runtime::executor::ExecutorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[tokio::test]
async fn configuration_updater_never_substitutes_invalid_credential_code_units() {
    use ivy_host_runtime::executor::{ExecutorOptions, HostExecutor};
    let f = ServiceFixture::new();
    let c = initial(&f).await;
    let options = ExecutorOptions {
        own_instance_id: None,
        account_home: node_path::from_native(&f.0),
        build_id: None,
    };
    assert!(
        HostExecutor::open_with_configuration(
            c.clone(),
            f.0.join("host.json"),
            Some(ivy_contracts::JsString(vec![0xd800])),
            options,
            Arc::new(NoActions)
        )
        .await
        .is_err()
    );
    let lock = ivy_host_runtime::executor_lock::ExecutorLock::acquire(
        path(c.required("runtimeRoot").unwrap()).join("executor"),
        false,
    )
    .await
    .unwrap();
    lock.close().await.unwrap();
}
#[derive(Default)]
struct ServerState {
    response: Mutex<Option<J>>,
    requests: Mutex<Vec<J>>,
    block: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
async fn wait_for(work: impl Future<Output = bool>) {
    assert!(
        tokio::time::timeout(Duration::from_secs(8), work)
            .await
            .unwrap()
    );
}
use std::future::Future;
#[tokio::test]
async fn configuration_updater_coordinator_real_sdk_hints_heartbeat_and_close() {
    use ivy_host_runtime::executor::{ExecutorOptions, HostExecutor};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let f = ServiceFixture::new();
    let mut c = initial(&f).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    c.insert(
        "publicBaseUrl",
        format!("http://{}/base", listener.local_addr().unwrap()).into(),
    );
    // A credentialed older seed without configurationUpdates must still sync.
    c.remove("configurationUpdates");
    let config_path = f.0.join("host.json");
    config::atomic_json(config_path.clone(), c.clone())
        .await
        .unwrap();
    let state = Arc::new(ServerState::default());
    *state.response.lock().unwrap() = Some(remote(c.clone(), 7.0));
    state.block.store(true, std::sync::atomic::Ordering::SeqCst);
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let state = server_state.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    bytes.push(byte[0]);
                    assert!(bytes.len() < 16384);
                }
                let header = String::from_utf8(bytes).unwrap().to_lowercase();
                assert!(header.starts_with("post /base/api/v1/rpc "));
                assert!(header.contains("authorization: bearer own-fixture-token\r\n"));
                let count = header
                    .lines()
                    .find_map(|v| v.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse::<usize>()
                    .unwrap();
                let mut body = vec![0; count];
                socket.read_exact(&mut body).await.unwrap();
                let request = json::parse(std::str::from_utf8(&body).unwrap(), 65536).unwrap();
                assert_eq!(
                    request.required("method").unwrap(),
                    &J::from("hostConfigurations.get")
                );
                state.requests.lock().unwrap().push(request.clone());
                state.entered.notify_one();
                if state.block.load(std::sync::atomic::Ordering::SeqCst) {
                    state.release.notified().await;
                }
                let response = J::object([
                    ("jsonrpc", "2.0".into()),
                    ("id", request.required("id").unwrap().clone()),
                    ("result", state.response.lock().unwrap().clone().unwrap()),
                ]);
                let body = json::stringify(&response, 65536).unwrap();
                let output = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(output.as_bytes()).await.unwrap();
            });
        }
    });
    let options = ExecutorOptions {
        own_instance_id: Some("custom-executor".into()),
        account_home: node_path::from_native(&f.0),
        build_id: None,
    };
    let owner = HostExecutor::open_with_configuration(
        c.clone(),
        config_path,
        Some("own-fixture-token".into()),
        options,
        Arc::new(NoActions),
    )
    .await
    .unwrap();
    owner.start().unwrap();
    tokio::time::timeout(Duration::from_secs(5), state.entered.notified())
        .await
        .unwrap();
    let status_path = path(c.required("runtimeRoot").unwrap()).join("executor.json");
    tokio::time::sleep(Duration::from_millis(1150)).await;
    let first = config::json_file(status_path.clone(), config::CONFIGURATION_BYTES)
        .await
        .unwrap();
    assert_eq!(first.required("state").unwrap(), &J::from("ready"));
    assert!(first.get("configurationRevision").is_none());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let second = config::json_file(status_path.clone(), config::CONFIGURATION_BYTES)
        .await
        .unwrap();
    assert_ne!(first.get("observedAt"), second.get("observedAt"));
    state
        .block
        .store(false, std::sync::atomic::Ordering::SeqCst);
    state.release.notify_one();
    wait_for(async {
        loop {
            if config::json_file(status_path.clone(), config::CONFIGURATION_BYTES)
                .await
                .ok()
                .is_some_and(|v| v.get("configurationRevision") == Some(&J::Number(7.0)))
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(state.requests.lock().unwrap().len(), 1);
    let hint_path = path(c.required("runtimeRoot").unwrap()).join("desired-state-hint.json");
    let mut invalid = remote(c.clone(), 8.0);
    invalid.insert("contentHash", format!("sha256:{}", "f".repeat(64)).into());
    *state.response.lock().unwrap() = Some(invalid);
    let hint = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("observedAt", "first".into()),
        ("configuration", true.into()),
        ("packages", false.into()),
    ]);
    config::atomic_json(hint_path.clone(), hint.clone())
        .await
        .unwrap();
    wait_for(async {
        loop {
            if config::json_file(status_path.clone(), config::CONFIGURATION_BYTES)
                .await
                .ok()
                .is_some_and(|v| {
                    v.get("code") == Some(&"configuration_changed".into())
                        && v.get("configurationRevision") == Some(&J::Number(7.0))
                })
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(450)).await;
    assert_eq!(state.requests.lock().unwrap().len(), 2);
    *state.response.lock().unwrap() = Some(remote(c.clone(), 9.0));
    let mut hint = hint;
    hint.insert("observedAt", "second".into());
    config::atomic_json(hint_path.clone(), hint.clone())
        .await
        .unwrap();
    wait_for(async {
        loop {
            if config::json_file(status_path.clone(), config::CONFIGURATION_BYTES)
                .await
                .ok()
                .is_some_and(|v| {
                    v.get("code") == Some(&J::Null)
                        && v.get("configurationRevision") == Some(&J::Number(9.0))
                })
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(state.requests.lock().unwrap().len(), 3);
    state.block.store(true, std::sync::atomic::Ordering::SeqCst);
    hint.insert("observedAt", "third".into());
    config::atomic_json(hint_path, hint).await.unwrap();
    wait_for(async {
        loop {
            if state.requests.lock().unwrap().len() == 4 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let closer = owner.clone();
    let waiter = tokio::spawn(async move { closer.close().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiter.is_finished());
    waiter.abort();
    state.release.notify_one();
    tokio::time::timeout(Duration::from_secs(8), owner.completion())
        .await
        .unwrap()
        .unwrap();
    let stopped = config::json_file(status_path, config::CONFIGURATION_BYTES)
        .await
        .unwrap();
    assert_eq!(stopped.required("state").unwrap(), &J::from("stopping"));
    let lock = ivy_host_runtime::executor_lock::ExecutorLock::acquire(
        path(c.required("runtimeRoot").unwrap()).join("executor"),
        false,
    )
    .await
    .unwrap();
    lock.close().await.unwrap();
    server.abort();
    if let Some(file) = std::env::var_os("IVY_HOST_CONFIGURATION_UPDATER_CORPUS") {
        let requests = J::Array(
            state
                .requests
                .lock()
                .unwrap()
                .iter()
                .map(|v| {
                    J::object([
                        ("method", v.required("method").unwrap().clone()),
                        ("params", v.required("params").unwrap().clone()),
                    ])
                })
                .collect(),
        );
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(
                &J::object([
                    ("case", "native-sdk-poll-runtime".into()),
                    ("requests", requests),
                    ("heartbeatWhileBlocked", true.into()),
                    ("unchangedHintConsumed", true.into()),
                    ("closeRetirement", true.into()),
                    ("original", J::Null)
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
}
