#[path = "reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    native_comments::{NativeCommentJournal, sync_native_comments},
    native_inspector::InspectionStore,
    operations::Document,
    read_evidence::ReadEvidenceStore,
};
use std::{
    cell::RefCell,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};
type Trace = Arc<Mutex<Vec<Json>>>;
fn record(trace: &Trace, parts: impl IntoIterator<Item = Json>) {
    trace
        .lock()
        .unwrap()
        .push(Json::Array(parts.into_iter().collect()));
}
struct Client {
    input: Json,
    trace: Trace,
    statuses: Mutex<usize>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            record(&self.trace, ["rpc".into(), method.into(), params.clone()]);
            if method == "tools.list" {
                let name = params.required("namePrefix")?.clone();
                return Ok(Json::object([
                    (
                        "provider",
                        Json::object([(
                            "node",
                            Json::object([(
                                "serviceNodeId",
                                params.required("serviceNodeId")?.clone(),
                            )]),
                        )]),
                    ),
                    (
                        "items",
                        Json::Array(vec![Json::object([
                            ("qualifiedName", format!("agent.{}", name.as_str()?).into()),
                            (
                                "definition",
                                Json::object([
                                    ("namespace", "agent".into()),
                                    ("name", name),
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
            if params.required("qualifiedName")?.is_str("agent.status") {
                let mut index = self.statuses.lock().unwrap();
                let value = self
                    .input
                    .get("statuses")
                    .and_then(Json::as_array)
                    .and_then(|items| items.get(*index))
                    .or_else(|| self.input.get("status"))
                    .unwrap()
                    .clone();
                *index += 1;
                return Ok(value);
            }
            if let Some(identity) = params
                .get("arguments")
                .and_then(|args| args.get("identity"))
            {
                return self
                    .input
                    .get("historicalPages")
                    .and_then(Json::as_array)
                    .and_then(|pages| {
                        pages
                            .iter()
                            .find(|entry| entry.get("identity") == Some(identity))
                    })
                    .and_then(|entry| entry.get("page"))
                    .cloned()
                    .ok_or_else(|| {
                        IvyError::new("service_not_ready", "Original input owner is unavailable.")
                    });
            }
            if self.input.get("rpcFailure").and_then(Json::as_bool) == Some(true) {
                return Err(IvyError::new("fixture_failed", "input query"));
            }
            if let Some(page) = self.input.get("page") {
                return Ok(page.clone());
            }
            Ok(Json::object([
                ("epoch", "epoch".into()),
                ("items", self.input.required("items")?.clone()),
                ("truncated", false.into()),
            ]))
        })
    }
}
struct Store {
    input: Json,
    tools: ToolClients,
    trace: Trace,
    current: RefCell<Json>,
    posted: RefCell<Vec<Json>>,
    writes: RefCell<usize>,
    task_reads: RefCell<usize>,
}
impl BlobStore for Store {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<Json> {
        unreachable!()
    }
    fn read(&self, _: ChunkKey, _: &Json, _: &JsString) -> IvyResult<Vec<u8>> {
        unreachable!()
    }
}
impl ReadEvidenceStore for Store {
    fn validate(&self, _: &str, _: &Json) -> IvyResult<()> {
        unreachable!()
    }
    async fn read_document(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document> {
        record(
            &self.trace,
            ["read".into(), key.into(), address.clone(), upgrade.into()],
        );
        let (pin, value) = if key == "task-board/run" {
            (
                Json::object([("objectId", "run".into()), ("revision", Json::Number(1.0))]),
                self.input.required("run")?.clone(),
            )
        } else {
            let mut reads = self.task_reads.borrow_mut();
            let current = self.current.borrow();
            let value = self
                .input
                .get("tasks")
                .and_then(Json::as_array)
                .and_then(|items| items.get(*reads))
                .cloned()
                .unwrap_or_else(|| current.required("value").unwrap().clone());
            *reads += 1;
            (current.required("pin")?.clone(), value)
        };
        Ok(Document {
            pin,
            value,
            parent_id: None,
            content_hash: String::new(),
        })
    }
    async fn write_document(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
}
impl InspectionStore for Store {
    async fn verify_owner(&self) -> IvyResult<()> {
        assert!(self.input.get("status").is_some());
        Ok(())
    }
    async fn named_document(&self, _: &str, _: &str, _: &JsString) -> IvyResult<Option<Document>> {
        unreachable!()
    }
    fn tools(&self) -> &ToolClients {
        &self.tools
    }
    fn principal_id(&self) -> JsString {
        unreachable!()
    }
    fn now(&self) -> JsString {
        unreachable!()
    }
}
impl Store {
    async fn publish_comment(&self, request: &Json) -> IvyResult<()> {
        record(&self.trace, ["comment".into(), request.clone()]);
        let mut writes = self.writes.borrow_mut();
        *writes += 1;
        let fails = self.input.get("failAt").and_then(Json::as_f64) == Some(*writes as f64);
        if fails && self.input.get("failAfterWrite").and_then(Json::as_bool) != Some(true) {
            return Err(IvyError::new("fixture_failed", "publication"));
        }
        let mut current = self.current.borrow_mut();
        let mut value = current.required("value")?.clone();
        let mut comments = value.required("comments")?.as_array().unwrap().to_vec();
        comments.push(Json::object([(
            "commentId",
            request.required("commentId")?.clone(),
        )]));
        value.insert("comments", Json::Array(comments));
        current.insert("value", value);
        let mut pin = current.required("pin")?.clone();
        let revision = pin.required("revision")?.as_f64().unwrap() + 1.0;
        pin.insert("revision", Json::Number(revision));
        current.insert("pin", pin);
        self.posted.borrow_mut().push(request.clone());
        if fails {
            return Err(
                IvyError::new("outcome_unknown", "publication").with_outcome(Outcome::Unknown)
            );
        }
        Ok(())
    }
}
pub(crate) fn data() -> &'static Json {
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        let output =
            reference_data::read(include_bytes!("task-board-native-comments-cases.json.gz"));
        parse(
            std::str::from_utf8(&output).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
#[tokio::test]
async fn native_comment_mirroring_preserves_identity_confidentiality_and_native_text() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let fixture = data();
    for case in fixture.required("rows").unwrap().as_array().unwrap() {
        let input = case.required("input").unwrap();
        let trace: Trace = Arc::default();
        let client = Arc::new(Client {
            input: input.clone(),
            trace: trace.clone(),
            statuses: Mutex::default(),
        });
        let current = Json::object([
            ("pin", fixture.required("taskPin").unwrap().clone()),
            ("value", input.required("task").unwrap().clone()),
        ]);
        let initial = current.clone();
        let store = Store {
            input: input.clone(),
            tools: ToolClients::new(client),
            trace: trace.clone(),
            current: RefCell::new(current),
            posted: RefCell::default(),
            writes: RefCell::default(),
            task_reads: RefCell::default(),
        };
        let store_ref = &store;
        let journal = NativeCommentJournal {
            inspection: &store,
            native_comment: move |request: Json| async move {
                store_ref
                    .publish_comment(&request)
                    .await
                    .map(|_| Json::Null)
            },
        };
        let mut result = Json::Null;
        for _ in 0..input.get("passes").and_then(Json::as_f64).unwrap_or(1.0) as usize {
            if let Err(error) = sync_native_comments(&journal, &catalogs, &"run".into()).await {
                result = Json::object([
                    ("code", error.code.into()),
                    ("message", error.message.into()),
                    ("outcome", error.outcome.to_json()),
                ]);
                break;
            }
        }
        drop(journal);
        let actual = Json::object([
            ("result", result),
            ("trace", Json::Array(trace.lock().unwrap().clone())),
            ("posted", Json::Array(store.posted.into_inner())),
            ("current", store.current.into_inner()),
        ]);
        let name = case.required("name").unwrap().as_str().unwrap();
        if name == "object and null answer values" {
            // The native text port already refuses arbitrary JSON conversion.
            // An invalid answer must fail before any comment is published.
            assert_eq!(
                actual.required("result").unwrap(),
                &Json::object([
                    ("code", "internal_error".into()),
                    ("message", "The operation failed internally.".into()),
                    ("outcome", "unknown".into()),
                ])
            );
            assert_eq!(actual.required("posted").unwrap(), &Json::Array(vec![]));
            assert_eq!(actual.required("current").unwrap(), &initial);
            assert_eq!(
                actual.required("trace").unwrap().as_array().unwrap().len(),
                4
            );
            continue;
        }
        if name == "question coercion" {
            // Preserve string questions and the original protocol value; do not
            // recreate JavaScript's formatting of numbers/booleans/arrays.
            let input = &input.required("items").unwrap().as_array().unwrap()[0];
            let id = format!(
                "native-question-{}",
                &hash_json(input.required("identity").unwrap()).unwrap()[7..]
            );
            let expected = Json::object([
                ("action", "comment".into()),
                ("taskId", "task".into()),
                ("expectedRevision", Json::Number(3.0)),
                ("operationId", format!("{id}-3").into()),
                ("commentId", id.clone().into()),
                ("purpose", "question".into()),
                ("body", "First".into()),
                ("nativeInput", input.clone()),
                ("contactUser", true.into()),
                ("requests", Json::Array(vec![])),
                ("responses", Json::Array(vec![])),
                ("attachments", Json::Array(vec![])),
                ("replyTo", Json::Null),
                ("agentDelivery", "none".into()),
            ]);
            assert_eq!(actual.required("result").unwrap(), &Json::Null);
            assert_eq!(
                hash_json(actual.required("posted").unwrap()).unwrap(),
                hash_json(&Json::Array(vec![expected.clone()])).unwrap()
            );
            let mut current = initial;
            let mut pin = current.required("pin").unwrap().clone();
            pin.insert("revision", Json::Number(4.0));
            current.insert("pin", pin);
            let mut value = current.required("value").unwrap().clone();
            value.insert(
                "comments",
                Json::Array(vec![Json::object([("commentId", id.into())])]),
            );
            current.insert("value", value);
            assert_eq!(actual.required("current").unwrap(), &current);
            let trace = actual.required("trace").unwrap().as_array().unwrap();
            assert_eq!(
                trace
                    .iter()
                    .map(|v| v.as_array().unwrap()[0].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["read", "read", "rpc", "rpc", "read", "comment"]
            );
            assert_eq!(
                hash_json(&trace.last().unwrap().as_array().unwrap()[1]).unwrap(),
                hash_json(&expected).unwrap()
            );
            continue;
        }
        assert_eq!(
            hash_json(&actual).unwrap(),
            case.required("expectedHash").unwrap().as_str().unwrap(),
            "{}: {}",
            case.required("name").unwrap().as_str().unwrap(),
            ivy_contracts::json::canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
        );
    }
}

#[tokio::test]
async fn routed_native_comments_preserve_exact_inputs_and_refuse_changed_route_or_claim() {
    use ivy_contracts::native_backend::{BackendIdentity, NativeProvider};
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let json = |value: &str| parse(value, MANAGEMENT_FRAME_BYTES).unwrap();
    for (provider, version) in [
        (NativeProvider::Codex, "0.163.0"),
        (NativeProvider::Claude, "0.142.3"),
    ] {
        for original_scoped in [false, true] {
            let backend = BackendIdentity::new("primary".into(), provider, "one".into()).unwrap();
            let foreign = BackendIdentity::new("primary".into(), provider, "two".into()).unwrap();
            let contract = catalogs
                .checked_native_contract(&version.into(), None)
                .unwrap();
            let mut target = json(r#"{"hostId":"fixture","serviceNodeId":"native-agent"}"#);
            let mut plan = Json::object([
                ("nativeVersion", version.into()),
                (
                    "catalogSourceHash",
                    contract.catalog().required("sourceHash").unwrap().clone(),
                ),
            ]);
            if original_scoped {
                target = backend.arguments(target).unwrap();
                plan = backend.arguments(plan).unwrap();
            }
            let selected = backend
                .arguments(json(r#"{"hostId":"fixture","serviceNodeId":"next"}"#))
                .unwrap();
            let run = Json::object([
                ("taskId", "task".into()),
                ("phase", "running".into()),
                ("target", target.clone()),
                ("plan", plan),
                ("nativeEpoch", "original".into()),
                ("turnId", "turn".into()),
                (
                    "primaryResourceRef",
                    Json::object([
                        (
                            "namespace",
                            if original_scoped {
                                backend.id().into()
                            } else {
                                "codex".into()
                            },
                        ),
                        ("kind", "thread".into()),
                        ("serviceNodeId", "native-agent".into()),
                        ("nativeId", "thread".into()),
                    ]),
                ),
            ]);
            let task = Json::object([
                ("claim", json(r#"{"run":{"objectId":"run","revision":1}}"#)),
                ("comments", Json::Array(vec![])),
                (
                    "nativeRoutes",
                    Json::Array(vec![Json::object([
                        ("source", target.clone()),
                        ("target", selected.clone()),
                    ])]),
                ),
            ]);
            let mut status = json(
                r#"{"serviceNodeId":"next","hostId":"fixture","epoch":"moved","pid":42,"state":"ready","observedAt":"2026-01-02T03:04:05.006Z","code":null,"initialized":{},"pendingInputs":0,"operations":{"retained":0,"maximum":100,"bytes":0,"maximumBytes":1048576},"observedMethods":[]}"#,
            );
            status.insert("nativeVersion", version.into());
            status.insert(
                "nativeExecutableHash",
                contract
                    .catalog()
                    .required("nativeExecutableHash")
                    .unwrap()
                    .clone(),
            );
            status.insert("catalogHash", contract.catalog_hash().into());
            let status = backend.arguments(status).unwrap();
            let mut input = json(
                r#"{"identity":{"serviceNodeId":"next","epoch":"moved","requestId":1},"method":"item/tool/requestUserInput","params":{"questions":[{"question":"Continue?"}]},"observedAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","threadId":"thread","turnId":"turn","state":"answered","answerOperationId":"answer","answerCallerPrincipalId":"caller","reply":{"result":{"answers":{"one":{"answers":["Continue."]}}}},"code":null}"#,
            );
            input.insert(
                "identity",
                backend
                    .arguments(input.required("identity").unwrap().clone())
                    .unwrap(),
            );
            let mut other_account = input.clone();
            let mut identity = other_account.required("identity").unwrap().clone();
            identity.insert("backend", foreign.json());
            other_account.insert("identity", identity);
            let mut old_epoch = input.clone();
            let mut identity = old_epoch.required("identity").unwrap().clone();
            identity.insert("epoch", "original".into());
            old_epoch.insert("identity", identity);
            let mut other_thread = input.clone();
            other_thread.insert("threadId", "another".into());
            let page = backend
                .arguments(Json::object([
                    ("epoch", "moved".into()),
                    (
                        "items",
                        Json::Array(vec![input.clone(), other_account, old_epoch, other_thread]),
                    ),
                    ("truncated", false.into()),
                ]))
                .unwrap();
            let base = Json::object([
                ("run", run.clone()),
                ("status", status.clone()),
                ("page", page.clone()),
            ]);
            let make = |input: Json| {
                let initial = input.get("initialTask").unwrap_or(&task).clone();
                let trace: Trace = Arc::default();
                let client = Arc::new(Client {
                    input: input.clone(),
                    trace: trace.clone(),
                    statuses: Mutex::default(),
                });
                Store {
                    input,
                    tools: ToolClients::new(client),
                    trace,
                    current: RefCell::new(Json::object([
                        ("pin", json(r#"{"objectId":"task","revision":3}"#)),
                        ("value", initial),
                    ])),
                    posted: RefCell::default(),
                    writes: RefCell::default(),
                    task_reads: RefCell::default(),
                }
            };
            let store = make(base.clone());
            for _ in 0..2 {
                sync_routed_fixture(&store, &catalogs).await.unwrap();
            }
            let posted = store.posted.borrow();
            assert_eq!(posted.len(), 2);
            for request in posted.iter() {
                assert_eq!(
                    request.required("nativeInput").unwrap().get("identity"),
                    input.get("identity")
                );
                assert_eq!(
                    request
                        .get("contactUser")
                        .and_then(Json::as_bool)
                        .unwrap_or(false),
                    false
                );
            }
            assert_eq!(posted[1].get("nativeInput"), Some(&input));
            let hash = hash_json(input.required("identity").unwrap()).unwrap();
            assert_eq!(
                posted[0].get("commentId"),
                Some(&format!("native-question-{}", &hash[7..]).into())
            );
            assert_eq!(posted[1].get("replyTo"), posted[0].get("commentId"));
            assert_eq!(store.input.get("run"), Some(&run));
            drop(posted);
            let trace = store.trace.lock().unwrap();
            for row in trace
                .iter()
                .filter_map(Json::as_array)
                .filter(|row| row[0].is_str("rpc"))
            {
                assert_eq!(row[2].get("serviceNodeId"), Some(&"next".into()));
                if row[1].is_str("tools.call") {
                    assert!(BackendIdentity::matches(
                        row[2].required("arguments").unwrap(),
                        Some(&backend)
                    ));
                    assert!(
                        row[2]
                            .required("qualifiedName")
                            .unwrap()
                            .is_str("agent.status")
                            || row[2]
                                .required("qualifiedName")
                                .unwrap()
                                .is_str("agent.inputs")
                    );
                }
            }
            drop(trace);
            // Original scoped owners still expose retained requests from their
            // original epoch after the current connection has been replaced.
            let mut direct = base.clone();
            let mut direct_run = run.clone();
            direct_run.insert("target", selected.clone());
            direct_run.insert("nativeEpoch", "moved".into());
            direct_run.insert(
                "plan",
                backend
                    .arguments(run.required("plan").unwrap().clone())
                    .unwrap(),
            );
            let mut primary = run.required("primaryResourceRef").unwrap().clone();
            primary.insert("serviceNodeId", "next".into());
            primary.insert("namespace", backend.id().into());
            direct_run.insert("primaryResourceRef", primary);
            direct.insert("run", direct_run);
            let mut historical_page = page.clone();
            historical_page.insert("epoch", "replacement".into());
            direct.insert("page", historical_page);
            let direct_store = make(direct);
            sync_routed_fixture(&direct_store, &catalogs).await.unwrap();
            assert_eq!(direct_store.posted.borrow().len(), 2);
            assert_eq!(
                direct_store.posted.borrow()[1].get("nativeInput"),
                Some(&input)
            );
            assert!(
                !direct_store
                    .trace
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(Json::as_array)
                    .any(|row| row[0].is_str("rpc")
                        && row[2]
                            .get("qualifiedName")
                            .is_some_and(|value| value.is_str("agent.status")))
            );
            for failure in ["account", "epoch", "route", "claim"] {
                let mut broken = base.clone();
                match failure {
                    "account" => {
                        let mut page = page.clone();
                        page.insert("backend", foreign.json());
                        broken.insert("page", page);
                    }
                    "epoch" => {
                        let mut page = page.clone();
                        page.insert("epoch", "other".into());
                        broken.insert("page", page);
                    }
                    "route" => {
                        let mut changed = task.clone();
                        let mut target = selected.clone();
                        target.insert("serviceNodeId", "last".into());
                        changed.insert(
                            "nativeRoutes",
                            Json::Array(vec![Json::object([
                                ("source", run.required("target").unwrap().clone()),
                                ("target", target),
                            ])]),
                        );
                        broken.insert(
                            "tasks",
                            Json::Array(vec![task.clone(), task.clone(), changed]),
                        );
                        let mut changed = status.clone();
                        changed.insert("serviceNodeId", "last".into());
                        broken.insert("statuses", Json::Array(vec![status.clone(), changed]));
                    }
                    "claim" => {
                        let mut changed = task.clone();
                        changed.insert("claim", Json::Null);
                        broken.insert(
                            "tasks",
                            Json::Array(vec![task.clone(), task.clone(), task.clone(), changed]),
                        );
                    }
                    _ => unreachable!(),
                }
                let store = make(broken);
                let error = sync_routed_fixture(&store, &catalogs).await.unwrap_err();
                assert_eq!(
                    error.code,
                    match failure {
                        "account" => "task_board_target_mismatch",
                        "claim" => "task_board_identity_conflict",
                        _ => "task_board_native_epoch_changed",
                    },
                    "{failure}"
                );
                assert!(store.posted.borrow().is_empty());
            }
            // Former service/backend owners are queried by their original
            // identity, never by substituting the destination's connection.
            for service_move in [false, true] {
                let previous =
                    BackendIdentity::new("previous".into(), provider, "one".into()).unwrap();
                let mut former_run = run.clone();
                let mut source = run.required("target").unwrap().clone();
                if !service_move {
                    source.insert("serviceNodeId", "next".into());
                }
                if original_scoped {
                    source.insert("backend", previous.json());
                }
                former_run.insert("target", source.clone());
                let mut primary = former_run.required("primaryResourceRef").unwrap().clone();
                primary.insert(
                    "serviceNodeId",
                    source.required("serviceNodeId").unwrap().clone(),
                );
                if original_scoped {
                    primary.insert("namespace", previous.id().into());
                }
                former_run.insert("primaryResourceRef", primary);
                if original_scoped {
                    let mut plan = former_run.required("plan").unwrap().clone();
                    plan.insert("backend", previous.json());
                    former_run.insert("plan", plan);
                }
                let mut initial = task.clone();
                initial.insert(
                    "nativeRoutes",
                    Json::Array(vec![Json::object([
                        ("source", source.clone()),
                        ("target", selected.clone()),
                    ])]),
                );
                let mut old_input = input.clone();
                let identity = BackendIdentity::select_arguments(
                    original_scoped.then_some(&previous),
                    Json::object([
                        (
                            "serviceNodeId",
                            source.required("serviceNodeId").unwrap().clone(),
                        ),
                        ("epoch", "moved".into()),
                        ("requestId", Json::Number(42.0)),
                    ]),
                )
                .unwrap();
                old_input.insert("identity", identity.clone());
                let hash = hash_json(&identity).unwrap();
                let question_id = format!("native-question-{}", &hash[7..]);
                let mut pending = old_input.clone();
                pending.insert("state", "pending".into());
                for key in [
                    "reply",
                    "answerOperationId",
                    "answerCallerPrincipalId",
                    "code",
                ] {
                    pending.insert(key, Json::Null);
                }
                let question = Json::object([
                    ("commentId", question_id.clone().into()),
                    ("purpose", "question".into()),
                    ("author", json(r#"{"source":"native"}"#)),
                    ("run", json(r#"{"objectId":"run","revision":1}"#)),
                    ("nativeInput", pending),
                ]);
                initial.insert("comments", Json::Array(vec![question.clone()]));
                for state in ["answered", "expired"] {
                    for secret in [false, true] {
                        let mut old_input = old_input.clone();
                        old_input.insert("state", state.into());
                        let mut initial = initial.clone();
                        let mut question = question.clone();
                        if secret {
                            let params =
                                json(r#"{"questions":[{"question":"Continue?","isSecret":true}]}"#);
                            old_input.insert("params", params.clone());
                            let mut pending = question.required("nativeInput").unwrap().clone();
                            pending.insert("params", params);
                            question.insert("nativeInput", pending);
                            initial.insert("comments", Json::Array(vec![question.clone()]));
                        }
                        let old_page = BackendIdentity::select_arguments(
                            original_scoped.then_some(&previous),
                            Json::object([
                                ("epoch", "retired-connection".into()),
                                ("items", Json::Array(vec![old_input.clone()])),
                                ("truncated", false.into()),
                            ]),
                        )
                        .unwrap();
                        let mut empty_page = page.clone();
                        empty_page.insert("items", Json::Array(vec![]));
                        let mut retained = base.clone();
                        retained.insert("run", former_run.clone());
                        retained.insert("initialTask", initial.clone());
                        retained.insert("page", empty_page);
                        retained.insert(
                            "historicalPages",
                            Json::Array(vec![Json::object([
                                ("identity", identity.clone()),
                                ("page", old_page.clone()),
                            ])]),
                        );
                        let store = make(retained.clone());
                        for _ in 0..2 {
                            sync_routed_fixture(&store, &catalogs).await.unwrap();
                        }
                        let posted = store.posted.borrow();
                        assert_eq!(
                            posted.len(),
                            1,
                            "former service={service_move} scoped={original_scoped} {state} secret={secret}"
                        );
                        assert_eq!(posted[0].get("replyTo"), Some(&question_id.clone().into()));
                        let mirrored = posted[0].required("nativeInput").unwrap();
                        assert_eq!(mirrored.get("identity"), Some(&identity));
                        assert_eq!(
                            mirrored.get("reply").cloned(),
                            if secret {
                                Some(Json::Null)
                            } else {
                                old_input.get("reply").cloned()
                            }
                        );
                        assert!(posted[0].get("contactUser").is_none());
                        drop(posted);
                        assert_eq!(store.input.get("run"), Some(&former_run));
                        let trace = store.trace.lock().unwrap();
                        let calls: Vec<_> = trace
                            .iter()
                            .filter_map(Json::as_array)
                            .filter(|row| {
                                row[0].is_str("rpc")
                                    && row[1].is_str("tools.call")
                                    && row[2]
                                        .get("arguments")
                                        .is_some_and(|a| a.get("identity").is_some())
                            })
                            .collect();
                        assert_eq!(
                            calls.len(),
                            1,
                            "A terminal response prevents another original-owner query"
                        );
                        assert_eq!(
                            calls[0][2].get("serviceNodeId"),
                            identity.get("serviceNodeId")
                        );
                        let args = calls[0][2].required("arguments").unwrap();
                        assert_eq!(args.get("identity"), Some(&identity));
                        assert!(BackendIdentity::matches(
                            args,
                            original_scoped.then_some(&previous)
                        ));
                        assert!(
                            trace
                                .iter()
                                .filter_map(Json::as_array)
                                .filter(|row| row[0].is_str("rpc") && row[1].is_str("tools.call"))
                                .all(|row| row[2].get("qualifiedName").is_some_and(|n| n
                                    .is_str("agent.inputs")
                                    || n.is_str("agent.status")))
                        );
                        drop(trace);
                        for failure in [
                            "missing-owner",
                            "missing",
                            "unrelated-owner",
                            "run",
                            "author",
                            "purpose",
                            "id",
                            "identity",
                            "params",
                            "method",
                            "observedAt",
                            "threadId",
                            "turnId",
                            "pending",
                            "answering",
                            "outcome_unknown",
                            "page-account",
                            "changed-question",
                            "claim",
                            "route",
                        ] {
                            let mut broken = retained.clone();
                            let mut initial = initial.clone();
                            let mut saved = question.clone();
                            let mut observed = old_input.clone();
                            match failure {
                                "missing-owner" => {
                                    broken.insert("historicalPages", Json::Array(vec![]))
                                }
                                "missing" => initial.insert("comments", Json::Array(vec![])),
                                "unrelated-owner" => {
                                    let mut pending =
                                        saved.required("nativeInput").unwrap().clone();
                                    let mut other = identity.clone();
                                    other.insert("serviceNodeId", "unrelated".into());
                                    pending.insert("identity", other);
                                    saved.insert("nativeInput", pending);
                                }
                                "run" => saved
                                    .insert("run", json(r#"{"objectId":"other","revision":1}"#)),
                                "author" => saved.insert("author", json(r#"{"source":"user"}"#)),
                                "purpose" => saved.insert("purpose", "update".into()),
                                "id" => saved.insert("commentId", "unrelated-question".into()),
                                "identity" => {
                                    let mut different = identity.clone();
                                    different.insert("requestId", Json::Number(43.0));
                                    observed.insert("identity", different);
                                }
                                "params" => observed.insert(
                                    "params",
                                    json(r#"{"questions":[{"question":"Different?"}]}"#),
                                ),
                                "method" => observed.insert("method", "other".into()),
                                "observedAt" => {
                                    observed.insert("observedAt", "2026-01-02T03:04:05.007Z".into())
                                }
                                "threadId" | "turnId" => observed.insert(failure, "other".into()),
                                "pending" | "answering" | "outcome_unknown" => {
                                    observed.insert("state", failure.into());
                                    if failure == "pending" {
                                        for key in [
                                            "reply",
                                            "answerOperationId",
                                            "answerCallerPrincipalId",
                                            "code",
                                        ] {
                                            observed.insert(key, Json::Null);
                                        }
                                    }
                                }
                                "page-account" => {}
                                "changed-question" | "claim" | "route" => {
                                    let mut changed = initial.clone();
                                    if failure == "claim" {
                                        changed.insert("claim", Json::Null);
                                    } else if failure == "route" {
                                        changed.insert("nativeRoutes", Json::Array(vec![]));
                                    } else {
                                        changed.insert("comments", Json::Array(vec![]));
                                    }
                                    broken.insert(
                                        "tasks",
                                        Json::Array(vec![
                                            initial.clone(),
                                            initial.clone(),
                                            initial.clone(),
                                            initial.clone(),
                                            changed,
                                        ]),
                                    );
                                }
                                _ => unreachable!(),
                            }
                            if failure != "missing" {
                                initial.insert("comments", Json::Array(vec![saved]));
                            }
                            if failure != "missing-owner" {
                                let mut old_page = old_page.clone();
                                old_page.insert("items", Json::Array(vec![observed]));
                                if failure == "page-account" {
                                    old_page.insert("backend", foreign.json());
                                }
                                broken.insert(
                                    "historicalPages",
                                    Json::Array(vec![Json::object([
                                        ("identity", identity.clone()),
                                        ("page", old_page),
                                    ])]),
                                );
                            }
                            broken.insert("initialTask", initial);
                            let store = make(broken);
                            let result = sync_routed_fixture(&store, &catalogs).await;
                            if failure == "page-account" {
                                assert_eq!(result.unwrap_err().code, "task_board_target_mismatch");
                            } else if failure == "claim" || failure == "route" {
                                assert_eq!(
                                    result.unwrap_err().code,
                                    "task_board_identity_conflict"
                                );
                            } else {
                                result.unwrap_or_else(|error| panic!("{failure}: {error:?}"));
                            }
                            assert!(store.posted.borrow().is_empty(), "{failure}");
                        }
                    }
                }
            }
            // A previously mirrored question from this selected owner keeps
            // its exact identity when its old connection later expires.
            for state in ["answered", "expired"] {
                for secret in [false, true] {
                    let mut historical = input.clone();
                    let mut identity = historical.required("identity").unwrap().clone();
                    identity.insert("epoch", "previous-connection".into());
                    historical.insert("identity", identity);
                    historical.insert("state", state.into());
                    if secret {
                        historical.insert(
                            "params",
                            json(r#"{"questions":[{"question":"Continue?","isSecret":true}]}"#),
                        );
                    }
                    let hash = hash_json(historical.required("identity").unwrap()).unwrap();
                    let question_id = format!("native-question-{}", &hash[7..]);
                    let mut pending = historical.clone();
                    pending.insert("state", "pending".into());
                    for key in [
                        "reply",
                        "answerOperationId",
                        "answerCallerPrincipalId",
                        "code",
                    ] {
                        pending.insert(key, Json::Null);
                    }
                    let question = Json::object([
                        ("commentId", question_id.clone().into()),
                        ("purpose", "question".into()),
                        ("author", json(r#"{"source":"native"}"#)),
                        ("run", json(r#"{"objectId":"run","revision":1}"#)),
                        ("nativeInput", pending),
                    ]);
                    let mut initial = task.clone();
                    initial.insert("comments", Json::Array(vec![question.clone()]));
                    let mut old_page = page.clone();
                    old_page.insert("items", Json::Array(vec![historical.clone()]));
                    let mut retained = base.clone();
                    retained.insert("page", old_page);
                    retained.insert("initialTask", initial.clone());
                    let store = make(retained.clone());
                    for _ in 0..2 {
                        sync_routed_fixture(&store, &catalogs).await.unwrap();
                    }
                    let posted = store.posted.borrow();
                    assert_eq!(posted.len(), 1, "{state} secret={secret}");
                    assert_eq!(posted[0].get("replyTo"), Some(&question_id.clone().into()));
                    let mirrored = posted[0].required("nativeInput").unwrap();
                    assert_eq!(mirrored.get("identity"), historical.get("identity"));
                    assert_eq!(mirrored.get("state"), Some(&state.into()));
                    assert_eq!(
                        mirrored.get("reply").cloned(),
                        if secret {
                            Some(Json::Null)
                        } else {
                            historical.get("reply").cloned()
                        }
                    );
                    assert_eq!(
                        posted[0].get("body"),
                        Some(
                            &match (state, secret) {
                                ("expired", _) => "This request has expired.",
                                (_, true) => "A confidential response was provided.",
                                _ => "Continue.",
                            }
                            .into()
                        )
                    );
                    assert!(posted[0].get("contactUser").is_none());
                    drop(posted);
                    for failure in [
                        "missing",
                        "run",
                        "author",
                        "purpose",
                        "identity",
                        "params",
                        "method",
                        "observedAt",
                        "pending",
                        "unknown",
                        "changed-question",
                        "claim",
                    ] {
                        let mut broken = retained.clone();
                        let mut initial = initial.clone();
                        let mut saved = question.clone();
                        match failure {
                            "missing" => initial.insert("comments", Json::Array(vec![])),
                            "run" => {
                                saved.insert("run", json(r#"{"objectId":"other","revision":1}"#))
                            }
                            "author" => saved.insert("author", json(r#"{"source":"user"}"#)),
                            "purpose" => saved.insert("purpose", "update".into()),
                            "identity" | "params" | "method" | "observedAt" => {
                                let mut input = saved.required("nativeInput").unwrap().clone();
                                input.insert(failure, Json::Null);
                                saved.insert("nativeInput", input);
                            }
                            "pending" | "unknown" => {
                                let mut input = historical.clone();
                                input.insert(
                                    "state",
                                    if failure == "pending" {
                                        "pending"
                                    } else {
                                        "outcome_unknown"
                                    }
                                    .into(),
                                );
                                if failure == "pending" {
                                    for key in [
                                        "reply",
                                        "answerOperationId",
                                        "answerCallerPrincipalId",
                                        "code",
                                    ] {
                                        input.insert(key, Json::Null);
                                    }
                                }
                                let mut page = broken.required("page").unwrap().clone();
                                page.insert("items", Json::Array(vec![input]));
                                broken.insert("page", page);
                            }
                            "changed-question" | "claim" => {
                                let mut changed = initial.clone();
                                if failure == "claim" {
                                    changed.insert("claim", Json::Null);
                                } else {
                                    changed.insert("comments", Json::Array(vec![]));
                                }
                                broken.insert(
                                    "tasks",
                                    Json::Array(vec![
                                        initial.clone(),
                                        initial.clone(),
                                        initial.clone(),
                                        initial.clone(),
                                        changed,
                                    ]),
                                );
                            }
                            _ => unreachable!(),
                        }
                        if failure != "missing" {
                            initial.insert("comments", Json::Array(vec![saved]));
                        }
                        broken.insert("initialTask", initial);
                        let store = make(broken);
                        let result = sync_routed_fixture(&store, &catalogs).await;
                        if failure == "claim" {
                            assert_eq!(result.unwrap_err().code, "task_board_identity_conflict");
                        } else {
                            result.unwrap_or_else(|error| panic!("{failure}: {error:?}"));
                        }
                        assert!(store.posted.borrow().is_empty(), "{failure}");
                    }
                }
            }
        }
    }
}

async fn sync_routed_fixture(store: &Store, catalogs: &CheckedNativeContracts) -> IvyResult<()> {
    let journal = NativeCommentJournal {
        inspection: store,
        native_comment: |request: Json| async move {
            store.publish_comment(&request).await.map(|_| Json::Null)
        },
    };
    sync_native_comments(&journal, catalogs, &"run".into()).await
}
