use ivy_agent_manager::{
    native_journal::{Limits, NativeJournal, Owner},
    notifications::{NativeNotifications, notification_handler},
};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::service::InvocationContext;
use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

fn text<'a>(value: &'a IvyJson, field: &str) -> &'a JsString {
    value.get(field).unwrap().as_js_string().unwrap()
}
fn number(value: &IvyJson, field: &str, default: u64) -> u64 {
    value
        .get(field)
        .and_then(IvyJson::as_f64)
        .map_or(default, |n| n as u64)
}

#[tokio::test]
async fn notification_handler_matches_original_query_validation_epochs_and_utf16_values() {
    let result = Command::new("node")
        .arg("tests/fixtures/agent-notifications-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let data = parse(
        std::str::from_utf8(&result.stdout).unwrap(),
        4 * 1024 * 1024,
    )
    .unwrap();
    let rows = data.get("cases").unwrap().as_array().unwrap();
    assert!(rows.len() >= 20);
    for row in rows {
        let (node, version) = if let Some(owner) = row.get("owner") {
            (
                text(owner, "serviceNodeId").clone(),
                text(owner, "nativeVersion").clone(),
            )
        } else {
            ("node".into(), "0.159.2".into())
        };
        let events = Arc::new(Mutex::new(NativeNotifications::new_js(
            node.clone(),
            version.clone(),
            number(row, "start", 0),
            number(row, "end", 9_007_199_254_740_991),
            number(row, "maximumBytes", 4096) as usize,
        )));
        let journal = Arc::new(Mutex::new(
            NativeJournal::new(
                Owner {
                    service_node_id: node,
                    native_version: version,
                    native_executable_hash: hash_json(&IvyJson::Null).unwrap().into(),
                },
                Limits {
                    max_operations: 10,
                    max_journal_bytes: 64 * 1024 * 1024,
                    max_pending_inputs: 10,
                },
            )
            .unwrap(),
        ));
        let handler = notification_handler(events.clone(), journal.clone());
        let context = InvocationContext::detached(
            "caller".into(),
            None,
            1,
            Default::default(),
            IvyError::new("fixture_cancelled", "Cancelled."),
        );
        context.signal.cancel();
        for (i, step) in row
            .get("commands")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let label = format!("{} step {i}", row.get("name").unwrap().as_str().unwrap());
            let result = match text(step, "action").to_utf8().unwrap().as_str() {
                "observe" => events.lock().unwrap().observe_js(
                    text(step, "epoch"),
                    text(step, "method"),
                    step.get("params").unwrap().clone(),
                    &"2026-10-18T00:00:00.000Z".into(),
                ),
                "epoch" => {
                    let mut journal = journal.lock().unwrap();
                    if let Some(epoch) = step.get("epoch").unwrap().as_js_string() {
                        journal.begin_epoch(epoch).unwrap();
                    } else if let Some(epoch) = journal.epoch().cloned() {
                        journal
                            .lose_epoch(&epoch, &"fixture_closed".into())
                            .unwrap();
                    }
                    Ok(IvyJson::Null)
                }
                "page" => handler(step.get("query").unwrap().clone(), context.clone()).await,
                _ => unreachable!(),
            };
            let actual = match result {
                Ok(value) => IvyJson::object([("result", value)]),
                Err(error) => IvyJson::object([("error", error.to_wire())]),
            };
            let expected = &row.get("expected").unwrap().as_array().unwrap()[i];
            assert_eq!(
                canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
                canonical(expected, MANAGEMENT_FRAME_BYTES).unwrap(),
                "{label}"
            );
        }
    }
}
