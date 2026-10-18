use ivy_agent_manager::{
    environment::{EnvironmentKind, environment_name, read_environment_document},
    instructions_manager::{InstructionsManager, InstructionsSettings},
    instructions_source::{
        DiscoveryHint, EnglishHintComparator, compact_mcp_instructions, discover_mcp_instructions,
        instruction_summary, legacy_mcp_instructions,
    },
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::digest,
    json::{canonical, parse},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};

fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let out = Command::new("node")
            .arg("tests/fixtures/agent-instructions-source-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        parse(std::str::from_utf8(&out.stdout).unwrap(), 4 * 1024 * 1024).unwrap()
    })
}
fn field<'a>(v: &'a IvyJson, k: &str) -> &'a IvyJson {
    v.get(k).unwrap()
}
fn string(v: &IvyJson) -> String {
    v.as_js_string().unwrap().to_utf8().unwrap()
}
fn equal(actual: &IvyJson, expected: &IvyJson, label: &str) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap(),
        "{label}"
    );
}
fn outcome(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("result", value)]),
        Err(e) => IvyJson::object([("error", e.to_wire())]),
    }
}
fn error(v: &IvyJson) -> IvyError {
    IvyError::new(string(field(v, "code")), string(field(v, "message"))).with_outcome(match string(
        field(v, "outcome"),
    )
    .as_str()
    {
        "unknown" => Outcome::Unknown,
        "completed" => Outcome::Completed,
        _ => Outcome::NotExecuted,
    })
}
fn comparator() -> Arc<EnglishHintComparator> {
    let collation = field(data(), "collation");
    let values = field(collation, "values").as_array().unwrap();
    let indices: HashMap<JsString, usize> = values
        .iter()
        .enumerate()
        .map(|(i, v)| (v.as_js_string().unwrap().clone(), i))
        .collect();
    let matrix = field(collation, "matrix")
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_f64().unwrap() as i8)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    Arc::new(move |left, right| {
        let a = indices.get(left).expect("Unplanned comparator input");
        let b = indices.get(right).expect("Unplanned comparator input");
        matrix[*a][*b].cmp(&0)
    })
}
struct Replay {
    replies: Vec<IvyJson>,
    index: Mutex<usize>,
}
impl Replay {
    fn new(v: &IvyJson) -> Self {
        Self {
            replies: v.as_array().unwrap().to_vec(),
            index: Mutex::new(0),
        }
    }
    fn consumed(&self) {
        assert_eq!(*self.index.lock().unwrap(), self.replies.len());
    }
}
impl RpcClient for Replay {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let reply = {
                let mut index = self.index.lock().unwrap();
                let reply = self.replies.get(*index).expect("Extra request").clone();
                *index += 1;
                reply
            };
            assert_eq!(field(&reply, "method"), &IvyJson::from(method));
            equal(&params, field(&reply, "params"), method);
            if let Some(e) = reply.get("error") {
                Err(error(e))
            } else {
                Ok(field(&reply, "result").clone())
            }
        })
    }
}
struct ReadClient {
    row: IvyJson,
    calls: Mutex<Vec<IvyJson>>,
}
impl RpcClient for ReadClient {
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
            let (stat, fail) = if method == "objects.stat" {
                ("stat", "statError")
            } else {
                assert_eq!(method, "objects.read");
                ("value", "readError")
            };
            if let Some(e) = self.row.get(fail).filter(|e| !e.is_null()) {
                Err(error(e))
            } else {
                Ok(field(&self.row, stat).clone())
            }
        })
    }
}
#[tokio::test]
async fn instruction_document_reads_keep_original_errors_scope_and_revision() {
    let names = field(data(), "names").as_array().unwrap();
    assert_eq!(names.len(), 11);
    for row in names {
        assert_eq!(
            &IvyJson::from(environment_name(
                EnvironmentKind::Instructions,
                field(row, "hostId").as_js_string()
            )),
            field(row, "expected")
        );
    }
    let rows = field(data(), "reads").as_array().unwrap();
    assert_eq!(rows.len(), 34);
    for row in rows {
        let client = ReadClient {
            row: row.clone(),
            calls: Mutex::new(Vec::new()),
        };
        let actual = read_environment_document(
            &client,
            EnvironmentKind::Instructions,
            field(row, "hostId").as_js_string(),
        )
        .await
        .map(|v| v.unwrap_or(IvyJson::Null));
        equal(
            &outcome(actual),
            field(row, "expected"),
            &string(field(row, "mode")),
        );
        equal(
            &IvyJson::Array(client.calls.into_inner().unwrap()),
            field(row, "calls"),
            "read trace",
        );
    }
}
#[test]
fn compact_and_legacy_guidance_match_original_projection_with_supplied_collation() {
    assert_eq!(
        &IvyJson::String(instruction_summary().clone()),
        field(data(), "summary")
    );
    let compact = field(data(), "compact").as_array().unwrap();
    assert_eq!(compact.len(), 12);
    for row in compact {
        assert_eq!(
            &IvyJson::String(compact_mcp_instructions(
                field(row, "text").as_js_string().unwrap()
            )),
            field(row, "expected")
        );
    }
    let compare = comparator();
    let rows = field(data(), "legacy").as_array().unwrap();
    assert_eq!(rows.len(), 98);
    for (index, row) in rows.iter().enumerate() {
        let hints = field(row, "hints")
            .as_array()
            .unwrap()
            .iter()
            .map(|v| DiscoveryHint {
                service_name: field(v, "serviceName").as_js_string().unwrap().clone(),
                discovery_hint: v
                    .get("discoveryHint")
                    .and_then(IvyJson::as_js_string)
                    .cloned(),
            })
            .collect::<Vec<_>>();
        equal(
            &outcome(legacy_mcp_instructions(&hints, compare.as_ref()).map(IvyJson::String)),
            field(row, "expected"),
            &format!("legacy {index}"),
        );
    }
}
#[tokio::test]
async fn modern_and_bounded_legacy_discovery_keep_original_request_trace() {
    let compare = comparator();
    let rows = field(data(), "discovery").as_array().unwrap();
    assert_eq!(rows.len(), 26);
    for row in rows {
        let client = Replay::new(field(row, "replies"));
        let actual = discover_mcp_instructions(&client, compare.as_ref())
            .await
            .map(IvyJson::String);
        equal(
            &outcome(actual),
            field(row, "expected"),
            &string(field(row, "name")),
        );
        client.consumed();
    }
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-instructions-source-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn unhex(v: &IvyJson) -> Vec<u8> {
    let s = string(v);
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn file_hash(path: &Path) -> IvyJson {
    match fs::read(path) {
        Ok(bytes) => digest(bytes).into(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => IvyJson::Null,
        Err(e) => panic!("{e}"),
    }
}
#[tokio::test]
async fn manager_composition_and_bootstrap_preserve_original_files_and_status() {
    let root = Root::new();
    let compare = comparator();
    let rows = field(data(), "managers").as_array().unwrap();
    assert_eq!(rows.len(), 46);
    for (index, row) in rows.iter().enumerate() {
        let home = root.0.join(index.to_string());
        fs::create_dir(&home).unwrap();
        for (name, v) in field(row, "files").as_object().unwrap() {
            let path = home.join(name.to_utf8().unwrap());
            if v.get("directory") == Some(&IvyJson::Bool(true)) {
                fs::create_dir_all(path).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, unhex(field(v, "hex"))).unwrap();
            }
        }
        let manager = InstructionsManager::new(InstructionsSettings {
            home: home.clone(),
            host_id: "fixture".into(),
            hive_url: "https://fixture.example".into(),
            defaults: field(row, "defaults").clone(),
            english_hint_comparator: compare.clone(),
        });
        let client = Arc::new(Replay::new(field(row, "replies")));
        if row.get("bootstrap") == Some(&IvyJson::Bool(true)) {
            manager.bootstrap().wait().await;
        } else {
            manager.synchronize(client.clone()).wait().await;
        }
        manager.close().wait().await;
        client.consumed();
        let s = manager.status();
        assert_eq!(s.home, home);
        assert_eq!(s.observed_at.len(), 24);
        let status = IvyJson::object([
            ("state", s.state.into()),
            ("home", ".".into()),
            (
                "desiredVersion",
                s.desired_version
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null),
            ),
            ("appliedVersion", s.applied_version),
            ("code", s.code.map(IvyJson::from).unwrap_or(IvyJson::Null)),
            ("bytes", IvyJson::Number(s.bytes as f64)),
        ]);
        let actual = IvyJson::object([
            ("status", status),
            ("target", file_hash(&home.join("AGENTS.override.md"))),
            (
                "state",
                file_hash(&home.join(".ivy-agent-instructions/state.json")),
            ),
        ]);
        equal(
            &actual,
            field(row, "expected"),
            &format!("manager {index} {}", string(field(row, "mode"))),
        );
    }
}
