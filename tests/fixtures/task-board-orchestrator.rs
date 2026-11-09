#[path = "reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    native_driver::NativeDriverRuntime,
    native_inspector::{Inspection, InspectionContext},
    native_intent::NativeSlot,
    native_lifecycle::{LifecycleDocument, LifecycleStore},
    native_orchestrator::{NativeOrchestrator, OrchestrationEngine, OrchestrationServices},
    native_state::{OutcomeState, RunPhase, finished_run, project_native_state, read_outcome},
    operations::{Document, Store},
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    path::Path,
    process::Command,
    sync::OnceLock,
};
#[path = "task-board-orchestration-journal.rs"]
mod journal;
#[path = "task-board-orchestration-policy.rs"]
mod policy;
#[path = "task-board-native-progress.rs"]
mod progress;
#[path = "task-board-orchestration-recovery.rs"]
mod recovery;
fn progress_tracker() -> ivy_task_board::native_progress::NativeProgress {
    ivy_task_board::native_progress::NativeProgress::new("fixture-task-board".into(), 300_000.0)
}
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn flag(v: &Json, k: &str) -> bool {
    v.get(k) == Some(&Json::Bool(true))
}
fn n(v: &Json, k: &str) -> f64 {
    v.get(k).and_then(Json::as_f64).unwrap_or(f64::NAN)
}
fn id(n: u64) -> JsString {
    format!("00000000-0000-4000-8000-{n:012}").into()
}
fn pin(n: u64, r: f64) -> Json {
    Json::object([
        ("objectId", Json::String(id(n))),
        ("revision", Json::Number(r)),
    ])
}
fn doc(v: &Json) -> Document {
    Document {
        pin: ivy_task_board::operations::ObjectPin::from_json(v.required("pin").unwrap().clone())
            .unwrap(),
        value: v.required("value").unwrap().clone(),
        content_hash: hash_json(v.required("value").unwrap()).unwrap(),
        parent_id: None,
    }
}
pub(crate) fn data() -> &'static Json {
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        let o = reference_data::read(include_bytes!("task-board-orchestrator-cases.json.gz"));
        parse(std::str::from_utf8(&o).unwrap(), MANAGEMENT_FRAME_BYTES).unwrap()
    })
}
fn outcome(result: IvyResult<Json>) -> Json {
    match result {
        Ok(v) => Json::object([("ok", v)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.to_json()),
            ]),
        )]),
    }
}
#[test]
fn current_native_state_projection_preserves_terminal_outcomes_and_native_attention() {
    let base = Json::object([
        ("nativeEpoch", "original".into()),
        ("cancellation", Json::Null),
        (
            "externalOutcome",
            Json::object([("state", "active".into()), ("code", Json::Null)]),
        ),
    ]);
    for (status, epoch, flag, phase, state) in [
        (
            "inProgress",
            Some("original"),
            Some("waitingOnApproval"),
            "waiting_input",
            "active",
        ),
        (
            "inProgress",
            Some("replacement"),
            Some("waitingOnUserInput"),
            "waiting_input",
            "active",
        ),
        ("inProgress", Some("replacement"), None, "running", "active"),
        ("inProgress", None, None, "outcome_unknown", "unknown"),
        (
            "completed",
            Some("original"),
            None,
            "publishing_result",
            "succeeded",
        ),
        (
            "failed",
            Some("original"),
            None,
            "publishing_result",
            "failed",
        ),
        (
            "interrupted",
            Some("replacement"),
            None,
            "publishing_result",
            "failed",
        ),
    ] {
        let thread = Json::object([(
            "status",
            Json::object([
                ("type", "active".into()),
                (
                    "activeFlags",
                    Json::Array(flag.into_iter().map(Json::from).collect()),
                ),
            ]),
        )]);
        let projected = project_native_state(
            &base,
            &status.into(),
            &epoch.map(Json::from).unwrap_or(Json::Null),
            &thread,
        )
        .unwrap();
        assert_eq!(projected.phase.as_str(), phase);
        assert_eq!(projected.outcome.state.as_str(), state);
        if phase == "waiting_input" {
            assert!(projected.waiting.is_none());
        }
    }
    for state in ["succeeded", "failed", "cancelled"] {
        let mut run = base.clone();
        run.insert(
            "externalOutcome",
            Json::object([("state", state.into()), ("code", Json::Null)]),
        );
        assert_eq!(
            project_native_state(&run, &"inProgress".into(), &"original".into(), &Json::Null,)
                .unwrap_err()
                .code,
            "task_board_native_state_changed"
        );
        run.insert(
            "phase",
            Json::from(if state == "succeeded" {
                "completed"
            } else {
                state
            }),
        );
        assert!(finished_run(&run));
    }
}

#[test]
fn native_run_projection_retains_boundary_values_without_json_state() {
    let at = JsString(vec![b't' as u16, 0xd800]);
    let evidence = Json::Object(vec![
        ("object".into(), "original evidence".into()),
        ("object".into(), Json::String(JsString(vec![0xdfff; 16384]))),
    ]);
    let thread = Json::object([(
        "status",
        Json::object([
            ("type", "active".into()),
            ("activeFlags", Json::Array(vec!["running".into()])),
        ]),
    )]);
    let mut run = Json::Object(vec![
        ("phase".into(), "running".into()),
        ("phase".into(), "completed".into()),
        ("cancellation".into(), Json::Null),
        ("cancellation".into(), true.into()),
        (
            "externalOutcome".into(),
            Json::object([("state", "active".into()), ("code", Json::Null)]),
        ),
        (
            "externalOutcome".into(),
            Json::object([("state", "succeeded".into())]),
        ),
    ]);
    let original = run.clone();
    for (status, phase, state, code) in [
        ("inProgress", RunPhase::Running, OutcomeState::Active, None),
        (
            "completed",
            RunPhase::PublishingResult,
            OutcomeState::Succeeded,
            None,
        ),
        (
            "failed",
            RunPhase::PublishingResult,
            OutcomeState::Failed,
            Some("native_failed"),
        ),
        (
            "interrupted",
            RunPhase::PublishingResult,
            OutcomeState::Failed,
            Some("native_interrupted"),
        ),
    ] {
        let projected =
            project_native_state(&run, &status.into(), &"epoch".into(), &thread).unwrap();
        assert_eq!(projected.phase, phase);
        assert_eq!(projected.outcome.state, state);
        assert_eq!(projected.outcome.code, code.map(JsString::from));
        assert!(projected.outcome.matches(f(
            &Json::object([(
                "outcome",
                Json::object([
                    ("state", state.as_str().into()),
                    ("code", code.map(Json::from).unwrap_or(Json::Null)),
                ])
            ),]),
            "outcome"
        )));
        let (serialized_phase, outcome, waiting) = projected.into_parts(&at, &evidence);
        assert_eq!(serialized_phase, Json::from(phase.as_str()));
        assert_eq!(
            outcome
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, _)| key.to_utf8().unwrap())
                .collect::<Vec<_>>(),
            ["state", "code", "observedAt", "evidence"]
        );
        assert_eq!(outcome.get("observedAt"), Some(&Json::String(at.clone())));
        assert_eq!(outcome.get("evidence"), Some(&evidence));
        if status == "inProgress" {
            assert!(waiting.is_null());
        } else {
            assert_eq!(f(&waiting, "reason"), &Json::from("publication"));
            assert_eq!(waiting.get("since"), Some(&Json::String(at.clone())));
        }
        assert_eq!(run, original);
    }
    for epoch in [Json::Null, "".into(), Json::Number(1.0)] {
        let projected = project_native_state(&run, &"inProgress".into(), &epoch, &thread).unwrap();
        assert_eq!(projected.phase, RunPhase::OutcomeUnknown);
        assert_eq!(projected.outcome.state, OutcomeState::Unknown);
        let (_, outcome, waiting) = projected.into_parts(&at, &evidence);
        assert_eq!(
            outcome
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, _)| key.to_utf8().unwrap())
                .collect::<Vec<_>>(),
            ["state", "observedAt", "evidence", "code"]
        );
        assert_eq!(
            f(&outcome, "code"),
            &Json::from("native_active_owner_unconfirmed")
        );
        assert_eq!(outcome.get("evidence"), Some(&evidence));
        assert_eq!(f(&waiting, "reason"), &Json::from("external_outcome"));
    }
    let status = JsString(vec![0xd800, b'x' as u16, 0xdfff]);
    let outcome = read_outcome(&run, &status).unwrap();
    let mut code = JsString::from("native_");
    code.0.extend_from_slice(&status.0);
    assert_eq!(outcome.state, OutcomeState::Active);
    assert_eq!(outcome.code, Some(code));
    run.insert("cancellation", false.into());
    assert_eq!(
        project_native_state(&run, &"inProgress".into(), &"epoch".into(), &thread)
            .unwrap()
            .phase,
        RunPhase::CancelRequested
    );
    assert_eq!(
        read_outcome(&run, &"interrupted".into()).unwrap().state,
        OutcomeState::Cancelled
    );
}

#[test]
fn native_outcome_preserves_terminal_guards_and_missing_code_distinction() {
    let base = |state: &str, code: Option<Json>| {
        let mut prior = Json::Object(vec![
            ("state".into(), state.into()),
            ("state".into(), "active".into()),
        ]);
        if let Some(code) = code {
            prior.insert("code", code);
        }
        Json::object([("externalOutcome", prior), ("cancellation", Json::Null)])
    };
    let invalid_thread = Json::object([(
        "status",
        Json::object([
            ("type", "active".into()),
            ("activeFlags", Json::Array(vec![Json::Number(1.0)])),
        ]),
    )]);
    for (prior, status) in [
        (base("succeeded", None), "completed"),
        (base("succeeded", Some("".into())), "completed"),
        (base("failed", Some("native_failed".into())), "inProgress"),
        (
            base("cancelled", Some("native_interrupted".into())),
            "completed",
        ),
    ] {
        assert_eq!(
            project_native_state(&prior, &status.into(), &"epoch".into(), &invalid_thread)
                .unwrap_err()
                .code,
            "task_board_native_state_changed"
        );
    }
    let succeeded = base("succeeded", Some(Json::Null));
    assert!(
        read_outcome(&succeeded, &"completed".into())
            .unwrap()
            .matches(f(&succeeded, "externalOutcome"))
    );
    let interrupted = base("failed", Some("native_interrupted".into()));
    assert_eq!(
        read_outcome(&interrupted, &"failed".into()).unwrap().code,
        Some("native_failed".into())
    );
    let mut cancelling = interrupted;
    cancelling.insert("cancellation", true.into());
    assert_eq!(
        read_outcome(&cancelling, &"failed".into())
            .unwrap_err()
            .code,
        "task_board_native_state_changed"
    );
    assert_eq!(
        read_outcome(&cancelling, &"interrupted".into())
            .unwrap()
            .state,
        OutcomeState::Failed
    );
    let active = base("active", Some(Json::Null));
    assert_eq!(
        project_native_state(
            &active,
            &"inProgress".into(),
            &"epoch".into(),
            &invalid_thread
        )
        .unwrap_err()
        .code,
        "task_board_native_state_invalid"
    );
    let early_input = Json::object([(
        "status",
        Json::object([
            ("type", "active".into()),
            (
                "activeFlags",
                Json::Array(vec!["waitingOnApproval".into(), Json::Number(1.0)]),
            ),
        ]),
    )]);
    assert_eq!(
        project_native_state(&active, &"inProgress".into(), &"epoch".into(), &early_input)
            .unwrap()
            .phase,
        RunPhase::WaitingInput
    );
    for (phase, terminal) in [
        (RunPhase::Starting, false),
        (RunPhase::Running, false),
        (RunPhase::WaitingInput, false),
        (RunPhase::PublishingResult, false),
        (RunPhase::Completed, true),
        (RunPhase::Failed, true),
        (RunPhase::CancelRequested, false),
        (RunPhase::Cancelled, true),
        (RunPhase::OutcomeUnknown, false),
    ] {
        assert_eq!(
            RunPhase::read(Some(&Json::from(phase.as_str()))),
            Some(phase)
        );
        assert_eq!(
            finished_run(&Json::object([("phase", phase.as_str().into())])),
            terminal
        );
    }
    assert_eq!(RunPhase::read(None), None);
    assert_eq!(RunPhase::read(Some(&Json::Number(1.0))), None);
    assert!(!finished_run(&Json::object([(
        "phase",
        "COMPLETED".into()
    )])));
}

struct World<'a> {
    row: &'a Json,
    runs: RefCell<HashMap<JsString, Document>>,
    tasks: RefCell<HashMap<JsString, Document>>,
    initial: RefCell<HashMap<JsString, Document>>,
    log: RefCell<Vec<Json>>,
    now: Cell<f64>,
    reads: Cell<usize>,
    progress: Cell<usize>,
    outputs: Cell<usize>,
    fault: RefCell<Json>,
    find_observations: RefCell<Option<(Json, Json)>>,
    compact: Cell<bool>,
    headers: Cell<usize>,
    queues: Cell<usize>,
}
impl World<'_> {
    fn new(row: &Json) -> World<'_> {
        World {
            row,
            runs: RefCell::new(HashMap::from([(
                id(200),
                doc(row.required("run").unwrap()),
            )])),
            tasks: RefCell::new(HashMap::from([(
                id(100),
                doc(row.required("task").unwrap()),
            )])),
            initial: RefCell::new(HashMap::from([(
                id(100),
                doc(row.required("initial").unwrap()),
            )])),
            log: RefCell::default(),
            now: Cell::new(0.0),
            reads: Cell::new(0),
            progress: Cell::new(0),
            outputs: Cell::new(0),
            fault: RefCell::new(f(row.required("input").unwrap(), "fault").clone()),
            find_observations: RefCell::default(),
            compact: Cell::new(false),
            headers: Cell::new(0),
            queues: Cell::new(0),
        }
    }
    fn input(&self) -> &Json {
        self.row.required("input").unwrap()
    }
    fn event(&self, v: impl IntoIterator<Item = Json>) {
        self.log
            .borrow_mut()
            .push(Json::Array(v.into_iter().collect()));
    }
    fn fail(&self, where_: &str) -> IvyResult<()> {
        let v = self.fault.borrow();
        if v.get("where") == Some(&Json::from(where_)) {
            Err(IvyError::new_js(
                s(&v, "code").clone(),
                s(&v, "message").clone(),
            ))
        } else {
            Ok(())
        }
    }
    fn run(&self, object: &JsString) -> Document {
        if let Some(d) = self.runs.borrow().get(object) {
            return d.clone();
        }
        let mut d = doc(self.row.required("run").unwrap());
        let text = object.to_utf8().unwrap();
        let number: u64 = text[24..].parse().unwrap();
        d.pin.object_id = object.clone();
        d.value.insert("taskId", Json::String(id(10000 + number)));
        let mut primary = d.value.required("primaryResourceRef").unwrap().clone();
        primary.insert("nativeId", format!("thread-{text}").into());
        d.value.insert("primaryResourceRef", primary);
        d.value.insert(
            "calls",
            Json::object([
                ("thread", Json::Null),
                ("turn", Json::Null),
                ("steer", Json::Null),
                ("interrupt", Json::Null),
            ]),
        );
        d.content_hash = hash_json(&d.value).unwrap();
        self.runs.borrow_mut().insert(object.clone(), d.clone());
        d
    }
    fn task(&self, object: &JsString) -> Document {
        if let Some(d) = self.tasks.borrow().get(object) {
            return d.clone();
        }
        let text = object.to_utf8().unwrap();
        let number: u64 = text[24..].parse().unwrap();
        let run = self.run(&id(number - 10000));
        let mut initial = doc(self.row.required("initial").unwrap());
        initial.pin.object_id = object.clone();
        initial.value.insert(
            "claim",
            Json::object([("run", run.pin.to_json()), ("phase", "running".into())]),
        );
        let mut d = initial.clone();
        d.pin.revision = 2.0;
        for (k, v) in f(self.input(), "task").as_object().unwrap_or(&[]) {
            d.value.insert(k.clone(), v.clone());
        }
        if flag(self.input(), "handoff") {
            d.value.insert(
                "comments",
                Json::Array(vec![Json::object([
                    ("purpose", "handoff".into()),
                    ("run", run.pin.to_json()),
                ])]),
            );
        }
        initial.content_hash = hash_json(&initial.value).unwrap();
        d.content_hash = hash_json(&d.value).unwrap();
        self.initial.borrow_mut().insert(object.clone(), initial);
        self.tasks.borrow_mut().insert(object.clone(), d.clone());
        d
    }
    fn effect(&self, run: &JsString) {
        if !flag(self.input(), "progress") {
            return;
        }
        let count = self.progress.get() + 1;
        self.progress.set(count);
        let mut r = self.run(run);
        let revision = n(&r.pin.to_json(), "revision") + 1.0;
        r.pin.revision = revision;
        r.value.insert(
            "phase",
            if self.input().get("finishAt").and_then(Json::as_f64) == Some(count as f64) {
                "completed"
            } else {
                "starting"
            }
            .into(),
        );
        r.content_hash = hash_json(&r.value).unwrap();
        self.runs.borrow_mut().insert(run.clone(), r.clone());
        let key = s(&r.value, "taskId").clone();
        let mut task = self.task(&key);
        let revision = n(&task.pin.to_json(), "revision") + 1.0;
        task.pin.revision = revision;
        let mut claim = task.value.required("claim").unwrap().clone();
        claim.insert("run", r.pin.to_json());
        task.value.insert("claim", claim);
        task.content_hash = hash_json(&task.value).unwrap();
        self.tasks.borrow_mut().insert(key, task);
    }
    fn context(&self) -> InspectionContext {
        let v = self.row.required("context").unwrap();
        InspectionContext {
            backend: None,
            run: v.required("run").unwrap().clone(),
            native_version: s(v, "nativeVersion").clone(),
            catalog_source_hash: s(v, "catalogSourceHash").clone(),
            service_node_id: s(v, "serviceNodeId").clone(),
            epoch: s(v, "epoch").clone(),
            thread_id: s(v, "threadId").clone(),
            turn_id: s(v, "turnId").clone(),
        }
    }
}
impl Store for World<'_> {
    fn root_object_id(&self) -> Option<JsString> {
        panic!("Unexpected root")
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        panic!("Unexpected contract")
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        panic!("Unexpected retired operation")
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>> {
        self.event(["local-key".into(), Json::String(id.clone())]);
        Ok(Some(
            self.input()
                .get("localKey")
                .map(Json::as_str)
                .transpose()?
                .unwrap_or_else(|| "task-board/native-turn-snapshot".into()),
        ))
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("Unexpected closure")
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        panic!("Unexpected named lookup")
    }
    async fn read(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        self.event(["read".into(), key.into(), address.clone(), primary.into()]);
        let count = self.reads.get() + 1;
        self.reads.set(count);
        if self.input().get("readFaultAt").and_then(Json::as_f64) == Some(count as f64) {
            return Err(IvyError::new("service_unavailable", "Fixture unavailable."));
        }
        let object = address
            .as_js_string()
            .unwrap_or_else(|| s(address, "objectId"));
        Ok(match key {
            "task-board/run" => self.run(object),
            "task-board/task" => {
                let task = self.task(object);
                if address.get("revision") == Some(&Json::Number(1.0)) {
                    self.initial.borrow().get(object).unwrap().clone()
                } else {
                    task
                }
            }
            "task-board/native-call" => {
                doc(self.row.required("calls").unwrap().get_key(object).unwrap())
            }
            _ => panic!("Unknown read {key}"),
        })
    }
    async fn write(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Unexpected direct write")
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("Unexpected mutation")
    }
    async fn write_prepared(
        &self,
        _: &str,
        _: &Json,
        _: &JsString,
        _: &Json,
        _version: Option<&JsString>,
        _references: Option<&Json>,
    ) -> IvyResult<Json> {
        panic!("Unexpected prepared write")
    }
}
impl LifecycleStore for World<'_> {
    async fn task_header(
        &self,
        address: &Json,
        primary: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        self.headers.set(self.headers.get() + 1);
        let mut result = self
            .lifecycle_read("task-board/task", address, primary, archived)
            .await?;
        if self.compact.get() {
            result
                .document
                .value
                .insert("comments", Json::Array(vec![]));
            result
                .document
                .value
                .insert("commentDeliveries", Json::Array(vec![]));
        }
        Ok(result)
    }
    async fn queued_comment_ids(&self, _: &Json, task: &JsString) -> IvyResult<Vec<Json>> {
        self.queues.set(self.queues.get() + 1);
        self.fail("queued")?;
        Ok(f(&self.task(task).value, "commentDeliveries")
            .as_array()
            .unwrap_or(&[])
            .iter()
            .filter(|receipt| f(receipt, "state").is_str("queued"))
            .map(|receipt| f(receipt, "commentId").clone())
            .collect())
    }
    async fn lifecycle_read(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        _: bool,
    ) -> IvyResult<LifecycleDocument> {
        Ok(LifecycleDocument {
            document: Store::read(self, key, address, primary).await?,
            metadata: Json::Null,
        })
    }
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Unexpected page")
    }
    async fn archive(&self, _: &Json) -> IvyResult<Json> {
        panic!("Unexpected archive")
    }
    async fn stat(&self, _: &JsString) -> IvyResult<Json> {
        panic!("Unexpected stat")
    }
    fn release_workspace(&self, _: &JsString) -> IvyResult<()> {
        panic!("Unexpected workspace release")
    }
}
impl NativeDriverRuntime for World<'_> {
    async fn verify_owner(&self) -> IvyResult<()> {
        self.event(["owner".into()]);
        self.fail("owner")
    }
    async fn scoped_operation_id(&self, _: &Json) -> IvyResult<JsString> {
        panic!("Unexpected scope")
    }
    async fn native_update(&self, v: &Json) -> IvyResult<Json> {
        self.event(["update".into(), v.clone()]);
        self.fail("update")?;
        Ok(Json::Null)
    }
    fn now(&self) -> JsString {
        s(data(), "time").clone()
    }
}
impl OrchestrationEngine for World<'_> {
    async fn recover_pending_native_operation(
        &self,
        task: &JsString,
        run: &JsString,
    ) -> IvyResult<bool> {
        if flag(self.input(), "pendingNative") {
            self.event([
                "recover-pending".into(),
                Json::String(task.clone()),
                Json::String(run.clone()),
            ]);
            self.fail("pending")?;
            return Ok(true);
        }
        Ok(false)
    }
    fn now_millis(&self) -> f64 {
        self.now.get()
    }
    fn run_handoff(&self, task: &Json, run: &JsString) -> IvyResult<bool> {
        Ok(f(task, "comments")
            .as_array()
            .unwrap_or(&[])
            .iter()
            .any(|v| {
                f(v, "purpose") == &Json::from("handoff")
                    && f(v, "run").get("objectId").and_then(Json::as_js_string) == Some(run)
            }))
    }
    fn claimed_workflow_state(&self, v: &Json) -> IvyResult<JsString> {
        Ok(if v != &Json::Null
            && !["host", "external_outcome", "publication"]
                .iter()
                .any(|s| f(v, "reason") == &Json::from(*s))
        {
            "waiting"
        } else {
            "in_progress"
        }
        .into())
    }
    async fn peer_state(&self, task: &Json, id: &JsString) -> IvyResult<JsString> {
        self.event(["peers".into(), task.clone(), Json::String(id.clone())]);
        self.fail("peers")?;
        Ok(self
            .input()
            .get("peersHash")
            .and_then(Json::as_js_string)
            .cloned()
            .unwrap_or_else(|| hash_json(&Json::Array(vec![])).unwrap().into()))
    }
    async fn dependency_snapshot(&self, task: &Json) -> IvyResult<Json> {
        self.event(["dependencies".into(), task.clone()]);
        self.fail("dependencies")?;
        Ok(self
            .input()
            .get("observedDependencies")
            .cloned()
            .unwrap_or_else(|| Json::object(Vec::<(&str, Json)>::new())))
    }
    async fn sync_native_comments(&self, id: &JsString) -> IvyResult<()> {
        self.event(["sync".into(), Json::String(id.clone())]);
        self.fail("sync")?;
        if flag(self.input(), "syncHandoff") {
            let r = self.run(id);
            let key = s(&r.value, "taskId");
            let mut task = self.task(key);
            let revision = n(&task.pin.to_json(), "revision") + 1.0;
            task.pin.revision = revision;
            let mut comments = f(&task.value, "comments").as_array().unwrap().to_vec();
            comments.push(Json::object([
                ("purpose", "handoff".into()),
                ("run", r.pin.to_json()),
            ]));
            task.value.insert("comments", Json::Array(comments));
            task.content_hash = hash_json(&task.value).unwrap();
            self.tasks.borrow_mut().insert(key.clone(), task);
        }
        Ok(())
    }
    async fn with_read_cache<T, F: Future<Output = IvyResult<T>>>(&self, work: F) -> IvyResult<T> {
        self.event(["cache-enter".into()]);
        let result = work.await;
        self.event(["cache-exit".into()]);
        result
    }
}
impl OrchestrationServices for World<'_> {
    async fn recover_uncertain_resume(&self, r: &JsString) -> IvyResult<bool> {
        self.event(["recover".into(), Json::String(r.clone())]);
        self.fail("recover")?;
        Ok(flag(self.input(), "recovered"))
    }
    async fn prepare(
        &self,
        r: &JsString,
        slot: NativeSlot,
        origin: Option<&Json>,
    ) -> IvyResult<Json> {
        self.event([
            "prepare".into(),
            Json::String(r.clone()),
            slot.as_str().into(),
            origin.cloned().unwrap_or(Json::Null),
        ]);
        self.fail(&format!("prepare:{}", slot.as_str()))?;
        self.effect(r);
        Ok(Json::Null)
    }
    async fn advance(&self, r: &JsString, slot: NativeSlot) -> IvyResult<Json> {
        self.event([
            "advance".into(),
            Json::String(r.clone()),
            slot.as_str().into(),
        ]);
        self.fail(&format!("advance:{}", slot.as_str()))?;
        self.effect(r);
        Ok(self
            .input()
            .get("advanceResult")
            .cloned()
            .unwrap_or_else(|| Json::object([("advanced", true.into())])))
    }
    async fn status(&self, r: &Json, c: &Json) -> IvyResult<Json> {
        self.event(["status".into(), r.clone(), c.clone()]);
        self.fail("status")?;
        Ok(Json::object([("epoch", "replacement".into())]))
    }
    async fn find(&self, r: &Json) -> IvyResult<Inspection> {
        self.event(["find".into(), r.clone()]);
        self.fail("find")?;
        Ok(
            match self.input().get("inspection").and_then(Json::as_js_string) {
                Some(v) if v == &JsString::from("pending") => Inspection::Pending {
                    search: pin(900, 1.0),
                },
                Some(v) if v == &JsString::from("absent") => Inspection::Absent,
                _ => {
                    let (mut metadata, mut turns) =
                        self.find_observations.borrow().clone().unwrap_or_else(|| {
                            (
                                self.row.required("metadata").unwrap().clone(),
                                self.row.required("turns").unwrap().clone(),
                            )
                        });
                    if r.get("objectId") != Some(&Json::String(id(200))) {
                        let run = self.run(s(r, "objectId"));
                        let thread_id = f(f(&run.value, "primaryResourceRef"), "nativeId").clone();
                        for observation in [&mut metadata, &mut turns] {
                            let mut params = f(observation, "params").clone();
                            params.insert("threadId", thread_id.clone());
                            observation.insert("params", params);
                        }
                        let mut reply = f(&metadata, "reply").clone();
                        let mut result = f(&reply, "result").clone();
                        let mut thread = f(&result, "thread").clone();
                        thread.insert("id", thread_id);
                        result.insert("thread", thread);
                        reply.insert("result", result);
                        metadata.insert("reply", reply);
                    }
                    Inspection::Found { metadata, turns }
                }
            },
        )
    }
    async fn owner_stopped(&self, r: &Json) -> IvyResult<bool> {
        self.event(["stopped".into(), r.clone()]);
        self.fail("stopped")?;
        Ok(flag(self.input(), "stopped"))
    }
    async fn context(&self, r: &Json) -> IvyResult<InspectionContext> {
        self.event(["context".into(), r.clone()]);
        self.fail("context")?;
        Ok(self.context())
    }
    async fn read_output(&self, c: &InspectionContext, p: Json) -> IvyResult<Json> {
        self.event([
            "output-read".into(),
            c.value(),
            "thread/items/list".into(),
            p.clone(),
            true.into(),
        ]);
        self.fail("output-read")?;
        let index = self.outputs.get();
        self.outputs.set(index + 1);
        let default = Json::object([(
            "item",
            Json::object([
                ("id", "a".into()),
                ("type", "agentMessage".into()),
                ("phase", "final_answer".into()),
                ("text", "answer".into()),
            ]),
        )]);
        let values = self.input().get("outputs").and_then(Json::as_array);
        let empty = Json::Object(vec![]);
        let v = values
            .map(|v| v.get(index).unwrap_or(&empty))
            .unwrap_or(&default);
        let reply = if let Some(code) = v.get("error") {
            Json::object([(
                "error",
                Json::object([("code", code.clone()), ("message", "unavailable".into())]),
            )])
        } else {
            let item = v
                .get("item")
                .filter(|v| !v.is_null())
                .unwrap_or(default.required("item").unwrap())
                .clone();
            let mut items = if v.get("item") == Some(&Json::Null) {
                vec![]
            } else {
                vec![item.clone()]
            };
            if flag(v, "many") {
                let mut b = item;
                b.insert("id", "b".into());
                items.push(b);
            }
            Json::object([(
                "result",
                Json::object([
                    (
                        "data",
                        Json::Array(
                            items
                                .into_iter()
                                .map(|v| Json::object([("turnId", "turn".into()), ("item", v)]))
                                .collect(),
                        ),
                    ),
                    ("nextCursor", f(v, "nextCursor").clone()),
                ]),
            )])
        };
        let mut o = self.row.required("turns").unwrap().clone();
        o.insert("method", "thread/items/list".into());
        o.insert("params", p.clone());
        o.insert(
            "requestHash",
            hash_json(&Json::object([
                ("method", "thread/items/list".into()),
                ("params", p),
            ]))
            .unwrap()
            .into(),
        );
        o.insert("reply", reply);
        Ok(o)
    }
    async fn snapshot(&self, r: &Json, a: &Json) -> IvyResult<Json> {
        self.event(["snapshot".into(), r.clone(), a.clone()]);
        self.fail("snapshot")?;
        Ok(Json::object([
            (
                "status",
                self.input()
                    .get("retainedStatus")
                    .or(self.input().get("status"))
                    .cloned()
                    .unwrap_or("completed".into()),
            ),
            (
                "epoch",
                self.input()
                    .get("retainedEpoch")
                    .or(self.input().get("epoch"))
                    .cloned()
                    .unwrap_or("epoch".into()),
            ),
        ]))
    }
    async fn save_snapshot(&self, r: &Json, m: &Json, t: &Json) -> IvyResult<Json> {
        self.event(["save-snapshot".into(), r.clone(), m.clone(), t.clone()]);
        self.fail("save-snapshot")?;
        Ok(data().required("artifact").unwrap().clone())
    }
    async fn save_output(
        &self,
        r: &Json,
        s: &Json,
        c: Option<&JsString>,
        o: &Json,
    ) -> IvyResult<Json> {
        self.event([
            "save-output".into(),
            r.clone(),
            s.clone(),
            c.cloned().map(Json::String).unwrap_or(Json::Null),
            o.clone(),
        ]);
        self.fail("save-output")?;
        Ok(data().required("output").unwrap().clone())
    }
}
