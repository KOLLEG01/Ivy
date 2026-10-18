use ivy_agent_manager::mcp_manager::{McpManager, McpSettings};
use ivy_contracts::{IvyError, IvyJson, Outcome, hash::digest, json::parse};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Semaphore;
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("ivy-mcp-manager-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn oracle() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let o = Command::new("node")
            .arg("tests/fixtures/agent-mcp-manager-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        parse(std::str::from_utf8(&o.stdout).unwrap(), 2 * 1024 * 1024).unwrap()
    })
}
fn manager(home: PathBuf, enabled: bool) -> McpManager {
    McpManager::new(McpSettings {
        home,
        host_id: "fixture".into(),
        hive_url: "https://fixture.example".into(),
        credential: "fixture-only".into(),
        defaults: IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("hostId", IvyJson::Null),
            ("enabled", enabled.into()),
            ("servers", IvyJson::Array(vec![])),
        ]),
    })
}
fn status(m: &McpManager, base: &Path) -> IvyJson {
    let s = m.status();
    assert_eq!(s.observed_at.len(), 24);
    assert!(chrono::DateTime::parse_from_rfc3339(&s.observed_at).is_ok());
    IvyJson::object([
        ("state", s.state.into()),
        (
            "target",
            s.target
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
                .into(),
        ),
        (
            "desiredVersion",
            s.desired_version
                .map(IvyJson::from)
                .unwrap_or(IvyJson::Null),
        ),
        (
            "appliedVersion",
            s.applied_version
                .map(IvyJson::from)
                .unwrap_or(IvyJson::Null),
        ),
        ("code", s.code.map(IvyJson::from).unwrap_or(IvyJson::Null)),
        ("bytes", IvyJson::Number(s.bytes as f64)),
    ])
}
struct Client {
    revision: u32,
    fail: bool,
    calls: AtomicUsize,
}
fn client(revision: u32) -> Arc<Client> {
    Arc::new(Client {
        revision,
        fail: false,
        calls: AtomicUsize::new(0),
    })
}
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(IvyError::new("service_unavailable", "fixture failure")
                    .with_outcome(Outcome::Unknown));
            }
            match method {
                "objects.stat" => Err(IvyError::new("not_found", "missing")),
                "openai.getConfiguration" => Ok(IvyJson::object([
                    ("revision", IvyJson::Number(self.revision as f64)),
                    (
                        "configuration",
                        IvyJson::object([
                            ("schemaVersion", IvyJson::Number(1.0)),
                            ("appId", IvyJson::Null),
                        ]),
                    ),
                ])),
                _ => panic!("Unexpected method"),
            }
        })
    }
}
#[tokio::test]
async fn bootstrap_preserves_existing_state_and_original_status() {
    let root = Root::new();
    for row in oracle().get("bootstraps").unwrap().as_array().unwrap() {
        let setup = row
            .get("setup")
            .unwrap()
            .as_js_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        let base = root.0.join(&setup);
        fs::create_dir(&base).unwrap();
        let home = base.join("home");
        if setup != "missing-home" {
            fs::create_dir(&home).unwrap();
        }
        if setup.contains("state") {
            let dir = home.join(".ivy-agent-mcp");
            fs::create_dir(&dir).unwrap();
            let path = dir.join("state.json");
            match setup.as_str() {
                "state-directory" => fs::create_dir(path).unwrap(),
                "empty-state" => fs::write(path, "").unwrap(),
                "invalid-state" => fs::write(path, "{").unwrap(),
                "null-state" => fs::write(path, "null").unwrap(),
                _ => fs::write(path, vec![32; 4 * 1024 * 1024 + 1]).unwrap(),
            }
        }
        let m = manager(home.clone(), setup != "disabled");
        assert_eq!(Some(&status(&m, &base)), row.get("initial"));
        let error = m
            .bootstrap()
            .await
            .err()
            .map(|e| IvyJson::from(e.code))
            .unwrap_or(IvyJson::Null);
        m.close().wait().await;
        assert_eq!(Some(&error), row.get("error"), "{setup}");
        assert_eq!(Some(&status(&m, &base)), row.get("status"), "{setup}");
        let config = fs::read(home.join("config.toml"))
            .ok()
            .map(|b| IvyJson::from(digest(b)))
            .unwrap_or(IvyJson::Null);
        assert_eq!(Some(&config), row.get("config"), "{setup}");
    }
}
struct Held {
    calls: Mutex<Vec<String>>,
    started: Semaphore,
    release: Semaphore,
}
impl RpcClient for Held {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let first = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(method.into());
                calls.len() == 1
            };
            if first {
                self.started.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            client(3).request(method, params, options).await
        })
    }
}
#[tokio::test]
async fn synchronization_coalesces_and_keeps_applied_status_after_errors() {
    let root = Root::new();
    let home = root.0.join("home");
    fs::create_dir(&home).unwrap();
    let m = manager(home.clone(), true);
    let mut states = vec![status(&m, &root.0)];
    m.synchronize(client(0)).wait().await;
    states.push(status(&m, &root.0));
    m.synchronize(Arc::new(Client {
        revision: 0,
        fail: true,
        calls: AtomicUsize::new(0),
    }))
    .wait()
    .await;
    states.push(status(&m, &root.0));
    let good = fs::read_to_string(home.join("config.toml")).unwrap();
    fs::write(
        home.join("config.toml"),
        good.replace("approval_policy = \"never\"", "approval_policy = \"ask\""),
    )
    .unwrap();
    m.synchronize(client(1)).wait().await;
    states.push(status(&m, &root.0));
    fs::write(home.join("config.toml"), good).unwrap();
    m.synchronize(client(2)).wait().await;
    m.close().wait().await;
    states.push(status(&m, &root.0));
    assert_eq!(Some(&IvyJson::Array(states)), oracle().get("lifecycle"));
    let held = Arc::new(Held {
        calls: Mutex::new(vec![]),
        started: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    let first = m.synchronize(held.clone());
    held.started.acquire().await.unwrap().forget();
    let second_client = client(99);
    let second = m.synchronize(second_client.clone());
    let mut close = Box::pin(m.close().wait());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), close.as_mut())
            .await
            .is_err()
    );
    let expected = oracle().get("coalescing").unwrap();
    assert_eq!(Some(&status(&m, &root.0)), expected.get("during"));
    held.release.add_permits(1);
    first.wait().await;
    second.wait().await;
    close.await;
    assert_eq!(second_client.calls.load(Ordering::SeqCst), 0);
    assert_eq!(Some(&status(&m, &root.0)), expected.get("after"));
    assert_eq!(
        Some(&IvyJson::Array(
            held.calls
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .map(IvyJson::from)
                .collect()
        )),
        expected.get("calls")
    );
    drop(m.synchronize(client(4)));
    m.close().wait().await;
    assert_eq!(Some(&status(&m, &root.0)), oracle().get("detached"));
}
#[tokio::test]
async fn successful_status_uses_original_realpath_presentation() {
    let root = Root::new();
    let home = root.0.join("home");
    fs::create_dir(&home).unwrap();
    let link = root.0.join("link");
    assert!(Command::new("node").args(["--input-type=module","-e","import{symlinkSync}from'node:fs';symlinkSync(process.argv[1],process.argv[2],process.platform==='win32'?'junction':'dir');"]).arg(home).arg(&link).status().unwrap().success());
    let m = manager(link, true);
    let expected = oracle().get("linked").unwrap();
    assert_eq!(Some(&status(&m, &root.0)), expected.get("initial"));
    m.synchronize(client(0)).wait().await;
    m.close().wait().await;
    assert_eq!(Some(&status(&m, &root.0)), expected.get("after"));
}
