use ivy_chat_bridge::collection_reads::ChatCollectionReads;
use ivy_contracts::{
    IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::{canonical, parse},
};
use std::{collections::BTreeMap, path::Path, process::Command, sync::OnceLock};

fn array<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    value.required(key).unwrap().as_array().unwrap()
}
fn text(value: &Json, key: &str) -> String {
    value.required(key).unwrap().as_str().unwrap()
}
fn string<'a>(value: &'a Json, key: &str) -> &'a JsString {
    value.required(key).unwrap().as_js_string().unwrap()
}
fn result(value: IvyResult<Json>) -> Json {
    match value {
        Ok(value) => Json::object([("value", value)]),
        Err(error) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(error.code_js())),
                ("message", Json::String(error.message_js())),
                ("outcome", error.outcome.to_json()),
            ]),
        )]),
    }
}
fn equal(actual: &Json, expected: &Json, label: &str) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap(),
        "{label}"
    );
}
fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-collection-reads-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_COLLECTION_READS_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
fn expand(value: &Json) -> Json {
    match value {
        Json::Array(values) => Json::Array(values.iter().map(expand).collect()),
        Json::Object(values) => {
            if let Some(repeat) = value.get("$repeatText") {
                return Json::String(JsString(
                    string(repeat, "unit")
                        .0
                        .repeat(repeat.required("count").unwrap().as_f64().unwrap() as usize),
                ));
            }
            Json::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), expand(value)))
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}
fn text_summary(value: &JsString) -> Json {
    let encoded = value.to_utf8().unwrap();
    let mut summary = Json::object([
        ("utf16Length", Json::Number(value.utf16_len() as f64)),
        ("byteLength", Json::Number(encoded.len() as f64)),
        ("contentHash", digest(encoded.as_bytes()).into()),
    ]);
    if value.utf16_len() < 128 {
        summary.insert("value", Json::String(value.clone()));
    }
    summary
}
fn snapshot(states: &BTreeMap<String, ChatCollectionReads>) -> Json {
    Json::Object(
        states
            .iter()
            .map(|(key, state)| {
                let mut value = Json::object([
                    ("phase", state.phase.as_str().into()),
                    (
                        "cursor",
                        state.cursor.clone().map(Json::String).unwrap_or(Json::Null),
                    ),
                    (
                        "foundCursor",
                        state
                            .found_cursor
                            .clone()
                            .map(Json::String)
                            .unwrap_or(Json::Null),
                    ),
                    ("nativeState", state.native_state.clone()),
                    ("nativeBytes", Json::Number(state.native_bytes as f64)),
                    ("count", Json::Number(state.count as f64)),
                    ("textBytes", Json::Number(state.text_bytes as f64)),
                    (
                        "texts",
                        Json::Array(state.texts.iter().map(text_summary).collect()),
                    ),
                    ("next", result(state.next())),
                ]);
                if let Some(at) = &state.last_observed_at {
                    value.insert("lastObservedAt", at.clone());
                }
                (key.clone().into(), value)
            })
            .collect(),
    )
}
fn compare(kind: &str) {
    let corpus = oracle();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut compared = 0;
    for case in array(corpus, "cases")
        .iter()
        .filter(|case| text(case, "kind") == kind)
    {
        let selected = array(corpus, "catalogs")
            .iter()
            .find(|catalog| catalog.get("version") == case.get("version"))
            .unwrap();
        let mut states = BTreeMap::from([(
            "main".to_owned(),
            ChatCollectionReads::new(
                string(case, "threadId").clone(),
                string(case, "turnId").clone(),
            ),
        )]);
        let mut observations = Vec::<Json>::new();
        for (index, step) in array(case, "steps").iter().enumerate() {
            let state_name = step
                .get("state")
                .and_then(Json::as_js_string)
                .map(|v| v.to_utf8().unwrap())
                .unwrap_or_else(|| "main".into());
            let mut observed = None;
            let output = (|| -> IvyResult<Json> {
                match text(step, "action").as_str() {
                    "next" => return states[&state_name].next(),
                    "fork" => {
                        let fork = states[&text(step, "from")].fork();
                        states.insert(text(step, "target"), fork);
                        return Ok(Json::Null);
                    }
                    "append" => {}
                    _ => panic!("Unknown reducer fixture action"),
                }
                let state = &states[&state_name];
                let mut value = if let Some(reuse) = step.get("reuse") {
                    observations[reuse.as_f64().unwrap() as usize].clone()
                } else {
                    let request = state.next()?;
                    let mut reply = expand(step.required("reply")?);
                    if step.get("items") == Some(&Json::Bool(true))
                        && selected.get("layout") == Some(&"wrapped".into())
                    {
                        let mut body = reply.required("result")?.clone();
                        let turn_id = step
                            .get("itemTurn")
                            .cloned()
                            .unwrap_or_else(|| Json::String(state.turn_id.clone()));
                        let items = array(&body, "data")
                            .iter()
                            .map(|item| {
                                Json::object([("turnId", turn_id.clone()), ("item", item.clone())])
                            })
                            .collect();
                        body.insert("data", Json::Array(items));
                        reply.insert("result", body);
                    }
                    let mut value = Json::object([
                        ("schemaVersion", Json::Number(1.0)),
                        (
                            "observationId",
                            step.get("id").cloned().unwrap_or_else(|| {
                                format!("{}:observation-{index}", text(case, "label")).into()
                            }),
                        ),
                        (
                            "requestId",
                            format!("{}:request-{index}", text(case, "label")).into(),
                        ),
                        ("serviceNodeId", "agent".into()),
                        ("callerPrincipalId", "chat".into()),
                        ("epoch", "epoch".into()),
                        ("nativeVersion", case.required("version")?.clone()),
                        (
                            "nativeExecutableHash",
                            selected.required("nativeExecutableHash")?.clone(),
                        ),
                        ("catalogHash", selected.required("catalogHash")?.clone()),
                        ("method", request.required("method")?.clone()),
                        ("params", request.required("params")?.clone()),
                        ("requestHash", hash_json(&request)?.into()),
                        (
                            "observedAt",
                            step.get("at")
                                .cloned()
                                .unwrap_or_else(|| "2026-09-07T04:00:00.000Z".into()),
                        ),
                        ("reply", reply),
                    ]);
                    if let Some(patch) = step.get("patch") {
                        for (key, value_to_set) in patch.as_object().unwrap() {
                            value.insert(key.clone(), value_to_set.clone());
                        }
                    }
                    value
                };
                observed = Some(value.clone());
                observations.push(std::mem::replace(&mut value, Json::Null));
                Ok(Json::Bool(
                    states
                        .get_mut(&state_name)
                        .unwrap()
                        .append(observed.as_ref().unwrap(), &catalogs)?,
                ))
            })();
            let descriptor = observed
                .as_ref()
                .map(|observed| {
                    let encoded = canonical(observed, 32 * 1024 * 1024).unwrap();
                    Json::object([
                        ("byteLength", Json::Number(encoded.len() as f64)),
                        ("contentHash", digest(encoded.as_bytes()).into()),
                    ])
                })
                .unwrap_or(Json::Null);
            let actual = Json::object([
                ("output", result(output)),
                ("observation", descriptor),
                ("states", snapshot(&states)),
            ]);
            equal(
                &actual,
                &array(case, "expected")[index],
                &format!("{} step{index}", text(case, "label")),
            );
            compared += 1;
        }
    }
    assert!(compared > 0);
}

#[test]
fn ordered_reads_fallback_forks_and_terminal_proof_match_original() {
    compare("behavior");
}
#[test]
fn actual_read_count_and_evidence_byte_bounds_match_original() {
    compare("limits");
}
