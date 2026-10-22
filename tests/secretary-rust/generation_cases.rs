use super::*;
use ivy_contracts::checked_native_contract::CheckedNativeContracts;
use ivy_sdk::{
    clock::Clock,
    service::{ConnectionOptions, InvocationContext, ServiceConnection, ServiceState},
};
use ivy_secretary::{
    assignment_driver::AssignmentDriver,
    assignment_execution::AssignmentExecution,
    assignment_native::AssignmentNative,
    assignment_scheduler::AssignmentScheduler,
    engine::SecretaryEngine,
    generation::{ReconciledGeneration, ServiceEvent},
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    service_handlers::SecretaryHandlers,
    store::Store,
};
use std::{
    cell::RefCell,
    future::{Future, poll_fn},
    io::{BufRead, BufReader},
    pin::Pin,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::Poll,
};
use tokio::sync::Notify;

struct SocketFixture(Child);
impl Drop for SocketFixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[derive(Default)]
struct Gate {
    seen: AtomicBool,
    released: AtomicBool,
    changed: Notify,
}
impl Gate {
    async fn wait(&self) {
        self.seen.store(true, Ordering::SeqCst);
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.released.load(Ordering::SeqCst) {
                break;
            }
            changed.await;
        }
    }
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
}
struct GenerationClient {
    fixture: IvyJson,
    configuration: Gate,
    notice: Gate,
    abandoned: Gate,
    block_owner: AtomicBool,
    configuration_queries: AtomicUsize,
    assignment_queries: AtomicUsize,
    notice_queries: AtomicUsize,
}
impl RpcClient for GenerationClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            match method {
                "serviceNodes.get" => {
                    if self.block_owner.swap(false, Ordering::SeqCst) {
                        self.abandoned.wait().await;
                    }
                    Ok(self.fixture.required("owner")?.clone())
                }
                "objects.query" => {
                    let key = params.required("contractKey")?.as_str()?;
                    if key == "secretary/configuration" {
                        self.configuration_queries.fetch_add(1, Ordering::SeqCst);
                        self.configuration.wait().await;
                        Ok(IvyJson::object([
                            (
                                "items",
                                IvyJson::Array(vec![IvyJson::object([
                                    (
                                        "objectId",
                                        self.fixture
                                            .required("document")?
                                            .required("object")?
                                            .required("id")?
                                            .clone(),
                                    ),
                                    ("revision", IvyJson::Number(1.0)),
                                ])]),
                            ),
                            ("nextCursor", IvyJson::Null),
                        ]))
                    } else {
                        if key == "secretary/item" {
                            self.notice_queries.fetch_add(1, Ordering::SeqCst);
                            self.notice.wait().await;
                        }
                        if key == "secretary/assignment" {
                            self.assignment_queries.fetch_add(1, Ordering::SeqCst);
                        }
                        assert!(
                            [
                                "secretary/item",
                                "secretary/assignment",
                                "secretary/execution"
                            ]
                            .contains(&key.as_str()),
                            "Unexpected query {key}"
                        );
                        Ok(IvyJson::object([
                            ("items", IvyJson::Array(vec![])),
                            ("nextCursor", IvyJson::Null),
                        ]))
                    }
                }
                "objects.read" => Ok(self.fixture.required("document")?.clone()),
                _ => panic!("Unexpected generation RPC {method}"),
            }
        })
    }
}
struct FixedClock;
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        1800000000000
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
async fn drive_until<F: Future>(mut work: Pin<&mut F>, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            assert!(
                poll_fn(|cx| Poll::Ready(work.as_mut().poll(cx)))
                    .await
                    .is_pending(),
                "Generation exited before its accepted work was joined"
            );
            if done() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Generation did not reach the expected fixture gate");
}
fn state(status: &'static str) -> ServiceEvent {
    ServiceEvent::State(ServiceState {
        status,
        generation: Some(7),
        code: None,
        message: None,
        phase: None,
    })
}
async fn exercise_generation(replace: bool) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/generation-fixture.mjs")
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut socket = SocketFixture(
        command
            .spawn()
            .expect("Original schema fixture/socket required"),
    );
    let mut line = String::new();
    BufReader::new(socket.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let fixture = parse(&line, 1024 * 1024).unwrap();
    let address = format!(
        "http://127.0.0.1:{}",
        fixture.required("port").unwrap().as_f64().unwrap() as u16
    );
    let connection = ServiceConnection::connect(
        &address,
        "fixture",
        ConnectionOptions::new("fixture.secretary".into()),
        RequestOptions::default().cancellation,
    )
    .await
    .unwrap();
    connection.set_generation(7);
    let client = Arc::new(GenerationClient {
        fixture: fixture.clone(),
        configuration: Gate::default(),
        notice: Gate::default(),
        abandoned: Gate::default(),
        block_owner: AtomicBool::new(false),
        configuration_queries: AtomicUsize::new(0),
        assignment_queries: AtomicUsize::new(0),
        notice_queries: AtomicUsize::new(0),
    });
    let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
    let store = Store {
        client: client.clone(),
        root: fixture
            .required("rootId")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        journal: &journal,
    };
    let node = "fixture.secretary".into();
    let host = "fixture-host".into();
    let evidence = InboxEvidence {
        store: &store,
        settings: fixture.required("settings").unwrap(),
        service_node_id: &node,
        cancellation: RequestOptions {
            cancellation: connection.signal(),
            ..RequestOptions::default()
        },
    };
    let clock = FixedClock;
    let tools = Arc::new(ToolClients::new(client.clone()));
    let retained = CheckedNativeContracts::new(root);
    let issues = RefCell::new(RecoveryScan::default());
    issues.borrow_mut().issue("follow-ups", "old");
    let engine = SecretaryEngine {
        evidence: &evidence,
        clock: &clock,
        operation_clock: &clock,
        runtime_epoch: epoch_fixture(&"fixture-epoch".into()),
        new_uuid: || "00000000-0000-4000-8000-000000000fff".into(),
        tools: tools.as_ref(),
        issues: &issues,
    };
    let policy = "Fixture policy".into();
    let execution = AssignmentExecution {
        native: AssignmentNative {
            evidence: &evidence,
            tools: tools.clone(),
        },
        clock: &clock,
        retained: &retained,
        notification_policy: &policy,
    };
    let (runner, driver) =
        AssignmentDriver::new(&execution, 0.0, &issues, std::env::temp_dir(), None);
    let generation = ReconciledGeneration {
        connection: connection.clone(),
        handlers: SecretaryHandlers {
            engine: &engine,
            generation: 7,
            service_node_id: &node,
            host_id: &host,
            tools: tools.clone(),
            retained: &retained,
        },
        scheduler: AssignmentScheduler {
            evidence: &evidence,
            tools: tools.clone(),
            clock: &clock,
            fallback_execution: None,
        },
        runner,
        minimum_message_age_minutes: 0.0,
    };
    let (sender, mut invocations) = tokio::sync::mpsc::unbounded_channel();
    let callbacks = ivy_secretary::service_handlers::registered_handlers(sender);
    let (event, mut events) = tokio::sync::mpsc::unbounded_channel();
    let mut work = Box::pin(generation.run(driver, &mut invocations, &mut events, |_| {}));
    assert!(event.send(state("ready")).is_ok());
    drive_until(work.as_mut(), || {
        client.configuration.seen.load(Ordering::SeqCst)
            && client.notice.seen.load(Ordering::SeqCst)
            && !issues
                .borrow()
                .issues
                .iter()
                .any(|(key, _)| key == "follow-ups")
    })
    .await;
    for _ in 0..8 {
        assert!(event.send(state("degraded")).is_ok());
        assert!(event.send(state("ready")).is_ok());
    }
    let context = InvocationContext {
        caller_principal_id: "caller".into(),
        operation_id: None,
        generation: 7,
        signal: connection.signal(),
    };
    let binding = callbacks.get(&"secretary.binding".into()).unwrap();
    let mut response = binding(IvyJson::Null, context.clone());
    let binding_result=tokio::time::timeout(std::time::Duration::from_secs(5),async {tokio::select!{_=&mut work=>panic!("Generation returned while two workers were blocked"),reply=&mut response=>reply.unwrap()}}).await.unwrap();
    assert_eq!(binding_result.get("available"), Some(&true.into()));
    assert_eq!(
        client.configuration_queries.load(Ordering::SeqCst),
        1,
        "No overlapping foreground chain"
    );
    assert_eq!(
        client.notice_queries.load(Ordering::SeqCst),
        1,
        "No duplicate busy notice worker"
    );
    client.configuration.release();
    drive_until(work.as_mut(), || {
        client.assignment_queries.load(Ordering::SeqCst) == 2
    })
    .await;
    // Let the second runner pass settle without reaching the real poll timer.
    for _ in 0..64 {
        drive_until(work.as_mut(), || true).await;
    }
    assert_eq!(
        client.configuration_queries.load(Ordering::SeqCst),
        2,
        "Eight busy wakes coalesce to one subsequent foreground pass"
    );
    assert_eq!(client.notice_queries.load(Ordering::SeqCst), 1);
    client.block_owner.store(true, Ordering::SeqCst);
    let mut abandoned = binding(IvyJson::Null, context.clone());
    assert!(
        poll_fn(|cx| Poll::Ready(abandoned.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    drop(abandoned);
    drive_until(work.as_mut(), || {
        client.abandoned.seen.load(Ordering::SeqCst)
    })
    .await;
    let replacement = if replace {
        let next = ServiceConnection::connect(
            &address,
            "fixture",
            ConnectionOptions::new("fixture.secretary".into()),
            RequestOptions::default().cancellation,
        )
        .await
        .unwrap();
        next.set_generation(8);
        assert!(event.send(ServiceEvent::Reconcile(next.clone())).is_ok());
        Some(next)
    } else {
        assert!(event.send(ServiceEvent::Stop).is_ok());
        None
    };
    drive_until(work.as_mut(), || connection.signal().is_cancelled()).await;
    let mut late = binding(IvyJson::Null, context);
    let error=tokio::time::timeout(std::time::Duration::from_secs(5),async {tokio::select!{_=&mut work=>panic!("Generation dropped accepted RPCs during shutdown"),reply=&mut late=>reply.unwrap_err()}}).await.unwrap();
    assert_eq!(error.code, "service_not_ready");
    client.notice.release();
    for _ in 0..32 {
        drive_until(work.as_mut(), || true).await;
    }
    client.abandoned.release();
    let exit = tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .unwrap();
    match (exit.next, replacement) {
        (Some(ServiceEvent::Stop), None) => {}
        (Some(ServiceEvent::Reconcile(next)), Some(expected)) => {
            assert_eq!(next.generation(), 8);
            assert_eq!(next.generation(), expected.generation());
            next.check_open()
                .expect("Joining the old generation must not close its replacement");
            next.close();
        }
        _ => panic!("The consumed lifecycle event changed during drain"),
    }
    assert_eq!(
        journal
            .db
            .query_row("SELECT COUNT(*) FROM technical_documents", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn independent_workers_coalescing_and_abandoned_handler_join_before_replacement() {
    exercise_generation(false).await;
    exercise_generation(true).await;
}
