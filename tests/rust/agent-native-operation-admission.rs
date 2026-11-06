use super::*;
use ivy_agent_manager::{
    local_state::{Pin as StatePin, SavedState},
    native_operations::{NativeOperationStore, NativeRun, StoreFuture},
};
use tokio::sync::Semaphore;

fn identity(suffix: &str) -> Identity {
    Identity {
        caller_principal_id: "caller".into(),
        operation_id: format!("epoch:{}:{suffix}", chrono::Utc::now().timestamp_millis()).into(),
    }
}
struct Reads {
    original: Option<SqliteOperationStore>,
    entered: Semaphore,
    release: Semaphore,
    count: AtomicUsize,
    gated: bool,
    panic_first: bool,
}
impl Reads {
    fn new(original: Option<SqliteOperationStore>, gated: bool, panic_first: bool) -> Self {
        Self {
            original,
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            count: AtomicUsize::new(0),
            gated,
            panic_first,
        }
    }
    async fn entered(&self, count: u32) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.acquire_many(count))
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
}
impl NativeOperationStore for Reads {
    fn service_node_id(&self) -> &JsString {
        self.original.as_ref().map_or_else(
            || {
                static NODE: std::sync::OnceLock<JsString> = std::sync::OnceLock::new();
                NODE.get_or_init(|| "fixture.agent".into())
            },
            NativeOperationStore::service_node_id,
        )
    }
    fn read(&self, key: String) -> StoreFuture<'_, Option<SavedState>> {
        Box::pin(async move {
            let index = self.count.fetch_add(1, Ordering::SeqCst);
            self.entered.add_permits(1);
            if self.gated && (self.original.is_none() || index == 0) {
                self.release.acquire().await.unwrap().forget();
            }
            assert!(!self.panic_first || index != 0, "fixture read panic");
            match &self.original {
                Some(original) => original.read(key).await,
                None => Err(IvyError::new("fixture_read_failed", "Fixture read failed")),
            }
        })
    }
    fn write(
        &self,
        key: String,
        value: IvyJson,
        pin: Option<StatePin>,
    ) -> StoreFuture<'_, SavedState> {
        self.original
            .as_ref()
            .expect("Failed reads must never write a claim")
            .write(key, value, pin)
    }
}
fn blocked() -> (Fixture, Arc<Reads>) {
    let reads = Arc::new(Reads::new(None, true, false));
    let store = reads.clone();
    let fixture = fixture_with_store(128 * 1024 * 1024, move |_| store);
    (fixture, reads)
}
fn start(
    operations: &NativeOperations,
    id: Identity,
    method: &str,
    params: IvyJson,
) -> IvyResult<NativeRun> {
    operations.run(
        id,
        method.into(),
        params,
        |_| async { panic!("A failed read must never start an action") },
        false,
    )
}
fn refused(run: IvyResult<NativeRun>) {
    refusal(run, Outcome::NotExecuted);
}
fn refused_observation(run: IvyResult<NativeRun>) {
    refusal(run, Outcome::Unknown);
}
fn refusal(run: IvyResult<NativeRun>, outcome: Outcome) {
    match run {
        Err(error) => {
            assert_eq!(error.code, "native_capacity");
            assert_eq!(error.outcome, outcome);
        }
        Ok(_) => panic!("Operation work must be refused synchronously before claim or spawn"),
    }
}
async fn failed(run: NativeRun) {
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), run.wait())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "fixture_read_failed"
    );
}
async fn close(fixture: Fixture) {
    fixture.rpc.close("fixture_done").await;
    fixture.receipts.drain().await.unwrap();
}
fn spare(mebibytes: usize) -> IvyJson {
    let mut units = Vec::with_capacity(mebibytes * 1024 * 1024 / 2);
    units.push(0xdfff);
    IvyJson::String(JsString(units))
}

#[tokio::test]
async fn original_work_is_bounded_before_storage_and_keeps_control_slots_after_waiters_drop() {
    let (fixture, reads) = blocked();
    let mut runs = Vec::new();
    for index in 0..56 {
        runs.push(
            start(
                &fixture.operations,
                identity(&format!("data-{index}")),
                "account/logout",
                IvyJson::Null,
            )
            .unwrap(),
        );
    }
    drop(runs.pop());
    refused(start(
        &fixture.operations.clone(),
        identity("overflow"),
        "account/logout",
        IvyJson::Null,
    ));
    for index in 0..8 {
        let method = ["agent.answer", "turn/interrupt", "thread/unsubscribe"][index % 3];
        runs.push(
            start(
                &fixture.operations,
                identity(&format!("control-{index}")),
                method,
                IvyJson::Null,
            )
            .unwrap(),
        );
    }
    refused(start(
        &fixture.operations,
        identity("control-overflow"),
        "agent.answer",
        IvyJson::Null,
    ));
    reads.entered(64).await;
    assert_eq!(reads.count.load(Ordering::SeqCst), 64);
    reads.release.add_permits(64);
    for run in runs {
        failed(run).await;
    }
    // Failed admission must not leak its partially acquired permits.
    let recovered = start(
        &fixture.operations,
        identity("recovered"),
        "account/logout",
        IvyJson::Null,
    )
    .unwrap();
    reads.entered(1).await;
    reads.release.add_permits(1);
    failed(recovered).await;
    close(fixture).await;
}

#[tokio::test]
async fn actual_input_and_identity_capacity_are_bounded_with_reserved_control_bytes() {
    let (fixture, reads) = blocked();
    refused(start(
        &fixture.operations,
        identity("single"),
        "account/logout",
        spare(49),
    ));
    let mut large_id = identity("large-id");
    large_id
        .caller_principal_id
        .0
        .reserve_exact(25 * 1024 * 1024);
    refused(start(
        &fixture.operations,
        large_id,
        "account/logout",
        IvyJson::Null,
    ));
    assert_eq!(reads.count.load(Ordering::SeqCst), 0);
    let first = start(
        &fixture.operations,
        identity("first"),
        "account/logout",
        spare(30),
    )
    .unwrap();
    let second = start(
        &fixture.operations,
        identity("second"),
        "account/logout",
        spare(20),
    )
    .unwrap();
    refused(start(
        &fixture.operations,
        identity("background-bytes"),
        "account/logout",
        spare(7),
    ));
    let control = start(
        &fixture.operations,
        identity("control-bytes"),
        "turn/interrupt",
        spare(7),
    )
    .unwrap();
    refused(start(
        &fixture.operations,
        identity("all-bytes"),
        "agent.answer",
        spare(8),
    ));
    reads.entered(3).await;
    reads.release.add_permits(3);
    for run in [first, second, control] {
        failed(run).await;
    }
    let recovered = start(
        &fixture.operations,
        identity("byte-recovery"),
        "account/logout",
        spare(30),
    )
    .unwrap();
    reads.entered(1).await;
    reads.release.add_permits(1);
    failed(recovered).await;
    close(fixture).await;
}

#[tokio::test]
async fn duplicate_observers_share_work_admission_and_preserve_conflict_before_capacity() {
    let (fixture, reads) = blocked();
    let original = identity("duplicate");
    let mut runs = vec![
        start(
            &fixture.operations,
            original.clone(),
            "account/logout",
            IvyJson::Null,
        )
        .unwrap(),
    ];
    for _ in 0..55 {
        runs.push(
            start(
                &fixture.operations,
                original.clone(),
                "account/logout",
                IvyJson::Null,
            )
            .unwrap(),
        );
    }
    drop(runs.pop());
    refused_observation(start(
        &fixture.operations,
        original.clone(),
        "account/logout",
        IvyJson::Null,
    ));
    match start(
        &fixture.operations,
        original.clone(),
        "account/logout",
        true.into(),
    ) {
        Err(error) => assert_eq!(error.code, "mutation_conflict"),
        Ok(_) => panic!("Conflicting duplicate must not be admitted"),
    }
    reads.entered(1).await;
    assert_eq!(reads.count.load(Ordering::SeqCst), 1);
    reads.release.add_permits(1);
    for run in runs {
        failed(run).await;
    }
    assert_eq!(reads.count.load(Ordering::SeqCst), 1);
    let recovered = start(
        &fixture.operations,
        original,
        "account/logout",
        IvyJson::Null,
    )
    .unwrap();
    reads.entered(1).await;
    reads.release.add_permits(1);
    failed(recovered).await;
    close(fixture).await;
}

#[tokio::test]
async fn panicked_original_work_notifies_its_waiter_and_releases_admission() {
    let reads = Arc::new(Reads::new(None, false, true));
    let store = reads.clone();
    let fixture = fixture_with_store(128 * 1024 * 1024, move |_| store);
    let result = start(
        &fixture.operations,
        identity("panic"),
        "account/logout",
        spare(40),
    )
    .unwrap();
    assert_eq!(result.wait().await.unwrap_err().code, "internal_error");
    // The panic freed its input reservation, not just the active identity.
    failed(
        start(
            &fixture.operations,
            identity("after-panic"),
            "account/logout",
            spare(40),
        )
        .unwrap(),
    )
    .await;
    assert_eq!(reads.count.load(Ordering::SeqCst), 2);
    close(fixture).await;
}

#[tokio::test]
async fn input_reservation_outlives_the_durable_claim_and_the_consumed_action_parameters() {
    let fixture = fixture(128 * 1024 * 1024);
    let original = identity("action-owner");
    let action_id = original.clone();
    let operations = fixture.operations.clone();
    let (entered, running) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let run = fixture
        .operations
        .run(
            original.clone(),
            "account/logout".into(),
            spare(40),
            move |params| async move {
                drop(params);
                entered.send(()).unwrap();
                released.await.unwrap();
                operations
                    .journal()
                    .lock()
                    .unwrap()
                    .reject_before_dispatch(&action_id, &"fixture_done".into())
            },
            false,
        )
        .unwrap();
    running.await.unwrap();
    let scenario = std::panic::AssertUnwindSafe(async {
        let claim = fixture.operations.read(&original).await.unwrap().unwrap();
        assert_eq!(claim.get("phase"), Some(&"accepted".into()));
        refused(start(
            &fixture.operations,
            identity("during-action"),
            "account/logout",
            spare(20),
        ));
        // Prevention persists a fence without native execution, and remains
        // available in the reserved control bytes even for a background method.
        let prevented = fixture
            .operations
            .run(
                identity("prevention"),
                "account/logout".into(),
                spare(14),
                |_| async { panic!("Prevention must never execute an action") },
                true,
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(prevented.get("phase"), Some(&"failed".into()));
    })
    .catch_unwind()
    .await;
    release.send(()).unwrap();
    let saved = run.wait().await.unwrap();
    assert_eq!(saved.get("code"), Some(&"fixture_done".into()));
    assert_eq!(saved.get("phase"), Some(&"failed".into()));
    if scenario.is_ok() {
        // Equal original parameters with different spare capacity recover the
        // same durable receipt once the actual task releases its reservation.
        let recovered = start(&fixture.operations, original, "account/logout", spare(30))
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(hash_json(&saved).unwrap(), hash_json(&recovered).unwrap());
    }
    close(fixture).await;
    if let Err(error) = scenario {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn overload_preserves_the_original_sqlite_claim_and_cleanup_without_native_replay() {
    let selected = Arc::new(Mutex::new(None));
    let target = selected.clone();
    let mut fixture = fixture_with_store(128 * 1024 * 1024, move |state| {
        let store = Arc::new(Reads::new(
            Some(SqliteOperationStore::new(state).unwrap()),
            true,
            false,
        ));
        *target.lock().unwrap() = Some(store.clone());
        store
    });
    let reads = selected.lock().unwrap().take().unwrap();
    let original = identity("sqlite-original");
    let rejected = identity("sqlite-refused");
    let cleanup = identity("sqlite-cleanup");
    let scenario = std::panic::AssertUnwindSafe(async {
        drop(
            fixture
                .dispatch
                .invoke(original.clone(), "account/logout".into(), IvyJson::Null)
                .unwrap(),
        );
        let mut duplicates = Vec::new();
        for _ in 0..55 {
            duplicates.push(
                fixture
                    .dispatch
                    .invoke(original.clone(), "account/logout".into(), IvyJson::Null)
                    .unwrap(),
            );
        }
        refused_observation(fixture.dispatch.invoke(
            original.clone(),
            "account/logout".into(),
            IvyJson::Null,
        ));
        refused(
            fixture
                .dispatch
                .invoke(rejected.clone(), "account/logout".into(), IvyJson::Null),
        );
        reads.entered(1).await;
        assert_eq!(reads.count.load(Ordering::SeqCst), 1);
        assert!(
            fixture
                .operations
                .journal()
                .lock()
                .unwrap()
                .get(&original)
                .unwrap()
                .is_none()
        );
        let control = fixture
            .dispatch
            .invoke(
                cleanup.clone(),
                "thread/unsubscribe".into(),
                IvyJson::object([("threadId", "owned-thread".into())]),
            )
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), fixture.peer.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.get("method"), Some(&"thread/unsubscribe".into()));
        fixture
            .input
            .try_send(IvyJson::object([
                ("id", frame.required("id").unwrap().clone()),
                (
                    "result",
                    IvyJson::object([("status", "unsubscribed".into())]),
                ),
            ]))
            .unwrap();
        assert_eq!(
            control.wait().await.unwrap().get("phase"),
            Some(&"succeeded".into())
        );
        reads.release.add_permits(1);
        let frame = tokio::time::timeout(Duration::from_secs(5), fixture.peer.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.get("method"), Some(&"account/logout".into()));
        let request_id = frame.required("id").unwrap().clone();
        let claim = fixture.operations.read(&original).await.unwrap().unwrap();
        assert_eq!(claim.get("requestId"), Some(&request_id));
        assert_eq!(claim.get("phase"), Some(&"dispatched".into()));
        for run in duplicates {
            assert_eq!(
                run.wait().await.unwrap().get("operationId"),
                Some(&IvyJson::String(original.operation_id.clone()))
            );
        }
        assert!(fixture.operations.read(&rejected).await.unwrap().is_none());
        fixture
            .input
            .try_send(IvyJson::object([
                ("id", request_id.clone()),
                ("result", IvyJson::object([] as [(&str, IvyJson); 0])),
            ]))
            .unwrap();
        fixture.operations.settle(&original).await.unwrap();
        let saved = fixture.operations.read(&original).await.unwrap().unwrap();
        assert_eq!(saved.get("phase"), Some(&"succeeded".into()));
        assert_eq!(saved.get("requestId"), Some(&request_id));
        let duplicate = fixture
            .dispatch
            .invoke(original.clone(), "account/logout".into(), IvyJson::Null)
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(hash_json(&duplicate).unwrap(), hash_json(&saved).unwrap());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), fixture.peer.recv())
                .await
                .is_err(),
            "Reading the original receipt must never replay native work"
        );
    })
    .catch_unwind()
    .await;
    reads.release.add_permits(128);
    close(fixture).await;
    if let Err(error) = scenario {
        std::panic::resume_unwind(error);
    }
}
