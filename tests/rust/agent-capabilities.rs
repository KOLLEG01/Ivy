use super::{Fixture, value};
use ivy_agent_manager::{
    capabilities::{
        CapabilityManagement, CapabilityPublisher, CapabilityStore, ProfileFuture,
        SqliteCapabilityStore,
    },
    local_state::{Pin, SavedState},
    native_journal::JournalClock,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::service::{InvocationContext, ToolHandler};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::Semaphore;

fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let result = Command::new("node")
            .arg("tests/fixtures/agent-capabilities-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        parse(
            std::str::from_utf8(&result.stdout).unwrap(),
            4 * 1024 * 1024,
        )
        .unwrap()
    })
}
fn get<'a>(v: &'a IvyJson, key: &str) -> &'a IvyJson {
    v.get(key).unwrap()
}
fn text(v: &IvyJson, key: &str) -> String {
    get(v, key).as_str().unwrap()
}
fn number(v: usize) -> IvyJson {
    IvyJson::Number(v as f64)
}
fn outcome(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("result", value)]),
        Err(error) => IvyJson::object([("error", error.to_wire())]),
    }
}
fn same(actual: &IvyJson, expected: &IvyJson, label: &str) {
    // Attempts can contain a number which the storage boundary rejects; compare
    // JSON-domain values without re-admitting that rejected write as storage.
    fn ordered(value: IvyJson) -> IvyJson {
        if value.as_object().is_some() {
            let mut pairs = value.into_object().unwrap();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            IvyJson::Object(
                pairs
                    .into_iter()
                    .map(|(key, value)| (key, ordered(value)))
                    .collect(),
            )
        } else if value.as_array().is_some() {
            IvyJson::Array(
                value
                    .into_array()
                    .unwrap()
                    .into_iter()
                    .map(ordered)
                    .collect(),
            )
        } else {
            value
        }
    }
    assert_eq!(
        ordered(actual.clone()),
        ordered(expected.clone()),
        "{label}"
    );
}
fn saved_json(saved: &SavedState) -> IvyJson {
    IvyJson::object([
        ("value", saved.value.clone()),
        (
            "pin",
            IvyJson::object([
                ("objectId", saved.pin.object_id.clone().into()),
                ("revision", IvyJson::Number(saved.pin.revision as f64)),
            ]),
        ),
    ])
}
fn profile_id() -> String {
    hash_json(&IvyJson::Array(vec![
        "agent".into(),
        "capabilities".into(),
        "profile".into(),
    ]))
    .unwrap()[7..]
        .into()
}

struct Gate {
    entered: Semaphore,
    opened: Semaphore,
}
impl Gate {
    fn new() -> Self {
        Self {
            entered: Semaphore::new(0),
            opened: Semaphore::new(0),
        }
    }
}
#[derive(Default)]
struct State {
    saved: Option<SavedState>,
    reads: usize,
    writes: Vec<IvyJson>,
    notifications: Vec<IvyJson>,
    fail_read: bool,
    failure: Option<bool>,
    fail_notification: bool,
    ready: bool,
    gate: Option<Arc<Gate>>,
}
#[derive(Default)]
struct Store(Mutex<State>);
impl Store {
    fn snapshot(&self, owner: &Option<CapabilityManagement>) -> IvyJson {
        let state = self.0.lock().unwrap();
        IvyJson::object([
            (
                "saved",
                state
                    .saved
                    .as_ref()
                    .map(saved_json)
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "cached",
                owner
                    .as_ref()
                    .map(|owner| owner.profile().unwrap())
                    .unwrap_or(IvyJson::Null),
            ),
            ("reads", number(state.reads)),
            ("writes", IvyJson::Array(state.writes.clone())),
            ("notifications", IvyJson::Array(state.notifications.clone())),
        ])
    }
    fn publisher(self: &Arc<Self>) -> Arc<CapabilityPublisher> {
        let store = self.clone();
        Arc::new(move |value| {
            let mut state = store.0.lock().unwrap();
            if state.ready {
                state.notifications.push(IvyJson::object([
                    ("namespace", "agent".into()),
                    ("name", "capabilities".into()),
                    ("version", "1.0.0".into()),
                    ("value", value.clone()),
                ]));
                if std::mem::take(&mut state.fail_notification) {
                    return Err(IvyError::new(
                        "service_unavailable",
                        "Fixture notification unavailable.",
                    ));
                }
            }
            Ok(())
        })
    }
}
impl CapabilityStore for Store {
    fn read(&self) -> ProfileFuture<'_, Option<SavedState>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            state.reads += 1;
            if std::mem::take(&mut state.fail_read) {
                return Err(IvyError::new(
                    "service_unavailable",
                    "Fixture read unavailable.",
                ));
            }
            Ok(state.saved.clone())
        })
    }
    fn write(&self, value: IvyJson, previous: Option<Pin>) -> ProfileFuture<'_, SavedState> {
        Box::pin(async move {
            let (failure, gate) = {
                let mut state = self.0.lock().unwrap();
                let previous = previous
                    .as_ref()
                    .map(|p| {
                        IvyJson::object([
                            ("objectId", p.object_id.clone().into()),
                            ("revision", IvyJson::Number(p.revision as f64)),
                        ])
                    })
                    .unwrap_or(IvyJson::Null);
                state.writes.push(IvyJson::object([
                    ("value", value.clone()),
                    ("previous", previous),
                ]));
                (state.failure.take(), state.gate.take())
            };
            canonical(&value, MANAGEMENT_FRAME_BYTES)?;
            if let Some(gate) = gate {
                gate.entered.add_permits(1);
                gate.opened.acquire().await.unwrap().forget();
            }
            if failure == Some(false) {
                return Err(IvyError::new(
                    "service_unavailable",
                    "Fixture write unavailable.",
                ));
            }
            let mut state = self.0.lock().unwrap();
            let matches = match &previous {
                Some(pin) => {
                    pin.object_id == profile_id()
                        && state
                            .saved
                            .as_ref()
                            .is_some_and(|saved| saved.pin.revision == pin.revision)
                }
                None => state.saved.is_none(),
            };
            if !matches {
                return Err(IvyError::new(
                    "revision_conflict",
                    "Agent workflow row changed.",
                ));
            }
            let saved = SavedState {
                value,
                pin: Pin {
                    object_id: profile_id(),
                    revision: state.saved.as_ref().map_or(1, |row| row.pin.revision + 1),
                },
            };
            state.saved = Some(saved.clone());
            if failure == Some(true) {
                return Err(IvyError::new(
                    "service_unavailable",
                    "Fixture write unavailable.",
                ));
            }
            Ok(saved)
        })
    }
}
fn context(step: &IvyJson) -> InvocationContext {
    let context = InvocationContext::detached(
        step.get("context")
            .and_then(|v| v.get("callerPrincipalId"))
            .and_then(IvyJson::as_js_string)
            .cloned()
            .unwrap_or("caller".into()),
        step.get("context")
            .and_then(|v| v.get("operationId"))
            .and_then(IvyJson::as_js_string)
            .cloned(),
        1,
        Default::default(),
        IvyError::new("fixture_cancelled", "Cancelled."),
    );
    if step.get("cancelled") == Some(&IvyJson::Bool(true)) {
        context.signal.cancel();
    }
    context
}
fn invoke(
    owner: &CapabilityManagement,
    method: &str,
    step: &IvyJson,
) -> ivy_sdk::service::ServiceFuture<IvyJson> {
    let handler: ToolHandler = owner
        .handlers()
        .remove(&JsString::from(format!("agent.{method}")))
        .unwrap();
    handler(
        step.get("input").cloned().unwrap_or(IvyJson::Null),
        context(step),
    )
}
async fn step(
    row: &IvyJson,
    step: &IvyJson,
    owner: &mut Option<CapabilityManagement>,
    store: &Arc<Store>,
    clock: &Arc<Mutex<JsString>>,
    gates: &mut HashMap<String, Arc<Gate>>,
    handles: &mut HashMap<String, tokio::task::JoinHandle<IvyJson>>,
    last: &mut IvyJson,
) -> IvyResult<IvyJson> {
    match text(step, "action").as_str() {
        "open" => {
            let clock = clock.clone();
            let clock: Arc<JournalClock> = Arc::new(move || clock.lock().unwrap().clone());
            let new = CapabilityManagement::with_clock(
                store.clone(),
                "agent".into(),
                "host".into(),
                row.get("capabilities").cloned(),
                row.get("defaults")
                    .cloned()
                    .unwrap_or(IvyJson::object::<&str>([])),
                store.publisher(),
                clock,
            )
            .await?;
            let result = new.profile()?;
            *owner = Some(new);
            Ok(result)
        }
        "profile" => invoke(owner.as_ref().unwrap(), "capabilities", step).await,
        "defaults" => invoke(owner.as_ref().unwrap(), "environmentDefaults", step).await,
        "mutateResult" => {
            if let Some(values) = last.remove("capabilities") {
                let mut values = values.into_array().unwrap();
                values[0].insert("label", "changed".into());
                last.insert("capabilities", IvyJson::Array(values));
            } else {
                let mut values = last.remove("nested").unwrap().into_array().unwrap();
                values[0] = "changed".into();
                last.insert("nested", IvyJson::Array(values));
            }
            Ok(IvyJson::Null)
        }
        "configure" => invoke(owner.as_ref().unwrap(), "configureCapabilities", step).await,
        "start" => {
            let future = invoke(owner.as_ref().unwrap(), "configureCapabilities", step);
            handles.insert(
                text(step, "handle"),
                tokio::spawn(async move { outcome(future.await) }),
            );
            Ok(IvyJson::Null)
        }
        "wait" => Ok(handles
            .remove(&text(step, "handle"))
            .unwrap()
            .await
            .unwrap()),
        "pending" => Ok(IvyJson::Bool(
            !handles.get(&text(step, "handle")).unwrap().is_finished(),
        )),
        "gate" => {
            let gate = Arc::new(Gate::new());
            gates.insert(text(step, "name"), gate.clone());
            store.0.lock().unwrap().gate = Some(gate);
            Ok(IvyJson::Null)
        }
        "entered" => {
            gates
                .get(&text(step, "name"))
                .unwrap()
                .entered
                .acquire()
                .await
                .unwrap()
                .forget();
            Ok(IvyJson::Null)
        }
        "release" => {
            gates
                .get(&text(step, "name"))
                .unwrap()
                .opened
                .add_permits(1);
            Ok(IvyJson::Null)
        }
        "readFailure" => {
            store.0.lock().unwrap().fail_read = true;
            Ok(IvyJson::Null)
        }
        "writeFailure" => {
            store.0.lock().unwrap().failure =
                Some(step.get("afterCommit") == Some(&IvyJson::Bool(true)));
            Ok(IvyJson::Null)
        }
        "notificationFailure" => {
            store.0.lock().unwrap().fail_notification = true;
            Ok(IvyJson::Null)
        }
        "ready" => {
            store.0.lock().unwrap().ready = step.get("value") == Some(&IvyJson::Bool(true));
            Ok(IvyJson::Null)
        }
        "now" => {
            *clock.lock().unwrap() = "2026-10-18T01:02:03.004Z".into();
            Ok(IvyJson::Null)
        }
        "externalWrite" => {
            let pin = store.0.lock().unwrap().saved.as_ref().unwrap().pin.clone();
            store.write(get(step, "value").clone(), Some(pin)).await?;
            Ok(IvyJson::Null)
        }
        "snapshot" => Ok(store.snapshot(owner)),
        action => panic!("{action}"),
    }
}

#[tokio::test]
async fn capability_startup_handlers_cas_replay_and_failures_match_original() {
    let rows = get(data(), "cases").as_array().unwrap();
    assert!(rows.len() >= 45);
    for row in rows {
        let store = Arc::new(Store::default());
        store.0.lock().unwrap().saved = row.get("saved").map(|v| SavedState {
            value: v.clone(),
            pin: Pin {
                object_id: profile_id(),
                revision: 1,
            },
        });
        let mut owner = None;
        let mut gates = HashMap::new();
        let mut handles = HashMap::new();
        let mut last = IvyJson::Null;
        let clock = Arc::new(Mutex::new(JsString::from("2026-10-18T00:00:00.000Z")));
        for (i, command) in get(row, "commands").as_array().unwrap().iter().enumerate() {
            let label = format!(
                "{} step {i}: {}",
                text(row, "name"),
                text(command, "action")
            );
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                step(
                    row,
                    command,
                    &mut owner,
                    &store,
                    &clock,
                    &mut gates,
                    &mut handles,
                    &mut last,
                ),
            )
            .await
            .expect(&label);
            if ["open", "profile", "defaults", "configure"]
                .contains(&text(command, "action").as_str())
                && let Ok(value) = &result
            {
                last = value.clone();
            }
            same(
                &outcome(result),
                &get(row, "expected").as_array().unwrap()[i],
                &label,
            );
        }
        assert!(handles.is_empty());
    }
}

#[tokio::test]
async fn capability_sqlite_cas_commit_notification_and_restart_keep_retained_profile() {
    let fixture = Fixture::new();
    let state = Arc::new(Mutex::new(fixture.open(10, 16 * 1024 * 1024)));
    let other_state = Arc::new(Mutex::new(fixture.open(10, 16 * 1024 * 1024)));
    let notices = Arc::new(Mutex::new(Vec::<IvyJson>::new()));
    let publisher: Arc<CapabilityPublisher> = {
        let state = state.clone();
        let notices = notices.clone();
        Arc::new(move |profile| {
            // Publication follows the commit, outside the database lock.
            same(
                &state
                    .lock()
                    .unwrap()
                    .read("capabilities", "profile")?
                    .unwrap()
                    .value,
                profile,
                "committed before notification",
            );
            notices.lock().unwrap().push(profile.clone());
            Ok(())
        })
    };
    let clock: Arc<JournalClock> = Arc::new(|| "2026-10-18T00:00:00.000Z".into());
    let first = CapabilityManagement::with_clock(
        Arc::new(SqliteCapabilityStore(state.clone())),
        "agent".into(),
        "host".into(),
        None,
        IvyJson::Null,
        publisher.clone(),
        clock.clone(),
    )
    .await
    .unwrap();
    let second = CapabilityManagement::with_clock(
        Arc::new(SqliteCapabilityStore(other_state.clone())),
        "agent".into(),
        "host".into(),
        None,
        IvyJson::Null,
        publisher,
        clock,
    )
    .await
    .unwrap();
    let command = value(
        r#"{"input":{"operationId":"one","expectedRevision":1,"capabilities":[{"key":"work","label":" \uFEFFWork\t"}]},"context":{"operationId":"one"}}"#,
    );
    let profile = invoke(&first, "configureCapabilities", &command)
        .await
        .unwrap();
    assert_eq!(notices.lock().unwrap().len(), 1);
    let error = invoke(&second, "configureCapabilities", &command)
        .await
        .unwrap_err();
    assert_eq!(error.code, "revision_conflict");
    same(
        &invoke(&first, "configureCapabilities", &command)
            .await
            .unwrap(),
        &profile,
        "original operation replay",
    );
    assert_eq!(notices.lock().unwrap().len(), 1);
    drop(first);
    drop(second);
    Arc::try_unwrap(state)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap()
        .close()
        .unwrap();
    Arc::try_unwrap(other_state)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap()
        .close()
        .unwrap();

    let result = Command::new("node")
        .arg("tests/fixtures/agent-capabilities-oracle.mjs")
        .arg("--read-db")
        .arg(fixture.path())
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let original = parse(
        std::str::from_utf8(&result.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    same(
        get(&original, "value"),
        &profile,
        "original Node reads Rust-saved profile",
    );
    let state = Arc::new(Mutex::new(fixture.open(10, 16 * 1024 * 1024)));
    let restarted = CapabilityManagement::open(
        Arc::new(SqliteCapabilityStore(state.clone())),
        "agent".into(),
        "new-host".into(),
        Some(value(r#"[{"key":"other","label":"New defaults"}]"#)),
        IvyJson::Null,
        Arc::new(|_| panic!("reopen must not publish")),
    )
    .await
    .unwrap();
    same(
        &restarted.profile().unwrap(),
        &profile,
        "restart keeps retained values instead of new defaults",
    );
    same(
        &invoke(&restarted, "configureCapabilities", &command)
            .await
            .unwrap(),
        &profile,
        "replay after explicit reopen",
    );
    drop(restarted);
    Arc::try_unwrap(state)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap()
        .close()
        .unwrap();
}
