use super::*;
#[path = "hive_catalog_fixture.rs"]
mod catalog_fixture;
use ivy_hive::{
    storage::{Completion, Notification, StorageService},
    store::Credential,
    worker_actions::WorkerActions,
    worker_kernel::KernelWorker,
};
use rusqlite::functions::FunctionFlags;
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "../fixtures/reference-data.rs"]
mod reference_data;
fn json_value(value: &IvyJson) -> Value {
    serde_json::from_str(&stringify(value, usize::MAX).unwrap()).unwrap()
}
fn original(input: &Value) -> Value {
    static CASES: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let cases = CASES.get_or_init(|| {
        serde_json::from_slice(&reference_data::read(include_bytes!(
            "../fixtures/hive-worker-loop-reference.json.gz"
        )))
        .unwrap()
    });
    let key = ivy_contracts::hash::digest(format!("worker-loop\n{input}"));
    cases["traces"]
        .get(&key)
        .unwrap_or_else(|| panic!("Missing original Hive worker loop trace: {key}"))
        .clone()
}
fn service(root: &Root, fail: usize) -> (KernelWorker, Arc<AtomicUsize>) {
    let profile = startup_profile::registry(true);
    let mut kernel = compose(
        &root.0.join("native.sqlite"),
        &profile,
        Rc::new(Cell::new(100000.0)),
    );
    let credentials = [Credential {
        principal_id: "alpha".into(),
        digest: format!("sha256:{}", "a".repeat(64)).into(),
    }];
    kernel
        .parts
        .store
        .configure_credentials(&credentials)
        .unwrap();
    let actions = WorkerActions::new(&mut kernel, &credentials).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    kernel
        .parts
        .store
        .connection()
        .create_scalar_function(
            "fixture_sequence",
            0,
            FunctionFlags::SQLITE_UTF8,
            move |_| {
                if observed.fetch_add(1, Ordering::SeqCst) < fail {
                    Err(rusqlite::Error::UserFunctionError(Box::new(
                        std::io::Error::other("Native sequence fixture failed"),
                    )))
                } else {
                    Ok(7_i64)
                }
            },
        )
        .unwrap();
    (KernelWorker::new(kernel, actions), calls)
}
fn credentials() -> Value {
    json!([{"principalId":"alpha","digest":format!("sha256:{}","a".repeat(64))}])
}
const VIEW: &str = "ALTER TABLE events RENAME TO saved_events; CREATE VIEW events AS SELECT fixture_sequence() AS sequence;";
fn dispatch_frames(service: &mut KernelWorker, id: u64, payload: &Value) -> (Vec<Value>, bool) {
    let dispatch = service.dispatch(crate::payload_fixture::native(IvyJson::from_serde(payload).unwrap()));
    let mut frames = vec![json!({"id":id.to_string(),"started":true})];
    frames.extend(dispatch.notifications.into_iter().map(|v| match v {
        Notification::Routing(v) => json!({"routing":json_value(&catalog_fixture::routing_to_wire(&v))}),
        Notification::EventsAvailable(v) => json!({"eventsAvailable":json_value(&v)}),
        Notification::Changes(v) => {
            json!({"changes":v.iter().map(|s|s.to_utf8().unwrap()).collect::<Vec<_>>()})
        }
    }));
    match dispatch.completion {
        Completion::Reply(result) => {
            frames.push(match result {
                Ok(v) => json!({"id":id.to_string(),"value":json_value(&v)}),
                Err(e) => json!({"id":id.to_string(),"error":json_value(&e.to_wire())}),
            });
            (frames, false)
        }
        Completion::Failed(_) => (frames, true),
    }
}

#[test]
fn kernel_worker_matches_original_publication_retry_action_errors_and_partial_fatal_chain() {
    for (fail, action_error) in [(0, false), (1, false), (2, false), (0, true), (1, true)] {
        let root = Root::new();
        let (mut worker, calls) = service(&root, fail);
        let failure = if action_error {
            json!({"action":"authenticate","context":{"credentialDigest":"invalid"}})
        } else {
            json!({"action":"health"})
        };
        let steps = json!([
            {"id":1,"payload":{"action":"health"}},
            {"id":2,"sql":VIEW,"invalidate":["objects","system/fixture"],"payload":failure},
            {"id":3,"payload":{"action":"health"}}
        ]);
        let expected =
            original(&json!({"credentials":credentials(),"failSequenceCalls":fail,"steps":steps}));
        let mut frames = vec![];
        let mut rejected = false;
        for step in steps.as_array().unwrap() {
            if let Some(sql) = step["sql"].as_str() {
                worker
                    .kernel()
                    .unwrap()
                    .parts
                    .store
                    .connection()
                    .execute_batch(sql)
                    .unwrap();
            }
            for scope in step["invalidate"].as_array().into_iter().flatten() {
                worker
                    .kernel_mut()
                    .unwrap()
                    .parts
                    .store
                    .invalidate(scope.as_str().unwrap().into());
            }
            let (values, failed) =
                dispatch_frames(&mut worker, step["id"].as_u64().unwrap(), &step["payload"]);
            frames.extend(values);
            rejected = failed;
            if failed {
                break;
            }
        }
        assert_eq!(
            json!(frames),
            expected["frames"],
            "fail={fail} action_error={action_error}"
        );
        assert_eq!(
            rejected,
            expected["states"].as_array().unwrap().last().unwrap()["rejected"].is_object()
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            expected["sequenceCalls"].as_u64().unwrap() as usize
        );
    }
}
