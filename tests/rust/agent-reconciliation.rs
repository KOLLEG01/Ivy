use super::{Root, compare, field, oracle, text, value};
use ivy_agent_manager::{
    agent_health::{AgentHealth, health_message},
    inventory::{InventoryRead, InventorySignal, NativeInventory},
    inventory_manager::{InventoryConnection, InventoryManager},
    local_state::{Limits as StateLimits, LocalAgentState},
    native_journal::{Limits, NativeJournal, Owner},
    notice_policy::NativeNoticePolicy,
    reconciliation::{
        AgentReconciliation, EnvironmentRefresh, EnvironmentSynchronizers, ReconciliationOwner,
        ReconciliationSettings, readiness_snapshot,
    },
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, hash::digest, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::{
    codex_rpc::{Events, NativeRpc},
    config::InstanceConfig,
    health::HealthFile,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::InvocationContext,
};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicI64, Ordering},
};
use tokio::sync::watch;

pub(super) fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| oracle("agent-reconciliation-oracle.mjs", None))
}
fn error(value: &IvyJson) -> IvyError {
    IvyError::new_js(text(value, "code"), text(value, "message"))
        .with_outcome(match text(value, "outcome").to_utf8().unwrap().as_str() {
            "unknown" => Outcome::Unknown,
            "completed" => Outcome::Completed,
            _ => Outcome::NotExecuted,
        })
        .with_details(field(value, "details").clone())
}
fn outcome(result: IvyResult<()>) -> IvyJson {
    match result {
        Ok(()) => IvyJson::object([("result", IvyJson::Null)]),
        Err(error) => IvyJson::object([("error", error.to_wire())]),
    }
}
pub(super) fn journal(maximum: usize, maximum_bytes: usize) -> Arc<Mutex<NativeJournal>> {
    Arc::new(Mutex::new(
        NativeJournal::new(
            Owner {
                service_node_id: "owner".into(),
                native_version: "fixture".into(),
                native_executable_hash: digest("fixture").into(),
            },
            Limits {
                max_operations: maximum,
                max_journal_bytes: maximum_bytes,
                max_pending_inputs: 10,
            },
        )
        .unwrap(),
    ))
}
pub(super) fn health(root: &Root, journal: Arc<Mutex<NativeJournal>>) -> AgentHealth {
    let config = InstanceConfig::from_json(IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", "host".into()),
        ("instanceId", "agent".into()),
        ("serviceNodeId", "owner".into()),
        ("componentId", "agent-manager".into()),
        ("publicBaseUrl", "https://example.invalid/ivy".into()),
        ("dataRoot", value(&root.path)),
        ("artifactRoot", value(&root.path.join("artifact"))),
        ("buildId", digest("fixture").into()),
        ("version", "1.0.0".into()),
        ("credential", "fixture".into()),
        ("settings", IvyJson::object::<&str>([])),
    ]))
    .unwrap();
    AgentHealth::new(
        Arc::new(HealthFile::with_launch_id(config, Some("fixture".into()))),
        journal,
    )
}
struct Client {
    row: IvyJson,
    calls: Mutex<Vec<IvyJson>>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params),
            ]));
            if method == "system.status" {
                if let Some(failure) = self.row.get("statusFailure") {
                    return Err(error(failure));
                }
                return Ok(IvyJson::object([(
                    "runtimeEpoch",
                    field(&self.row, "runtimeEpoch").clone(),
                )]));
            }
            assert_eq!(method, "tools.call");
            let probe = field(&self.row, "probe");
            if probe.get("code").is_some() {
                Err(error(probe))
            } else {
                Ok(probe.clone())
            }
        })
    }
}
fn signal() -> InventorySignal {
    InventorySignal::from_context(InvocationContext::detached(
        "caller".into(),
        None,
        7,
        Default::default(),
        IvyError::internal(),
    ))
}
fn environment(
    names: Arc<Mutex<Vec<IvyJson>>>,
    gate: Option<watch::Receiver<bool>>,
    complete: Option<watch::Sender<usize>>,
) -> EnvironmentSynchronizers {
    let make = |name: &'static str| -> Arc<EnvironmentRefresh> {
        let names = names.clone();
        let gate = gate.clone();
        let complete = complete.clone();
        Arc::new(move |_| {
            names.lock().unwrap().push(name.into());
            let mut gate = gate.clone();
            let complete = complete.clone();
            Box::pin(async move {
                if let Some(gate) = gate.as_mut() {
                    while !*gate.borrow_and_update() {
                        if gate.changed().await.is_err() {
                            break;
                        }
                    }
                }
                if let Some(complete) = complete {
                    complete.send_modify(|count| *count += 1);
                }
            })
        })
    };
    EnvironmentSynchronizers {
        instructions: make("instructions"),
        mcp: make("mcp"),
        skills: make("skills"),
    }
}
pub(super) fn manager(
    journal: Arc<Mutex<NativeJournal>>,
    closing: Arc<AtomicBool>,
    locations: Arc<Mutex<Vec<IvyJson>>>,
    now: Arc<AtomicI64>,
) -> InventoryManager {
    let read: Arc<InventoryRead> = Arc::new(|_, _, _| {
        Box::pin(async {
            Err(IvyError::new(
                "fixture_inventory_failure",
                "Isolated inventory failure.",
            ))
        })
    });
    let inventory = Arc::new(
        NativeInventory::with_reader(
            field(data(), "catalog").clone(),
            "epoch".into(),
            read,
            {
                let now = now.clone();
                Arc::new(move || now.load(Ordering::SeqCst) as f64)
            },
            Arc::new(|| text(data(), "time")),
        )
        .unwrap(),
    );
    InventoryManager::with_clock(
        inventory,
        journal,
        Arc::new(Mutex::new(NativeNoticePolicy::default())),
        Arc::new(move |projects| {
            locations
                .lock()
                .unwrap()
                .push(IvyJson::Array(projects.to_vec()));
            Ok(())
        }),
        closing,
        Arc::new(move || now.load(Ordering::SeqCst) as f64),
    )
    .unwrap()
}
#[test]
fn readiness_and_health_warning_rows_match_original_including_empty_and_utf16_codes() {
    let rows = field(data(), "readiness").as_array().unwrap();
    assert_eq!(rows.len(), 20);
    for row in rows {
        let ready = field(row, "connected") == &IvyJson::Bool(true)
            && field(row, "closing") != &IvyJson::Bool(true);
        let code = row
            .get("threadCode")
            .filter(|value| !value.is_null())
            .or_else(|| row.get("projectCode").filter(|value| !value.is_null()))
            .map(|value| value.as_js_string().unwrap().clone());
        compare(
            &readiness_snapshot(&"owner".into(), ready, code, text(data(), "time")),
            field(field(row, "expected"), "result"),
            "Actual original readiness",
        );
    }
    let rows = field(data(), "warnings").as_array().unwrap();
    assert_eq!(rows.len(), 6);
    for row in rows {
        let mut message = JsString::from("Original health");
        message.0.push(0xd800);
        let message = health_message(message, row).unwrap();
        assert_eq!(
            message,
            field(row, "expected").as_array().unwrap()[1]
                .as_js_string()
                .unwrap()
                .clone()
        );
    }
}
#[tokio::test]
async fn generation_reconciliation_matches_original_status_bind_probe_and_environment_order() {
    let rows = field(data(), "rows").as_array().unwrap();
    assert_eq!(rows.len(), 17);
    for row in rows {
        let root = Root::new();
        let journal = journal(100, 64 * 1024 * 1024);
        journal
            .lock()
            .unwrap()
            .begin_epoch(&text(row, "epoch"))
            .unwrap();
        let closing = Arc::new(AtomicBool::new(
            field(row, "closing") == &IvyJson::Bool(true),
        ));
        let now = Arc::new(AtomicI64::new(field(data(), "now").as_f64().unwrap() as i64));
        let locations = Arc::new(Mutex::new(Vec::new()));
        let names = Arc::new(Mutex::new(Vec::new()));
        let inventory = manager(
            journal.clone(),
            closing.clone(),
            locations.clone(),
            now.clone(),
        );
        let (input, _output_peer) = tokio::io::duplex(1024);
        let (_input_peer, output) = tokio::io::duplex(1024);
        let rpc =
            NativeRpc::start(field(data(), "catalog"), input, output, Events::default()).unwrap();
        if field(row, "connected") == &IvyJson::Bool(false) {
            rpc.close("fixture_disconnected").await;
        }
        let state = Arc::new(Mutex::new(
            LocalAgentState::open(
                &root.path.join("state.sqlite"),
                "owner".into(),
                StateLimits {
                    max_operations: 100,
                    max_journal_bytes: 64 * 1024 * 1024,
                },
            )
            .unwrap(),
        ));
        let client = Arc::new(Client {
            row: row.clone(),
            calls: Mutex::new(Vec::new()),
        });
        let connection = InventoryConnection {
            client: client.clone(),
            signal: signal(),
        };
        let mut registry = field(data(), "registry").clone();
        if row.get("missingProbe") == Some(&IvyJson::Bool(true)) {
            registry = IvyJson::object([(
                "namespaces",
                IvyJson::Array(vec![IvyJson::object([("tools", IvyJson::Array(vec![]))])]),
            )]);
        }
        let settings = ReconciliationSettings {
            service_node_id: "owner".into(),
            registry,
            owner: ReconciliationOwner {
                rpc: rpc.clone(),
                epoch: text(row, "ownerEpoch"),
                closing,
            },
            journal: journal.clone(),
            state,
            inventory: inventory.clone(),
            environment: if row.get("environment") == Some(&IvyJson::Bool(true)) {
                Some(environment(names.clone(), None, None))
            } else {
                None
            },
            environment_at: 0.0,
            health: health(&root, journal),
        };
        let (result, bound) = match AgentReconciliation::with_clock(
            settings,
            {
                let now = now.clone();
                Arc::new(move || now.load(Ordering::SeqCst) as f64)
            },
            Arc::new(|| text(data(), "time")),
        ) {
            Ok(owner) => {
                let result = owner.reconcile(connection).await;
                let bound = owner.connection().unwrap().is_some();
                (result, bound)
            }
            Err(error) => (Err(error), false),
        };
        inventory.drain().await.unwrap();
        let mut expected = field(row, "expected").clone();
        expected.remove("runtimeEpoch");
        expected.remove("environmentAt");
        expected.remove("refreshes");
        compare(
            &IvyJson::object([
                ("outcome", outcome(result)),
                (
                    "calls",
                    IvyJson::Array(client.calls.lock().unwrap().clone()),
                ),
                (
                    "locations",
                    IvyJson::Array(locations.lock().unwrap().clone()),
                ),
                ("environment", IvyJson::Array(names.lock().unwrap().clone())),
                ("bound", bound.into()),
            ]),
            &expected,
            &String::from_utf16_lossy(&text(row, "name").0),
        );
        rpc.close("fixture_complete").await;
    }
}
#[tokio::test]
async fn health_writes_are_owned_ordered_and_recover_after_a_previous_failure() {
    let root = Root::new();
    let owner = health(&root, journal(0, 100));
    let path = root.path.join("health.json");
    std::fs::create_dir(&path).unwrap();
    let first = owner.write(false, None, "first".into()).unwrap();
    assert!(first.wait().await.is_err());
    std::fs::remove_dir(&path).unwrap();
    drop(owner.write(false, Some(1), "second".into()).unwrap());
    drop(owner.write(true, Some(2), "last".into()).unwrap());
    owner.drain_snapshot().await.unwrap();
    let stored = parse(
        std::str::from_utf8(&std::fs::read(path).unwrap()).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    assert_eq!(stored.get("ready"), Some(&IvyJson::Bool(true)));
    assert_eq!(stored.get("generation"), Some(&IvyJson::Number(2.0)));
    assert_eq!(
        stored.get("details"),
        Some(&"last Connection memory near capacity.".into())
    );
}
#[tokio::test]
async fn readiness_does_not_wait_for_environment_and_keeps_the_ten_second_start_boundary() {
    let root = Root::new();
    let journal = journal(100, 64 * 1024 * 1024);
    journal
        .lock()
        .unwrap()
        .begin_epoch(&"stale".into())
        .unwrap();
    let closing = Arc::new(AtomicBool::new(false));
    let now = Arc::new(AtomicI64::new(42000));
    let names = Arc::new(Mutex::new(Vec::new()));
    let inventory = manager(
        journal.clone(),
        closing.clone(),
        Arc::new(Mutex::new(Vec::new())),
        now.clone(),
    );
    let (input, _output_peer) = tokio::io::duplex(1024);
    let (_input_peer, output) = tokio::io::duplex(1024);
    let rpc = NativeRpc::start(field(data(), "catalog"), input, output, Events::default()).unwrap();
    let state = Arc::new(Mutex::new(
        LocalAgentState::open(
            &root.path.join("state.sqlite"),
            "owner".into(),
            StateLimits {
                max_operations: 100,
                max_journal_bytes: 64 * 1024 * 1024,
            },
        )
        .unwrap(),
    ));
    let (gate, receiver) = watch::channel(false);
    let (complete, mut finished) = watch::channel(0);
    let owner = AgentReconciliation::with_clock(
        ReconciliationSettings {
            service_node_id: "owner".into(),
            registry: field(data(), "registry").clone(),
            owner: ReconciliationOwner {
                rpc: rpc.clone(),
                epoch: "current".into(),
                closing,
            },
            journal: journal.clone(),
            state,
            inventory,
            environment: Some(environment(names.clone(), Some(receiver), Some(complete))),
            environment_at: 32001.0,
            health: health(&root, journal),
        },
        {
            let now = now.clone();
            Arc::new(move || now.load(Ordering::SeqCst) as f64)
        },
        Arc::new(|| text(data(), "time")),
    )
    .unwrap();
    let client = Arc::new(Client {
        row: field(data(), "rows").as_array().unwrap()[0].clone(),
        calls: Mutex::new(Vec::new()),
    });
    let connection = InventoryConnection {
        client,
        signal: signal(),
    };
    assert_eq!(owner.available().unwrap_err().code, "native_unavailable");
    let first = owner.readiness(connection.clone(), 7).await.unwrap();
    assert_eq!(first.get("ready"), Some(&IvyJson::Bool(true)));
    assert!(names.lock().unwrap().is_empty());
    now.store(42001, Ordering::SeqCst);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        owner.readiness(connection.clone(), 7),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        *names.lock().unwrap(),
        vec![IvyJson::from("instructions"), "mcp".into(), "skills".into()]
    );
    now.store(42002, Ordering::SeqCst);
    owner.readiness(connection, 7).await.unwrap();
    assert_eq!(names.lock().unwrap().len(), 3);
    drop(owner);
    gate.send_replace(true);
    while *finished.borrow_and_update() < 3 {
        finished.changed().await.unwrap();
    }
    rpc.close("fixture_complete").await;
}
