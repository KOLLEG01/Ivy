use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_task_board::journal_store::persistent_reads::{
    fill_verification_sizes, read_legacy_conversation,
    record_upgrades::{upgrade_stored, upgrade_stored_with},
    verification_sizes::fill_verification_sizes_with,
};
use std::{collections::VecDeque, sync::Mutex};

fn artifact(id: &str) -> Json {
    Json::object([
        (
            "object",
            Json::object([("objectId", id.into()), ("revision", Json::Number(7.0))]),
        ),
        ("contentHash", "sha256:immutable".into()),
        ("mediaType", "application/octet-stream".into()),
    ])
}
fn saved() -> Json {
    Json::object([(
        "revision",
        Json::object([
            ("contentHash", "sha256:immutable".into()),
            ("mediaType", "application/octet-stream".into()),
            ("byteLength", Json::Number(17.0)),
        ]),
    )])
}
struct Client {
    pending: Mutex<VecDeque<IvyResult<Json>>>,
    trace: Mutex<Vec<Json>>,
}
impl Client {
    fn new(values: impl IntoIterator<Item = IvyResult<Json>>) -> Self {
        Self {
            pending: Mutex::new(values.into_iter().collect()),
            trace: Mutex::default(),
        }
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(method, "objects.read");
            self.trace.lock().unwrap().push(params);
            self.pending
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected read")
        })
    }
}
#[tokio::test(flavor = "current_thread")]
async fn retained_sizes_cover_checks_comments_and_deliveries_without_mutating_the_source() {
    let finding = |id| Json::object([("evidence", Json::Array(vec![artifact(id)]))]);
    let mut existing = artifact("present");
    existing.insert("byteLength", Json::Number(99.0));
    let input = Json::object([
        ("latestChecks", Json::Array(vec![finding("latest")])),
        (
            "content",
            Json::object([("checks", Json::Array(vec![finding("content")]))]),
        ),
        (
            "comments",
            Json::Array(vec![Json::object([(
                "handoff",
                Json::object([("checks", Json::Array(vec![finding("comment")]))]),
            )])]),
        ),
        (
            "deliveries",
            Json::Array(vec![Json::object([(
                "delivery",
                Json::object([(
                    "artifacts",
                    Json::Array(vec![artifact("delivery"), existing]),
                )]),
            )])]),
        ),
    ]);
    let before = input.clone();
    let client = Client::new((0..4).map(|_| Ok(saved())));
    let full = fill_verification_sizes(&client, "task-board/task", &input)
        .await
        .unwrap();
    assert_eq!(input, before);
    let ids: Vec<JsString> = client
        .trace
        .lock()
        .unwrap()
        .iter()
        .map(|pin| {
            pin.required("objectId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone()
        })
        .collect();
    assert_eq!(
        ids,
        ["latest", "content", "comment", "delivery"].map(JsString::from)
    );
    assert!(client.pending.lock().unwrap().is_empty());
    let artifacts = full.required("deliveries").unwrap().as_array().unwrap()[0]
        .required("delivery")
        .unwrap()
        .required("artifacts")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(artifacts[0].get("byteLength"), Some(&Json::Number(17.0)));
    assert_eq!(artifacts[1].get("byteLength"), Some(&Json::Number(99.0)));
}
#[tokio::test(flavor = "current_thread")]
async fn evidence_mismatch_stops_before_a_later_read_and_retains_source_errors() {
    let input = Json::object([(
        "deliveries",
        Json::Array(vec![Json::object([(
            "delivery",
            Json::object([(
                "artifacts",
                Json::Array(vec![artifact("first"), artifact("second")]),
            )]),
        )])]),
    )]);
    let mut wrong = saved();
    let mut revision = wrong.remove("revision").unwrap();
    revision.insert("contentHash", "sha256:different".into());
    wrong.insert("revision", revision);
    let client = Client::new([Ok(wrong)]);
    let error = fill_verification_sizes(&client, "task-board/task", &input)
        .await
        .unwrap_err();
    assert_eq!(error.code, "task_board_evidence_mismatch");
    assert_eq!(client.trace.lock().unwrap().len(), 1);
    let failure = IvyError::new("source_offline", "original source")
        .with_provider_outcome(Json::object([("committed", false.into())]));
    let client = Client::new([Err(failure.clone())]);
    let result = fill_verification_sizes(&client, "task-board/task", &input)
        .await
        .unwrap_err();
    assert_eq!(result, failure);
    // Generic callers can retain a borrowed, non-Send cause without a JS exception adapter.
    enum Failure<'a> {
        Ivy(IvyError),
        Source(&'a std::cell::Cell<usize>),
    }
    impl From<IvyError> for Failure<'_> {
        fn from(e: IvyError) -> Self {
            Self::Ivy(e)
        }
    }
    let witness = std::cell::Cell::new(0);
    let output = fill_verification_sizes_with("task-board/task", &input, |_| async {
        Err::<Json, _>(Failure::Source(&witness))
    })
    .await;
    match output {
        Err(Failure::Source(actual)) => assert!(std::ptr::eq(actual, &witness)),
        Err(Failure::Ivy(e)) => panic!("{e:?}"),
        _ => panic!("expected source error"),
    }
}
#[tokio::test(flavor = "current_thread")]
async fn shared_record_upgrades_preserve_retained_findings_and_unrelated_records() {
    let client = Client::new([]);
    let input = Json::object([(
        "content",
        Json::object([
            ("summary", "Work".into()),
            ("artifacts", Json::Array(vec![])),
            (
                "checks",
                Json::Array(vec![Json::object([
                    ("summary", "Verified".into()),
                    ("evidence", Json::Array(vec![])),
                ])]),
            ),
        ]),
    )]);
    let upgraded = upgrade_stored(&client, "task-board/result", &input)
        .await
        .unwrap()
        .into_owned();
    let content = upgraded.required("content").unwrap();
    assert!(content.get("checks").is_none());
    assert!(
        content
            .required("summary")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("Verified")
    );
    let run = Json::object([(
        "executionContext",
        Json::object([
            ("workflowState", "todo".into()),
            ("latestResult", Json::Null),
            ("acceptedReview", Json::Null),
            ("nextSteps", Json::Array(vec![])),
        ]),
    )]);
    assert_eq!(
        upgrade_stored(&client, "task-board/run", &run)
            .await
            .unwrap()
            .into_owned(),
        ivy_sdk::upgrade_run_record(&run).unwrap()
    );
    let history = Json::object([("event", "unchanged".into())]);
    let result = upgrade_stored(&client, "task-board/history", &history)
        .await
        .unwrap();
    assert!(matches!(result,std::borrow::Cow::Borrowed(value) if std::ptr::eq(value,&history)));
    assert!(client.trace.lock().unwrap().is_empty());
    let invalid = Json::object([("content", Json::object([("checks", Json::Array(vec![]))]))]);
    // A missing summary is rejected by the SDK when a finding requires it.
    let mut invalid_content = invalid.required("content").unwrap().clone();
    invalid_content.insert(
        "checks",
        input
            .required("content")
            .unwrap()
            .required("checks")
            .unwrap()
            .clone(),
    );
    let invalid = Json::object([("content", invalid_content)]);
    let error = upgrade_stored_with::<IvyError, _, _>("task-board/result", &invalid, |_| async {
        panic!("unexpected read")
    })
    .await
    .unwrap_err();
    assert_eq!(error, ivy_sdk::upgrade_result_record(&invalid).unwrap_err());
}
#[tokio::test(flavor = "current_thread")]
async fn retained_comment_prefix_checks_scope_length_and_native_state_overlays() {
    let state = Json::object([
        ("commentId", "a".into()),
        ("nativeInput", Json::object([("state", "answered".into())])),
    ]);
    let task = Json::object([
        ("title", "kept".into()),
        (
            "commentStore",
            Json::object([
                ("objectId", "log".into()),
                ("length", Json::Number(1.0)),
                ("nativeInputs", Json::Array(vec![state.clone()])),
            ]),
        ),
    ]);
    let comment = Json::object([
        ("commentId", "a".into()),
        ("body", "retained".into()),
        ("delivery", "obsolete".into()),
        ("handoff", "obsolete".into()),
    ]);
    let log = Json::object([
        (
            "object",
            Json::object([
                ("contractKey", "task-board/comments".into()),
                ("ownerObjectId", Json::Null),
                ("parentId", "task".into()),
            ]),
        ),
        (
            "content",
            Json::object([
                ("encoding", "json".into()),
                (
                    "value",
                    Json::object([
                        ("taskId", "task".into()),
                        (
                            "comments",
                            Json::Array(vec![
                                comment,
                                Json::object([("commentId", "later".into())]),
                            ]),
                        ),
                    ]),
                ),
            ]),
        ),
    ]);
    let before = task.clone();
    let client = Client::new([Ok(log.clone())]);
    let full = read_legacy_conversation(&client, &task, &"task".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task, before);
    assert!(full.get("commentStore").is_none());
    assert_eq!(full.get("title"), Some(&"kept".into()));
    let comments = full.required("comments").unwrap().as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].get("nativeInput"), state.get("nativeInput"));
    assert!(comments[0].get("delivery").is_none() && comments[0].get("handoff").is_none());
    assert_eq!(
        client.trace.lock().unwrap()[0],
        Json::object([("objectId", "log".into())])
    );
    let client = Client::new([Ok(log.clone())]);
    assert_eq!(
        read_legacy_conversation(&client, &task, &"other".into())
            .await
            .unwrap_err()
            .code,
        "task_board_evidence_mismatch"
    );
    let mut too_long = task.clone();
    let mut reference = too_long.remove("commentStore").unwrap();
    reference.insert("length", Json::Number(3.0));
    too_long.insert("commentStore", reference);
    let client = Client::new([Ok(log)]);
    assert_eq!(
        read_legacy_conversation(&client, &too_long, &"task".into())
            .await
            .unwrap_err()
            .code,
        "task_board_evidence_mismatch"
    );
}
