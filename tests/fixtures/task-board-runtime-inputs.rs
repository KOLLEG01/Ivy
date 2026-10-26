//! Current request admission and native intent boundaries.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::parse};
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    journal_store::Contracts,
    native_intent::{NativeIntent, NativeSlot, TaskPrompts},
    operations::{Document, Store},
    read_evidence::ReadEvidenceStore,
    runtime_inputs,
    schema::RuntimeContracts,
};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
};

#[test]
fn durable_requests_admit_overlapping_actions_and_keep_nested_record_guards() {
    let contracts = runtime_inputs::contracts().unwrap();
    let defer = parse(
        r#"{"action":"defer","operationId":"defer-fixture","taskId":"fixture-task","expectedRevision":1,"wakeAt":null,"dependencies":[]}"#,
        usize::MAX,
    )
    .unwrap();
    for definition in ["ActionRequest", "SchedulerRequest", "DurableRequest"] {
        contracts.validate_definition(definition, &defer).unwrap();
    }
    let mut operation = parse(
        r#"{"schemaVersion":1,"operationId":"record-fixture","callerPrincipalId":"fixture-caller","request":null,"requestHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","actor":{"principalId":"fixture-caller","serviceNodeId":"fixture.task-board","generation":1,"source":"native"},"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z","phase":"accepted","writes":[],"outcome":null,"error":null}"#,
        usize::MAX,
    )
    .unwrap();
    let mut requests = vec![defer.clone()];
    for text in [
        r#"{"action":"transition","operationId":"transition-fixture","taskId":"fixture-task","expectedRevision":1,"workflowState":"todo","detail":null}"#,
        r#"{"action":"cancel","operationId":"cancel-fixture","taskId":"fixture-task","expectedRevision":1,"reason":"fixture"}"#,
        r#"{"action":"nativeUpdate","operationId":"native-fixture","taskId":"fixture-task","expectedRevision":1,"run":{"objectId":"fixture-run","revision":1},"change":{"kind":"cancelUnstarted"}}"#,
        r#"{"action":"archive","operationId":"archive-fixture","taskId":"fixture-task","expectedRevision":1,"archived":true}"#,
    ] {
        requests.push(parse(text, usize::MAX).unwrap());
    }
    for request in requests {
        contracts
            .validate_definition("DurableRequest", &request)
            .unwrap();
        operation.insert("requestHash", hash_json(&request).unwrap().into());
        operation.insert("request", request.clone());
        contracts
            .validate_record("task-board/operation", &"1.0.0".into(), &operation)
            .unwrap();
        let mut extra = request;
        extra.insert("unexpected", true.into());
        assert_eq!(
            contracts
                .validate_definition("DurableRequest", &extra)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
        operation.insert("request", extra);
        assert_eq!(
            contracts
                .validate_record("task-board/operation", &"1.0.0".into(), &operation)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    for (field, invalid) in [
        ("action", "unknown".into()),
        ("expectedRevision", Json::Number(0.0)),
        ("dependencies", "fixture-task".into()),
        ("wakeAt", "tomorrow".into()),
        ("operationId", "".into()),
    ] {
        let mut invalid_request = defer.clone();
        invalid_request.insert(field, invalid);
        assert_eq!(
            contracts
                .validate_definition("DurableRequest", &invalid_request)
                .unwrap_err()
                .code,
            "invalid_arguments",
            "{field}"
        );
    }
}

#[test]
fn retained_task_field_requests_do_not_change_current_action_admission() {
    let contracts = runtime_inputs::contracts().unwrap();
    let mut operation = parse(
        r#"{"schemaVersion":1,"operationId":"retained-create","callerPrincipalId":"fixture-caller","request":{"action":"create","operationId":"retained-create","workflowState":"backlog","fields":{"title":"Original retained ticket","description":"","acceptanceCriteria":[],"category":null,"control":"user","priority":2,"executionRequirement":null,"workspaceRequirement":{"kind":"task_workspace"},"dependencies":["dependency-task"],"userContact":"ticket","nextReviewAt":"2026-01-01T00:00:00.000Z","dueAt":null}},"requestHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","actor":{"principalId":"fixture-caller","serviceNodeId":"fixture.task-board","generation":1,"source":"user"},"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z","phase":"succeeded","writes":[],"outcome":{"operationId":"retained-create","task":{"objectId":"retained-task","revision":4},"plan":null,"run":null,"history":null,"result":null,"review":null,"attachment":null},"error":null}"#,
        usize::MAX,
    ).unwrap();
    let mut request = operation.required("request").unwrap().clone();
    for action in ["create", "edit"] {
        request.insert("action", action.into());
        if action == "edit" {
            request.remove("workflowState");
            request.insert("taskId", "retained-task".into());
            request.insert("expectedRevision", Json::Number(4.0));
        }
        operation.insert("requestHash", hash_json(&request).unwrap().into());
        operation.insert("request", request.clone());
        contracts
            .validate_record("task-board/operation", &"1.0.0".into(), &operation)
            .unwrap();
        for definition in ["ActionInput", "DurableRequest"] {
            assert_eq!(
                contracts
                    .validate_definition(definition, &request)
                    .unwrap_err()
                    .code,
                "invalid_arguments"
            );
        }
    }
    let mut fields = request.required("fields").unwrap().clone();
    fields.insert("dependencies", "dependency-task".into());
    request.insert("fields", fields);
    operation.insert("request", request);
    assert_eq!(
        contracts
            .validate_record("task-board/operation", &"1.0.0".into(), &operation)
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
}

struct ReadOnlyTask {
    contracts: RuntimeContracts,
    task: Json,
    reads: Cell<usize>,
}
fn unexpected<T>() -> IvyResult<T> {
    panic!("Intent preparation attempted a write or evidence effect")
}
impl Store for ReadOnlyTask {
    fn root_object_id(&self) -> Option<JsString> {
        None
    }
    fn contract_version(&self, key: &str) -> IvyResult<String> {
        self.contracts.version(key)
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        unexpected()
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        unexpected()
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        unexpected()
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        unexpected()
    }
    async fn read(&self, key: &str, _: &Json, upgrade: bool) -> IvyResult<Document> {
        assert_eq!(key, "task-board/task");
        assert!(upgrade);
        self.reads.set(self.reads.get() + 1);
        Ok(Document {
            pin: Json::object([
                ("objectId", "fixture-task".into()),
                ("revision", Json::Number(1.0)),
            ]),
            value: self.task.clone(),
            parent_id: None,
            content_hash: hash_json(&self.task)?,
        })
    }
    async fn write(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        unexpected()
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        unexpected()
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        unexpected()
    }
}
impl BlobStore for ReadOnlyTask {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<Json> {
        unexpected()
    }
    fn read(&self, _: ChunkKey, _: &Json, _: &JsString) -> IvyResult<Vec<u8>> {
        unexpected()
    }
}
impl ReadEvidenceStore for ReadOnlyTask {
    fn validate(&self, name: &str, value: &Json) -> IvyResult<()> {
        self.contracts.validate_definition(name, value)
    }
    async fn read_document(&self, _: &str, _: &Json, _: bool) -> IvyResult<Document> {
        unexpected()
    }
    async fn write_document(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        unexpected()
    }
}
struct PromptProbe(RefCell<Vec<Option<bool>>>);
impl TaskPrompts for PromptProbe {
    fn task_prompt(&self, _: &Json, _: &JsString, flag: Option<&Json>) -> IvyResult<JsString> {
        self.0.borrow_mut().push(flag.map(|v| v.as_bool().unwrap()));
        Err(IvyError::new(
            "prompt_probe",
            "Stopped before catalog access.",
        ))
    }
    fn task_update_prompt(&self, _: &Json, _: &JsString, flag: bool) -> IvyResult<JsString> {
        self.0.borrow_mut().push(Some(flag));
        Err(IvyError::new(
            "prompt_probe",
            "Stopped before catalog access.",
        ))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_intent_uses_the_origin_execution_change_flag_before_catalog_or_effects() {
    let vectors = parse(include_str!("task-board-prompts.json"), 65_536).unwrap();
    let store = ReadOnlyTask {
        contracts: runtime_inputs::contracts().unwrap(),
        task: vectors.required("baseTask").unwrap().clone(),
        reads: Cell::new(0),
    };
    let catalogs =
        ivy_contracts::checked_native_contract::CheckedNativeContracts::new(PathBuf::new());
    let run=parse(r#"{"taskId":"fixture-task","primaryResourceRef":{"kind":"thread","namespace":"codex","serviceNodeId":"fixture-owner","nativeId":"fixture-thread"},"turnId":"fixture-turn","nativeEpoch":"fixture-epoch","cancellation":null,"owner":{"serviceNodeId":"fixture.task-board","source":"agent"},"originalRequest":{"action":"continue","expectedRevision":1,"coordinationOnly":true},"plan":{}}"#,usize::MAX).unwrap();
    let intent = NativeIntent {
        store: &store,
        catalogs: &catalogs,
    };
    let original = parse(r#"{"kind":"steer","commentIds":[],"workRevision":1,"peersHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","predecessor":null}"#, usize::MAX).unwrap();
    for changed in [false, true] {
        let mut origin = original.clone();
        origin.insert("executionChangeRequested", changed.into());
        let probe = PromptProbe(RefCell::new(vec![]));
        assert_eq!(
            intent
                .intended_call(&run, NativeSlot::Steer, Some(&origin), &probe)
                .await
                .unwrap_err()
                .code,
            "prompt_probe"
        );
        assert_eq!(*probe.0.borrow(), vec![Some(changed)]);
    }
    let probe = PromptProbe(RefCell::new(vec![]));
    assert_eq!(
        intent
            .intended_call(&run, NativeSlot::Turn, None, &probe)
            .await
            .unwrap_err()
            .code,
        "prompt_probe"
    );
    assert_eq!(*probe.0.borrow(), vec![None]);
    let mut invalid = original.clone();
    invalid.insert("executionChangeRequested", "true".into());
    let reads = store.reads.get();
    let probe = PromptProbe(RefCell::new(vec![]));
    assert_eq!(
        intent
            .intended_call(&run, NativeSlot::Steer, Some(&invalid), &probe)
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    assert_eq!(store.reads.get(), reads);
    assert!(probe.0.borrow().is_empty());
}
