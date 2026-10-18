use super::{Database, field, json, text};
use ivy_contracts::{IvyError, IvyJson, Outcome, builtin_schema::json_equal, json::parse};
use ivy_data_collector::{
    definitions::Definitions,
    engine::{Engine, RegisterJob, Runner},
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::ServiceFuture,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};
use tokio_util::sync::CancellationToken;
struct FixedClock;
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        1000000
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct NoWork(Arc<AtomicUsize>);
impl Runner for NoWork {
    fn run(
        &self,
        _: IvyJson,
        _: IvyJson,
        _: IvyJson,
        _: CancellationToken,
        _: RegisterJob,
    ) -> ServiceFuture<IvyJson> {
        Box::pin(async { panic!("Control fixture must not execute workloads") })
    }
    fn stopped(&self, _: String, _: String) -> ServiceFuture<bool> {
        Box::pin(async { Ok(false) })
    }
    fn clear_authentication(&self, _: &str) -> Result<(), IvyError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn normalized(mut value: IvyJson) -> IvyJson {
    value.remove("mutationId");
    if let Some(mut create) = value.remove("create") {
        if let Some(name) = create.get("name").and_then(|v| v.as_str().ok()) {
            if ["definition-", "script-", "assignment-", "observation-"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                create.insert("name", "<uuid-name>".into());
            }
        }
        value.insert("create", create);
    }
    value
}
#[derive(Default)]
struct TraceClient {
    trace: Mutex<VecDeque<IvyJson>>,
    peers: Mutex<HashMap<String, (Definitions, Arc<AtomicBool>)>>,
}
fn peer_params(mut value: IvyJson) -> IvyJson {
    // Peer mutation IDs are generated independently; preserve their routed/inner
    // equality while comparing all caller-supplied values exactly.
    if let Some(operation) = value.get("operation").cloned() {
        let mut args = value.remove("args").unwrap();
        assert_eq!(args.get("operationId"), Some(&operation));
        args.insert("operationId", "<generated-operation>".into());
        value.insert("args", args);
        value.insert("operation", "<generated-operation>".into());
    }
    value
}
impl RpcClient for TraceClient {
    fn request(&self, method: &str, params: IvyJson, _: RequestOptions) -> RequestFuture<'_> {
        if method == "tools.list" {
            let name = format!("data-collector.{}", text(&params, "namePrefix"));
            let node = field(&params, "serviceNodeId").clone();
            return Box::pin(async move {
                Ok(IvyJson::object([
                    (
                        "provider",
                        IvyJson::object([("node", IvyJson::object([("serviceNodeId", node)]))]),
                    ),
                    ("nextCursor", IvyJson::Null),
                    (
                        "items",
                        IvyJson::Array(vec![IvyJson::object([
                            ("qualifiedName", name.clone().into()),
                            (
                                "definitionHash",
                                format!("sha256:{}", "a".repeat(64)).into(),
                            ),
                            (
                                "definition",
                                IvyJson::object([
                                    ("interfaceVersion", "1.0.0".into()),
                                    (
                                        "annotations",
                                        IvyJson::object([(
                                            "readOnlyHint",
                                            (name == "data-collector.read").into(),
                                        )]),
                                    ),
                                ]),
                            ),
                        ])]),
                    ),
                ]))
            });
        }
        if method == "tools.call" {
            let mut peer = IvyJson::object([
                ("node", field(&params, "serviceNodeId").clone()),
                ("name", field(&params, "qualifiedName").clone()),
                ("args", field(&params, "arguments").clone()),
            ]);
            if let Some(operation) = params.get("operationId") {
                peer.insert("operation", operation.clone());
            }
            let expected = self.trace.lock().unwrap().pop_front().expect("peer trace");
            assert_eq!(text(&expected, "method"), "fixture.peer");
            assert!(
                json_equal(
                    &peer_params(peer.clone()),
                    &peer_params(field(&expected, "params").clone())
                ),
                "peer request {peer:?}; expected {expected:?}"
            );
            let (definitions, online) = self
                .peers
                .lock()
                .unwrap()
                .get(&text(&peer, "node"))
                .unwrap()
                .clone();
            return Box::pin(async move {
                let result = if !online.load(Ordering::SeqCst) {
                    Err(IvyError::new(
                        "provider_unavailable",
                        "Fixture source is offline.",
                    ))
                } else if text(&peer, "name") == "data-collector.read" {
                    definitions.read(field(&peer, "args").clone()).await
                } else {
                    definitions.update(field(&peer, "args").clone(), None).await
                };
                match &result {
                    Ok(value) => assert!(
                        json_equal(value, field(&expected, "result")),
                        "peer result {value:?}; expected {expected:?}"
                    ),
                    Err(error) => assert_eq!(error.code, text(field(&expected, "error"), "code")),
                }
                result
            });
        }
        if method == "system.status" {
            return Box::pin(async { Ok(json(r#"{"runtimeEpoch":"fixture-epoch"}"#)) });
        }
        if method == "objects.read" && params.get("objectId") == Some(&"root".into()) {
            return Box::pin(async {
                Ok(json(
                    r#"{"object":{"id":"root","contractKey":"data-collector/root","effectivelyArchived":false}}"#,
                ))
            });
        }
        let expected = self
            .trace
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected call {method} {params:?}"));
        assert_eq!(
            method,
            text(&expected, "method"),
            "actual params {params:?}; expected {expected:?}"
        );
        assert!(
            json_equal(
                &normalized(params.clone()),
                &normalized(field(&expected, "params").clone())
            ),
            "request mismatch: actual {params:?}; expected {expected:?}"
        );
        Box::pin(async move {
            if let Some(error) = expected.get("error") {
                Err(IvyError::new(text(error, "code"), text(error, "message"))
                    .with_outcome(Outcome::NotExecuted))
            } else {
                Ok(field(&expected, "result").clone())
            }
        })
    }
}
#[tokio::test]
async fn central_control_matches_original_drafts_activation_recovery_and_freeze() {
    control_trace("runtime").await;
}
#[tokio::test]
async fn two_instances_match_original_frozen_handoff_authentication_and_one_time_adoption() {
    tokio::time::timeout(std::time::Duration::from_secs(30), control_trace("handoff"))
        .await
        .unwrap();
}
async fn control_trace(mode: &str) {
    let directory = Database::new();
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/data-collector-control-oracle.mjs",
            "runtime",
        ])
        .arg(directory.0.join("oracle.sqlite"))
        .arg(mode)
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let cleared = Arc::new(AtomicUsize::new(0));
    let client = Arc::new(TraceClient::default());
    let mut nodes = HashMap::new();
    let directories = [Database::new(), Database::new()];
    for (index, name) in ["fixture-node", "target-node"].iter().enumerate() {
        let store = directories[index].open();
        let engine = Engine::new(
            store.clone(),
            Arc::new(NoWork(cleared.clone())),
            json(r#"{"rootObjectId":"root","secrets":{"KNOWN":"synthetic-do-not-store"}}"#),
            Arc::new(FixedClock),
        )
        .await
        .unwrap();
        engine.attach(client.clone()).await.unwrap();
        let fingerprint = Arc::new(Mutex::new(None));
        let captured = fingerprint.clone();
        let definitions = Definitions::new(
            (*name).into(),
            engine.clone(),
            Arc::new(move |_| Ok(captured.lock().unwrap().clone())),
            Arc::new(|a, b| a.cmp(b)),
        );
        let online = Arc::new(AtomicBool::new(true));
        client
            .peers
            .lock()
            .unwrap()
            .insert((*name).to_string(), (definitions.clone(), online.clone()));
        nodes.insert(
            (*name).to_string(),
            (store, engine, definitions, fingerprint, online),
        );
    }
    for (index, case) in cases.as_array().unwrap().iter().enumerate() {
        let step = field(case, "step");
        let node = step
            .get("node")
            .map(|v| v.as_str().unwrap())
            .unwrap_or_else(|| "fixture-node".to_string());
        let (store, engine, definitions, fingerprint, online) = nodes.get(&node).unwrap();
        *client.trace.lock().unwrap() = field(case, "trace")
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .collect();
        let input = field(step, "input").clone();
        let result = match text(step, "kind").as_str() {
            "localSave" => engine.update(input, None).await,
            "localPatch" => {
                store
                    .call(move |s| {
                        let mut row = s.get(&text(&input, "id"), false)?.unwrap();
                        row.insert("state", field(&input, "state").clone());
                        row.insert("nextAt", field(&input, "nextAt").clone());
                        s.save(&row)?;
                        Ok(IvyJson::Null)
                    })
                    .await
            }
            "update" => definitions.update(input, None).await,
            "read" => definitions.read(input).await,
            "sync" => definitions.sync().await.map(|_| IvyJson::Null),
            "offline" => {
                online.store(input != true.into(), Ordering::SeqCst);
                if input == true.into() {
                    engine.detach();
                } else {
                    engine.attach(client.clone()).await.unwrap();
                }
                Ok(IvyJson::Null)
            }
            "fingerprint" => {
                *fingerprint.lock().unwrap() = Some(input.as_str().unwrap());
                Ok(IvyJson::Null)
            }
            "clearCache" => {
                store
                    .call(|s| {
                        s.connection()
                            .execute("DELETE FROM tasks", [])
                            .map_err(|_| IvyError::internal())?;
                        Ok(IvyJson::Null)
                    })
                    .await
            }
            "stopQueued" => {
                store
                    .call(|s| {
                        for mut run in s.runs("queued")? {
                            run.insert("status", "cancelled".into());
                            run.insert("error", "cancelled".into());
                            run.insert("finishedAt", "1970-01-01T00:16:40.000Z".into());
                            s.save_run(&run)?;
                        }
                        Ok(IvyJson::Null)
                    })
                    .await
            }
            kind => panic!("unknown step {kind}"),
        };
        let (result, error) = match result {
            Ok(v) => (v, IvyJson::Null),
            Err(e) => (
                IvyJson::Null,
                IvyJson::object([
                    ("code", e.code.into()),
                    ("message", e.message.into()),
                    ("outcome", e.outcome.as_str().into()),
                ]),
            ),
        };
        assert!(
            json_equal(&error, field(case, "error")),
            "step {index} {step:?} error {error:?}; expected {:?}",
            field(case, "error")
        );
        assert!(
            json_equal(&result, field(case, "result")),
            "step {index} {step:?} result {result:?}; expected {:?}",
            field(case, "result")
        );
        assert!(
            client.trace.lock().unwrap().is_empty(),
            "step {index} omitted calls {:?}",
            client.trace.lock().unwrap()
        );
        assert_eq!(
            cleared.load(Ordering::SeqCst) as f64,
            field(case, "clears").as_f64().unwrap(),
            "step {index} cleanup"
        );
        let (tasks, runs) = nodes
            .get("fixture-node")
            .unwrap()
            .0
            .call(|s| {
                let mut runs = Vec::new();
                for status in ["queued", "running", "publishing", "failed", "cancelled"] {
                    runs.extend(s.runs(status)?);
                }
                Ok((s.list(true)?, runs))
            })
            .await
            .unwrap();
        assert!(
            json_equal(&IvyJson::Array(tasks), field(case, "tasks")),
            "step {index} local cache"
        );
        assert!(
            json_equal(&IvyJson::Array(runs), field(case, "runs")),
            "step {index} run cache"
        );
        if let Some(expected_nodes) = case.get("nodes") {
            for (name, (store, _, _, _, _)) in &nodes {
                let snapshot = store
                    .call(|s| {
                        let mut runs = Vec::new();
                        for status in ["queued", "running", "publishing", "failed", "cancelled"] {
                            runs.extend(s.runs(status)?);
                        }
                        let handoff = ["handoff-lock:", "handoff-state:", "handoff-consumed:"]
                            .into_iter()
                            .map(|prefix| {
                                Ok((
                                    prefix,
                                    s.metadata(&format!("{prefix}object-2"))?
                                        .map_or(IvyJson::Null, IvyJson::from),
                                ))
                            })
                            .collect::<Result<Vec<_>, IvyError>>()?;
                        Ok(IvyJson::object([
                            ("tasks", IvyJson::Array(s.list(true)?)),
                            ("runs", IvyJson::Array(runs)),
                            ("handoff", IvyJson::object(handoff)),
                        ]))
                    })
                    .await
                    .unwrap();
                assert!(
                    json_equal(&snapshot, field(expected_nodes, name)),
                    "step {index} node {name}: {snapshot:?}; expected {:?}",
                    field(expected_nodes, name)
                );
            }
        }
    }
    client.peers.lock().unwrap().clear();
    for (_, (_, engine, _, _, _)) in nodes {
        engine.close().await;
    }
}
