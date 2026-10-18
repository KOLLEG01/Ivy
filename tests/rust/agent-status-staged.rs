use ivy_agent_manager::{
    native_journal::{Identity, Limits, NativeJournal, Owner},
    staged_files::staged_name,
    status::{AgentStatus, StatusConfig, StatusSource, StatusState},
};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::service::InvocationContext;
use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};
fn get<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn text(value: &IvyJson, key: &str) -> JsString {
    get(value, key).as_js_string().unwrap().clone()
}
fn json(value: &str) -> IvyJson {
    parse(value, MANAGEMENT_FRAME_BYTES).unwrap()
}
fn flag(value: &IvyJson, key: &str) -> bool {
    value.get(key) == Some(&IvyJson::Bool(true))
}
fn optional(value: &IvyJson, key: &str) -> Option<IvyJson> {
    if flag(value, &format!("{key}Present")) {
        value.get(key).cloned()
    } else {
        value
            .get(key)
            .filter(|value| match value {
                IvyJson::Null | IvyJson::Bool(false) => false,
                IvyJson::Number(n) => *n != 0.0,
                IvyJson::String(s) => !s.0.is_empty(),
                _ => true,
            })
            .cloned()
    }
}

#[tokio::test]
async fn status_handler_and_staged_names_match_original_runtime_observations() {
    let output = Command::new("node")
        .arg("tests/fixtures/agent-status-staged-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        4 * 1024 * 1024,
    )
    .unwrap();
    for row in get(&data, "names").as_array().unwrap() {
        assert_eq!(staged_name(&text(row, "name")), text(row, "result"));
    }
    for row in get(&data, "cases").as_array().unwrap() {
        let input = get(row, "input");
        let time = text(&data, "time");
        let mut journal = NativeJournal::with_clock(
            Owner {
                service_node_id: text(get(&data, "owner"), "serviceNodeId"),
                native_version: text(get(&data, "owner"), "nativeVersion"),
                native_executable_hash: text(get(&data, "owner"), "nativeExecutableHash"),
            },
            Limits {
                max_operations: 100,
                max_journal_bytes: 128 * 1024 * 1024,
                max_pending_inputs: 16,
            },
            Arc::new({
                let time = time.clone();
                move || time.clone()
            }),
        )
        .unwrap();
        let seed = text(input, "seed").to_utf8().unwrap();
        if seed != "empty" {
            journal.begin_epoch(&"epoch".into()).unwrap();
        }
        if ["operation", "input", "lost", "closed"].contains(&seed.as_str()) {
            journal
                .accept(
                    &Identity {
                        caller_principal_id: "caller".into(),
                        operation_id: "operation".into(),
                    },
                    &"turn/start".into(),
                    &json(r#"{"threadId":"thread"}"#),
                )
                .unwrap();
        }
        if ["input", "lost", "closed"].contains(&seed.as_str()) {
            journal
                .observe_input(
                    &json(r#"{"serviceNodeId":"agent","epoch":"epoch","requestId":7}"#),
                    &"currentTime/read".into(),
                    &json(r#"{"threadId":"thread"}"#),
                )
                .unwrap();
        }
        if seed == "lost" {
            journal
                .lose_epoch(&"epoch".into(), &"native_unavailable".into())
                .unwrap();
        }
        if seed == "closed" {
            journal.close();
        }
        let source: Arc<StatusSource> = Arc::new({
            let input = input.clone();
            move || {
                Ok(StatusState {
                    owner: get(&input, "owner").clone(),
                    closing: flag(&input, "closing"),
                    connected: flag(&input, "connected"),
                    ready: flag(&input, "ready"),
                    inventory_code: input
                        .get("threadCode")
                        .and_then(IvyJson::as_js_string)
                        .or_else(|| input.get("projectCode").and_then(IvyJson::as_js_string))
                        .cloned(),
                    capabilities: get(&input, "capabilities").clone(),
                    instructions: optional(&input, "instructions"),
                    mcp: optional(&input, "mcp"),
                    skills: optional(&input, "skills"),
                })
            }
        });
        let status = Arc::new(AgentStatus::with_clock(
            StatusConfig {
                service_node_id: text(get(input, "config"), "serviceNodeId"),
                host_id: text(get(input, "config"), "hostId"),
                settings: get(input, "settings").clone(),
                catalog_hash: text(input, "catalogHash"),
                process_stops: get(input, "processStops").clone(),
                windows: flag(input, "windows"),
            },
            Arc::new(Mutex::new(journal)),
            source,
            Arc::new(move || time.clone()),
        ));
        // Original agent.status ignores args, availability and caller cancellation.
        let context = InvocationContext::detached(
            "caller".into(),
            None,
            1,
            Default::default(),
            IvyError::new("fixture_cancelled", "cancelled"),
        );
        context.signal.cancel();
        let result = (status.handler())(false.into(), context).await;
        let actual = match result {
            Ok(result) => IvyJson::object([("result", result)]),
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(get(row, "expected"), MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            text(row, "name").to_utf8().unwrap()
        );
    }
}
