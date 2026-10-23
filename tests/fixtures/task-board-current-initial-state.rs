use super::*;
use crate::{
    engine_read::read_failure::{ReadCause, ReadFailure},
    wake::WakeStore,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
struct Inputs {
    calls: RefCell<Vec<JsString>>,
}
impl WakeStore for Inputs {
    async fn watched_task(&self, id: &JsString) -> IvyResult<Json> {
        self.calls.borrow_mut().push(id.clone());
        Ok(Json::object([
            (
                "metadata",
                Json::object([("effectivelyArchived", true.into())]),
            ),
            (
                "value",
                Json::object([
                    ("workRevision", Json::Number(3.0)),
                    ("acceptedReview", Json::Null),
                    ("workflowState", "waiting".into()),
                ]),
            ),
        ]))
    }
    fn deleted(&self, _: &IvyError) -> bool {
        false
    }
}
#[tokio::test(flavor = "current_thread")]
async fn creation_records_current_wake_and_watches_without_foreign_state_gates() {
    let inputs = Inputs {
        calls: RefCell::default(),
    };
    let fields = Json::object([("control", "agent".into())]);
    let actor = Json::object([("source", "user".into())]);
    for state in ["todo", "waiting", "backlog"] {
        let request = Json::object([
            ("workflowState", state.into()),
            ("dependencies", Json::Array(vec!["peer".into()])),
            ("wakeAt", "2027-01-01T00:00:00.000Z".into()),
        ]);
        let result = current_initial_state(&inputs, &request, &fields, &actor, async {
            panic!("No initial result")
        })
        .await
        .unwrap();
        assert_eq!(result.get("workflowState"), Some(&state.into()));
        assert_eq!(result.get("wakeAt"), request.get("wakeAt"));
        assert_eq!(result.get("dependencies"), request.get("dependencies"));
        assert!(
            result
                .required("observedDependencies")
                .unwrap()
                .get("peer")
                .is_some()
        );
        assert!(result.get("nextSteps").is_none());
        assert_eq!(result.get("runtimeWait"), Some(&Json::Null));
    }
    assert_eq!(
        inputs.calls.borrow().len(),
        6,
        "Each initial watch is validated then snapshotted"
    );
    let empty = current_initial_state(
        &inputs,
        &Json::object([] as [(&str, Json); 0]),
        &fields,
        &actor,
        async { unreachable!() },
    )
    .await
    .unwrap();
    assert_eq!(empty.get("workflowState"), Some(&"todo".into()));
    assert_eq!(empty.get("dependencies"), Some(&Json::Array(vec![])));
    assert_eq!(empty.get("wakeAt"), Some(&Json::Null));
}
#[tokio::test(flavor = "current_thread")]
async fn creation_checks_manual_material_before_dates_and_watch_reads() {
    let inputs = Inputs {
        calls: RefCell::default(),
    };
    let content_calls = Cell::new(0);
    let actor = Json::object([("source", "user".into())]);
    for (control, state, material, code) in [
        ("agent", "in_progress", false, "task_board_user_controlled"),
        (
            "agent",
            "review",
            true,
            "task_board_native_evidence_required",
        ),
        ("user", "review", false, "task_board_result_required"),
        ("user", "todo", true, "invalid_arguments"),
        ("user", "review", true, "fixture_content"),
    ] {
        let fields = Json::object([("control", control.into())]);
        let mut request = Json::object([
            ("workflowState", state.into()),
            ("wakeAt", "bad".into()),
            ("dependencies", Json::Array(vec!["peer".into()])),
        ]);
        if material {
            request.insert("initialResult", Json::object([] as [(&str, Json); 0]));
        }
        let result = current_initial_state(&inputs, &request, &fields, &actor, async {
            content_calls.set(content_calls.get() + 1);
            tokio::task::yield_now().await;
            Err(IvyError::new(
                "fixture_content",
                "Refused original material.",
            ))
        })
        .await
        .unwrap_err();
        assert_eq!(result.code, code);
    }
    assert_eq!(content_calls.get(), 1);
    assert!(inputs.calls.borrow().is_empty());
    let fields = Json::object([("control", "agent".into())]);
    let result = current_initial_state(
        &inputs,
        &Json::object([("wakeAt", "bad".into())]),
        &fields,
        &actor,
        async { Ok(()) },
    )
    .await
    .unwrap_err();
    assert_eq!(result.code, "task_board_invalid_date");
}
struct Opaque<'a> {
    borrowed: &'a str,
    token: Rc<()>,
}
struct Failed<'a>(ReadFailure<Opaque<'a>>);
impl<'a> WakeStore<ReadFailure<Opaque<'a>>> for Failed<'a> {
    async fn watched_task(&self, _: &JsString) -> Result<Json, ReadFailure<Opaque<'a>>> {
        Err(self.0.clone())
    }
    fn deleted(&self, _: &ReadFailure<Opaque<'a>>) -> bool {
        false
    }
}
#[tokio::test(flavor = "current_thread")]
async fn creation_dependency_failure_preserves_the_borrowed_native_cause() {
    let borrowed = String::from("Original native read error");
    let token = Rc::new(());
    let store = Failed(ReadFailure::source(Opaque {
        borrowed: &borrowed,
        token: token.clone(),
    }));
    let result = current_initial_state(
        &store,
        &Json::object([("dependencies", Json::Array(vec!["peer".into()]))]),
        &Json::object([("control", "agent".into())]),
        &Json::object([("source", "user".into())]),
        async { Ok(()) },
    )
    .await
    .unwrap_err();
    assert!(result.ptr_eq(&store.0));
    let ReadCause::Source(cause) = result.cause() else {
        panic!("Erased source")
    };
    assert_eq!(cause.borrowed, borrowed);
    assert!(Rc::ptr_eq(&cause.token, &token));
}

struct UnusedContracts;
impl Contracts for UnusedContracts {
    fn version(&self, _: &str) -> IvyResult<String> {
        unreachable!("No contract lookup in prefix control")
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        unreachable!()
    }
    fn readable_version(&self, _: &str, _: &JsString) -> IvyResult<bool> {
        unreachable!()
    }
    fn validate_record(&self, _: &str, _: &JsString, _: &Json) -> IvyResult<()> {
        unreachable!()
    }
    fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
        unreachable!()
    }
}
struct NoPages;
impl crate::lifecycle_journal::LifecyclePages for NoPages {
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
}
struct ArtifactSource {
    calls: std::sync::Mutex<Vec<Json>>,
    gate: tokio::sync::Notify,
    refuse: bool,
}
impl RpcClient for ArtifactSource {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> ivy_sdk::rpc::RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(method, "objects.read");
            self.calls.lock().unwrap().push(params);
            self.gate.notified().await;
            if self.refuse {
                return Err(IvyError::new("fixture_read", "Artifact refused."));
            }
            Ok(Json::object([
                (
                    "object",
                    Json::object([("effectivelyArchived", false.into())]),
                ),
                (
                    "revision",
                    Json::object([
                        ("revision", Json::Number(1.0)),
                        ("contentHash", digest(b"a").into()),
                        ("mediaType", "text/plain".into()),
                        ("byteLength", Json::Number(1.0)),
                    ]),
                ),
                (
                    "content",
                    Json::object([("encoding", "utf8".into()), ("value", "a".into())]),
                ),
            ]))
        })
    }
}
#[tokio::test(flavor = "current_thread")]
async fn current_prefixes_keep_their_own_fields_and_artifact_await_order() {
    use crate::{
        journal_store::JournalStore,
        lifecycle_journal::LifecycleJournal,
        local_journal::{LocalJournal, initialize},
        local_retention::HistoryLimits,
    };
    use std::{
        future::{Future, poll_fn},
        path::Path,
        task::Poll,
    };
    for refuse in [false, true] {
        let connection = ivy_store::rusqlite::Connection::open_in_memory().unwrap();
        initialize(&connection).unwrap();
        let source = Arc::new(ArtifactSource {
            calls: Default::default(),
            gate: tokio::sync::Notify::new(),
            refuse,
        });
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &connection,
                root_object_id: None,
            },
            client: source.clone(),
            contracts: UnusedContracts,
            limits: HistoryLimits::default(),
            now: || "2026-10-22T06:00:00.000Z".to_owned(),
            new_id: || "unused".into(),
            cache: None,
        };
        let store = LifecycleJournal {
            journal: &journal,
            pages: &NoPages,
        };
        let tools = ToolClients::new(source.clone());
        let catalogs =
            CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        let checks = TaskChecks {
            store: &store,
            tools: &tools,
            catalogs: &catalogs,
        };
        let fields = Json::object([
            ("dueAt", Json::Null),
            ("category", Json::Null),
            (
                "workspaceRequirement",
                Json::object([
                    ("kind", "directory_path".into()),
                    ("path", "/fixture".into()),
                ]),
            ),
        ]);
        checks.current_fields(&fields).await.unwrap();
        checks.fields(&fields, None).await.unwrap();
        let mut invalid = fields.clone();
        invalid.insert("dueAt", "bad".into());
        invalid.insert("category", " padded ".into());
        assert_eq!(
            checks.current_fields(&invalid).await.unwrap_err().code,
            "task_board_invalid_date"
        );
        invalid.insert("dueAt", Json::Null);
        assert_eq!(
            checks.current_fields(&invalid).await.unwrap_err().code,
            "invalid_arguments"
        );
        let content = Json::object([
            (
                "artifacts",
                Json::Array(vec![Json::object([
                    (
                        "object",
                        Json::object([
                            ("objectId", "artifact".into()),
                            ("revision", Json::Number(1.0)),
                        ]),
                    ),
                    ("contentHash", digest(b"a").into()),
                    ("mediaType", "text/plain".into()),
                ])]),
            ),
            (
                "checks",
                "Legacy traversal would fail before the artifact await".into(),
            ),
            (
                "codeReferences",
                Json::Array(vec![Json::object([("url", "bad url".into())])]),
            ),
        ]);
        assert_eq!(
            checks.content(&content).await.unwrap_err().code,
            "internal_error"
        );
        assert!(source.calls.lock().unwrap().is_empty());
        let mut current = Box::pin(checks.current_content(&content));
        poll_fn(|cx| {
            assert!(current.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(source.calls.lock().unwrap().len(), 1);
        source.gate.notify_one();
        let error = current.await.unwrap_err();
        assert_eq!(
            error.code,
            if refuse {
                "fixture_read"
            } else {
                "task_board_invalid_reference"
            }
        );
    }
}
