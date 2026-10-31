use ivy_agent_manager::{
    local_state::{Limits as StateLimits, LocalAgentState},
    native_dispatch::{NativeDispatch, Observations, journal_events},
    native_journal::{Identity, Limits, NativeJournal, Owner},
    native_operations::{NativeOperations, SqliteOperationStore},
    service_work::AgentWork,
    shutdown::{AgentShutdown, ShutdownPhases, ShutdownStep},
};
use ivy_contracts::{
    IvyError, IvyJson, Outcome,
    hash::hash_json,
    json::{parse, stringify},
};
use ivy_host_runtime::codex_rpc::NativeRpc;
use ivy_sdk::service::{InvocationContext, ToolHandler};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::oneshot,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-agent-shutdown-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn context() -> InvocationContext {
    InvocationContext::detached(
        "caller".into(),
        None,
        1,
        Default::default(),
        IvyError::internal(),
    )
}
fn noop() -> Arc<ShutdownStep> {
    Arc::new(|| Box::pin(async { Ok(()) }))
}
fn phases(work: &AgentWork) -> ShutdownPhases {
    let work = work.clone();
    ShutdownPhases {
        stop_timer: Arc::new(|| {}),
        admissions: Arc::new(move || work.close()),
        native: noop(),
        work: noop(),
        service: noop(),
        instructions: noop(),
        mcp: noop(),
        skills: noop(),
        locations: noop(),
        state: noop(),
        pending_health: noop(),
        final_health: Arc::new(|_| Box::pin(async { Ok(()) })),
        journal: Arc::new(|| Ok(())),
    }
}

#[tokio::test]
async fn accepted_native_call_commits_original_receipt_before_retirement_despite_dropped_waiters() {
    let root = Root::new();
    let database = root.0.join("agent.sqlite");
    let state = Arc::new(Mutex::new(
        LocalAgentState::open(
            &database,
            "fixture.agent".into(),
            StateLimits {
                max_operations: 10,
                max_journal_bytes: 64 * 1024 * 1024,
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
                max_journal_bytes: 64 * 1024 * 1024,
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
    let operations = NativeOperations::new(
        Arc::new(SqliteOperationStore::new(state.clone()).unwrap()),
        journal.clone(),
    );
    let receipts = operations.reconcile_terminals(Arc::new(|error| panic!("{error:?}")));
    let catalog = parse(
        include_str!("../../specs/native/codex-0.154.0/catalog.json"),
        2 * 1024 * 1024,
    )
    .unwrap();
    let (input, sent) = tokio::io::duplex(65536);
    let (mut emit, output) = tokio::io::duplex(65536);
    let rpc = NativeRpc::start(
        Arc::new(catalog),
        input,
        output,
        journal_events(
            "fixture.agent".into(),
            "epoch".into(),
            &operations,
            Observations::default(),
        ),
    )
    .unwrap();
    let closing = Arc::new(AtomicBool::new(false));
    let dispatch = NativeDispatch::new(
        rpc.clone(),
        "epoch".into(),
        operations.clone(),
        closing.clone(),
    );
    let identity = Identity {
        caller_principal_id: "caller".into(),
        operation_id: format!("epoch:{}:original", chrono::Utc::now().timestamp_millis()).into(),
    };
    let work = AgentWork::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let original = identity.clone();
    let handler: ToolHandler = Arc::new(move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        let dispatch = dispatch.clone();
        let identity = original.clone();
        Box::pin(async move {
            dispatch
                .invoke(identity, "account/logout".into(), IvyJson::Null)?
                .wait()
                .await
        })
    });
    let handler = work.handler(handler);
    // No yield before close: accepted work has not reached NativeDispatch yet.
    let waiter = handler(IvyJson::Null, context());
    let retired = Arc::new(AtomicBool::new(false));
    let mut steps = phases(&work);
    let native = rpc.clone();
    let flag = retired.clone();
    let gate = closing.clone();
    steps.native = Arc::new(move || {
        assert!(gate.load(Ordering::Acquire));
        let native = native.clone();
        let flag = flag.clone();
        Box::pin(async move {
            native.close("native_owner_stopping").await;
            flag.store(true, Ordering::Release);
            Ok(())
        })
    });
    steps.work = Arc::new(move || {
        let receipts = receipts.clone();
        Box::pin(async move {
            receipts.drain().await;
            Ok(())
        })
    });
    let close_state = state.clone();
    let after_native = retired.clone();
    steps.state = Arc::new(move || {
        assert!(after_native.load(Ordering::Acquire));
        let result = close_state.lock().unwrap().close_shared();
        Box::pin(async move { result })
    });
    let close_journal = journal.clone();
    steps.journal = Arc::new(move || {
        close_journal.lock().unwrap().close();
        Ok(())
    });
    let shutdown = AgentShutdown::new(steps, closing.clone());
    let first = shutdown.close("agent_stopped".into()).unwrap();
    let joined = shutdown.close("ignored".into()).unwrap();
    assert!(first.shares_completion(&joined));
    drop(first);
    drop(waiter);
    let error = handler(IvyJson::Null, context()).await.unwrap_err();
    assert_eq!(error.code, "service_unavailable");
    assert_eq!(error.outcome, Outcome::NotExecuted);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Rejected handler factory must not run"
    );
    let mut sent = BufReader::new(sent);
    let mut line = String::new();
    let bytes = tokio::time::timeout(Duration::from_secs(5), sent.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert!(
        bytes > 0,
        "Native owner retired before the accepted request was sent"
    );
    let request = parse(&line, 65536).unwrap();
    assert_eq!(request.get("method"), Some(&"account/logout".into()));
    assert!(!retired.load(Ordering::Acquire));
    assert!(
        !closing.load(Ordering::Acquire),
        "Accepted dispatch still owns availability"
    );
    assert_eq!(
        operations
            .read(&identity)
            .await
            .unwrap()
            .unwrap()
            .get("phase"),
        Some(&"dispatched".into())
    );
    let reply = IvyJson::object([
        ("id", request.get("id").unwrap().clone()),
        ("result", IvyJson::Null),
    ]);
    emit.write_all(format!("{}\n", stringify(&reply, 65536).unwrap()).as_bytes())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), joined.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(shutdown.closed().await.unwrap(), "agent_stopped".into());
    assert!(retired.load(Ordering::Acquire));
    assert_eq!(work.pending().unwrap(), 0);
    let reopened = LocalAgentState::open(
        &database,
        "fixture.agent".into(),
        StateLimits {
            max_operations: 10,
            max_journal_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap();
    let receipt = reopened
        .read("operation", &hash_json(&identity.value()).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(receipt.value.get("phase"), Some(&"succeeded".into()));
}

#[tokio::test]
async fn admission_is_reserved_before_handler_factory_and_shutdown_does_not_hold_its_lock() {
    let work = AgentWork::default();
    let (entered, ready) = oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, wait) = std::sync::mpsc::channel();
    let wait = Mutex::new(wait);
    let handler = work.handler(Arc::new(move |_, _| {
        entered.lock().unwrap().take().unwrap().send(()).unwrap();
        wait.lock().unwrap().recv().unwrap();
        Box::pin(async { Ok(IvyJson::Null) })
    }));
    let runtime = tokio::runtime::Handle::current();
    let caller = tokio::task::spawn_blocking(move || {
        let _guard = runtime.enter();
        handler(IvyJson::Null, context())
    });
    tokio::time::timeout(Duration::from_secs(5), ready)
        .await
        .unwrap()
        .unwrap();
    let drain = work.close();
    assert_eq!(work.pending().unwrap(), 1);
    let drained = Arc::new(AtomicBool::new(false));
    let completed = drained.clone();
    let owner = tokio::spawn(async move {
        drain.await.unwrap();
        completed.store(true, Ordering::Release);
    });
    tokio::task::yield_now().await;
    assert!(!drained.load(Ordering::Acquire));
    release.send(()).unwrap();
    let waiter = caller.await.unwrap();
    assert_eq!(waiter.await.unwrap(), IvyJson::Null);
    tokio::time::timeout(Duration::from_secs(5), owner)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn pending_native_input_can_be_answered_during_drain_and_answer_finishes_before_retirement() {
    let work = AgentWork::default();
    let (answer, answered) = oneshot::channel();
    let call = work
        .track(Box::pin(async move {
            answered.await.unwrap();
            Ok(IvyJson::Null)
        }))
        .unwrap();
    let mut drain = work.close();
    let (finish, finished) = oneshot::channel();
    let send = Mutex::new(Some((answer, finished)));
    let handler = work.reply_handler(Arc::new(move |_, _| {
        let (answer, finished) = send
            .lock()
            .unwrap()
            .take()
            .expect("Late answer factory must not run");
        Box::pin(async move {
            answer.send(()).unwrap();
            finished.await.unwrap();
            Ok(IvyJson::Null)
        })
    }));
    let reply = handler(IvyJson::Null, context());
    call.wait().await.unwrap();
    tokio::select! {
        biased;
        _ = &mut drain => panic!("Answer still owns pending work"),
        _ = tokio::task::yield_now() => {}
    }
    assert_eq!(
        handler(IvyJson::Null, context()).await.unwrap_err().outcome,
        Outcome::NotExecuted
    );
    finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .unwrap()
        .unwrap();
    reply.await.unwrap();
    assert_eq!(work.pending().unwrap(), 0);
}

#[tokio::test]
async fn admission_failure_still_retires_native_and_preserves_first_failure_through_cleanup() {
    let work = AgentWork::default();
    let mut steps = phases(&work);
    steps.admissions =
        Arc::new(|| Box::pin(async { Err(IvyError::new("admission_failed", "fixture")) }));
    let native_ran = Arc::new(AtomicBool::new(false));
    let flag = native_ran.clone();
    steps.native = Arc::new(move || {
        flag.store(true, Ordering::Release);
        Box::pin(async { Err(IvyError::new("native_failed", "fixture")) })
    });
    let finalized = Arc::new(AtomicBool::new(false));
    let flag = finalized.clone();
    steps.journal = Arc::new(move || {
        flag.store(true, Ordering::Release);
        Err(IvyError::new("journal_failed", "fixture"))
    });
    let shutdown = AgentShutdown::new(steps, Arc::new(AtomicBool::new(false)));
    assert_eq!(
        shutdown
            .close("agent_stopped".into())
            .unwrap()
            .wait()
            .await
            .unwrap_err()
            .code,
        "admission_failed"
    );
    assert!(native_ran.load(Ordering::Acquire));
    assert!(finalized.load(Ordering::Acquire));
    assert_eq!(
        shutdown.closed().await.unwrap(),
        "agent_shutdown_failed".into()
    );
}
