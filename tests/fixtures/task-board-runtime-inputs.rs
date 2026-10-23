//! Current packaged inputs compared with the actual original producers.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::parse};
use ivy_sdk::collation::LocaleRuntime;
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    journal_store::Contracts,
    native_intent::{NativeIntent, NativeSlot, TaskPrompts},
    operations::{Document, Store},
    prompts::CurrentTaskPrompts,
    read_evidence::ReadEvidenceStore,
    runtime_inputs,
    schema::RuntimeContracts,
};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
};

fn producer(name: &str) -> Json {
    let root = PathBuf::from(
        std::env::var_os("IVY_TASK_BOARD_PRODUCER")
            .expect("Supply the actual current producer outputs"),
    );
    parse(
        &std::fs::read_to_string(root.join(name)).unwrap(),
        usize::MAX,
    )
    .unwrap()
}

#[test]
fn packaged_inputs_match_current_producer_registry_and_reader_writer_versions() {
    let locale = LocaleRuntime::load(&PathBuf::from(
        std::env::var_os("IVY_TEST_LOCALE_RUNTIME").unwrap(),
    ))
    .unwrap();
    let inputs = runtime_inputs::packaged(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        locale.executing_default().unwrap(),
    )
    .unwrap();
    let expected = producer("registry.json");
    if hash_json(inputs.registry.value()).unwrap() != hash_json(&expected).unwrap() {
        let root = PathBuf::from(std::env::var_os("IVY_TASK_BOARD_PRODUCER").unwrap());
        std::fs::write(
            root.join("native-registry.actual.json"),
            ivy_contracts::json::stringify(inputs.registry.value(), usize::MAX).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        hash_json(inputs.registry.value()).unwrap(),
        hash_json(&expected).unwrap()
    );
    let metadata = producer("runtime-inputs.json");
    for (key, version) in metadata
        .required("writerVersions")
        .unwrap()
        .as_object()
        .unwrap()
    {
        assert_eq!(
            inputs.contracts.version(&key.to_utf8().unwrap()).unwrap(),
            version.as_str().unwrap()
        );
    }
    assert_eq!(
        inputs.chat_readable_versions,
        vec![JsString::from("1.2.0"), JsString::from("1.3.0")]
    );
    assert!(
        inputs
            .contracts
            .readable_version("task-board/task", &"2.3.0".into())
            .unwrap()
    );
    assert!(
        inputs
            .contracts
            .readable_version("task-board/task", &"3.1.0".into())
            .unwrap()
    );
    assert!(
        !inputs
            .contracts
            .readable_version("task-board/task", &"4.0.0".into())
            .unwrap()
    );
}

#[test]
fn current_prompts_and_local_admission_match_original_producers() {
    let contracts = runtime_inputs::contracts().unwrap();
    for row in producer("prompt-vectors.json").as_array().unwrap() {
        let changed = row.required("executionChangeRequested").unwrap();
        let owner = row
            .required("serviceNodeId")
            .unwrap()
            .as_js_string()
            .unwrap();
        let task = row.required("task").unwrap();
        let expected = row.required("expected").unwrap().as_js_string().unwrap();
        assert_eq!(
            &CurrentTaskPrompts
                .task_prompt(task, owner, Some(changed))
                .unwrap(),
            expected
        );
        assert_eq!(
            &CurrentTaskPrompts
                .task_update_prompt(task, owner, changed.as_bool().unwrap())
                .unwrap(),
            expected
        );
    }
    let rows = producer("admission-vectors-v2.json");
    for row in rows.as_array().unwrap() {
        let definition = row.required("definition").unwrap().as_str().unwrap();
        let result = contracts.validate_definition(&definition, row.required("value").unwrap());
        assert_eq!(
            result.is_ok(),
            row.required("accepted").unwrap().as_bool().unwrap(),
            "{definition}"
        );
        if let Err(error) = result {
            assert_eq!(error.code, row.required("code").unwrap().as_str().unwrap());
        }
    }
}

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
    let vectors = producer("prompt-vectors.json");
    let store = ReadOnlyTask {
        contracts: runtime_inputs::contracts().unwrap(),
        task: vectors.as_array().unwrap()[0]
            .required("task")
            .unwrap()
            .clone(),
        reads: Cell::new(0),
    };
    let catalogs =
        ivy_contracts::checked_native_contract::CheckedNativeContracts::new(PathBuf::new());
    let run=parse(r#"{"taskId":"fixture-task","primaryResourceRef":{"kind":"thread","namespace":"codex","serviceNodeId":"fixture-owner","nativeId":"fixture-thread"},"turnId":"fixture-turn","nativeEpoch":"fixture-epoch","cancellation":null,"owner":{"serviceNodeId":"fixture.task-board","source":"agent"},"originalRequest":{"action":"continue","expectedRevision":1,"coordinationOnly":true},"plan":{}}"#,usize::MAX).unwrap();
    let intent = NativeIntent {
        store: &store,
        catalogs: &catalogs,
    };
    let rows = producer("admission-vectors-v2.json");
    let original = rows.as_array().unwrap()[0].required("value").unwrap();
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

#[tokio::test(flavor = "current_thread")]
async fn current_prompts_materialize_and_retain_exact_calls_against_the_packaged_native_catalog() {
    let distribution = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let catalogs =
        ivy_contracts::checked_native_contract::CheckedNativeContracts::new(distribution);
    let contract = catalogs
        .checked_native_contract(&"0.159.2".into(), None)
        .unwrap();
    let mut plan = Json::object([
        (
            "nativeVersion",
            contract.catalog().required("version").unwrap().clone(),
        ),
        (
            "catalogSourceHash",
            contract.catalog().required("sourceHash").unwrap().clone(),
        ),
        ("turnStart", Json::Object(vec![])),
    ]);
    plan.insert(
        "definitions",
        Json::object([(
            "turnStart",
            hash_json(contract.definition(&"turn/start".into()).unwrap())
                .unwrap()
                .into(),
        )]),
    );
    let origins = producer("admission-vectors-v2.json");
    for row in producer("prompt-vectors.json").as_array().unwrap() {
        let store = ReadOnlyTask {
            contracts: runtime_inputs::contracts().unwrap(),
            task: row.required("task").unwrap().clone(),
            reads: Cell::new(0),
        };
        let mut run = parse(r#"{"taskId":"fixture-task","primaryResourceRef":{"kind":"thread","namespace":"codex","serviceNodeId":"fixture-owner","nativeId":"fixture-thread"},"turnId":"fixture-turn","nativeEpoch":"fixture-epoch","cancellation":null,"owner":{"source":"agent"},"originalRequest":{"action":"continue","expectedRevision":1,"coordinationOnly":true}}"#,usize::MAX).unwrap();
        run.insert("plan", plan.clone());
        let mut owner = run.required("owner").unwrap().clone();
        owner.insert(
            "serviceNodeId",
            row.required("serviceNodeId").unwrap().clone(),
        );
        run.insert("owner", owner);
        let intent = NativeIntent {
            store: &store,
            catalogs: &catalogs,
        };
        let mut origin = origins.as_array().unwrap()[0]
            .required("value")
            .unwrap()
            .clone();
        origin.insert(
            "executionChangeRequested",
            row.required("executionChangeRequested").unwrap().clone(),
        );
        let intended = intent
            .intended_call(&run, NativeSlot::Steer, Some(&origin), &CurrentTaskPrompts)
            .await
            .unwrap();
        let request = intended.required("request").unwrap();
        assert_eq!(request.get("method"), Some(&"turn/steer".into()));
        assert_eq!(
            request
                .required("params")
                .unwrap()
                .required("input")
                .unwrap()
                .as_array()
                .unwrap()[0]
                .get("text"),
            row.get("expected")
        );
        assert_eq!(
            intended.get("expectedDefinitionHash"),
            Some(&Json::from(
                hash_json(contract.definition(&"turn/steer".into()).unwrap()).unwrap()
            ))
        );
        let retained = intent
            .retain_native_request(request, &"current-call".into())
            .unwrap();
        assert_eq!(
            hash_json(&intent.resolve_native_request(&retained).unwrap()).unwrap(),
            hash_json(request).unwrap()
        );
        if row.required("executionChangeRequested").unwrap().as_bool() == Some(false) {
            let initial = intent
                .intended_call(&run, NativeSlot::Turn, None, &CurrentTaskPrompts)
                .await
                .unwrap();
            let input = initial
                .required("request")
                .unwrap()
                .required("params")
                .unwrap()
                .required("input")
                .unwrap()
                .as_array()
                .unwrap();
            assert_eq!(input.last().unwrap().get("text"), row.get("expected"));
        }
    }
}
