use super::reconciliation_cases::{health, journal, manager};
use super::{Root, compare, field, oracle, text};
use ivy_agent_manager::{
    local_state::{Limits, LocalAgentState, Pin, SavedState},
    native_journal::Identity,
    native_operations::{
        NativeOperationStore, NativeOperations, SqliteOperationStore, StoreFuture,
    },
    service_work::AgentWork,
    shutdown::{AgentShutdown, AgentShutdownWork, FinalHealth, ShutdownPhases, ShutdownStep},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, hash::hash_json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, Ordering},
};
use tokio::sync::watch;
type Trace = Arc<Mutex<Vec<IvyJson>>>;
fn trace(out: &Trace, name: &str) {
    out.lock().unwrap().push(name.into());
}
fn failure(name: &str) -> IvyError {
    let mut message = JsString::from(format!("Original {name} cleanup failure"));
    message.0.push(0xd800);
    IvyError::new_js(
        if name == "native" {
            "outcome_unknown".into()
        } else {
            format!("fixture_{name}_failed").into()
        },
        message,
    )
    .with_outcome(if name == "native" {
        Outcome::Unknown
    } else {
        Outcome::NotExecuted
    })
    .with_details(IvyJson::object([
        ("phase", name.into()),
        ("retained", IvyJson::String(JsString(vec![0xdfff]))),
    ]))
}
fn message(code: JsString) -> JsString {
    let mut text = JsString::from("AgentManager stopped: ");
    text.0.extend(code.0);
    text.0.push(46);
    text
}
fn result(result: IvyResult<()>) -> IvyJson {
    match result {
        Ok(()) => IvyJson::object([("result", IvyJson::Null)]),
        Err(error) => IvyJson::object([("error", error.to_wire())]),
    }
}
fn basic(
    calls: Trace,
    fail: Vec<JsString>,
    missing: Vec<JsString>,
    health_codes: Trace,
) -> ShutdownPhases {
    let step = |name: &'static str| -> Arc<ShutdownStep> {
        let calls = calls.clone();
        let skip = missing.contains(&name.into());
        let fail = fail.contains(&name.into());
        Arc::new(move || {
            if !skip {
                trace(&calls, name);
            }
            Box::pin(async move {
                if !skip && fail {
                    Err(failure(name))
                } else {
                    Ok(())
                }
            })
        })
    };
    let final_health: Arc<FinalHealth> = {
        let calls = calls.clone();
        let fail = fail.contains(&"health".into());
        Arc::new(move |code| {
            health_codes
                .lock()
                .unwrap()
                .push(IvyJson::String(message(code)));
            trace(&calls, "health");
            Box::pin(async move { if fail { Err(failure("health")) } else { Ok(()) } })
        })
    };
    ShutdownPhases {
        stop_timer: {
            let calls = calls.clone();
            Arc::new(move || trace(&calls, "timer"))
        },
        native: step("native"),
        work: step("work-snapshot"),
        service: step("service"),
        instructions: step("instructions"),
        mcp: step("mcp"),
        skills: step("skills"),
        locations: step("locations"),
        state: step("state"),
        pending_health: step("health-work"),
        final_health,
        journal: {
            let fail = fail.contains(&"journal".into());
            Arc::new(move || {
                trace(&calls, "journal");
                if fail {
                    Err(failure("journal"))
                } else {
                    Ok(())
                }
            })
        },
    }
}
#[tokio::test]
async fn shutdown_preserves_original_success_and_the_exact_parent_approved_failure_exceptions() {
    let data = oracle("agent-shutdown-oracle.mjs", None);
    let rows = field(&data, "cases").as_array().unwrap();
    assert_eq!(rows.len(), 17);
    for row in rows {
        let calls = Trace::default();
        let health_codes = Trace::default();
        let closing = Arc::new(AtomicBool::new(false));
        let strings = |key| {
            row.get(key)
                .and_then(IvyJson::as_array)
                .unwrap_or(&Vec::new())
                .iter()
                .map(|value| value.as_js_string().unwrap().clone())
                .collect::<Vec<_>>()
        };
        let owner = AgentShutdown::new(
            basic(
                calls.clone(),
                strings("fail"),
                strings("missing"),
                health_codes.clone(),
            ),
            closing.clone(),
        );
        let requested = row
            .get("requestedCode")
            .and_then(IvyJson::as_js_string)
            .cloned()
            .unwrap_or_else(|| "agent_stopped".into());
        let first = owner.close(requested).unwrap();
        let second = owner.close("different".into()).unwrap();
        assert!(closing.load(Ordering::Acquire));
        assert!(first.shares_completion(&second));
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), first.wait())
            .await
            .unwrap();
        let closed = owner.closed().await.unwrap();
        trace(&calls, "closed");
        assert!(first.shares_completion(&owner.close("again".into()).unwrap()));
        let mut actual = result(outcome);
        actual.insert("samePromise", true.into());
        actual.insert("closedCode", IvyJson::String(closed));
        actual.insert("calls", IvyJson::Array(calls.lock().unwrap().clone()));
        actual.insert(
            "healthCodes",
            IvyJson::Array(health_codes.lock().unwrap().clone()),
        );
        compare(
            &actual,
            field(row, "approved"),
            &text(row, "name").to_utf8().unwrap(),
        );
    }
}
#[derive(Clone)]
struct Gate {
    entered: watch::Sender<bool>,
    release: watch::Sender<bool>,
    finished: watch::Sender<bool>,
}
impl Gate {
    fn new() -> Self {
        Self {
            entered: watch::channel(false).0,
            release: watch::channel(false).0,
            finished: watch::channel(false).0,
        }
    }
    async fn wait(sender: &watch::Sender<bool>) {
        let mut receiver = sender.subscribe();
        while !*receiver.borrow_and_update() {
            receiver.changed().await.unwrap();
        }
    }
    async fn block(&self) {
        self.entered.send_replace(true);
        Self::wait(&self.release).await;
    }
    fn step(&self, name: &'static str, calls: Trace, fail: bool) -> Arc<ShutdownStep> {
        let gate = self.clone();
        Arc::new(move || {
            trace(&calls, name);
            gate.entered.send_replace(true);
            let gate = gate.clone();
            let calls = calls.clone();
            Box::pin(async move {
                Gate::wait(&gate.release).await;
                trace(&calls, &format!("{name}-settled"));
                gate.finished.send_replace(true);
                if fail { Err(failure(name)) } else { Ok(()) }
            })
        })
    }
}
#[tokio::test]
async fn all_managers_settle_before_later_cleanup_and_abandoned_waiters_keep_first_observed_failure()
 {
    for native_failed in [false, true] {
        let calls = Trace::default();
        let mut phases = basic(
            calls.clone(),
            if native_failed {
                vec!["native".into()]
            } else {
                vec![]
            },
            vec![],
            Trace::default(),
        );
        let instructions = Gate::new();
        let mcp = Gate::new();
        let skills = Gate::new();
        phases.instructions = instructions.step("instructions", calls.clone(), true);
        phases.mcp = mcp.step("mcp", calls.clone(), true);
        phases.skills = skills.step("skills", calls.clone(), true);
        let owner = AgentShutdown::new(phases, Arc::new(AtomicBool::new(false)));
        let run = owner.close("agent_stopped".into()).unwrap();
        drop(run.wait());
        drop(run);
        for gate in [&instructions, &mcp, &skills] {
            Gate::wait(&gate.entered).await;
        }
        assert!(!calls.lock().unwrap().contains(&"locations".into()));
        mcp.release.send_replace(true);
        Gate::wait(&mcp.finished).await;
        instructions.release.send_replace(true);
        Gate::wait(&instructions.finished).await;
        assert!(!calls.lock().unwrap().contains(&"state".into()));
        skills.release.send_replace(true);
        let repeated = owner.close("different".into()).unwrap();
        let error = repeated.wait().await.unwrap_err();
        assert_eq!(
            error.to_wire(),
            failure(if native_failed { "native" } else { "mcp" }).to_wire()
        );
        assert_eq!(
            owner.closed().await.unwrap(),
            "agent_shutdown_failed".into()
        );
        let calls = calls.lock().unwrap();
        let state = calls
            .iter()
            .position(|value| value == &"state".into())
            .unwrap();
        for name in ["instructions-settled", "mcp-settled", "skills-settled"] {
            assert!(
                calls
                    .iter()
                    .position(|value| value == &name.into())
                    .unwrap()
                    < state
            );
        }
        assert_eq!(
            calls
                .iter()
                .filter(|name| *name == &IvyJson::from("native"))
                .count(),
            1
        );
    }
}
struct ReceiptStore {
    inner: SqliteOperationStore,
    gate: Gate,
    written: Arc<AtomicBool>,
}
impl NativeOperationStore for ReceiptStore {
    fn service_node_id(&self) -> &JsString {
        self.inner.service_node_id()
    }
    fn read(&self, key: String) -> StoreFuture<'_, Option<SavedState>> {
        Box::pin(async move {
            self.gate.block().await;
            self.inner.read(key).await
        })
    }
    fn write(
        &self,
        key: String,
        value: IvyJson,
        previous: Option<Pin>,
    ) -> StoreFuture<'_, SavedState> {
        Box::pin(async move {
            let result = self.inner.write(key, value, previous).await?;
            self.written.store(true, Ordering::SeqCst);
            Ok(result)
        })
    }
}
#[tokio::test]
async fn native_unknown_still_drains_abandoned_invocation_and_real_terminal_receipt_before_sqlite_close()
 {
    let root = Root::new();
    let path = root.path.join("agent-runtime.sqlite");
    let journal = journal(100, 64 * 1024 * 1024);
    journal
        .lock()
        .unwrap()
        .begin_epoch(&"native-epoch".into())
        .unwrap();
    let mut database = LocalAgentState::open(
        &path,
        "owner".into(),
        Limits {
            max_operations: 100,
            max_journal_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap();
    database.set_runtime_epoch("epoch".into()).unwrap();
    let state = Arc::new(Mutex::new(database));
    let store = SqliteOperationStore::new(state.clone()).unwrap();
    let identity = Identity {
        caller_principal_id: "caller".into(),
        operation_id: format!("epoch:{}:original", chrono::Utc::now().timestamp_millis()).into(),
    };
    let key = hash_json(&identity.value()).unwrap();
    let request_id = IvyJson::from("request");
    let intent = {
        let mut journal = journal.lock().unwrap();
        journal
            .accept(
                &identity,
                &"thread/start".into(),
                &IvyJson::object::<&str>([]),
            )
            .unwrap();
        journal
            .dispatch(&identity, &"native-epoch".into(), &request_id)
            .unwrap()
    };
    store.write(key.clone(), intent, None).await.unwrap();
    let receipt_gate = Gate::new();
    let receipt_written = Arc::new(AtomicBool::new(false));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let operations = NativeOperations::new(
        Arc::new(ReceiptStore {
            inner: store,
            gate: receipt_gate.clone(),
            written: receipt_written.clone(),
        }),
        journal.clone(),
    );
    let receipts = operations.reconcile_terminals({
        let errors = errors.clone();
        Arc::new(move |error| errors.lock().unwrap().push(error))
    });
    let invocations = AgentWork::default();
    let work_gate = Gate::new();
    let work_written = Arc::new(AtomicBool::new(false));
    drop(
        invocations
            .track(Box::pin({
                let gate = work_gate.clone();
                let state = state.clone();
                let written = work_written.clone();
                async move {
                    gate.block().await;
                    state.lock().unwrap().write(
                        "allocation",
                        "owned",
                        &IvyJson::object([("retained", "yes".into())]),
                        None,
                    )?;
                    written.store(true, Ordering::SeqCst);
                    Ok(IvyJson::Null)
                }
            }))
            .unwrap(),
    );
    let closing = Arc::new(AtomicBool::new(false));
    let inventory = manager(
        journal.clone(),
        closing.clone(),
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(AtomicI64::new(42000)),
    );
    let work = AgentShutdownWork {
        invocations,
        receipts,
        inventory,
    };
    let calls = Trace::default();
    let mut phases = basic(calls.clone(), vec![], vec![], Trace::default());
    let fence = root.path.join("process-stop-fence.json");
    std::fs::write(&fence, b"retained unknown-stop fixture").unwrap();
    phases.native = {
        let journal = journal.clone();
        let identity = identity.clone();
        Arc::new(move || {
            let journal = journal.clone();
            let identity = identity.clone();
            let request_id = request_id.clone();
            Box::pin(async move {
                journal.lock().unwrap().finish(
                    &identity,
                    &"native-epoch".into(),
                    &request_id,
                    &IvyJson::object([("result", IvyJson::object::<&str>([]))]),
                )?;
                Err(failure("native"))
            })
        })
    };
    phases.work = Arc::new(move || work.drain());
    let database_closed = Arc::new(AtomicBool::new(false));
    phases.state = {
        let state = state.clone();
        let receipt_written = receipt_written.clone();
        let work_written = work_written.clone();
        let closed = database_closed.clone();
        Arc::new(move || {
            let state = state.clone();
            let receipt_written = receipt_written.clone();
            let work_written = work_written.clone();
            let closed = closed.clone();
            Box::pin(async move {
                assert!(receipt_written.load(Ordering::SeqCst));
                assert!(work_written.load(Ordering::SeqCst));
                state.lock().unwrap().close_shared()?;
                closed.store(true, Ordering::SeqCst);
                Ok(())
            })
        })
    };
    let health = health(&root, journal.clone());
    drop(health.write(false, None, "pending".into()).unwrap());
    phases.pending_health = {
        let health = health.clone();
        Arc::new(move || health.drain_snapshot())
    };
    phases.final_health = {
        let health = health.clone();
        Arc::new(move |code| {
            let file = health.file().clone();
            Box::pin(async move { file.write(false, message(code), None).await })
        })
    };
    phases.journal = {
        let journal = journal.clone();
        Arc::new(move || {
            journal.lock().unwrap().close();
            Ok(())
        })
    };
    let owner = AgentShutdown::new(phases, closing);
    drop(owner.close("agent_stopped".into()).unwrap());
    Gate::wait(&work_gate.entered).await;
    Gate::wait(&receipt_gate.entered).await;
    assert!(!database_closed.load(Ordering::SeqCst));
    work_gate.release.send_replace(true);
    receipt_gate.release.send_replace(true);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        owner.close("other".into()).unwrap().wait(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.to_wire(), failure("native").to_wire());
    assert!(database_closed.load(Ordering::SeqCst));
    assert!(errors.lock().unwrap().is_empty());
    assert!(state.lock().unwrap().read("allocation", "owned").is_err());
    assert_eq!(
        std::fs::read(&fence).unwrap(),
        b"retained unknown-stop fixture"
    );
    let reopened = LocalAgentState::open(
        &path,
        "owner".into(),
        Limits {
            max_operations: 100,
            max_journal_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap();
    assert!(reopened.read("allocation", "owned").unwrap().is_some());
    assert_eq!(
        reopened
            .read("operation", &key)
            .unwrap()
            .unwrap()
            .value
            .get("phase"),
        Some(&"succeeded".into())
    );
    reopened.close().unwrap();
    assert!(journal.lock().unwrap().epoch().is_none());
}
