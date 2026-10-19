use super::*;
use ivy_sdk::tools::ToolClients;
use ivy_task_board::{
    native_driver::NativeDriverConnection, native_evidence::NativeEvidence,
    native_intent::NativeIntent,
};
use std::sync::OnceLock;

fn data() -> &'static Json {
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let output = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-native-advance-oracle.mjs",
            ])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(
            std::str::from_utf8(&output.stdout).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
fn flag(v: &Json, k: &str) -> bool {
    v.get(k) == Some(&Json::Bool(true))
}
fn operation(base: &Json, slot: &str, phase: &str, at: &str) -> Json {
    let call = base.required("calls").unwrap().required(slot).unwrap();
    let request = call.required("request").unwrap();
    let params = request
        .required("params")
        .unwrap()
        .required("params")
        .unwrap()
        .clone();
    let status = base.required("status").unwrap();
    let pending = matches!(phase, "accepted" | "prevented");
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("operationId", "original".into()),
        ("callerPrincipalId", "workflow-owner".into()),
        ("serviceNodeId", "native-agent".into()),
        (
            "nativeVersion",
            status.required("nativeVersion").unwrap().clone(),
        ),
        (
            "nativeExecutableHash",
            status.required("nativeExecutableHash").unwrap().clone(),
        ),
        ("method", request.required("method").unwrap().clone()),
        ("params", params.clone()),
        (
            "requestHash",
            hash_json(&Json::object([
                ("method", request.required("method").unwrap().clone()),
                ("params", params),
            ]))
            .unwrap()
            .into(),
        ),
        (
            "phase",
            if phase == "prevented" {
                "failed".into()
            } else {
                phase.into()
            },
        ),
        ("createdAt", TIME.into()),
        ("updatedAt", at.into()),
        ("epoch", if pending { Json::Null } else { "epoch".into() }),
        (
            "requestId",
            if pending {
                Json::Null
            } else {
                Json::Number(42.0)
            },
        ),
        (
            "reply",
            match phase {
                "succeeded" => Json::object([("result", Json::Object(vec![]))]),
                "failed" => Json::object([(
                    "error",
                    Json::object([
                        ("code", Json::Number(-32000.0)),
                        ("message", "native failed".into()),
                    ]),
                )]),
                _ => Json::Null,
            },
        ),
        (
            "code",
            match phase {
                "failed" => "native_failed".into(),
                "outcome_unknown" => "outcome_unknown".into(),
                "prevented" => "request_prevented".into(),
                _ => Json::Null,
            },
        ),
    ])
}
struct Client {
    input: Json,
    base: Json,
    slot: String,
    definitions: HashMap<JsString, Json>,
    log: Arc<Mutex<Vec<Json>>>,
    lists: Mutex<HashMap<JsString, u64>>,
    operations: Mutex<usize>,
    invokes: Mutex<u64>,
    invoke_error: Mutex<Option<JsString>>,
}
impl Client {
    fn new(
        input: &Json,
        base: &Json,
        log: Arc<Mutex<Vec<Json>>>,
        catalogs: &CheckedNativeContracts,
    ) -> Self {
        let status = base.required("status").unwrap();
        let source = catalogs
            .checked_native_contract(
                &s(status, "nativeVersion"),
                Some(&Json::object([(
                    "catalogHash",
                    status.required("catalogHash").unwrap().clone(),
                )])),
            )
            .unwrap();
        let definitions = [
            "thread/resume",
            "turn/start",
            "turn/steer",
            "turn/interrupt",
        ]
        .into_iter()
        .map(|m| (m.into(), source.definition(&m.into()).unwrap().clone()))
        .collect();
        Self {
            input: input.clone(),
            base: base.clone(),
            slot: s(input, "slot").to_utf8().unwrap(),
            definitions,
            log,
            lists: Mutex::default(),
            operations: Mutex::new(0),
            invokes: Mutex::new(0),
            invoke_error: Mutex::new(
                input
                    .get("fault")
                    .and_then(|f| f.get("invoke"))
                    .and_then(Json::as_js_string)
                    .cloned(),
            ),
        }
    }
}
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, args: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            event(&self.log, ["rpc".into(), method.into(), args.clone()]);
            if method == "serviceNodes.get" {
                return Ok(Json::object([
                    ("principalId", "workflow-owner".into()),
                    ("serviceName", "task-board".into()),
                    ("connected", true.into()),
                    ("synced", true.into()),
                ]));
            }
            if method == "tools.list" {
                let namespace = args.required("namespace")?.as_str()?;
                let prefix = s(&args, "namePrefix");
                let name = format!("{namespace}.{}", prefix.to_utf8()?);
                let count = {
                    let mut lists = self.lists.lock().unwrap();
                    let count = lists.entry(name.clone().into()).or_default();
                    let old = *count;
                    *count += 1;
                    old
                };
                let definition = if namespace == "codex" {
                    self.definitions.get(&prefix).unwrap().clone()
                } else {
                    Json::object([
                        ("namespace", "agent".into()),
                        ("name", Json::String(prefix)),
                        ("interfaceVersion", "1.0.0".into()),
                        ("annotations", Json::object([("readOnlyHint", true.into())])),
                        ("inputSchema", Json::Object(vec![])),
                        ("outputSchema", Json::Object(vec![])),
                    ])
                };
                let hash = if namespace == "codex" {
                    if flag(&self.input, "nativeDefinitionChanged") {
                        format!("sha256:{}", "0".repeat(64))
                    } else {
                        hash_json(&definition)?
                    }
                } else {
                    format!(
                        "sha256:{}",
                        if flag(&self.input, "retry") && name == "agent.invoke" && count > 0 {
                            "b"
                        } else {
                            "a"
                        }
                        .repeat(64)
                    )
                };
                return Ok(Json::object([
                    (
                        "provider",
                        Json::object([(
                            "node",
                            Json::object([("serviceNodeId", "native-agent".into())]),
                        )]),
                    ),
                    (
                        "items",
                        Json::Array(vec![Json::object([
                            ("qualifiedName", name.into()),
                            ("definition", definition),
                            ("definitionHash", hash.into()),
                            ("catalogHash", format!("sha256:{}", "0".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", Json::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            let name = args.required("qualifiedName")?.as_str()?;
            let status = self.base.required("status")?;
            if name == "agent.operation" {
                if let Some(code) = self.input.get("fault").and_then(|v| v.get("operation")) {
                    return Err(failure(code.as_js_string().unwrap().clone()));
                }
                let default = [Json::from("absent")];
                let phases = self
                    .input
                    .get("operations")
                    .and_then(Json::as_array)
                    .unwrap_or(&default);
                let phase = {
                    let mut n = self.operations.lock().unwrap();
                    let v = phases[(*n).min(phases.len() - 1)].clone();
                    *n += 1;
                    v
                };
                if phase == Json::from("absent") {
                    return Err(IvyError::new("not_found", "Original operation is absent.")
                        .with_details(merge(
                            &Json::object([
                                ("kind", "agent_operation_absent".into()),
                                ("operationId", "original".into()),
                                ("serviceNodeId", "native-agent".into()),
                                ("epoch", "epoch".into()),
                            ]),
                            self.input.get("absence"),
                        )));
                }
                let at = self
                    .input
                    .get("updatedAt")
                    .map(|s| s.as_str().unwrap())
                    .unwrap_or_else(|| "2026-01-02T03:04:05.007Z".to_owned());
                return Ok(merge(
                    &operation(&self.base, &self.slot, &phase.as_str()?, &at),
                    self.input.get("observation"),
                ));
            }
            if name == "agent.status" {
                return Ok(merge(status, self.input.get("status")));
            }
            if name == "agent.prevent" {
                return Ok(operation(
                    &self.base,
                    &self.slot,
                    "prevented",
                    "2026-01-02T03:04:05.007Z",
                ));
            }
            if name == "agent.invoke" {
                let n = {
                    let mut n = self.invokes.lock().unwrap();
                    *n += 1;
                    *n
                };
                if flag(&self.input, "retry") && n == 1 {
                    return Err(IvyError::new("tool_definition_changed", "Changed."));
                }
                if let Some(code) = self.invoke_error.lock().unwrap().take() {
                    return Err(failure(code));
                }
                return Ok(merge(
                    &operation(
                        &self.base,
                        &self.slot,
                        "accepted",
                        "2026-01-02T03:04:05.007Z",
                    ),
                    self.input.get("observation"),
                ));
            }
            assert_eq!(name, "agent.read");
            let p = args.required("arguments")?.required("params")?.clone();
            let m = args.required("arguments")?.required("method")?.clone();
            Ok(merge(
                &Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("observationId", "read".into()),
                    ("callerPrincipalId", "workflow-owner".into()),
                    ("serviceNodeId", "native-agent".into()),
                    ("nativeVersion", status.required("nativeVersion")?.clone()),
                    (
                        "nativeExecutableHash",
                        status.required("nativeExecutableHash")?.clone(),
                    ),
                    ("catalogHash", status.required("catalogHash")?.clone()),
                    ("epoch", "epoch".into()),
                    ("requestId", Json::Number(1.0)),
                    ("observedAt", TIME.into()),
                    ("method", m.clone()),
                    ("params", p.clone()),
                    (
                        "requestHash",
                        hash_json(&Json::object([("method", m), ("params", p)]))?.into(),
                    ),
                    (
                        "reply",
                        Json::object([(
                            "result",
                            Json::object([(
                                "thread",
                                merge(
                                    &Json::object([
                                        ("id", "thread".into()),
                                        ("ephemeral", false.into()),
                                        ("status", Json::object([("type", "idle".into())])),
                                        ("canAcceptDirectInput", true.into()),
                                    ]),
                                    self.input.get("threadRead"),
                                ),
                            )]),
                        )]),
                    ),
                ]),
                self.input.get("read"),
            ))
        })
    }
}
struct Runtime<'a, F> {
    state: &'a State,
    tools: Arc<ToolClients>,
    principal: JsString,
    owners: Cell<u64>,
    input: &'a Json,
    changed: F,
}
impl<F: Fn(u64) -> IvyResult<()>> NativeDriverRuntime for Runtime<'_, F> {
    async fn verify_owner(&self) -> IvyResult<()> {
        self.state.verify_owner().await?;
        let n = self.owners.get() + 1;
        self.owners.set(n);
        if self.input.get("failOwner").and_then(Json::as_f64) == Some(n as f64) {
            return Err(failure("fixture_failed".into()));
        }
        (self.changed)(n)
    }
    async fn scoped_operation_id(&self, _: &Json) -> IvyResult<JsString> {
        panic!("Advance regenerated an operation identity")
    }
    async fn native_update(&self, v: &Json) -> IvyResult<Json> {
        self.state.native_update(v).await
    }
    fn now(&self) -> JsString {
        panic!("Advance invented an observation time")
    }
}
impl<F: Fn(u64) -> IvyResult<()>> NativeDriverConnection for Runtime<'_, F> {
    fn tools(&self) -> &Arc<ToolClients> {
        &self.tools
    }
    fn principal_id(&self) -> &JsString {
        &self.principal
    }
}
struct NoPrompts;
impl TaskPrompts for NoPrompts {
    fn task_prompt(&self, _: &Json, _: &JsString, _: Option<&Json>) -> IvyResult<JsString> {
        panic!("Advance generated input")
    }
    fn task_update_prompt(&self, _: &Json, _: &JsString, _: bool) -> IvyResult<JsString> {
        panic!("Advance generated input")
    }
}
fn documents(db: &Connection) -> Json {
    let mut stmt=db.prepare("SELECT d.key,d.id,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key IN ('task-board/native-call','task-board/native-evidence') ORDER BY d.key,d.id").unwrap();
    Json::Array(
        stmt.query_map([], |r| {
            Ok(Json::object([
                ("key", Json::from(r.get::<_, String>(0)?)),
                (
                    "pin",
                    Json::object([
                        ("objectId", Json::from(r.get::<_, String>(1)?)),
                        ("revision", Json::Number(r.get::<_, f64>(4)?)),
                    ]),
                ),
                (
                    "value",
                    parse(&r.get::<_, String>(5)?, MANAGEMENT_FRAME_BYTES).unwrap(),
                ),
                ("name", Json::from(r.get::<_, String>(2)?)),
                ("parent", Json::from(r.get::<_, String>(3)?)),
            ]))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap(),
    )
}
fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|n| u8::from_str_radix(&s[n..n + 2], 16).unwrap())
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn reconciliation_and_local_dispatch_match_original_with_actual_journal_cas() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut count = 0;
    for group in data().as_array().unwrap() {
        let base = group.required("base").unwrap();
        for row in group.required("rows").unwrap().as_array().unwrap() {
            count += 1;
            let input = row.required("input").unwrap();
            let selected = s(input, "slot").to_utf8().unwrap();
            let db = Connection::open_in_memory().unwrap();
            local_journal::initialize(&db).unwrap();
            let allowed = Rc::new(RefCell::new(HashSet::new()));
            let state = State {
                log: Arc::default(),
                fault: RefCell::new(input.get("fault").cloned().unwrap_or(Json::Null)),
                scopes: RefCell::default(),
                identities: Cell::new(0),
                named: Cell::new(0),
                operation: None,
            };
            let client = Arc::new(Client::new(input, base, state.log.clone(), &catalogs));
            let next = Cell::new(400);
            let store = JournalStore {
                journal: LocalJournal {
                    connection: &db,
                    root_object_id: Some("root".into()),
                },
                client: client.clone(),
                contracts: Policy {
                    allowed: allowed.clone(),
                },
                limits: HistoryLimits::default(),
                now: || TIME.to_owned(),
                new_id: || {
                    let n = next.get();
                    next.set(n + 1);
                    id(n)
                },
                cache: None,
            };
            let trace = Trace {
                inner: &store,
                state: &state,
            };
            // Control only opaque allocation: pre-existing immutable chunk rows
            // keep the original UUID-independent comparison deterministic. Save
            // and read still use the actual SQLite BlobStore. A separate test
            // below qualifies fresh real allocation, reopen and corruption.
            for blob in row.required("blobs").unwrap().as_array().unwrap() {
                db.execute(
                    "INSERT INTO blobs VALUES(?,?,?,?,?)",
                    ivy_store::rusqlite::params![
                        s(blob.required("pin").unwrap(), "objectId")
                            .to_utf8()
                            .unwrap(),
                        s(blob, "key").to_utf8().unwrap(),
                        s(blob, "name").to_utf8().unwrap(),
                        s(blob, "parent").to_utf8().unwrap(),
                        hex(&blob.required("hex").unwrap().as_str().unwrap())
                    ],
                )
                .unwrap();
            }
            let mut run = base.required("run").unwrap().clone();
            run.insert(
                "turnId",
                if matches!(selected.as_str(), "steer" | "interrupt") {
                    "turn".into()
                } else {
                    Json::Null
                },
            );
            run.insert(
                "cancellation",
                if selected == "interrupt" {
                    Json::object([("reason", "stop".into())])
                } else {
                    Json::Null
                },
            );
            let mut attached = run.required("calls").unwrap().clone();
            attached.insert(selected.as_str(), pin(300, 1.0));
            if selected == "turn" && !flag(input, "missingThread") {
                attached.insert("thread", pin(301, 1.0));
            }
            run.insert("calls", attached);
            run = merge(&run, input.get("run"));
            let task = merge(base.required("task").unwrap(), input.get("task"));
            let call = merge(
                base.required("calls")
                    .unwrap()
                    .required(selected.as_str())
                    .unwrap(),
                input.get("call"),
            );
            seed(
                &store.journal,
                &allowed,
                "task-board/native-call",
                300,
                1.0,
                &call,
            );
            if selected == "turn" && !flag(input, "missingThread") {
                let thread = merge(base.required("threadCall").unwrap(), input.get("thread"));
                seed(
                    &store.journal,
                    &allowed,
                    "task-board/native-call",
                    301,
                    1.0,
                    &thread,
                );
            }
            if let Some(phase) = input.get("previous") {
                let phase = phase.as_str().unwrap();
                let identity = NativeIntent {
                    store: &trace,
                    catalogs: &catalogs,
                }
                .evidence_identity(&run, &call, &"workflow-owner".into())
                .unwrap();
                let evidence = NativeEvidence {
                    store: &trace,
                    catalogs: &catalogs,
                }
                .save(
                    &pin(300, 1.0),
                    &operation(base, &selected, &phase, TIME),
                    &identity,
                )
                .await
                .unwrap();
                let value = merge(
                    &call,
                    Some(&Json::object([
                        (
                            "state",
                            if phase == "outcome_unknown" {
                                "outcome_unknown".into()
                            } else {
                                "observed".into()
                            },
                        ),
                        ("ownerPhase", phase.as_str().into()),
                        ("observedAt", TIME.into()),
                        (
                            "epoch",
                            if phase == "accepted" {
                                Json::Null
                            } else {
                                "epoch".into()
                            },
                        ),
                        ("evidence", evidence),
                        (
                            "code",
                            if phase == "outcome_unknown" {
                                "outcome_unknown".into()
                            } else {
                                Json::Null
                            },
                        ),
                    ])),
                );
                seed(
                    &store.journal,
                    &allowed,
                    "task-board/native-call",
                    300,
                    2.0,
                    &value,
                );
                let mut attached = run.required("calls").unwrap().clone();
                attached.insert(selected.as_str(), pin(300, 2.0));
                run.insert("calls", attached);
            }
            if let Some(patch) = input.get("latest") {
                let value = merge(&call, Some(patch));
                seed(
                    &store.journal,
                    &allowed,
                    "task-board/native-call",
                    300,
                    2.0,
                    &value,
                );
            }
            if flag(input, "corruptPrevious") {
                let blob = &row.required("blobs").unwrap().as_array().unwrap()[0];
                let mut bytes = hex(&blob.required("hex").unwrap().as_str().unwrap());
                bytes[0] ^= 1;
                db.execute(
                    "UPDATE blobs SET bytes=? WHERE id=?",
                    ivy_store::rusqlite::params![
                        bytes,
                        s(blob.required("pin").unwrap(), "objectId")
                            .to_utf8()
                            .unwrap()
                    ],
                )
                .unwrap();
            }
            seed(&store.journal, &allowed, "task-board/run", 200, 1.0, &run);
            seed(&store.journal, &allowed, "task-board/run", 200, 2.0, &run);
            seed(&store.journal, &allowed, "task-board/task", 100, 1.0, &task);
            seed(&store.journal, &allowed, "task-board/task", 100, 2.0, &task);
            let runtime = Runtime {
                state: &state,
                tools: Arc::new(ToolClients::new(client)),
                principal: "workflow-owner".into(),
                owners: Cell::new(0),
                input,
                changed: |n| {
                    for (flag, key, number) in [
                        ("bumpTask", "task-board/task", 100),
                        ("bumpRun", "task-board/run", 200),
                    ] {
                        if input.get(flag).and_then(Json::as_f64) != Some(n as f64) {
                            continue;
                        }
                        let old = store.journal.read(key, &Json::String(id(number)), true)?;
                        let revision = old.pin.required("revision")?.as_f64().unwrap() + 1.0;
                        seed(&store.journal, &allowed, key, number, revision, &old.value);
                        if flag == "bumpRun" {
                            let task = store.journal.read(
                                "task-board/task",
                                &Json::String(id(100)),
                                true,
                            )?;
                            let value = merge(
                                &task.value,
                                Some(&Json::object([(
                                    "claim",
                                    Json::object([("run", pin(number, revision))]),
                                )])),
                            );
                            seed(
                                &store.journal,
                                &allowed,
                                "task-board/task",
                                100,
                                task.pin.required("revision")?.as_f64().unwrap() + 1.0,
                                &value,
                            );
                        }
                        event(
                            &state.log,
                            ["change".into(), key.into(), pin(number, revision)],
                        );
                    }
                    Ok(())
                },
            };
            state.log.lock().unwrap().clear();
            let driver = NativeDriver {
                store: &trace,
                runtime: &runtime,
                prompts: &NoPrompts,
                catalogs: &catalogs,
            };
            let mut outcomes = vec![];
            for _ in 0..input.get("passes").and_then(Json::as_f64).unwrap_or(1.0) as usize {
                outcomes.push(match driver.advance(&id(200), slot(input)).await {
                    Ok(value) => Json::object([("ok", value)]),
                    Err(e) => Json::object([(
                        "error",
                        Json::object([
                            ("code", Json::String(e.code_js())),
                            ("message", Json::String(e.message_js())),
                            ("outcome", e.outcome.as_str().into()),
                        ]),
                    )]),
                });
            }
            let actual = Json::object([
                ("outcomes", Json::Array(outcomes)),
                ("trace", Json::Array(state.log.lock().unwrap().clone())),
                ("documents", documents(&db)),
            ]);
            assert_eq!(
                hash_json(&actual).unwrap(),
                s(row, "expectedHash").to_utf8().unwrap(),
                "{} / {}: {}",
                s(group, "path").to_utf8().unwrap(),
                s(row, "name").to_utf8().unwrap(),
                canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
            );
            assert!(db.is_autocommit());
        }
    }
    assert_eq!(count, 244);
}

#[tokio::test(flavor = "current_thread")]
async fn actual_receipt_blobs_and_call_cas_survive_file_reopen_without_redispatch() {
    let base = data().as_array().unwrap()[0].required("base").unwrap();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let input = Json::object([
        ("slot", "thread".into()),
        ("operations", Json::Array(vec!["accepted".into()])),
    ]);
    let client = Arc::new(Client::new(&input, base, Arc::default(), &catalogs));
    let updates = Arc::new(Mutex::new(Vec::<Json>::new()));
    let runtime = DriverRuntime::new(
        client.clone(),
        "workflow-owner".into(),
        "workflow-service".into(),
        |v: Json| {
            updates.lock().unwrap().push(v);
            std::future::ready(Ok(Json::Null))
        },
        || JsString::from(TIME),
    );
    let temp = Temporary::new();
    let path = temp.0.join("receipt.sqlite");
    let mut run = base.required("run").unwrap().clone();
    let mut attached = run.required("calls").unwrap().clone();
    attached.insert("thread", pin(300, 1.0));
    run.insert("calls", attached);
    let task = base.required("task").unwrap();
    let call = base.required("calls").unwrap().required("thread").unwrap();
    for round in 0..2 {
        let db = Connection::open(&path).unwrap();
        local_journal::initialize(&db).unwrap();
        let allowed = Rc::new(RefCell::new(
            [hash_json(&run).unwrap(), hash_json(task).unwrap()].into(),
        ));
        let next = Cell::new(400);
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Policy {
                allowed: allowed.clone(),
            },
            limits: HistoryLimits::default(),
            now: || TIME.to_owned(),
            new_id: || {
                let n = next.get();
                next.set(n + 1);
                id(n)
            },
            cache: None,
        };
        if round == 0 {
            seed(&store.journal, &allowed, "task-board/run", 200, 1.0, &run);
            seed(&store.journal, &allowed, "task-board/run", 200, 2.0, &run);
            seed(&store.journal, &allowed, "task-board/task", 100, 1.0, task);
            seed(&store.journal, &allowed, "task-board/task", 100, 2.0, task);
            seed(
                &store.journal,
                &allowed,
                "task-board/native-call",
                300,
                1.0,
                call,
            );
        }
        let before = db.total_changes();
        NativeDriver {
            store: &store,
            runtime: &runtime,
            prompts: &NoPrompts,
            catalogs: &catalogs,
        }
        .advance(&id(200), NativeSlot::Thread)
        .await
        .unwrap();
        if round == 1 {
            assert_eq!(db.total_changes(), before);
        }
        let saved = Store::read(
            &store,
            "task-board/native-call",
            &Json::String(id(300)),
            true,
        )
        .await
        .unwrap();
        assert_eq!(saved.pin, pin(300, 2.0));
        assert_eq!(saved.value.get("ownerPhase"), Some(&Json::from("accepted")));
        let expected = NativeIntent {
            store: &store,
            catalogs: &catalogs,
        }
        .evidence_identity(&run, &saved.value, &"workflow-owner".into())
        .unwrap();
        let evidence = NativeEvidence {
            store: &store,
            catalogs: &catalogs,
        };
        assert_eq!(
            evidence
                .read(
                    &saved.pin,
                    saved.value.required("evidence").unwrap(),
                    &expected
                )
                .await
                .unwrap(),
            operation(base, "thread", "accepted", "2026-01-02T03:04:05.007Z")
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM blobs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        if round == 1 {
            let (blob, mut bytes): (String, Vec<u8>) = db
                .query_row("SELECT id,bytes FROM blobs", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            bytes[0] ^= 1;
            db.execute(
                "UPDATE blobs SET bytes=? WHERE id=?",
                ivy_store::rusqlite::params![bytes, blob],
            )
            .unwrap();
            assert_eq!(
                evidence
                    .read(
                        &saved.pin,
                        saved.value.required("evidence").unwrap(),
                        &expected
                    )
                    .await
                    .unwrap_err()
                    .code,
                "task_board_evidence_mismatch"
            );
        }
        assert!(db.is_autocommit());
    }
    assert_eq!(
        *client.operations.lock().unwrap(),
        1,
        "Reopen must publish the saved original Call revision before any new owner lookup"
    );
    assert_eq!(
        *client.invokes.lock().unwrap(),
        0,
        "A retained owner observation is not dispatch authority"
    );
    let updates = updates.lock().unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0], updates[1]);
}
