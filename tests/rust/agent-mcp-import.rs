use ivy_agent_manager::{
    mcp_files::apply_mcp,
    mcp_import::import_mcp,
    mcp_prepare::{PreparedMcp, prepare_mcp_import_snapshot},
};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    json::{canonical, parse, stringify},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-mcp-import-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn field<'a>(row: &'a IvyJson, key: &str) -> &'a IvyJson {
    row.get(key).unwrap()
}
fn js<'a>(row: &'a IvyJson, key: &str) -> &'a JsString {
    field(row, key).as_js_string().unwrap()
}
fn text(row: &IvyJson, key: &str) -> String {
    field(row, key).as_str().unwrap()
}
fn same(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        canonical(actual, 4 * 1024 * 1024).unwrap(),
        canonical(expected, 4 * 1024 * 1024).unwrap()
    );
}
fn openai(row: &IvyJson) -> IvyJson {
    IvyJson::object([
        ("revision", IvyJson::Number(0.0)),
        (
            "configuration",
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("appId", field(row, "appId").clone()),
            ]),
        ),
    ])
}
fn prepare(row: &IvyJson) -> PreparedMcp {
    prepare_mcp_import_snapshot(
        field(row, "document"),
        &openai(row),
        &"fixture".into(),
        js(row, "hive"),
        js(row, "credential"),
    )
    .unwrap()
}
fn install(home: &Path, row: &IvyJson) -> (Vec<u8>, Option<Vec<u8>>) {
    fs::create_dir(home).unwrap();
    let config = text(row, "fileSource").into_bytes();
    fs::write(home.join("config.toml"), &config).unwrap();
    let state = if field(row, "storedPresent") == &IvyJson::Bool(true) {
        fs::create_dir(home.join(".ivy-agent-mcp")).unwrap();
        let mut stored = field(row, "state").clone();
        if stored.as_object().is_some() {
            stored.insert(
                "migrationFixture",
                IvyJson::object([
                    ("text", "retained 😀".into()),
                    ("value", IvyJson::Number(7.0)),
                ]),
            );
        }
        let bytes = (stringify(&stored, 4 * 1024 * 1024).unwrap() + "\n").into_bytes();
        fs::write(home.join(".ivy-agent-mcp/state.json"), &bytes).unwrap();
        Some(bytes)
    } else {
        None
    };
    (config, state)
}
struct Client(IvyJson);
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            match method {
                "objects.stat" => Err(IvyError::new("not_found", "fixture missing")),
                "openai.getConfiguration" => Ok(self.0.clone()),
                _ => panic!("unexpected request"),
            }
        })
    }
}

#[tokio::test]
async fn explicit_import_on_original_copies_retains_bytes_values_and_current_runtime_behavior() {
    let root = Root::new();
    let rows = super::mcp_output_cases::original().as_array().unwrap();
    let mut successes = 0;
    let mut conflicts = 0;
    for (index, row) in rows
        .iter()
        .enumerate()
        .filter(|(_, row)| super::mcp_output_cases::requires_import(row))
    {
        let home = root.0.join(index.to_string());
        let (config, old_state) = install(&home, row);
        let imported = import_mcp(home.clone(), prepare(row)).await;
        if let Some(expected) = field(row, "expected").get("error") {
            let error = imported
                .err()
                .expect("original conflict must remain a conflict");
            assert_eq!(IvyJson::from(error.code), *expected, "case {index}");
            assert_eq!(fs::read(home.join("config.toml")).unwrap(), config);
            assert_eq!(
                fs::read(home.join(".ivy-agent-mcp/state.json")).ok(),
                old_state
            );
            conflicts += 1;
            continue;
        }
        let imported = imported.unwrap_or_else(|error| panic!("case {index}: {error:?}"));
        assert!(imported.changed, "case {index}");
        let backup = imported.source_backup.unwrap();
        assert_eq!(fs::read(backup.join("config.toml")).unwrap(), config);
        assert_eq!(fs::read(backup.join("state.json")).ok(), old_state);
        let current = parse(
            &fs::read_to_string(&imported.state_path).unwrap(),
            4 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(current.get("schemaVersion"), Some(&IvyJson::Number(2.0)));
        if let Some(bytes) = old_state.as_ref() {
            let original = parse(std::str::from_utf8(bytes).unwrap(), 4 * 1024 * 1024).unwrap();
            if let Some(extra) = original.get("migrationFixture") {
                same(field(&current, "migrationFixture"), extra);
            }
            if let Some(version) = original.get("appliedVersion") {
                same(field(&current, "appliedVersion"), version);
            }
            if let Some(hash) = original.get("appliedPermissionsBlockHash") {
                same(field(&current, "appliedPermissionsBlockHash"), hash);
            }
        }
        let expected = field(field(row, "expected"), "result");
        assert_eq!(
            fs::read_to_string(&imported.target).unwrap(),
            text(expected, "text"),
            "converted case {index}"
        );
        let repeated = import_mcp(home.clone(), prepare(row)).await.unwrap();
        assert!(!repeated.changed);
        assert!(repeated.source_backup.is_none());
        let result = apply_mcp(
            home.clone(),
            &Client(openai(row)),
            field(row, "document"),
            &"fixture".into(),
            js(row, "hive"),
            js(row, "credential"),
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(&result.target).unwrap(),
            text(expected, "text")
        );
        same(
            &parse(
                &fs::read_to_string(home.join(".ivy-agent-mcp/state.json")).unwrap(),
                4 * 1024 * 1024,
            )
            .unwrap(),
            field(expected, "state"),
        );
        assert_eq!(fs::read(backup.join("config.toml")).unwrap(), config);
        assert_eq!(fs::read(backup.join("state.json")).ok(), old_state);
        successes += 1;
    }
    assert_eq!(successes, 30);
    assert_eq!(conflicts, 74);
}

fn invoke(request: &IvyJson, args: &[&str]) -> (i32, IvyJson, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ivy-agent-mcp-import"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let written = child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(request, 4 * 1024 * 1024).unwrap().as_bytes());
    if args.is_empty() {
        written.unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("fixture-only"),
        "credential must not enter output"
    );
    (
        output.status.code().unwrap(),
        parse(&stdout, 4 * 1024 * 1024).unwrap(),
        stdout,
    )
}
fn request(home: &Path, row: &IvyJson) -> IvyJson {
    IvyJson::object([
        (
            "home",
            ivy_host_runtime::node_path::from_native(home).into(),
        ),
        ("hostId", "fixture".into()),
        ("hiveUrl", field(row, "hive").clone()),
        ("credential", field(row, "credential").clone()),
        ("document", field(row, "document").clone()),
        ("openai", openai(row)),
    ])
}
#[test]
fn offline_command_imports_a_copy_then_refuses_bad_snapshots_without_side_effects() {
    let root = Root::new();
    let row = super::mcp_output_cases::original()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            super::mcp_output_cases::requires_import(row)
                && field(row, "state").get("schemaVersion") == Some(&IvyJson::Number(1.0))
                && text(row, "source").contains(ivy_agent_manager::environment_text::BEGIN_MCP)
                && field(row, "expected").get("result").is_some()
        })
        .unwrap();
    let home = root.0.join("success");
    let (config, state) = install(&home, row);
    let input = request(&home, row);
    let (code, result, _) = invoke(&input, &[]);
    assert_eq!(code, 0);
    assert_eq!(
        field(field(&result, "result"), "changed"),
        &IvyJson::Bool(true)
    );
    let backup =
        ivy_host_runtime::node_path::filesystem_path(js(field(&result, "result"), "sourceBackup"));
    assert_eq!(fs::read(backup.join("config.toml")).unwrap(), config);
    assert_eq!(fs::read(backup.join("state.json")).ok(), state);
    let (code, result, _) = invoke(&input, &[]);
    assert_eq!(code, 0);
    assert_eq!(
        field(field(&result, "result"), "changed"),
        &IvyJson::Bool(false)
    );
    for mode in [
        "unknown-field",
        "wrong-scope",
        "inheritance-only",
        "bad-openai",
        "relative-home",
        "arguments",
    ] {
        let target = root.0.join(mode);
        let (config, state) = install(&target, row);
        let mut input = request(&target, row);
        match mode {
            "unknown-field" => input.insert("extra", true.into()),
            "wrong-scope" => {
                let mut doc = field(&input, "document").clone();
                doc.insert("hostId", "another-host".into());
                input.insert("document", doc);
            }
            "inheritance-only" => {
                let mut doc = field(&input, "document").clone();
                doc.insert("enabled", IvyJson::Null);
                input.insert("document", doc);
            }
            "bad-openai" => input.insert("openai", IvyJson::Null),
            "relative-home" => input.insert("home", "relative".into()),
            _ => {}
        }
        let args = if mode == "arguments" {
            &["--unexpected"][..]
        } else {
            &[]
        };
        let (code, result, _) = invoke(&input, args);
        assert_ne!(code, 0, "{mode}");
        assert!(result.get("error").is_some());
        assert_eq!(fs::read(target.join("config.toml")).unwrap(), config);
        assert_eq!(
            fs::read(target.join(".ivy-agent-mcp/state.json")).ok(),
            state
        );
    }
}
