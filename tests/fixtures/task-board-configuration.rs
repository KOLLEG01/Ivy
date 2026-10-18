use super::{Memory, Stored};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    configuration::{
        check_configuration, configuration_view, effective_execution, execution_with_defaults,
        initial_configuration, supports_execution_options,
    },
    journal_store::{Contracts, JournalStore},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    operations::Document,
};
use std::{
    collections::HashMap,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn text(value: &Json, key: &str) -> JsString {
    value.required(key).unwrap().as_js_string().unwrap().clone()
}
fn flag(value: &Json, key: &str) -> bool {
    value.get(key).and_then(Json::as_bool) == Some(true)
}
fn failure(value: &Json) -> IvyError {
    IvyError::new_js(text(value, "code"), "Fixture discovery RPC failed.".into()).with_outcome(
        if field(value, "outcome") == &Json::from("unknown") {
            Outcome::Unknown
        } else {
            Outcome::NotExecuted
        },
    )
}
fn tier_schema() -> Json {
    Json::object([
        ("type", "object".into()),
        (
            "properties",
            Json::object([(
                "serviceTier",
                Json::object([(
                    "anyOf",
                    Json::Array(vec![
                        Json::object([("type", "string".into())]),
                        Json::object([("type", "null".into())]),
                    ]),
                )]),
            )]),
        ),
    ])
}
#[derive(Default)]
struct Calls {
    node_page: usize,
    models: HashMap<JsString, usize>,
    issued: HashMap<JsString, JsString>,
    refreshed: bool,
}
struct Client {
    input: Json,
    trace: Arc<Mutex<Vec<Json>>>,
    calls: Mutex<Calls>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut logged = params.clone();
            if method == "tools.call" {
                let id = text(&params, "operationId");
                let ascii = id.to_utf8().unwrap();
                let pieces = ascii.split(':').collect::<Vec<_>>();
                assert_eq!(pieces.len(), 3);
                assert_eq!(pieces[0], "11111111-1111-4111-8111-111111111111");
                assert!(pieces[1].parse::<u64>().unwrap() > 0);
                assert_eq!(
                    uuid::Uuid::parse_str(pieces[2]).unwrap().get_version_num(),
                    4
                );
                let mut calls = self.calls.lock().unwrap();
                let next = calls.issued.len() + 1;
                let alias = calls
                    .issued
                    .entry(id)
                    .or_insert_with(|| format!("model-operation-{next}").into())
                    .clone();
                logged.insert("operationId", Json::String(alias));
            }
            self.trace
                .lock()
                .unwrap()
                .push(Json::Array(vec!["rpc".into(), method.into(), logged]));
            if let Some(error) = self.input.get("rpcFailure") {
                if field(error, "method") == &Json::from(method)
                    && (error.get("name").is_none()
                        || error.get("name") == params.get("namePrefix"))
                {
                    return Err(failure(error));
                }
            }
            match method {
                "serviceNodes.list" => {
                    let mut calls = self.calls.lock().unwrap();
                    let value = self.input.required("nodePages")?.as_array().unwrap()
                        [calls.node_page]
                        .clone();
                    calls.node_page += 1;
                    Ok(value)
                }
                "system.status" => Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )])),
                "tools.list" => {
                    let name = text(&params, "namePrefix");
                    if self.input.get("delayBinding").and_then(Json::as_js_string) == Some(&name) {
                        for _ in 0..6 {
                            tokio::task::yield_now().await;
                        }
                    }
                    if let Some(code) = self
                        .input
                        .get("bindingFailures")
                        .and_then(|values| values.get_key(&name))
                    {
                        return Err(IvyError::new_js(
                            code.as_js_string().unwrap().clone(),
                            "Fixture discovery RPC failed.".into(),
                        ));
                    }
                    let provider = Json::object([(
                        "node",
                        Json::object([(
                            "serviceNodeId",
                            self.input
                                .get("provider")
                                .unwrap_or(params.required("serviceNodeId")?)
                                .clone(),
                        )]),
                    )]);
                    if self
                        .input
                        .get("missingBinding")
                        .and_then(Json::as_js_string)
                        == Some(&name)
                        || (flag(&self.input, "pagedBinding") && params.get("cursor").is_none())
                    {
                        return Ok(Json::object([
                            ("provider", provider),
                            ("items", Json::Array(vec![])),
                            (
                                "nextCursor",
                                if flag(&self.input, "pagedBinding") {
                                    "catalog-more".into()
                                } else {
                                    Json::Null
                                },
                            ),
                        ]));
                    }
                    let mut qualified = text(&params, "namespace");
                    qualified.0.push(46);
                    qualified.0.extend(&name.0);
                    let schema = self
                        .input
                        .get("tierSchemas")
                        .and_then(|values| values.get_key(&name))
                        .cloned()
                        .unwrap_or_else(tier_schema);
                    let definition = Json::object([
                        ("namespace", params.required("namespace")?.clone()),
                        ("name", Json::String(name)),
                        (
                            "interfaceVersion",
                            self.input
                                .get("interfaceVersion")
                                .cloned()
                                .unwrap_or_else(|| {
                                    let id = params.required("serviceNodeId").unwrap();
                                    let advertised = self
                                        .input
                                        .get("node")
                                        .filter(|node| node.get("serviceNodeId") == Some(id))
                                        .or_else(|| {
                                            self.input
                                                .get("nodePages")
                                                .and_then(Json::as_array)
                                                .unwrap_or(&[])
                                                .iter()
                                                .flat_map(|page| {
                                                    page.required("items")
                                                        .unwrap()
                                                        .as_array()
                                                        .unwrap()
                                                })
                                                .find(|node| node.get("serviceNodeId") == Some(id))
                                        })
                                        .expect(
                                            "Fixture provider must advertise its native version",
                                        );
                                    advertised.required("nativeVersion").unwrap().clone()
                                }),
                        ),
                        ("annotations", Json::object([("readOnlyHint", true.into())])),
                        ("inputSchema", schema),
                        ("outputSchema", Json::object(Vec::<(String, Json)>::new())),
                    ]);
                    let letter = if self.calls.lock().unwrap().refreshed {
                        'c'
                    } else {
                        'a'
                    };
                    Ok(Json::object([
                        ("provider", provider),
                        (
                            "items",
                            Json::Array(vec![Json::object([
                                ("qualifiedName", Json::String(qualified)),
                                ("definition", definition),
                                (
                                    "definitionHash",
                                    format!("sha256:{}", letter.to_string().repeat(64)).into(),
                                ),
                                ("catalogHash", format!("sha256:{}", "b".repeat(64)).into()),
                            ])]),
                        ),
                        ("nextCursor", Json::Null),
                    ]))
                }
                "tools.call" => {
                    let mut calls = self.calls.lock().unwrap();
                    if flag(&self.input, "refresh") && !calls.refreshed {
                        calls.refreshed = true;
                        return Err(IvyError::new(
                            "tool_definition_changed",
                            "Fixture changed read definition.",
                        ));
                    }
                    let node = text(&params, "serviceNodeId");
                    let index = calls.models.entry(node.clone()).or_default();
                    let page = self
                        .input
                        .required("models")?
                        .get_key(&node)
                        .unwrap()
                        .as_array()
                        .unwrap()[*index]
                        .clone();
                    *index += 1;
                    if let Some(error) = page.get("error") {
                        Err(failure(error))
                    } else {
                        Ok(page)
                    }
                }
                _ => panic!("Unexpected discovery RPC {method}"),
            }
        })
    }
}
// Exact captured-value admission only; not a production TaskBoard validator.
struct Policy {
    value_hash: String,
    reject: bool,
    trace: Arc<Mutex<Vec<Json>>>,
}
impl Contracts for Policy {
    fn version(&self, _: &str) -> IvyResult<String> {
        panic!("Discovery must not request a write version")
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        panic!("Discovery must not route a write")
    }
    fn readable_version(&self, _: &str, _: &JsString) -> IvyResult<bool> {
        panic!("Discovery must not read a record")
    }
    fn validate_record(&self, _: &str, _: &JsString, _: &Json) -> IvyResult<()> {
        panic!("Discovery must not read a record")
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        self.trace.lock().unwrap().push(Json::Array(vec![
            "validate".into(),
            name.into(),
            value.clone(),
        ]));
        if name != "Configuration" || self.reject || hash_json(value)? != self.value_hash {
            Err(IvyError::new(
                "fixture_admission",
                "Fixture configuration was not admitted.",
            ))
        } else {
            Ok(())
        }
    }
}
fn outcome(result: IvyResult<Json>) -> Json {
    match result {
        Ok(value) => value,
        Err(error) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(error.code_js())),
                ("message", Json::String(error.message_js())),
                ("outcome", error.outcome.as_str().into()),
            ]),
        )]),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn configuration_and_discovery_match_original_defaults_guards_and_rpc_traces() {
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-configuration-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for row in rows.as_array().unwrap() {
        let input = row.required("input").unwrap();
        let method = row.required("method").unwrap().as_str().unwrap();
        let store = Memory::default();
        for r in input.get("records").and_then(Json::as_array).unwrap_or(&[]) {
            let value = r.required("value").unwrap();
            store.documents.borrow_mut().push(Stored {
                key: r.required("key").unwrap().as_str().unwrap(),
                name: text(r, "name"),
                document: Document {
                    pin: Json::object([
                        ("objectId", r.required("id").unwrap().clone()),
                        ("revision", Json::Number(1.0)),
                    ]),
                    value: value.clone(),
                    parent_id: Some(text(r, "parentId")),
                    content_hash: hash_json(value).unwrap(),
                },
            });
        }
        let trace = Arc::new(Mutex::new(vec![]));
        let client = Arc::new(Client {
            input: input.clone(),
            trace: trace.clone(),
            calls: Mutex::new(Calls::default()),
        });
        let result = match method.as_str() {
            "initial" => Ok(initial_configuration()),
            "view" => configuration_view(&store).await,
            "merge" => Ok(execution_with_defaults(
                input.required("fields").unwrap(),
                input.required("defaults").unwrap(),
            )),
            "effective" => effective_execution(&store, input.required("task").unwrap()).await,
            "supports" => supports_execution_options(
                client.clone(),
                input.required("node").unwrap(),
                input.required("options").unwrap(),
            )
            .await
            .map(Json::Bool),
            "check" => {
                let database = Connection::open_in_memory().unwrap();
                local_journal::initialize(&database).unwrap();
                let before = database.total_changes();
                let value = input.required("value").unwrap();
                let journal = JournalStore {
                    journal: LocalJournal {
                        connection: &database,
                        root_object_id: Some("root".into()),
                    },
                    client: client.clone(),
                    contracts: Policy {
                        value_hash: hash_json(value).unwrap(),
                        reject: flag(input, "reject"),
                        trace: trace.clone(),
                    },
                    limits: HistoryLimits {
                        count: 1000,
                        bytes: 32 * 1024 * 1024,
                    },
                    now: || "2026-01-02T03:04:05.000Z".into(),
                    new_id: || JsString::from("unused"),
                    cache: None,
                };
                let result = check_configuration(
                    &journal,
                    value,
                    input.get("serviceNodeId").and_then(Json::as_js_string),
                    flag(input, "allowUnavailable"),
                )
                .await;
                assert_eq!(
                    database.total_changes(),
                    before,
                    "Configuration discovery changed SQLite"
                );
                result.map(|()| Json::Null)
            }
            _ => panic!("Unknown configuration case {method}"),
        };
        for _ in 0..12 {
            tokio::task::yield_now().await;
        }
        let mut actual_trace = store.trace.borrow().clone();
        actual_trace.extend(trace.lock().unwrap().clone());
        let actual = Json::object([
            ("value", outcome(result)),
            ("trace", Json::Array(actual_trace)),
            (
                "issued",
                Json::Number(client.calls.lock().unwrap().issued.len() as f64),
            ),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(row.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            row.required("name").unwrap().as_str().unwrap()
        );
    }
}
