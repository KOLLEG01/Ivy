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
#[path = "agent-native-input-owner.rs"]
mod input_owner_cases;
#[path = "agent-native-operation-admission.rs"]
mod admission_cases;
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
fn fixture(max_journal_bytes: usize) -> Fixture {
    fixture_with_store(max_journal_bytes, |state| {
        Arc::new(SqliteOperationStore::new(state).unwrap())
    })
}
fn fixture_with_store(
    max_journal_bytes: usize,
    store: impl FnOnce(
        Arc<Mutex<LocalAgentState>>,
    ) -> Arc<dyn ivy_agent_manager::native_operations::NativeOperationStore>,
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
                native_version: "0.154.0".into(),
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
        include_str!("../../specs/native/codex-0.154.0/catalog.json"),
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
