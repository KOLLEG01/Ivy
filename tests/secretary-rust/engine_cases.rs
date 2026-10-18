use super::*;
use ivy_contracts::json::canonical;
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    engine::{SecretaryEngine, validate_settings},
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;
use std::cell::RefCell;

struct FixedClock(i64);
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
#[tokio::test]
async fn original_engine_initialization_settings_queries_and_status() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/engine-oracle.mjs")
        .current_dir(root);
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
    assert_eq!(rows.len(), 108);
    for row in rows {
        let input = row.required("input").unwrap();
        let label =
            String::from_utf16_lossy(&input.required("label").unwrap().as_js_string().unwrap().0);
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
            trace: Mutex::default(),
        });
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        for local in input.required("locals").unwrap().as_array().unwrap() {
            let id = local.required("id").unwrap().as_str().unwrap();
            let encoded = canonical(local.required("value").unwrap(), 1_048_576).unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![
                        id,
                        local.required("key").unwrap().as_str().unwrap(),
                        local.required("name").unwrap().as_str().unwrap(),
                        1,
                        0,
                        Option::<i64>::None,
                        encoded
                    ],
                )
                .unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO revisions VALUES(?,?,?)",
                    params![id, 1, encoded],
                )
                .unwrap();
        }
        let store = Store {
            client: client.clone(),
            root: corpus
                .required("rootId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            journal: &journal,
        };
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..RequestOptions::default()
        };
        if input.get("cancelled") == Some(&IvyJson::Bool(true)) {
            cancellation.cancellation.cancel();
        }
        let node = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: input.required("settings").unwrap(),
            service_node_id: &node,
            cancellation,
        };
        let clock = FixedClock(input.required("time").unwrap().as_f64().unwrap() as i64);
        let tools = ToolClients::new(client.clone());
        let issues = RefCell::new(RecoveryScan {
            issues: input
                .required("issues")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|pair| {
                    let pair = pair.as_array().unwrap();
                    (pair[0].as_str().unwrap(), pair[1].as_str().unwrap())
                })
                .collect(),
            ..RecoveryScan::default()
        });
        let runtime_epoch = epoch_fixture(&"fixture-epoch".into());
        let engine = SecretaryEngine {
            evidence: &evidence,
            clock: &clock,
            operation_clock: &clock,
            runtime_epoch,
            new_uuid: ivy_secretary::engine::random_uuid,
            tools: &tools,
            issues: &issues,
        };
        let args = input.required("args").unwrap().as_array().unwrap();
        let mut results = Vec::new();
        for _ in 0..input.required("repeats").unwrap().as_f64().unwrap() as usize {
            let result = match input.required("method").unwrap().as_str().unwrap().as_str() {
                "validateSettings" => validate_settings(evidence.settings).map(|_| IvyJson::Null),
                "initialize" => engine.initialize().await.map(|_| IvyJson::Null),
                "listAssignments" => engine.list_assignments(&args[0], &args[1]).await,
                "getAssignment" => engine.get_assignment(&args[0], &args[1]).await,
                "noticeTarget" => engine.notice_target().await,
                "status" => engine.status(&args[0], &args[1]).await,
                method => panic!("Unexpected {method}"),
            };
            results.push(match result {
                Ok(value) => IvyJson::object([("value", value)]),
                Err(error) => IvyJson::object([
                    ("code", error.code.into()),
                    ("message", error.message.into()),
                    ("outcome", error.outcome.as_str().into()),
                ]),
            });
        }
        let mut statement = journal
            .db
            .prepare("SELECT key,name,current_revision,value_json FROM documents ORDER BY key,name")
            .unwrap();
        let retained = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .map(|r| {
                let (key, name, revision, value) = r.unwrap();
                IvyJson::object([
                    ("key", key.into()),
                    ("name", name.into()),
                    ("revision", IvyJson::Number(revision as f64)),
                    ("value", parse(&value, 1_048_576).unwrap()),
                ])
            })
            .collect();
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Unconsumed RPC: {label}"
        );
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            (
                "trace",
                IvyJson::Array(client.trace.lock().unwrap().clone()),
            ),
            ("retained", IvyJson::Array(retained)),
        ]);
        assert_eq!(
            canonical(&actual, 8 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 8 * 1024 * 1024).unwrap(),
            "case {label}"
        );
    }
}
