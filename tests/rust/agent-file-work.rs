use ivy_agent_manager::{
    claude_mcp::{ClaudeMcpManager, ClaudeMcpSettings},
    instructions::ComposedInstructions,
    instructions_files::InstructionsSource,
    instructions_writer::InstructionsWriter,
    local_state::{Pin, SavedState},
    mcp_manager::{McpManager, McpRun, McpSettings},
    native_journal::{Identity, Limits, NativeJournal, Owner},
    native_operations::{NativeOperationStore, NativeOperations, StoreFuture},
    service_work::AgentWork,
    skills_manager::{SkillsManager, SkillsSettings},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::digest};
use ivy_host_runtime::node_path;
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::{InvocationContext, ToolHandler},
};
use std::{
    future::{Future, poll_fn},
    path::{Path, PathBuf},
    pin::Pin as FuturePin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};
use tokio::sync::Semaphore;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-file-work-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Gate {
    entered: Semaphore,
    release: Semaphore,
}
impl Gate {
    fn new() -> Self {
        Self {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
    async fn enter(&self) {
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
}
struct Client {
    calls: AtomicUsize,
    gate: Option<Arc<Gate>>,
    panic: bool,
}
impl Client {
    fn new(gate: Option<Arc<Gate>>, panic: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            gate,
            panic,
        })
    }
}
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        Box::pin(async move {
            if first {
                if let Some(gate) = &self.gate {
                    gate.enter().await;
                }
                assert!(!self.panic, "fixture application panic");
            }
            match method {
                "objects.stat" => Err(IvyError::new("not_found", "Fixture has no override")),
                "openai.getConfiguration" => Ok(IvyJson::object([
                    ("revision", IvyJson::Number(0.0)),
                    (
                        "configuration",
                        IvyJson::object([
                            ("schemaVersion", IvyJson::Number(1.0)),
                            ("appId", IvyJson::Null),
                        ]),
                    ),
                ])),
                _ => panic!("Unexpected request {method}"),
            }
        })
    }
}
enum Manager {
    Codex(McpManager),
    Claude(ClaudeMcpManager),
    Skills(SkillsManager),
}
impl Manager {
    fn new(root: &Root, kind: u8) -> Self {
        let defaults = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("hostId", IvyJson::Null),
            ("enabled", true.into()),
            ("servers", IvyJson::Array(vec![])),
        ]);
        match kind {
            0 => Self::Codex(McpManager::new(McpSettings {
                home: root.0.clone(),
                host_id: "fixture".into(),
                hive_url: "https://fixture.example".into(),
                credential: "synthetic".into(),
                defaults,
                credentials: Default::default(),
            })),
            1 => Self::Claude(
                ClaudeMcpManager::new(ClaudeMcpSettings {
                    target: node_path::from_native(&root.0.join("mcp.json")),
                    host_id: "fixture".into(),
                    hive_url: "https://fixture.example".into(),
                    credential: "synthetic".into(),
                    defaults,
                    credentials: Default::default(),
                })
                .unwrap(),
            ),
            _ => Self::Skills(SkillsManager::new(SkillsSettings {
                state_home: node_path::from_native(&root.0),
                root: node_path::from_native(&root.0.join("skills")),
                host_id: "fixture".into(),
                hive_url: "https://fixture.example".into(),
                defaults: IvyJson::object([
                    ("schemaVersion", IvyJson::Number(1.0)),
                    ("hostId", IvyJson::Null),
                    ("enabled", true.into()),
                    (
                        "files",
                        IvyJson::Array(vec![IvyJson::object([
                            ("path", "fixture/SKILL.md".into()),
                            ("content", "# Fixture".into()),
                        ])]),
                    ),
                ]),
                compare: Arc::new(|a, b| a.cmp(b)),
            })),
        }
    }
    fn synchronize(&self, client: Arc<dyn RpcClient>) -> McpRun {
        match self {
            Self::Codex(v) => v.synchronize(client),
            Self::Claude(v) => v.synchronize(client),
            Self::Skills(v) => v.synchronize(client),
        }
    }
    fn close(&self) -> McpRun {
        match self {
            Self::Codex(v) => v.close(),
            Self::Claude(v) => v.close(),
            Self::Skills(v) => v.close(),
        }
    }
    fn status(&self) -> IvyJson {
        match self {
            Self::Codex(v) => v.status().json(),
            Self::Claude(v) => v.status().json(),
            Self::Skills(v) => v.status().json(),
        }
    }
}

#[tokio::test]
async fn mcp_and_skills_join_the_original_source_outlive_waiters_and_can_apply_after_close() {
    for kind in 0..3 {
        let root = Root::new();
        let manager = Manager::new(&root, kind);
        let gate = Arc::new(Gate::new());
        let original = Client::new(Some(gate.clone()), false);
        let unused = Client::new(None, false);
        let first = manager.synchronize(original.clone());
        gate.entered().await;
        let joined = manager.synchronize(unused.clone());
        let mut close = Box::pin(manager.close().wait());
        drop((first, joined));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut close)
                .await
                .is_err()
        );
        assert_eq!(unused.calls.load(Ordering::SeqCst), 0);
        gate.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), close)
            .await
            .unwrap();
        assert_eq!(
            manager.status().get("state"),
            Some(&"applied".into()),
            "kind {kind}"
        );
        assert_eq!(unused.calls.load(Ordering::SeqCst), 0);
        manager.synchronize(unused.clone()).wait().await;
        assert!(unused.calls.load(Ordering::SeqCst) > 0);
        assert_eq!(manager.status().get("code"), Some(&IvyJson::Null));
    }
}

#[tokio::test]
async fn mcp_and_skills_publish_failure_after_panic_and_release_the_original_slot() {
    for kind in 0..3 {
        let root = Root::new();
        let manager = Manager::new(&root, kind);
        let gate = Arc::new(Gate::new());
        let panicking = Client::new(Some(gate.clone()), true);
        let run = manager.synchronize(panicking);
        gate.entered().await;
        let close = manager.close();
        gate.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), close.wait())
            .await
            .unwrap();
        run.wait().await;
        assert_eq!(manager.status().get("code"), Some(&"internal_error".into()));
        assert_eq!(manager.status().get("state"), Some(&"pending".into()));
        manager.synchronize(Client::new(None, false)).wait().await;
        assert_eq!(manager.status().get("state"), Some(&"applied".into()));
    }
}

struct Source {
    gate: Option<Arc<Gate>>,
    calls: AtomicUsize,
    panic: bool,
}
impl InstructionsSource for Source {
    fn desired<'a>(
        &'a self,
        _: &'a Path,
    ) -> FuturePin<Box<dyn Future<Output = IvyResult<ComposedInstructions>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                gate.enter().await;
            }
            assert!(!self.panic, "fixture instruction panic");
            Ok(ComposedInstructions {
                body: None,
                version: digest("disabled"),
            })
        })
    }
}
#[tokio::test]
async fn instruction_panic_notifies_the_captured_close_and_does_not_adopt_a_joining_source() {
    let root = Root::new();
    let writer = InstructionsWriter::new(
        root.0.clone(),
        "fixture".into(),
        "https://fixture.example".into(),
    );
    let gate = Arc::new(Gate::new());
    let panicking = Arc::new(Source {
        gate: Some(gate.clone()),
        calls: AtomicUsize::new(0),
        panic: true,
    });
    let unused = Arc::new(Source {
        gate: None,
        calls: AtomicUsize::new(0),
        panic: false,
    });
    drop(writer.synchronize(panicking));
    gate.entered().await;
    drop(writer.synchronize(unused.clone()));
    let close = writer.close();
    gate.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), close.wait())
        .await
        .unwrap();
    assert_eq!(writer.status().code.as_deref(), Some("internal_error"));
    assert_eq!(unused.calls.load(Ordering::SeqCst), 0);
    writer.synchronize(unused.clone()).wait().await;
    assert_eq!(writer.status().state, "disabled");
    assert_eq!(writer.status().code, None);
    assert_eq!(unused.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invocation_factory_and_task_panics_release_admission_without_retiring_the_owner() {
    let work = AgentWork::default();
    let handler: ToolHandler = Arc::new(|_, _| panic!("fixture factory panic"));
    let handler = work.handler(handler);
    let context = InvocationContext::detached(
        "fixture".into(),
        None,
        1,
        Default::default(),
        IvyError::internal(),
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(
            IvyJson::Null,
            context
        )))
        .is_err()
    );
    assert_eq!(work.pending().unwrap(), 0);
    let run = work
        .track(Box::pin(async { panic!("fixture operation panic") }))
        .unwrap();
    assert_eq!(run.wait().await.unwrap_err().code, "internal_error");
    work.drain_snapshot().await.unwrap();
    assert_eq!(work.pending().unwrap(), 0);
    assert_eq!(
        work.track(Box::pin(async { Ok("fresh".into()) }))
            .unwrap()
            .wait()
            .await
            .unwrap(),
        "fresh".into()
    );
}

#[tokio::test]
async fn invocation_snapshot_excludes_later_admission_while_close_owns_it() {
    let work = AgentWork::default();
    let first = Arc::new(Gate::new());
    let gate = first.clone();
    drop(
        work.track(Box::pin(async move {
            gate.enter().await;
            Ok(IvyJson::Null)
        }))
        .unwrap(),
    );
    first.entered().await;
    let snapshot = work.drain_snapshot();
    let second = Arc::new(Gate::new());
    let gate = second.clone();
    drop(
        work.track(Box::pin(async move {
            gate.enter().await;
            Ok(IvyJson::Null)
        }))
        .unwrap(),
    );
    second.entered().await;
    first.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), snapshot)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(work.pending().unwrap(), 1);
    let mut close = work.close();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut close)
            .await
            .is_err()
    );
    second.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), close)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(work.pending().unwrap(), 0);
}

struct Store {
    node: JsString,
    gates: [Arc<Gate>; 2],
    calls: AtomicUsize,
    panic: bool,
}
impl NativeOperationStore for Store {
    fn service_node_id(&self) -> &JsString {
        &self.node
    }
    fn read(&self, _: String) -> StoreFuture<'_, Option<SavedState>> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.gates[index].enter().await;
            assert!(!self.panic || index != 0, "fixture receipt panic");
            Err(IvyError::new("fixture_receipt_failed", "fixture"))
        })
    }
    fn write(&self, _: String, _: IvyJson, _: Option<Pin>) -> StoreFuture<'_, SavedState> {
        panic!("Failed fixture reads must not write")
    }
}
fn journal() -> Arc<Mutex<NativeJournal>> {
    let mut journal = NativeJournal::new(
        Owner {
            service_node_id: "fixture.agent".into(),
            native_version: "1".into(),
            native_executable_hash: digest("fixture").into(),
        },
        Limits {
            max_operations: 8,
            max_journal_bytes: 64 * 1024 * 1024,
            max_pending_inputs: 8,
        },
    )
    .unwrap();
    journal.begin_epoch(&"epoch".into()).unwrap();
    Arc::new(Mutex::new(journal))
}
fn terminal(journal: &Arc<Mutex<NativeJournal>>, index: u64) {
    let id = Identity {
        caller_principal_id: "caller".into(),
        operation_id: format!("epoch:{}:r{index}", chrono::Utc::now().timestamp_millis()).into(),
    };
    let request: IvyJson = format!("request-{index}").into();
    let mut journal = journal.lock().unwrap();
    journal
        .accept(&id, &"account/logout".into(), &IvyJson::Null)
        .unwrap();
    journal.dispatch(&id, &"epoch".into(), &request).unwrap();
    journal
        .finish(
            &id,
            &"epoch".into(),
            &request,
            &IvyJson::object([("result", IvyJson::Null)]),
        )
        .unwrap();
}
async fn receipt_drain(panic: bool) {
    let journal = journal();
    let store = Arc::new(Store {
        node: "fixture.agent".into(),
        gates: [Arc::new(Gate::new()), Arc::new(Gate::new())],
        calls: AtomicUsize::new(0),
        panic,
    });
    let operations = NativeOperations::new(store.clone(), journal.clone());
    let errors = Arc::new(Mutex::new(Vec::new()));
    let reported = Arc::new(Semaphore::new(0));
    let observed = errors.clone();
    let signal = reported.clone();
    let receipts = operations.reconcile_terminals(Arc::new(move |error| {
        observed.lock().unwrap().push(error.code);
        signal.add_permits(1);
    }));
    terminal(&journal, 1);
    store.gates[0].entered().await;
    let mut drain = Box::pin(receipts.drain());
    poll_fn(|cx| match drain.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("First receipt must own the drain before later work arrives"),
    })
    .await;
    terminal(&journal, 2);
    store.gates[1].entered().await;
    store.gates[0].release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), reported.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut drain)
            .await
            .is_err(),
        "Later accepted receipt must still own the drain"
    );
    store.gates[1].release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        *errors.lock().unwrap(),
        vec![
            if panic {
                "internal_error"
            } else {
                "fixture_receipt_failed"
            },
            "fixture_receipt_failed"
        ]
    );
    assert_eq!(store.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn receipt_drain_includes_later_work_and_reports_each_error_before_completion() {
    receipt_drain(false).await;
}
#[tokio::test]
async fn receipt_panic_is_reported_and_releases_work_without_ending_other_receipts() {
    receipt_drain(true).await;
}
