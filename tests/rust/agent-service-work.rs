use super::{compare, field, oracle, text};
use ivy_agent_manager::service_work::{AgentRun, AgentWork};
use ivy_contracts::{IvyError, IvyJson};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

#[tokio::test]
async fn accepted_agent_work_matches_original_waiters_and_shutdown_snapshot() {
    let data = oracle("agent-service-work-oracle.mjs", None);
    let rows = field(&data, "cases").as_array().unwrap();
    assert_eq!(rows.len(), 8);
    for row in rows {
        let owner = AgentWork::default();
        let completed = Arc::new(Mutex::new(Vec::<IvyJson>::new()));
        let mut gates = HashMap::new();
        let mut handles = HashMap::<_, AgentRun>::new();
        let mut snapshots = HashMap::new();
        let mut trace = Vec::new();
        for step in field(row, "steps").as_array().unwrap() {
            let action = text(step, "action").to_utf8().unwrap();
            let mut record = IvyJson::object([("action", action.clone().into())]);
            let name = step.get("name").and_then(IvyJson::as_js_string).cloned();
            match action.as_str() {
                "start" => {
                    let name = name.unwrap();
                    let (send, receive) = oneshot::channel();
                    gates.insert(name.clone(), send);
                    let fail = step.get("fail") == Some(&IvyJson::Bool(true));
                    let value = step.get("value").cloned().unwrap_or(IvyJson::Null);
                    let completed = completed.clone();
                    let finished = name.clone();
                    let work = owner
                        .track(Box::pin(async move {
                            receive.await.unwrap();
                            completed.lock().unwrap().push(IvyJson::String(finished));
                            if fail {
                                Err(IvyError::new("fixture_work_failed", "Fixture work failed."))
                            } else {
                                Ok(value)
                            }
                        }))
                        .unwrap();
                    handles.insert(name, work);
                }
                "release" => {
                    gates.remove(&name.unwrap()).unwrap().send(()).unwrap();
                    continue;
                }
                "clone" => {
                    handles.insert(
                        text(step, "target"),
                        handles.get(&name.unwrap()).unwrap().clone(),
                    );
                }
                "drop" => {
                    handles.remove(&name.unwrap());
                }
                "snapshot" => {
                    snapshots.insert(name.unwrap(), owner.drain_snapshot());
                }
                "waitSnapshot" => {
                    snapshots.remove(&name.unwrap()).unwrap().await.unwrap();
                }
                "drain" => {
                    owner.drain_snapshot().await.unwrap();
                }
                "wait" => match handles.get(&name.unwrap()).unwrap().clone().wait().await {
                    Ok(value) => record.insert("result", value),
                    Err(error) => record.insert("error", error.to_wire()),
                },
                _ => panic!("Unknown original work action"),
            }
            record.insert("pending", IvyJson::Number(owner.pending().unwrap() as f64));
            trace.push(record);
        }
        let mut completed = completed.lock().unwrap().clone();
        completed.sort_by(|left, right| {
            left.as_js_string()
                .unwrap()
                .0
                .cmp(&right.as_js_string().unwrap().0)
        });
        compare(
            &IvyJson::object([
                ("trace", IvyJson::Array(trace)),
                ("completed", IvyJson::Array(completed)),
            ]),
            field(row, "expected"),
            &text(row, "name").to_utf8().unwrap(),
        );
    }
}

#[tokio::test]
async fn panicking_owned_work_releases_shutdown_and_returns_internal_failure() {
    let owner = AgentWork::default();
    let work = owner
        .track(Box::pin(async {
            panic!("Isolated owned-work panic fixture")
        }))
        .unwrap();
    assert_eq!(work.wait().await.unwrap_err().code, "internal_error");
    owner.drain_snapshot().await.unwrap();
    assert_eq!(owner.pending().unwrap(), 0);
}
