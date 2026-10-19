use ivy_agent_manager::claude_mcp::{ClaudeMcpManager, ClaudeMcpSettings};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    hash::{digest, hash_json},
    json::{canonical, parse, stringify},
};
use ivy_host_runtime::node_path;
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Notify, watch};
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-claude-mcp-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn same(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap()
    );
}
fn original() -> &'static IvyJson {
    static VALUE: OnceLock<IvyJson> = OnceLock::new();
    VALUE.get_or_init(|| {
        let output = Command::new("node")
            .arg("tests/fixtures/agent-claude-mcp-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
fn document(enabled: bool) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", IvyJson::Null),
        ("enabled", enabled.into()),
        (
            "servers",
            IvyJson::Array(if enabled {
                vec![IvyJson::object([
                    ("name", "fixture".into()),
                    ("url", "https://fixture.example/mcp".into()),
                    ("authentication", "agent-manager".into()),
                    ("enabled", true.into()),
                    ("startupTimeoutSeconds", IvyJson::Number(10.0)),
                    ("toolTimeoutSeconds", IvyJson::Number(30.0)),
                ])]
            } else {
                vec![]
            }),
        ),
    ])
}
fn new_manager(
    target: &Path,
    doc: IvyJson,
    hive: JsString,
    credential: JsString,
) -> ClaudeMcpManager {
    ClaudeMcpManager::new(ClaudeMcpSettings {
        target: node_path::from_native(target),
        host_id: "fixture".into(),
        hive_url: hive,
        credential,
        defaults: doc,
    })
    .unwrap()
}
fn authority() -> IvyJson {
    hash_json(&IvyJson::object([
        ("hostId", "fixture".into()),
        ("hiveUrl", "https://fixture.example".into()),
    ]))
    .unwrap()
    .into()
}
fn state_path(target: &Path) -> PathBuf {
    let mut path = target.as_os_str().to_os_string();
    path.push(".state.json");
    PathBuf::from(path)
}
fn owned_state(version: i32, authority: IvyJson, hash: IvyJson) -> Vec<u8> {
    stringify(
        &IvyJson::object([
            ("schemaVersion", IvyJson::Number(version as f64)),
            ("authority", authority),
            ("appliedHash", hash),
        ]),
        usize::MAX,
    )
    .unwrap()
    .into_bytes()
}
fn install(base: &Path, mode: &str) -> PathBuf {
    fs::create_dir(base).unwrap();
    let target = base.join("claude-mcp.json");
    let state = state_path(&target);
    match mode {
        "target-directory" => fs::create_dir(&target).unwrap(),
        "state-directory" => fs::create_dir(&state).unwrap(),
        "oversized-target" => fs::write(&target, vec![32; 4 * 1024 * 1024 + 1]).unwrap(),
        "oversized-state" => fs::write(&state, vec![32; 4097]).unwrap(),
        "malformed-state" => fs::write(&state, "{").unwrap(),
        "bom-state" => fs::write(&state, "\u{feff}null").unwrap(),
        "null-state" => fs::write(&state, "null").unwrap(),
        "primitive-state" => fs::write(&state, "false").unwrap(),
        _ => {}
    }
    let old = b"{\"prior\":true}\n";
    if [
        "output-only",
        "owned",
        "wrong-authority",
        "wrong-hash",
        "wrong-version",
        "partial-publication",
    ]
    .contains(&mode)
    {
        fs::write(&target, old).unwrap();
    }
    if [
        "state-only",
        "owned",
        "wrong-authority",
        "wrong-hash",
        "wrong-version",
        "partial-publication",
    ]
    .contains(&mode)
    {
        let version = if mode == "wrong-version" { 2 } else { 1 };
        let auth = if mode == "wrong-authority" {
            "other".into()
        } else {
            authority()
        };
        let hash = if ["wrong-hash", "partial-publication"].contains(&mode) {
            format!("sha256:{}", "0".repeat(64)).into()
        } else {
            digest(old).into()
        };
        fs::write(state, owned_state(version, auth, hash)).unwrap();
    }
    target
}
fn snapshot_file(path: &Path) -> IvyJson {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return IvyJson::object([("missing", true.into())]);
        }
        Ok(metadata) if metadata.is_dir() => {
            return IvyJson::object([("kind", "directory".into())]);
        }
        Ok(_) => {}
        Err(error) => panic!("{error}"),
    }
    let bytes = fs::read(path).unwrap();
    IvyJson::object([
        ("hash", digest(&bytes).into()),
        ("bytes", IvyJson::Number(bytes.len() as f64)),
        (
            "text",
            if bytes.len() <= 8192 {
                String::from_utf8_lossy(&bytes).to_string().into()
            } else {
                IvyJson::Null
            },
        ),
    ])
}
fn snapshot(manager: &ClaudeMcpManager, base: &Path) -> IvyJson {
    let status = manager.status();
    assert_eq!(status.observed_at.len(), 24);
    assert!(status.observed_at.ends_with('Z'));
    let mut value = IvyJson::object(
        status
            .json()
            .as_object()
            .unwrap()
            .iter()
            .filter(|(name, _)| name != &JsString::from("observedAt"))
            .cloned(),
    );
    value.insert(
        "target",
        status
            .target
            .strip_prefix(base)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
            .into(),
    );
    IvyJson::object([
        ("status", value),
        ("config", snapshot_file(&status.target)),
        ("state", snapshot_file(&state_path(&status.target))),
    ])
}
#[derive(Default)]
struct Client {
    calls: Mutex<Vec<IvyJson>>,
    fail: bool,
    entered: Option<Arc<Notify>>,
    release: Option<watch::Receiver<bool>>,
    override_document: Option<IvyJson>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params),
            ]));
            if let Some(entered) = &self.entered {
                entered.notify_one();
            }
            if let Some(release) = &self.release {
                let mut release = release.clone();
                while !*release.borrow_and_update() {
                    release.changed().await.unwrap();
                }
            }
            if self.fail {
                Err(IvyError::new("service_unavailable", "fixture unavailable"))
            } else if let Some(document) = &self.override_document {
                match method {
                    "objects.stat" => Ok(IvyJson::object([
                        ("id", "override".into()),
                        ("currentRevision", IvyJson::Number(1.0)),
                        ("effectivelyArchived", false.into()),
                    ])),
                    "objects.read" => Ok(IvyJson::object([
                        (
                            "object",
                            IvyJson::object([("contractKey", "agent/mcp-configuration".into())]),
                        ),
                        (
                            "revision",
                            IvyJson::object([("contractVersion", "1.0.0".into())]),
                        ),
                        (
                            "content",
                            IvyJson::object([
                                ("encoding", "json".into()),
                                ("value", document.clone()),
                            ]),
                        ),
                    ])),
                    _ => panic!("unexpected method {method}"),
                }
            } else {
                assert_eq!(method, "objects.stat");
                Err(IvyError::new("not_found", "fixture missing"))
            }
        })
    }
}
fn calls(client: &Client) -> IvyJson {
    IvyJson::Array(client.calls.lock().unwrap().clone())
}
#[tokio::test]
async fn original_claude_mcp_policy_and_owned_files_match() {
    let root = Root::new();
    let rows = field(original(), "rows").as_array().unwrap();
    assert_eq!(rows.len(), 37);
    for row in rows {
        let mode = field(row, "mode").as_str().unwrap();
        let base = root.0.join(&mode);
        let target = install(&base, &mode);
        let credential = if field(row, "credentialMode") == &IvyJson::from("large") {
            JsString::from("x".repeat(4 * 1024 * 1024))
        } else {
            field(row, "credential").as_js_string().unwrap().clone()
        };
        let manager = new_manager(
            &target,
            field(row, "document").clone(),
            field(row, "hive").as_js_string().unwrap().clone(),
            credential,
        );
        let client = Arc::new(Client {
            override_document: row.get("override").cloned(),
            ..Default::default()
        });
        manager.synchronize(client.clone()).wait().await;
        manager.close().wait().await;
        same(&calls(&client), field(row, "calls"));
        same(&snapshot(&manager, &base), field(row, "expected"));
    }
    let relative = ClaudeMcpManager::new(ClaudeMcpSettings {
        target: "relative".into(),
        host_id: "fixture".into(),
        hive_url: "https://fixture.example".into(),
        credential: "fixture-only".into(),
        defaults: document(true),
    })
    .err()
    .unwrap();
    same(&relative.code.into(), field(original(), "relativeError"));
}
#[tokio::test]
async fn claude_mcp_retains_success_refuses_memory_hash_changes_and_preserves_current_bootstrap() {
    let root = Root::new();
    let base = root.0.join("lifetime");
    let target = install(&base, "fresh");
    let manager = new_manager(
        &target,
        document(true),
        "https://fixture.example".into(),
        "fixture-only".into(),
    );
    let client = Arc::new(Client::default());
    let mut history = Vec::new();
    manager.synchronize(client.clone()).wait().await;
    history.push(snapshot(&manager, &base));
    let failed = Arc::new(Client {
        fail: true,
        ..Default::default()
    });
    manager.synchronize(failed.clone()).wait().await;
    history.push(snapshot(&manager, &base));
    let changed = b"{\"external\":true}\n";
    fs::write(&target, changed).unwrap();
    fs::write(
        state_path(&target),
        owned_state(1, authority(), digest(changed).into()),
    )
    .unwrap();
    manager.synchronize(client.clone()).wait().await;
    history.push(snapshot(&manager, &base));
    same(
        &IvyJson::Array(history),
        field(field(original(), "lifetime"), "history"),
    );
    let bootstrap_base = root.0.join("bootstrap");
    let target = install(&bootstrap_base, "fresh");
    let first = new_manager(
        &target,
        document(false),
        "https://fixture.example".into(),
        "fixture-only".into(),
    );
    first.bootstrap().wait().await;
    let next = new_manager(
        &target,
        document(true),
        "https://fixture.example".into(),
        "fixture-only".into(),
    );
    next.bootstrap().wait().await;
    same(
        &snapshot(&next, &bootstrap_base),
        field(field(original(), "bootstrap"), "afterBootstrap"),
    );
    next.synchronize(failed).wait().await;
    same(
        &snapshot(&next, &bootstrap_base),
        field(field(original(), "bootstrap"), "afterUnavailable"),
    );
}
#[tokio::test]
async fn claude_mcp_coalesces_and_close_waits_after_observers_are_dropped() {
    let root = Root::new();
    let base = root.0.join("coalesced");
    let target = install(&base, "fresh");
    let manager = new_manager(
        &target,
        document(true),
        "https://fixture.example".into(),
        "fixture-only".into(),
    );
    let entered = Arc::new(Notify::new());
    let (release, receiver) = watch::channel(false);
    let first = Arc::new(Client {
        entered: Some(entered.clone()),
        release: Some(receiver),
        ..Default::default()
    });
    let second = Arc::new(Client::default());
    let run = manager.synchronize(first.clone());
    entered.notified().await;
    let joined = manager.synchronize(second.clone());
    drop(run);
    let closed = Arc::new(AtomicBool::new(false));
    let closed_flag = closed.clone();
    let close = manager.close();
    let close = tokio::spawn(async move {
        close.wait().await;
        closed_flag.store(true, Ordering::Release);
    });
    tokio::task::yield_now().await;
    let closed_while_blocked = closed.load(Ordering::Acquire);
    release.send_replace(true);
    joined.wait().await;
    close.await.unwrap();
    let expected = field(original(), "coalesced");
    same(&calls(&first), field(expected, "firstCalls"));
    same(&calls(&second), field(expected, "secondCalls"));
    same(
        &closed_while_blocked.into(),
        field(expected, "closedWhileBlocked"),
    );
    same(&snapshot(&manager, &base), field(expected, "expected"));
}
