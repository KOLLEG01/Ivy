use super::*;
use ivy_agent_manager::{
    local_state::{Pin as StatePin, SavedState},
    native_operations::{NativeOperationStore, StoreFuture},
};

fn identity(suffix: &str) -> Identity {
    Identity {
        caller_principal_id: "caller".into(),
        operation_id: format!("epoch:{}:{suffix}", chrono::Utc::now().timestamp_millis()).into(),
    }
}
fn payload() -> (IvyJson, usize, usize) {
    let mut units = Vec::with_capacity(1024 * 1024);
    units.extend([65, 0xd800, 0xdc01, 0xdfff]);
    let pointer = units.as_ptr() as usize;
    let capacity = units.capacity();
    (
        IvyJson::object([
            (
                "nested",
                IvyJson::object([("text", IvyJson::String(JsString(units)))]),
            ),
            (
                "numbers",
                IvyJson::Array(vec![(-0.0).into(), 9_007_199_254_740_991.0.into()]),
            ),
        ]),
        pointer,
        capacity,
    )
}
fn text(value: &IvyJson) -> &JsString {
    value
        .required("nested")
        .unwrap()
        .required("text")
        .unwrap()
        .as_js_string()
        .unwrap()
}
async fn saved(operations: &NativeOperations, identity: &Identity, phase: &str) -> IvyJson {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(value) = operations.read(identity).await.unwrap() {
                if value.get("phase") == Some(&IvyJson::from(phase)) {
                    return value;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn invoke_moves_original_parameters_after_the_durable_claim_even_with_a_dropped_waiter() {
    let Fixture {
        _root,
        operations,
        receipts,
        rpc,
        mut peer,
        input,
        dispatch,
    } = fixture(128 * 1024 * 1024);
    let original = identity("owned-invoke");
    let (params, pointer, capacity) = payload();
    let replay = params.clone();
    let expected_hash = hash_json(&IvyJson::object([
        ("method", "account/logout".into()),
        ("params", replay.clone()),
    ]))
    .unwrap();
    let scenario = std::panic::AssertUnwindSafe(async {
        drop(
            dispatch
                .invoke(original.clone(), "account/logout".into(), params)
                .unwrap(),
        );
        let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
            .await
            .unwrap()
            .unwrap();
        let original_id = frame.required("id").unwrap().clone();
        let claim = saved(&operations, &original, "dispatched").await;
        assert_eq!(claim.get("requestId"), Some(&original_id));
        assert_eq!(
            claim.get("requestHash"),
            Some(&IvyJson::from(expected_hash.clone()))
        );
        let forwarded = frame.required("params").unwrap();
        assert_eq!(hash_json(forwarded).unwrap(), hash_json(&replay).unwrap());
        assert_eq!(text(forwarded).0.as_slice(), &[65, 0xd800, 0xdc01, 0xdfff]);
        assert_eq!(
            text(forwarded).0.as_ptr() as usize,
            pointer,
            "Dispatch copied the caller's parameter allocation"
        );
        assert_eq!(text(forwarded).0.capacity(), capacity);
        input
            .try_send(IvyJson::object([
                ("id", original_id.clone()),
                ("result", IvyJson::Null),
            ]))
            .unwrap();
        drop(frame);
        operations.settle(&original).await.unwrap();
        receipts.drain().await.unwrap();
        let receipt = saved(&operations, &original, "succeeded").await;
        assert_eq!(receipt.get("requestId"), Some(&original_id));
        assert_eq!(
            receipt.get("requestHash"),
            Some(&IvyJson::from(expected_hash))
        );
        assert_eq!(
            receipt.get("operationId"),
            Some(&IvyJson::String(original.operation_id.clone()))
        );
        assert_eq!(
            hash_json(receipt.required("params").unwrap()).unwrap(),
            hash_json(&replay).unwrap()
        );
        let duplicate = dispatch
            .invoke(original.clone(), "account/logout".into(), replay)
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
    })
    .catch_unwind()
    .await;
    rpc.close("fixture_complete").await;
    receipts.drain().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn answer_moves_the_original_reply_and_preserves_the_input_owner_through_resolution() {
    let Fixture {
        _root,
        operations,
        receipts,
        rpc,
        mut peer,
        input,
        dispatch,
    } = fixture(128 * 1024 * 1024);
    let original = identity("owned-answer");
    let request_id = IvyJson::String(JsString(vec![66, 0xdfff]));
    let input_identity = operations
        .journal()
        .lock()
        .unwrap()
        .input_identity(&"epoch".into(), &request_id);
    input
        .try_send(IvyJson::object([
            ("id", request_id.clone()),
            ("method", "item/commandExecution/requestApproval".into()),
            (
                "params",
                IvyJson::object([
                    ("threadId", "original-thread".into()),
                    ("turnId", "original-turn".into()),
                ]),
            ),
        ]))
        .unwrap();
    let (value, pointer, capacity) = payload();
    let reply = IvyJson::object([("result", value)]);
    let replay = reply.clone();
    let expected_hash = hash_json(&IvyJson::object([
        ("method", "agent.answer".into()),
        (
            "params",
            IvyJson::object([
                ("identity", input_identity.clone()),
                ("reply", replay.clone()),
            ]),
        ),
    ]))
    .unwrap();
    let scenario = std::panic::AssertUnwindSafe(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while operations
                .journal()
                .lock()
                .unwrap()
                .input(&input_identity)
                .unwrap()
                .is_none()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(
            dispatch
                .answer(original.clone(), input_identity.clone(), reply)
                .unwrap(),
        );
        let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.get("id"), Some(&request_id));
        let forwarded = frame.required("result").unwrap();
        assert_eq!(
            hash_json(forwarded).unwrap(),
            hash_json(replay.required("result").unwrap()).unwrap()
        );
        assert_eq!(
            text(forwarded).0.as_ptr() as usize,
            pointer,
            "Dispatch or RPC copied the caller's reply allocation"
        );
        assert_eq!(text(forwarded).0.capacity(), capacity);
        let claim = saved(&operations, &original, "dispatched").await;
        assert_eq!(claim.get("requestId"), Some(&request_id));
        assert_eq!(
            claim.get("requestHash"),
            Some(&IvyJson::from(expected_hash.clone()))
        );
        drop(frame);
        input
            .try_send(IvyJson::object([
                ("method", "serverRequest/resolved".into()),
                (
                    "params",
                    IvyJson::object([
                        ("requestId", request_id.clone()),
                        ("threadId", "original-thread".into()),
                    ]),
                ),
            ]))
            .unwrap();
        let receipt = saved(&operations, &original, "succeeded").await;
        receipts.drain().await.unwrap();
        assert_eq!(
            receipt.get("requestHash"),
            Some(&IvyJson::from(expected_hash))
        );
        let resolved = operations
            .journal()
            .lock()
            .unwrap()
            .input(&input_identity)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.get("state"), Some(&"answered".into()));
        assert_eq!(resolved.get("identity"), Some(&input_identity));
        assert_eq!(
            hash_json(resolved.required("reply").unwrap()).unwrap(),
            hash_json(&replay).unwrap()
        );
        let duplicate = dispatch
            .answer(original.clone(), input_identity, replay)
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
    })
    .catch_unwind()
    .await;
    rpc.close("fixture_complete").await;
    receipts.drain().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

struct RacingStore {
    inner: SqliteOperationStore,
    writers: tokio::sync::Barrier,
    attempts: AtomicUsize,
}

async fn stage_terminal(
    operations: &NativeOperations,
    store: &dyn NativeOperationStore,
    suffix: &str,
) -> (Identity, String, IvyJson) {
    let original = identity(suffix);
    let key = hash_json(&original.value()).unwrap();
    let journal = operations.journal();
    let intent = {
        let mut memory = journal.lock().unwrap();
        memory.set_on_terminal(None);
        memory
            .accept(&original, &"account/logout".into(), &IvyJson::Null)
            .unwrap();
        memory
            .dispatch(&original, &"epoch".into(), &"original-request".into())
            .unwrap()
    };
    store.write(key.clone(), intent, None).await.unwrap();
    let terminal = journal
        .lock()
        .unwrap()
        .finish(
            &original,
            &"epoch".into(),
            &"original-request".into(),
            &IvyJson::object([("result", IvyJson::String(JsString(vec![65, 0xdfff])))]),
        )
        .unwrap();
    (original, key, terminal)
}
impl NativeOperationStore for RacingStore {
    fn service_node_id(&self) -> &JsString {
        self.inner.service_node_id()
    }
    fn owns_receipt(&self, value: &IvyJson) -> bool {
        self.inner.owns_receipt(value)
    }
    fn read(&self, key: String) -> StoreFuture<'_, Option<SavedState>> {
        self.inner.read(key)
    }
    fn write(
        &self,
        key: String,
        value: IvyJson,
        previous: Option<StatePin>,
    ) -> StoreFuture<'_, SavedState> {
        Box::pin(async move {
            if previous.is_some() && value.get("phase") == Some(&"succeeded".into()) {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                self.writers.wait().await;
            }
            self.inner.write(key, value, previous).await
        })
    }
}

#[tokio::test]
async fn concurrent_terminal_readers_confirm_the_original_receipt_after_a_lost_cas() {
    let selected = Arc::new(Mutex::new(None));
    let capture = selected.clone();
    let Fixture {
        _root,
        operations,
        receipts,
        rpc,
        mut peer,
        input: _input,
        dispatch: _dispatch,
    } = fixture_with_store(128 * 1024 * 1024, move |state| {
        let store = Arc::new(RacingStore {
            inner: SqliteOperationStore::new(state).unwrap(),
            writers: tokio::sync::Barrier::new(2),
            attempts: AtomicUsize::new(0),
        });
        *capture.lock().unwrap() = Some(store.clone());
        store
    });
    let store = selected.lock().unwrap().take().unwrap();
    let (original, key, terminal) =
        stage_terminal(&operations, store.as_ref(), "concurrent-receipt").await;
    let scenario = std::panic::AssertUnwindSafe(async {
        let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(operations.read(&original), operations.read(&original))
        }).await.unwrap();
        assert!(first.is_ok() && second.is_ok(), "A concurrent original receipt was reported as a failed reconciliation: first={:?}, second={:?}", first.as_ref().err(), second.as_ref().err());
        for value in [first.unwrap().unwrap(), second.unwrap().unwrap(), store.read(key).await.unwrap().unwrap().value] {
            assert_eq!(hash_json(&value).unwrap(), hash_json(&terminal).unwrap());
        }
        assert_eq!(store.attempts.load(Ordering::SeqCst), 2, "A failed CAS was retried instead of checking the original saved receipt");
        assert!(tokio::time::timeout(Duration::from_millis(20), peer.recv()).await.is_err());
    }).catch_unwind().await;
    rpc.close("fixture_complete").await;
    receipts.drain().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

struct CompetingStore {
    inner: SqliteOperationStore,
    field: &'static str,
    attempts: AtomicUsize,
}
impl NativeOperationStore for CompetingStore {
    fn service_node_id(&self) -> &JsString {
        self.inner.service_node_id()
    }
    fn owns_receipt(&self, value: &IvyJson) -> bool {
        self.inner.owns_receipt(value)
    }
    fn read(&self, key: String) -> StoreFuture<'_, Option<SavedState>> {
        self.inner.read(key)
    }
    fn write(
        &self,
        key: String,
        value: IvyJson,
        previous: Option<StatePin>,
    ) -> StoreFuture<'_, SavedState> {
        Box::pin(async move {
            if previous.is_some() && value.get("phase") == Some(&"succeeded".into()) {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                let mut competing = value.clone();
                competing.insert(
                    self.field,
                    if self.field == "reply" {
                        IvyJson::object([("result", "different-original-result".into())])
                    } else {
                        format!("sha256:{}", "1".repeat(64)).into()
                    },
                );
                self.inner
                    .write(key.clone(), competing, previous.clone())
                    .await?;
            }
            self.inner.write(key, value, previous).await
        })
    }
}

#[tokio::test]
async fn a_different_saved_execution_or_result_never_confirms_the_lost_cas() {
    for field in ["reply", "nativeExecutableHash"] {
        let selected = Arc::new(Mutex::new(None));
        let capture = selected.clone();
        let Fixture {
            _root,
            operations,
            receipts,
            rpc,
            mut peer,
            input: _input,
            dispatch: _dispatch,
        } = fixture_with_store(128 * 1024 * 1024, move |state| {
            let store = Arc::new(CompetingStore {
                inner: SqliteOperationStore::new(state).unwrap(),
                field,
                attempts: AtomicUsize::new(0),
            });
            *capture.lock().unwrap() = Some(store.clone());
            store
        });
        let store = selected.lock().unwrap().take().unwrap();
        let (original, key, terminal) =
            stage_terminal(&operations, store.as_ref(), "competing-receipt").await;
        let scenario = std::panic::AssertUnwindSafe(async {
            let error = operations.read(&original).await.unwrap_err();
            assert_eq!(error.code, "revision_conflict");
            let current = store.read(key).await.unwrap().unwrap().value;
            assert_ne!(current.get(field), terminal.get(field));
            assert_eq!(current.get("requestHash"), terminal.get("requestHash"));
            assert_eq!(
                store.attempts.load(Ordering::SeqCst),
                1,
                "A different saved record was overwritten after the original CAS failed"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(20), peer.recv())
                    .await
                    .is_err()
            );
        })
        .catch_unwind()
        .await;
        rpc.close("fixture_complete").await;
        receipts.drain().await.unwrap();
        if let Err(panic) = scenario {
            std::panic::resume_unwind(panic);
        }
    }
}
