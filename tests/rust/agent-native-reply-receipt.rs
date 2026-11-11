use super::*;
use futures_util::FutureExt;
use ivy_contracts::{IvyResult, JsString};
use ivy_host_runtime::codex_rpc::RequestOptions;
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
};
use tokio::sync::mpsc;
#[path = "agent-native-operation-admission.rs"]
mod admission_cases;
#[path = "agent-native-input-owner.rs"]
mod input_owner_cases;
#[path = "agent-native-run-results.rs"]
mod run_result_cases;

struct Fixture {
    _root: Root,
    operations: NativeOperations,
    receipts: ivy_agent_manager::native_operations::ReceiptWork,
    rpc: NativeRpc,
    peer: ivy_host_runtime::codex_rpc::NativeFrameReceiver,
    input: ivy_host_runtime::codex_rpc::NativeFrameSender,
    dispatch: NativeDispatch,
}

async fn release_frame(f: &mut Fixture, method: &str) -> ivy_host_runtime::codex_rpc::NativeFrame {
    let frame = tokio::time::timeout(Duration::from_secs(5), f.peer.recv())
        .await
        .unwrap_or_else(|error| panic!("Missing native {method} frame: {error}"))
        .unwrap();
    assert!(
        frame.required("method").unwrap().is_str(method),
        "Unexpected native method"
    );
    frame
}
fn release_reply(f: &Fixture, frame: &IvyJson, result: IvyJson) {
    f.input
        .try_send(IvyJson::object([
            ("id", frame.required("id").unwrap().clone()),
            ("result", result),
        ]))
        .unwrap();
}
fn release_identity(caller: &str, operation: &str) -> Identity {
    static ISSUED_AT: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let issued_at = ISSUED_AT.get_or_init(|| chrono::Utc::now().timestamp_millis());
    Identity {
        caller_principal_id: caller.into(),
        operation_id: format!("epoch:{issued_at}:{operation}").into(),
    }
}
fn release_params(expected: &str) -> IvyJson {
    IvyJson::object([
        ("threadId", "same-thread".into()),
        ("expectedEpoch", "epoch".into()),
        (
            "expectedOperationId",
            release_identity("one", expected).operation_id.into(),
        ),
    ])
}
fn release_thread(status: &str) -> IvyJson {
    IvyJson::object([(
        "thread",
        IvyJson::object([
            ("id", "same-thread".into()),
            ("status", IvyJson::object([("type", status.into())])),
        ]),
    )])
}
async fn release_resume(f: &mut Fixture, caller: &str, operation: &str) {
    let work = f
        .dispatch
        .invoke(
            release_identity(caller, operation),
            "thread/resume".into(),
            IvyJson::object([("threadId", "same-thread".into())]),
        )
        .unwrap();
    let observed = work.clone();
    let frame = tokio::select! {
        frame = release_frame(f, "thread/resume") => frame,
        outcome = observed.wait() => panic!("Resume ended before its native frame: {outcome:?}"),
    };
    release_reply(f, &frame, release_thread("idle"));
    assert!(
        work.wait()
            .await
            .unwrap()
            .required("phase")
            .unwrap()
            .is_str("succeeded")
    );
}

#[tokio::test]
async fn scoped_release_preserves_foreign_subscriber_and_orders_same_thread_resume_after_original_reply()
 {
    let mut f = release_fixture();
    let scenario = std::panic::AssertUnwindSafe(async {
        release_resume(&mut f, "one", "one-resume").await;
        release_resume(&mut f, "two", "two-resume").await;
        let identity = release_identity("one", "one-release");
        let params = release_params("one-resume");
        let work = f
            .dispatch
            .release(identity.clone(), params.clone())
            .unwrap();
        let read = release_frame(&mut f, "thread/read").await;
        assert_eq!(
            read.required("params").unwrap().get("includeTurns"),
            Some(&false.into())
        );
        release_reply(&f, &read, release_thread("idle"));
        let logical = work.wait().await.unwrap();
        assert!(
            logical
                .required("release")
                .unwrap()
                .required("kind")
                .unwrap()
                .is_str("logical")
        );
        assert!(
            logical.required("requestId").unwrap().is_null()
                && logical.required("reply").unwrap().is_null()
        );
        assert_eq!(
            f.operations.read(&identity).await.unwrap(),
            Some(logical.clone())
        );
        assert_eq!(
            f.dispatch
                .release(identity, params)
                .unwrap()
                .wait()
                .await
                .unwrap(),
            logical
        );
        {
            let memory = f.operations.journal();
            let journal = memory.lock().unwrap();
            assert!(
                journal
                    .thread_subscription(&"same-thread".into(), &"one".into())
                    .unwrap()
                    .is_none()
            );
            let foreign = journal
                .thread_subscription(&"same-thread".into(), &"two".into())
                .unwrap()
                .unwrap();
            assert!(foreign.attached && foreign.settled && !foreign.shared);
        }
        let identity = release_identity("two", "two-release");
        // Losing the waiter must neither cancel cleanup nor duplicate the frame.
        drop(
            f.dispatch
                .release(identity.clone(), release_params("two-resume"))
                .unwrap(),
        );
        let read = release_frame(&mut f, "thread/read").await;
        release_reply(&f, &read, release_thread("idle"));
        let unsubscribe = release_frame(&mut f, "thread/unsubscribe").await;
        assert_eq!(
            unsubscribe.required("params").unwrap(),
            &IvyJson::object([("threadId", "same-thread".into())])
        );
        let resumed = f
            .dispatch
            .invoke(
                release_identity("two", "new-work"),
                "thread/resume".into(),
                IvyJson::object([("threadId", "same-thread".into())]),
            )
            .unwrap();
        let unrelated = f
            .dispatch
            .invoke(
                release_identity("other", "other-work"),
                "thread/resume".into(),
                IvyJson::object([("threadId", "other-thread".into())]),
            )
            .unwrap();
        let frame = release_frame(&mut f, "thread/resume").await;
        assert!(
            frame
                .required("params")
                .unwrap()
                .required("threadId")
                .unwrap()
                .is_str("other-thread")
        );
        release_reply(
            &f,
            &frame,
            IvyJson::object([("thread", IvyJson::object([("id", "other-thread".into())]))]),
        );
        unrelated.wait().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), f.peer.recv())
                .await
                .is_err()
        );
        let original = IvyJson::object([("status", "unsubscribed".into())]);
        release_reply(&f, &unsubscribe, original.clone());
        let frame = release_frame(&mut f, "thread/resume").await;
        assert!(
            frame
                .required("params")
                .unwrap()
                .required("threadId")
                .unwrap()
                .is_str("same-thread")
        );
        release_reply(&f, &frame, release_thread("idle"));
        resumed.wait().await.unwrap();
        let saved = f.operations.read(&identity).await.unwrap().unwrap();
        assert!(
            saved
                .required("release")
                .unwrap()
                .required("kind")
                .unwrap()
                .is_str("native")
        );
        assert_eq!(
            saved.required("requestId").unwrap(),
            unsubscribe.required("id").unwrap()
        );
        assert_eq!(
            saved.required("reply").unwrap(),
            &IvyJson::object([("result", original)])
        );
        assert_eq!(
            f.dispatch
                .release(identity, release_params("two-resume"))
                .unwrap()
                .wait()
                .await
                .unwrap(),
            saved
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), f.peer.recv())
                .await
                .is_err()
        );
        let memory = f.operations.journal();
        let journal = memory.lock().unwrap();
        assert_eq!(
            journal
                .thread_subscription(&"same-thread".into(), &"two".into())
                .unwrap()
                .unwrap()
                .operation_id,
            &release_identity("two", "new-work").operation_id
        );
    })
    .catch_unwind()
    .await;
    f.rpc.close("test_complete").await;
    f.receipts.drain().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn scoped_release_keeps_active_inputs_and_newly_accepted_work_attached() {
    let mut f = release_fixture();
    let scenario = std::panic::AssertUnwindSafe(async {
        release_resume(&mut f, "one", "resume").await;
        let work = f
            .dispatch
            .release(
                release_identity("one", "active-release"),
                release_params("resume"),
            )
            .unwrap();
        let read = release_frame(&mut f, "thread/read").await;
        release_reply(&f, &read, release_thread("active"));
        let result = work.wait().await.unwrap();
        assert!(
            result
                .required("code")
                .unwrap()
                .is_str("native_thread_busy")
        );
        let work = f
            .dispatch
            .release(
                release_identity("one", "racing-release"),
                release_params("resume"),
            )
            .unwrap();
        let read = release_frame(&mut f, "thread/read").await;
        let resumed = f
            .dispatch
            .invoke(
                release_identity("one", "new-work"),
                "thread/resume".into(),
                IvyJson::object([("threadId", "same-thread".into())]),
            )
            .unwrap();
        release_reply(&f, &read, release_thread("idle"));
        let result = work.wait().await.unwrap();
        assert!(
            result
                .required("code")
                .unwrap()
                .is_str("native_thread_busy")
        );
        let frame = release_frame(&mut f, "thread/resume").await;
        release_reply(&f, &frame, release_thread("idle"));
        resumed.wait().await.unwrap();
        let work = f
            .dispatch
            .release(
                release_identity("one", "stale-release"),
                release_params("older-operation"),
            )
            .unwrap();
        assert!(
            work.wait()
                .await
                .unwrap()
                .required("code")
                .unwrap()
                .is_str("native_subscription_changed")
        );
        let memory = f.operations.journal();
        memory
            .lock()
            .unwrap()
            .observe_input(
                &IvyJson::object([
                    ("serviceNodeId", "fixture.agent".into()),
                    ("epoch", "epoch".into()),
                    ("requestId", 99.0.into()),
                ]),
                &"item/tool/requestUserInput".into(),
                &IvyJson::object([("threadId", "same-thread".into())]),
            )
            .unwrap();
        let work = f
            .dispatch
            .release(
                release_identity("one", "input-release"),
                release_params("new-work"),
            )
            .unwrap();
        assert!(
            work.wait()
                .await
                .unwrap()
                .required("code")
                .unwrap()
                .is_str("native_thread_busy")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), f.peer.recv())
                .await
                .is_err()
        );
        assert!(
            memory
                .lock()
                .unwrap()
                .thread_subscription(&"same-thread".into(), &"one".into())
                .unwrap()
                .is_some()
        );
        memory
            .lock()
            .unwrap()
            .resolve_input_json(&"epoch".into(), &99.0.into(), &"same-thread".into())
            .unwrap();
        let work = f
            .dispatch
            .release(
                release_identity("one", "native-error-release"),
                release_params("new-work"),
            )
            .unwrap();
        let read = release_frame(&mut f, "thread/read").await;
        release_reply(&f, &read, release_thread("idle"));
        let unsubscribe = release_frame(&mut f, "thread/unsubscribe").await;
        let error = IvyJson::object([
            ("code", (-32603.0).into()),
            ("message", "Fixture native rejection".into()),
        ]);
        f.input
            .try_send(IvyJson::object([
                ("id", unsubscribe.required("id").unwrap().clone()),
                ("error", error.clone()),
            ]))
            .unwrap();
        let result = work.wait().await.unwrap();
        assert!(result.required("code").unwrap().is_str("native_error"));
        assert_eq!(
            result.required("reply").unwrap(),
            &IvyJson::object([("error", error)])
        );
        let journal = memory.lock().unwrap();
        let own = journal
            .thread_subscription(&"same-thread".into(), &"one".into())
            .unwrap()
            .unwrap();
        assert!(own.attached && own.settled);
        assert_eq!(
            own.operation_id,
            &release_identity("one", "new-work").operation_id
        );
    })
    .catch_unwind()
    .await;
    f.rpc.close("test_complete").await;
    f.receipts.drain().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn scoped_release_preserves_unknown_original_on_connection_loss_without_blind_replay() {
    let mut f = release_fixture();
    let scenario = std::panic::AssertUnwindSafe(async {
        release_resume(&mut f, "one", "resume").await;
        let identity = release_identity("one", "unknown-release");
        let params = release_params("resume");
        let work = f
            .dispatch
            .release(identity.clone(), params.clone())
            .unwrap();
        let read = release_frame(&mut f, "thread/read").await;
        release_reply(&f, &read, release_thread("idle"));
        let unsubscribe = release_frame(&mut f, "thread/unsubscribe").await;
        f.rpc.close("test_connection_lost").await;
        let unknown = work.wait().await.unwrap();
        assert!(unknown.required("phase").unwrap().is_str("outcome_unknown"));
        assert_eq!(
            unknown.required("requestId").unwrap(),
            unsubscribe.required("id").unwrap()
        );
        assert!(unknown.required("reply").unwrap().is_null() && unknown.get("release").is_none());
        assert_eq!(
            f.operations.read(&identity).await.unwrap(),
            Some(unknown.clone())
        );
        assert_eq!(
            f.dispatch
                .release(identity, params)
                .unwrap()
                .wait()
                .await
                .unwrap(),
            unknown
        );
    })
    .catch_unwind()
    .await;
    f.rpc.close("test_complete").await;
    f.receipts.drain().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}
fn fixture(max_journal_bytes: usize) -> Fixture {
    fixture_with_store(max_journal_bytes, |state| {
        Arc::new(SqliteOperationStore::new(state).unwrap())
    })
}

#[tokio::test]
async fn scoped_release_management_binds_authenticated_caller_original_operation_and_native_definition()
 {
    use ivy_agent_manager::native_management::{NativeManagement, NativeManagementOwner};
    use ivy_sdk::service::InvocationContext;
    let mut f = release_fixture();
    let scenario = std::panic::AssertUnwindSafe(async {
        release_resume(&mut f, "one", "one-resume").await;
        release_resume(&mut f, "two", "two-resume").await;
        let catalog = Arc::new(
            parse(
                include_str!("../../specs/native/codex-0.163.0/catalog.json"),
                2 * 1024 * 1024,
            )
            .unwrap(),
        );
        let management = NativeManagement::new(
            NativeManagementOwner {
                backend: None,
                service_node_id: "fixture.agent".into(),
                epoch: "epoch".into(),
                catalog,
                rpc: f.rpc.clone(),
                closing: Arc::new(AtomicBool::new(false)),
                platform: "linux-x64".into(),
            },
            f.operations.clone(),
            ivy_agent_manager::interactions::NativeInteractions::new(
                "epoch".into(),
                Default::default(),
            ),
        )
        .unwrap();
        let definition = management
            .registry()
            .required("namespaces")
            .unwrap()
            .as_array()
            .unwrap()[0]
            .required("tools")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| {
                tool.get("nativeMethod")
                    .is_some_and(|method| method.is_str("thread/unsubscribe"))
            })
            .unwrap();
        let hash = ivy_contracts::hash::tool_definition_hash(definition).unwrap();
        let handler = management
            .handlers()
            .remove(&JsString::from("agent.release"))
            .unwrap();
        let params = IvyJson::object([
            (
                "operationId",
                release_identity("one", "release-api").operation_id.into(),
            ),
            ("nativeVersion", "0.163.0".into()),
            ("expectedDefinitionHash", hash.into()),
            ("threadId", "same-thread".into()),
            ("expectedEpoch", "epoch".into()),
            (
                "expectedOperationId",
                release_identity("one", "one-resume").operation_id.into(),
            ),
        ]);
        let context = |caller: &str, operation: &str| {
            InvocationContext::detached(
                caller.into(),
                Some(release_identity(caller, operation).operation_id),
                1,
                Default::default(),
                IvyError::internal(),
            )
        };
        assert_eq!(
            handler(params.clone(), context("one", "different-operation"))
                .await
                .unwrap_err()
                .code,
            "mutation_conflict"
        );
        let mut wrong = params.clone();
        wrong.insert("nativeVersion", "0.154.0".into());
        assert_eq!(
            handler(wrong, context("one", "release-api"))
                .await
                .unwrap_err()
                .code,
            "native_definition_mismatch"
        );
        let mut wrong = params.clone();
        wrong.insert(
            "expectedDefinitionHash",
            format!("sha256:{}", "f".repeat(64)).into(),
        );
        assert_eq!(
            handler(wrong, context("one", "release-api"))
                .await
                .unwrap_err()
                .code,
            "native_definition_mismatch"
        );
        let unknown = handler(params.clone(), context("unrelated", "release-api"))
            .await
            .unwrap();
        assert!(
            unknown
                .required("code")
                .unwrap()
                .is_str("native_subscription_unknown")
        );
        let work = tokio::spawn(handler(params, context("one", "release-api")));
        let read = release_frame(&mut f, "thread/read").await;
        release_reply(&f, &read, release_thread("idle"));
        let result = work.await.unwrap().unwrap();
        assert!(result.required("callerPrincipalId").unwrap().is_str("one"));
        assert!(
            result
                .required("release")
                .unwrap()
                .required("kind")
                .unwrap()
                .is_str("logical")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), f.peer.recv())
                .await
                .is_err()
        );
        assert!(
            f.operations
                .journal()
                .lock()
                .unwrap()
                .thread_subscription(&"same-thread".into(), &"two".into())
                .unwrap()
                .is_some()
        );
    })
    .catch_unwind()
    .await;
    f.rpc.close("test_complete").await;
    f.receipts.drain().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}
fn fixture_with_store(
    max_journal_bytes: usize,
    store: impl FnOnce(
        Arc<Mutex<LocalAgentState>>,
    ) -> Arc<dyn ivy_agent_manager::native_operations::NativeOperationStore>,
) -> Fixture {
    fixture_with_store_and_version(max_journal_bytes, store, "0.154.0")
}
fn release_fixture() -> Fixture {
    fixture_with_store_and_version(
        128 * 1024 * 1024,
        |state| Arc::new(SqliteOperationStore::new(state).unwrap()),
        "0.163.0",
    )
}
fn fixture_with_store_and_version(
    max_journal_bytes: usize,
    store: impl FnOnce(
        Arc<Mutex<LocalAgentState>>,
    ) -> Arc<dyn ivy_agent_manager::native_operations::NativeOperationStore>,
    version: &str,
) -> Fixture {
    let root = Root::new();
    let actual = root.0.canonicalize().unwrap();
    assert_eq!(
        actual.parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    assert!(
        actual
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ivy-agent-shutdown-")
    );
    assert!(
        !std::fs::symlink_metadata(&root.0)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let state = Arc::new(Mutex::new(
        LocalAgentState::open(
            &root.0.join("replies.sqlite"),
            "fixture.agent".into(),
            StateLimits {
                max_operations: 10,
                max_journal_bytes: i64::try_from(max_journal_bytes).unwrap(),
            },
        )
        .unwrap(),
    ));
    state
        .lock()
        .unwrap()
        .set_runtime_epoch("epoch".into())
        .unwrap();
    let journal = Arc::new(Mutex::new(
        NativeJournal::new(
            Owner {
                service_node_id: "fixture.agent".into(),
                native_version: version.into(),
                native_executable_hash: format!("sha256:{}", "0".repeat(64)).into(),
            },
            Limits {
                max_operations: 10,
                max_journal_bytes,
                max_pending_inputs: 8,
            },
        )
        .unwrap(),
    ));
    journal
        .lock()
        .unwrap()
        .begin_epoch(&"epoch".into())
        .unwrap();
    let operations = NativeOperations::new(store(state.clone()), journal.clone());
    let receipts = operations.reconcile_terminals(Arc::new(|error| panic!("{error:?}")));
    let catalog = parse(
        match version {
            "0.154.0" => include_str!("../../specs/native/codex-0.154.0/catalog.json"),
            "0.163.0" => include_str!("../../specs/native/codex-0.163.0/catalog.json"),
            _ => panic!("Unexpected fixture native version"),
        },
        2 * 1024 * 1024,
    )
    .unwrap();
    let (rpc, peer, input) = NativeRpc::start_frames(
        Arc::new(catalog),
        journal_events("epoch".into(), &operations, Observations::default()),
    )
    .unwrap();
    let dispatch = NativeDispatch::new(
        rpc.clone(),
        "epoch".into(),
        operations.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    Fixture {
        _root: root,
        operations,
        receipts,
        rpc,
        peer,
        input,
        dispatch,
    }
}

#[tokio::test]
async fn transient_native_subscribers_keep_caller_identity_after_observer_loss_and_late_reply() {
    let Fixture {
        _root,
        operations,
        receipts,
        rpc,
        mut peer,
        input,
        dispatch,
    } = fixture(64 * 1024 * 1024);
    let scenario = std::panic::AssertUnwindSafe(async {
        let interactions = ivy_agent_manager::interactions::NativeInteractions::new(
            "epoch".into(),
            Default::default(),
        );
        let thread = JsString::from("same-thread");
        for caller in ["one", "two"] {
            let identity = Identity {
                caller_principal_id: caller.into(),
                operation_id: format!("{caller}-resume").into(),
            };
            let owner = dispatch.clone();
            let observed = identity.clone();
            drop(
                interactions
                    .run(
                        identity.caller_principal_id.clone(),
                        identity.operation_id.clone(),
                        "thread/resume".into(),
                        IvyJson::object([("threadId", IvyJson::String(thread.clone()))]),
                        move |request| async move { owner.interact(observed, request).await },
                    )
                    .unwrap(),
            );
            let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(frame.required("method").unwrap().is_str("thread/resume"));
            let request_id = frame.required("id").unwrap().clone();
            {
                let memory = operations.journal();
                let journal = memory.lock().unwrap();
                let pending = journal
                    .thread_subscription(&thread, &identity.caller_principal_id)
                    .unwrap()
                    .unwrap();
                assert!(!pending.attached && !pending.settled);
                assert_eq!(pending.operation_id, &identity.operation_id);
            }
            input
                .try_send(IvyJson::object([
                    ("id", request_id),
                    (
                        "result",
                        IvyJson::object([(
                            "thread",
                            IvyJson::object([("id", IvyJson::String(thread.clone()))]),
                        )]),
                    ),
                ]))
                .unwrap();
            let result = interactions
                .lookup(&identity.caller_principal_id, &identity.operation_id)
                .unwrap()
                .wait()
                .await
                .unwrap();
            assert_eq!(
                result.required("operationId").unwrap(),
                &IvyJson::String(identity.operation_id.clone())
            );
            let memory = operations.journal();
            let journal = memory.lock().unwrap();
            let owned = journal
                .thread_subscription(&thread, &identity.caller_principal_id)
                .unwrap()
                .unwrap();
            assert!(owned.attached && owned.settled);
            assert_eq!(owned.shared, caller == "two");
            assert_eq!(owned.operation_id, &identity.operation_id);
        }
        let memory = operations.journal();
        let journal = memory.lock().unwrap();
        assert!(
            journal
                .thread_subscription(&thread, &"one".into())
                .unwrap()
                .unwrap()
                .shared
        );
        assert!(
            journal
                .thread_subscription(&thread, &"two".into())
                .unwrap()
                .unwrap()
                .shared
        );
    })
    .catch_unwind()
    .await;
    rpc.close("test_complete").await;
    receipts.drain().await.unwrap();
    assert!(
        operations
            .journal()
            .lock()
            .unwrap()
            .thread_subscription(&"same-thread".into(), &"one".into())
            .unwrap()
            .is_none()
    );
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn reply_budget_refusal_preserves_the_original_successful_receipt_and_never_redispatches() {
    let Fixture {
        _root,
        operations,
        receipts,
        rpc,
        mut peer,
        input,
        dispatch,
    } = fixture(64 * 1024 * 1024);
    let identity = Identity {
        caller_principal_id: "caller".into(),
        operation_id: format!(
            "epoch:{}:original-reply",
            chrono::Utc::now().timestamp_millis()
        )
        .into(),
    };
    let scenario = std::panic::AssertUnwindSafe(async {
        let (committed, mut observed) = mpsc::unbounded_channel();
        let mut originals: Vec<Pin<Box<dyn Future<Output = IvyResult<IvyJson>> + Send>>> = vec![];
        for index in 0..2 {
            let client = rpc.clone();
            let committed = committed.clone();
            let mut original: Pin<Box<dyn Future<Output = IvyResult<IvyJson>> + Send>> =
                Box::pin(async move {
                    client
                        .request(
                            "account/logout".into(),
                            IvyJson::Null,
                            RequestOptions {
                                request_id: Some(format!("raw-original-{index}").into()),
                                before_resolve: Some(Arc::new(move |_, _| {
                                    committed.send(index).unwrap();
                                    Box::pin(async { Ok(()) })
                                })),
                                ..Default::default()
                            },
                        )
                        .await
                });
            poll_fn(|cx| {
                assert!(original.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
                .await
                .unwrap()
                .unwrap();
            let mut units = Vec::with_capacity(12 * 1024 * 1024);
            units.extend([65, 0xd800]);
            input
                .try_send(IvyJson::object([
                    ("id", frame.get("id").unwrap().clone()),
                    ("result", IvyJson::String(JsString(units))),
                ]))
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), observed.recv())
                    .await
                    .unwrap()
                    .unwrap(),
                index
            );
            originals.push(original);
        }
        // Prove the very same RPC instance is refusing this retained reply size,
        // then exercise NativeDispatch's independent durable-result authority.
        let client = rpc.clone();
        let mut probe = Box::pin(async move {
            client
                .request(
                    "account/logout".into(),
                    IvyJson::Null,
                    RequestOptions {
                        request_id: Some("original-capacity-probe".into()),
                        ..Default::default()
                    },
                )
                .await
        });
        poll_fn(|cx| {
            assert!(probe.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.get("id"), Some(&"original-capacity-probe".into()));
        let mut probe_units = Vec::with_capacity(6 * 1024 * 1024);
        probe_units.extend([65, 0xd800]);
        input
            .try_send(IvyJson::object([
                ("id", frame.get("id").unwrap().clone()),
                ("result", IvyJson::String(JsString(probe_units))),
            ]))
            .unwrap();
        let refused = probe.await.unwrap_err();
        assert_eq!(refused.code, "native_capacity");
        assert_eq!(refused.outcome, Outcome::Unknown);
        let run = dispatch
            .invoke(identity.clone(), "account/logout".into(), IvyJson::Null)
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
            .await
            .unwrap()
            .unwrap();
        let original_request_id = frame.get("id").unwrap().clone();
        let mut units = Vec::with_capacity(6 * 1024 * 1024);
        units.extend([65, 0xd800]);
        input
            .try_send(IvyJson::object([
                ("id", original_request_id.clone()),
                ("result", IvyJson::String(JsString(units))),
            ]))
            .unwrap();
        let receipt = tokio::time::timeout(Duration::from_secs(5), run.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.get("phase"), Some(&"succeeded".into()));
        assert_eq!(receipt.get("requestId"), Some(&original_request_id));
        assert_eq!(
            receipt.get("operationId"),
            Some(&IvyJson::String(identity.operation_id.clone()))
        );
        assert_eq!(receipt.get("callerPrincipalId"), Some(&"caller".into()));
        assert_eq!(receipt.get("serviceNodeId"), Some(&"fixture.agent".into()));
        assert_eq!(receipt.get("nativeVersion"), Some(&"0.154.0".into()));
        assert_eq!(
            receipt
                .get("reply")
                .unwrap()
                .get("result")
                .unwrap()
                .as_js_string()
                .unwrap()
                .0,
            [65, 0xd800]
        );
        receipts.drain().await.unwrap();
        let saved = operations.read(&identity).await.unwrap().unwrap();
        for key in [
            "phase",
            "requestId",
            "operationId",
            "callerPrincipalId",
            "serviceNodeId",
            "nativeVersion",
            "requestHash",
            "reply",
        ] {
            assert_eq!(
                saved.get(key),
                receipt.get(key),
                "Original receipt field changed:{key}"
            );
        }
        let duplicate = dispatch
            .invoke(identity.clone(), "account/logout".into(), IvyJson::Null)
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(duplicate.get("reply"), receipt.get("reply"));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), peer.recv())
                .await
                .is_err(),
            "Known original receipt was replayed after reply-budget refusal"
        );
        assert!(rpc.connected());
        rpc.notify("initialized".into(), IvyJson::Null)
            .await
            .unwrap();
        assert_eq!(
            peer.recv().await.unwrap().get("method"),
            Some(&"initialized".into())
        );
        rpc.close("fixture-complete").await;
        for original in originals {
            let reply = original.await.unwrap();
            let text = reply.get("result").unwrap().as_js_string().unwrap();
            assert_eq!(text.0, [65, 0xd800]);
            assert_eq!(text.0.capacity(), 12 * 1024 * 1024);
        }
    })
    .catch_unwind()
    .await;
    rpc.close("test-complete").await;
    receipts.drain().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn command_budget_refusal_persists_the_original_unexecuted_receipt_without_replay() {
    let Fixture {
        _root,
        operations,
        receipts,
        rpc,
        mut peer,
        input,
        dispatch,
    } = fixture(64 * 1024 * 1024);
    let (entered, mut entering) = mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let permit = release.clone();
    let mut gate = Box::pin(rpc.request(
        "account/logout".into(),
        IvyJson::Null,
        RequestOptions {
            request_id: Some("gate".into()),
            before_send: Some(Arc::new(move |_| {
                let permit = permit.clone();
                entered.send(()).unwrap();
                Box::pin(async move {
                    permit.acquire().await.unwrap().forget();
                    Ok(())
                })
            })),
            ..Default::default()
        },
    ));
    poll_fn(|cx| {
        assert!(gate.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), entering.recv())
        .await
        .unwrap()
        .unwrap();
    let mut queued = Vec::new();
    for index in 0..55 {
        let client = rpc.clone();
        let mut call = Box::pin(async move {
            client
                .request(
                    "account/logout".into(),
                    IvyJson::Null,
                    RequestOptions {
                        request_id: Some(format!("queued-{index}").into()),
                        ..Default::default()
                    },
                )
                .await
        });
        poll_fn(|cx| {
            assert!(call.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        queued.push(call);
    }
    let identity = Identity {
        caller_principal_id: "caller".into(),
        operation_id: format!(
            "epoch:{}:original-command",
            chrono::Utc::now().timestamp_millis()
        )
        .into(),
    };
    let scenario = std::panic::AssertUnwindSafe(async {
        drop(
            dispatch
                .invoke(identity.clone(), "account/logout".into(), IvyJson::Null)
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(5), operations.settle(&identity))
            .await
            .unwrap()
            .unwrap();
        receipts.drain().await.unwrap();
        let receipt = operations.read(&identity).await.unwrap().unwrap();
        assert_eq!(receipt.get("phase"), Some(&"failed".into()));
        assert_eq!(receipt.get("code"), Some(&"native_capacity".into()));
        assert_eq!(receipt.get("requestId"), Some(&IvyJson::Null));
        assert_eq!(receipt.get("reply"), Some(&IvyJson::Null));
        assert_eq!(
            receipt.get("operationId"),
            Some(&IvyJson::String(identity.operation_id.clone()))
        );
        assert_eq!(receipt.get("callerPrincipalId"), Some(&"caller".into()));
        assert_eq!(receipt.get("serviceNodeId"), Some(&"fixture.agent".into()));
        assert_eq!(receipt.get("nativeVersion"), Some(&"0.154.0".into()));
        release.add_permits(1);
        for index in 0..56 {
            let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
                .await
                .unwrap()
                .unwrap();
            let id = if index == 0 {
                "gate".to_owned()
            } else {
                format!("queued-{}", index - 1)
            };
            assert_eq!(frame.get("id"), Some(&IvyJson::from(id)));
        }
        for (method, params) in [
            (
                "turn/interrupt",
                IvyJson::object([
                    ("threadId", "original-thread".into()),
                    ("turnId", "original-turn".into()),
                ]),
            ),
            (
                "thread/unsubscribe",
                IvyJson::object([("threadId", "original-thread".into())]),
            ),
        ] {
            let control_identity = Identity {
                caller_principal_id: identity.caller_principal_id.clone(),
                operation_id: format!(
                    "epoch:{}:control-{}",
                    chrono::Utc::now().timestamp_millis(),
                    method.replace('/', "-")
                )
                .into(),
            };
            let run = dispatch
                .invoke(control_identity.clone(), method.into(), params.clone())
                .unwrap();
            let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
                .await
                .expect("An original cleanup action could not use the reserved control capacity")
                .unwrap();
            assert_eq!(frame.get("method"), Some(&IvyJson::from(method)));
            assert_eq!(frame.get("params"), Some(&params));
            let request_id = frame.required("id").unwrap().clone();
            input
                .try_send(IvyJson::object([
                    ("id", request_id.clone()),
                    ("result", IvyJson::Null),
                ]))
                .unwrap();
            drop(frame);
            let saved = tokio::time::timeout(Duration::from_secs(5), run.wait())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(saved.get("phase"), Some(&"succeeded".into()));
            assert_eq!(
                saved.get("operationId"),
                Some(&IvyJson::String(control_identity.operation_id.clone()))
            );
            assert_eq!(saved.get("requestId"), Some(&request_id));
            assert_eq!(
                operations.read(&control_identity).await.unwrap(),
                Some(saved)
            );
        }
        // The original failure remains authoritative after all command capacity
        // has returned; re-observation must not turn it into another native call.
        let duplicate = dispatch
            .invoke(identity.clone(), "account/logout".into(), IvyJson::Null)
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(hash_json(&duplicate).unwrap(), hash_json(&receipt).unwrap());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), peer.recv())
                .await
                .is_err()
        );
        rpc.close("fixture_complete").await;
        assert_eq!(gate.await.unwrap_err().outcome, Outcome::Unknown);
        for call in queued {
            assert_eq!(call.await.unwrap_err().outcome, Outcome::Unknown);
        }
        receipts.drain().await.unwrap();
        assert_eq!(
            hash_json(&operations.read(&identity).await.unwrap().unwrap()).unwrap(),
            hash_json(&receipt).unwrap()
        );
    })
    .catch_unwind()
    .await;
    release.add_permits(1);
    rpc.close("test_complete").await;
    receipts.drain().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}
