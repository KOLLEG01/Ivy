use ivy_agent_manager::mcp_files::apply_mcp;
use ivy_contracts::{IvyError, IvyJson as J, json::parse};
use ivy_host_runtime::executor_lock::ExecutorLock;
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("ivy-mcp-files-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("Retained MCP files fixture: {}", self.0.display());
            return;
        }
        let actual = fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            actual
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-mcp-files-")
        );
        fs::remove_dir_all(actual).unwrap();
    }
}
struct Client;
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, _: J, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            match method {
                "objects.stat" => Err(IvyError::new("not_found", "Fixture missing")),
                "openai.getConfiguration" => Ok(parse(
                    r#"{"revision":0,"configuration":{"schemaVersion":1,"appId":null}}"#,
                    usize::MAX,
                )
                .unwrap()),
                _ => panic!("Unexpected request: {method}"),
            }
        })
    }
}
async fn apply(home: &Path) -> ivy_contracts::IvyResult<ivy_agent_manager::mcp_files::AppliedMcp> {
    apply_mcp(
        home.to_path_buf(),
        &Client,
        &parse(
            r#"{"schemaVersion":1,"hostId":null,"enabled":true,"servers":[]}"#,
            usize::MAX,
        )
        .unwrap(),
        &"fixture".into(),
        &"https://fixture.example".into(),
        &"fixture-only".into(),
        &Default::default(),
    )
    .await
}
fn read_json(path: &Path) -> J {
    parse(&fs::read_to_string(path).unwrap(), usize::MAX).unwrap()
}

#[tokio::test]
async fn actual_mcp_files_use_current_ownership_and_refuse_local_or_legacy_edits() {
    let root = Root::new();
    let config = root.0.join("config.toml");
    fs::write(
        &config,
        "\u{feff}# user\r\ncustom = true\r\n[other]\r\nvalue = 'keep'\r\n",
    )
    .unwrap();
    let applied = apply(&root.0).await.unwrap();
    assert!(applied.applied);
    assert!(applied.managed_bytes > 0);
    let state_path = root.0.join(".ivy-agent-mcp/state.json");
    let state = read_json(&state_path);
    assert_eq!(state.get("schemaVersion"), Some(&J::Number(2.0)));
    assert_eq!(
        state.get("appliedVersion"),
        Some(&J::from(applied.desired_version))
    );
    let current = fs::read_to_string(&config).unwrap();
    assert!(current.contains("custom = true"));
    assert!(current.contains("[other]\r\nvalue = 'keep'"));
    let repeated = apply(&root.0).await.unwrap();
    assert!(repeated.applied);
    assert_eq!(fs::read_to_string(&config).unwrap(), current);
    let changed = current.replace("approval_policy = \"never\"", "approval_policy = \"ask\"");
    fs::write(&config, &changed).unwrap();
    let before_state = fs::read(&state_path).unwrap();
    assert_eq!(apply(&root.0).await.err().unwrap().code, "target_conflict");
    assert_eq!(fs::read(&config).unwrap(), changed.as_bytes());
    assert_eq!(fs::read(&state_path).unwrap(), before_state);
    fs::write(&config, &current).unwrap();
    let mut legacy = state;
    legacy.insert("schemaVersion", J::Number(1.0));
    fs::write(
        &state_path,
        ivy_contracts::json::stringify(&legacy, usize::MAX).unwrap(),
    )
    .unwrap();
    assert_eq!(apply(&root.0).await.err().unwrap().code, "target_conflict");
    assert_eq!(fs::read(&config).unwrap(), current.as_bytes());
}

#[tokio::test]
async fn mcp_files_refuse_invalid_utf8_nonregular_oversized_and_malformed_state_without_replacing_config()
 {
    for (setup, expected) in [
        ("invalid-utf8", "target_conflict"),
        ("config-directory", "target_conflict"),
        ("oversized-config", "target_conflict"),
        ("state-directory", "target_conflict"),
        ("invalid-state", "internal_error"),
        ("oversized-state", "target_conflict"),
    ] {
        let root = Root::new();
        let config = root.0.join("config.toml");
        let state_root = root.0.join(".ivy-agent-mcp");
        let state_path = state_root.join("state.json");
        match setup {
            "invalid-utf8" => fs::write(&config, [0xc3, 0x28]).unwrap(),
            "config-directory" => fs::create_dir(&config).unwrap(),
            "oversized-config" => fs::write(&config, vec![32; 4 * 1024 * 1024 + 1]).unwrap(),
            _ => {
                fs::write(&config, "# retain\n").unwrap();
                fs::create_dir(&state_root).unwrap();
                match setup {
                    "state-directory" => fs::create_dir(&state_path).unwrap(),
                    "invalid-state" => fs::write(&state_path, "{").unwrap(),
                    _ => fs::write(&state_path, vec![32; 4 * 1024 * 1024 + 1]).unwrap(),
                }
            }
        }
        let before = fs::read(&config).ok();
        assert_eq!(
            apply(&root.0).await.err().unwrap().code,
            expected,
            "{setup}"
        );
        assert_eq!(fs::read(&config).ok(), before);
        assert!(!fs::read_dir(&root.0).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ivy-environment-")
        }));
    }
}

#[tokio::test]
async fn native_owner_lock_excludes_another_mcp_application_until_release() {
    let root = Root::new();
    let lock_root = root.0.join(".ivy-agent-mcp");
    fs::create_dir(&lock_root).unwrap();
    let lock = ExecutorLock::acquire(lock_root, false).await.unwrap();
    assert_eq!(
        apply(&root.0).await.err().unwrap().code,
        "executor_already_running"
    );
    assert!(!root.0.join("config.toml").exists());
    drop(lock);
    assert!(apply(&root.0).await.unwrap().applied);
}
