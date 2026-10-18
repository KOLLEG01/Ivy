use ivy_agent_manager::{
    inventory::{InventoryRead, InventorySignal, NativeInventory},
    inventory_manager::{InventoryConnection, InventoryManager},
    native_journal::{Limits, NativeJournal, Owner},
    notice_policy::NativeNoticePolicy,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, hash::digest, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::codex_admission::Priority;
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::InvocationContext,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};
use tokio::sync::{oneshot, watch};

fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn text(value: &IvyJson, key: &str) -> String {
    field(value, key).as_str().unwrap()
}
fn array<'a>(value: &'a IvyJson, key: &str) -> &'a [IvyJson] {
    field(value, key).as_array().unwrap()
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let result = Command::new("node")
            .arg("tests/fixtures/agent-inventory-manager-oracle.mjs")
            .current_dir(root())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        parse(
            std::str::from_utf8(&result.stdout).unwrap(),
            8 * 1024 * 1024,
        )
        .unwrap()
    })
}
fn error(value: &IvyJson) -> IvyError {
    let mut error = IvyError::new_js(
        field(value, "code").as_js_string().unwrap().clone(),
        field(value, "message").as_js_string().unwrap().clone(),
    )
    .with_outcome(match text(value, "outcome").as_str() {
        "completed" => Outcome::Completed,
        "unknown" => Outcome::Unknown,
        _ => Outcome::NotExecuted,
    });
    if let Some(details) = value.get("details") {
        error = error.with_details(details.clone());
    }
    error
}
fn context() -> InvocationContext {
    InvocationContext::detached(
        "caller".into(),
        None,
        1,
        Default::default(),
        error(field(data(), "abortReason")),
    )
}
fn failure(code: &str) -> IvyError {
    let mut value = field(data(), "abortReason").clone();
    value.insert("code", code.into());
    value.insert("outcome", "not_executed".into());
    error(&value)
}
fn outcome(value: IvyResult<IvyJson>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("result", value)]),
        Err(error) => IvyJson::object([("error", error.to_wire())]),
    }
}
fn sorted(value: IvyJson) -> IvyJson {
    match value {
        IvyJson::Object(mut pairs) => {
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            IvyJson::Object(pairs.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        IvyJson::Array(values) => IvyJson::Array(values.into_iter().map(sorted).collect()),
        other => other,
    }
}
fn same(actual: IvyJson, expected: &IvyJson, name: &str, index: usize) {
    assert_eq!(
        sorted(actual),
        sorted(expected.clone()),
        "{name} step {index}"
    );
}
fn policy_snapshot(policy: &NativeNoticePolicy) -> IvyResult<IvyJson> {
    let mut values = Vec::new();
    for id in array(data(), "checks") {
        let key = id.as_js_string().unwrap().clone();
        values.push((
            key.clone(),
            policy
                .is_internal(&key)?
                .map(IvyJson::Bool)
                .unwrap_or(IvyJson::Null),
        ));
    }
    Ok(IvyJson::object([
        ("revision", IvyJson::Number(policy.revision())),
        ("internal", IvyJson::Object(values)),
    ]))
}

#[test]
fn notice_policy_matches_original_membership_revisions_and_push_events() {
    let rows = array(data(), "policyRows");
    assert_eq!(rows.len(), 21);
    for row in rows {
        let mut owner = NativeNoticePolicy::default();
        for (index, step) in array(row, "commands").iter().enumerate() {
            let result = (|| {
                match text(step, "action").as_str() {
                    "setProjects" => owner.set_projects(array(step, "projects").to_vec()),
                    "setThread" => owner.set_thread(
                        field(step, "id").as_js_string().unwrap().clone(),
                        field(step, "value"),
                        step.get("revision").and_then(IvyJson::as_f64),
                    ),
                    "setThreads" => owner.set_threads(
                        array(step, "entries"),
                        field(step, "revision").as_f64().unwrap(),
                    )?,
                    "observe" => owner.observe(field(step, "event")),
                    "snapshot" => return policy_snapshot(&owner),
                    action => panic!("{action}"),
                }
                Ok(IvyJson::Null)
            })();
            same(
                outcome(result),
                &array(row, "expected")[index],
                &text(row, "name"),
                index,
            );
        }
    }
}

struct Barrier {
    entered: watch::Sender<bool>,
    opened: watch::Sender<bool>,
}
impl Barrier {
    fn new() -> Self {
        Self {
            entered: watch::channel(false).0,
            opened: watch::channel(false).0,
        }
    }
    async fn wait(sender: &watch::Sender<bool>) {
        let mut receiver = sender.subscribe();
        while !*receiver.borrow_and_update() {
            receiver.changed().await.unwrap();
        }
    }
}
struct Script {
    steps: Vec<IvyJson>,
    cursor: usize,
    calls: Vec<IvyJson>,
}
impl Script {
    fn new(steps: &[IvyJson]) -> Self {
        Self {
            steps: steps.to_vec(),
            cursor: 0,
            calls: vec![],
        }
    }
    fn next(&mut self, optional: bool) -> IvyJson {
        let step = self.steps.get(self.cursor).cloned();
        self.cursor += 1;
        if optional {
            step.unwrap_or_else(|| IvyJson::object::<&str>([]))
        } else {
            step.expect("Unexpected native request")
        }
    }
}
struct Fixture {
    native: [Mutex<Script>; 2],
    hive: [Mutex<Script>; 2],
    barriers: Mutex<HashMap<String, Arc<Barrier>>>,
    locations: Mutex<Vec<IvyJson>>,
    fail_sink: AtomicBool,
}
impl Fixture {
    fn barrier(&self, name: &str) -> Arc<Barrier> {
        self.barriers
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_insert_with(|| Arc::new(Barrier::new()))
            .clone()
    }
    async fn gate(&self, step: &IvyJson) {
        if let Some(name) = step.get("gate").and_then(IvyJson::as_str) {
            let barrier = self.barrier(&name);
            barrier.entered.send_replace(true);
            Barrier::wait(&barrier.opened).await;
        }
    }
    fn calls(scripts: &[Mutex<Script>; 2]) -> IvyJson {
        IvyJson::object([
            (
                "thread",
                IvyJson::Array(scripts[0].lock().unwrap().calls.clone()),
            ),
            (
                "project",
                IvyJson::Array(scripts[1].lock().unwrap().calls.clone()),
            ),
        ])
    }
}
struct Publisher {
    name: String,
    context: InvocationContext,
    fixture: Arc<Fixture>,
}
impl RpcClient for Publisher {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(method, "inventory.sync");
            assert_eq!(
                options.cancellation.is_cancelled(),
                self.context.signal.is_cancelled()
            );
            let lane = usize::from(text(&params, "kind") == "project");
            let step = {
                let mut script = self.fixture.hive[lane].lock().unwrap();
                let step = script.next(true);
                script.calls.push(IvyJson::object([
                    ("connection", self.name.clone().into()),
                    ("method", method.into()),
                    ("params", params),
                ]));
                step
            };
            self.fixture.gate(&step).await;
            if step.get("abort") == Some(&IvyJson::Bool(true)) {
                self.context.signal.cancel();
            }
            assert_eq!(
                options.cancellation.is_cancelled(),
                self.context.signal.is_cancelled()
            );
            if let Some(failure) = step.get("throw") {
                return Err(error(failure));
            }
            Ok(IvyJson::object::<&str>([]))
        })
    }
}
fn connection(
    name: &str,
    fixture: Arc<Fixture>,
    contexts: &mut HashMap<String, InvocationContext>,
) -> InventoryConnection {
    let context = contexts
        .entry(name.to_owned())
        .or_insert_with(context)
        .clone();
    InventoryConnection {
        client: Arc::new(Publisher {
            name: name.to_owned(),
            context: context.clone(),
            fixture,
        }),
        signal: InventorySignal::from_context(context),
    }
}

#[tokio::test]
async fn inventory_manager_matches_original_coalescing_generations_snapshot_retention_and_timing() {
    let rows = array(data(), "rows");
    assert_eq!(rows.len(), 24);
    let catalog = parse(
        &std::fs::read_to_string(root().join("specs/native/codex-0.154.0/catalog.json")).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for row in rows {
        let fixture = Arc::new(Fixture {
            native: [
                Mutex::new(Script::new(array(field(row, "native"), "thread"))),
                Mutex::new(Script::new(array(field(row, "native"), "project"))),
            ],
            hive: [
                Mutex::new(Script::new(
                    field(row, "hive")
                        .get("thread")
                        .and_then(IvyJson::as_array)
                        .unwrap_or(&[]),
                )),
                Mutex::new(Script::new(
                    field(row, "hive")
                        .get("project")
                        .and_then(IvyJson::as_array)
                        .unwrap_or(&[]),
                )),
            ],
            barriers: Mutex::new(HashMap::new()),
            locations: Mutex::new(vec![]),
            fail_sink: AtomicBool::new(false),
        });
        let now = Arc::new(AtomicI64::new(
            row.get("initialTime")
                .and_then(IvyJson::as_f64)
                .unwrap_or(1_788_600_000_000.0) as i64,
        ));
        let read: Arc<InventoryRead> = {
            let fixture = fixture.clone();
            Arc::new(move |method, params, options| {
                let fixture = fixture.clone();
                Box::pin(async move {
                    let lane = usize::from(method.0.starts_with(&JsString::from("project/").0));
                    let step = {
                        let mut script = fixture.native[lane].lock().unwrap();
                        let step = script.next(false);
                        assert_eq!(
                            method,
                            field(&step, "method").as_js_string().unwrap().clone()
                        );
                        assert!(matches!(options.priority, Priority::Background));
                        assert!(
                            options.request_id.is_none()
                                && options.before_send.is_none()
                                && options.before_resolve.is_none()
                        );
                        script.calls.push(IvyJson::object([
                            ("method", IvyJson::String(method)),
                            ("params", params),
                            ("detachOnTimeout", options.detach_on_timeout.into()),
                            ("budget", IvyJson::Number(options.timeout_ms as f64)),
                        ]));
                        step
                    };
                    fixture.gate(&step).await;
                    if let Some(failure) = step.get("throw") {
                        return Err(error(failure));
                    }
                    Ok(field(&step, "reply").clone())
                })
            })
        };
        let clock_now = now.clone();
        let clock_iso = now.clone();
        let inventory = Arc::new(
            NativeInventory::with_reader(
                catalog.clone(),
                "epoch".into(),
                read,
                Arc::new(move || clock_now.load(Ordering::SeqCst) as f64),
                Arc::new(move || {
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                        clock_iso.load(Ordering::SeqCst),
                    )
                    .unwrap()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into()
                }),
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            NativeJournal::new(
                Owner {
                    service_node_id: "owner".into(),
                    native_version: "fixture".into(),
                    native_executable_hash: digest("fixture").into(),
                },
                Limits {
                    max_operations: 100,
                    max_journal_bytes: 64 * 1024 * 1024,
                    max_pending_inputs: 10,
                },
            )
            .unwrap(),
        ));
        let policy = Arc::new(Mutex::new(NativeNoticePolicy::default()));
        let closing = Arc::new(AtomicBool::new(false));
        let clock_now = now.clone();
        let manager = InventoryManager::with_clock(
            inventory.clone(),
            journal,
            policy.clone(),
            {
                let fixture = fixture.clone();
                Arc::new(move |projects| {
                    fixture
                        .locations
                        .lock()
                        .unwrap()
                        .push(IvyJson::Array(projects.to_vec()));
                    if fixture.fail_sink.swap(false, Ordering::SeqCst) {
                        return Err(failure("locations_failed"));
                    }
                    Ok(())
                })
            },
            closing.clone(),
            Arc::new(move || clock_now.load(Ordering::SeqCst) as f64),
        )
        .unwrap();
        let defaults = row
            .get("defaults")
            .cloned()
            .unwrap_or_else(|| IvyJson::object::<&str>([]));
        let handler = manager.projects_handler(Arc::new(move || Ok(defaults.clone())));
        let mut current = None;
        let mut contexts = HashMap::new();
        let mut handles = HashMap::new();
        let mut ready = true;
        for (index, step) in array(row, "commands").iter().enumerate() {
            let result: IvyResult<IvyJson> = async {
                match text(step, "action").as_str() {
                    "bind" => {
                        let next = connection(&text(step, "name"), fixture.clone(), &mut contexts);
                        manager.bind(next.clone())?;
                        current = Some(next);
                    }
                    "refreshThreads" => {
                        manager
                            .refresh_threads(current.clone().unwrap())?
                            .wait()
                            .await?
                    }
                    "refreshProjects" => {
                        manager
                            .refresh_projects(
                                current.clone().unwrap(),
                                step.get("nativeRead")
                                    .and_then(IvyJson::as_bool)
                                    .unwrap_or(true),
                            )?
                            .wait()
                            .await?
                    }
                    "projects" => {
                        let context = context();
                        if step.get("cancelled") == Some(&IvyJson::Bool(true)) {
                            context.signal.cancel();
                        }
                        return handler(
                            step.get("input").cloned().unwrap_or(IvyJson::Null),
                            context,
                        )
                        .await;
                    }
                    "start" => {
                        let run = match text(step, "method").as_str() {
                            "threads" => Some(manager.refresh_threads(current.clone().unwrap())?),
                            "projects" => Some(
                                manager.refresh_projects(
                                    current.clone().unwrap(),
                                    step.get("nativeRead")
                                        .and_then(IvyJson::as_bool)
                                        .unwrap_or(true),
                                )?,
                            ),
                            "fresh" => None,
                            method => panic!("{method}"),
                        };
                        let owner = manager.clone();
                        let connection = current.clone().unwrap();
                        let (begun, started) = oneshot::channel();
                        let handle = tokio::spawn(async move {
                            let _ = begun.send(());
                            outcome(
                                match run {
                                    Some(run) => run.wait().await,
                                    None => owner.fresh_projects(connection).await,
                                }
                                .map(|()| IvyJson::Null),
                            )
                        });
                        started.await.unwrap();
                        handles.insert(text(step, "handle"), handle);
                    }
                    "wait" => {
                        return Ok(handles
                            .remove(&text(step, "handle"))
                            .unwrap()
                            .await
                            .unwrap());
                    }
                    "entered" => Barrier::wait(&fixture.barrier(&text(step, "name")).entered).await,
                    "release" => {
                        fixture
                            .barrier(&text(step, "name"))
                            .opened
                            .send_replace(true);
                    }
                    "abort" => {
                        let name = text(step, "name");
                        connection(&name, fixture.clone(), &mut contexts);
                        contexts.get(&name).unwrap().signal.cancel();
                    }
                    "closing" => {
                        closing.store(field(step, "value").as_bool().unwrap(), Ordering::SeqCst)
                    }
                    "ready" => ready = field(step, "value").as_bool().unwrap(),
                    "requestThreads" => manager.request_threads(ready)?,
                    "requestProjects" => manager.request_projects(ready)?,
                    "due" => manager.refresh_due(current.clone().unwrap())?,
                    "time" => now.store(
                        field(step, "value").as_f64().unwrap() as i64,
                        Ordering::SeqCst,
                    ),
                    "sinkFailure" => fixture.fail_sink.store(true, Ordering::SeqCst),
                    "observe" => policy.lock().unwrap().observe(field(step, "event")),
                    "drain" => manager.drain().await?,
                    "snapshot" => {
                        return Ok(IvyJson::object([
                            ("projectSnapshot", manager.project_snapshot()?),
                            ("projectObserved", manager.projects_observed()?.into()),
                            (
                                "code",
                                manager
                                    .code()?
                                    .map(IvyJson::String)
                                    .unwrap_or(IvyJson::Null),
                            ),
                            ("currentProjects", inventory.current_projects(None)?),
                            ("policy", policy_snapshot(&policy.lock().unwrap())?),
                            ("nativeCalls", Fixture::calls(&fixture.native)),
                            ("hiveCalls", Fixture::calls(&fixture.hive)),
                            (
                                "locations",
                                IvyJson::Array(fixture.locations.lock().unwrap().clone()),
                            ),
                        ]));
                    }
                    action => panic!("{action}"),
                }
                Ok(IvyJson::Null)
            }
            .await;
            same(
                outcome(result),
                &array(row, "expected")[index],
                &text(row, "name"),
                index,
            );
        }
        manager.drain().await.unwrap();
        for lane in &fixture.native {
            let script = lane.lock().unwrap();
            assert_eq!(script.cursor, script.steps.len(), "{}", text(row, "name"));
        }
    }
}
