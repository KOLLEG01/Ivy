use super::*;
use ivy_contracts::json::canonical;
use ivy_secretary::{
    assignment_native::AssignmentNative,
    inbox_evidence::InboxEvidence,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;
use std::sync::atomic::{AtomicUsize, Ordering};

struct NativeClient {
    scripted: Scripted,
    options: RequestOptions,
    cancel_after: Option<usize>,
    requests: AtomicUsize,
}
impl RpcClient for NativeClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let result = self.scripted.request(method, params, options).await;
            let count = self.requests.fetch_add(1, Ordering::Relaxed) + 1;
            if self.cancel_after == Some(count) {
                self.options.cancellation.cancel();
            }
            result
        })
    }
}

#[tokio::test]
async fn original_assignment_native_admission_and_recovery() {
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-native-oracle.mjs")
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
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 43);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        let id = input
            .required("operationId")
            .unwrap()
            .as_js_string()
            .unwrap();
        if let Some(retained) = input.get("retained").filter(|v| !v.is_null()) {
            let encoded = canonical(retained, 1_048_576).unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                    params![
                        "retained",
                        "secretary/assignment-native-call",
                        id.to_utf8().unwrap(),
                        1,
                        encoded
                    ],
                )
                .unwrap();
            journal
                .db
                .execute(
                    "INSERT INTO technical_revisions VALUES(?,?,?)",
                    params!["retained", 1, encoded],
                )
                .unwrap();
        }
        let options = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..RequestOptions::default()
        };
        if input.get("cancelBefore") == Some(&IvyJson::Bool(true)) {
            options.cancellation.cancel();
        }
        let client = Arc::new(NativeClient {
            scripted: Scripted {
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
            },
            options: options.clone(),
            cancel_after: input
                .get("cancelAfter")
                .and_then(IvyJson::as_f64)
                .map(|n| n as usize),
            requests: AtomicUsize::new(0),
        });
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
        let service_node_id = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: corpus.required("settings").unwrap(),
            service_node_id: &service_node_id,
            cancellation: options,
        };
        let native = AssignmentNative {
            evidence: &evidence,
            tools: Arc::new(ToolClients::new(client.clone())),
        };
        let outcome = match native.owner(input.required("execution").unwrap()) {
            Ok(owner) => {
                native
                    .completed(
                        &owner,
                        id,
                        input.required("method").unwrap().as_js_string().unwrap(),
                        input.required("params").unwrap(),
                    )
                    .await
            }
            Err(e) => Err(e),
        };
        let result = match outcome {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(e) => IvyJson::object([
                ("code", e.code.into()),
                ("message", e.message.into()),
                ("outcome", e.outcome.as_str().into()),
            ]),
        };
        let saved = journal
            .named(
                "secretary/assignment-native-call",
                &id.to_utf8().unwrap(),
                true,
            )
            .unwrap()
            .map_or(IvyJson::Null, |d| {
                IvyJson::object([
                    ("name", d.name.into()),
                    ("revision", IvyJson::Number(d.pin.revision as f64)),
                    ("value", d.value),
                ])
            });
        let actual = IvyJson::object([
            ("result", result),
            (
                "trace",
                IvyJson::Array(client.scripted.trace.lock().unwrap().clone()),
            ),
            ("saved", saved),
        ]);
        assert!(
            client.scripted.steps.lock().unwrap().is_empty(),
            "Case {index}"
        );
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "Case {index}: {:?}",
            input.get("label")
        );
    }
}
