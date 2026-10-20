use super::*;
use ivy_task_board::native_rate_limit::{RetryDateContext, SharedRetryDates};

impl RetryDateContext for &World {
    type Zone = JsString;
    fn parse(&self, value: &JsString) -> f64 {
        <World as RetryDates>::parse(self, value)
    }
    fn zone(&self, name: &JsString) -> IvyResult<JsString> {
        <World as RetryDates>::zone(self, name)
    }
    fn parts(&self, zone: &JsString, at: f64) -> IvyResult<LocalParts> {
        <World as RetryDates>::parts(self, zone, at)
    }
}
// Compare every shared numeric/ISO result against the original boundary while
// Date.parse, Intl, current-question, store and inspection effects stay explicit.
struct Dates<'a>(&'a World);
impl RetryDates for Dates<'_> {
    type Zone = JsString;
    fn parse(&self, value: &JsString) -> f64 {
        SharedRetryDates { context: self.0 }.parse(value)
    }
    fn zone(&self, name: &JsString) -> IvyResult<JsString> {
        SharedRetryDates { context: self.0 }.zone(name)
    }
    fn parts(&self, zone: &JsString, at: f64) -> IvyResult<LocalParts> {
        SharedRetryDates { context: self.0 }.parts(zone, at)
    }
    fn utc(&self, year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> f64 {
        let actual =
            SharedRetryDates { context: self.0 }.utc(year, month, day, hour, minute, second);
        let expected = <World as RetryDates>::utc(self.0, year, month, day, hour, minute, second);
        assert_eq!(number(actual), number(expected));
        actual
    }
    fn iso(&self, at: f64) -> IvyResult<JsString> {
        let actual = SharedRetryDates { context: self.0 }.iso(at);
        let expected = <World as RetryDates>::iso(self.0, at);
        assert_eq!(
            outcome(actual.clone().map(Some)),
            outcome(expected.map(Some))
        );
        actual
    }
}

#[tokio::test(flavor = "current_thread")]
async fn shared_dates_preserve_original_retry_results_and_all_required_context() {
    let mut rows = 0;
    let mut boundaries = 0;
    let mut utc = 0;
    let mut iso = 0;
    let mut invalid_iso = 0;
    for kind in ["pure", "guards"] {
        for row in f(data(), kind).as_array().unwrap() {
            let replay = Arc::new(Replay::new(row));
            let world = World(replay.clone());
            let dates = Dates(&world);
            let actual = if kind == "pure" {
                native_rate_limit_retry_at(&dates, f(row, "turn"), s(row, "observedAt"))
            } else {
                FailedRateLimit {
                    store: &world,
                    questions: &world,
                    dates: &dates,
                    inspector: &world,
                }
                .retry_at(&document(f(row, "task")))
                .await
            };
            assert_eq!(
                hash_json(&outcome(actual)).unwrap(),
                s(row, "expectedHash").to_utf8().unwrap(),
                "{:?}",
                s(row, "name")
            );
            replay.finished();
            rows += 1;
            for boundary in f(row, "boundaries").as_array().unwrap() {
                boundaries += 1;
                match f(boundary, "kind").as_str().unwrap().as_str() {
                    "utc" => utc += 1,
                    "iso" => {
                        iso += 1;
                        invalid_iso += usize::from(boundary.get("error").is_some());
                    }
                    _ => {}
                }
            }
        }
    }
    assert_eq!(
        (rows, boundaries, utc, iso, invalid_iso),
        (130, 545, 124, 79, 3)
    );
}

fn value_number(value: &Json) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| match value.as_str().unwrap().as_str() {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            "-0" => -0.0,
            _ => panic!("Unknown original numeric value"),
        })
}
fn success(value: Json) -> Json {
    Json::object([("ok", true.into()), ("value", value)])
}
#[test]
fn shared_date_primitives_match_original_node_across_the_full_numeric_range() {
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/sdk-query-date-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    let (mut dates, mut utc, mut skipped) = (0, 0, 0);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), MANAGEMENT_FRAME_BYTES).unwrap();
        let expected = parse(row["result"].as_str().unwrap(), MANAGEMENT_FRAME_BYTES).unwrap();
        let value = f(&input, "value");
        let actual = match s(&input, "kind").to_utf8().unwrap().as_str() {
            "query" => {
                skipped += 1;
                continue;
            }
            "utc" => {
                utc += 1;
                let args: Vec<_> = value.as_array().unwrap().iter().map(value_number).collect();
                let millis = ivy_sdk::clock::date_utc(
                    args[0], args[1], args[2], args[3], args[4], args[5], args[6],
                );
                success(Json::object([
                    (
                        "millis",
                        if millis.is_nan() {
                            "NaN".into()
                        } else {
                            Json::Number(millis)
                        },
                    ),
                    (
                        "negativeZero",
                        (millis == 0.0 && millis.is_sign_negative()).into(),
                    ),
                    (
                        "iso",
                        if millis.is_nan() {
                            Json::Null
                        } else {
                            Json::String(ivy_sdk::clock::date_to_iso(millis).unwrap())
                        },
                    ),
                ]))
            }
            "date" => {
                dates += 1;
                match ivy_sdk::clock::date_to_iso(value_number(value)) {
                    Ok(iso) => success(Json::String(iso)),
                    Err(error) => {
                        // Retain the shared API's explicit error mapping while
                        // comparing the original RangeError's exact message.
                        assert_eq!(error.code, "invalid_arguments");
                        assert_eq!(error.outcome, Outcome::NotExecuted);
                        Json::object([
                            ("ok", false.into()),
                            (
                                "error",
                                Json::object([
                                    ("code", Json::Null),
                                    ("message", Json::String(error.message_js())),
                                    ("outcome", Json::Null),
                                ]),
                            ),
                        ])
                    }
                }
            }
            _ => panic!("Unknown original date case"),
        };
        assert_eq!(actual, expected, "input={}", row["input"]);
    }
    assert_eq!((dates, utc, skipped), (5047, 5788, 14));
    assert_eq!(
        ivy_sdk::clock::date_to_iso(-0.0).unwrap(),
        ivy_sdk::clock::date_to_iso(0.0).unwrap()
    );
}
