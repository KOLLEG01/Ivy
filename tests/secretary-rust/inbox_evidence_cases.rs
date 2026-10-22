use super::*;
use ivy_contracts::{hash::hash_json, json::canonical};
use ivy_secretary::{
    inbox_evidence::*,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;

#[tokio::test]
async fn original_inbox_identities_and_retained_evidence() {
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/inbox-evidence-oracle.mjs")
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
        16 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert!(rows.len() > 100);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let method = input.required("method").unwrap().as_str().unwrap();
        let args = input.required("args").unwrap().as_array().unwrap();
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
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        for local in input.required("locals").unwrap().as_array().unwrap() {
            let id = local.required("id").unwrap().as_str().unwrap();
            let value = local.required("value").unwrap();
            let first = local.get("first").unwrap_or(value);
            let revision = if hash_json(first).unwrap() == hash_json(value).unwrap() {
                1
            } else {
                2
            };
            let encoded = canonical(value, 1024 * 1024).unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![
                        id,
                        local.required("key").unwrap().as_str().unwrap(),
                        local.required("name").unwrap().as_str().unwrap(),
                        revision,
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
                    params![id, 1, canonical(first, 1024 * 1024).unwrap()],
                )
                .unwrap();
            if revision == 2 {
                journal
                    .db
                    .execute(
                        "INSERT INTO revisions VALUES(?,?,?)",
                        params![id, 2, encoded],
                    )
                    .unwrap();
            }
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
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..Default::default()
        };
        if input.get("cancelled").and_then(IvyJson::as_bool) == Some(true) {
            cancellation.cancellation.cancel();
        }
        let service_node_id = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: input.required("settings").unwrap(),
            service_node_id: &service_node_id,
            cancellation,
        };
        let result = match method.as_str() {
            "operationName" => operation_name(&args[0], &args[1]).map(IvyJson::String),
            "sourceName" => source_name(&args[0]).map(IvyJson::String),
            "identityHash" => identity_hash(&args[0], &args[1]).map(IvyJson::from),
            "messageHash" => message_hash(&args[0]).map(IvyJson::from),
            "sourceIdentity" => source_identity(&args[0]),
            "noticeText" => notice_text(&args[0], &args[1], &args[2]).map(IvyJson::String),
            "sourceDefinition" => evidence.source_definition(&args[0]).cloned(),
            "authorize" => evidence
                .authorize(&args[0], &args[1], &args[2].as_str().unwrap(), args.get(3))
                .map(|_| IvyJson::Null),
            "verifyOwner" => evidence.verify_owner().await.map(|_| IvyJson::Null),
            "source" => evidence.source(&args[0]).await,
            "item" => evidence.item(&args[0]).await,
            "checked" => evidence.checked(&args[0]).await,
            "find" => evidence
                .find(&args[0], &args[1])
                .await
                .map(|v| v.unwrap_or(IvyJson::Null)),
            "operation" => evidence.operation(&args[0], &args[1], &args[2]).await,
            _ => panic!("unknown method {method}"),
        };
        let result = match result {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(error) => IvyJson::object([
                ("code", error.code.into()),
                ("message", error.message.into()),
                ("outcome", error.outcome.to_json()),
            ]),
        };
        let actual = IvyJson::object([
            ("result", result),
            (
                "trace",
                IvyJson::Array(client.trace.lock().unwrap().clone()),
            ),
            (
                "remaining",
                IvyJson::Number(client.steps.lock().unwrap().len() as f64),
            ),
        ]);
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "inbox evidence case {index}: {method}"
        );
    }
}
