use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use ivy_hive::storage::{
    BackupTask, Completion, Dispatch, Event, MAX_PENDING, MAX_QUEUED_BYTES, Notification,
    Payload, Request, RequestEffects, RequestOptions, StorageClient, StorageService,
};
#[path = "../fixtures/hive-storage-payload.rs"]
mod payload_fixture;
use rusqlite::Connection;
use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

#[path = "hive_admission.rs"]
mod admission;
#[path = "hive_dispatch_gate.rs"]
mod dispatch_gate;

#[derive(Default)]
struct Control {
    opened: AtomicUsize,
    calls: AtomicUsize,
    closed: AtomicBool,
    entered: Notify,
    gate: (Mutex<bool>, Condvar),
    backup_entered: Notify,
    backup_release: Notify,
    events: Mutex<Vec<Event>>,
}
impl Control {
    fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}
struct ReleaseOnDrop(Arc<Control>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) { self.0.release(); }
}
struct Service {
    control: Arc<Control>,
    db: Rc<Connection>,
    local: Rc<Cell<usize>>,
    thread: std::thread::ThreadId,
}
impl StorageService for Service {
    fn handle(&mut self, payload: Payload) -> IvyResult<IvyJson> {
        let payload = payload_fixture::json(payload);
        assert_eq!(self.thread, std::thread::current().id());
        self.control.calls.fetch_add(1, Ordering::SeqCst);
        self.local.set(self.local.get() + 1);
        match payload.required("action")?.as_str()?.as_str() {
            "block" => {
                self.control.entered.notify_one();
                let mut open = self.control.gate.0.lock().unwrap();
                while !*open {
                    open = self.control.gate.1.wait(open).unwrap();
                }
            }
            "write" => {
                self.db
                    .execute("INSERT INTO effects VALUES(1)", [])
                    .unwrap();
            }
            "panic" => panic!("DB owner fixture failure"),
            _ => {}
        }
        Ok(IvyJson::Number(
            self.db
                .query_row("SELECT COUNT(*) FROM effects", [], |r| r.get::<_, i64>(0))
                .unwrap() as f64,
        ))
    }
    fn backup(&mut self, _: JsString) -> IvyResult<BackupTask> {
        self.control.backup_entered.notify_one();
        let control = self.control.clone();
        let db = self.db.clone();
        let thread = self.thread;
        Ok(Box::pin(async move {
            control.backup_release.notified().await;
            assert_eq!(thread, std::thread::current().id());
            Ok(IvyJson::Number(
                db.query_row("SELECT COUNT(*) FROM effects", [], |r| r.get::<_, i64>(0))
                    .unwrap() as f64,
            ))
        }))
    }
    fn notifications(&mut self) -> Vec<Notification> {
        vec![]
    }
    fn close(&mut self) -> IvyResult<()> {
        assert_eq!(self.thread, std::thread::current().id());
        self.control.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn dispatch(&mut self, payload: Payload) -> Dispatch {
        if matches!(&payload, Payload::Json(value) if value.get("action") == Some(&"fatal".into())) {
            return Dispatch {
                notifications: vec![Notification::Changes(vec!["published".into()])],
                completion: Completion::Failed(IvyError::internal()),
            };
        }
        Dispatch {
            notifications: vec![],
            completion: Completion::Reply(self.handle(payload)),
        }
    }
}
fn client() -> (StorageClient, Arc<Control>) {
    let control = Arc::new(Control::default());
    let owned = control.clone();
    let events = control.clone();
    let network_thread = std::thread::current().id();
    let client = StorageClient::new(
        IvyJson::Null,
        Box::new(move |_| {
            assert_ne!(network_thread, std::thread::current().id());
            owned.opened.fetch_add(1, Ordering::SeqCst);
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch("CREATE TABLE effects(value INTEGER)")
                .unwrap();
            Ok(Box::new(Service {
                control: owned,
                db: Rc::new(db),
                local: Rc::new(Cell::new(0)),
                thread: std::thread::current().id(),
            }) as Box<dyn StorageService>)
        }),
        Arc::new(move |event| events.events.lock().unwrap().push(event)),
    );
    (client, control)
}
fn action(name: &str) -> IvyJson {
    IvyJson::object([("action", name.into())])
}

#[tokio::test]
async fn native_credential_queue_charges_capacity_and_shared_effects_once() {
    struct CredentialEffects(Arc<Vec<ivy_hive::store::Credential>>);
    impl RequestEffects for CredentialEffects {
        fn complete(self: Box<Self>, result: IvyResult<IvyJson>) -> IvyResult<IvyJson> {
            assert_eq!(self.0.len(), 1);
            result
        }
    }
    let (client, control) = client();
    let _release = ReleaseOnDrop(control.clone());
    client.start().wait().await.unwrap();
    let blocked = client.request(action("block"), RequestOptions::default());
    control.entered.notified().await;
    let allocated = MAX_QUEUED_BYTES * 3 / 4;
    let credentials = || Arc::new(vec![ivy_hive::store::Credential {
        principal_id: JsString(Vec::with_capacity(allocated / 2)), digest: "digest".into(),
    }]);
    let first = credentials();
    let posted = client.dispatch(Request::with_effects(Payload::ConfigureCredentials(first.clone()),
        CredentialEffects(first)), RequestOptions::default());
    assert_eq!(client.pending_count(), 2);
    assert!(client.queued_bytes() >= allocated);
    assert!(client.queued_bytes() < MAX_QUEUED_BYTES, "Shared credential backing was counted twice");
    let rejected = client.request(Payload::ConfigureCredentials(credentials()), RequestOptions::default()).await.unwrap_err();
    assert_eq!(rejected.code, "limit_exceeded", "Tiny serialized strings must not bypass retained capacity");
    control.release();
    blocked.await.unwrap();
    posted.await.unwrap();
    client.close().wait().await.unwrap();
    assert_eq!(client.queued_bytes(), 0);
}

struct OrderedEffects {
    state: Arc<AtomicUsize>,
    expected: usize,
    next: usize,
    network: tokio::runtime::Id,
}
impl RequestEffects for OrderedEffects {
    fn prepare(&self, _: &mut Payload) -> IvyResult<()> {
        assert_eq!(tokio::runtime::Handle::current().id(), self.network);
        assert_eq!(self.state.load(Ordering::SeqCst), self.expected);
        Ok(())
    }
    fn complete(self: Box<Self>, result: IvyResult<IvyJson>) -> IvyResult<IvyJson> {
        assert_eq!(tokio::runtime::Handle::current().id(), self.network);
        let result = result?;
        assert_eq!(self.state.swap(self.next, Ordering::SeqCst), self.expected);
        Ok(result)
    }
}

#[tokio::test]
async fn db_queue_applies_effects_before_the_next_action_without_observer_polling() {
    let (client, control) = client();
    let _release = ReleaseOnDrop(control.clone());
    client.start().wait().await.unwrap();
    let blocked = client.request(action("block"), RequestOptions::default());
    control.entered.notified().await;
    let state = Arc::new(AtomicUsize::new(0));
    let network = tokio::runtime::Handle::current().id();
    let first = client.dispatch(Request::with_effects(action("write"), OrderedEffects {
        state: state.clone(), expected: 0, next: 1, network,
    }), RequestOptions::default());
    let second = client.dispatch(Request::with_effects(action("write"), OrderedEffects {
        state: state.clone(), expected: 1, next: 2, network,
    }), RequestOptions::default());
    assert_eq!(client.pending_count(), 3, "Both actions must already be in the only queue");
    drop(first);
    control.release();
    blocked.await.unwrap();
    assert_eq!(second.await.unwrap(), 2.0.into());
    assert_eq!(state.load(Ordering::SeqCst), 2);
    client.close().wait().await.unwrap();
    assert_eq!(control.opened.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn committed_effects_survive_a_running_timeout_on_the_original_owner() {
    let (client, control) = client();
    let _release = ReleaseOnDrop(control.clone());
    client.start().wait().await.unwrap();
    let state = Arc::new(AtomicUsize::new(0));
    let original = client.dispatch(Request::with_effects(action("block"), OrderedEffects {
        state: state.clone(), expected: 0, next: 1,
        network: tokio::runtime::Handle::current().id(),
    }), RequestOptions { timeout_ms: 100, ..Default::default() });
    control.entered.notified().await;
    assert_eq!(original.await.unwrap_err().outcome, Outcome::Unknown);
    control.release();
    client.close().wait().await.unwrap();
    assert_eq!(state.load(Ordering::SeqCst), 1);
    assert_eq!(control.opened.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn expired_queued_actions_never_prepare_or_apply_effects() {
    let (client, control) = client();
    let _release = ReleaseOnDrop(control.clone());
    client.start().wait().await.unwrap();
    let blocked = client.request(action("block"), RequestOptions::default());
    control.entered.notified().await;
    let state = Arc::new(AtomicUsize::new(0));
    let expired = client.dispatch(Request::with_effects(action("write"), OrderedEffects {
        state: state.clone(), expected: usize::MAX, next: 1,
        network: tokio::runtime::Handle::current().id(),
    }), RequestOptions { timeout_ms: 10, ..Default::default() });
    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(expired.await.unwrap_err().outcome, Outcome::NotExecuted);
    assert!(client.is_ready());
    tokio::time::resume();
    control.release();
    blocked.await.unwrap();
    assert_eq!(client.request(action("read"), RequestOptions::default()).await.unwrap(), 0.0.into());
    client.close().wait().await.unwrap();
    assert_eq!(state.load(Ordering::SeqCst), 0);
    assert_eq!(control.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn db_owner_is_created_once_off_the_network_and_drains_abandoned_work() {
    let (client, control) = client();
    assert_eq!(control.opened.load(Ordering::SeqCst), 0);
    let first = client.start();
    let second = client.start();
    first.wait().await.unwrap();
    second.wait().await.unwrap();
    drop(client.request(action("write"), RequestOptions::default()));
    assert_eq!(
        client
            .request(action("read"), RequestOptions::default())
            .await
            .unwrap(),
        IvyJson::Number(1.0)
    );
    client.close().wait().await.unwrap();
    assert_eq!(control.opened.load(Ordering::SeqCst), 1);
    assert!(control.closed.load(Ordering::SeqCst));
    assert_eq!(client.pending_count(), 0);
    assert_eq!(client.queued_bytes(), 0);
    assert!(
        client
            .request(action("write"), RequestOptions::default())
            .await
            .is_err()
    );
    assert!(client.start().wait().await.is_err());
}

#[tokio::test]
async fn queue_charges_retained_data_and_keeps_abandoned_reservations_until_consumed() {
    let (client, control) = client();
    client.start().wait().await.unwrap();
    let too_large = IvyJson::String(JsString(vec![b'x' as u16; MAX_QUEUED_BYTES / 2]));
    assert_eq!(
        client
            .request(too_large, RequestOptions::default())
            .await
            .unwrap_err()
            .code,
        "limit_exceeded"
    );
    let blocking = client.request(action("block"), RequestOptions::default());
    control.entered.notified().await;
    let original_bytes = client.queued_bytes();
    for _ in 1..MAX_PENDING {
        drop(client.request(action("write"), RequestOptions::default()));
    }
    assert_eq!(client.pending_count(), MAX_PENDING);
    assert!(client.queued_bytes() > original_bytes);
    assert_eq!(
        client
            .request(action("write"), RequestOptions::default())
            .await
            .unwrap_err()
            .outcome,
        Outcome::NotExecuted
    );
    // The event loop remains responsive while the DB thread is occupied.
    tokio::task::yield_now().await;
    control.release();
    blocking.await.unwrap();
    client.close().wait().await.unwrap();
    assert_eq!(control.calls.load(Ordering::SeqCst), MAX_PENDING);
    assert_eq!(client.pending_count(), 0);
    assert_eq!(client.queued_bytes(), 0);
}

#[tokio::test]
async fn queue_charges_effect_allocations_until_abandoned_work_completes() {
    struct Held(Vec<u8>);
    impl RequestEffects for Held {
        fn retained_bytes(&self) -> usize {
            std::mem::size_of::<Self>() + self.0.capacity()
        }
    }
    let (client, control) = client();
    let _release = ReleaseOnDrop(control.clone());
    client.start().wait().await.unwrap();
    let blocked = client.request(action("block"), RequestOptions::default());
    control.entered.notified().await;
    let before = client.queued_bytes();
    let held = client.dispatch(Request::with_effects(action("write"), Held(vec![0; 1024 * 1024])),
        RequestOptions { reserved_bytes: 0, ..Default::default() });
    let retained = client.queued_bytes();
    assert!(retained >= before + 1024 * 1024);
    drop(held);
    assert_eq!(client.queued_bytes(), retained);
    control.release();
    blocked.await.unwrap();
    client.close().wait().await.unwrap();
    assert_eq!(client.queued_bytes(), 0);
    assert_eq!(control.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_queued_deadline_prevents_execution_without_starting_another_owner() {
    let (client, control) = client();
    client.start().wait().await.unwrap();
    let blocking = client.request(action("block"), RequestOptions::default());
    control.entered.notified().await;
    let expired = client.request(
        action("write"),
        RequestOptions {
            timeout_ms: 0,
            terminate_on_timeout: false,
            ..Default::default()
        },
    );
    let error = expired.await.unwrap_err();
    assert_eq!(error.code, "deadline_exceeded");
    assert_eq!(error.outcome, Outcome::NotExecuted);
    assert!(client.is_ready());
    assert_eq!(client.pending_count(), 2);
    control.release();
    blocking.await.unwrap();
    assert_eq!(
        client
            .request(action("read"), RequestOptions::default())
            .await
            .unwrap(),
        IvyJson::Number(0.0)
    );
    client.close().wait().await.unwrap();
    assert_eq!(control.opened.load(Ordering::SeqCst), 1);
    assert_eq!(control.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn invocation_is_posted_eagerly_and_queued_expiry_keeps_the_owner_ready() {
    use ivy_hive::{admission::RequestBudget, server_rpc::{Invocation, Options}};
    let (client, control) = client();
    let _release = ReleaseOnDrop(control.clone());
    client.start().wait().await.unwrap();
    let blocked = client.request(action("block"), RequestOptions { timeout_ms: 60_000, ..Default::default() });
    control.entered.notified().await;
    let invocation = Invocation::new(
        Arc::new(client.clone()), Arc::new(|_, _| Ok(None)), Arc::new(|| 0.0),
        RequestBudget::default(), Options::default(),
    );
    let admitted_at = tokio::time::Instant::now();
    let waiting = invocation.invoke(
        IvyJson::object([("credentialDigest", "fixture".into())]),
        IvyJson::object([("id", "waiting".into()), ("method", "fixture.write".into())]), 0,
    );
    assert_eq!(client.pending_count(), 2, "Invocation must reach the DB queue before its observer is polled");
    tokio::time::advance(Duration::from_secs(30)).await;
    let error = waiting.await.unwrap_err();
    let elapsed = tokio::time::Instant::now() - admitted_at;
    assert!(elapsed >= Duration::from_secs(30) && elapsed < Duration::from_millis(30_020));
    assert_eq!(error.code, "deadline_exceeded");
    assert_eq!(error.outcome, Outcome::NotExecuted);
    assert!(client.is_ready());
    assert!(client.failure().is_none());
    // Cleanup waits on a real DB thread, so do not auto-advance its observer timeout.
    tokio::time::resume();
    control.release();
    blocked.await.unwrap();
    // Resuming keeps the 30-second virtual offset from the real DB clock.
    assert_eq!(client.request(action("read"), RequestOptions {
        timeout_ms: 60_000, ..Default::default()
    }).await.unwrap(), 0.0.into());
    client.close().wait().await.unwrap();
    assert_eq!(control.calls.load(Ordering::SeqCst), 2, "Expired invocation executed");
    assert_eq!(control.opened.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_running_timeout_remains_unknown_and_keeps_the_original_owner_until_close() {
    let (client, control) = client();
    let _release = ReleaseOnDrop(control.clone());
    client.start().wait().await.unwrap();
    let blocked = client.request(action("block"), RequestOptions { timeout_ms: 100, ..Default::default() });
    control.entered.notified().await;
    let error = blocked.await.unwrap_err();
    let ready = client.is_ready();
    let failure = client.failure();
    control.release();
    client.close().wait().await.unwrap();
    assert_eq!(error.code, "deadline_exceeded");
    assert_eq!(error.outcome, Outcome::Unknown);
    assert!(!ready);
    assert_eq!(failure.unwrap().outcome, Outcome::Unknown);
    assert_eq!(control.opened.load(Ordering::SeqCst), 1);
    assert_eq!(control.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn backup_yields_to_actions_and_close_waits_for_the_same_connection() {
    let (client, control) = client();
    client.start().wait().await.unwrap();
    let backup = client.request(
        IvyJson::object([
            ("action", "backup".into()),
            ("destination", "fixture".into()),
        ]),
        RequestOptions::default(),
    );
    control.backup_entered.notified().await;
    client
        .request(action("write"), RequestOptions::default())
        .await
        .unwrap();
    let second = client
        .request(
            IvyJson::object([
                ("action", "backup".into()),
                ("destination", "fixture".into()),
            ]),
            RequestOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(second.code, "limit_exceeded");
    let closing = client.close();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), closing.wait())
            .await
            .is_err()
    );
    control.backup_release.notify_one();
    assert_eq!(backup.await.unwrap(), IvyJson::Number(1.0));
    closing.wait().await.unwrap();
    assert!(control.closed.load(Ordering::SeqCst));
    assert_eq!(client.pending_count(), 0);
}

#[tokio::test]
async fn fatal_publication_and_panics_keep_unknown_outcomes_and_never_reopen_sqlite() {
    for command in ["fatal", "panic"] {
        let (client, control) = client();
        client.start().wait().await.unwrap();
        let error = client
            .request(action(command), RequestOptions::default())
            .await
            .unwrap_err();
        assert_eq!(error.outcome, Outcome::Unknown);
        assert!(client.close().wait().await.is_err());
        assert!(client.start().wait().await.is_err());
        assert_eq!(control.opened.load(Ordering::SeqCst), 1);
        assert_eq!(client.pending_count(), 0);
        let events = control.events.lock().unwrap();
        if command == "fatal" {
            assert!(matches!(events.first(), Some(Event::Notification(_))));
        }
        assert!(matches!(events.last(), Some(Event::Failure(_))));
    }
}
