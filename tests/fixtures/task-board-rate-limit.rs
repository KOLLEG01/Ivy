use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::clock::LocalParts;
use ivy_task_board::{
    native_inspector::Inspection,
    native_rate_limit::{
        FailedRateLimit, RetryDates, RetryInspection, RetryQuestions, native_rate_limit_retry_at,
        waiting_detail,
    },
    operations::{Document, Store},
};
use std::{
    collections::VecDeque,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    v.required(key).unwrap()
}
fn s<'a>(v: &'a Json, key: &str) -> &'a JsString {
    f(v, key).as_js_string().unwrap()
}
fn data() -> &'static Json {
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        let output = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-rate-limit-oracle.mjs",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(
            std::str::from_utf8(&output.stdout).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
fn number(value: f64) -> Json {
    let text = if value.is_nan() {
        Some("NaN")
    } else if value == f64::INFINITY {
        Some("Infinity")
    } else if value == f64::NEG_INFINITY {
        Some("-Infinity")
    } else if value == 0.0 && value.is_sign_negative() {
        Some("-0")
    } else {
        None
    };
    text.map_or(Json::Number(value), |text| {
        Json::object([("number", text.into())])
    })
}
fn numeric(value: &Json) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| match s(value, "number").to_utf8().unwrap().as_str() {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            "-0" => -0.0,
            _ => panic!("Unknown numeric tag"),
        })
}
fn error(v: &Json) -> IvyError {
    IvyError::new_js(s(v, "code").clone(), s(v, "message").clone())
        .with_outcome(match s(v, "outcome").to_utf8().unwrap().as_str() {
            "unknown" => Outcome::Unknown,
            "completed" => Outcome::Completed,
            _ => Outcome::NotExecuted,
        })
        .with_details(f(v, "details").clone())
}
fn outcome(result: IvyResult<Option<JsString>>) -> Json {
    match result {
        Ok(v) => Json::object([("ok", v.map_or(Json::Null, Json::String))]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.as_str().into()),
                ("details", e.details.unwrap_or(Json::Null)),
            ]),
        )]),
    }
}
struct Replay {
    name: JsString,
    steps: Mutex<VecDeque<Json>>,
}
impl Replay {
    fn new(row: &Json) -> Self {
        Self {
            name: s(row, "name").clone(),
            steps: Mutex::new(
                f(row, "boundaries")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
        }
    }
    fn call(&self, kind: &str, args: impl IntoIterator<Item = Json>) -> IvyResult<Json> {
        let actual = Json::Array(args.into_iter().collect());
        let expected = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("{:?}: unexpected {kind}: {actual:?}", self.name));
        assert_eq!(
            f(&expected, "kind"),
            &Json::from(kind),
            "{:?}: {actual:?}",
            self.name
        );
        assert_eq!(
            hash_json(f(&expected, "args")).unwrap(),
            hash_json(&actual).unwrap(),
            "{:?}: {kind} {actual:?}",
            self.name
        );
        if let Some(e) = expected.get("error") {
            Err(error(e))
        } else {
            Ok(f(&expected, "value").clone())
        }
    }
    fn finished(&self) {
        assert!(
            self.steps.lock().unwrap().is_empty(),
            "{:?}: unused original boundaries",
            self.name
        );
    }
}
// Required date/current-question/inspection providers replay actual Date/Intl
// and finite original store effects. This is not a production date provider or
// an implementation of the current question policy/native inspector.
struct World(Arc<Replay>);
impl RetryDates for World {
    type Zone = JsString;
    fn parse(&self, value: &JsString) -> f64 {
        numeric(&self.0.call("parse", [Json::String(value.clone())]).unwrap())
    }
    fn zone(&self, name: &JsString) -> IvyResult<JsString> {
        self.0.call("zone", [Json::String(name.clone())])?;
        Ok(name.clone())
    }
    fn parts(&self, zone: &JsString, at: f64) -> IvyResult<LocalParts> {
        let value = self
            .0
            .call("parts", [Json::String(zone.clone()), number(at)])?;
        let n = |key| f(&value, key).as_f64().unwrap();
        Ok(LocalParts {
            year: n("year") as i32,
            month: n("month") as u32,
            day: n("day") as u32,
            hour: n("hour") as u32,
            minute: n("minute") as u32,
            second: n("second") as u32,
            weekday: 0,
        })
    }
    fn utc(&self, year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> f64 {
        numeric(
            &self
                .0
                .call(
                    "utc",
                    [
                        year as f64,
                        month as f64,
                        day as f64,
                        hour as f64,
                        minute as f64,
                        second as f64,
                    ]
                    .map(Json::Number),
                )
                .unwrap(),
        )
    }
    fn iso(&self, at: f64) -> IvyResult<JsString> {
        self.0
            .call("iso", [number(at)])
            .map(|v| v.as_js_string().unwrap().clone())
    }
}
impl RetryQuestions for World {
    fn unanswered(&self, task: &Json, run: &JsString) -> IvyResult<bool> {
        self.0
            .call("question", [task.clone(), Json::String(run.clone())])
            .map(|v| v.as_bool().unwrap())
    }
}
impl RetryInspection for World {
    async fn find(&self, run: &Json) -> IvyResult<Inspection> {
        let v = self.0.call("inspect", [run.clone()])?;
        Ok(match s(&v, "kind").to_utf8().unwrap().as_str() {
            "found" => Inspection::Found {
                metadata: f(&v, "metadata").clone(),
                turns: f(&v, "turns").clone(),
            },
            "pending" => Inspection::Pending {
                search: f(&v, "search").clone(),
            },
            "absent" => Inspection::Absent,
            _ => panic!("Unknown inspection"),
        })
    }
}
fn document(value: &Json) -> Document {
    Document {
        pin: f(value, "pin").clone(),
        value: f(value, "value").clone(),
        parent_id: None,
        content_hash: hash_json(f(value, "value")).unwrap(),
    }
}
impl Store for World {
    fn root_object_id(&self) -> Option<JsString> {
        panic!("Retry cannot select a new workflow root")
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        panic!("Retry cannot change contracts")
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        panic!("Retry cannot admit operations")
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        panic!("Retry cannot select evidence placement")
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("Retry cannot close a workflow")
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        panic!("Retry cannot allocate a new identity")
    }
    async fn read(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        self.0
            .call("read", [key.into(), address.clone(), primary.into()])
            .map(|v| document(&v))
    }
    async fn write(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Retry cannot publish a new call")
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("Retry cannot allocate a mutation")
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        panic!("Retry cannot publish a new invocation")
    }
}
#[test]
fn failed_native_retry_dates_and_messages_match_original() {
    let rows = f(data(), "pure").as_array().unwrap();
    let mut count = 0;
    for row in rows {
        let replay = Arc::new(Replay::new(row));
        let world = World(replay.clone());
        let actual = outcome(native_rate_limit_retry_at(
            &world,
            f(row, "turn"),
            s(row, "observedAt"),
        ));
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: {actual:?}",
            s(row, "name")
        );
        count += f(row, "boundaries").as_array().unwrap().len();
        replay.finished();
    }
    assert_eq!(rows.len(), 91);
    assert_eq!(count, 433);
    for row in f(data(), "waiting").as_array().unwrap() {
        assert_eq!(&waiting_detail(s(row, "at")), s(row, "text"));
    }
}
#[tokio::test(flavor = "current_thread")]
async fn retry_reinspects_only_the_original_failed_run_with_current_question_guards() {
    let rows = f(data(), "guards").as_array().unwrap();
    let mut count = 0;
    for row in rows {
        let replay = Arc::new(Replay::new(row));
        let world = World(replay.clone());
        let subject = FailedRateLimit {
            store: &world,
            questions: &world,
            dates: &world,
            inspector: &world,
        };
        let actual = outcome(subject.retry_at(&document(f(row, "task"))).await);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: {actual:?}",
            s(row, "name")
        );
        count += f(row, "boundaries").as_array().unwrap().len();
        replay.finished();
    }
    assert_eq!(rows.len(), 39);
    assert_eq!(count, 112);
}
