use super::*;
use ivy_contracts::checked_native_contract::CheckedNativeContracts;
use ivy_sdk::tools::ToolClients;
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    native_adoption::{
        DirectTurnAdoption, NativeAdoption, candidate, latest_turn, turns_params,
        verify_observations,
    },
    native_driver::{NativeDriverConnection, NativeDriverRuntime},
    read_evidence::ReadEvidenceStore,
};
use std::{path::Path, sync::Mutex};

fn catalogs() -> &'static CheckedNativeContracts {
    static C: OnceLock<CheckedNativeContracts> = OnceLock::new();
    C.get_or_init(|| {
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
    })
}
fn catalog() -> Json {
    catalogs()
        .checked_native_contract(&"0.159.2".into(), None)
        .unwrap()
        .catalog()
        .clone()
}
fn observation(method: &str, params: Json, result: Json) -> Json {
    let catalog = catalog();
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("observationId", format!("observed:{method}").into()),
        ("callerPrincipalId", "board".into()),
        ("serviceNodeId", "agent".into()),
        ("nativeVersion", "0.159.2".into()),
        (
            "nativeExecutableHash",
            f(&catalog, "nativeExecutableHash").clone(),
        ),
        ("catalogHash", hash_json(&catalog).unwrap().into()),
        ("epoch", "replacement-epoch".into()),
        ("requestId", Json::Number(1.0)),
        ("observedAt", AT.into()),
        ("method", method.into()),
        (
            "requestHash",
            hash_json(&Json::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]))
            .unwrap()
            .into(),
        ),
        ("params", params),
        ("reply", Json::object([("result", result)])),
    ])
}
fn turn(id: &str, status: &str, started: f64, completed: Json) -> Json {
    Json::object([
        ("id", id.into()),
        ("status", status.into()),
        ("startedAt", Json::Number(started)),
        ("completedAt", completed),
    ])
}
fn pair(rows: Vec<Json>) -> (Json, Json) {
    (
        observation(
            "thread/read",
            Json::object([
                ("threadId", "thread".into()),
                ("includeTurns", false.into()),
            ]),
            Json::object([(
                "thread",
                Json::object([
                    ("id", "thread".into()),
                    ("ephemeral", false.into()),
                    ("path", "sessions/current.jsonl".into()),
                    ("status", Json::object([("type", "active".into())])),
                ]),
            )]),
        ),
        observation(
            "thread/turns/list",
            turns_params(&"thread".into()),
            Json::object([("data", Json::Array(rows)), ("nextCursor", Json::Null)]),
        ),
    )
}
pub(super) fn current_pair() -> (Json, Json) {
    pair(vec![turn("direct", "inProgress", 1767323045.0, Json::Null)])
}
pub(super) fn idle(state: &str) -> World {
    let mut task = base_task();
    task.insert("claim", Json::Null);
    task.insert("workflowState", state.into());
    task.insert("attemptCount", Json::Number(2.0));
    task.insert(
        "completionIssue",
        Json::object([("code", "old_issue".into())]),
    );
    let mut run = base_run();
    run.insert("phase", "completed".into());
    run.insert("createdAt", AT.into());
    run.insert(
        "plan",
        Json::object([
            ("nativeVersion", "old-version".into()),
            ("continuationSetting", "keep".into()),
        ]),
    );
    World::new(task, run)
}
pub(super) fn verify(
    w: &World,
    task: &Json,
    previous: &Json,
    metadata: &Json,
    turns: &Json,
) -> IvyResult<Json> {
    verify_observations(
        w,
        catalogs(),
        &"board".into(),
        task,
        previous,
        metadata,
        turns,
    )
}
impl BlobStore for World {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<Json> {
        panic!("Read verification stores no blobs")
    }
    fn read(&self, _: ChunkKey, _: &Json, _: &JsString) -> IvyResult<Vec<u8>> {
        panic!("Read verification reads no blobs")
    }
}
impl ReadEvidenceStore for World {
    fn validate(&self, _: &str, _: &Json) -> IvyResult<()> {
        panic!("Use exact observation admission")
    }
    async fn read_document(&self, _: &str, _: &Json, _: bool) -> IvyResult<Document> {
        panic!("Verification does not materialize unrelated records")
    }
    async fn write_document(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("No evidence mutation")
    }
}

#[test]
fn newest_direct_turn_requires_complete_bounded_unambiguous_metadata() {
    let (_, empty) = pair(vec![]);
    assert!(latest_turn(&empty).unwrap().is_none());
    let (_, rows) = pair(vec![
        turn("new", "completed", 20.0, Json::Number(30.0)),
        turn("old", "completed", 10.0, Json::Number(15.0)),
    ]);
    assert_eq!(
        f(latest_turn(&rows).unwrap().unwrap(), "id"),
        &Json::from("new")
    );
    let (_, tied) = pair(vec![
        turn("a", "inProgress", 20.0, Json::Null),
        turn("b", "inProgress", 20.0, Json::Null),
    ]);
    assert!(latest_turn(&tied).unwrap().is_none());
    let mut paged = rows.clone();
    set(
        &mut paged,
        "reply",
        "result",
        Json::object([
            ("data", f(f(f(&rows, "reply"), "result"), "data").clone()),
            ("nextCursor", "more".into()),
        ]),
    );
    assert_eq!(
        latest_turn(&paged).unwrap_err().code,
        "task_board_native_page_limit"
    );
    let (_, many) = pair(vec![turn("x", "inProgress", 0.0, Json::Null); 101]);
    assert_eq!(
        latest_turn(&many).unwrap_err().code,
        "task_board_native_page_limit"
    );
    let mut missing_time = rows;
    let mut data = f(f(f(&missing_time, "reply"), "result"), "data")
        .as_array()
        .unwrap()
        .to_vec();
    data[0].remove("startedAt");
    set(
        &mut missing_time,
        "reply",
        "result",
        Json::object([("data", Json::Array(data)), ("nextCursor", Json::Null)]),
    );
    assert_eq!(
        latest_turn(&missing_time).unwrap_err().code,
        "task_board_native_page_limit"
    );
}

#[test]
fn direct_turn_candidates_exclude_old_inactive_ephemeral_and_archived_history() {
    let w = idle("review");
    let task = w.final_task();
    let previous = w.saved_run();
    let (metadata, turns) = current_pair();
    assert!(
        candidate(&task, &previous, &metadata, &turns)
            .unwrap()
            .is_some()
    );
    for (key, value) in [("id", "other".into()), ("ephemeral", true.into())] {
        let mut bad = metadata.clone();
        let mut thread = f(f(f(&bad, "reply"), "result"), "thread").clone();
        thread.insert(key, value);
        set(
            &mut bad,
            "reply",
            "result",
            Json::object([("thread", thread)]),
        );
        assert_eq!(
            candidate(&task, &previous, &bad, &turns).unwrap_err().code,
            "task_board_evidence_mismatch"
        );
    }
    for (key, value) in [
        (
            "path",
            "C:\\sessions\\archived_sessions\\saved.jsonl".into(),
        ),
        ("status", Json::object([("type", "idle".into())])),
    ] {
        let mut bad = metadata.clone();
        let mut thread = f(f(f(&bad, "reply"), "result"), "thread").clone();
        thread.insert(key, value);
        set(
            &mut bad,
            "reply",
            "result",
            Json::object([("thread", thread)]),
        );
        assert!(candidate(&task, &previous, &bad, &turns).unwrap().is_none());
    }
    let boundary = ivy_sdk::clock::parse_date(&AT.into()) / 1000.0;
    for status in ["completed", "failed", "interrupted"] {
        for (finished, accepted) in [
            (boundary - 1.0, false),
            (boundary, true),
            (boundary + 1.0, true),
        ] {
            let (metadata, turns) = pair(vec![turn(
                "direct",
                status,
                boundary - 3.0,
                Json::Number(finished),
            )]);
            assert_eq!(
                candidate(&task, &previous, &metadata, &turns)
                    .unwrap()
                    .is_some(),
                accepted
            );
        }
    }
    let (metadata, turns) = pair(vec![turn(
        "turn",
        "completed",
        boundary,
        Json::Number(boundary),
    )]);
    assert!(
        candidate(&task, &previous, &metadata, &turns)
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn adopted_publication_keeps_execution_context_settings_and_zero_native_calls() {
    for state in ["backlog", "todo", "waiting", "review", "done", "cancelled"] {
        let w = idle(state);
        let before = w.final_task();
        let (metadata, turns) = current_pair();
        let out = w
            .publish(Json::object([
                ("kind", "adoptTurn".into()),
                ("metadata", metadata),
                ("turns", turns),
            ]))
            .await
            .unwrap();
        let saved = w.lookup("task-board/run", f(&out, "run")).unwrap().value;
        let task = w.final_task();
        assert_eq!(f(&saved, "phase"), &Json::from("running"));
        assert_eq!(f(&saved, "attempt"), &Json::Number(3.0));
        assert_eq!(f(&saved, "turnId"), &Json::from("direct"));
        assert_eq!(f(&saved, "nativeEpoch"), &Json::from("replacement-epoch"));
        assert_eq!(f(f(&saved, "owner"), "source"), &Json::from("native"));
        assert_eq!(
            f(f(&saved, "plan"), "continuationSetting"),
            &Json::from("keep")
        );
        assert_eq!(
            f(f(&saved, "plan"), "nativeVersion"),
            &Json::from("0.159.2")
        );
        assert_eq!(
            f(f(&saved, "plan"), "definitions")
                .as_object()
                .unwrap()
                .len(),
            4
        );
        for key in ["thread", "turn", "interrupt"] {
            assert_eq!(f(f(&saved, "calls"), key), &Json::Null);
        }
        for key in ["workflowState", "latestResult", "acceptedReview"] {
            assert_eq!(f(f(&saved, "executionContext"), key), f(&before, key));
        }
        assert_eq!(f(&task, "workflowState"), &Json::from("in_progress"));
        assert!(task.get("completionIssue").is_none());
        assert_eq!(f(&task, "latestResult"), f(&before, "latestResult"));
        assert_eq!(f(&task, "acceptedReview"), f(&before, "acceptedReview"));
        assert_eq!(f(f(&task, "claim"), "run"), f(&out, "run"));
        assert_eq!(w.creates.borrow().len(), 2);
        assert_eq!(w.writes.borrow().len(), 2);
        assert_eq!(w.workspace_reads.get(), 0);
        assert_eq!(w.cleanup.get(), 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn adopted_publication_rejects_mismatched_proof_claims_and_stale_task_before_writes() {
    for changed in [
        "callerPrincipalId",
        "serviceNodeId",
        "nativeVersion",
        "nativeExecutableHash",
        "catalogHash",
        "epoch",
        "method",
        "requestHash",
        "params",
    ] {
        let w = idle("review");
        let (metadata, mut turns) = current_pair();
        turns.insert(
            changed,
            if changed == "params" {
                Json::object([("threadId", "other".into())])
            } else {
                "mismatched".into()
            },
        );
        assert!(
            w.publish(Json::object([
                ("kind", "adoptTurn".into()),
                ("metadata", metadata),
                ("turns", turns)
            ]))
            .await
            .is_err(),
            "{changed}"
        );
        assert!(w.writes.borrow().is_empty());
        assert!(w.creates.borrow().is_empty());
    }
    for changed in [
        "claim",
        "publication",
        "lastRun",
        "primaryResourceRef",
        "fields",
        "revision",
    ] {
        let w = idle("review");
        let mut task = w.final_task();
        match changed {
            "fields" => set(&mut task, "fields", "control", "manual".into()),
            "primaryResourceRef" => {
                set(&mut task, "primaryResourceRef", "nativeId", "other".into())
            }
            "lastRun" => {
                task.insert(changed, pin(999));
            }
            "revision" => {}
            _ => {
                task.insert(changed, Json::object([("busy", true.into())]));
            }
        }
        let mut p = pin(1);
        if changed == "revision" {
            p.insert("revision", Json::Number(2.0));
        }
        w.put("task-board/task", p, task);
        let (metadata, turns) = current_pair();
        assert!(
            w.publish(Json::object([
                ("kind", "adoptTurn".into()),
                ("metadata", metadata),
                ("turns", turns)
            ]))
            .await
            .is_err(),
            "{changed}"
        );
        assert!(w.writes.borrow().is_empty());
        assert!(w.creates.borrow().is_empty());
    }
}

struct Client {
    turns: Mutex<Json>,
    metadata: Json,
    calls: Mutex<Vec<Json>>,
    failure: Mutex<Option<IvyError>>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(Json::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            if let Some(error) = self.failure.lock().unwrap().clone() {
                return Err(error);
            }
            if method == "serviceNodes.get" {
                return Ok(Json::object([
                    ("serviceName", "agent-manager".into()),
                    ("connected", true.into()),
                    ("synced", true.into()),
                    ("ready", true.into()),
                    ("nativeVersion", "0.159.2".into()),
                ]));
            }
            if method == "tools.list" {
                let name = f(&params, "namePrefix").as_str()?;
                return Ok(Json::object([
                    (
                        "provider",
                        Json::object([("node", Json::object([("serviceNodeId", "agent".into())]))]),
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
                                    ("inputSchema", Json::object([] as [(&str, Json); 0])),
                                    ("outputSchema", Json::object([] as [(&str, Json); 0])),
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
            match f(&params, "qualifiedName").as_str()?.as_str() {
                "agent.status" => {
                    let c = catalog();
                    Ok(Json::object([
                        ("serviceNodeId", "agent".into()),
                        ("hostId", "host".into()),
                        ("state", "ready".into()),
                        ("epoch", "replacement-epoch".into()),
                        ("nativeVersion", "0.159.2".into()),
                        (
                            "nativeExecutableHash",
                            f(&c, "nativeExecutableHash").clone(),
                        ),
                        ("catalogHash", hash_json(&c)?.into()),
                        ("observedAt", AT.into()),
                        ("pid", Json::Number(42.0)),
                        ("code", Json::Null),
                        ("initialized", Json::object([] as [(&str, Json); 0])),
                        ("pendingInputs", Json::Number(0.0)),
                        (
                            "operations",
                            Json::object([
                                ("retained", Json::Number(0.0)),
                                ("maximum", Json::Number(100.0)),
                                ("bytes", Json::Number(0.0)),
                                ("maximumBytes", Json::Number(1048576.0)),
                            ]),
                        ),
                        ("observedMethods", Json::Array(vec![])),
                    ]))
                }
                "agent.read" => {
                    let args = f(&params, "arguments");
                    match f(args, "method").as_str()?.as_str() {
                        "thread/turns/list" => Ok(self.turns.lock().unwrap().clone()),
                        "thread/read" => Ok(self.metadata.clone()),
                        method => panic!("Unexpected read {method}"),
                    }
                }
                name => panic!("No native write permitted: {name}"),
            }
        })
    }
}
struct Driver {
    tools: Arc<ToolClients>,
    updates: RefCell<Vec<Json>>,
    valid: Cell<bool>,
}
impl NativeDriverRuntime for Driver {
    async fn verify_owner(&self) -> IvyResult<()> {
        if self.valid.get() {
            Ok(())
        } else {
            Err(IvyError::new("owner_lost", "Original owner lost"))
        }
    }
    async fn scoped_operation_id(&self, _: &Json) -> IvyResult<JsString> {
        panic!("Adoption dispatches no native calls")
    }
    async fn native_update(&self, r: &Json) -> IvyResult<Json> {
        self.updates.borrow_mut().push(r.clone());
        Ok(Json::object([("run", pin(80))]))
    }
    fn now(&self) -> JsString {
        AT.into()
    }
}
impl NativeDriverConnection for Driver {
    fn tools(&self) -> &Arc<ToolClients> {
        &self.tools
    }
    fn principal_id(&self) -> &JsString {
        static P: OnceLock<JsString> = OnceLock::new();
        P.get_or_init(|| "board".into())
    }
}
fn client() -> Arc<Client> {
    let (metadata, turns) = current_pair();
    Arc::new(Client {
        turns: Mutex::new(turns),
        metadata,
        calls: Mutex::default(),
        failure: Mutex::new(None),
    })
}
fn driver(client: Arc<Client>) -> Driver {
    Driver {
        tools: Arc::new(ToolClients::new(client)),
        updates: RefCell::default(),
        valid: Cell::new(true),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn discovery_is_read_only_and_retains_exact_owner_observations_in_native_update() {
    let w = idle("review");
    let client = client();
    let driver = driver(client.clone());
    let subject = NativeAdoption::new(&w, &driver, client.as_ref(), || 0.0);
    assert_eq!(subject.adopt(&id(1)).await.unwrap(), Some(id(80)));
    let updates = driver.updates.borrow();
    assert_eq!(updates.len(), 1);
    let request = &updates[0];
    assert_eq!(f(request, "run"), &pin(2));
    assert_eq!(f(request, "expectedRevision"), &Json::Number(1.0));
    assert_eq!(f(f(request, "change"), "metadata"), &client.metadata);
    assert_eq!(
        f(f(request, "change"), "turns"),
        &*client.turns.lock().unwrap()
    );
    assert!(
        s(request, "operationId")
            .to_utf8()
            .unwrap()
            .starts_with("task-board:")
    );
    assert!(w.writes.borrow().is_empty());
    assert!(w.creates.borrow().is_empty());
    assert!(client.calls.lock().unwrap().iter().all(|v| {
        ["serviceNodes.get", "tools.list", "tools.call"]
            .contains(&f(v, "method").as_str().unwrap().as_str())
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn idle_discovery_cooldown_notifications_and_transport_failures_never_authorize_a_start() {
    let w = idle("waiting");
    let client = client();
    let driver = driver(client.clone());
    let (_, same) = pair(vec![turn("turn", "inProgress", 1.0, Json::Null)]);
    *client.turns.lock().unwrap() = same;
    let now = Cell::new(0.0);
    let subject = NativeAdoption::new(&w, &driver, client.as_ref(), || now.get());
    assert!(subject.adopt(&id(1)).await.unwrap().is_none());
    let calls = client.calls.lock().unwrap().len();
    now.set(29_999.0);
    assert!(subject.adopt(&id(1)).await.unwrap().is_none());
    assert_eq!(client.calls.lock().unwrap().len(), calls);
    let event = |node: &str| {
        Json::object([(
            "params",
            Json::object([
                ("serviceNodeId", node.into()),
                (
                    "payload",
                    Json::object([
                        ("method", "turn/started".into()),
                        ("params", Json::object([("threadId", "thread".into())])),
                    ]),
                ),
            ]),
        )])
    };
    assert!(!subject.notification(&event("other")));
    assert!(subject.notification(&event("agent")));
    *client.turns.lock().unwrap() = current_pair().1;
    assert_eq!(subject.adopt(&id(1)).await.unwrap(), Some(id(80)));
    assert_eq!(driver.updates.borrow().len(), 1);
    *client.failure.lock().unwrap() = Some(IvyError::new(
        "service_unavailable",
        "Exact transport failure",
    ));
    let error = subject.adopt(&id(1)).await.unwrap_err();
    assert_eq!(error.code, "service_unavailable");
    assert_eq!(
        error.message_js(),
        JsString::from("Exact transport failure")
    );
    driver.valid.set(false);
    assert_eq!(subject.adopt(&id(1)).await.unwrap_err().code, "owner_lost");
    assert_eq!(driver.updates.borrow().len(), 1);
}
