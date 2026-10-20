use ivy_agent_manager::claude::{
    error::{AdapterError, Result},
    models::runtime_model_list,
    projects::ProjectStore,
    rate_limits::RateLimitStore,
};
use ivy_contracts::{
    IvyJson as J, JsString,
    json::{canonical, parse, stringify},
};
use ivy_host_runtime::node_path::{self, Style};
use std::{
    collections::VecDeque,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};
fn v(text: &str) -> J {
    parse(text, usize::MAX).unwrap()
}
static NULL: J = J::Null;
fn f<'a>(value: &'a J, key: &str) -> &'a J {
    value.get(key).unwrap_or(&NULL)
}
fn s(value: &J) -> &JsString {
    value.as_js_string().unwrap()
}
fn capture(result: Result<J>) -> J {
    match result {
        Ok(value) => J::object([("result", value)]),
        Err(e) => J::object([(
            "error",
            J::object([("name", e.name.into()), ("message", J::String(e.message))]),
        )]),
    }
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-claude-adapter-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let actual = fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-claude-adapter-")
        );
        assert!(
            !fs::symlink_metadata(&self.0)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(actual).unwrap();
    }
}
fn oracle(root: &Path, spec: &J) -> J {
    let mut input = spec.clone();
    input.insert("root", J::String(node_path::from_native(root)));
    let mut child = Command::new("node")
        .arg("tests/fixtures/agent-claude-data-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap()
}
fn compare(label: &str, actual: J, expected: J) {
    if let Some(path) = std::env::var_os("IVY_CLAUDE_ADAPTER_ATTEMPT_PROOF") {
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            canonical(
                &J::object([
                    ("label", label.into()),
                    ("actual", actual.clone()),
                    ("expected", expected.clone())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_eq!(
        canonical(&actual, usize::MAX).unwrap(),
        canonical(&expected, usize::MAX).unwrap(),
        "{label}"
    );
    if let Some(path) = std::env::var_os("IVY_CLAUDE_ADAPTER_PROOF") {
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            canonical(
                &J::object([
                    ("label", label.into()),
                    ("actual", actual),
                    ("expected", expected)
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
}
#[path = "agent-claude-adapter/protocol.rs"]
mod protocol_cases;
#[path = "agent-claude-adapter/sdk.rs"]
mod sdk_cases;
#[path = "agent-claude-adapter/events.rs"]
mod event_cases;
#[path = "agent-claude-adapter/driver.rs"]
mod driver_cases;
#[path = "agent-claude-adapter/workflows.rs"]
mod workflow_cases;
#[path = "agent-claude-adapter/workflow_files.rs"]
mod workflow_file_cases;
fn local(home: &Path, spec: &J) -> J {
    let mutable = Arc::new(Mutex::new((
        f(spec, "now").as_f64().unwrap_or(1700000000123.0),
        0usize,
        0usize,
        f(spec, "threads").clone(),
        f(spec, "ids")
            .as_array()
            .unwrap_or(&[])
            .iter()
            .map(|v| s(v).clone())
            .collect::<VecDeque<_>>(),
    )));
    let state = mutable.clone();
    let clock = Arc::new(move || {
        let mut state = state.lock().unwrap();
        state.2 += 1;
        (state.0 / 1000.0).floor()
    });
    let state = mutable.clone();
    let ids = Arc::new(move || {
        let mut state = state.lock().unwrap();
        state.1 += 1;
        state.4.pop_front().expect("Fixture id exhausted")
    });
    let state = mutable.clone();
    let lookup = move |id: &JsString| -> Result<Option<J>> {
        Ok(state.lock().unwrap().3.get_key(id).cloned())
    };
    let directory = std::env::var_os("IVY_TEST_LOCALE_RUNTIME")
        .expect("Provide the qualified packaged locale runtime");
    let locale = ivy_sdk::collation::LocaleRuntime::load(Path::new(&directory))
        .unwrap()
        .executing_default()
        .unwrap();
    let compare = Arc::new(move |a: &JsString, b: &JsString| locale.compare(a, b));
    let style = if f(spec, "style") == &J::from("posix") {
        Style::Posix
    } else {
        Style::Windows
    };
    let project = f(spec, "kind") == &J::from("project");
    let mut projects =
        ProjectStore::open(home, style, compare.clone(), clock.clone(), ids.clone()).unwrap();
    let mut rates = RateLimitStore::open(home);
    let path = home.join(if project {
        "projects.json"
    } else {
        "rate-limits.json"
    });
    let mut records = Vec::new();
    for step in f(spec, "steps").as_array().unwrap() {
        if let Some(now) = step.get("now") {
            mutable.lock().unwrap().0 = now.as_f64().unwrap();
        }
        let op = f(step, "op").as_str().unwrap();
        let now = mutable.lock().unwrap().0;
        let result = (|| -> Result<J> {
            match op.as_str() {
                "reopen" => {
                    if project {
                        projects = ProjectStore::open(
                            home,
                            style,
                            compare.clone(),
                            clock.clone(),
                            ids.clone(),
                        )?;
                    } else {
                        rates = RateLimitStore::open(home);
                    }
                    Ok(J::Null)
                }
                "threads" => {
                    mutable.lock().unwrap().3 = f(step, "value").clone();
                    Ok(J::Null)
                }
                "blockWrite" => {
                    fs::create_dir(home.join(if project {
                        "projects.json.tmp"
                    } else {
                        "rate-limits.json.tmp"
                    }))?;
                    Ok(J::Null)
                }
                "create" => projects.create(f(step, "params"), &lookup),
                "update" => projects.update(f(step, "params"), &lookup),
                "delete" => {
                    projects.delete(f(step, "id"), &lookup)?;
                    Ok(J::Null)
                }
                "read" if project => projects.read(f(step, "id"), &lookup),
                "list" => projects.list(f(step, "params"), &lookup),
                "bind" => {
                    projects.bind(f(step, "thread"), f(step, "id"), &lookup)?;
                    Ok(J::Null)
                }
                "projectId" => projects.project_id(f(step, "thread"), &lookup),
                "event" => {
                    rates.record_event(f(step, "value"), now);
                    Ok(J::Null)
                }
                "usage" => {
                    rates.record_usage(f(step, "value"), now);
                    Ok(J::Null)
                }
                "due" => Ok(rates
                    .usage_due(
                        now,
                        step.get("interval").and_then(J::as_f64).unwrap_or(60000.0),
                    )
                    .into()),
                "read" => {
                    let seconds = step
                        .get("seconds")
                        .and_then(J::as_f64)
                        .unwrap_or((now / 1000.0).floor());
                    rates.read(seconds)
                }
                _ => Err(AdapterError::new("Unknown fixture operation")),
            }
        })();
        let mut record = capture(result);
        record.insert("op", op.into());
        record.insert("file", fs::read_to_string(&path).map_or(J::Null, J::from));
        let state = mutable.lock().unwrap();
        record.insert("idCalls", J::Number(state.1 as f64));
        record.insert("clockCalls", J::Number(state.2 as f64));
        records.push(record);
    }
    J::object([("records", J::Array(records))])
}
#[test]
fn project_identity_membership_recency_and_replay_match_original() {
    let root = Root::new();
    for style in ["windows", "posix"] {
        let original = root.0.join(format!("original-{style}"));
        let native = root.0.join(format!("native-{style}"));
        fs::create_dir(&original).unwrap();
        fs::create_dir(&native).unwrap();
        let mut spec = v(
            r#"{"kind":"project","now":1700000000123,"ids":["é","é","z"],"threads":{},"steps":[{"op":"list","params":{}},{"op":"create","params":{"idempotencyKey":"first","name":"  Alpha  ","roots":[],"metadata":{"é":"e","é":"combining","2":"two","10":"ten"}}},{"op":"create","params":{"idempotencyKey":"first","name":"Alpha","roots":[],"metadata":{"2":"two","10":"ten","é":"e","é":"combining"}}},{"op":"create","params":{"idempotencyKey":"first","name":"Other","roots":[]}},{"op":"create","params":{"idempotencyKey":"__proto__","name":"Beta","roots":[]}},{"op":"bind","id":"é","thread":{"id":"thread","ephemeral":false}},{"op":"bind","id":"é","thread":{"id":"temporary","ephemeral":true}},{"op":"threads","value":{"thread":{"id":"thread","updatedAt":99,"archived":false,"ephemeral":false}}},{"op":"list","params":{"sortKey":"recencyAt"}},{"op":"read","id":"é"},{"op":"projectId","thread":{"id":"temporary"}},{"op":"projectId","thread":{"id":"child","forkedFromId":"thread"}},{"op":"update","now":1700000100999,"params":{"projectId":"é","name":"Updated","metadata":null}},{"op":"reopen"},{"op":"projectId","thread":{"id":"temporary"}},{"op":"list","params":{"limit":1}},{"op":"list","params":{"cursor":"1","limit":1}},{"op":"delete","id":"é"},{"op":"projectId","thread":{"id":"thread"}},{"op":"create","params":{"idempotencyKey":"first","name":"Alpha","roots":[],"metadata":{"é":"e","é":"combining","2":"two","10":"ten"}}},{"op":"reopen"},{"op":"list","params":{}}]}"#,
        );
        spec.insert("style", style.into());
        compare(style, local(&native, &spec), oracle(&original, &spec));
    }
}
#[test]
fn project_validation_and_pagination_match_original() {
    let root = Root::new();
    let original = root.0.join("original");
    let native = root.0.join("native");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&native).unwrap();
    let spec = v(
        r#"{"kind":"project","ids":["project"],"steps":[{"op":"create","params":{}},{"op":"create","params":{"idempotencyKey":"a","name":" "}},{"op":"create","params":{"idempotencyKey":"a","name":"yes","roots":null}},{"op":"create","params":{"idempotencyKey":"a","name":"yes","roots":[{"path":"relative"}]}},{"op":"create","params":{"idempotencyKey":"a","name":"yes","roots":[],"metadata":{"x":1}}},{"op":"read","id":null},{"op":"list","params":{"sortDirection":"desc"}},{"op":"list","params":{"cursor":"-1"}},{"op":"list","params":{"cursor":"1.5"}},{"op":"list","params":{"cursor":"9007199254740992"}},{"op":"list","params":{"limit":"bad"}},{"op":"create","params":{"idempotencyKey":"a","name":"yes","roots":[{"path":"C:\\fixture\\folder\\..\\project\\"}]}},{"op":"list","params":{"limit":0}},{"op":"list","params":{"limit":201}}]}"#,
    );
    compare(
        "project validation",
        local(&native, &spec),
        oracle(&original, &spec),
    );
}
#[test]
fn quota_windows_refresh_persistence_and_memory_fallback_match_original() {
    let root = Root::new();
    let original = root.0.join("original");
    let native = root.0.join("native");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&native).unwrap();
    let spec = v(
        r#"{"kind":"rates","now":10000,"steps":[{"op":"due"},{"op":"read"},{"op":"event","value":{"rateLimitType":"five_hour","utilization":0.445,"resetsAt":20,"status":"rejected"}},{"op":"read"},{"op":"event","value":{"rateLimitType":"overage","utilization":1}},{"op":"usage","now":20000,"value":{"five_hour":{"utilization":-5,"resets_at":"1970-01-01T00:01:00Z"},"seven_day":{"utilization":110,"resets_at":120},"seven_day_opus":{"utilization":55.5,"resets_at":"invalid"},"model_scoped":[{"display_name":"é","utilization":12.5,"resets_at":-0.5},{"display_name":"","utilization":5}]}},{"op":"due","now":79999},{"op":"due","now":80000},{"op":"read","seconds":60},{"op":"reopen"},{"op":"due","now":100},{"op":"read","seconds":200},{"op":"usage","now":1000,"value":null},{"op":"due","now":60000},{"op":"blockWrite"},{"op":"event","value":{"rateLimitType":"seven_day_sonnet","utilization":0.3}},{"op":"read"}]}"#,
    );
    compare(
        "quota windows",
        local(&native, &spec),
        oracle(&original, &spec),
    );
}
#[test]
fn dynamic_model_alias_effort_and_display_projection_match_original() {
    let root = Root::new();
    let spec = v(
        r#"{"kind":"models","rows":[{"models":[],"model":"default","effort":"medium"},{"models":[{"value":"default","resolvedModel":"claude-opus-4-8-20261001","displayName":"Opus","description":"default","supportedEffortLevels":["low","high"]},{"value":"opus","resolvedModel":"claude-opus-4-8-20261001","displayName":"Opus 4.8","description":"actual","supportedEffortLevels":["low","high"]},{"value":"opus[1m]","resolvedModel":"claude-opus-4-8-20261001","displayName":"Opus","description":"long","supportedEffortLevels":["max"]}],"model":"opus[1m]","effort":"high"},{"models":[{"value":"other","displayName":"Other","description":"value","supportsEffort":false,"supportedEffortLevels":["high"]}],"model":"missing","effort":"high"}]}"#,
    );
    let records = f(&spec, "rows")
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            capture(runtime_model_list(
                f(row, "models").as_array().unwrap(),
                s(f(row, "model")),
                s(f(row, "effort")),
            ))
        })
        .collect();
    compare(
        "model projection",
        J::object([("records", J::Array(records))]),
        oracle(&root.0, &spec),
    );
}
