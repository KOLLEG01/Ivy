use super::*;
use ivy_contracts::{JsString, operation::parse_operation_id_js};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    browser_notice::secretary_browser_notice,
    journal::{Journal, Limits},
    store::Store,
};
use std::sync::atomic::{AtomicI64, Ordering};
struct NoticeClock {
    at: i64,
    calls: AtomicI64,
}
impl Clock for NoticeClock {
    fn utc_millis(&self) -> i64 {
        self.at + self.calls.fetch_add(1, Ordering::SeqCst) * 1000
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
#[tokio::test]
async fn original_browser_notice_publication_and_receipts() {
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/browser-notice-oracle.mjs")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().expect("Original Node oracle required");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        8 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert!(rows.len() >= 35);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let client = Arc::new(Scripted {
            steps: Mutex::new(
                input
                    .required("steps")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            trace: Mutex::new(Vec::new()),
        });
        let journal = Journal::open(
            std::path::Path::new(":memory:"),
            Limits {
                count: input.required("capacity").unwrap().as_f64().unwrap() as i64,
                ..Limits::default()
            },
        )
        .unwrap();
        let tag = input.required("tag").unwrap().as_js_string().unwrap();
        let name = String::from_utf16_lossy(&tag.0);
        if input.get("existing") == Some(&IvyJson::Bool(true)) {
            journal
                .create_technical(
                    "secretary/browser-notice",
                    &name,
                    &IvyJson::object([("expiresAt", IvyJson::Number(1.0))]),
                )
                .unwrap();
        }
        let store = Store {
            client: client.clone(),
            root: corpus
                .required("root")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            journal: &journal,
        };
        let clock = NoticeClock {
            at: corpus.required("at").unwrap().as_f64().unwrap() as i64,
            calls: AtomicI64::new(0),
        };
        let mut results = Vec::new();
        for _ in 0..input.required("repeats").unwrap().as_f64().unwrap() as usize {
            let result = secretary_browser_notice(
                &store,
                input.required("node").unwrap().as_js_string().unwrap(),
                tag,
                input.required("text").unwrap().as_js_string().unwrap(),
                &clock,
            )
            .await;
            results.push(match result {
                Ok(()) => IvyJson::object([("value", IvyJson::Null)]),
                Err(e) => IvyJson::object([
                    ("code", e.code.into()),
                    ("message", e.message.into()),
                    ("outcome", e.outcome.to_json()),
                ]),
            });
        }
        let mut trace = client.trace.lock().unwrap().clone();
        let mut ids: Vec<JsString> = Vec::new();
        for call in &mut trace {
            if call.get("method") == Some(&IvyJson::from("events.publish")) {
                let mut params = call.required("params").unwrap().clone();
                let id = params
                    .required("mutationId")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone();
                parse_operation_id_js(&id).unwrap();
                let number = ids.iter().position(|v| v == &id).unwrap_or_else(|| {
                    ids.push(id);
                    ids.len() - 1
                });
                params.insert("mutationId", format!("<scoped-{number}>").into());
                call.insert("params", params);
            }
        }
        let saved = journal
            .named("secretary/browser-notice", &name, true)
            .unwrap()
            .map(|d| d.value)
            .unwrap_or(IvyJson::Null);
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            ("trace", IvyJson::Array(trace)),
            ("saved", saved),
            (
                "timeCalls",
                IvyJson::Number(clock.calls.load(Ordering::SeqCst) as f64),
            ),
            (
                "remaining",
                IvyJson::Number(client.steps.lock().unwrap().len() as f64),
            ),
        ]);
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "browser notice case {index}"
        );
    }
}
