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
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
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
struct TraceClient(Mutex<VecDeque<IvyJson>>);
impl RpcClient for TraceClient {
    fn request(&self, method: &str, params: IvyJson, _: RequestOptions) -> RequestFuture<'_> {
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
            .0
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
    let directory = Database::new();
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/data-collector-control-oracle.mjs",
            "runtime",
        ])
        .arg(directory.0.join("oracle.sqlite"))
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let store = directory.open();
    let cleared = Arc::new(AtomicUsize::new(0));
    let engine = Engine::new(
        store.clone(),
        Arc::new(NoWork(cleared.clone())),
        json(r#"{"rootObjectId":"root","secrets":{"KNOWN":"synthetic-do-not-store"}}"#),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let client = Arc::new(TraceClient::default());
    engine.attach(client.clone()).await.unwrap();
    let definitions = Definitions::new(
        "fixture-node".into(),
        engine.clone(),
        Arc::new(|_| Ok(None)),
        Arc::new(|a, b| a.cmp(b)),
    );
    for (index, case) in cases.as_array().unwrap().iter().enumerate() {
        let step = field(case, "step");
        *client.0.lock().unwrap() = field(case, "trace")
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
                if input == true.into() {
                    engine.detach();
                } else {
                    engine.attach(client.clone()).await.unwrap();
                }
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
            client.0.lock().unwrap().is_empty(),
            "step {index} omitted calls {:?}",
            client.0.lock().unwrap()
        );
        assert_eq!(
            cleared.load(Ordering::SeqCst) as f64,
            field(case, "clears").as_f64().unwrap(),
            "step {index} cleanup"
        );
        let (tasks, runs) = store
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
    }
    engine.close().await;
}
