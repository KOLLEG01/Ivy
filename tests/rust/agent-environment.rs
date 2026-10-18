use ivy_agent_manager::environment::{
    EnvironmentKind, environment_name, read_environment_document, select_environment_document,
};
use ivy_contracts::{IvyError, IvyJson, Outcome, json::parse};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    path::PathBuf,
    process::Command,
    sync::{Mutex, OnceLock},
};

fn oracle() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let output = Command::new("node")
            .arg("tests/fixtures/agent-environment-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(
            std::str::from_utf8(&output.stdout).unwrap(),
            2 * 1024 * 1024,
        )
        .unwrap()
    })
}
fn kind(row: &IvyJson) -> EnvironmentKind {
    if row.get("kind") == Some(&IvyJson::from("mcp")) {
        EnvironmentKind::Mcp
    } else {
        EnvironmentKind::Skills
    }
}
fn option(value: &IvyJson) -> Option<&IvyJson> {
    (!value.is_null()).then_some(value)
}

#[test]
fn environment_names_inheritance_and_revision_hashes_match_original() {
    let names = oracle().get("names").unwrap().as_array().unwrap();
    assert_eq!(names.len(), 22);
    for row in names {
        assert_eq!(
            IvyJson::from(environment_name(
                kind(row),
                row.get("hostId").unwrap().as_js_string()
            )),
            *row.get("expected").unwrap()
        );
    }
    let cases = oracle().get("selections").unwrap().as_array().unwrap();
    assert_eq!(cases.len(), 68);
    for row in cases {
        let selected = select_environment_document(
            row.get("defaults").unwrap(),
            option(row.get("global").unwrap()),
            option(row.get("host").unwrap()),
        )
        .unwrap();
        assert_eq!(
            IvyJson::object([
                ("document", selected.document),
                ("version", selected.version.into())
            ]),
            *row.get("expected").unwrap()
        );
    }
}

struct Client {
    row: IvyJson,
    calls: Mutex<Vec<IvyJson>>,
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
            let (result, error) = match method {
                "objects.stat" => ("stat", "statError"),
                "objects.read" => ("value", "readError"),
                _ => panic!("Unexpected method {method}"),
            };
            if let Some(e) = self.row.get(error).and_then(option) {
                let code = e
                    .get("code")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap();
                let outcome = if e.get("outcome") == Some(&IvyJson::from("unknown")) {
                    Outcome::Unknown
                } else {
                    Outcome::NotExecuted
                };
                return Err(IvyError::new(&code, "fixture failure").with_outcome(outcome));
            }
            Ok(self.row.get(result).unwrap().clone())
        })
    }
}

#[tokio::test]
async fn environment_reads_pin_revisions_validate_scope_and_preserve_failures() {
    let rows = oracle().get("reads").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 52);
    for row in rows {
        let client = Client {
            row: row.clone(),
            calls: Mutex::new(Vec::new()),
        };
        let actual = match read_environment_document(
            &client,
            kind(row),
            row.get("hostId").unwrap().as_js_string(),
        )
        .await
        {
            Ok(value) => IvyJson::object([("result", value.unwrap_or(IvyJson::Null))]),
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        assert_eq!(
            &actual,
            row.get("expected").unwrap(),
            "{:?}",
            row.get("mode")
        );
        assert_eq!(
            IvyJson::Array(client.calls.into_inner().unwrap()),
            *row.get("calls").unwrap()
        );
    }
}
