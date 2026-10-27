//! Exercise the active publication Store against real object bounds and schemas.
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::{digest, hash_json},
    json::{canonical, parse},
    limits::{JSON_OBJECT_CONTENT_BYTES, MANAGEMENT_FRAME_BYTES},
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    task_comments::RpcCommentSource,
};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    journal_store::{Contracts, JournalStore, persistent_reads::task_comments::TaskComments},
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    operations::Store,
    runtime_inputs,
    schema::RuntimeContracts,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const TIME: &str = "2026-01-02T03:04:05.006Z";
fn json(text: &str) -> Json {
    parse(text, MANAGEMENT_FRAME_BYTES).unwrap()
}
fn record(id: &str, key: &str, value: Json, revision: f64, references: Json) -> Json {
    Json::object([
        (
            "object",
            Json::object([
                ("id", id.into()),
                ("contractKey", key.into()),
                (
                    "parentId",
                    if key == "task-board/task" {
                        Json::Null
                    } else {
                        "task".into()
                    },
                ),
                (
                    "ownerObjectId",
                    if key == "task-board/task" {
                        Json::Null
                    } else {
                        "task".into()
                    },
                ),
                ("name", id.into()),
                ("effectivelyArchived", false.into()),
                ("currentRevision", Json::Number(revision)),
            ]),
        ),
        (
            "revision",
            Json::object([
                ("revision", Json::Number(revision)),
                (
                    "contractVersion",
                    if key == "task-board/task" {
                        "3.1.0"
                    } else {
                        "2.0.0"
                    }
                    .into(),
                ),
                ("mediaType", "application/json".into()),
                (
                    "byteLength",
                    Json::Number(canonical(&value, JSON_OBJECT_CONTENT_BYTES).unwrap().len() as f64),
                ),
                ("contentHash", hash_json(&value).unwrap().into()),
                ("references", references),
            ]),
        ),
        (
            "content",
            Json::object([("encoding", "json".into()), ("value", value)]),
        ),
    ])
}
fn comment(sequence: usize, body: String) -> Json {
    let mut value = json(
        r#"{"commentId":"new","sequence":1,"author":{"principalId":"caller","serviceNodeId":"fixture.task-board","generation":1,"source":"user"},"authorKind":"user","purpose":"update","body":"","requests":[],"responses":[],"delivery":null,"attachments":[],"run":null,"turnId":null,"replyTo":null,"createdAt":"2026-01-02T03:04:05.006Z","operationId":"short-comment"}"#,
    );
    value.insert("commentId", format!("c{sequence}").into());
    value.insert("sequence", Json::Number(sequence as f64));
    value.insert("body", body.into());
    value
}
fn generated_fixture() -> Json {
    let task = json(
        r#"{"schemaVersion":1,"taskKey":"TASK-0001","fields":{"title":"Long conversation","description":"","acceptanceCriteria":[],"category":null,"control":"user","priority":2,"executionRequirement":null,"workspaceRequirement":{"kind":"task_workspace"},"userContact":"ticket","dueAt":null},"workflowState":"waiting","publication":null,"primaryResourceRef":null,"claim":null,"attemptCount":0,"lastRun":null,"lastHistory":null,"latestResult":null,"acceptedReview":null,"comments":[],"commentDeliveries":[],"agentCommentCount":0,"workRevision":0,"attachments":[],"createdAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","wakeAt":null,"dependencies":[],"runtimeWait":null}"#,
    );
    let mut nodes = vec![];
    let mut children = vec![];
    for sequence in 1..=300 {
        let id = format!("node{sequence}");
        let node = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("taskId", "task".into()),
            ("kind", "comment".into()),
            (
                "entry",
                Json::object([
                    ("comment", comment(sequence, "retained ".repeat(600))),
                    ("receipt", Json::Null),
                ]),
            ),
        ]);
        nodes.push(record(
            &id,
            "task-board/comment-node",
            node,
            1.0,
            Json::Object(vec![]),
        ));
        children.push(Json::object([
            (
                "object",
                Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))]),
            ),
            ("length", Json::Number(1.0)),
        ]));
    }
    let mut parents = vec![];
    for (index, chunk) in children.chunks(64).enumerate() {
        let id = format!("page{index}");
        let node = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("taskId", "task".into()),
            ("kind", "page".into()),
            ("children", Json::Array(chunk.to_vec())),
        ]);
        nodes.push(record(
            &id,
            "task-board/comment-node",
            node,
            1.0,
            Json::Object(vec![]),
        ));
        parents.push(Json::object([
            (
                "object",
                Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))]),
            ),
            ("length", Json::Number(chunk.len() as f64)),
        ]));
    }
    let root = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskId", "task".into()),
        ("kind", "page".into()),
        ("children", Json::Array(parents)),
    ]);
    nodes.push(record(
        "root",
        "task-board/comment-node",
        root,
        1.0,
        Json::Object(vec![]),
    ));
    let root_pin = Json::object([("objectId", "root".into()), ("revision", Json::Number(1.0))]);
    let mut task = task;
    task.insert(
        "commentStore",
        Json::object([
            ("kind", "tree".into()),
            (
                "root",
                Json::object([
                    ("object", root_pin.clone()),
                    ("length", Json::Number(300.0)),
                ]),
            ),
            ("length", Json::Number(300.0)),
        ]),
    );
    Json::object([
        (
            "task",
            record(
                "task",
                "task-board/task",
                task,
                1.0,
                Json::object([("conversation", root_pin)]),
            ),
        ),
        ("nodes", Json::Array(nodes)),
    ])
}
#[derive(Default)]
struct State {
    records: HashMap<String, Json>,
    receipts: HashMap<String, (String, Json)>,
    writes: usize,
    maximum_bytes: usize,
    lost_reply: bool,
}
struct Hive {
    state: Mutex<State>,
    contracts: RuntimeContracts,
}
impl Hive {
    fn new(fixture: &Json) -> Self {
        let mut state = State::default();
        for row in std::iter::once(fixture.required("task").unwrap())
            .chain(fixture.required("nodes").unwrap().as_array().unwrap())
        {
            state.records.insert(
                row.required("object")
                    .unwrap()
                    .required("id")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                row.clone(),
            );
        }
        Self {
            state: Mutex::new(state),
            contracts: runtime_inputs::contracts().unwrap(),
        }
    }
}
impl RpcClient for Hive {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            Ok(match method {
                "system.status" => Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )]),
                "objects.read" => {
                    let id = params.required("objectId")?.as_str()?;
                    let value = state
                        .records
                        .get(&id)
                        .ok_or_else(|| IvyError::new("not_found", "Missing fixture object."))?;
                    if let Some(revision) = params.get("revision") {
                        assert_eq!(Some(revision), value.required("revision")?.get("revision"));
                    }
                    value.clone()
                }
                "objects.query" => {
                    let clauses = params
                        .required("where")?
                        .required("args")?
                        .as_array()
                        .unwrap();
                    let parent = clauses[0].required("value")?;
                    let name = clauses[1].required("value")?;
                    let matches = state
                        .records
                        .values()
                        .filter(|row| {
                            let object = row.required("object").unwrap();
                            object.get("contractKey") == params.get("contractKey")
                                && object.get("parentId") == Some(parent)
                                && object.get("name") == Some(name)
                        })
                        .map(|row| {
                            Json::object([
                                (
                                    "objectId",
                                    row.required("object")
                                        .unwrap()
                                        .required("id")
                                        .unwrap()
                                        .clone(),
                                ),
                                (
                                    "revision",
                                    row.required("revision")
                                        .unwrap()
                                        .required("revision")
                                        .unwrap()
                                        .clone(),
                                ),
                            ])
                        })
                        .collect();
                    Json::object([("items", Json::Array(matches)), ("nextCursor", Json::Null)])
                }
                "objects.write" => {
                    let mutation = params.required("mutationId")?.as_str()?;
                    let hash = hash_json(&params)?;
                    if let Some((saved_hash, saved)) = state.receipts.get(&mutation) {
                        assert_eq!(
                            saved_hash, &hash,
                            "A replay must retain exact prepared content."
                        );
                        return Ok(saved.clone());
                    }
                    let create = params.get("create");
                    let key = if let Some(create) = create {
                        create.required("contractKey")?.as_str()?
                    } else {
                        "task-board/task".into()
                    };
                    let value = params.required("content")?.required("value")?;
                    // These are Hive's actual JSON-content bound and the service's
                    // exact published read/write schemas, not a permissive mock.
                    let bytes = canonical(value, JSON_OBJECT_CONTENT_BYTES)?;
                    self.contracts.validate_record(
                        &key,
                        params.required("contractVersion")?.as_js_string().unwrap(),
                        value,
                    )?;
                    let mut row = if let Some(create) = create {
                        let id = format!("created{}", state.writes);
                        let mut row = record(
                            &id,
                            &key,
                            value.clone(),
                            1.0,
                            params.required("references")?.clone(),
                        );
                        let mut object = create.clone();
                        object.insert("id", id.into());
                        object.insert("effectivelyArchived", false.into());
                        object.insert("currentRevision", Json::Number(1.0));
                        row.insert("object", object);
                        row
                    } else {
                        let id = params.required("objectId")?.as_str()?;
                        let previous = state.records.get(&id).unwrap();
                        let revision = previous
                            .required("revision")?
                            .required("revision")?
                            .as_f64()
                            .unwrap();
                        assert_eq!(
                            params.get("expectedRevision"),
                            Some(&Json::Number(revision))
                        );
                        assert_eq!(params.get("expectedArchived"), Some(&Json::Bool(false)));
                        let mut row = record(
                            &id,
                            &key,
                            value.clone(),
                            revision + 1.0,
                            params.required("references")?.clone(),
                        );
                        let mut object = previous.required("object")?.clone();
                        object.insert("currentRevision", Json::Number(revision + 1.0));
                        row.insert("object", object);
                        row
                    };
                    let mut revision = row.required("revision")?.clone();
                    revision.insert(
                        "contractVersion",
                        params.required("contractVersion")?.clone(),
                    );
                    row.insert("revision", revision);
                    state.writes += 1;
                    state.maximum_bytes = state.maximum_bytes.max(bytes.len());
                    state.records.insert(
                        row.required("object")?.required("id")?.as_str()?,
                        row.clone(),
                    );
                    state.receipts.insert(mutation, (hash, row.clone()));
                    if std::mem::take(&mut state.lost_reply) {
                        return Err(IvyError::new(
                            "transport_lost",
                            "Committed write reply was lost.",
                        ));
                    }
                    row
                }
                _ => panic!("Unexpected fixture request {method}"),
            })
        })
    }
}
async fn hydrated(hive: Arc<Hive>, header: &Json, id: &JsString) -> Json {
    let reader = TaskComments::new(
        Arc::new(RpcCommentSource::new(hive)),
        tokio::runtime::Handle::current(),
    );
    let value = reader.hydrate(header, id).await.unwrap().unwrap();
    reader.settled().await;
    value
}
async fn exercise(fixture: Json) {
    let original = fixture.required("task").unwrap();
    let id = original
        .required("object")
        .unwrap()
        .required("id")
        .unwrap()
        .as_js_string()
        .unwrap();
    let hive = Arc::new(Hive::new(&fixture));
    let header = original
        .required("content")
        .unwrap()
        .required("value")
        .unwrap();
    let full = hydrated(hive.clone(), header, id).await;
    assert!(canonical(&full, MANAGEMENT_FRAME_BYTES).unwrap().len() > JSON_OBJECT_CONTENT_BYTES);
    assert_eq!(
        canonical(&full, JSON_OBJECT_CONTENT_BYTES)
            .unwrap_err()
            .code,
        "content_too_large",
        "The former direct write fails at Hive's real boundary."
    );
    let previous_comments = full.required("comments").unwrap().as_array().unwrap();
    let mut proposed = full.clone();
    let mut comments = previous_comments.to_vec();
    comments.push(comment(
        comments.len() + 1,
        "Continue the approved work.".into(),
    ));
    proposed.insert("comments", Json::Array(comments));
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let contracts = runtime_inputs::contracts().unwrap();
    let store = JournalStore {
        journal: LocalJournal {
            connection: &db,
            root_object_id: original
                .required("object")
                .unwrap()
                .get("parentId")
                .and_then(Json::as_js_string)
                .cloned(),
        },
        client: hive.clone(),
        contracts,
        limits: HistoryLimits::default(),
        now: || TIME.into(),
        new_id: || uuid::Uuid::new_v4().to_string().into(),
        cache: None,
    };
    let destination = Json::object([
        ("objectId", Json::String(id.clone())),
        (
            "expectedRevision",
            original
                .required("revision")
                .unwrap()
                .required("revision")
                .unwrap()
                .clone(),
        ),
    ]);
    hive.state.lock().unwrap().lost_reply = true;
    assert_eq!(
        store
            .write_prepared(
                "task-board/task",
                &proposed,
                &"prepared-comment".into(),
                &destination
            )
            .await
            .unwrap_err()
            .code,
        "transport_lost"
    );
    let pin = store
        .write_prepared(
            "task-board/task",
            &proposed,
            &"prepared-comment".into(),
            &destination,
        )
        .await
        .unwrap();
    let writes = hive.state.lock().unwrap().writes;
    assert_eq!(
        store
            .write_prepared(
                "task-board/task",
                &proposed,
                &"prepared-comment".into(),
                &destination
            )
            .await
            .unwrap(),
        pin
    );
    assert_eq!(
        hive.state.lock().unwrap().writes,
        writes,
        "Replay must not create another node or Task revision."
    );
    let saved = hive.state.lock().unwrap().records[&id.to_utf8().unwrap()].clone();
    let stored = saved
        .required("content")
        .unwrap()
        .required("value")
        .unwrap();
    assert!(
        stored
            .required("comments")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        stored.required("commentStore").unwrap().get("length"),
        Some(&Json::Number((previous_comments.len() + 1) as f64))
    );
    assert_eq!(
        saved
            .required("revision")
            .unwrap()
            .required("references")
            .unwrap()
            .get("conversation"),
        stored
            .required("commentStore")
            .unwrap()
            .required("root")
            .unwrap()
            .get("object")
    );
    let roundtrip = hydrated(hive.clone(), stored, id).await;
    assert_eq!(
        roundtrip.required("comments").unwrap().as_array().unwrap(),
        proposed.required("comments").unwrap().as_array().unwrap()
    );
    assert_eq!(
        roundtrip.required("commentDeliveries").unwrap(),
        full.required("commentDeliveries").unwrap()
    );
    for (key, value) in full.as_object().unwrap() {
        if !key.eq_str("comments") {
            assert_eq!(
                roundtrip.get_key(key),
                Some(value),
                "Original task field must remain unchanged."
            );
        }
    }
    assert!(hive.state.lock().unwrap().maximum_bytes <= JSON_OBJECT_CONTENT_BYTES);
    let mut too_large = proposed.clone();
    let mut fields = too_large.required("fields").unwrap().clone();
    fields.insert("description", "x".repeat(JSON_OBJECT_CONTENT_BYTES).into());
    too_large.insert("fields", fields);
    assert_eq!(
        store
            .write_prepared(
                "task-board/task",
                &too_large,
                &"oversized-header".into(),
                &destination
            )
            .await
            .unwrap_err()
            .code,
        "content_too_large"
    );
    assert_eq!(
        hive.state.lock().unwrap().writes,
        writes,
        "Oversized headers fail before writes."
    );
    // Read-only fixture hashes are retained without exposing ticket text in logs.
    println!(
        "large Task history: {} comments, input {}, all persisted objects <= {} bytes",
        previous_comments.len(),
        digest(canonical(&full, MANAGEMENT_FRAME_BYTES).unwrap()),
        JSON_OBJECT_CONTENT_BYTES
    );
}
#[tokio::test(flavor = "current_thread")]
async fn prepared_short_comment_preserves_large_conversation_and_replays_exactly() {
    exercise(generated_fixture()).await;
}
#[tokio::test(flavor = "current_thread")]
async fn copied_task_histories_use_the_same_active_store_and_registered_bounds() {
    let Some(paths) = std::env::var_os("IVY_TASK_BOARD_LARGE_TASK_FIXTURES") else {
        return;
    };
    for path in std::env::split_paths(&paths) {
        exercise(
            parse(
                &std::fs::read_to_string(path).unwrap(),
                MANAGEMENT_FRAME_BYTES,
            )
            .unwrap(),
        )
        .await;
    }
}
