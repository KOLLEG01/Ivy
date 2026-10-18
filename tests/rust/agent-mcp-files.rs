use ivy_agent_manager::mcp_files::apply_mcp;
use ivy_contracts::{
    IvyError, IvyJson,
    hash::digest,
    json::{parse, stringify},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
};
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-mcp-files-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Client(IvyJson);
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            match method {
                "objects.stat" => Err(IvyError::new("not_found", "missing")),
                "openai.getConfiguration" => Ok(IvyJson::object([
                    ("revision", IvyJson::Number(0.0)),
                    (
                        "configuration",
                        IvyJson::object([
                            ("schemaVersion", IvyJson::Number(1.0)),
                            ("appId", self.0.clone()),
                        ]),
                    ),
                ])),
                _ => panic!("Unexpected request"),
            }
        })
    }
}
fn string(v: &IvyJson) -> String {
    v.as_js_string().unwrap().to_utf8().unwrap()
}
fn oracle(name: &str) -> IvyJson {
    let out = Command::new("node")
        .arg(format!("tests/fixtures/{name}.mjs"))
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    parse(std::str::from_utf8(&out.stdout).unwrap(), 2 * 1024 * 1024).unwrap()
}
#[tokio::test]
async fn actual_files_match_original_ownership_cases() {
    let rows = oracle("agent-mcp-output-oracle");
    let root = Root::new();
    for (index, row) in rows.as_array().unwrap().iter().enumerate() {
        let home = root.0.join(index.to_string());
        fs::create_dir(&home).unwrap();
        let before = string(row.get("fileSource").unwrap());
        fs::write(home.join("config.toml"), before.as_bytes()).unwrap();
        let state_path = home.join(".ivy-agent-mcp/state.json");
        let stored = row.get("storedPresent") == Some(&IvyJson::Bool(true));
        if stored {
            fs::create_dir(home.join(".ivy-agent-mcp")).unwrap();
            fs::write(
                &state_path,
                stringify(row.get("state").unwrap(), usize::MAX).unwrap(),
            )
            .unwrap();
        }
        let client = Client(row.get("appId").unwrap().clone());
        let result = apply_mcp(
            home.clone(),
            &client,
            row.get("document").unwrap(),
            &"fixture".into(),
            row.get("hive").unwrap().as_js_string().unwrap(),
            row.get("credential").unwrap().as_js_string().unwrap(),
        )
        .await;
        let expected = row.get("expected").unwrap();
        match result {
            Ok(result) => {
                let actual = IvyJson::object([
                    (
                        "text",
                        fs::read_to_string(home.join("config.toml")).unwrap().into(),
                    ),
                    (
                        "state",
                        parse(&fs::read_to_string(&state_path).unwrap(), 2 * 1024 * 1024).unwrap(),
                    ),
                    ("applied", result.applied.into()),
                    ("managedBytes", IvyJson::Number(result.managed_bytes as f64)),
                ]);
                assert_eq!(Some(&actual), expected.get("result"), "Case {index}");
            }
            Err(error) => {
                assert_eq!(
                    Some(&IvyJson::from(error.code)),
                    expected.get("error"),
                    "Case {index}"
                );
                assert_eq!(
                    fs::read(home.join("config.toml")).unwrap(),
                    before.as_bytes()
                );
                if stored {
                    assert_eq!(
                        parse(&fs::read_to_string(&state_path).unwrap(), 2 * 1024 * 1024).unwrap(),
                        *row.get("state").unwrap()
                    );
                } else {
                    assert!(!state_path.exists());
                }
            }
        }
        assert!(!fs::read_dir(home).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ivy-environment-")
        }));
    }
}
#[tokio::test]
async fn filesystem_rejections_and_bom_match_original() {
    let data = oracle("agent-mcp-files-oracle");
    let root = Root::new();
    for row in data.get("rows").unwrap().as_array().unwrap() {
        let setup = string(row.get("setup").unwrap());
        let home = root.0.join(&setup);
        fs::create_dir(&home).unwrap();
        let config = home.join("config.toml");
        let state_root = home.join(".ivy-agent-mcp");
        let state = state_root.join("state.json");
        match setup.as_str() {
            "missing" => {}
            "bom" => fs::write(&config, "\u{feff}model=\"fixture\"\r\n").unwrap(),
            "invalid-utf8" => fs::write(&config, [0xc3, 0x28]).unwrap(),
            "config-directory" => fs::create_dir(&config).unwrap(),
            "oversized-config" => fs::write(&config, vec![32; 4 * 1024 * 1024 + 1]).unwrap(),
            "state-directory" | "invalid-state" | "bom-state" | "oversized-state" => {
                fs::create_dir(&state_root).unwrap();
                match setup.as_str() {
                    "state-directory" => fs::create_dir(&state).unwrap(),
                    "invalid-state" => fs::write(&state, "{").unwrap(),
                    "bom-state" => fs::write(&state, "\u{feff}null").unwrap(),
                    _ => fs::write(&state, vec![32; 4 * 1024 * 1024 + 1]).unwrap(),
                }
            }
            "state-root-file" => fs::write(&state_root, "x").unwrap(),
            "linked-state-root" => {
                let dest = root.0.join("link-destination");
                fs::create_dir(&dest).unwrap();
                let status=Command::new("node").args(["--input-type=module","-e","import{symlinkSync}from'node:fs';symlinkSync(process.argv[1],process.argv[2],process.platform==='win32'?'junction':'dir');"]).arg(dest).arg(&state_root).status().unwrap();
                assert!(status.success());
            }
            _ => panic!("Unknown setup"),
        }
        let result = apply_mcp(
            home.clone(),
            &Client(IvyJson::Null),
            data.get("defaults").unwrap(),
            &"fixture".into(),
            &"https://fixture.example".into(),
            &"fixture-only".into(),
        )
        .await;
        let expected = row.get("expected").unwrap();
        let code = result
            .as_ref()
            .err()
            .map(|e| IvyJson::from(e.code.clone()))
            .unwrap_or(IvyJson::Null);
        assert_eq!(Some(&code), expected.get("code"), "{setup}");
        for (path, key) in [(&config, "config"), (&state, "state")] {
            if let Ok(bytes) = fs::read(path) {
                assert_eq!(
                    Some(&IvyJson::from(digest(bytes))),
                    expected.get(key),
                    "{setup} {key}"
                );
            } else {
                assert!(
                    !string(expected.get(key).unwrap()).starts_with("sha256:"),
                    "{setup} {key}"
                );
            }
        }
        assert!(!fs::read_dir(home).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ivy-environment-")
        }));
    }
}
#[tokio::test]
async fn original_node_owner_excludes_rust_until_released() {
    let root = Root::new();
    let lock_root = root.0.join(".ivy-agent-mcp");
    let mut child=Command::new("node").args(["--input-type=module","-e","import{ExecutorLock}from'./dist/packages/host-runtime/src/journal.js';const lock=new ExecutorLock(process.argv[1]);console.log('held');process.stdin.once('data',()=>{lock.close();process.exit(0);});"]).arg(&lock_root).current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "held");
    let defaults = parse(
        "{\"schemaVersion\":1,\"hostId\":null,\"enabled\":true,\"servers\":[]}",
        1024,
    )
    .unwrap();
    let result = apply_mcp(
        root.0.clone(),
        &Client(IvyJson::Null),
        &defaults,
        &"fixture".into(),
        &"https://fixture.example".into(),
        &"fixture-only".into(),
    )
    .await;
    child.stdin.take().unwrap().write_all(b"close\n").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(result.err().unwrap().code, "executor_already_running");
    assert!(!root.0.join("config.toml").exists());
    assert!(
        apply_mcp(
            root.0.clone(),
            &Client(IvyJson::Null),
            &defaults,
            &"fixture".into(),
            &"https://fixture.example".into(),
            &"fixture-only".into()
        )
        .await
        .unwrap()
        .applied
    );
}
