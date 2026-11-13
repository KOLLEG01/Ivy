#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::{json::canonical, operation::parse_operation_id_js};
use ivy_secretary::{
    journal::{Journal, Limits},
    store::{Record, Store, conflict, mutation},
};
use ivy_store::rusqlite::params;

fn record(value: &IvyJson) -> Record {
    match value.as_str().unwrap().as_str() {
        "secretary/item" => Record::Item,
        "secretary/source" => Record::Source,
        "secretary/operation" => Record::Operation,
        "secretary/configuration" => Record::Configuration,
        "secretary/assignment" => Record::Assignment,
        "secretary/schedule-progress" => Record::ScheduleProgress,
        "secretary/event-progress" => Record::EventProgress,
        "secretary/execution" => Record::Execution,
        _ => panic!("Unknown record"),
    }
}
#[test]
fn mutation_parent_is_explicit_and_never_selected_from_user_data() {
    let parent: ivy_contracts::JsString = "owner:1800000000000:original".into();
    let parts = vec![
        "foreign:1900000000000:decoy".into(),
        IvyJson::object([
            ("text", "nested:1900000000001:another".into()),
            (
                "original",
                IvyJson::String(ivy_contracts::JsString(vec![0xd800, 0, 0xdc00])),
            ),
        ]),
    ];
    let derived = mutation(Some(&parent), parts.clone()).unwrap();
    let identity = parse_operation_id_js(&derived).unwrap();
    assert_eq!(identity.runtime_epoch, "owner".into());
    assert_eq!(identity.issued_at_unix_ms, 1_800_000_000_000);
    assert!(
        mutation(None, parts.clone())
            .unwrap()
            .to_utf8()
            .unwrap()
            .starts_with("secretary:")
    );
    assert_eq!(
        mutation(Some(&"invalid-parent".into()), parts)
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
}
#[tokio::test]
async fn original_store_records_and_rpc_recovery() {
    let output = reference_data::read(include_bytes!("store-cases.json.gz"));
    let corpus = parse(std::str::from_utf8(&output).unwrap(), 16 * 1024 * 1024).unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert!(rows.len() > 200);
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let args = input.required("args").unwrap().as_array().unwrap();
        let method = input.required("method").unwrap().as_str().unwrap();
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
            let encoded = canonical(local.required("value").unwrap(), 1024 * 1024).unwrap();
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
                .required("root")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            journal: &journal,
        };
        let result = match method.as_str() {
            "decode" => store.decode(record(&args[0]), args[1].clone()),
            "objectWriteRequest" => store.object_write_request(
                record(&args[0]),
                &args[1],
                &args[2],
                args[3].as_js_string().unwrap(),
            ),
            "read" => store.read(record(&args[0]), &args[1]).await,
            "named" => store
                .named(record(&args[0]), args[1].as_js_string().unwrap())
                .await
                .map(|v| v.unwrap_or(IvyJson::Null)),
            "create" => {
                store
                    .create(
                        record(&args[0]),
                        args[1].as_js_string().unwrap(),
                        &args[2],
                        args[3].as_js_string().unwrap(),
                    )
                    .await
            }
            "amend" => store.amend(&args[0], record(&args[1]), &args[2]).await,
            "original" => store.original(record(&args[0]), &args[1]).await,
            "mutation" => mutation(
                input.get("parent").and_then(IvyJson::as_js_string),
                args.to_vec(),
            )
            .map(IvyJson::String),
            "conflict" => {
                let value = &args[0];
                let outcome = match value
                    .required("outcome")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .as_str()
                {
                    "unknown" => Outcome::Unknown,
                    "completed" => Outcome::Completed,
                    _ => Outcome::NotExecuted,
                };
                Ok(conflict(
                    &IvyError::new(
                        value.required("code").unwrap().as_str().unwrap(),
                        "Fixture failure.",
                    )
                    .with_outcome(outcome),
                )
                .into())
            }
            _ => panic!("Unknown method {method}"),
        };
        let value = match result {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(error) => IvyJson::object([
                ("code", error.code.into()),
                ("message", error.message.into()),
                ("outcome", error.outcome.to_json()),
            ]),
        };
        let mut trace = client.trace.lock().unwrap().clone();
        if method == "amend" {
            for call in &mut trace {
                if call.get("method") == Some(&IvyJson::from("objects.write")) {
                    let mut params = call.required("params").unwrap().clone();
                    parse_operation_id_js(
                        params
                            .required("mutationId")
                            .unwrap()
                            .as_js_string()
                            .unwrap(),
                    )
                    .unwrap();
                    params.insert("mutationId", "<scoped-operation>".into());
                    call.insert("params", params);
                }
            }
        }
        let actual = IvyJson::object([
            ("result", value),
            ("trace", IvyJson::Array(trace)),
            (
                "remaining",
                IvyJson::Number(client.steps.lock().unwrap().len() as f64),
            ),
        ]);
        assert_eq!(
            &actual,
            row.required("expected").unwrap(),
            "store case {index} {method}"
        );
    }
}
