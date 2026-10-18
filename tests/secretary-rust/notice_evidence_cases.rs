use super::*;
use ivy_secretary::{
    inbox_evidence::InboxEvidence,
    journal::{Journal, Limits},
    notice_evidence::{NoticeMethod, NoticeProvider, notice_evidence},
    store::Store,
};

#[tokio::test]
async fn original_notice_proof_and_bound_provider_calls() {
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/notice-evidence-oracle.mjs")
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
    assert_eq!(rows.len(), 55);
    for row in rows {
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
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
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
        let settings = corpus.required("settings").unwrap();
        let node = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings,
            service_node_id: &node,
            cancellation: RequestOptions::default(),
        };
        let tools = ToolClients::new(client.clone());
        let result = if input.get("kind") == Some(&"evidence".into()) {
            notice_evidence(
                client.as_ref(),
                settings
                    .required("identity")
                    .unwrap()
                    .required("principalId")
                    .unwrap(),
                input.required("reply").unwrap(),
                input.required("notice").unwrap(),
            )
            .await
            .map(|_| IvyJson::Null)
        } else {
            let method = match input.required("method").unwrap().as_str().unwrap().as_str() {
                "notice" => NoticeMethod::Notice,
                "notify" => NoticeMethod::Notify,
                _ => NoticeMethod::Operation,
            };
            NoticeProvider {
                evidence: &evidence,
                tools: &tools,
            }
            .call(
                method,
                input.required("target").unwrap(),
                input.required("args").unwrap().clone(),
                input
                    .get("operationId")
                    .and_then(IvyJson::as_js_string)
                    .cloned(),
            )
            .await
        };
        let result = match result {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(e) => IvyJson::object([
                ("code", e.code.into()),
                ("message", e.message.into()),
                ("outcome", e.outcome.as_str().into()),
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
            "{}",
            input.required("label").unwrap().as_str().unwrap()
        );
    }
}
