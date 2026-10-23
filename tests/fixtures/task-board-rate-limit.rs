use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::parse,
};
use ivy_task_board::{
    native_inspector::Inspection,
    native_rate_limit::{
        FailedRateLimit, RetryInspection, native_error_retry_at, native_rate_limit_retry_at,
        waiting_detail,
    },
    operations::{Document, Store},
};
use std::cell::{Cell, RefCell};

#[test]
fn publication_runtime_binds_native_retry_without_a_date_or_policy_callback() {
    use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
    use ivy_task_board::native_publication::{
        NativePublicationEngine, NativePublicationPolicies, NativePublicationRuntime,
    };
    struct NoRpc;
    impl RpcClient for NoRpc {
        fn request<'a>(&'a self, _: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
            Box::pin(async { panic!("Retry policy must not send an RPC") })
        }
    }
    struct NoPublication;
    impl NativePublicationPolicies for NoPublication {
        fn now(&self) -> JsString {
            unreachable!()
        }
        async fn aggregate(&self, _: &Json, _: &JsString) -> IvyResult<()> {
            unreachable!()
        }
    }
    let runtime = NativePublicationRuntime::new(
        std::sync::Arc::new(NoRpc),
        "board".into(),
        Json::Null,
        NoPublication,
    );
    assert_eq!(
        runtime
            .retry_at(&turn(error("serverOverloaded", "")), &AT.into())
            .unwrap(),
        Json::from("2026-03-29T00:20:30.000Z")
    );
}

const AT: &str = "2026-03-29T00:20:00.000Z";
fn json(text: &str) -> Json {
    parse(text, 65536).unwrap()
}
fn error(kind: impl Into<Json>, message: &str) -> Json {
    Json::object([("codexErrorInfo", kind.into()), ("message", message.into())])
}
fn turn(error: Json) -> Json {
    Json::object([("status", "failed".into()), ("error", error)])
}
fn retry(value: &Json, at: &str) -> Option<String> {
    native_rate_limit_retry_at(value, &at.into())
        .unwrap()
        .map(|v| v.to_utf8().unwrap())
}
fn http(kind: &str, status: Json) -> Json {
    error(
        Json::object([(kind, Json::object([("httpStatusCode", status)]))]),
        "",
    )
}

#[test]
fn current_native_error_codes_select_only_conclusive_retries() {
    for (kind, expected) in [
        ("serverOverloaded", "2026-03-29T00:20:30.000Z"),
        ("rateLimitExceeded", "2026-03-29T00:21:00.000Z"),
        ("httpConnectionFailed", "2026-03-29T00:20:10.000Z"),
        ("responseStreamConnectionFailed", "2026-03-29T00:20:10.000Z"),
        ("responseTooManyFailedAttempts", "2026-03-29T00:20:10.000Z"),
        ("responseStreamDisconnected", "2026-03-29T00:20:10.000Z"),
        ("usageLimitExceeded", "2026-03-29T00:35:00.000Z"),
    ] {
        assert_eq!(
            retry(&turn(error(kind, "")), AT).as_deref(),
            Some(expected),
            "{kind}"
        );
        let nested = Json::object([("data", Json::object([("codexErrorInfo", kind.into())]))]);
        assert_eq!(retry(&turn(nested), AT).as_deref(), Some(expected));
    }
    for status in ["completed", "inProgress", "interrupted"] {
        let mut value = turn(error("serverOverloaded", ""));
        value.insert("status", status.into());
        assert_eq!(retry(&value, AT), None);
    }
    for kind in ["native_failed", "internalServerError", "unknown"] {
        assert_eq!(
            retry(&turn(error(kind, "Unrelated assistant prose")), AT),
            None
        );
    }
}

#[test]
fn native_http_errors_honor_status_and_reject_permanent_failures() {
    for kind in [
        "httpConnectionFailed",
        "responseStreamConnectionFailed",
        "responseTooManyFailedAttempts",
    ] {
        for (status, expected) in [
            (408.0, "2026-03-29T00:20:10.000Z"),
            (425.0, "2026-03-29T00:20:10.000Z"),
            (429.0, "2026-03-29T00:21:00.000Z"),
            (500.0, "2026-03-29T00:20:10.000Z"),
            (502.0, "2026-03-29T00:20:10.000Z"),
            (503.0, "2026-03-29T00:20:30.000Z"),
        ] {
            assert_eq!(
                retry(&turn(http(kind, Json::Number(status))), AT).as_deref(),
                Some(expected)
            );
        }
        for status in [
            Json::Number(400.0),
            Json::Number(401.0),
            Json::Number(403.0),
            Json::Number(404.0),
            Json::Number(409.0),
            "503".into(),
            false.into(),
        ] {
            assert_eq!(retry(&turn(http(kind, status)), AT), None);
        }
    }
}

#[test]
fn completed_time_and_native_error_message_boundaries_are_preserved() {
    let mut value = turn(error("serverOverloaded", ""));
    value.insert("completedAt", Json::Number(0.125));
    assert_eq!(
        retry(&value, AT).as_deref(),
        Some("1970-01-01T00:00:30.125Z")
    );
    for completed in [Json::Number(2_000_000_000.0), "0".into(), Json::Null] {
        value.insert("completedAt", completed);
        assert_eq!(
            retry(&value, AT).as_deref(),
            Some("2026-03-29T00:20:30.000Z")
        );
    }
    for message in [
        "Selected model is at capacity",
        "SELECTED MODEL IS AT CAPACITY. Later",
        "Selected model is at capacity!",
    ] {
        assert_eq!(
            retry(&turn(error("unknown", message)), AT).as_deref(),
            Some("2026-03-29T00:20:30.000Z")
        );
    }
    for message in [
        "Warning: rate limit reached",
        "Rate limit exceededed",
        "Selected model is at capacity?",
        "We will retry later.",
    ] {
        assert_eq!(retry(&turn(error("unknown", message)), AT), None);
    }
    for message in [
        "You've hit your usage limit.",
        "You’ve hit your session limit.",
        "RATE LIMIT REACHED",
        "Claude Sonnet limit reached",
    ] {
        assert_eq!(
            retry(&turn(error(Json::Null, message)), AT).as_deref(),
            Some("2026-03-29T00:35:00.000Z")
        );
    }
    assert!(native_rate_limit_retry_at(&value, &"bad date".into()).is_err());
    assert!(
        native_error_retry_at(
            &error("usageLimitExceeded", "resets 2026-03-29T01:00:00Z"),
            &"bad date".into()
        )
        .is_err()
    );
    assert_eq!(
        waiting_detail(&AT.into()).to_utf8().unwrap(),
        format!("Agent temporarily unavailable; automatic continuation after {AT}.")
    );
}

#[test]
fn utc_and_local_reset_clocks_use_native_calendar_rollover() {
    for (message, observed, expected) in [
        (
            "resets 2026-03-29T01:00:00Z",
            AT,
            "2026-03-29T01:01:00.000Z",
        ),
        (
            "resets 2026-03-29T02:00:00+01:00",
            AT,
            "2026-03-29T01:01:00.000Z",
        ),
        ("resets at 1:00 (UTC)", AT, "2026-03-29T01:01:00.000Z"),
        ("resets 0:10 (UTC)", AT, "2026-03-30T00:11:00.000Z"),
        (
            "resets 0:10 (UTC)",
            "2026-01-31T23:50:00.000Z",
            "2026-02-01T00:11:00.000Z",
        ),
        (
            "resets 0:10 (UTC)",
            "2024-02-28T23:50:00.000Z",
            "2024-02-29T00:11:00.000Z",
        ),
        (
            "resets 0:10 (UTC)",
            "2026-12-31T23:50:00.000Z",
            "2027-01-01T00:11:00.000Z",
        ),
        (
            "resets 1:00 (UTC)",
            "0099-01-01T00:20:00.000Z",
            "0099-01-01T01:01:00.000Z",
        ),
        ("resets 8:00 (Asia/Kolkata)", AT, "2026-03-29T02:31:00.000Z"),
        ("resets 8:00 (+02:30)", AT, "2026-03-29T05:31:00.000Z"),
        (
            "resets 12:00 AM (UTC)",
            "2026-03-28T23:00:00.000Z",
            "2026-03-29T00:01:00.000Z",
        ),
    ] {
        assert_eq!(
            retry(&turn(error("usageLimitExceeded", message)), observed).as_deref(),
            Some(expected),
            "{message} at {observed}"
        );
    }
}

#[test]
fn reset_clocks_account_for_dst_and_discard_unknown_or_stale_hints() {
    for (message, observed, expected) in [
        (
            "resets 3:30 (Europe/Berlin)",
            AT,
            "2026-03-29T01:31:00.000Z",
        ),
        (
            "resets 2:30 (Europe/Berlin)",
            "2026-10-25T00:00:00.000Z",
            "2026-10-25T01:31:00.000Z",
        ),
        (
            "session limit; resets 0:10 (UTC)",
            AT,
            "2026-03-29T00:35:00.000Z",
        ),
        ("resets 2:60 (UTC)", AT, "2026-03-29T00:35:00.000Z"),
        ("resets 24:00 (UTC)", AT, "2026-03-29T00:35:00.000Z"),
        ("resets 8:00 (Unknown/Zone)", AT, "2026-03-29T00:35:00.000Z"),
        (
            "resets 2026-03-28T01:00:00Z",
            AT,
            "2026-03-29T00:35:00.000Z",
        ),
    ] {
        assert_eq!(
            retry(&turn(error("usageLimitExceeded", message)), observed).as_deref(),
            Some(expected),
            "{message}"
        );
    }
}

fn pin(id: &str, revision: f64) -> Json {
    Json::object([
        ("objectId", id.into()),
        ("revision", Json::Number(revision)),
    ])
}
fn document(id: &str, revision: f64, value: Json) -> Document {
    Document {
        pin: pin(id, revision),
        content_hash: hash_json(&value).unwrap(),
        value,
        parent_id: None,
    }
}
struct World {
    task: Document,
    run: Document,
    result: Document,
    inspection: RefCell<Option<Inspection>>,
    reads: RefCell<Vec<(String, Json)>>,
    inspections: Cell<usize>,
    failure: RefCell<Option<IvyError>>,
}
impl World {
    fn new() -> Self {
        Self {
            task: document(
                "self",
                4.0,
                json(
                    r#"{"workflowState":"done","runtimeWait":{"reason":"host"},"primaryResourceRef":{"kind":"thread","namespace":"codex","nativeId":"thread","serviceNodeId":"agent"},"lastRun":{"objectId":"run","revision":2},"latestResult":{"objectId":"result","revision":3},"comments":[{"purpose":"question"}]}"#,
                ),
            ),
            run: document(
                "run",
                2.0,
                json(
                    r#"{"taskId":"self","phase":"failed","externalOutcome":{"state":"failed","code":"native_failed"},"cancellation":null,"turnId":"turn","nativeEpoch":"epoch","result":{"objectId":"result","revision":3},"primaryResourceRef":{"kind":"thread","namespace":"codex","nativeId":"thread","serviceNodeId":"agent"},"finishedAt":"2026-03-29T00:20:00.000Z"}"#,
                ),
            ),
            result: document(
                "result",
                3.0,
                json(r#"{"content":{"summary":"Native turn failed."}}"#),
            ),
            inspection: RefCell::new(Some(Inspection::Found {
                metadata: json(r#"{"reply":{"result":{"thread":{"status":{"type":"idle"}}}}}"#),
                turns: json(
                    r#"{"observedAt":"2026-03-29T00:20:00.000Z","reply":{"result":{"data":[{"id":"turn","status":"failed","error":{"codexErrorInfo":"serverOverloaded","message":""}}]}}}"#,
                ),
            })),
            reads: RefCell::default(),
            inspections: Cell::new(0),
            failure: RefCell::new(None),
        }
    }
    async fn retry(&self) -> IvyResult<Option<JsString>> {
        FailedRateLimit {
            store: self,
            inspector: self,
        }
        .retry_at(&self.task)
        .await
    }
}
impl RetryInspection for World {
    async fn find(&self, run: &Json) -> IvyResult<Inspection> {
        assert_eq!(run, &self.run.pin);
        self.inspections.set(self.inspections.get() + 1);
        if let Some(error) = self.failure.borrow().as_ref() {
            return Err(error.clone());
        }
        Ok(self
            .inspection
            .borrow_mut()
            .take()
            .expect("Only one inspection is allowed"))
    }
}
impl Store for World {
    fn root_object_id(&self) -> Option<JsString> {
        unreachable!()
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        unreachable!()
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        unreachable!()
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        unreachable!()
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        unreachable!()
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        unreachable!()
    }
    async fn read(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        assert!(primary);
        self.reads.borrow_mut().push((key.into(), address.clone()));
        let document = match key {
            "task-board/run" => &self.run,
            "task-board/result" => &self.result,
            _ => panic!("Unexpected read {key}"),
        };
        assert_eq!(address, &document.pin);
        Ok(document.clone())
    }
    async fn write(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        unreachable!()
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn current_host_wait_reinspects_exact_failed_run_without_foreign_question_gates() {
    let world = World::new();
    assert_eq!(
        world.retry().await.unwrap().unwrap().to_utf8().unwrap(),
        "2026-03-29T00:20:30.000Z"
    );
    assert_eq!(world.inspections.get(), 1);
    assert_eq!(
        world.reads.borrow().as_slice(),
        [
            ("task-board/run".into(), pin("run", 2.0)),
            ("task-board/result".into(), pin("result", 3.0))
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn retry_requires_current_runtime_wait_and_unchanged_original_provenance() {
    for (field, value) in [
        ("runtimeWait", Json::Null),
        ("primaryResourceRef", Json::Null),
        ("lastRun", Json::Null),
    ] {
        let mut world = World::new();
        world.task.value.insert(field, value);
        assert!(world.retry().await.unwrap().is_none());
        assert!(world.reads.borrow().is_empty());
    }
    for (field, value) in [
        ("taskId", "other".into()),
        ("phase", "completed".into()),
        ("turnId", Json::Null),
        ("nativeEpoch", Json::Null),
        ("cancellation", Json::object([] as [(&str, Json); 0])),
        ("result", pin("different", 3.0)),
        ("primaryResourceRef", Json::Null),
        (
            "externalOutcome",
            json(r#"{"state":"unknown","code":"native_failed"}"#),
        ),
    ] {
        let mut world = World::new();
        world.run.value.insert(field, value);
        assert!(world.retry().await.unwrap().is_none(), "{field}");
        assert_eq!(world.reads.borrow().len(), 1);
        assert_eq!(world.inspections.get(), 0);
    }
    let mut world = World::new();
    world
        .task
        .value
        .insert("runtimeWait", json(r#"{"reason":"external_outcome"}"#));
    assert!(world.retry().await.unwrap().is_none());
    assert!(world.reads.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn reinspection_preserves_native_errors_and_does_not_retry_unknown_observations() {
    let world = World::new();
    let failure = IvyError::new("original_failure", "Original inspection refusal")
        .with_outcome(Outcome::Unknown)
        .with_details(json(r#"{"identity":"saved"}"#));
    *world.failure.borrow_mut() = Some(failure.clone());
    let actual = world.retry().await.unwrap_err();
    assert_eq!(actual.code, failure.code);
    assert_eq!(actual.message, failure.message);
    assert_eq!(actual.outcome, failure.outcome);
    assert_eq!(actual.details, failure.details);
    for result in [
        Inspection::Absent,
        Inspection::Pending {
            search: pin("search", 1.0),
        },
    ] {
        let world = World::new();
        *world.inspection.borrow_mut() = Some(result);
        assert!(world.retry().await.unwrap().is_none());
    }
    let mut world = World::new();
    world
        .result
        .value
        .insert("content", json(r#"{"summary":"We will retry later."}"#));
    assert!(world.retry().await.unwrap().is_none());
    assert_eq!(world.inspections.get(), 0);
}
