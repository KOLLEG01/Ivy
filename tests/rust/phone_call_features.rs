use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_phone_bridge::{
    call_features::{CallAccessSettings, CallFeatureTime, FeatureDelay, PhoneCallFeatures},
    dtmf::InputSource,
    dtmf_access::{AccessCodeGate, CodeProgress, FailureLimiter},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::oneshot,
    time::{sleep, timeout},
};

#[derive(Default)]
struct Clock(Mutex<(i64, Vec<(i64, oneshot::Sender<()>)>)>);
impl Clock {
    fn new() -> Self {
        Self(Mutex::new((638_000_000_000_000_000, Vec::new())))
    }
    fn advance(&self, ms: i64) {
        let mut s = self.0.lock().unwrap();
        s.0 += ms * 10_000;
        let mut index = 0;
        while index < s.1.len() {
            if s.1[index].0 <= s.0 {
                let (_, tx) = s.1.remove(index);
                let _ = tx.send(());
            } else {
                index += 1;
            }
        }
    }
}
impl CallFeatureTime for Clock {
    fn utc_ticks(&self) -> i64 {
        self.0.lock().unwrap().0
    }
    fn delay(&self, duration: Duration) -> FeatureDelay {
        let mut s = self.0.lock().unwrap();
        let due = s.0 + duration.as_nanos() as i64 / 100;
        let (tx, rx) = oneshot::channel();
        s.1.push((due, tx));
        Box::pin(async move {
            let _ = rx.await;
        })
    }
}
fn j(value: &str) -> IvyJson {
    parse(value, 1_000_000).unwrap()
}
fn n(value: usize) -> IvyJson {
    IvyJson::Number(value as f64)
}
fn op(name: &str) -> IvyJson {
    IvyJson::object([("op", name.into())])
}
fn digits(text: &str) -> IvyJson {
    IvyJson::object([("op", "digits".into()), ("digits", text.into())])
}
fn configure(challenge: IvyJson, incoming: bool) -> IvyJson {
    IvyJson::object([
        ("op", "configure".into()),
        ("challenge", challenge),
        ("incoming", incoming.into()),
    ])
}
fn code(value: &str) -> IvyJson {
    IvyJson::object([
        ("code", value.into()),
        ("failureResponseDelayMilliseconds", n(0)),
    ])
}
fn advance(ms: usize) -> IvyJson {
    IvyJson::object([("op", "advance".into()), ("milliseconds", n(ms))])
}
fn wait(count: usize) -> IvyJson {
    IvyJson::object([("op", "wait".into()), ("hangups", n(count))])
}
fn trace(name: &str, value: IvyJson) {
    if let Some(path) = std::env::var_os("IVY_PHONE_CALL_AUDIO_TRACE_DIR") {
        std::fs::write(
            std::path::PathBuf::from(path).join(format!("{name}.json")),
            stringify(&value, 32_000_000).unwrap(),
        )
        .unwrap();
    }
}
fn fixture(mode: &str) -> IvyJson {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-call-features-cases.json");
    parse(&std::fs::read_to_string(path).unwrap(), 1024 * 1024)
        .unwrap()
        .get(mode)
        .unwrap()
        .clone()
}
fn scenarios() -> Vec<Vec<IvyJson>> {
    let mut rows = vec![
        vec![
            op("status"),
            op("connected"),
            digits("*0#"),
            configure(IvyJson::Null, false),
            digits("*0#"),
            configure(IvyJson::Null, false),
        ],
        vec![
            configure(code("bad"), true),
            configure(code("*aB09#"), false),
            configure(code("*aB09#"), true),
            digits("?09*Ab09#"),
            op("connected"),
            digits("*125#*0#"),
        ],
        vec![
            configure(code("*12#"), true),
            digits("**12#"),
            op("connected"),
            digits("*136#*0#"),
        ],
        vec![
            configure(code("*12#"), true),
            digits("*13#"),
            wait(1),
            op("connected"),
            digits("*12#*0#"),
        ],
        vec![
            configure(code("*12#"), true),
            digits(&format!("*{}#", "1".repeat(100))),
            wait(1),
        ],
        vec![
            op("screen"),
            configure(IvyJson::Null, false),
            digits("0*#1"),
            op("bridge"),
            op("screenTimeout"),
            op("connected"),
            digits("*0#"),
        ],
        vec![
            configure(IvyJson::Null, false),
            op("connected"),
            op("screen"),
            digits("2"),
            wait(1),
            op("bridge"),
            op("screenTimeout"),
        ],
        vec![
            op("screen"),
            op("screenTimeout"),
            wait(1),
            configure(IvyJson::Null, false),
            digits("1"),
            op("bridge"),
        ],
        vec![
            configure(code("*1#"), true),
            op("screen"),
            op("bridge"),
            op("connected"),
            digits("*1#"),
            op("screen"),
            digits("1"),
            op("bridge"),
        ],
        vec![
            configure(code("*1#"), true),
            op("connected"),
            advance(59_999),
            digits("*1#"),
            advance(1),
            op("status"),
        ],
        vec![
            configure(code("*1#"), true),
            op("connected"),
            advance(60_000),
            wait(1),
            digits("*1#"),
            op("new"),
            configure(code("*1#"), true),
        ],
        vec![
            configure(code("*1#"), true),
            op("connected"),
            op("dispose"),
            advance(120_000),
            digits("*1#"),
            op("screen"),
        ],
        vec![
            configure(IvyJson::Null, false),
            op("connected"),
            digits("*125#*0#*116#*0#*2#*0#"),
            digits(&format!("*{}#*0#", "9".repeat(32))),
            op("dispose"),
            digits("*0#"),
            op("bridge"),
        ],
        vec![
            configure(IvyJson::Null, false),
            op("connected"),
            digits(&"*0#".repeat(35)),
            op("dispose"),
        ],
    ];
    for attempt in ["*1#", "*121#", "**1#", "***12#", "**12#", "*12##?", "*1?2#"] {
        let valid = ["**12#", "*12##?", "*1?2#"].contains(&attempt);
        let mut row = vec![configure(code("*12#"), true), digits(attempt)];
        if !valid {
            row.push(wait(1));
        }
        rows.push(row);
    }
    for bad in [
        r#"{"code":"*1#","timeoutSeconds":0}"#,
        r#"{"code":"*1#","timeoutSeconds":121}"#,
        r#"{"code":"*1#","maxFailedAttempts":0}"#,
        r#"{"code":"*1#","maxFailedAttempts":101}"#,
        r#"{"code":"*1#","failedAttemptWindowSeconds":0}"#,
        r#"{"code":"*1#","failedAttemptWindowSeconds":86401}"#,
        r#"{"code":"*1#","failureResponseDelayMilliseconds":-1}"#,
        r#"{"code":"*1#","failureResponseDelayMilliseconds":5001}"#,
        r#"{"code":"*é#"}"#,
    ] {
        rows.push(vec![
            configure(j(bad), true),
            configure(IvyJson::Null, false),
        ]);
    }
    let mut selection = vec![configure(IvyJson::Null, false), op("connected")];
    for model in 0..=4 {
        for effort in 0..=7 {
            selection.push(digits(&format!("*1{model}{effort}#*0#")));
        }
    }
    rows.push(selection);
    // A shared refusal is immutable and never leaves the next call trusted.
    rows.push(vec![
        configure(
            j(r#"{"code":"*1#","maxFailedAttempts":1,"failureResponseDelayMilliseconds":0}"#),
            true,
        ),
        digits("*2#"),
        wait(1),
        op("new"),
        configure(j(r#"{"code":"*1#","maxFailedAttempts":1}"#), true),
        configure(IvyJson::Null, false),
        op("connected"),
        digits("*0#"),
        advance(600_000),
        op("new"),
        configure(j(r#"{"code":"*1#","maxFailedAttempts":1}"#), true),
    ]);
    rows
}
async fn run_scenario(steps: &[IvyJson]) -> Vec<IvyJson> {
    let limiter = Arc::new(Mutex::new(FailureLimiter::default()));
    let clock = Arc::new(Clock::new());
    let hangups = Arc::new(AtomicUsize::new(0));
    let create = || {
        let count = hangups.clone();
        PhoneCallFeatures::new(limiter.clone(), clock.clone(), move || {
            count.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap()
    };
    let mut features = create();
    let mut rows = Vec::new();
    for step in steps {
        let name = step.get("op").unwrap().as_str().unwrap();
        let outcome = match name.as_str() {
            "configure" => {
                // This policy corpus supplies typed valid JSON DTOs. Native
                // deserialization/error ordering has its own handler corpus.
                let value = step.get("challenge").unwrap();
                let settings = if value.is_null() {
                    None
                } else {
                    let mut settings =
                        CallAccessSettings::new(value.get("code").unwrap().as_str().unwrap());
                    for (key, target) in [
                        ("timeoutSeconds", &mut settings.timeout_seconds),
                        ("maxFailedAttempts", &mut settings.max_failed_attempts),
                        (
                            "failedAttemptWindowSeconds",
                            &mut settings.failed_attempt_window_seconds,
                        ),
                        (
                            "failureResponseDelayMilliseconds",
                            &mut settings.failure_response_delay_milliseconds,
                        ),
                    ] {
                        if let Some(value) = value.get(key) {
                            *target = value.as_f64().unwrap() as i32;
                        }
                    }
                    Some(settings)
                };
                features.configure(settings, step.get("incoming").unwrap().as_bool().unwrap())
            }
            "connected" => {
                features.connected();
                Ok(())
            }
            "digits" => {
                let source = match step.get("source").map(|v| v.as_str().unwrap()).as_deref() {
                    Some("SipInfo") => InputSource::SipInfo,
                    Some("InBandAudio") => InputSource::InBandAudio,
                    _ => InputSource::Rfc4733,
                };
                let mut outcome = Ok(());
                for digit in step.get("digits").unwrap().as_str().unwrap().chars() {
                    outcome = features.digit(digit, source);
                    if outcome.is_err() {
                        break;
                    }
                }
                outcome
            }
            "screen" => features.start_screening(),
            "bridge" => features.bridge_screening(),
            "screenTimeout" => {
                features.screening_timeout();
                Ok(())
            }
            "dispose" => {
                features.dispose();
                Ok(())
            }
            "advance" => {
                clock.advance(step.get("milliseconds").unwrap().as_f64().unwrap() as i64);
                Ok(())
            }
            "wait" => {
                let expected = step.get("hangups").unwrap().as_f64().unwrap() as usize;
                timeout(Duration::from_secs(3), async {
                    while hangups.load(Ordering::SeqCst) < expected {
                        sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
                Ok(())
            }
            "new" => {
                features.dispose();
                features = create();
                Ok(())
            }
            "status" => Ok(()),
            _ => unreachable!(),
        };
        let error = outcome.err().map_or(IvyJson::Null, |e| e.code().into());
        rows.push(if name == "advance" {
            IvyJson::object([("error", error)])
        } else {
            IvyJson::object([
                ("error", error),
                ("features", j(&features.observation().json())),
                ("needsInBand", features.needs_inband_dtmf().into()),
                ("authenticated", features.authenticated().into()),
                (
                    "hangups",
                    if name == "wait" {
                        n(hangups.load(Ordering::SeqCst))
                    } else {
                        IvyJson::Null
                    },
                ),
            ])
        });
    }
    features.dispose();
    rows
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feature_policy_preserves_access_screening_commands_and_deadlines() {
    let scenarios = scenarios();
    let inputs = IvyJson::Array(scenarios.iter().cloned().map(IvyJson::Array).collect());
    let expected = fixture("policy");
    assert_eq!(expected.as_array().unwrap().len(), scenarios.len());
    let mut actual = Vec::new();
    for (index, scenario) in scenarios.iter().enumerate() {
        let row = IvyJson::Array(run_scenario(scenario).await);
        assert_eq!(
            &row,
            &expected.as_array().unwrap()[index],
            "scenario {index}"
        );
        actual.push(row);
    }
    trace(
        "call-features-policy",
        IvyJson::object([
            ("inputs", inputs),
            ("rust", IvyJson::Array(actual)),
            ("expected", expected),
        ]),
    );
}
#[test]
fn access_gate_preserves_framing_bounded_attempts_and_completion() {
    let mut cases = Vec::new();
    for code in [
        "*1#",
        "*aBcd09#",
        "*99999999999999999999999999999999999999999999999999999999999999#",
        "",
        "*#",
        "*E#",
        " *1#",
        "*é#",
    ] {
        for digits in [
            code.to_owned(),
            format!("*{code}"),
            format!("0?{code}#?1"),
            "*2#".into(),
            format!("*{}#", "1".repeat(100)),
            "***1#".into(),
            "*1?2#".into(),
        ] {
            cases.push(IvyJson::object([
                ("code", code.into()),
                ("digits", digits.into()),
            ]));
        }
    }
    let inputs = IvyJson::object([("cases", IvyJson::Array(cases.clone()))]);
    let expected = fixture("gate");
    assert_eq!(expected.as_array().unwrap().len(), cases.len());
    let mut actual = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let value = if let Some(mut gate) =
            AccessCodeGate::new(&case.get("code").unwrap().as_str().unwrap())
        {
            let mut digits = Vec::new();
            for digit in case.get("digits").unwrap().as_str().unwrap().chars() {
                let progress = match gate.process(digit) {
                    CodeProgress::Unsupported => "Unsupported",
                    CodeProgress::Ignored => "Ignored",
                    CodeProgress::Progress => "Progress",
                    CodeProgress::Accepted => "Accepted",
                    CodeProgress::Rejected => "Rejected",
                    CodeProgress::AlreadyCompleted => "AlreadyCompleted",
                };
                digits.push(IvyJson::object([
                    ("progress", progress.into()),
                    ("accepted", gate.is_accepted().into()),
                    ("disable", gate.disable_codec_upgrade().into()),
                ]));
            }
            IvyJson::object([("digits", IvyJson::Array(digits))])
        } else {
            IvyJson::object([("error", "invalid_request".into())])
        };
        assert_eq!(
            &value,
            &expected.as_array().unwrap()[index],
            "gate case {index}"
        );
        actual.push(value);
    }
    trace(
        "call-features-gate",
        IvyJson::object([
            ("inputs", inputs),
            ("rust", IvyJson::Array(actual)),
            ("expected", expected),
        ]),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejection_uses_system_delay_yields_and_disposal_cancels_the_original_timer() {
    let clock = Arc::new(Clock::new());
    let count = Arc::new(AtomicUsize::new(0));
    let original = count.clone();
    let features = PhoneCallFeatures::new(Arc::default(), clock.clone(), move || {
        original.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    let mut settings = CallAccessSettings::new("*1#".into());
    settings.failure_response_delay_milliseconds = 200;
    features.configure(Some(settings), true).unwrap();
    features.connected();
    let started = std::time::Instant::now();
    for digit in "*2#".chars() {
        features.digit(digit, InputSource::Rfc4733).unwrap();
    }
    assert_eq!(count.load(Ordering::SeqCst), 0);
    clock.advance(120_000);
    sleep(Duration::from_millis(40)).await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
    timeout(Duration::from_secs(3), async {
        while count.load(Ordering::SeqCst) == 0 {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(200));
    let original = count.clone();
    let features = PhoneCallFeatures::new(Arc::default(), clock.clone(), move || {
        original.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    features
        .configure(Some(CallAccessSettings::new("*1#".into())), true)
        .unwrap();
    features.connected();
    features.dispose();
    clock.advance(120_000);
    sleep(Duration::from_millis(20)).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    trace(
        "call-features-timers",
        IvyJson::object([
            ("rejectionUsedSystemDelay", true.into()),
            ("callbackDidNotRunInline", true.into()),
            ("disposedDeadlineCancelled", true.into()),
        ]),
    );
}
