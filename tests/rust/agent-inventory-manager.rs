#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_agent_manager::notice_policy::NativeNoticePolicy;
use ivy_contracts::{IvyJson, IvyResult, json::parse};
use std::sync::OnceLock;
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn text(value: &IvyJson, key: &str) -> String {
    field(value, key).as_str().unwrap()
}
fn array<'a>(value: &'a IvyJson, key: &str) -> &'a [IvyJson] {
    field(value, key).as_array().unwrap()
}
fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let result = reference_data::read(include_bytes!(
            "../fixtures/agent-inventory-manager-cases.json.gz"
        ));
        parse(std::str::from_utf8(&result).unwrap(), 8 * 1024 * 1024).unwrap()
    })
}
fn outcome(value: IvyResult<IvyJson>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("result", value)]),
        Err(error) => IvyJson::object([("error", error.to_wire())]),
    }
}
fn sorted(value: IvyJson) -> IvyJson {
    if value.as_object().is_some() {
        let mut pairs = value.into_object().unwrap();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        IvyJson::Object(pairs.into_iter().map(|(k, v)| (k, sorted(v))).collect())
    } else if value.as_array().is_some() {
        IvyJson::Array(
            value
                .into_array()
                .unwrap()
                .into_iter()
                .map(sorted)
                .collect(),
        )
    } else {
        value
    }
}
fn same(actual: IvyJson, expected: &IvyJson, name: &str, index: usize) {
    assert_eq!(
        sorted(actual),
        sorted(expected.clone()),
        "{name} step {index}"
    );
}
fn policy_snapshot(policy: &NativeNoticePolicy) -> IvyResult<IvyJson> {
    let mut values = Vec::new();
    for id in array(data(), "checks") {
        let key = id.as_js_string().unwrap().clone();
        values.push((
            key.clone(),
            policy
                .is_internal(&key)?
                .map(IvyJson::Bool)
                .unwrap_or(IvyJson::Null),
        ));
    }
    Ok(IvyJson::object([
        ("revision", IvyJson::Number(policy.revision())),
        ("internal", IvyJson::Object(values)),
    ]))
}

#[test]
fn notice_policy_matches_original_membership_revisions_and_push_events() {
    let rows = array(data(), "policyRows");
    assert_eq!(rows.len(), 21);
    for row in rows {
        let mut owner = NativeNoticePolicy::default();
        for (index, step) in array(row, "commands").iter().enumerate() {
            let result = (|| {
                match text(step, "action").as_str() {
                    "setProjects" => owner.set_projects(array(step, "projects").to_vec()),
                    "setThread" => owner.set_thread(
                        field(step, "id").as_js_string().unwrap().clone(),
                        field(step, "value"),
                        step.get("revision").and_then(IvyJson::as_f64),
                    ),
                    "setThreads" => owner.set_threads(
                        array(step, "entries"),
                        field(step, "revision").as_f64().unwrap(),
                    )?,
                    "observe" => {
                        let event = field(step, "event");
                        let method = event
                            .get("method")
                            .and_then(IvyJson::as_js_string)
                            .cloned()
                            .unwrap_or_default();
                        owner.observe(&method, event.get("params").unwrap_or(&IvyJson::Null));
                    }
                    "snapshot" => return policy_snapshot(&owner),
                    action => panic!("{action}"),
                }
                Ok(IvyJson::Null)
            })();
            same(
                outcome(result),
                &array(row, "expected")[index],
                &text(row, "name"),
                index,
            );
        }
    }
}
