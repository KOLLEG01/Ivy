#[path = "reference-data.rs"]
mod reference_data;
use super::*;
use ivy_sdk::service::InvocationContext;
use ivy_task_board::{
    engine_read::{EngineReads, ReadRuntime},
    engine_service::{EngineService, ServiceCategories, ServiceRuntime},
    native_lifecycle::{LifecycleDocument, LifecycleStore},
    service_bridge::{LocalHandler, service_bridge},
};
use std::{
    cell::{Cell, RefCell},
    future::{Future, poll_fn},
    task::Poll,
};

impl LifecycleStore for World {
    async fn lifecycle_read(
        &self,
        k: &str,
        a: &Json,
        p: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        let record = self.read_record(k, a, p, archived)?;
        Ok(LifecycleDocument {
            document: document(&record),
            metadata: record.required("metadata")?.clone(),
        })
    }
    async fn page(&self, k: &str, q: &Json) -> IvyResult<Json> {
        self.replay.call("page", [k.into(), q.clone()])
    }
    async fn archive(&self, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
    async fn stat(&self, _: &JsString) -> IvyResult<Json> {
        unreachable!()
    }
    fn release_workspace(&self, _: &JsString) -> IvyResult<()> {
        unreachable!()
    }
}
struct Runtime<'a> {
    replay: &'a Replay,
    recovered: bool,
}
impl ServiceRuntime for Runtime<'_> {
    fn recovered(&self) -> bool {
        self.recovered
    }
    fn observed_at(&self) -> JsString {
        "2026-01-02T03:04:05.006Z".into()
    }
    fn task_changed(&self) {
        self.replay.call("taskChanged", []).unwrap();
    }
    fn kick(&self) {
        self.replay.call("kick", []).unwrap();
    }
}
impl ServiceCategories for Runtime<'_> {
    async fn categories(&self) -> IvyResult<Json> {
        self.replay.call("categories", [])
    }
}
fn context(row: &Json, step: &Json) -> InvocationContext {
    let opts = f(row, "options");
    let signal = RequestOptions::default().cancellation;
    if f(opts, "abort").as_bool() == Some(true) {
        signal.cancel();
    }
    InvocationContext::detached(
        f(opts, "caller")
            .as_js_string()
            .cloned()
            .unwrap_or_else(|| "caller".into()),
        Some(
            f(opts, "outerOperationId")
                .as_js_string()
                .or_else(|| f(f(step, "request"), "operationId").as_js_string())
                .cloned()
                .unwrap_or_else(|| "context".into()),
        ),
        f(opts, "generation").as_f64().unwrap_or(7.0) as u64,
        signal,
        ivy_sdk::IvyError::new("service_unavailable", "Service connection is stopping.")
            .with_outcome(ivy_sdk::Outcome::Unknown),
    )
}
#[tokio::test(flavor = "current_thread")]
async fn public_service_dispatch_matches_original_through_send_bridge() {
    let p = reference_data::read(include_bytes!("task-board-engine-service-cases.json.gz"));
    let data = parse(
        std::str::from_utf8(&p).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut boundaries = 0;
    for row in data.as_array().unwrap() {
        let replay = Arc::new(Replay {
            steps: Mutex::new(
                f(row, "boundaries")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            name: s(row, "name").clone(),
        });
        boundaries += replay.steps.lock().unwrap().len();
        let bindings = Bindings(replay.clone());
        let owner = EngineOwner {
            service_node_id: "task-node".into(),
            generation: 7.0,
        };
        let admission_runtime = AdmissionRuntime {
            client: Arc::new(Client(replay.clone())),
            contracts: &bindings,
            catalogs: &catalogs,
            handoff: &|_, _| Box::pin(async { panic!("No handoff in this fixture") }),
        };
        let operations = Operations {
            store: World {
                replay: replay.clone(),
                root: f(row, "root").as_js_string().cloned(),
            },
            now: || "2026-01-02T03:04:05.006Z".to_owned(),
        };
        let admission = EngineAdmission::new(
            &operations,
            f(row, "settings"),
            &owner,
            &admission_runtime,
            &bindings,
        )
        .unwrap();
        let principal = JsString::from("board");
        let read_runtime = ReadRuntime {
            client: admission_runtime.client.as_ref(),
            contracts: &bindings,
            principal_id: &principal,
            service_node_id: &owner.service_node_id,
            generation: 7.0,
        };
        let runtime = Runtime {
            replay: &replay,
            recovered: f(f(row, "options"), "recovered").as_bool().unwrap_or(true),
        };
        let engine = EngineService {
            admission: &admission,
            reads: EngineReads {
                operations: &operations,
                runtime: &read_runtime,
            },
            runtime: &runtime,
            categories: &runtime,
        };
        let mut outcomes = vec![];
        for step in f(row, "steps").as_array().unwrap() {
            let (bridge, local) = service_bridge(7);
            let handler = bridge.handler(&s(step, "tool").to_utf8().unwrap());
            let reply = handler(f(step, "request").clone(), context(row, step));
            let retired = bridge.retire();
            local.run(&engine, async { Ok(()) }).await.unwrap();
            outcomes.push(outcome(reply.await));
            retired.await.unwrap();
        }
        let actual = Json::object([("outcomes", Json::Array(outcomes))]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: actual {actual:?}, expected {:?}",
            replay.name,
            f(row, "expected")
        );
        assert!(
            replay.steps.lock().unwrap().is_empty(),
            "{:?}: unconsumed boundaries",
            replay.name
        );
    }
    assert_eq!(data.as_array().unwrap().len(), 36);
    assert!(boundaries > 100);
}

struct Gate {
    open: Cell<bool>,
    waker: RefCell<Option<std::task::Waker>>,
}
impl Gate {
    fn new() -> Self {
        Self {
            open: Cell::new(false),
            waker: RefCell::default(),
        }
    }
    fn release(&self) {
        self.open.set(true);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
    async fn wait(&self) {
        poll_fn(|cx| {
            if self.open.get() {
                Poll::Ready(())
            } else {
                *self.waker.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}
struct LocalJournalOwner<'a> {
    connection: &'a ivy_store::rusqlite::Connection,
    gate: &'a Gate,
    entered: Cell<usize>,
    at: &'a str,
}
impl LocalHandler for LocalJournalOwner<'_> {
    async fn handle(&self, tool: &str, args: Json, context: InvocationContext) -> IvyResult<Json> {
        self.entered.set(self.entered.get() + 1);
        if tool == "panic" {
            panic!("Injected local handler unwind");
        }
        assert_eq!(context.caller_principal_id, JsString(vec![0x63, 0xd800]));
        assert_eq!(context.operation_id, Some(JsString(vec![0x6f, 0xdfff])));
        assert_eq!(context.generation, 7);
        if tool == "first" {
            self.gate.wait().await;
        }
        // The work really uses a borrowed !Sync connection on this local owner.
        let journal = ivy_task_board::local_journal::LocalJournal {
            connection: self.connection,
            root_object_id: None,
        };
        journal.create(
            "task-board/history",
            &JsString::from(tool),
            None,
            &args,
            &JsString::from(self.at),
            &JsString::from(tool),
        )?;
        Ok(args)
    }
}
fn local_context() -> InvocationContext {
    InvocationContext::detached(
        JsString(vec![0x63, 0xd800]),
        Some(JsString(vec![0x6f, 0xdfff])),
        7,
        RequestOptions::default().cancellation,
        ivy_sdk::IvyError::new("service_unavailable", "Service connection is stopping.")
            .with_outcome(ivy_sdk::Outcome::Unknown),
    )
}
#[tokio::test(flavor = "current_thread")]
async fn abandoned_send_observer_retains_sqlite_work_and_eager_retirement_drains() {
    let connection = ivy_store::rusqlite::Connection::open_in_memory().unwrap();
    ivy_task_board::local_journal::initialize(&connection).unwrap();
    let gate = Gate::new();
    let handler = LocalJournalOwner {
        connection: &connection,
        gate: &gate,
        entered: Cell::new(0),
        at: "2026-01-02T03:04:05.006Z",
    };
    let (bridge, owner) = service_bridge(7);
    fn send_sync<T: Send + Sync>(_: &T) {}
    send_sync(&bridge);
    send_sync(&bridge.handler("first"));
    // A real foreign thread calls the SDK ToolHandler; the local owner retains
    // SQLite. The Send observation is then destroyed before it is ever polled.
    let remote = bridge.handler("first");
    let abandoned = std::thread::spawn(move || {
        remote(Json::object([("which", "first".into())]), local_context())
    })
    .join()
    .unwrap();
    drop(abandoned);
    let second = bridge.invoke(
        "second",
        Json::object([("which", "second".into())]),
        local_context(),
    );
    let panicked = bridge.invoke("panic", Json::Null, local_context());
    let mut old = local_context();
    old.generation = 8;
    assert_eq!(
        bridge
            .invoke("wrong", Json::Null, old)
            .await
            .unwrap_err()
            .code,
        "service_not_ready"
    );
    let drain_gate = Gate::new();
    let drained = Cell::new(false);
    let mut work = Box::pin(owner.run(&handler, async {
        drain_gate.wait().await;
        drained.set(true);
        Ok(())
    }));
    poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        assert_eq!(handler.entered.get(), 3);
        Poll::Ready(())
    })
    .await;
    let lost_retirement = bridge.retire();
    drop(lost_retirement);
    let mut retired = bridge.retire();
    assert_eq!(
        bridge
            .invoke("late", Json::Null, local_context())
            .await
            .unwrap_err()
            .code,
        "service_not_ready"
    );
    poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        assert!(retired.as_mut().poll(cx).is_pending());
        assert!(!drained.get());
        Poll::Ready(())
    })
    .await;
    gate.release();
    poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        assert!(retired.as_mut().poll(cx).is_pending());
        assert!(!drained.get());
        Poll::Ready(())
    })
    .await;
    assert_eq!(
        second.await.unwrap().get("which"),
        Some(&Json::from("second"))
    );
    let error = panicked.await.unwrap_err();
    assert_eq!(error.code, "internal_error");
    assert_eq!(error.outcome, Outcome::Unknown);
    let count: i64 = connection
        .query_row("SELECT count(*) FROM documents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 2,
        "The abandoned call committed its actual journal write."
    );
    drain_gate.release();
    work.await.unwrap();
    retired.await.unwrap();
    assert!(drained.get());
    drop(handler);
    connection.close().unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn interrupted_owner_and_failed_drain_never_report_success() {
    let (bridge, owner) = service_bridge(7);
    let reply = bridge.invoke("never", Json::Null, local_context());
    let retired = bridge.retire();
    drop(owner);
    assert_eq!(reply.await.unwrap_err().outcome, Outcome::Unknown);
    assert_eq!(retired.await.unwrap_err().outcome, Outcome::Unknown);
    assert_eq!(
        bridge
            .invoke("late", Json::Null, local_context())
            .await
            .unwrap_err()
            .code,
        "service_not_ready"
    );
    struct Unused;
    impl LocalHandler for Unused {
        async fn handle(&self, _: &str, _: Json, _: InvocationContext) -> IvyResult<Json> {
            unreachable!()
        }
    }
    let (bridge, owner) = service_bridge(7);
    let error = IvyError::new("service_unavailable", "Drain failed")
        .with_outcome(Outcome::Unknown)
        .with_details(Json::object([("retained", true.into())]));
    let retired = bridge.retire();
    let observed = owner
        .run(&Unused, async { Err(error.clone()) })
        .await
        .unwrap_err();
    let result = retired.await.unwrap_err();
    assert_eq!(result.code, observed.code);
    assert_eq!(result.outcome, observed.outcome);
    assert_eq!(result.details, observed.details);
}
