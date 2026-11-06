use super::{catalog, journal};
use ivy_agent_manager::{
    inventory::{InventorySignal, NativeInventory},
    inventory_manager::{InventoryConnection, InventoryManager},
    notice_policy::NativeNoticePolicy,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, wire_schema::validate_shared,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::InvocationContext,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};

struct Gate {
    entered: Notify,
    release: Semaphore,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Semaphore::new(0),
        })
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.notified())
            .await
            .unwrap();
    }
    fn release(&self) {
        self.release.add_permits(1);
    }
    async fn wait(&self) {
        self.entered.notify_one();
        self.release.acquire().await.unwrap().forget();
    }
}
fn native_thread(id: &str, name: &str) -> IvyJson {
    IvyJson::object([
        ("id", id.into()),
        ("name", name.into()),
        ("preview", name.into()),
        ("cwd", "/work".into()),
        ("status", IvyJson::object([("type", "idle".into())])),
        ("source", "cli".into()),
        ("threadSource", "cli".into()),
        ("projectId", IvyJson::Null),
        ("updatedAt", IvyJson::Number(1.0)),
        ("recencyAt", IvyJson::Number(1.0)),
        ("ephemeral", false.into()),
        ("canAcceptDirectInput", true.into()),
    ])
}
struct NativeState {
    rows: BTreeMap<String, (IvyJson, bool)>,
    calls: Vec<(JsString, IvyJson)>,
    gate: Option<Arc<Gate>>,
    gate_method: &'static str,
    wrong_read: bool,
}
#[derive(Clone, Copy)]
enum Failure {
    Gap,
    Foreign,
    Unknown,
}
#[derive(Default)]
struct HiveState {
    calls: Vec<IvyJson>,
    project_calls: Vec<IvyJson>,
    entries: BTreeMap<JsString, IvyJson>,
    revision: u64,
    fail: Option<Failure>,
    gate: Option<Arc<Gate>>,
}
#[derive(Default)]
struct Hive(Mutex<HiveState>);
impl RpcClient for Hive {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        let prepared = (|| -> IvyResult<_> {
            if method != "inventory.sync" {
                return Err(IvyError::internal());
            }
            validate_shared("InventorySync", &params)?;
            let mut state = self.0.lock().unwrap();
            if params.get("kind") == Some(&"project".into()) {
                let result = IvyJson::object([
                    (
                        "snapshotRevision",
                        params.required("snapshotRevision")?.clone(),
                    ),
                    ("observedAt", params.required("observedAt")?.clone()),
                ]);
                state.project_calls.push(params);
                return Ok((Ok(result), None));
            }
            let delta = params.get("mode") == Some(&"delta".into());
            let revision = params.required("snapshotRevision")?.as_f64().unwrap() as u64;
            let failure = if delta { state.fail.take() } else { None };
            if matches!(failure, Some(Failure::Gap)) {
                state.revision += 10;
            }
            state.calls.push(params.clone());
            if matches!(failure, Some(Failure::Gap | Failure::Foreign))
                || revision <= state.revision
                || delta
                    && params.get("expectedRevision").and_then(IvyJson::as_f64)
                        != Some(state.revision as f64)
            {
                return Ok((
                    Err(
                        IvyError::new("revision_conflict", "fixture gap").with_details(
                            IvyJson::object([
                                (
                                    "serviceNodeId",
                                    if matches!(failure, Some(Failure::Foreign)) {
                                        "foreign.agent"
                                    } else {
                                        "fixture.agent"
                                    }
                                    .into(),
                                ),
                                ("namespace", params.required("namespace")?.clone()),
                                ("kind", "thread".into()),
                                ("currentRevision", IvyJson::Number(state.revision as f64)),
                            ]),
                        ),
                    ),
                    None,
                ));
            }
            if !delta {
                state.entries.clear();
            }
            if let Some(removed) = params.get("removedNativeIds").and_then(IvyJson::as_array) {
                for id in removed {
                    state.entries.remove(id.as_js_string().unwrap());
                }
            }
            for entry in params.required("entries")?.as_array().unwrap() {
                state.entries.insert(
                    entry.required("nativeId")?.as_js_string().unwrap().clone(),
                    entry.clone(),
                );
            }
            state.revision = revision;
            let result = if matches!(failure, Some(Failure::Unknown)) {
                Err(
                    IvyError::new("connection_closed", "fixture lost acknowledgement")
                        .with_outcome(Outcome::Unknown),
                )
            } else {
                Ok(IvyJson::object([
                    ("snapshotRevision", IvyJson::Number(revision as f64)),
                    ("observedAt", "2026-01-01T00:00:00.000Z".into()),
                ]))
            };
            Ok((result, state.gate.take()))
        })();
        Box::pin(async move {
            let (result, gate) = prepared?;
            if let Some(gate) = gate {
                gate.wait().await;
            }
            result
        })
    }
}
struct Fixture {
    manager: InventoryManager,
    native: Arc<Mutex<NativeState>>,
    hive: Arc<Hive>,
    policy: Arc<Mutex<NativeNoticePolicy>>,
    ticks: Arc<AtomicU64>,
}
fn connection(hive: Arc<Hive>, generation: u64) -> InventoryConnection {
    InventoryConnection {
        client: hive,
        signal: InventorySignal::from_context(InvocationContext::detached(
            "fixture".into(),
            None,
            generation,
            Default::default(),
            IvyError::new("fixture_retired", ""),
        )),
    }
}
impl Fixture {
    async fn new() -> Self {
        let ticks = Arc::new(AtomicU64::new(0));
        let native = Arc::new(Mutex::new(NativeState {
            rows: BTreeMap::from([("t".into(), (native_thread("t", "initial"), false))]),
            calls: vec![],
            gate: None,
            gate_method: "thread/read",
            wrong_read: false,
        }));
        let inventory = Arc::new(
            NativeInventory::with_reader(
                &catalog(),
                "native-epoch".into(),
                Arc::new({
                    let native = native.clone();
                    move |method, params, _| {
                        let mut state = native.lock().unwrap();
                        state.calls.push((method.clone(), params.clone()));
                        let gate = if method.eq_str(state.gate_method) {
                            state.gate.take()
                        } else {
                            None
                        };
                        let result = if method.eq_str("thread/read") {
                            let id = params.get("threadId").unwrap().as_str().unwrap();
                            if state.wrong_read {
                                Ok(IvyJson::object([(
                                    "thread",
                                    native_thread("different", "wrong"),
                                )]))
                            } else {
                                state
                                    .rows
                                    .get(&id)
                                    .map(|(row, _)| IvyJson::object([("thread", row.clone())]))
                                    .ok_or_else(|| IvyError::new("not_found", "fixture"))
                            }
                        } else {
                            let data = if method.eq_str("thread/loaded/list") {
                                if state.rows.contains_key("t") {
                                    vec!["t".into()]
                                } else {
                                    vec![]
                                }
                            } else if method.eq_str("thread/list") {
                                let archived = params.get("archived") == Some(&true.into());
                                state
                                    .rows
                                    .values()
                                    .filter(|(row, value)| {
                                        *value == archived
                                            && row.get("ephemeral") != Some(&IvyJson::Bool(true))
                                    })
                                    .map(|(row, _)| row.clone())
                                    .collect::<Vec<_>>()
                            } else {
                                vec![]
                            };
                            let start = params
                                .get("cursor")
                                .and_then(|value| value.as_str().ok())
                                .map(|s| s.parse::<usize>().unwrap())
                                .unwrap_or(0);
                            let end = (start + 100).min(data.len());
                            Ok(IvyJson::object([
                                ("data", IvyJson::Array(data[start..end].to_vec())),
                                (
                                    "nextCursor",
                                    if end < data.len() {
                                        end.to_string().into()
                                    } else {
                                        IvyJson::Null
                                    },
                                ),
                            ]))
                        };
                        Box::pin(async move {
                            if let Some(gate) = gate {
                                gate.wait().await;
                            }
                            result.map(|result| IvyJson::object([("result", result)]))
                        })
                    }
                }),
                Arc::new({
                    let ticks = ticks.clone();
                    move || ticks.load(Ordering::SeqCst) as f64
                }),
                Arc::new({
                    let ticks = ticks.clone();
                    move || instant(ticks.load(Ordering::SeqCst))
                }),
            )
            .unwrap(),
        );
        let policy = Arc::new(Mutex::new(NativeNoticePolicy::default()));
        let manager = InventoryManager::with_clock(
            inventory,
            journal(),
            policy.clone(),
            Arc::new(|_| Ok(())),
            Arc::new(AtomicBool::new(false)),
            Arc::new({
                let ticks = ticks.clone();
                move || ticks.load(Ordering::SeqCst) as f64
            }),
        )
        .unwrap();
        let hive = Arc::new(Hive::default());
        manager.bind(connection(hive.clone(), 1)).unwrap();
        manager
            .refresh_threads(connection(hive.clone(), 1))
            .unwrap()
            .wait()
            .await
            .unwrap();
        Self {
            manager,
            native,
            hive,
            policy,
            ticks,
        }
    }
    fn event(&self, method: &str, id: &str) {
        self.manager
            .observe_thread(
                true,
                &method.into(),
                Some(id.into()),
                &instant(self.ticks.load(Ordering::SeqCst)),
            )
            .unwrap();
    }
    async fn drain(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.manager.drain())
            .await
            .unwrap()
            .unwrap();
    }
    fn calls(&self, method: &str) -> usize {
        self.native
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(m, _)| m.eq_str(method))
            .count()
    }
    fn change(&self, name: &str, archived: bool) {
        self.native
            .lock()
            .unwrap()
            .rows
            .insert("t".into(), (native_thread("t", name), archived));
    }
    fn modes(&self) -> Vec<String> {
        self.hive
            .0
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|c| c.get("mode").unwrap().as_str().unwrap())
            .collect()
    }
}

fn instant(ticks: u64) -> JsString {
    chrono::DateTime::from_timestamp_millis(1_767_225_600_000 + ticks as i64)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .into()
}

#[tokio::test]
async fn inadmissible_notice_time_requires_a_complete_observation() {
    for length in [0, 65, 1024 * 1024, 24] {
        let fixture = Fixture::new().await;
        let mut units = vec![120; length];
        if length == 24 {
            units[0] = 0xd800;
        }
        let oversized = JsString(units);
        fixture
            .manager
            .observe_thread(true, &"thread/deleted".into(), Some("t".into()), &oversized)
            .unwrap();
        fixture.drain().await;
        assert_eq!(fixture.modes(), ["snapshot", "snapshot"]);
        assert_eq!(fixture.calls("thread/list"), 4);
        assert_eq!(fixture.calls("thread/read"), 0);
        let hive = fixture.hive.0.lock().unwrap();
        assert!(
            hive.entries.contains_key(&"t".into()),
            "An inadmissible timestamp authorized a deletion instead of fresh reconciliation"
        );
        assert_eq!(
            hive.calls[1]
                .get("observedAt")
                .and_then(IvyJson::as_js_string),
            Some(&instant(0))
        );
        assert!(fixture.manager.code().unwrap().is_none());
    }
}

#[tokio::test]
async fn queued_deletion_keeps_its_notice_time_after_an_older_acknowledgement() {
    let fixture = Fixture::new().await;
    let gate = Gate::new();
    fixture.hive.0.lock().unwrap().gate = Some(gate.clone());
    fixture.ticks.store(1_000, Ordering::SeqCst);
    fixture.change("in flight", false);
    fixture.event("thread/name/updated", "t");
    gate.entered().await;
    let native_calls = fixture.native.lock().unwrap().calls.len();
    fixture.ticks.store(10_000, Ordering::SeqCst);
    fixture.native.lock().unwrap().rows.remove("t");
    fixture.event("thread/deleted", "t");
    fixture.ticks.store(40_000, Ordering::SeqCst);
    gate.release();
    fixture.drain().await;
    let hive = fixture.hive.0.lock().unwrap();
    assert_eq!(hive.calls.len(), 3);
    let deletion = hive.calls.last().unwrap();
    assert_eq!(deletion.get("mode"), Some(&"delta".into()));
    assert_eq!(
        deletion.get("observedAt").and_then(IvyJson::as_js_string),
        Some(&instant(10_000)),
        "A queued deletion acquired the later refresh time"
    );
    assert_eq!(
        deletion.get("removedNativeIds"),
        Some(&IvyJson::Array(vec!["t".into()]))
    );
    assert!(
        deletion
            .required("entries")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(hive.entries.is_empty());
    assert_eq!(fixture.native.lock().unwrap().calls.len(), native_calls);
}

#[tokio::test]
async fn mixed_coalesced_changes_keep_first_notice_and_actual_entry_read_times() {
    let fixture = Fixture::new().await;
    let acknowledgement = Gate::new();
    fixture.hive.0.lock().unwrap().gate = Some(acknowledgement.clone());
    fixture.ticks.store(1_000, Ordering::SeqCst);
    fixture.event("thread/name/updated", "t");
    acknowledgement.entered().await;
    fixture.ticks.store(10_000, Ordering::SeqCst);
    fixture.event("thread/deleted", "removed");
    fixture.ticks.store(20_000, Ordering::SeqCst);
    fixture.change("latest original name", false);
    fixture.event("thread/name/updated", "t");
    fixture.ticks.store(25_000, Ordering::SeqCst);
    fixture.event("thread/deleted", "removed");
    let read = Gate::new();
    fixture.native.lock().unwrap().gate = Some(read.clone());
    fixture.ticks.store(30_000, Ordering::SeqCst);
    acknowledgement.release();
    read.entered().await;
    fixture.ticks.store(50_000, Ordering::SeqCst);
    read.release();
    fixture.drain().await;
    {
        let hive = fixture.hive.0.lock().unwrap();
        assert_eq!(hive.calls.len(), 3);
        let mixed = hive.calls.last().unwrap();
        assert_eq!(
            mixed.get("observedAt").and_then(IvyJson::as_js_string),
            Some(&instant(10_000)),
            "Coalescing replaced the original batch notice time"
        );
        assert_eq!(
            mixed.get("removedNativeIds"),
            Some(&IvyJson::Array(vec!["removed".into()]))
        );
        let entries = mixed.required("entries").unwrap().as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].get("observedAt").and_then(IvyJson::as_js_string),
            Some(&instant(30_000))
        );
        assert_eq!(
            entries[0].required("summary").unwrap().get("name"),
            Some(&"latest original name".into())
        );
    }
    assert_eq!(fixture.calls("thread/list"), 2);
    assert_eq!(fixture.calls("thread/read"), 2);
    fixture.ticks.store(55_000, Ordering::SeqCst);
    let next = Arc::new(Hive::default());
    fixture.manager.bind(connection(next.clone(), 2)).unwrap();
    fixture
        .manager
        .refresh_threads(connection(next.clone(), 2))
        .unwrap()
        .wait()
        .await
        .unwrap();
    let next = next.0.lock().unwrap();
    assert_eq!(next.calls.len(), 1);
    assert_eq!(next.calls[0].get("mode"), Some(&"snapshot".into()));
    assert_eq!(
        next.calls[0]
            .get("observedAt")
            .and_then(IvyJson::as_js_string),
        Some(&instant(55_000))
    );
}

#[tokio::test]
async fn empty_and_removal_only_publications_keep_the_original_batch_time() {
    let fixture = Fixture::new().await;
    fixture.ticks.store(10_000, Ordering::SeqCst);
    fixture.native.lock().unwrap().rows.remove("t");
    fixture.event("thread/deleted", "t");
    fixture.drain().await;
    {
        let hive = fixture.hive.0.lock().unwrap();
        let deletion = hive.calls.last().unwrap();
        assert_eq!(deletion.get("mode"), Some(&"delta".into()));
        assert_eq!(
            deletion.get("observedAt").and_then(IvyJson::as_js_string),
            Some(&instant(10_000))
        );
        assert_eq!(
            deletion.get("removedNativeIds"),
            Some(&IvyJson::Array(vec!["t".into()]))
        );
        assert!(
            deletion
                .required("entries")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    fixture.ticks.store(20_000, Ordering::SeqCst);
    let gate = Gate::new();
    {
        let mut native = fixture.native.lock().unwrap();
        native.gate = Some(gate.clone());
        native.gate_method = "thread/list";
    }
    let work = fixture
        .manager
        .refresh_threads(connection(fixture.hive.clone(), 1))
        .unwrap();
    gate.entered().await;
    fixture.ticks.store(70_000, Ordering::SeqCst);
    gate.release();
    work.wait().await.unwrap();
    let hive = fixture.hive.0.lock().unwrap();
    let empty = hive.calls.last().unwrap();
    assert_eq!(empty.get("mode"), Some(&"snapshot".into()));
    assert_eq!(
        empty.get("observedAt").and_then(IvyJson::as_js_string),
        Some(&instant(20_000))
    );
    assert!(
        empty
            .required("entries")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.calls("thread/read"), 0);
}

#[tokio::test]
async fn slow_complete_observation_keeps_its_original_time_and_cannot_postpone_refresh() {
    let fixture = Fixture::new().await;
    let gate = Gate::new();
    {
        let mut native = fixture.native.lock().unwrap();
        native.gate = Some(gate.clone());
        native.gate_method = "thread/list";
    }
    let work = fixture
        .manager
        .refresh_threads(connection(fixture.hive.clone(), 1))
        .unwrap();
    gate.entered().await;
    fixture.ticks.store(70_000, Ordering::SeqCst);
    gate.release();
    work.wait().await.unwrap();
    {
        let hive = fixture.hive.0.lock().unwrap();
        let entries = hive
            .calls
            .last()
            .unwrap()
            .required("entries")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].get("observedAt").and_then(IvyJson::as_js_string),
            Some(&instant(0)),
            "A slow complete scan relabelled old native data with its finish time"
        );
    }
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "snapshot", "delta"]);
    assert_eq!(fixture.calls("thread/list"), 5);
    fixture.ticks.store(900_000, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(
        fixture.modes(),
        ["snapshot", "snapshot", "delta", "snapshot"]
    );
    assert_eq!(fixture.calls("thread/list"), 7);
    let hive = fixture.hive.0.lock().unwrap();
    let entries = hive
        .calls
        .last()
        .unwrap()
        .required("entries")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        entries[0].get("observedAt").and_then(IvyJson::as_js_string),
        Some(&instant(900_000))
    );
}

#[tokio::test]
async fn slow_targeted_observation_keeps_the_original_read_time() {
    let fixture = Fixture::new().await;
    fixture.ticks.store(10_000, Ordering::SeqCst);
    let gate = Gate::new();
    fixture.native.lock().unwrap().gate = Some(gate.clone());
    fixture.change("original changed name", false);
    fixture.event("thread/name/updated", "t");
    gate.entered().await;
    fixture.ticks.store(50_000, Ordering::SeqCst);
    gate.release();
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "delta"]);
    let hive = fixture.hive.0.lock().unwrap();
    let entries = hive
        .calls
        .last()
        .unwrap()
        .required("entries")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        entries[0].get("observedAt").and_then(IvyJson::as_js_string),
        Some(&instant(10_000)),
        "A delayed targeted read relabelled its original observation at completion"
    );
    assert_eq!(
        entries[0].required("summary").unwrap().get("name"),
        Some(&"original changed name".into())
    );
}

#[tokio::test]
async fn rebinding_during_a_native_scan_rejects_the_old_publication_before_hive() {
    let fixture = Fixture::new().await;
    let gate = Gate::new();
    {
        let mut native = fixture.native.lock().unwrap();
        native.gate = Some(gate.clone());
        native.gate_method = "thread/list";
    }
    let old = fixture
        .manager
        .refresh_threads(connection(fixture.hive.clone(), 1))
        .unwrap();
    gate.entered().await;
    let next = Arc::new(Hive::default());
    fixture.manager.bind(connection(next.clone(), 2)).unwrap();
    let _queued = fixture
        .manager
        .refresh_threads(connection(next.clone(), 2))
        .unwrap();
    gate.release();
    assert_eq!(
        old.wait().await.unwrap_err().code,
        "native_inventory_changed"
    );
    fixture.drain().await;
    assert_eq!(
        fixture.hive.0.lock().unwrap().calls.len(),
        1,
        "A completed native scan reached Hive after its original binding was replaced"
    );
    let hive = next.0.lock().unwrap();
    assert_eq!(hive.calls.len(), 1);
    assert_eq!(hive.calls[0].get("mode"), Some(&"snapshot".into()));
    assert!(hive.calls[0].get("expectedRevision").is_none());
    assert!(fixture.manager.code().unwrap().is_none());
}

#[tokio::test]
async fn ordinary_thread_events_publish_exact_deltas_and_deletion_needs_no_native_read() {
    let fixture = Fixture::new().await;
    fixture.change("renamed", false);
    fixture.event("thread/name/updated", "t");
    fixture.drain().await;
    fixture.change("archived", true);
    fixture.event("thread/archived", "t");
    fixture.drain().await;
    let state = fixture.hive.0.lock().unwrap();
    assert_eq!(
        state.entries[&JsString::from("t")]
            .get("summary")
            .unwrap()
            .get("archived"),
        Some(&true.into())
    );
    drop(state);
    fixture.change("restored", false);
    fixture.event("thread/unarchived", "t");
    fixture.drain().await;
    let before_delete = fixture.native.lock().unwrap().calls.len();
    fixture.native.lock().unwrap().rows.remove("t");
    fixture.event("thread/deleted", "t");
    fixture.drain().await;
    assert_eq!(fixture.native.lock().unwrap().calls.len(), before_delete);
    assert_eq!(fixture.calls("thread/list"), 2);
    assert_eq!(fixture.calls("thread/read"), 3);
    assert_eq!(
        fixture.modes(),
        ["snapshot", "delta", "delta", "delta", "delta"]
    );
    let state = fixture.hive.0.lock().unwrap();
    for pair in state.calls.windows(2) {
        assert_eq!(
            pair[1].get("expectedRevision"),
            pair[0].get("snapshotRevision")
        );
    }
    let last = state.calls.last().unwrap();
    assert_eq!(
        last.get("removedNativeIds"),
        Some(&IvyJson::Array(vec!["t".into()]))
    );
    assert!(state.entries.is_empty());
    assert_eq!(
        fixture
            .policy
            .lock()
            .unwrap()
            .is_internal(&"t".into())
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn event_bursts_keep_one_follow_up_and_the_in_flight_archive_transition() {
    let fixture = Fixture::new().await;
    let gate = Gate::new();
    fixture.native.lock().unwrap().gate = Some(gate.clone());
    fixture.change("first", true);
    fixture.event("thread/archived", "t");
    gate.entered().await;
    fixture.change("latest", true);
    for _ in 0..1000 {
        fixture.event("thread/name/updated", "t");
    }
    gate.release();
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "delta", "delta"]);
    assert_eq!(fixture.calls("thread/read"), 2);
    assert_eq!(fixture.calls("thread/list"), 2);
    let state = fixture.hive.0.lock().unwrap();
    let summary = state.entries[&JsString::from("t")].get("summary").unwrap();
    assert_eq!(summary.get("name"), Some(&"latest".into()));
    assert_eq!(summary.get("archived"), Some(&true.into()));
}

#[tokio::test]
async fn pending_overflow_and_unknown_archive_state_require_complete_observation() {
    let fixture = Fixture::new().await;
    let gate = Gate::new();
    fixture.native.lock().unwrap().gate = Some(gate.clone());
    fixture.event("thread/name/updated", "t");
    gate.entered().await;
    for i in 0..66 {
        let id = format!("n{i}");
        fixture
            .native
            .lock()
            .unwrap()
            .rows
            .insert(id.clone(), (native_thread(&id, "new"), false));
    }
    for i in 0..1000 {
        fixture.event("thread/started", &format!("n{i}"));
    }
    gate.release();
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "delta", "snapshot"]);
    assert_eq!(fixture.calls("thread/read"), 1);
    assert_eq!(fixture.hive.0.lock().unwrap().entries.len(), 67);
    let reads = fixture.calls("thread/read");
    fixture.native.lock().unwrap().rows.insert(
        "unseen".into(),
        (native_thread("unseen", "historical"), true),
    );
    fixture.event("thread/name/updated", "unseen");
    fixture.drain().await;
    assert_eq!(fixture.calls("thread/read"), reads);
    assert_eq!(fixture.modes().last().unwrap(), "snapshot");
    assert_eq!(
        fixture.hive.0.lock().unwrap().entries[&JsString::from("unseen")]
            .get("summary")
            .unwrap()
            .get("archived"),
        Some(&true.into())
    );
}

#[tokio::test]
async fn frequent_deltas_cannot_postpone_the_full_freshness_observation() {
    let fixture = Fixture::new().await;
    fixture.ticks.store(899_000, Ordering::SeqCst);
    fixture.event("thread/name/updated", "t");
    fixture.drain().await;
    fixture.ticks.store(900_000, Ordering::SeqCst);
    fixture.event("thread/name/updated", "t");
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "delta", "snapshot"]);
    assert_eq!(fixture.calls("thread/list"), 4);
}

#[tokio::test]
async fn a_recent_current_check_cannot_postpone_a_due_complete_observation() {
    let fixture = Fixture::new().await;
    fixture.ticks.store(899_000, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "delta"]);
    fixture.ticks.store(899_999, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "delta"]);
    fixture.ticks.store(900_000, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "delta", "snapshot"]);
    assert_eq!(fixture.calls("thread/list"), 5);
    let context = InvocationContext::detached(
        "fixture".into(),
        None,
        1,
        Default::default(),
        IvyError::new("fixture_retired", ""),
    );
    context.signal.cancel();
    fixture.ticks.store(1_800_000, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(InventoryConnection {
            client: fixture.hive.clone(),
            signal: InventorySignal::from_context(context),
        })
        .unwrap();
    fixture.drain().await;
    assert_eq!(fixture.calls("thread/list"), 5);
    fixture.ticks.store(1_800_001, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(
        fixture.calls("thread/list"),
        5,
        "An expired complete timer bypassed failure retry pacing"
    );
    fixture.ticks.store(1_860_000, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(fixture.calls("thread/list"), 7);
}

#[tokio::test]
async fn quiet_inventory_reconciles_lost_events_without_minute_history_scans() {
    let fixture = Fixture::new().await;
    {
        let mut native = fixture.native.lock().unwrap();
        native.rows.insert(
            "history".into(),
            (native_thread("history", "missed archive"), true),
        );
    }
    for minute in 1..15 {
        fixture.ticks.store(minute * 60_000, Ordering::SeqCst);
        fixture
            .manager
            .refresh_due(connection(fixture.hive.clone(), 1))
            .unwrap();
        fixture.drain().await;
        assert_eq!(
            fixture
                .native
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|(method, args)| method.eq_str("thread/list")
                    && args.get("archived") == Some(&IvyJson::Bool(true)))
                .count(),
            1,
            "A readiness tick scanned archived history at minute {minute}"
        );
        let hive = fixture.hive.0.lock().unwrap();
        assert_eq!(hive.calls.len(), minute as usize + 1);
        assert_eq!(
            hive.entries[&JsString::from("t")].get("observedAt"),
            Some(&IvyJson::String(instant(minute * 60_000)))
        );
    }
    fixture.ticks.store(900_000, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(fixture.calls("thread/list"), 18);
    let hive = fixture.hive.0.lock().unwrap();
    assert_eq!(
        hive.project_calls.len(),
        3,
        "Project observation must retain its independent five-minute cadence"
    );
    assert_eq!(hive.entries.len(), 2);
    for (id, archived) in [("t", false), ("history", true)] {
        let entry = &hive.entries[&JsString::from(id)];
        assert_eq!(
            entry.get("observedAt"),
            Some(&IvyJson::String(instant(900_000)))
        );
        assert_eq!(
            entry.required("summary").unwrap().get("archived"),
            Some(&IvyJson::Bool(archived))
        );
    }
}

#[tokio::test]
async fn changed_current_membership_requires_a_complete_observation_before_publication() {
    let fixture = Fixture::new().await;
    {
        let mut native = fixture.native.lock().unwrap();
        native.rows.remove("t");
        native.rows.insert(
            "quiet".into(),
            (native_thread("quiet", "missed start"), false),
        );
        native.rows.insert(
            "history".into(),
            (native_thread("history", "missed archive"), true),
        );
    }
    fixture.ticks.store(60_000, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(
        fixture.modes(),
        ["snapshot", "snapshot"],
        "A partial current partition cannot remove or classify unseen IDs"
    );
    assert_eq!(fixture.calls("thread/list"), 5);
    let hive = fixture.hive.0.lock().unwrap();
    assert_eq!(hive.entries.len(), 2);
    assert!(!hive.entries.contains_key(&JsString::from("t")));
    assert_eq!(
        hive.entries[&JsString::from("history")]
            .required("summary")
            .unwrap()
            .get("archived"),
        Some(&IvyJson::Bool(true))
    );
}

#[tokio::test]
async fn empty_current_checks_preserve_archived_and_ephemeral_observations() {
    for ephemeral in [false, true] {
        let fixture = Fixture::new().await;
        {
            let mut native = fixture.native.lock().unwrap();
            let (row, archived) = native.rows.get_mut("t").unwrap();
            *archived = !ephemeral;
            row.insert("ephemeral", IvyJson::Bool(ephemeral));
        }
        fixture.event(
            if ephemeral {
                "thread/status/changed"
            } else {
                "thread/archived"
            },
            "t",
        );
        fixture.drain().await;
        let original = fixture.hive.0.lock().unwrap().entries[&JsString::from("t")].clone();
        let reads = fixture.calls("thread/read");
        fixture.ticks.store(60_000, Ordering::SeqCst);
        fixture
            .manager
            .refresh_due(connection(fixture.hive.clone(), 1))
            .unwrap();
        fixture.drain().await;
        let hive = fixture.hive.0.lock().unwrap();
        let delta = hive.calls.last().unwrap();
        assert_eq!(delta.get("mode"), Some(&"delta".into()));
        assert!(
            delta
                .required("entries")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            delta
                .required("removedNativeIds")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            delta.get("observedAt"),
            Some(&IvyJson::String(instant(60_000)))
        );
        assert_eq!(
            hive.entries[&JsString::from("t")],
            original,
            "An empty current check deleted or relabelled historical source data"
        );
        assert_eq!(fixture.calls("thread/read"), reads);
    }
}

#[tokio::test]
async fn unknown_current_publication_recovers_at_the_minute_check_without_rebasing_it() {
    let fixture = Fixture::new().await;
    fixture.hive.0.lock().unwrap().fail = Some(Failure::Unknown);
    fixture.ticks.store(60_000, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "delta"]);
    assert_eq!(
        fixture.manager.code().unwrap(),
        Some("connection_closed".into())
    );
    fixture.ticks.store(119_999, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "delta"]);
    fixture.ticks.store(120_000, Ordering::SeqCst);
    fixture
        .manager
        .refresh_due(connection(fixture.hive.clone(), 1))
        .unwrap();
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "delta", "snapshot"]);
    assert_eq!(fixture.calls("thread/list"), 5);
    assert!(fixture.manager.code().unwrap().is_none());
}

#[tokio::test]
async fn verified_delta_gap_reobserves_a_snapshot_but_foreign_or_unknown_results_never_rebase() {
    for failure in [Failure::Gap, Failure::Foreign, Failure::Unknown] {
        let fixture = Fixture::new().await;
        fixture.hive.0.lock().unwrap().fail = Some(failure);
        fixture.change("changed", false);
        fixture.event("thread/name/updated", "t");
        fixture.drain().await;
        match failure {
            Failure::Gap => {
                assert_eq!(fixture.modes(), ["snapshot", "delta", "snapshot"]);
                assert!(fixture.manager.code().unwrap().is_none());
                assert_eq!(fixture.calls("thread/list"), 4);
            }
            Failure::Foreign | Failure::Unknown => {
                assert_eq!(fixture.modes(), ["snapshot", "delta"]);
                assert_eq!(fixture.calls("thread/list"), 2);
                assert_eq!(
                    fixture.manager.code().unwrap(),
                    Some(
                        if matches!(failure, Failure::Foreign) {
                            "target_conflict"
                        } else {
                            "connection_closed"
                        }
                        .into()
                    )
                );
                // A separate later event starts with a fresh snapshot, not a replay.
                fixture.event("thread/status/changed", "t");
                fixture.drain().await;
                assert_eq!(fixture.modes(), ["snapshot", "delta", "snapshot"]);
            }
        }
    }
}

#[tokio::test]
async fn delayed_hive_acknowledgement_cannot_renew_an_old_native_observation() {
    let fixture = Fixture::new().await;
    let gate = Gate::new();
    fixture.hive.0.lock().unwrap().gate = Some(gate.clone());
    let work = fixture
        .manager
        .refresh_threads(connection(fixture.hive.clone(), 1))
        .unwrap();
    gate.entered().await;
    fixture.ticks.store(901_000, Ordering::SeqCst);
    gate.release();
    work.wait().await.unwrap();
    fixture.event("thread/name/updated", "t");
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "snapshot", "snapshot"]);
    assert_eq!(fixture.calls("thread/list"), 6);
}

#[tokio::test]
async fn old_connection_acknowledgement_cannot_become_a_new_bindings_delta_base() {
    let fixture = Fixture::new().await;
    let gate = Gate::new();
    fixture.hive.0.lock().unwrap().gate = Some(gate.clone());
    fixture.event("thread/name/updated", "t");
    gate.entered().await;
    let next = Arc::new(Hive::default());
    fixture.manager.bind(connection(next.clone(), 2)).unwrap();
    fixture.event("thread/name/updated", "t");
    gate.release();
    fixture.drain().await;
    let state = next.0.lock().unwrap();
    assert_eq!(state.calls.len(), 1);
    assert_eq!(state.calls[0].get("mode"), Some(&"snapshot".into()));
    assert!(state.calls[0].get("expectedRevision").is_none());
    assert!(fixture.manager.code().unwrap().is_none());
}

#[tokio::test]
async fn wrong_native_read_identity_requires_reconciliation_without_publishing_its_body() {
    let fixture = Fixture::new().await;
    fixture.native.lock().unwrap().wrong_read = true;
    fixture.event("thread/name/updated", "t");
    fixture.drain().await;
    assert_eq!(fixture.modes(), ["snapshot", "snapshot"]);
    assert!(
        fixture
            .hive
            .0
            .lock()
            .unwrap()
            .entries
            .contains_key(&JsString::from("t"))
    );
    assert!(
        !fixture
            .hive
            .0
            .lock()
            .unwrap()
            .entries
            .contains_key(&JsString::from("different"))
    );
}
