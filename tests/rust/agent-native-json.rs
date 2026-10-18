use super::{
    Fixture, Handle, Normalize, Store, context, equal, execute, field, management_for, outcome,
    string,
};
use ivy_agent_manager::native_management::operation_outcome;
use ivy_contracts::{IvyJson, IvyResult, JsString, json::parse};
use ivy_sdk::service::{InvocationContext, ToolHandler};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{Arc, OnceLock},
    time::Duration,
};

fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let result = Command::new("node")
            .args([
                "tests/fixtures/agent-native-management-oracle.mjs",
                "--native-json",
            ])
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
async fn native_step(
    fixture: &mut Fixture,
    management: &HashMap<JsString, ToolHandler>,
    json: &HashMap<JsString, ToolHandler>,
    contexts: &mut HashMap<String, InvocationContext>,
    step: &IvyJson,
) -> IvyResult<IvyJson> {
    let action = string(step, "action");
    match action.as_str() {
        "json" | "startJson" => {
            let context = context(step);
            if let Some(handle) = step.get("handle") {
                contexts.insert(handle.as_str().unwrap(), context.clone());
            }
            let method = step.get("method").unwrap().as_js_string().unwrap();
            let mut name = JsString::from("codex.");
            name.0.extend_from_slice(&method.0);
            let work = json.get(&name).unwrap()(field(step, "input").clone(), context);
            if action == "json" {
                return work.await;
            }
            fixture
                .handles
                .insert(string(step, "handle"), Handle::track(work));
            Ok(IvyJson::Null)
        }
        "abort" => {
            contexts
                .get(&string(step, "handle"))
                .unwrap()
                .signal
                .cancel();
            Ok(IvyJson::Null)
        }
        "outcome" => operation_outcome(field(step, "operation")),
        "jsonDefinitions" => {
            let mut names: Vec<_> = json.keys().cloned().collect();
            names.sort();
            Ok(IvyJson::Array(
                names.into_iter().map(IvyJson::String).collect(),
            ))
        }
        _ => execute(fixture, management, step).await,
    }
}

#[tokio::test]
async fn native_json_handlers_preserve_read_cancellation_and_durable_mutation_outcomes() {
    let rows = field(data(), "cases").as_array().unwrap();
    assert!(rows.len() >= 45);
    for row in rows {
        let catalog = field(data(), "catalog").clone();
        let store = Arc::new(Store::new());
        let mut fixture = Fixture::with_catalog(row, store.clone(), store, catalog.clone());
        let owner = management_for(&fixture, catalog);
        let management = owner.handlers();
        let json = owner.json_handlers();
        let mut contexts = HashMap::new();
        let mut normalize = Normalize::default();
        for (index, step) in field(row, "commands")
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let action = string(step, "action");
            let label = format!("{} step {index}: {action}", string(row, "name"));
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                native_step(&mut fixture, &management, &json, &mut contexts, step),
            )
            .await
            .expect(&label);
            if action == "sent" {
                normalize.frame(result.as_ref().unwrap());
            }
            let mut actual = if action == "wait" {
                result.unwrap()
            } else {
                outcome(result)
            };
            normalize.value(&mut actual);
            equal(
                &actual,
                &field(row, "expected").as_array().unwrap()[index],
                &label,
            );
        }
        tokio::time::timeout(Duration::from_secs(5), fixture.finish())
            .await
            .unwrap();
    }
}
