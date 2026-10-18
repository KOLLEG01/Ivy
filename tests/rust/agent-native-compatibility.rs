#[path = "../../packages/host-runtime/rust/src/native_catalog_compatibility.rs"]
mod compatibility;
#[path = "../../packages/host-runtime/rust/src/concurrency.rs"]
mod concurrency;
use ivy_contracts::{
    IvyError, IvyJson, Outcome,
    json::{parse, stringify},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};
fn json(s: &str) -> IvyJson {
    parse(s, usize::MAX).unwrap()
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("ivy-native-compatibility-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("ivy-native-compatibility-")
        );
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Client {
    input: IvyJson,
    trace: Mutex<Vec<IvyJson>>,
    page: Mutex<usize>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut trace =
                IvyJson::object([("method", method.into()), ("params", params.clone())]);
            if method != "tools.list" {
                trace.insert(
                    "timeout",
                    IvyJson::Number(options.timeout.as_millis() as f64),
                );
            }
            self.trace.lock().unwrap().push(trace);
            if self.input.get("fail") == Some(&IvyJson::from(method)) {
                return Err(
                    IvyError::new("fixture_failure", "Read failed.").with_outcome(Outcome::Unknown)
                );
            }
            if method == "serviceNodes.list" {
                let mut p = self.page.lock().unwrap();
                let result = self.input.required("pages")?.as_array().unwrap()[*p].clone();
                *p += 1;
                return Ok(result);
            }
            if method == "tools.list" {
                let mut definition = json(
                    r#"{"namespace":"agent","name":"status","interfaceVersion":"1.0.0","annotations":{"readOnlyHint":true},"inputSchema":{},"outputSchema":{}}"#,
                );
                if let Some(version) = self.input.get("interface") {
                    definition.insert("interfaceVersion", version.clone());
                }
                return Ok(IvyJson::object([
                    (
                        "provider",
                        IvyJson::object([(
                            "node",
                            IvyJson::object([(
                                "serviceNodeId",
                                params.required("serviceNodeId")?.clone(),
                            )]),
                        )]),
                    ),
                    (
                        "items",
                        IvyJson::Array(vec![IvyJson::object([
                            ("qualifiedName", "agent.status".into()),
                            ("definition", definition),
                            (
                                "definitionHash",
                                format!("sha256:{}", "a".repeat(64)).into(),
                            ),
                            ("catalogHash", format!("sha256:{}", "b".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", IvyJson::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            Ok(self.input.required("status")?.clone())
        })
    }
}
fn client(input: IvyJson) -> Arc<Client> {
    Arc::new(Client {
        input,
        trace: Mutex::new(vec![]),
        page: Mutex::new(0),
    })
}
#[tokio::test]
async fn compatibility_matches_original_checks_and_exact_wire_traces() {
    let f = Fixture::new();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child = Command::new("node")
        .arg("tests/fixtures/native-compatibility-oracle.mjs")
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            stringify(
                &IvyJson::object([("root", f.0.to_string_lossy().into_owned().into())]),
                usize::MAX,
            )
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = json(std::str::from_utf8(&output.stdout).unwrap());
    assert!(rows.as_array().unwrap().len() >= 35);
    for row in rows.as_array().unwrap() {
        let input = row.get("input").unwrap();
        let c = client(input.clone());
        let root = PathBuf::from(input.get("root").unwrap().as_str().unwrap());
        let result = if input.get("mode") == Some(&"configured".into()) {
            compatibility::verify_configured_native_catalog(
                &root,
                input.get("instance").unwrap().as_js_string().unwrap(),
                input.get("settings").unwrap(),
            )
            .await
        } else {
            compatibility::verify_native_catalog_compatibility(
                c.clone(),
                &root,
                input.get("component").unwrap().as_js_string().unwrap(),
            )
            .await
        };
        let actual = match result {
            Ok(()) => json(r#"{"ok":true}"#),
            Err(e) => IvyJson::object([("ok", false.into()), ("error", e.to_wire())]),
        };
        assert_eq!(
            actual,
            *row.get("expected").unwrap(),
            "{}",
            row.get("label").unwrap().as_str().unwrap()
        );
        assert_eq!(
            IvyJson::Array(c.trace.lock().unwrap().clone()),
            *row.get("trace").unwrap(),
            "trace {}",
            row.get("label").unwrap().as_str().unwrap()
        );
    }
    // An immutable candidate keeps successful identities, but a failed load must be retried.
    let input = rows.as_array().unwrap()[0].get("input").unwrap();
    let root = PathBuf::from(input.get("root").unwrap().as_str().unwrap());
    std::fs::write(
        root.join("dist/specs/native/codex-0.142.3/catalog.json"),
        b"{}",
    )
    .unwrap();
    compatibility::verify_native_catalog_compatibility(
        client(input.clone()),
        &root,
        &"fixture".into(),
    )
    .await
    .unwrap();
    let configured = IvyJson::object([
        ("nativeVersion", "0.142.3".into()),
        (
            "nativeExecutableHash",
            input
                .get("status")
                .unwrap()
                .get("nativeExecutableHash")
                .unwrap()
                .clone(),
        ),
    ]);
    assert!(
        compatibility::verify_configured_native_catalog(&root, &"fixture".into(), &configured)
            .await
            .is_err()
    );
    let empty = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r.get("label") == Some(&"empty".into()))
        .unwrap()
        .get("input")
        .unwrap();
    let empty_root = PathBuf::from(empty.get("root").unwrap().as_str().unwrap());
    let catalog = input.get("catalogs").unwrap().as_array().unwrap()[0]
        .get("value")
        .unwrap();
    let directory = empty_root.join("dist/specs/native/codex-0.142.3");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join("catalog.json"),
        stringify(catalog, usize::MAX).unwrap(),
    )
    .unwrap();
    let mut repaired = empty.clone();
    repaired.insert("status", input.get("status").unwrap().clone());
    compatibility::verify_native_catalog_compatibility(
        client(repaired),
        &empty_root,
        &"fixture".into(),
    )
    .await
    .unwrap();
}
