use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_task_board::{
    native_intent::EvidenceIdentity,
    native_lifecycle::{LifecycleDocument, LifecyclePublication, LifecycleStore},
    native_publication::*,
    operations::{Document, Store},
};
use std::{
    cell::{Cell, RefCell},
    sync::{Arc, OnceLock},
};
#[path = "task-board-native-adoption.rs"]
mod adoption;
#[path = "task-board-native-publication-journal.rs"]
mod journal;

const AT: &str = "2026-01-02T03:04:05.006Z";
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn id(n: u32) -> JsString {
    format!("00000000-0000-4000-8000-{n:012}").into()
}
fn pin(n: u32) -> Json {
    Json::object([
        ("objectId", Json::String(id(n))),
        ("revision", Json::Number(1.0)),
    ])
}
fn actor(source: &str) -> Json {
    Json::object([
        ("principalId", "board".into()),
        ("serviceNodeId", "board-node".into()),
        ("source", source.into()),
        ("generation", Json::Number(1.0)),
    ])
}
fn primary() -> Json {
    Json::object([
        ("serviceNodeId", "agent".into()),
        ("namespace", "codex".into()),
        ("kind", "thread".into()),
        ("nativeId", "thread".into()),
    ])
}
fn doc(pin: Json, value: Json) -> Document {
    Document {
        pin,
        content_hash: hash_json(&value).unwrap(),
        value,
        parent_id: Some("root".into()),
    }
}
fn fields() -> Json {
    Json::object([
        ("title", "Task".into()),
        ("description", "Work".into()),
        ("acceptanceCriteria", Json::Array(vec![])),
        ("control", "agent".into()),
        ("userContact", "ticket".into()),
        (
            "workspaceRequirement",
            Json::object([("kind", "isolated".into())]),
        ),
    ])
}
fn base_task() -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskKey", "TASK-1".into()),
        ("fields", fields()),
        ("workflowState", "in_progress".into()),
        ("workRevision", Json::Number(3.0)),
        (
            "claim",
            Json::object([("run", pin(2)), ("phase", "publishing_result".into())]),
        ),
        ("publication", Json::Null),
        ("runtimeWait", Json::Null),
        ("primaryResourceRef", primary()),
        ("lastRun", pin(2)),
        ("latestResult", pin(901)),
        ("acceptedReview", pin(902)),
        ("lastHistory", Json::Null),
        ("parentTaskId", Json::Null),
        ("comments", Json::Array(vec![])),
        ("attachments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        ("agentCommentCount", Json::Number(0.0)),
        ("openQuestionCount", Json::Number(0.0)),
        ("dependencies", Json::Array(vec![])),
        (
            "observedDependencies",
            Json::object([] as [(&str, Json); 0]),
        ),
        ("wakeAt", Json::Null),
        ("updatedAt", AT.into()),
    ])
}
fn base_run() -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskId", Json::String(id(1))),
        ("operationId", "start-original".into()),
        ("owner", actor("scheduler")),
        ("phase", "publishing_result".into()),
        ("primaryResourceRef", primary()),
        ("turnId", "turn".into()),
        ("nativeEpoch", "epoch".into()),
        ("cancellation", Json::Null),
        ("notificationCursor", Json::Number(0.0)),
        (
            "target",
            Json::object([("hostId", "host".into()), ("serviceNodeId", "agent".into())]),
        ),
        (
            "workspace",
            Json::object([
                ("hostId", "host".into()),
                ("serviceNodeId", "agent".into()),
                ("intendedPath", "/workspace".into()),
                ("project", Json::Null),
                ("repository", Json::Null),
            ]),
        ),
        (
            "calls",
            Json::object([
                ("thread", Json::Null),
                ("turn", Json::Null),
                ("interrupt", Json::Null),
            ]),
        ),
        (
            "originalRequest",
            Json::object([
                ("action", "continue".into()),
                ("operationId", "start-original".into()),
                ("taskId", Json::String(id(1))),
                ("expectedRevision", Json::Number(1.0)),
                ("intent", pin(20)),
                ("commentIds", Json::Array(vec![])),
            ]),
        ),
        (
            "executionContext",
            Json::object([
                ("workflowState", "todo".into()),
                ("latestResult", pin(901)),
                ("acceptedReview", pin(902)),
            ]),
        ),
        (
            "externalOutcome",
            Json::object([
                ("state", "active".into()),
                ("code", Json::Null),
                ("evidence", Json::Null),
            ]),
        ),
        ("updatedAt", AT.into()),
    ])
}
fn comment(id: &str, run: Json, purpose: &str) -> Json {
    Json::object([
        ("commentId", id.into()),
        ("sequence", Json::Number(1.0)),
        ("author", actor("worker")),
        ("authorKind", "agent".into()),
        ("purpose", purpose.into()),
        ("body", "Saved handoff".into()),
        ("requests", Json::Array(vec![])),
        ("responses", Json::Array(vec![])),
        ("delivery", Json::Null),
        ("attachments", Json::Array(vec![])),
        ("run", run),
        ("turnId", "turn".into()),
        ("replyTo", Json::Null),
        ("createdAt", AT.into()),
        ("operationId", id.into()),
    ])
}
fn handoff(state: &str) -> Json {
    let mut c = comment("handoff", pin(2), "handoff");
    c.insert("handoff", Json::object([("workflowState", state.into())]));
    c
}
fn proof(n: u32) -> Json {
    Json::object([
        ("object", pin(n)),
        ("contentHash", format!("sha256:{}", "a".repeat(64)).into()),
        ("mediaType", "application/json".into()),
        ("label", "Owner evidence".into()),
    ])
}
fn set(v: &mut Json, parent: &str, key: &str, value: Json) {
    let mut p = f(v, parent).clone();
    p.insert(key, value);
    v.insert(parent, p);
}

struct World {
    documents: RefCell<Vec<(String, Document)>>,
    writes: RefCell<Vec<(String, Json, Json)>>,
    creates: RefCell<Vec<(String, Json)>>,
    calls: RefCell<Vec<(Json, Json)>>,
    pending: Cell<usize>,
    failure: RefCell<Option<Json>>,
    workspace_reads: Cell<usize>,
    cleanup: Cell<usize>,
    released: Cell<usize>,
    snapshot: RefCell<Json>,
}
impl World {
    fn new(task: Json, run: Json) -> Self {
        Self {
            documents: RefCell::new(vec![
                ("task-board/task".into(), doc(pin(1), task)),
                ("task-board/run".into(), doc(pin(2), run)),
                (
                    "task-board/native-plan".into(),
                    doc(pin(20), Json::object([("nativeVersion", "native".into())])),
                ),
            ]),
            writes: RefCell::new(vec![]),
            creates: RefCell::new(vec![]),
            calls: RefCell::new(vec![]),
            pending: Cell::new(0),
            failure: RefCell::new(None),
            workspace_reads: Cell::new(0),
            cleanup: Cell::new(0),
            released: Cell::new(0),
            snapshot: RefCell::new(Json::object([
                (
                    "document",
                    Json::object([(
                        "value",
                        Json::object([("status", "completed".into()), ("epoch", "epoch".into())]),
                    )]),
                ),
                (
                    "thread",
                    Json::object([(
                        "status",
                        Json::object([
                            ("type", "active".into()),
                            ("activeFlags", Json::Array(vec![])),
                        ]),
                    )]),
                ),
                (
                    "turn",
                    Json::object([
                        ("id", "turn".into()),
                        ("status", "completed".into()),
                        ("items", Json::Array(vec![])),
                        ("error", Json::Null),
                    ]),
                ),
            ])),
        }
    }
    fn lookup(&self, key: &str, address: &Json) -> IvyResult<Document> {
        let object_id = address
            .as_js_string()
            .map(|v| Json::String(v.clone()))
            .unwrap_or_else(|| f(address, "objectId").clone());
        self.documents
            .borrow()
            .iter()
            .rev()
            .find(|(k, d)| {
                *k == key
                    && f(&d.pin, "objectId") == &object_id
                    && (address.as_js_string().is_some()
                        || f(&d.pin, "revision") == f(address, "revision"))
            })
            .map(|(_, d)| d.clone())
            .ok_or_else(|| IvyError::new("not_found", "Finite retained document unavailable."))
    }
    fn put(&self, key: &str, p: Json, value: Json) {
        self.documents
            .borrow_mut()
            .push((key.into(), doc(p, value)));
    }
    fn final_task(&self) -> Json {
        self.lookup("task-board/task", &Json::String(id(1)))
            .unwrap()
            .value
    }
    fn saved_run(&self) -> Json {
        self.lookup("task-board/run", &Json::String(id(2)))
            .unwrap()
            .value
    }
    fn result(&self) -> Json {
        self.creates
            .borrow()
            .iter()
            .find(|(k, _)| *k == "task-board/result")
            .unwrap()
            .1
            .clone()
    }
    fn status(&self, status: &str, epoch: &str, error: Json) {
        let mut snapshot = self.snapshot.borrow_mut();
        set(
            &mut snapshot,
            "document",
            "value",
            Json::object([("status", status.into()), ("epoch", epoch.into())]),
        );
        set(&mut snapshot, "turn", "status", status.into());
        set(&mut snapshot, "turn", "error", error);
    }
    fn observe(&self, call_pin: Json, call: Json, observation: Json) {
        self.put("task-board/native-call", call_pin, call.clone());
        self.calls
            .borrow_mut()
            .push((f(f(&call, "evidence"), "object").clone(), observation));
    }
    async fn publish(&self, change: Json) -> IvyResult<Json> {
        let op = doc(
            pin(3),
            Json::object([
                (
                    "request",
                    Json::object([
                        ("action", "nativeUpdate".into()),
                        ("operationId", "publish-original".into()),
                        ("taskId", Json::String(id(1))),
                        ("expectedRevision", Json::Number(1.0)),
                        ("run", pin(2)),
                        ("change", change),
                    ]),
                ),
                ("actor", actor("native")),
                ("callerPrincipalId", "board".into()),
                ("createdAt", AT.into()),
                ("writes", Json::Array(vec![])),
            ]),
        );
        NativePublisher {
            store: self,
            engine: self,
            evidence: self,
        }
        .publish(&op, &mut Steps(self), None)
        .await
    }
    async fn complete(&self) -> IvyResult<Json> {
        self.publish(Json::object([
            ("kind", "completion".into()),
            ("snapshot", proof(30)),
            ("output", proof(31)),
            ("notificationCursor", Json::Number(0.0)),
        ]))
        .await
    }
}
impl Store for World {
    fn root_object_id(&self) -> Option<JsString> {
        Some("root".into())
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        Ok(false)
    }
    fn local_key(&self, object_id: &JsString) -> IvyResult<Option<String>> {
        Ok(if object_id == &id(2) {
            Some("task-board/run".into())
        } else if object_id == &id(20) {
            Some("task-board/native-plan".into())
        } else {
            None
        })
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("Use retained completion dependencies")
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        panic!("No native dispatch")
    }
    async fn read(&self, key: &str, address: &Json, _: bool) -> IvyResult<Document> {
        self.lookup(key, address)
    }
    async fn write(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Use retained steps")
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("Use retained steps")
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        panic!("Use retained steps")
    }
}
impl LifecycleStore for World {
    async fn lifecycle_read(
        &self,
        key: &str,
        address: &Json,
        _: bool,
        _: bool,
    ) -> IvyResult<LifecycleDocument> {
        let d = self.lookup(key, address)?;
        let current = self.lookup(key, f(&d.pin, "objectId"))?;
        Ok(LifecycleDocument {
            metadata: Json::object([("currentRevision", f(&current.pin, "revision").clone())]),
            document: d,
        })
    }
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("No unrelated scan")
    }
    async fn archive(&self, _: &Json) -> IvyResult<Json> {
        panic!("No native effect")
    }
    async fn stat(&self, _: &JsString) -> IvyResult<Json> {
        panic!("No native effect")
    }
    fn release_workspace(&self, _: &JsString) -> IvyResult<()> {
        self.released.set(self.released.get() + 1);
        Ok(())
    }
}
impl NativePublicationStore for World {
    fn pending_handoffs(&self, _: &JsString) -> IvyResult<usize> {
        if self.pending.get() > 16 {
            Err(IvyError::new(
                "task_board_handoff_limit",
                "Too many pending handoffs.",
            ))
        } else {
            Ok(self.pending.get())
        }
    }
    fn failed_handoff(&self, _: &JsString) -> IvyResult<Option<Json>> {
        Ok(self.failure.borrow().clone())
    }
    fn complete_after_publication(
        &self,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> IvyResult<()> {
        self.cleanup.set(self.cleanup.get() + 1);
        Ok(())
    }
}
impl NativePublicationPolicies for World {
    fn now(&self) -> JsString {
        AT.into()
    }
    async fn aggregate(&self, _: &Json, _: &JsString) -> IvyResult<()> {
        Ok(())
    }
}
impl NativePublicationEngine for World {
    fn principal_id(&self) -> &JsString {
        static P: OnceLock<JsString> = OnceLock::new();
        P.get_or_init(|| "board".into())
    }
    fn phone_target(&self) -> &Json {
        static NONE: Json = Json::Null;
        &NONE
    }
    fn retry_at(&self, t: &Json, at: &JsString) -> IvyResult<Json> {
        Ok(
            ivy_task_board::native_rate_limit::native_rate_limit_retry_at(t, at)?
                .map(Json::String)
                .unwrap_or(Json::Null),
        )
    }
    async fn workspace(&self, _: &Json, run: &Json) -> IvyResult<Json> {
        self.workspace_reads.set(self.workspace_reads.get() + 1);
        let mut w = f(run, "workspace").clone();
        w.insert("nativeProjectId", "unpublished-native-id".into());
        Ok(w)
    }
}
impl NativePublicationEvidence for World {
    fn adoption(
        &self,
        task: &Json,
        previous: &Json,
        metadata: &Json,
        turns: &Json,
    ) -> IvyResult<Json> {
        adoption::verify(self, task, previous, metadata, turns)
    }
    async fn snapshot(&self, _: &Json, _: &Json) -> IvyResult<Json> {
        Ok(self.snapshot.borrow().clone())
    }
    async fn completion(&self, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        let snapshot = self.snapshot.borrow().clone();
        Ok(Json::object([
            ("status", f(f(&snapshot, "turn"), "status").clone()),
            ("snapshot", snapshot),
            ("summary", "Verified newest answer".into()),
        ]))
    }
    fn identity(
        &self,
        run: &Json,
        call: &Json,
        principal: &JsString,
    ) -> IvyResult<EvidenceIdentity> {
        Ok(EvidenceIdentity {
            caller_principal_id: principal.clone(),
            service_node_id: s(f(run, "target"), "serviceNodeId").clone(),
            native_version: "native".into(),
            operation_id: s(call, "operationId").clone(),
            method: s(f(call, "request"), "method").clone(),
            params: f(f(call, "request"), "params").clone(),
        })
    }
    async fn read(&self, _: &Json, artifact: &Json, _: &EvidenceIdentity) -> IvyResult<Json> {
        self.calls
            .borrow()
            .iter()
            .find(|(p, _)| p == f(artifact, "object"))
            .map(|(_, o)| o.clone())
            .ok_or_else(|| IvyError::new("not_found", "Saved owner proof unavailable."))
    }
    async fn intended(&self, _: &Json, origin: &Json) -> IvyResult<Json> {
        let documents = self.documents.borrow();
        let call = &documents
            .iter()
            .rev()
            .find(|(key, document)| {
                key == "task-board/native-call" && f(&document.value, "origin") == origin
            })
            .ok_or_else(IvyError::internal)?
            .1
            .value;
        Ok(Json::object([
            ("request", f(call, "request").clone()),
            (
                "expectedDefinitionHash",
                f(call, "expectedDefinitionHash").clone(),
            ),
        ]))
    }
    fn resolve(&self, r: &Json) -> IvyResult<Json> {
        Ok(r.clone())
    }
}
struct Steps<'a>(&'a World);
impl LifecyclePublication for Steps<'_> {
    fn pin(&self) -> IvyResult<Json> {
        Ok(pin(3))
    }
    async fn write(&mut self, key: &str, value: &Json, destination: &Json) -> IvyResult<Json> {
        let current = self.0.lookup(key, f(destination, "objectId"))?;
        assert_eq!(
            f(&current.pin, "revision"),
            f(destination, "expectedRevision")
        );
        let p = Json::object([
            ("objectId", f(destination, "objectId").clone()),
            (
                "revision",
                Json::Number(f(destination, "expectedRevision").as_f64().unwrap() + 1.0),
            ),
        ]);
        self.0
            .writes
            .borrow_mut()
            .push((key.into(), value.clone(), destination.clone()));
        self.0.put(key, p.clone(), value.clone());
        Ok(p)
    }
    async fn create(&mut self, key: &str, value: &Json, _: &JsString) -> IvyResult<Json> {
        let p = pin(80 + self.0.creates.borrow().len() as u32);
        self.0
            .creates
            .borrow_mut()
            .push((key.into(), value.clone()));
        self.0.put(key, p.clone(), value.clone());
        Ok(p)
    }
}

#[tokio::test]
async fn replies_restore_prior_workflow_result_and_acceptance_without_finishing_the_ticket() {
    for state in [
        "review",
        "waiting",
        "backlog",
        "done",
        "cancelled",
        "in_progress",
        "todo",
    ] {
        let mut run = base_run();
        set(&mut run, "executionContext", "workflowState", state.into());
        let mut t = base_task();
        t.insert("wakeAt", "2026-02-01T00:00:00.000Z".into());
        t.insert("dependencies", Json::Array(vec!["peer".into()]));
        let w = World::new(t, run);
        w.complete().await.unwrap();
        let t = w.final_task();
        assert_eq!(
            f(&t, "workflowState"),
            &Json::from(if state == "in_progress" {
                "todo"
            } else {
                state
            })
        );
        assert_eq!(
            f(&t, "acceptedReview"),
            &if state == "todo" {
                Json::Null
            } else {
                pin(902)
            }
        );
        assert_eq!(
            f(&t, "latestResult"),
            &if state == "todo" {
                f(&w.saved_run(), "result").clone()
            } else {
                pin(901)
            }
        );
        assert_eq!(f(&w.result(), "kind"), &Json::from("intermediate"));
        assert!(f(&w.result(), "content").get("checks").is_none());
        assert_eq!(f(&t, "wakeAt"), &Json::from("2026-02-01T00:00:00.000Z"));
        assert_eq!(f(&t, "dependencies"), &Json::Array(vec!["peer".into()]));
        assert_eq!(w.workspace_reads.get(), 0);
        assert_eq!(w.cleanup.get(), 2);
        assert!(f(&t, "claim").is_null());
        assert!(t.get("waiting").is_none());
    }
}
#[tokio::test]
async fn only_successful_review_handoffs_make_final_results_and_root_notifications() {
    for state in ["todo", "waiting", "review"] {
        for child in [false, true] {
            let mut t = base_task();
            t.insert("comments", Json::Array(vec![handoff(state)]));
            set(&mut t, "fields", "userContact", "chat".into());
            if child {
                t.insert("parentTaskId", "parent".into());
            }
            let w = World::new(t, base_run());
            w.complete().await.unwrap();
            let t = w.final_task();
            let ready = state == "review";
            assert_eq!(f(&t, "workflowState"), &Json::from(state));
            assert_eq!(
                f(&w.result(), "kind"),
                &Json::from(if ready { "final" } else { "intermediate" })
            );
            assert_eq!(
                f(&t, "workRevision"),
                &Json::Number(if ready { 4.0 } else { 3.0 })
            );
            assert!(f(&t, "acceptedReview").is_null());
            assert_eq!(w.workspace_reads.get(), usize::from(ready));
            assert_eq!(
                w.creates
                    .borrow()
                    .iter()
                    .filter(|(k, _)| *k == "task-board/delivery")
                    .count(),
                usize::from(ready && !child)
            );
            assert!(
                f(&w.saved_run(), "workspace")
                    .get("nativeProjectId")
                    .is_none()
            );
        }
    }
}
#[tokio::test]
async fn direct_native_starts_default_to_review_but_terminal_replies_do_not_reopen() {
    for state in ["todo", "done", "cancelled"] {
        let mut run = base_run();
        run.insert("owner", actor("native"));
        set(&mut run, "executionContext", "workflowState", state.into());
        let w = World::new(base_task(), run);
        w.complete().await.unwrap();
        assert_eq!(
            f(&w.final_task(), "workflowState"),
            &Json::from(if state == "todo" { "review" } else { state })
        );
    }
}
#[tokio::test]
async fn execution_option_changes_suppress_stale_handoff_intent_and_retry_the_original_input() {
    let mut t = base_task();
    t.insert("executionChangeRequested", true.into());
    t.insert("comments", Json::Array(vec![handoff("review")]));
    let mut run = base_run();
    set(
        &mut run,
        "executionContext",
        "workflowState",
        "waiting".into(),
    );
    let w = World::new(t, run);
    w.status(
        "failed",
        "epoch",
        Json::object([("message", "Original failure".into())]),
    );
    w.complete().await.unwrap();
    let t = w.final_task();
    assert_eq!(f(&t, "workflowState"), &Json::from("waiting"));
    assert_eq!(
        f(f(&t, "completionIssue"), "retryAt"),
        &Json::from("2026-01-02T03:04:35.006Z")
    );
    assert_eq!(
        f(f(&w.result(), "content"), "summary"),
        &Json::from("Original failure")
    );
    assert_eq!(w.workspace_reads.get(), 0);
    assert_eq!(w.cleanup.get(), 0);
}
#[tokio::test]
async fn retry_preserves_origin_marker_and_requeues_only_the_original_inputs() {
    let mut original = base_task();
    original.insert("executionChangeRequested", true.into());
    let mut t = original.clone();
    t.remove("executionChangeRequested");
    t.insert(
        "commentDeliveries",
        Json::Array(
            ["old", "new"]
                .map(|id| {
                    Json::object([
                        ("commentId", id.into()),
                        ("state", "delivered".into()),
                        ("updatedAt", AT.into()),
                        ("code", Json::Null),
                    ])
                })
                .to_vec(),
        ),
    );
    let mut run = base_run();
    set(&mut run, "executionContext", "workflowState", "done".into());
    set(
        &mut run,
        "originalRequest",
        "commentIds",
        Json::Array(vec!["old".into()]),
    );
    let w = World::new(original, run);
    let mut p = pin(1);
    p.insert("revision", Json::Number(3.0));
    w.put("task-board/task", p, t);
    w.status(
        "failed",
        "epoch",
        Json::object([
            ("message", "Capacity".into()),
            ("codexErrorInfo", "serverOverloaded".into()),
        ]),
    );
    w.complete().await.unwrap();
    let t = w.final_task();
    assert_eq!(f(&t, "workflowState"), &Json::from("done"));
    assert_eq!(f(&t, "executionChangeRequested"), &Json::Bool(true));
    assert_eq!(f(f(&t, "runtimeWait"), "reason"), &Json::from("host"));
    let d = f(&t, "commentDeliveries").as_array().unwrap();
    assert_eq!(f(&d[0], "state"), &Json::from("queued"));
    assert_eq!(f(&d[1], "state"), &Json::from("delivered"));
    assert_eq!(w.cleanup.get(), 0);
    assert_eq!(
        f(&w.writes.borrow()[0].2, "expectedRevision"),
        &Json::Number(3.0)
    );
}
#[tokio::test]
async fn a_saved_reply_avoids_duplicate_result_comments_and_questions_remain_open() {
    let mut t = base_task();
    let mut answer = comment("answer", pin(2), "update");
    answer.insert("replyTo", "input".into());
    let mut question = comment("question", pin(2), "question");
    question.insert("recipient", Json::object([("kind", "user".into())]));
    t.insert("comments", Json::Array(vec![question, answer]));
    t.insert("agentCommentCount", Json::Number(2.0));
    let mut run = base_run();
    set(
        &mut run,
        "originalRequest",
        "commentIds",
        Json::Array(vec!["input".into()]),
    );
    let w = World::new(t, run);
    w.complete().await.unwrap();
    let t = w.final_task();
    assert_eq!(f(&t, "comments").as_array().unwrap().len(), 2);
    assert_eq!(f(&t, "openQuestionCount"), &Json::Number(1.0));
}
#[tokio::test]
async fn cancellation_and_publication_failure_never_publish_success_or_release_failed_evidence() {
    let mut run = base_run();
    run.insert("cancellation", Json::object([("reason", "cancel".into())]));
    let mut t = base_task();
    t.insert("executionChangeRequested", true.into());
    let w = World::new(t, run);
    w.status("interrupted", "replacement", Json::Null);
    w.complete().await.unwrap();
    let t = w.final_task();
    assert_eq!(f(&t, "workflowState"), &Json::from("cancelled"));
    assert!(t.get("executionChangeRequested").is_none());
    assert_eq!(w.released.get(), 1);
    assert!(f(&t, "comments").as_array().unwrap().is_empty());
    let w = World::new(base_task(), base_run());
    *w.failure.borrow_mut() = Some(Json::object([
        ("code", "storage_full".into()),
        ("detail", "Handoff could not be saved".into()),
        ("operationId", "handoff-original".into()),
    ]));
    w.complete().await.unwrap();
    let t = w.final_task();
    assert_eq!(
        f(f(&t, "completionIssue"), "code"),
        &Json::from("storage_full")
    );
    assert!(f(f(&t, "completionIssue"), "retryAt").is_null());
    assert_eq!(w.cleanup.get(), 0);
}
#[tokio::test]
async fn pending_handoffs_and_stale_run_pins_fence_every_publication_write() {
    for pending in [1, 17] {
        let w = World::new(base_task(), base_run());
        w.pending.set(pending);
        assert_eq!(
            w.complete().await.unwrap_err().code,
            if pending == 1 {
                "task_board_handoff_pending"
            } else {
                "task_board_handoff_limit"
            }
        );
        assert!(w.writes.borrow().is_empty());
        assert!(w.creates.borrow().is_empty());
    }
    let w = World::new(base_task(), base_run());
    let mut p = pin(2);
    p.insert("revision", Json::Number(2.0));
    w.put("task-board/run", p, base_run());
    assert_eq!(w.complete().await.unwrap_err().code, "revision_conflict");
    assert!(w.writes.borrow().is_empty());
}
#[tokio::test]
async fn replacement_owner_interruptions_retry_but_permanent_and_same_owner_failures_do_not() {
    for (status, epoch, code, retry) in [
        ("interrupted", "replacement", "native_interrupted", true),
        ("interrupted", "epoch", "native_interrupted", false),
        ("failed", "epoch", "native_failed", false),
    ] {
        let w = World::new(base_task(), base_run());
        w.status(status, epoch, Json::Null);
        w.complete().await.unwrap();
        let t = w.final_task();
        assert_eq!(f(f(&t, "completionIssue"), "code"), &Json::from(code));
        assert_eq!(!f(f(&t, "completionIssue"), "retryAt").is_null(), retry);
        assert_eq!(f(&w.result(), "kind"), &Json::from("intermediate"));
    }
    let mut run = base_run();
    run.insert(
        "externalOutcome",
        Json::object([
            ("state", "failed".into()),
            ("code", "native_interrupted".into()),
        ]),
    );
    let w = World::new(base_task(), run);
    w.status(
        "failed",
        "epoch",
        Json::object([("message", "Actual native error".into())]),
    );
    w.complete().await.unwrap();
    assert_eq!(
        f(f(&w.final_task(), "completionIssue"), "code"),
        &Json::from("native_failed")
    );
}

fn native_call(number: u32, method: &str, origin: Json, phase: Option<&str>) -> Json {
    Json::object([
        ("operationId", format!("native-{number}").into()),
        ("preparedAt", AT.into()),
        ("origin", origin),
        (
            "request",
            Json::object([
                ("nativeVersion", "native".into()),
                ("method", method.into()),
                ("params", Json::object([("threadId", "thread".into())])),
            ]),
        ),
        ("expectedDefinitionHash", "hash".into()),
        (
            "state",
            if phase.is_some() {
                "observed".into()
            } else {
                "prepared".into()
            },
        ),
        ("ownerPhase", phase.map(Json::from).unwrap_or(Json::Null)),
        (
            "epoch",
            phase.map(|_| Json::from("epoch")).unwrap_or(Json::Null),
        ),
        ("code", Json::Null),
        (
            "observedAt",
            phase.map(|_| Json::from(AT)).unwrap_or(Json::Null),
        ),
        (
            "evidence",
            phase.map(|_| proof(number + 100)).unwrap_or(Json::Null),
        ),
    ])
}
fn owner(phase: &str, result: Json) -> Json {
    Json::object([
        ("phase", phase.into()),
        ("epoch", "epoch".into()),
        ("code", Json::Null),
        ("reply", Json::object([("result", result)])),
    ])
}

#[tokio::test]
async fn reattachment_preserves_the_running_turn_and_failed_resume_never_finishes_it() {
    let mut run = base_run();
    set(&mut run, "calls", "thread", pin(10));
    run.insert("phase", "running".into());
    let w = World::new(base_task(), run);
    w.observe(
        pin(10),
        native_call(10, "thread/resume", Json::Null, Some("succeeded")),
        owner("succeeded", Json::object([] as [(&str, Json); 0])),
    );
    let origin = Json::object([
        ("kind", "reattach".into()),
        ("predecessor", pin(10)),
        ("preparedEpoch", "epoch".into()),
    ]);
    w.put(
        "task-board/native-call",
        pin(11),
        native_call(11, "thread/resume", origin, None),
    );
    w.publish(Json::object([
        ("kind", "reattachCall".into()),
        ("call", pin(11)),
    ]))
    .await
    .unwrap();
    assert_eq!(f(&w.saved_run(), "turnId"), &Json::from("turn"));
    assert_eq!(f(&w.saved_run(), "phase"), &Json::from("running"));
    assert!(w.creates.borrow().is_empty());

    let mut run = base_run();
    set(&mut run, "calls", "thread", pin(10));
    let w = World::new(base_task(), run);
    let original = native_call(10, "thread/resume", Json::Null, Some("outcome_unknown"));
    w.put("task-board/native-call", pin(10), original.clone());
    let mut observed = original;
    observed.insert("ownerPhase", "failed".into());
    observed.insert("evidence", proof(110));
    let mut p = pin(10);
    p.insert("revision", Json::Number(2.0));
    w.observe(p.clone(), observed, owner("failed", Json::Null));
    w.publish(Json::object([
        ("kind", "observeCall".into()),
        ("slot", "thread".into()),
        ("call", p),
    ]))
    .await
    .unwrap();
    assert_eq!(f(&w.saved_run(), "phase"), &Json::from("outcome_unknown"));
    assert_eq!(
        f(f(&w.final_task(), "runtimeWait"), "reason"),
        &Json::from("external_outcome")
    );
    assert!(
        w.creates
            .borrow()
            .iter()
            .all(|(k, _)| k != "task-board/result")
    );
}

#[tokio::test]
async fn steering_acknowledges_only_captured_input_and_preserves_a_newer_appointment() {
    for newer in [false, true] {
        let wake = if newer {
            "2026-03-01T00:00:00.000Z"
        } else {
            "2026-02-01T00:00:00.000Z"
        };
        let mut t = base_task();
        t.insert("wakeAt", wake.into());
        t.insert("workRevision", Json::Number(8.0));
        t.insert(
            "commentDeliveries",
            Json::Array(
                ["captured", "new"]
                    .map(|id| {
                        Json::object([
                            ("commentId", id.into()),
                            ("state", "queued".into()),
                            ("code", Json::Null),
                        ])
                    })
                    .to_vec(),
            ),
        );
        let mut run = base_run();
        set(&mut run, "calls", "steer", pin(10));
        run.insert("phase", "waiting_input".into());
        let origin = Json::object([
            ("kind", "steer".into()),
            ("predecessor", Json::Null),
            ("commentIds", Json::Array(vec!["captured".into()])),
            ("workRevision", Json::Number(4.0)),
            ("wakeAt", "2026-02-01T00:00:00.000Z".into()),
            (
                "observedDependencies",
                Json::object([("peer", "captured-hash".into())]),
            ),
        ]);
        let original = native_call(10, "turn/steer", origin, Some("outcome_unknown"));
        let w = World::new(t, run);
        w.put("task-board/native-call", pin(10), original.clone());
        let mut call = original;
        call.insert("ownerPhase", "succeeded".into());
        call.insert("evidence", proof(110));
        let mut p = pin(10);
        p.insert("revision", Json::Number(2.0));
        w.observe(
            p.clone(),
            call,
            owner("succeeded", Json::object([("turnId", "turn".into())])),
        );
        w.publish(Json::object([
            ("kind", "observeCall".into()),
            ("slot", "steer".into()),
            ("call", p),
        ]))
        .await
        .unwrap();
        let t = w.final_task();
        assert_eq!(f(&t, "workflowState"), &Json::from("waiting"));
        assert_eq!(
            f(&t, "wakeAt"),
            &if newer { wake.into() } else { Json::Null }
        );
        assert_eq!(
            f(f(&t, "observedDependencies"), "peer"),
            &Json::from("captured-hash")
        );
        let d = f(&t, "commentDeliveries").as_array().unwrap();
        assert_eq!(f(&d[0], "state"), &Json::from("delivered"));
        assert_eq!(f(&d[1], "state"), &Json::from("queued"));
    }
}

#[tokio::test]
async fn waiting_for_native_input_keeps_runtime_wait_empty_without_a_user_wait() {
    let w = World::new(base_task(), base_run());
    w.status("inProgress", "replacement", Json::Null);
    set(
        &mut w.snapshot.borrow_mut(),
        "thread",
        "status",
        Json::object([
            ("type", "active".into()),
            ("activeFlags", Json::Array(vec!["waitingOnApproval".into()])),
        ]),
    );
    w.publish(Json::object([
        ("kind", "readSnapshot".into()),
        ("snapshot", proof(30)),
        ("notificationCursor", Json::Number(0.0)),
    ]))
    .await
    .unwrap();
    let t = w.final_task();
    assert_eq!(f(&t, "workflowState"), &Json::from("waiting"));
    assert!(f(&t, "runtimeWait").is_null());
    assert_eq!(f(&w.saved_run(), "phase"), &Json::from("waiting_input"));
    assert_eq!(w.cleanup.get(), 0);
}

#[tokio::test]
async fn observed_calls_keep_original_identity_and_require_exact_conclusive_owner_proof() {
    for (key, value, code) in [
        (
            "operationId",
            "different".into(),
            "task_board_native_intent_mismatch",
        ),
        (
            "preparedAt",
            "2027-01-01T00:00:00.000Z".into(),
            "task_board_native_intent_mismatch",
        ),
        (
            "request",
            Json::object([("method", "other".into())]),
            "task_board_native_intent_mismatch",
        ),
        (
            "origin",
            Json::object([("kind", "other".into())]),
            "task_board_native_intent_mismatch",
        ),
        ("epoch", "different".into(), "task_board_evidence_mismatch"),
        ("code", "different".into(), "task_board_evidence_mismatch"),
        (
            "ownerPhase",
            "failed".into(),
            "task_board_evidence_mismatch",
        ),
        (
            "evidence",
            Json::Null,
            "task_board_native_evidence_required",
        ),
        (
            "state",
            "prepared".into(),
            "task_board_native_evidence_required",
        ),
    ] {
        let mut run = base_run();
        set(&mut run, "calls", "turn", pin(10));
        let w = World::new(base_task(), run);
        let original = native_call(10, "turn/start", Json::Null, Some("outcome_unknown"));
        w.put("task-board/native-call", pin(10), original.clone());
        let mut observed = original;
        observed.insert("ownerPhase", "succeeded".into());
        observed.insert("evidence", proof(110));
        observed.insert(key, value);
        let mut p = pin(10);
        p.insert("revision", Json::Number(2.0));
        w.observe(
            p.clone(),
            observed,
            owner(
                "succeeded",
                Json::object([("turn", Json::object([("id", "turn".into())]))]),
            ),
        );
        let error = w
            .publish(Json::object([
                ("kind", "observeCall".into()),
                ("slot", "turn".into()),
                ("call", p),
            ]))
            .await
            .unwrap_err();
        assert_eq!(error.code, code, "{key}");
        assert!(w.writes.borrow().is_empty());
        assert!(w.creates.borrow().is_empty());
    }
}

#[tokio::test]
async fn terminal_publication_waits_for_steering_and_keeps_reassigned_task_results() {
    for phase in [
        None,
        Some("outcome_unknown"),
        Some("succeeded"),
        Some("failed"),
    ] {
        let mut t = base_task();
        t.insert("claim", Json::Null);
        t.insert("lastRun", pin(999));
        t.insert("workflowState", "review".into());
        let mut run = base_run();
        set(&mut run, "calls", "steer", pin(10));
        let w = World::new(t, run);
        let call = native_call(10, "turn/steer", Json::Null, phase);
        if let Some(phase) = phase {
            w.observe(pin(10), call, owner(phase, Json::Null));
        } else {
            w.put("task-board/native-call", pin(10), call);
        }
        let result = w.complete().await;
        if [Some("succeeded"), Some("failed")].contains(&phase) {
            result.unwrap();
            let t = w.final_task();
            assert_eq!(f(&t, "workflowState"), &Json::from("review"));
            assert_eq!(f(&t, "latestResult"), &pin(901));
            assert_eq!(f(&t, "lastRun"), &pin(999));
            assert_eq!(f(&t, "acceptedReview"), &pin(902));
            assert_eq!(w.released.get(), 0);
            assert_eq!(w.cleanup.get(), 3);
        } else {
            assert_eq!(
                result.unwrap_err().code,
                if phase.is_none() {
                    "task_board_native_evidence_required"
                } else {
                    "task_board_outcome_unresolved"
                }
            );
            assert!(w.writes.borrow().is_empty());
        }
    }
}

#[tokio::test]
async fn completion_releases_shared_workspace_reservations_and_keeps_isolated_ones() {
    for (kind, worktree, released) in [
        ("directory_path", false, 1),
        ("existing_project", false, 1),
        ("existing_project", true, 0),
        ("isolated", false, 0),
    ] {
        let mut t = base_task();
        set(
            &mut t,
            "fields",
            "workspaceRequirement",
            Json::object([
                ("kind", kind.into()),
                ("useWorktree", worktree.into()),
                ("projectId", "project".into()),
            ]),
        );
        let w = World::new(t, base_run());
        w.complete().await.unwrap();
        assert_eq!(w.released.get(), released);
    }
}

#[tokio::test]
async fn steering_can_deliver_captured_input_after_later_edits_but_never_future_or_consumed_input()
{
    for (work, queued, accepted) in [(4.0, true, true), (9.0, true, false), (4.0, false, false)] {
        let mut t = base_task();
        t.insert("workRevision", Json::Number(8.0));
        t.insert(
            "commentDeliveries",
            Json::Array(vec![Json::object([
                ("commentId", "captured".into()),
                (
                    "state",
                    if queued {
                        "queued".into()
                    } else {
                        "delivered".into()
                    },
                ),
            ])]),
        );
        let w = World::new(t, base_run());
        let origin = Json::object([
            ("kind", "steer".into()),
            ("predecessor", Json::Null),
            ("workRevision", Json::Number(work)),
            ("commentIds", Json::Array(vec!["captured".into()])),
        ]);
        w.put(
            "task-board/native-call",
            pin(10),
            native_call(10, "turn/steer", origin, None),
        );
        let result = w
            .publish(Json::object([
                ("kind", "attachCall".into()),
                ("slot", "steer".into()),
                ("call", pin(10)),
            ]))
            .await;
        if accepted {
            result.unwrap();
            assert_eq!(f(f(&w.saved_run(), "calls"), "steer"), &pin(10));
        } else {
            assert_eq!(
                result.unwrap_err().code,
                "task_board_native_intent_mismatch"
            );
            assert!(w.writes.borrow().is_empty());
        }
    }
}
