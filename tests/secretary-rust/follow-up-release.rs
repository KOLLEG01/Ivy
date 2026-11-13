use super::*;
use crate::{
    engine::SecretaryEngine, inbox_evidence::InboxEvidence, inbox_recovery::RecoveryScan,
    journal::Limits, store::Store,
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestFuture, RpcClient},
    tools::ToolClients,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

fn value(v: serde_json::Value) -> IvyJson {
    ivy_contracts::json::parse(&v.to_string(), 1_048_576).unwrap()
}
fn backend() -> IvyJson {
    value(json!({"backendId":"codex","provider":"codex","accountId":"default"}))
}
fn target() -> IvyJson {
    value(
        json!({"serviceNodeId":"fixture.agent","hostId":"fixture-host","backend":{"backendId":"codex","provider":"codex","accountId":"default"},"nativeVersion":"0.159.2","nativeExecutableHash":format!("sha256:{}","c".repeat(64)),"catalogHash":format!("sha256:{}","d".repeat(64))}),
    )
}
fn seed(journal: &Journal) -> (Document, Document) {
    let mut context = value(
        json!({"itemId":"item","threadId":"thread","createdFor":"question","start":{"method":"thread/start","params":{"serviceName":"secretary","threadSource":"ivy-secretary-follow-up"}}}),
    );
    context.insert("target", target());
    let context = journal
        .create_technical(CONTEXT_KEY, "item", &context)
        .unwrap();
    let mut question = value(
        json!({"request":{"itemId":"item","operationId":"fixture:1800000000000:question"},"threadId":"thread","turnId":"turn","phase":"succeeded","answer":"Original answer","errorCode":null}),
    );
    question.insert(
        "contextRelease",
        plan(
            &context.pin,
            &context.value,
            &question,
            &value(json!({"epoch":"epoch"})),
        )
        .unwrap()
        .unwrap(),
    );
    (
        context,
        journal
            .create_technical(FOLLOW_UP_KEY, "question", &question)
            .unwrap(),
    )
}
fn call(question: &Document) -> NativeOperationCall {
    let release = question.value.required("contextRelease").unwrap();
    NativeOperationCall {
        operation_id: text(release, "operationId").unwrap(),
        method: "agent.release".into(),
        params: IvyJson::object([
            ("threadId", "thread".into()),
            ("expectedEpoch", "epoch".into()),
            (
                "expectedOperationId",
                release.required("expectedOperationId").unwrap().clone(),
            ),
        ]),
        definition_hash: format!("sha256:{}", "a".repeat(64)).into(),
    }
}
struct Client {
    original: NativeOperationCall,
    remote: Mutex<Option<IvyJson>>,
    dispatched: AtomicUsize,
    calls: Mutex<Vec<IvyJson>>,
    inject: Option<PathBuf>,
    checks: AtomicUsize,
}
impl Client {
    fn new(question: &Document) -> Self {
        Self {
            original: call(question),
            remote: Mutex::new(None),
            dispatched: AtomicUsize::new(0),
            calls: Mutex::new(vec![]),
            inject: None,
            checks: AtomicUsize::new(0),
        }
    }
    fn operation(&self, phase: &str) -> IvyJson {
        let mut result = value(
            json!({"schemaVersion":1,"callerPrincipalId":"secretary-principal","serviceNodeId":"fixture.agent","nativeVersion":"0.159.2","nativeExecutableHash":format!("sha256:{}","c".repeat(64)),"method":"agent.release","phase":phase,"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z","epoch":"epoch","requestId":null,"reply":null,"code":null}),
        );
        result.insert("backend", backend());
        result.insert(
            "operationId",
            IvyJson::String(self.original.operation_id.clone()),
        );
        result.insert("params", self.original.params.clone());
        result.insert(
            "requestHash",
            hash_json(&IvyJson::object([
                ("method", "agent.release".into()),
                ("params", self.original.params.clone()),
            ]))
            .unwrap()
            .into(),
        );
        if phase == "accepted" {
            result.insert("epoch", IvyJson::Null);
        }
        if phase == "outcome_unknown" {
            result.insert("requestId", 42.0.into());
            result.insert("code", "native_connection_lost".into());
        }
        if phase == "succeeded" {
            result.insert(
                "release",
                value(json!({"kind":"logical","otherSubscribers":true})),
            );
        }
        if phase == "failed" {
            result.insert("code", "native_subscription_changed".into());
        }
        validate_agent("Operation", &result).unwrap();
        result
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "system.status" {
                return Ok(value(json!({"runtimeEpoch":"fixture"})));
            }
            self.calls.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            match method {
                "serviceNodes.get" => {
                    if self.checks.fetch_add(1, Ordering::Relaxed) == 1 {
                        if let Some(path) = &self.inject {
                            Journal::open(path,Limits::default())?.create_technical(FOLLOW_UP_KEY,"queued-after-observation",&value(json!({"request":{"itemId":"item"},"phase":"prepared","threadId":null})))?;
                        }
                    }
                    Ok(value(
                        json!({"connected":true,"synced":true,"serviceName":"secretary","principalId":"secretary-principal","hostId":"fixture-host"}),
                    ))
                }
                "tools.list" => {
                    let namespace = text(&params, "namespace")?.to_utf8()?;
                    let name = text(&params, "namePrefix")?.to_utf8()?;
                    assert!(matches!(
                        (namespace.as_str(), name.as_str()),
                        ("codex", "thread/unsubscribe")
                            | ("agent", "operation")
                            | ("agent", "release")
                    ));
                    Ok(value(
                        json!({"provider":{"node":{"serviceNodeId":"fixture.agent"}},"items":[{"qualifiedName":format!("{namespace}.{name}"),"definition":{"namespace":namespace,"name":name,"interfaceVersion":if namespace=="agent" {"1.0.0"} else {"0.159.2"},"inputSchema":{},"outputSchema":{}},"definitionHash":format!("sha256:{}","a".repeat(64))}],"nextCursor":null}),
                    ))
                }
                "tools.call" => {
                    let args = params.required("arguments")?;
                    assert_eq!(args.get("backend"), Some(&backend()));
                    assert_eq!(
                        args.get("operationId"),
                        Some(&IvyJson::String(self.original.operation_id.clone()))
                    );
                    match text(&params, "qualifiedName")?.to_utf8()?.as_str() {
                        "agent.operation" => {
                            if let Some(remote) = self.remote.lock().unwrap().clone() {
                                return Ok(remote);
                            }
                            let mut absence = value(
                                json!({"kind":"agent_operation_absent","serviceNodeId":"fixture.agent","epoch":"epoch"}),
                            );
                            absence.insert(
                                "operationId",
                                IvyJson::String(self.original.operation_id.clone()),
                            );
                            absence.insert("backend", backend());
                            Err(IvyError::new("not_found", "Original absent.")
                                .with_details(absence))
                        }
                        "agent.release" => {
                            assert_eq!(
                                args.get("expectedEpoch"),
                                self.original.params.get("expectedEpoch")
                            );
                            assert_eq!(
                                args.get("expectedOperationId"),
                                self.original.params.get("expectedOperationId")
                            );
                            assert_eq!(args.get("threadId"), self.original.params.get("threadId"));
                            self.dispatched.fetch_add(1, Ordering::Relaxed);
                            *self.remote.lock().unwrap() = Some(self.operation("accepted"));
                            Err(IvyError::new("transport_lost", "Lost original reply.")
                                .with_outcome(Outcome::Unknown))
                        }
                        other => panic!("Unexpected effect: {other}"),
                    }
                }
                other => panic!("Unexpected RPC: {other}"),
            }
        })
    }
}
struct Fixed;
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        1_800_000_000_000
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
async fn release_once(journal: &Journal, client: Arc<Client>) -> IvyResult<Document> {
    advance_once(journal, client, "question").await
}
async fn advance_once(journal: &Journal, client: Arc<Client>, name: &str) -> IvyResult<Document> {
    let tools = Arc::new(ToolClients::new(client.clone()));
    let store = Store {
        client: client.clone(),
        root: "root".into(),
        journal,
    };
    let settings =
        value(json!({"identity":{"principalId":"secretary-principal","hostId":"fixture-host"}}));
    let node = "fixture.secretary".into();
    let evidence = InboxEvidence {
        store: &store,
        settings: &settings,
        service_node_id: &node,
        cancellation: RequestOptions::default(),
    };
    let issues = RefCell::new(RecoveryScan::default());
    let engine = SecretaryEngine {
        evidence: &evidence,
        clock: &Fixed,
        operation_clock: &Fixed,
        runtime_epoch: ivy_sdk::operation_ids::start_runtime_epoch(client),
        new_uuid: crate::engine::random_uuid,
        tools: &tools,
        issues: &issues,
    };
    let retained =
        CheckedNativeContracts::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let worker = SecretaryFollowUp::new(&engine, tools.clone(), &retained);
    worker.advance(name).await?;
    Ok(journal.named(FOLLOW_UP_KEY, name, true)?.unwrap())
}
fn unchanged_answer(question: &Document) {
    assert!(field(&question.value, "phase").is_str("succeeded"));
    assert!(field(&question.value, "answer").is_str("Original answer"));
    assert!(field(&question.value, "errorCode").is_null());
}
#[test]
fn release_requires_a_proven_own_context_and_explicit_account() {
    let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
    let (context, question) = seed(&journal);
    for field_name in ["start", "createdFor", "target", "threadId"] {
        let mut changed = context.value.clone();
        changed.remove(field_name);
        assert!(
            plan(
                &context.pin,
                &changed,
                &question.value,
                &value(json!({"epoch":"epoch"}))
            )
            .unwrap()
            .is_none(),
            "{field_name}"
        );
    }
    let mut legacy = context.value.clone();
    legacy.get_mut("target").unwrap().remove("backend");
    assert!(
        plan(
            &context.pin,
            &legacy,
            &question.value,
            &value(json!({"epoch":"epoch"}))
        )
        .unwrap()
        .is_none()
    );
    let mut failed = question.value.clone();
    failed.insert("phase", "failed".into());
    assert!(
        plan(
            &context.pin,
            &context.value,
            &failed,
            &value(json!({"epoch":"epoch"}))
        )
        .unwrap()
        .is_none()
    );
}
#[tokio::test]
async fn accepted_questions_foreign_work_and_changed_contexts_prevent_dispatch() {
    for case in ["queued", "same-thread", "context-change", "deleted-context"] {
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        let (context, question) = seed(&journal);
        let client = Arc::new(Client::new(&question));
        assert!(pending_for_item(&journal, &"item".into(), "other").unwrap());
        match case {
            "queued" => {
                journal
                    .create_technical(
                        FOLLOW_UP_KEY,
                        "other",
                        &value(
                            json!({"request":{"itemId":"item"},"threadId":null,"phase":"prepared"}),
                        ),
                    )
                    .unwrap();
            }
            "same-thread" => {
                journal.create_technical(FOLLOW_UP_KEY,"other",&value(json!({"request":{"itemId":"different"},"threadId":"thread","phase":"running"}))).unwrap();
            }
            "context-change" => {
                let mut next = context.value.clone();
                next.insert("model", "changed".into());
                journal
                    .amend_technical(CONTEXT_KEY, &context.pin, &next)
                    .unwrap();
            }
            _ => {
                journal
                    .delete_technical(CONTEXT_KEY, &context.pin.object_id)
                    .unwrap();
            }
        }
        let saved = release_once(&journal, client.clone()).await.unwrap();
        assert!(
            field(field(&saved.value, "contextRelease"), "state").is_str("skipped"),
            "{case}"
        );
        unchanged_answer(&saved);
        assert!(client.calls.lock().unwrap().is_empty(), "{case}");
    }
}
struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Self {
        Self(
            std::env::temp_dir().join(format!("secretary-release-{}.sqlite", uuid::Uuid::new_v4())),
        )
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
#[tokio::test]
async fn lost_release_reply_reopens_and_observes_only_the_original() {
    let temp = Temporary::new();
    let journal = Journal::open(&temp.0, Limits::default()).unwrap();
    let (_, question) = seed(&journal);
    let client = Arc::new(Client::new(&question));
    journal
        .complete_technical_workflow(&question.pin.object_id, 0)
        .unwrap();
    journal
        .cleanup(0, journal.limits.count, None, None)
        .unwrap();
    assert!(
        journal
            .named(FOLLOW_UP_KEY, "question", true)
            .unwrap()
            .is_some()
    );
    assert!(journal.named(CONTEXT_KEY, "item", true).unwrap().is_some());
    let saved = release_once(&journal, client.clone()).await.unwrap();
    unchanged_answer(&saved);
    assert!(field(field(&saved.value, "contextRelease"), "state").is_str("observing"));
    let original_call = field(field(&saved.value, "contextRelease"), "call").clone();
    journal
        .create_technical(
            FOLLOW_UP_KEY,
            "next",
            &value(json!({"request":{"itemId":"item"},"phase":"prepared","threadId":null})),
        )
        .unwrap();
    let calls = client.calls.lock().unwrap().len();
    let pending = advance_once(&journal, client.clone(), "next")
        .await
        .unwrap_err();
    assert_eq!(pending.code, "secretary_follow_up_release_pending");
    assert_eq!(client.calls.lock().unwrap().len(), calls);
    drop(journal);
    *client.remote.lock().unwrap() = Some(client.operation("succeeded"));
    let journal = Journal::open(&temp.0, Limits::default()).unwrap();
    let saved = release_once(&journal, client.clone()).await.unwrap();
    unchanged_answer(&saved);
    assert!(field(field(&saved.value, "contextRelease"), "state").is_str("released"));
    assert_eq!(
        field(field(&saved.value, "contextRelease"), "call"),
        &original_call
    );
    release_once(&journal, client.clone()).await.unwrap();
    assert_eq!(client.dispatched.load(Ordering::Relaxed), 1);
    assert!(!pending_for_item(&journal, &"item".into(), "next").unwrap());
}
#[tokio::test]
async fn absence_after_an_uncertain_send_cannot_dispatch_again() {
    let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
    let (_, question) = seed(&journal);
    let client = Arc::new(Client::new(&question));
    release_once(&journal, client.clone()).await.unwrap();
    *client.remote.lock().unwrap() = Some(client.operation("outcome_unknown"));
    let error = release_once(&journal, client.clone()).await.unwrap_err();
    assert_eq!(error.code, "native_connection_lost");
    *client.remote.lock().unwrap() = None;
    let error = release_once(&journal, client.clone()).await.unwrap_err();
    assert_eq!(error.code, "secretary_follow_up_release_absent");
    let saved = journal
        .named(FOLLOW_UP_KEY, "question", true)
        .unwrap()
        .unwrap();
    unchanged_answer(&saved);
    let release = field(&saved.value, "contextRelease");
    assert!(field(release, "state").is_str("outcome_unknown"));
    assert!(field(release, "errorCode").is_str("secretary_follow_up_release_absent"));
    journal
        .complete_technical_workflow(&question.pin.object_id, 0)
        .unwrap();
    journal
        .cleanup(0, journal.limits.count, None, None)
        .unwrap();
    assert!(
        journal
            .named(FOLLOW_UP_KEY, "question", true)
            .unwrap()
            .is_some()
    );
    assert!(pending_for_item(&journal, &"item".into(), "next").unwrap());
    assert_eq!(client.dispatched.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn the_final_guard_sees_a_newly_accepted_question() {
    let temp = Temporary::new();
    let journal = Journal::open(&temp.0, Limits::default()).unwrap();
    let (_, question) = seed(&journal);
    let mut client = Client::new(&question);
    client.inject = Some(temp.0.clone());
    let client = Arc::new(client);
    release_once(&journal, client.clone()).await.unwrap();
    assert_eq!(client.dispatched.load(Ordering::Relaxed), 0);
    let saved = journal
        .named(FOLLOW_UP_KEY, "question", true)
        .unwrap()
        .unwrap();
    unchanged_answer(&saved);
    assert!(field(field(&saved.value, "contextRelease"), "state").is_str("blocked"));
}
