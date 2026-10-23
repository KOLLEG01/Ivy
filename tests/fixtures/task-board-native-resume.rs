use super::*;
use ivy_sdk::tools::ToolClients;
use ivy_task_board::native_driver::NativeDriverConnection;

struct NativeClient {
    input: Json,
    base: Json,
    log: Arc<Mutex<Vec<Json>>>,
}
impl RpcClient for NativeClient {
    fn request<'a>(&'a self, method: &'a str, args: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            event(&self.log, ["rpc".into(), method.into(), args.clone()]);
            if method == "tools.list" {
                let name = args.required("namePrefix")?.as_str()?;
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
                            ("qualifiedName", format!("agent.{name}").into()),
                            (
                                "definition",
                                Json::object([
                                    ("namespace", "agent".into()),
                                    ("name", name.into()),
                                    ("interfaceVersion", "1.0.0".into()),
                                    ("annotations", Json::object([("readOnlyHint", true.into())])),
                                    ("inputSchema", Json::Object(vec![])),
                                    ("outputSchema", Json::Object(vec![])),
                                ]),
                            ),
                            (
                                "definitionHash",
                                format!("sha256:{}", "a".repeat(64)).into(),
                            ),
                            ("catalogHash", format!("sha256:{}", "b".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", Json::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            let name = args.required("qualifiedName")?.as_str()?;
            let fault_key = if name == "agent.status" {
                "status"
            } else {
                "nativeRead"
            };
            if let Some(code) = self.input.get("fault").and_then(|v| v.get(fault_key)) {
                return Err(failure(code.as_js_string().unwrap().clone()));
            }
            let status = self.base.required("status")?;
            if name == "agent.status" {
                return Ok(merge(status, self.input.get("status")));
            }
            assert_eq!(name, "agent.read");
            let arguments = args.required("arguments")?;
            let request = Json::object([
                ("method", arguments.required("method")?.clone()),
                ("params", arguments.required("params")?.clone()),
            ]);
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
                    ("epoch", "new-epoch".into()),
                    ("requestId", Json::Number(1.0)),
                    ("observedAt", TIME.into()),
                    ("method", arguments.required("method")?.clone()),
                    ("params", arguments.required("params")?.clone()),
                    ("requestHash", hash_json(&request)?.into()),
                    (
                        "reply",
                        Json::object([(
                            "result",
                            Json::object([(
                                "thread",
                                Json::object([
                                    ("id", "thread".into()),
                                    ("ephemeral", false.into()),
                                ]),
                            )]),
                        )]),
                    ),
                ]),
                self.input.get("read"),
            ))
        })
    }
}
struct Connected<'a> {
    state: &'a State,
    tools: Arc<ToolClients>,
    principal: JsString,
    owners: Cell<u64>,
    fail_owner: Option<f64>,
}
impl NativeDriverRuntime for Connected<'_> {
    async fn verify_owner(&self) -> IvyResult<()> {
        self.state.verify_owner().await?;
        let n = self.owners.get() + 1;
        self.owners.set(n);
        if self.fail_owner == Some(n as f64) {
            return Err(failure("fixture_failed".into()));
        }
        Ok(())
    }
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString> {
        self.state.scoped_operation_id(scope).await
    }
    async fn native_update(&self, request: &Json) -> IvyResult<Json> {
        self.state.native_update(request).await
    }
    fn now(&self) -> JsString {
        self.state.now()
    }
}
impl NativeDriverConnection for Connected<'_> {
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
        panic!("Recovery unexpectedly generated user input")
    }
    fn task_update_prompt(&self, _: &Json, _: &JsString, _: bool) -> IvyResult<JsString> {
        panic!("Recovery unexpectedly generated update input")
    }
}
fn flag(v: &Json, k: &str) -> bool {
    v.get(k) == Some(&Json::Bool(true))
}
#[tokio::test(flavor = "current_thread")]
async fn uncertain_resume_matches_original_with_real_owner_transport_and_journal() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-native-resume-oracle.mjs",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let groups = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let catalogs = CheckedNativeContracts::new(root);
    let mut count = 0;
    for group in groups.as_array().unwrap() {
        let base = group.required("base").unwrap();
        for row in group.required("rows").unwrap().as_array().unwrap() {
            count += 1;
            let input = row.required("input").unwrap();
            let db = Connection::open_in_memory().unwrap();
            local_journal::initialize(&db).unwrap();
            let allowed = Rc::new(RefCell::new(HashSet::new()));
            let journal = LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            };
            let mut run = merge(base.required("run").unwrap(), input.get("run"));
            let task = merge(base.required("task").unwrap(), input.get("task"));
            let call = merge(base.required("call").unwrap(), input.get("call"));
            let turn = merge(base.required("turn").unwrap(), input.get("turn"));
            if flag(input, "attachedTurn") {
                let mut calls = run.required("calls").unwrap().clone();
                calls.insert("turn", pin(301, 1.0));
                run.insert("calls", calls);
            }
            seed(&journal, &allowed, "task-board/run", 200, 1.0, &run);
            seed(&journal, &allowed, "task-board/run", 200, 2.0, &run);
            // The current-only Task read must not silently choose an old revision.
            seed(&journal, &allowed, "task-board/task", 100, 1.0, &task);
            seed(&journal, &allowed, "task-board/task", 100, 2.0, &task);
            if !flag(input, "missingCall") {
                seed(
                    &journal,
                    &allowed,
                    "task-board/native-call",
                    300,
                    1.0,
                    &call,
                );
            }
            if flag(input, "callNext") {
                let next = merge(
                    &call,
                    Some(&Json::object([("ownerPhase", "failed".into())])),
                );
                seed(
                    &journal,
                    &allowed,
                    "task-board/native-call",
                    300,
                    2.0,
                    &next,
                );
            }
            if flag(input, "attachedTurn") && !flag(input, "missingTurn") {
                seed(
                    &journal,
                    &allowed,
                    "task-board/native-call",
                    301,
                    1.0,
                    &turn,
                );
            }
            if flag(input, "turnNext") {
                seed(
                    &journal,
                    &allowed,
                    "task-board/native-call",
                    301,
                    2.0,
                    &turn,
                );
            }
            let state = State {
                log: Arc::default(),
                fault: RefCell::new(input.get("fault").cloned().unwrap_or(Json::Null)),
                scopes: RefCell::default(),
                identities: Cell::new(0),
                named: Cell::new(0),
                operation: None,
            };
            let client = Arc::new(NativeClient {
                input: input.clone(),
                base: base.clone(),
                log: state.log.clone(),
            });
            let runtime = Connected {
                state: &state,
                tools: Arc::new(ToolClients::new(client.clone())),
                principal: "workflow-owner".into(),
                owners: Cell::new(0),
                fail_owner: input.get("failOwner").and_then(Json::as_f64),
            };
            let next = Cell::new(400);
            let store = JournalStore {
                journal,
                client,
                contracts: Policy { allowed },
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
            let driver = NativeDriver {
                store: &trace,
                runtime: &runtime,
                prompts: &NoPrompts,
                catalogs: &catalogs,
            };
            let mut outcomes = vec![];
            for _ in 0..input.get("passes").and_then(Json::as_f64).unwrap_or(1.0) as usize {
                let result = if input.get("action") == Some(&Json::from("status")) {
                    driver.status(&run, &call).await
                } else {
                    driver
                        .recover_uncertain_resume(&id(200))
                        .await
                        .map(Json::Bool)
                };
                outcomes.push(match result {
                    Ok(v) => Json::object([("ok", v)]),
                    Err(e) => Json::object([(
                        "error",
                        Json::object([
                            ("code", Json::String(e.code_js())),
                            ("message", Json::String(e.message_js())),
                            ("outcome", e.outcome.to_json()),
                        ]),
                    )]),
                });
            }
            let generated = calls(&db)
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| {
                    ![id(300), id(301)].contains(&s(v.required("pin").unwrap(), "objectId"))
                })
                .cloned()
                .collect();
            let actual = Json::object([
                ("outcomes", Json::Array(outcomes)),
                ("trace", Json::Array(state.log.lock().unwrap().clone())),
                ("calls", Json::Array(generated)),
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
    assert_eq!(count, 256);
}
