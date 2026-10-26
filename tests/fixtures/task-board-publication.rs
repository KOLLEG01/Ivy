#[path = "reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::{digest, hash_json},
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    operation_ids::scoped_operation_id,
    rpc::{RequestFuture, RequestOptions, RpcClient},
};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    category_projection::CategoryLocale,
    journal_store::{Contracts, JournalStore},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
};
use std::{
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

#[path = "task-board-attachment-recovery.rs"]
mod attachment_recovery;

fn text(value: &Json, key: &str) -> String {
    value.required(key).unwrap().as_str().unwrap()
}
fn string(value: &Json, key: &str) -> JsString {
    value.required(key).unwrap().as_js_string().unwrap().clone()
}
struct FixtureContracts;
struct RemoteOnly;
impl CategoryLocale for RemoteOnly {
    fn lowercase_default(&self, _: &JsString) -> IvyResult<JsString> {
        panic!("Remote queries do not select a local locale")
    }
    fn compare_default(&self, _: &JsString, _: &JsString) -> std::cmp::Ordering {
        panic!("Remote queries do not select a local locale")
    }
}
impl Contracts for FixtureContracts {
    fn version(&self, key: &str) -> IvyResult<String> {
        assert_eq!(key, "task-board/attachment");
        Ok("1.0.0".into())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        assert_eq!(key, "task-board/task");
        Ok(false)
    }
    fn readable_version(&self, _: &str, _: &JsString) -> IvyResult<bool> {
        unreachable!()
    }
    fn validate_record(&self, _: &str, _: &JsString, _: &Json) -> IvyResult<()> {
        unreachable!()
    }
    fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
        unreachable!()
    }
}
struct Client {
    input: Json,
    trace: Mutex<Vec<Json>>,
    identities: Mutex<Vec<JsString>>,
    epoch_reads: Mutex<usize>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        mut params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "system.status" {
                *self.epoch_reads.lock().unwrap() += 1;
                return Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )]));
            }
            if let Some(content) = params.get("content") {
                let encoded = content.required("value")?.as_str()?;
                let mut content = content.clone();
                content.insert(
                    "value",
                    Json::object([
                        ("base64Length", Json::Number(encoded.len() as f64)),
                        ("base64Hash", digest(encoded.as_bytes()).into()),
                    ]),
                );
                params.insert("content", content);
                self.identities
                    .lock()
                    .unwrap()
                    .push(string(&params, "mutationId"));
                params.insert("mutationId", "retained-identity".into());
            }
            self.trace
                .lock()
                .unwrap()
                .push(Json::Array(vec![method.into(), params]));
            if let Some(error) = self.input.get("error") {
                return Err(
                    IvyError::new(text(error, "code"), "Fixture publication failure.")
                        .with_outcome(if error.get("outcome") == Some(&Json::from("unknown")) {
                            Outcome::Unknown
                        } else {
                            Outcome::NotExecuted
                        }),
                );
            }
            Ok(if text(&self.input, "kind") == "attachment" {
                Json::object([
                    ("object", Json::object([("id", "binary".into())])),
                    ("revision", Json::object([("revision", Json::Number(1.0))])),
                ])
            } else {
                self.input.get("reply").cloned().unwrap_or_else(|| {
                    Json::object([("items", Json::Array(vec![])), ("nextCursor", Json::Null)])
                })
            })
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn attachment_and_remote_query_publication_match_original_scope_bytes_and_errors() {
    let bytes = if let Ok(path) = std::env::var("IVY_TASK_BOARD_PUBLICATION_FIXTURE") {
        std::fs::read(path).unwrap()
    } else {
        let output = reference_data::read(include_bytes!("task-board-publication-cases.json.gz"));
        output
    };
    let cases = parse(std::str::from_utf8(&bytes).unwrap(), MANAGEMENT_FRAME_BYTES).unwrap();
    verify_publication_cases(&cases).await;
}

#[tokio::test(flavor = "current_thread")]
async fn current_remote_queries_distinguish_all_versions_from_bundle_and_explicit_filters() {
    let cases = parse(
        include_str!("task-board-remote-pages.json"),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    verify_publication_cases(&cases).await;
}

async fn verify_publication_cases(cases: &Json) {
    for case in cases.as_array().unwrap() {
        let i = case.required("input").unwrap();
        // The pre-recovery oracle's one-call conflict expectation is superseded
        // by the frozen recovery producer and identity controls in the child module.
        if text(i, "kind") == "attachment"
            && i.get("error").and_then(|error| error.get("code"))
                == Some(&Json::from("revision_conflict"))
        {
            continue;
        }
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        let client = Arc::new(Client {
            input: i.clone(),
            trace: Mutex::default(),
            identities: Mutex::default(),
            epoch_reads: Mutex::new(0),
        });
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: i.get("root").and_then(Json::as_js_string).cloned(),
            },
            client: client.clone(),
            contracts: FixtureContracts,
            limits: HistoryLimits::default(),
            now: || panic!("Unexpected clock"),
            new_id: || panic!("Unexpected UUID"),
            cache: None,
        };
        let mut outcomes = vec![];
        let mut scopes = vec![];
        let attachment = text(i, "kind") == "attachment";
        for _ in 0..i.get("repeat").and_then(Json::as_f64).unwrap_or(1.0) as usize {
            let result = if attachment {
                let count = i.required("length").unwrap().as_f64().unwrap() as usize;
                let bytes = (0..count)
                    .map(|index| (index.wrapping_mul(37).wrapping_add(11) & 255) as u8)
                    .collect::<Vec<_>>();
                store
                    .write_attachment(
                        &string(i, "taskId"),
                        &string(i, "operationId"),
                        &string(i, "attachmentId"),
                        &bytes,
                    )
                    .await
            } else {
                let versions = i
                    .required("versions")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_js_string().unwrap().clone())
                    .collect::<Vec<_>>();
                store
                    .page(
                        &text(i, "key"),
                        i.required("options").unwrap(),
                        &versions,
                        &RemoteOnly,
                    )
                    .await
            };
            outcomes.push(match result {
                Ok(value) => value,
                Err(error) => Json::object([(
                    "error",
                    Json::object([
                        ("code", Json::String(error.code_js())),
                        ("message", Json::String(error.message_js())),
                        ("outcome", error.outcome.to_json()),
                    ]),
                )]),
            });
        }
        if attachment && i.required("length").unwrap().as_f64().unwrap() <= (8 * 1024 * 1024) as f64
        {
            let scope = Json::Array(vec![
                "task-board-attachment".into(),
                Json::String(string(i, "operationId")),
            ]);
            let expected = scoped_operation_id(client.clone(), &scope).await.unwrap();
            for identity in client.identities.lock().unwrap().iter() {
                assert_eq!(
                    identity, &expected,
                    "Original retained scope must select the exact SDK mutation identity"
                );
                scopes.push(scope.clone());
            }
            assert_eq!(*client.epoch_reads.lock().unwrap(), 1);
        } else {
            assert_eq!(*client.epoch_reads.lock().unwrap(), 0);
        }
        let actual = Json::object([
            ("outcomes", Json::Array(outcomes)),
            ("trace", Json::Array(client.trace.lock().unwrap().clone())),
            ("scopes", Json::Array(scopes)),
        ]);
        if let Some(expected) = case.get("expectedHash") {
            assert_eq!(
                hash_json(&actual).unwrap(),
                expected.as_str().unwrap(),
                "{}",
                text(case, "name")
            );
        } else {
            assert_eq!(
                canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
                canonical(case.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
                "{}",
                text(case, "name")
            );
        }
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
