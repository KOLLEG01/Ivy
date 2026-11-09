//! Exercise the active publication Store against real object bounds and schemas.
#[path = "task-board-admission-writes.rs"]
mod admission_writes;
#[path = "task-board-consumer-reads.rs"]
mod consumer_reads;
#[path = "task-board-native-comment-reads.rs"]
mod native_comment_reads;
#[path = "task-board-native-header-reads.rs"]
mod native_headers;
#[path = "task-board-receipt-writes.rs"]
mod receipts;
#[path = "task-board-response-writes.rs"]
mod responses;
#[path = "task-board-retarget-writes.rs"]
mod retarget;
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
    native_lifecycle::LifecycleStore,
    operations::{Operations, Store, TaskChange},
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
    conversation_fixture(300, 600)
}
fn conversation_fixture(length: usize, repeats: usize) -> Json {
    let task = json(
        r#"{"schemaVersion":1,"taskKey":"TASK-0001","fields":{"title":"Long conversation","description":"","acceptanceCriteria":[],"category":null,"control":"user","priority":2,"executionRequirement":null,"workspaceRequirement":{"kind":"task_workspace"},"userContact":"ticket","dueAt":null},"workflowState":"waiting","publication":null,"primaryResourceRef":null,"claim":null,"attemptCount":0,"lastRun":null,"lastHistory":null,"latestResult":null,"acceptedReview":null,"comments":[],"commentDeliveries":[],"agentCommentCount":0,"workRevision":0,"attachments":[],"createdAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","wakeAt":null,"dependencies":[],"runtimeWait":null}"#,
    );
    let mut nodes = vec![];
    let mut children = vec![];
    for sequence in 1..=length {
        let id = format!("node{sequence}");
        let node = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("taskId", "task".into()),
            ("kind", "comment".into()),
            (
                "entry",
                Json::object([
                    ("comment", comment(sequence, "retained ".repeat(repeats))),
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
    let mut depth = 0;
    while parents.len() > 64 {
        let mut next = Vec::new();
        for (index, children) in parents.chunks(64).enumerate() {
            let id = format!("level{depth}-{index}");
            let total = children
                .iter()
                .map(|c| c.required("length").unwrap().as_f64().unwrap())
                .sum();
            nodes.push(record(
                &id,
                "task-board/comment-node",
                Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("taskId", "task".into()),
                    ("kind", "page".into()),
                    ("children", Json::Array(children.to_vec())),
                ]),
                1.0,
                Json::Object(vec![]),
            ));
            next.push(Json::object([
                (
                    "object",
                    Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))]),
                ),
                ("length", Json::Number(total)),
            ]));
        }
        parents = next;
        depth += 1;
    }
    let root = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskId", "task".into()),
        ("kind", "page".into()),
        ("children", Json::Array(parents)),
    ]);
    if length > 0 {
        nodes.push(record(
            "root",
            "task-board/comment-node",
            root,
            1.0,
            Json::Object(vec![]),
        ));
    }
    let root_pin = Json::object([("objectId", "root".into()), ("revision", Json::Number(1.0))]);
    let mut task = task;
    task.insert(
        "commentStore",
        Json::object([
            ("kind", "tree".into()),
            (
                "root",
                if length == 0 {
                    Json::Null
                } else {
                    Json::object([
                        ("object", root_pin.clone()),
                        ("length", Json::Number(length as f64)),
                    ])
                },
            ),
            ("length", Json::Number(length as f64)),
            (
                "summary",
                Json::object([
                    ("directCount", Json::Number(length as f64)),
                    ("questions", Json::Array(vec![])),
                ]),
            ),
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
                if length == 0 {
                    Json::Object(vec![])
                } else {
                    Json::object([("conversation", root_pin)])
                },
            ),
        ),
        ("nodes", Json::Array(nodes)),
    ])
}
#[derive(Default)]
struct State {
    records: HashMap<String, Json>,
    revisions: HashMap<(String, u64), Json>,
    receipts: HashMap<String, (String, Json)>,
    writes: usize,
    maximum_bytes: usize,
    lost_reply: bool,
    lost_task_reply: bool,
    comment_reads: usize,
    task_writes: usize,
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
            state.revisions.insert(
                (
                    row.required("object")
                        .unwrap()
                        .required("id")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                    row.required("revision")
                        .unwrap()
                        .required("revision")
                        .unwrap()
                        .as_f64()
                        .unwrap() as u64,
                ),
                row.clone(),
            );
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
                "serviceNodes.get" => {
                    return Err(IvyError::new(
                        "preflight_reached",
                        "No native dispatch in this fixture.",
                    ));
                }
                "objects.read" => {
                    let id = params.required("objectId")?.as_str()?;
                    if state
                        .records
                        .get(&id)
                        .and_then(|row| row.get("object"))
                        .and_then(|object| object.get("contractKey"))
                        == Some(&Json::from("task-board/comment-node"))
                    {
                        state.comment_reads += 1;
                    }
                    let latest = state.records.get(&id).ok_or_else(|| {
                        IvyError::new("not_found", format!("Missing fixture object {id}."))
                    })?;
                    let mut value = if let Some(revision) =
                        params.get("revision").filter(|revision| {
                            Some(*revision)
                                != latest
                                    .required("revision")
                                    .ok()
                                    .and_then(|v| v.get("revision"))
                        }) {
                        state
                            .revisions
                            .get(&(id.clone(), revision.as_f64().unwrap() as u64))
                            .expect("Exact fixture revision must be retained")
                            .clone()
                    } else {
                        latest.clone()
                    };
                    value.insert("object", latest.required("object")?.clone());
                    value
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
                        let id = params.required("objectId")?.as_str()?;
                        state.records[&id]
                            .required("object")?
                            .required("contractKey")?
                            .as_str()?
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
                        if key == "task-board/task" {
                            assert!(params.get("expectedArchived").is_some());
                            if params.get("expectedArchived")
                                != previous.required("object")?.get("effectivelyArchived")
                            {
                                return Err(IvyError::new(
                                    "revision_conflict",
                                    "Archive membership changed before publication.",
                                ));
                            }
                        } else if let Some(expected) = params.get("expectedArchived") {
                            assert_eq!(
                                previous.required("object")?.get("effectivelyArchived"),
                                Some(expected)
                            );
                        }
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
                    if key == "task-board/task" {
                        state.task_writes += 1;
                    }
                    state.maximum_bytes = state.maximum_bytes.max(bytes.len());
                    state.revisions.insert(
                        (
                            row.required("object")?.required("id")?.as_str()?,
                            row.required("revision")?
                                .required("revision")?
                                .as_f64()
                                .unwrap() as u64,
                        ),
                        row.clone(),
                    );
                    state.records.insert(
                        row.required("object")?.required("id")?.as_str()?,
                        row.clone(),
                    );
                    state.receipts.insert(mutation, (hash, row.clone()));
                    if std::mem::take(&mut state.lost_reply)
                        || key == "task-board/task" && std::mem::take(&mut state.lost_task_reply)
                    {
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
    let appended = comment(comments.len() + 1, "Continue the approved work.".into());
    comments.push(appended.clone());
    proposed.insert("comments", Json::Array(comments));
    let mut receipts = full
        .required("commentDeliveries")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    receipts.push(Json::object([
        ("commentId", appended.required("commentId").unwrap().clone()),
        ("state", "queued".into()),
        (
            "operationId",
            appended.required("operationId").unwrap().clone(),
        ),
        ("updatedAt", TIME.into()),
        ("code", Json::Null),
    ]));
    proposed.insert("commentDeliveries", Json::Array(receipts));
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
    if previous_comments.iter().any(|comment| {
        !comment
            .required("attachments")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    }) {
        hive.state.lock().unwrap().lost_task_reply = true;
    } else {
        hive.state.lock().unwrap().lost_reply = true;
    }
    assert_eq!(
        store
            .write_prepared(
                "task-board/task",
                &proposed,
                &"prepared-comment".into(),
                &destination,
                None,
                None
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
            None,
            None,
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
                &destination,
                None,
                None
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
    let domain = Store::read(&store, "task-board/task", &Json::String(id.clone()), true)
        .await
        .unwrap();
    store
        .contracts
        .validate_definition("Task", &domain.value)
        .unwrap();
    assert!(ivy_task_board::wake::comments_pending(&domain.value));
    let roundtrip = domain.value;
    assert_eq!(
        roundtrip.required("comments").unwrap().as_array().unwrap(),
        proposed.required("comments").unwrap().as_array().unwrap()
    );
    assert_eq!(
        roundtrip.required("commentDeliveries").unwrap(),
        proposed.required("commentDeliveries").unwrap()
    );
    for (key, value) in full.as_object().unwrap() {
        if !key.eq_str("comments") && !key.eq_str("commentDeliveries") {
            assert_eq!(
                roundtrip.get_key(key),
                Some(value),
                "Original task field must remain unchanged."
            );
        }
    }
    assert!(hive.state.lock().unwrap().maximum_bytes <= JSON_OBJECT_CONTENT_BYTES);
    for page in hive.state.lock().unwrap().records.values().filter(|row| {
        row.required("object").unwrap().get("contractKey")
            == Some(&Json::from("task-board/reference-page"))
    }) {
        let name = page
            .required("object")
            .unwrap()
            .required("name")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(
            name.strip_prefix("Task references ")
                .unwrap()
                .parse::<usize>()
                .is_ok(),
            "Inline replay preserves its original reference-page identities."
        );
    }
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
                &destination,
                None,
                None
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

fn snapshot_store<'a>(
    db: &'a Connection,
    hive: Arc<Hive>,
) -> JournalStore<'a, Hive, RuntimeContracts, fn() -> String, fn() -> JsString> {
    JournalStore {
        journal: LocalJournal {
            connection: db,
            root_object_id: None,
        },
        client: hive,
        contracts: runtime_inputs::contracts().unwrap(),
        limits: HistoryLimits::default(),
        now: || TIME.into(),
        new_id: || uuid::Uuid::new_v4().to_string().into(),
        cache: None,
    }
}

fn append_request(id: &str, revision: f64) -> Json {
    Json::object([
        ("action", "comment".into()),
        ("operationId", id.into()),
        ("taskId", "task".into()),
        ("expectedRevision", Json::Number(revision)),
        ("commentId", id.into()),
        ("body", "A small new comment".into()),
        ("requests", Json::Array(vec![])),
        ("responses", Json::Array(vec![])),
        ("attachments", Json::Array(vec![])),
        ("replyTo", Json::Null),
        ("authorKind", "user".into()),
        ("purpose", "update".into()),
        ("agentDelivery", "queue".into()),
    ])
}
async fn publish_incremental_comment(
    journal: &JournalStore<'_, Hive, RuntimeContracts, fn() -> String, fn() -> JsString>,
    request: &Json,
    lose_final_reply: bool,
) -> IvyResult<Json> {
    publish_comment_after_admission(journal, request, lose_final_reply, || {}).await
}
async fn publish_comment_after_admission(
    journal: &JournalStore<'_, Hive, RuntimeContracts, fn() -> String, fn() -> JsString>,
    request: &Json,
    lose_final_reply: bool,
    after_admission: impl FnOnce(),
) -> IvyResult<Json> {
    use ivy_contracts::checked_native_contract::CheckedNativeContracts;
    use ivy_task_board::{
        engine_actions::TaskActions,
        engine_publication::{ExistingTaskPublication, empty_outcome},
        engine_validation::ActionValidation,
        lifecycle_journal::LifecycleJournal,
        native_lifecycle::LifecycleDocument,
        publication::Publication,
    };
    let pages = NoPages;
    let ops = Operations {
        store: LifecycleJournal {
            journal,
            pages: &pages,
        },
        now: edit_clock as fn() -> String,
    };
    let read = |_: Json,
                _: bool|
     -> futures_util::future::LocalBoxFuture<'_, IvyResult<LifecycleDocument>> {
        Box::pin(async {
            panic!("Comment admission and publication must not hydrate the conversation")
        })
    };
    let checks = EditChecks(&ops.store, &journal.contracts);
    let validation = ActionValidation {
        store: &ops.store,
        checks: &checks,
    };
    let catalogs = CheckedNativeContracts::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
    );
    let settings = Json::object([("phoneTarget", Json::Null)]);
    let actions = TaskActions {
        store: &ops.store,
        settings: &settings,
        catalogs: &catalogs,
        policies: &validation,
        lifecycle: &NoRestore,
        task_read: &read,
    };
    let mut caller = actor();
    if request.get("sourceTaskId").is_some_and(|v| !v.is_null()) {
        caller.insert("source", "native".into());
    }
    let at = TIME.into();
    let op = if request.get("handoff").is_some_and(|value| !value.is_null()) {
        caller.insert("source", "worker".into());
        ops.begin_with_handoff(request, &caller, || {
            actions.admit_handoff(request, &caller, &at)
        })
        .await?
    } else {
        ops.begin(request, &caller).await?
    };
    after_admission();
    let callback =
        |_: Json, value: Json| -> futures_util::future::LocalBoxFuture<'_, IvyResult<()>> {
            Box::pin(async move {
                if lose_final_reply && value.get("publication").is_some_and(|v| !v.is_null()) {
                    journal.client.state.lock().unwrap().lost_task_reply = true;
                }
                Ok(())
            })
        };
    let mut steps = Publication {
        operations: &ops,
        operation: &op,
        task_written: Some(&callback),
        index: 0.0,
    };
    let outcome = actions
        .publish(
            &op,
            &mut steps,
            empty_outcome(request.required("operationId")?.as_js_string().unwrap()),
            None,
        )
        .await?;
    ops.finish(&op.pin.object_id, &outcome).await?;
    Ok(outcome)
}

fn claimed_conversation_fixture(length: usize) -> Json {
    let mut fixture = conversation_fixture(length, if length == 300 { 600 } else { 0 });
    let target = json(r#"{"hostId":"fixture","serviceNodeId":"native-agent"}"#);
    let workspace = json(
        r#"{"hostId":"fixture","serviceNodeId":"native-agent","canonicalCwd":"/fixture","taskRoot":"/fixture","bootstrapPath":"/fixture","intendedPath":"/fixture","sourcePath":null,"project":null,"useWorktree":false,"repository":null}"#,
    );
    let request = Json::object([
        ("action", "start".into()),
        ("operationId", "original-start".into()),
        ("taskId", "task".into()),
        ("expectedRevision", Json::Number(1.0)),
        ("target", target.clone()),
        ("workspace", workspace.clone()),
        ("intent", json(r#"{"objectId":"plan","revision":1}"#)),
        ("commentIds", Json::Array(vec![])),
    ]);
    let primary = json(
        r#"{"serviceNodeId":"native-agent","namespace":"codex","kind":"thread","nativeId":"original-thread"}"#,
    );
    let owner = actor();
    let hash = hash_json(&request).unwrap();
    let pin = json(r#"{"objectId":"run","revision":1}"#);
    let mut run = json(
        r#"{"schemaVersion":1,"taskId":"task","attempt":1,"operationId":"original-start","callerPrincipalId":"caller","phase":"running","turnId":"original-turn","nativeEpoch":"epoch","calls":{"thread":null,"turn":null,"interrupt":null},"notificationCursor":0,"result":null,"cancellation":null,"externalOutcome":{"state":"active","observedAt":null,"evidence":null,"code":null},"createdAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","finishedAt":null}"#,
    );
    // Reuse the retained catalog plan; this test never calls a native provider.
    let bytes = include_bytes!("task-board-native-advance-cases.json.gz");
    let mut decoded = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&bytes[..]), &mut decoded)
        .unwrap();
    let cases = json(&decoded);
    run.insert(
        "plan",
        cases
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .required("base")
            .unwrap()
            .required("run")
            .unwrap()
            .required("plan")
            .unwrap()
            .clone(),
    );
    for (key, value) in [
        ("originalRequest", request.clone()),
        ("requestHash", hash.clone().into()),
        ("target", target.clone()),
        ("workspace", workspace.clone()),
        ("owner", owner.clone()),
        ("primaryResourceRef", primary.clone()),
    ] {
        run.insert(key, value);
    }
    let task = fixture.get_mut("task").unwrap();
    let value = task.get_mut("content").unwrap().get_mut("value").unwrap();
    value
        .get_mut("fields")
        .unwrap()
        .insert("control", "agent".into());
    value.insert("workflowState", "in_progress".into());
    value.insert("attemptCount", Json::Number(1.0));
    value.insert("lastRun", pin.clone());
    value.insert("primaryResourceRef", primary.clone());
    value.insert(
        "claim",
        Json::object([
            ("operationId", "original-start".into()),
            ("callerPrincipalId", "caller".into()),
            ("attempt", Json::Number(1.0)),
            ("owner", owner),
            ("request", request),
            ("requestHash", hash.into()),
            ("target", target),
            ("workspace", workspace),
            ("primaryAtStart", primary),
            ("claimedAt", TIME.into()),
            ("phase", "running".into()),
            ("run", pin),
        ]),
    );
    *value = ivy_sdk::upgrade_task_record(value).unwrap();
    let contracts = runtime_inputs::contracts().unwrap();
    contracts.validate_task(value).unwrap();
    let hash = hash_json(value).unwrap();
    let bytes = canonical(value, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
    task.get_mut("revision")
        .unwrap()
        .insert("contentHash", hash.into());
    task.get_mut("revision")
        .unwrap()
        .insert("byteLength", Json::Number(bytes as f64));
    task.get_mut("revision").unwrap().insert(
        "contractVersion",
        contracts.version("task-board/task").unwrap().into(),
    );
    let mut row = record("run", "task-board/run", run, 1.0, Json::Object(vec![]));
    row.get_mut("object")
        .unwrap()
        .insert("parentId", Json::Null);
    row.get_mut("object")
        .unwrap()
        .insert("ownerObjectId", Json::Null);
    row.get_mut("revision").unwrap().insert(
        "contractVersion",
        runtime_inputs::contracts()
            .unwrap()
            .version("task-board/run")
            .unwrap()
            .into(),
    );
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    nodes.push(row);
    fixture
}

#[tokio::test(flavor = "current_thread")]
async fn handoff_admission_and_recovery_preserve_the_claim_without_hydrating_history() {
    for length in [0, 300, 4096] {
        let hive = Arc::new(Hive::new(&claimed_conversation_fixture(length)));
        let path = std::env::temp_dir().join(format!(
            "task-board-handoff-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let mut request = append_request("compact-handoff", 1.0);
        request.insert("authorKind", "agent".into());
        request.insert("agentDelivery", "none".into());
        request.insert("purpose", "handoff".into());
        request.insert(
            "handoff",
            Json::object([
                ("workflowState", "todo".into()),
                ("wakeAt", "2026-02-01T00:00:00.000Z".into()),
            ]),
        );
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            assert_eq!(
                publish_incremental_comment(&journal, &request, true)
                    .await
                    .unwrap_err()
                    .code,
                "transport_lost"
            );
        }
        let writes = hive.state.lock().unwrap().writes;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let first = publish_incremental_comment(&journal, &request, false)
                .await
                .unwrap();
            assert_eq!(
                publish_incremental_comment(&journal, &request, false)
                    .await
                    .unwrap(),
                first
            );
            assert_eq!(hive.state.lock().unwrap().writes, writes);
            let saved = hive.state.lock().unwrap().records["task"].clone();
            let header = saved
                .required("content")
                .unwrap()
                .required("value")
                .unwrap();
            assert_eq!(
                header.get("workflowState"),
                Some(&Json::from("in_progress"))
            );
            assert_eq!(
                header.get("wakeAt"),
                request.required("handoff").unwrap().get("wakeAt")
            );
            assert_eq!(
                header.required("claim").unwrap().get("operationId"),
                Some(&Json::from("original-start"))
            );
            assert_eq!(
                header
                    .required("primaryResourceRef")
                    .unwrap()
                    .get("nativeId"),
                Some(&Json::from("original-thread"))
            );
            let reads = hive.state.lock().unwrap().comment_reads;
            let mut next = request.clone();
            next.insert("operationId", "next-handoff".into());
            next.insert("commentId", "next-handoff".into());
            next.insert("expectedRevision", Json::Number(3.0));
            publish_incremental_comment(&journal, &next, false)
                .await
                .unwrap();
            let added = hive.state.lock().unwrap().comment_reads - reads;
            assert!(
                added <= if length == 4096 { 4 } else { 3 },
                "Warm handoff reads only its right path: {length}, {added}"
            );
            let saved = hive.state.lock().unwrap().records["task"].clone();
            let header = saved
                .required("content")
                .unwrap()
                .required("value")
                .unwrap();
            let full = hydrated(hive.clone(), header, &"task".into()).await;
            let comments = full.required("comments").unwrap().as_array().unwrap();
            assert_eq!(comments.len(), length + 2);
            assert_eq!(
                comments[length].get("sequence"),
                Some(&Json::Number((length + 1) as f64))
            );
            assert_eq!(
                hash_json(comments[length].required("handoff").unwrap()).unwrap(),
                hash_json(request.required("handoff").unwrap()).unwrap()
            );
            assert!(
                full.required("commentDeliveries")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            println!(
                "handoff: {length} retained; warm admission/publication {added} node reads; original claim/thread and lost final reply/reopen/replay preserved"
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn compact_handoff_revalidates_original_work_and_run_after_admission() {
    for change in [
        "priority",
        "nativeOptions",
        "title",
        "run",
        "claim",
        "primary",
    ] {
        let hive = Arc::new(Hive::new(&claimed_conversation_fixture(300)));
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        let mut request = append_request("racing-handoff", 1.0);
        request.insert("authorKind", "agent".into());
        request.insert("agentDelivery", "none".into());
        request.insert("purpose", "handoff".into());
        request.insert("handoff", Json::object([("workflowState", "todo".into())]));
        let outcome = publish_comment_after_admission(&journal, &request, false, || {
            let mut state = hive.state.lock().unwrap();
            let mut current = state.records["task"].clone();
            let value = current
                .get_mut("content")
                .unwrap()
                .get_mut("value")
                .unwrap();
            match change {
                "priority" => {
                    value
                        .get_mut("fields")
                        .unwrap()
                        .insert("priority", Json::Number(4.0));
                }
                "nativeOptions" => {
                    value.get_mut("fields").unwrap().insert(
                        "nativeOptions",
                        Json::object([
                            ("model", "next".into()),
                            ("reasoningEffort", Json::Null),
                            ("serviceTier", Json::Null),
                        ]),
                    );
                    value.insert("executionChangeRequested", true.into());
                }
                "title" => {
                    value
                        .get_mut("fields")
                        .unwrap()
                        .insert("title", "Changed assignment".into());
                }
                "run" => {
                    value
                        .get_mut("claim")
                        .unwrap()
                        .insert("run", json(r#"{"objectId":"other-run","revision":1}"#));
                }
                "claim" => {
                    value
                        .get_mut("claim")
                        .unwrap()
                        .insert("operationId", "other-claim".into());
                }
                "primary" => {
                    value
                        .get_mut("primaryResourceRef")
                        .unwrap()
                        .insert("nativeId", "other-thread".into());
                }
                _ => unreachable!(),
            }
            journal.contracts.validate_task(value).unwrap();
            let hash = hash_json(value).unwrap();
            let bytes = canonical(value, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
            current
                .get_mut("revision")
                .unwrap()
                .insert("contentHash", hash.into());
            current
                .get_mut("revision")
                .unwrap()
                .insert("byteLength", Json::Number(bytes as f64));
            current
                .get_mut("revision")
                .unwrap()
                .insert("revision", Json::Number(2.0));
            current
                .get_mut("object")
                .unwrap()
                .insert("currentRevision", Json::Number(2.0));
            state.revisions.insert(("task".into(), 2), current.clone());
            state.records.insert("task".into(), current);
        })
        .await;
        if ["priority", "nativeOptions"].contains(&change) {
            outcome.unwrap_or_else(|error| panic!("Permitted {change} edit: {error:?}"));
            let saved = hive.state.lock().unwrap().records["task"].clone();
            let header = saved
                .required("content")
                .unwrap()
                .required("value")
                .unwrap();
            assert_eq!(
                header.get("workflowState"),
                Some(&Json::from("in_progress"))
            );
            if change == "priority" {
                assert_eq!(
                    header.required("fields").unwrap().get("priority"),
                    Some(&Json::Number(4.0))
                );
            } else {
                assert_eq!(
                    header
                        .required("fields")
                        .unwrap()
                        .required("nativeOptions")
                        .unwrap()
                        .get("model"),
                    Some(&Json::from("next"))
                );
                assert_eq!(
                    header.get("executionChangeRequested"),
                    Some(&Json::Bool(true))
                );
            }
            assert_eq!(hive.state.lock().unwrap().task_writes, 2);
        } else {
            assert_eq!(
                outcome.unwrap_err().code,
                "task_board_handoff_changed",
                "{change}"
            );
            assert_eq!(
                hive.state.lock().unwrap().task_writes,
                0,
                "No reservation for changed {change}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_appends_use_persisted_root_index_and_replay_after_a_lost_final_reply() {
    for length in [0, 1, 63, 64, 65, 300, 4096] {
        let hive = Arc::new(Hive::new(&conversation_fixture(
            length,
            if length == 300 { 600 } else { 0 },
        )));
        let path = std::env::temp_dir().join(format!(
            "task-board-comment-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap();
        let first = append_request("incremental-first", 1.0);
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let error = publish_incremental_comment(&journal, &first, true)
                .await
                .unwrap_err();
            assert_eq!(error.code, "transport_lost");
            assert_eq!(hive.state.lock().unwrap().task_writes, 2);
        }
        let before_replay = hive.state.lock().unwrap().writes;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let outcome = publish_incremental_comment(&journal, &first, false)
                .await
                .unwrap();
            assert_eq!(
                outcome.required("task").unwrap().get("revision"),
                Some(&Json::Number(3.0))
            );
            assert_eq!(hive.state.lock().unwrap().writes, before_replay);
            assert_eq!(
                publish_incremental_comment(&journal, &first, false)
                    .await
                    .unwrap(),
                outcome
            );
            assert_eq!(hive.state.lock().unwrap().writes, before_replay);
            let reads = hive.state.lock().unwrap().comment_reads;
            let second = append_request("incremental-second", 1.0); // append rebases to the actual current revision
            publish_incremental_comment(&journal, &second, false)
                .await
                .unwrap();
            let added_reads = hive.state.lock().unwrap().comment_reads - reads;
            assert!(
                added_reads <= if length == 4096 { 4 } else { 3 },
                "A warm append reads only the right path: {length}, {added_reads}"
            );
            let mut duplicate = append_request(
                if length > 0 {
                    "c1"
                } else {
                    "incremental-first"
                },
                1.0,
            );
            duplicate.insert("operationId", "duplicate-id-check".into());
            let writes = hive.state.lock().unwrap().writes;
            assert_eq!(
                publish_incremental_comment(&journal, &duplicate, false)
                    .await
                    .unwrap_err()
                    .code,
                "task_board_comment_mismatch"
            );
            assert_eq!(hive.state.lock().unwrap().writes, writes);
            assert_eq!(
                db.query_row("SELECT complete FROM comment_index_roots", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            let saved = hive.state.lock().unwrap().records["task"].clone();
            let stored = saved
                .required("content")
                .unwrap()
                .required("value")
                .unwrap();
            let full = hydrated(hive.clone(), stored, &"task".into()).await;
            assert_eq!(
                full.required("comments").unwrap().as_array().unwrap().len(),
                length + 2
            );
            assert_eq!(
                full.required("commentDeliveries")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                stored
                    .required("commentStore")
                    .unwrap()
                    .required("summary")
                    .unwrap()
                    .get("directCount"),
                Some(&Json::Number((length + 2) as f64))
            );
            println!(
                "ordinary comment: {length} retained; persisted warm append {added_reads} node reads; lost final reply/reopen/exact replay passed"
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn incremental_typed_responses_preserve_partial_coverage_and_reject_reused_identities() {
    let hive = Arc::new(Hive::new(&generated_fixture()));
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    let mut question = append_request("two-requests", 1.0);
    question.insert("purpose", "question".into());
    question.insert("recipient", json(r#"{"kind":"user"}"#));
    question.insert("requests",json(r#"[{"requestId":"a","type":"approval","title":"A","detail":""},{"requestId":"b","type":"approval","title":"B","detail":""}]"#));
    publish_incremental_comment(&journal, &question, false)
        .await
        .unwrap();
    for id in ["a", "b"] {
        let mut reply = append_request(&format!("answer-{id}"), 1.0);
        reply.insert(
            "responses",
            json(&format!(
                r#"[{{"requestId":"{id}","type":"approval","decision":"approved","detail":""}}]"#
            )),
        );
        publish_incremental_comment(&journal, &reply, false)
            .await
            .unwrap();
        let saved = hive.state.lock().unwrap().records["task"].clone();
        let stored = saved
            .required("content")
            .unwrap()
            .required("value")
            .unwrap();
        let questions = stored
            .required("commentStore")
            .unwrap()
            .required("summary")
            .unwrap()
            .required("questions")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(questions.len(), if id == "a" { 1 } else { 0 });
        let full = hydrated(hive.clone(), stored, &"task".into()).await;
        assert_eq!(
            ivy_sdk::open_task_questions(&full, None).unwrap().len(),
            questions.len()
        );
    }
    for (id, field, items, code) in [
        (
            "repeat-request",
            "requests",
            r#"[{"requestId":"a","type":"approval","title":"A","detail":""}]"#,
            "mutation_conflict",
        ),
        (
            "repeat-answer",
            "responses",
            r#"[{"requestId":"a","type":"approval","decision":"approved","detail":""}]"#,
            "task_board_request_already_answered",
        ),
        (
            "missing-request",
            "responses",
            r#"[{"requestId":"absent","type":"approval","decision":"approved","detail":""}]"#,
            "task_board_response_request_missing",
        ),
    ] {
        let mut request = append_request(id, 1.0);
        request.insert(field, json(items));
        let writes = hive.state.lock().unwrap().writes;
        assert_eq!(
            publish_incremental_comment(&journal, &request, false)
                .await
                .unwrap_err()
                .code,
            code
        );
        assert_eq!(hive.state.lock().unwrap().writes, writes);
    }
    let mut missing = append_request("missing-reply", 1.0);
    missing.insert("replyTo", "not-in-root".into());
    assert_eq!(
        publish_incremental_comment(&journal, &missing, false)
            .await
            .unwrap_err()
            .code,
        "task_board_comment_mismatch"
    );
    let mut unicode = append_request("surrogate-id", 1.0);
    unicode.insert("commentId", Json::String(JsString(vec![0xd800])));
    publish_incremental_comment(&journal, &unicode, false)
        .await
        .unwrap();
    unicode.insert("operationId", "surrogate-id-duplicate".into());
    assert_eq!(
        publish_incremental_comment(&journal, &unicode, false)
            .await
            .unwrap_err()
            .code,
        "task_board_comment_mismatch"
    );
    let mut old_question = append_request("old-question", 1.0);
    old_question.insert("purpose", "question".into());
    old_question.insert("recipient", json(r#"{"kind":"user"}"#));
    publish_incremental_comment(&journal, &old_question, false)
        .await
        .unwrap();
    let mut new_question = append_request("new-question", 1.0);
    new_question.insert("purpose", "question".into());
    new_question.insert("recipient", json(r#"{"kind":"user"}"#));
    new_question.insert("replyTo", "old-question".into());
    publish_incremental_comment(&journal, &new_question, false)
        .await
        .unwrap();
    let saved = hive.state.lock().unwrap().records["task"].clone();
    let stored = saved
        .required("content")
        .unwrap()
        .required("value")
        .unwrap();
    let questions = stored
        .required("commentStore")
        .unwrap()
        .required("summary")
        .unwrap()
        .required("questions")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        questions.len(),
        1,
        "A reply can close one question and open another."
    );
    assert_eq!(
        questions[0].get("commentId"),
        Some(&Json::from("new-question"))
    );
    let full = hydrated(hive.clone(), stored, &"task".into()).await;
    assert_eq!(
        ivy_sdk::open_task_questions(&full, None)
            .unwrap()
            .iter()
            .map(|q| q.get("commentId").cloned())
            .collect::<Vec<_>>(),
        vec![Some(Json::from("new-question"))]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn incomplete_or_foreign_root_indices_never_prove_absence_and_eviction_is_bounded() {
    use ivy_task_board::{
        engine_validation::ActionValidationStore, lifecycle_journal::LifecycleJournal,
    };
    let hive = Arc::new(Hive::new(&generated_fixture()));
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    let pages = NoPages;
    let store = LifecycleJournal {
        journal: &journal,
        pages: &pages,
    };
    let header = store
        .task_header(&"task".into(), true, false)
        .await
        .unwrap()
        .document
        .value;
    let missing = hive.state.lock().unwrap().records.remove("node25").unwrap();
    let request = append_request("c250", 1.0);
    assert!(
        store
            .comment_context(&header, &"task".into(), &request)
            .await
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT complete FROM comment_index_roots", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    hive.state
        .lock()
        .unwrap()
        .records
        .insert("node25".into(), missing);
    let context = store
        .comment_context(&header, &"task".into(), &request)
        .await
        .unwrap();
    assert!(context.identity_used);
    db.execute("DELETE FROM comment_index_roots", []).unwrap();
    let task_id = JsString::from("task");
    let first = append_request("c1", 1.0);
    let (first, last) = futures_util::future::join(
        store.comment_context(&header, &task_id, &first),
        store.comment_context(&header, &task_id, &request),
    )
    .await;
    assert!(first.unwrap().identity_used && last.unwrap().identity_used);
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM comment_index_ids WHERE kind=0",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        300
    );
    // An orphan in Hive is deliberately outside this immutable root.
    let orphan = record(
        "orphan",
        "task-board/comment-node",
        Json::object([
            ("schemaVersion", 1.0.into()),
            ("taskId", "task".into()),
            ("kind", "comment".into()),
            (
                "entry",
                Json::object([
                    ("comment", comment(301, "orphan".into())),
                    ("receipt", Json::Null),
                ]),
            ),
        ]),
        1.0,
        Json::Object(vec![]),
    );
    hive.state
        .lock()
        .unwrap()
        .records
        .insert("orphan".into(), orphan.clone());
    assert!(
        !store
            .comment_context(&header, &"task".into(), &append_request("c301", 1.0))
            .await
            .unwrap()
            .identity_used
    );
    let mut foreign = header.clone();
    let tree = foreign.get_mut("commentStore").unwrap();
    tree.insert("length", 1.0.into());
    tree.insert(
        "root",
        json(r#"{"object":{"objectId":"orphan","revision":1},"length":1}"#),
    );
    let mut other = orphan;
    let other_value = other.get_mut("content").unwrap().get_mut("value").unwrap();
    other_value.insert("taskId", "other-task".into());
    other_value
        .get_mut("entry")
        .unwrap()
        .get_mut("comment")
        .unwrap()
        .insert("sequence", 1.0.into());
    let digest = hash_json(other_value).unwrap();
    let bytes = canonical(other_value, JSON_OBJECT_CONTENT_BYTES)
        .unwrap()
        .len();
    other
        .get_mut("revision")
        .unwrap()
        .insert("contentHash", digest.into());
    other
        .get_mut("revision")
        .unwrap()
        .insert("byteLength", Json::Number(bytes as f64));
    hive.state
        .lock()
        .unwrap()
        .records
        .insert("orphan".into(), other);
    assert!(
        store
            .comment_context(&foreign, &"task".into(), &append_request("new", 1.0))
            .await
            .is_err()
    );
    // Fill only disposable metadata to exercise both count and accounted-byte bounds.
    for id in 0..129 {
        db.execute(
            "INSERT INTO comment_index_roots VALUES(?,?,0,1,65536,?)",
            ivy_store::rusqlite::params![format!("evict-{id}"), "unused", id + 1],
        )
        .unwrap();
    }
    store
        .comment_context(&header, &"task".into(), &request)
        .await
        .unwrap();
    let (roots, bytes) = db
        .query_row(
            "SELECT COUNT(*),SUM(bytes) FROM comment_index_roots",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert!(roots <= 128 && bytes <= 8 * 1024 * 1024);
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_identity_projection_finishes_the_stream_without_retaining_an_unbounded_index() {
    use ivy_task_board::{
        engine_validation::ActionValidationStore, lifecycle_journal::LifecycleJournal,
    };
    let mut fixture = conversation_fixture(512, 0);
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        panic!("fixture nodes")
    };
    for row in nodes {
        let value = row.get_mut("content").unwrap().get_mut("value").unwrap();
        if value.get("kind") != Some(&Json::from("comment")) {
            continue;
        }
        let comment = value.get_mut("entry").unwrap().get_mut("comment").unwrap();
        let sequence = comment.required("sequence").unwrap().as_f64().unwrap() as usize;
        let mut requests = vec![];
        let mut responses = vec![];
        for number in 0..32 {
            let prefix = format!("{sequence}-{number}-");
            let id = prefix.clone() + &"x".repeat(256 - prefix.len());
            requests.push(Json::object([
                ("requestId", id.clone().into()),
                ("type", "approval".into()),
                ("title", "Bounded identity".into()),
                ("detail", "".into()),
            ]));
            responses.push(Json::object([
                ("requestId", id.into()),
                ("type", "approval".into()),
                ("decision", "approved".into()),
                ("detail", "".into()),
            ]));
        }
        comment.insert("requests", Json::Array(requests));
        comment.insert("responses", Json::Array(responses));
        let hash = hash_json(value).unwrap();
        let bytes = canonical(value, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
        row.get_mut("revision")
            .unwrap()
            .insert("contentHash", hash.into());
        row.get_mut("revision")
            .unwrap()
            .insert("byteLength", Json::Number(bytes as f64));
    }
    let hive = Arc::new(Hive::new(&fixture));
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive);
    let pages = NoPages;
    let store = LifecycleJournal {
        journal: &journal,
        pages: &pages,
    };
    let header = store
        .task_header(&"task".into(), true, false)
        .await
        .unwrap()
        .document
        .value;
    let context = store
        .comment_context(&header, &"task".into(), &append_request("c512", 1.0))
        .await
        .unwrap();
    assert!(
        context.identity_used,
        "A match after eviction must still be found in the remaining stream."
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM comment_index_roots", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM comment_index_ids", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_later_generated_identity_does_not_hide_a_retained_ordinary_question() {
    let mut fixture = conversation_fixture(2, 0);
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        panic!("fixture nodes")
    };
    for row in nodes.iter_mut().take(2) {
        let value = row.get_mut("content").unwrap().get_mut("value").unwrap();
        let comment = value.get_mut("entry").unwrap().get_mut("comment").unwrap();
        comment.insert("commentId", "retained-shared-id".into());
        if comment.get("sequence") == Some(&Json::Number(1.0)) {
            comment.insert("purpose", "question".into());
            comment.insert("recipient", json(r#"{"kind":"user"}"#));
        } else {
            comment.insert("authorKind", "system".into());
        }
        let hash = hash_json(value).unwrap();
        let bytes = canonical(value, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
        row.get_mut("revision")
            .unwrap()
            .insert("contentHash", hash.into());
        row.get_mut("revision")
            .unwrap()
            .insert("byteLength", Json::Number(bytes as f64));
    }
    let task = fixture.get_mut("task").unwrap();
    let value = task.get_mut("content").unwrap().get_mut("value").unwrap();
    value.get_mut("commentStore").unwrap().get_mut("summary").unwrap().insert("questions",json(r#"[{"commentId":"retained-shared-id","sequence":1,"recipient":{"kind":"user"},"open":true}]"#));
    let hash = hash_json(value).unwrap();
    let bytes = canonical(value, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
    task.get_mut("revision")
        .unwrap()
        .insert("contentHash", hash.into());
    task.get_mut("revision")
        .unwrap()
        .insert("byteLength", Json::Number(bytes as f64));
    let hive = Arc::new(Hive::new(&fixture));
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    let mut request = append_request("resolve-retained-question", 1.0);
    request.insert("resolvesQuestions", json(r#"["retained-shared-id"]"#));
    publish_incremental_comment(&journal, &request, false)
        .await
        .unwrap();
    let saved = hive.state.lock().unwrap().records["task"].clone();
    let stored = saved
        .required("content")
        .unwrap()
        .required("value")
        .unwrap();
    assert!(
        stored
            .required("commentStore")
            .unwrap()
            .required("summary")
            .unwrap()
            .required("questions")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let full = hydrated(hive, stored, &"task".into()).await;
    assert_eq!(
        full.required("comments").unwrap().as_array().unwrap().len(),
        3
    );
    assert!(
        ivy_sdk::open_task_questions(&full, None)
            .unwrap()
            .is_empty()
    );
}

fn referenced_fixture() -> Json {
    let mut fixture = generated_fixture();
    // Exercise reference paging as well as conversation references. Each
    // original immutable comment retains one exact attachment revision.
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        panic!()
    };
    for (index, row) in nodes.iter_mut().take(300).enumerate() {
        let node = row.get_mut("content").unwrap().get_mut("value").unwrap();
        let comment = node.get_mut("entry").unwrap().get_mut("comment").unwrap();
        let uploader = comment.required("author").unwrap().clone();
        comment.insert(
            "attachments",
            Json::Array(vec![Json::object([
                ("attachmentId", format!("a{index}").into()),
                (
                    "object",
                    Json::object([
                        ("objectId", format!("attachment{index}").into()),
                        ("revision", Json::Number(1.0)),
                    ]),
                ),
                ("filename", "evidence.txt".into()),
                ("mediaType", "text/plain".into()),
                ("byteLength", Json::Number(1.0)),
                ("contentHash", digest(b"x").into()),
                ("createdAt", TIME.into()),
                ("uploader", uploader),
            ])]),
        );
        let hash = hash_json(node).unwrap();
        let bytes = canonical(node, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
        let revision = row.get_mut("revision").unwrap();
        revision.insert("contentHash", hash.into());
        revision.insert("byteLength", Json::Number(bytes as f64));
    }
    fixture
}

#[tokio::test(flavor = "current_thread")]
async fn inline_preparation_with_reference_pages_replays_a_lost_task_reply() {
    exercise(referenced_fixture()).await;
}

#[tokio::test(flavor = "current_thread")]
async fn compact_preparation_survives_restart_and_replays_exact_references_without_history_reads() {
    let hive = Arc::new(Hive::new(&referenced_fixture()));
    let path = std::env::temp_dir().join(format!(
        "task-board-snapshot-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let db = Connection::open(&path).unwrap();
    initialize(&db).unwrap();
    let proposed = Store::read(
        &snapshot_store(&db, hive.clone()),
        "task-board/task",
        &"task".into(),
        true,
    )
    .await
    .unwrap()
    .value;
    let full_bytes = canonical(&proposed, MANAGEMENT_FRAME_BYTES).unwrap().len();
    let actor = json(
        r#"{"principalId":"caller","serviceNodeId":"fixture.task-board","generation":1,"source":"user"}"#,
    );
    let request = Json::object([
        ("action", "edit".into()),
        ("operationId", "compact-edit".into()),
        ("taskId", "task".into()),
        ("expectedRevision", Json::Number(1.0)),
        ("fields", proposed.required("fields").unwrap().clone()),
    ]);
    let destination = Json::object([
        ("objectId", "task".into()),
        ("expectedRevision", Json::Number(1.0)),
    ]);
    let ops = Operations {
        store: snapshot_store(&db, hive.clone()),
        now: || TIME.into(),
    };
    let operation = ops.begin(&request, &actor).await.unwrap();
    let operation_id = operation.pin.object_id.clone();
    let entry = ops
        .stage(
            &operation_id,
            0.0,
            "task-board/task",
            &destination,
            &proposed,
        )
        .await
        .unwrap();
    assert_eq!(
        entry.get("payloadContract"),
        Some(&Json::from("task-board/prepared-payload"))
    );
    let address = entry
        .required("payload")
        .unwrap()
        .required("object")
        .unwrap()
        .clone();
    let operation_text = operation_id.to_utf8().unwrap();
    let (revision, saved): (i64, String) = db.query_row(
        "SELECT d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.id=?",
        [&operation_text], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    let mut foreign = parse(&saved, MANAGEMENT_FRAME_BYTES).unwrap();
    foreign
        .get_mut("request")
        .unwrap()
        .insert("taskId", "another-task".into());
    foreign.insert(
        "requestHash",
        hash_json(foreign.required("request").unwrap())
            .unwrap()
            .into(),
    );
    db.execute(
        "UPDATE revisions SET value_json=? WHERE id=? AND revision=?",
        ivy_store::rusqlite::params![
            canonical(&foreign, MANAGEMENT_FRAME_BYTES).unwrap(),
            &operation_text,
            revision
        ],
    )
    .unwrap();
    assert_eq!(
        ops.store
            .read_prepared("task-board/prepared-payload", &address)
            .await
            .unwrap_err()
            .code,
        "task_board_scope_mismatch"
    );
    db.execute(
        "UPDATE revisions SET value_json=? WHERE id=? AND revision=?",
        ivy_store::rusqlite::params![saved, &operation_text, revision],
    )
    .unwrap();
    let snapshot = ops
        .store
        .read_prepared("task-board/prepared-payload", &address)
        .await
        .unwrap();
    let compact_bytes = canonical(&snapshot.value, MANAGEMENT_FRAME_BYTES)
        .unwrap()
        .len();
    assert!(compact_bytes < 16 * 1024 && compact_bytes * 20 < full_bytes);
    assert!(
        snapshot
            .value
            .required("content")
            .unwrap()
            .required("comments")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        hive.state.lock().unwrap().task_writes,
        0,
        "Preparing nodes does not publish the Task."
    );
    let captured = snapshot.value;
    let resumed = Store::read(&ops.store, "task-board/task", &address, false)
        .await
        .unwrap();
    assert_eq!(
        resumed.value.required("comments").unwrap(),
        proposed.required("comments").unwrap()
    );
    assert_eq!(
        ops.store
            .read_prepared("task-board/prepared-payload", &address)
            .await
            .unwrap()
            .value,
        captured
    );
    drop(ops);
    drop(db);

    let db = Connection::open(&path).unwrap();
    initialize(&db).unwrap();
    let ops = Operations {
        store: snapshot_store(&db, hive.clone()),
        now: || TIME.into(),
    };
    // Re-entering a saved stage must not re-prepare a different caller value.
    let reads = hive.state.lock().unwrap().comment_reads;
    assert_eq!(
        ops.stage(
            &operation_id,
            0.0,
            "task-board/task",
            &destination,
            &Json::Null
        )
        .await
        .unwrap(),
        entry
    );
    hive.state.lock().unwrap().lost_reply = true;
    assert_eq!(
        ops.apply(&operation_id, 0).await.unwrap_err().code,
        "transport_lost"
    );
    {
        let mut state = hive.state.lock().unwrap();
        let newer = state.records.get_mut("task").unwrap();
        let content = newer.get_mut("content").unwrap().get_mut("value").unwrap();
        content
            .get_mut("fields")
            .unwrap()
            .insert("title", "A later concurrent edit".into());
        let hash = hash_json(content).unwrap();
        let bytes = canonical(content, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
        let revision = newer.get_mut("revision").unwrap();
        revision.insert("revision", Json::Number(3.0));
        revision.insert("contentHash", hash.into());
        revision.insert("byteLength", Json::Number(bytes as f64));
        newer
            .get_mut("object")
            .unwrap()
            .insert("currentRevision", Json::Number(3.0));
    }
    drop(ops);
    drop(db);

    let db = Connection::open(&path).unwrap();
    let ops = Operations {
        store: snapshot_store(&db, hive.clone()),
        now: || TIME.into(),
    };
    let pin = ops.apply(&operation_id, 0).await.unwrap();
    assert_eq!(ops.apply(&operation_id, 0).await.unwrap(), pin);
    let state = hive.state.lock().unwrap();
    assert_eq!(
        state.comment_reads, reads,
        "Apply and lost-reply replay need no conversation hydration."
    );
    assert_eq!(state.task_writes, 1);
    let saved = &state.receipts[&entry.required("mutationId").unwrap().as_str().unwrap()].1;
    assert_eq!(
        saved
            .required("content")
            .unwrap()
            .required("value")
            .unwrap(),
        captured.required("content").unwrap()
    );
    assert_eq!(
        saved
            .required("revision")
            .unwrap()
            .required("references")
            .unwrap(),
        captured.required("references").unwrap()
    );
    assert_eq!(
        saved.required("revision").unwrap().get("contractVersion"),
        captured.get("contractVersion")
    );
    assert_eq!(
        state.records["task"]
            .required("content")
            .unwrap()
            .required("value")
            .unwrap()
            .required("fields")
            .unwrap()
            .get("title"),
        Some(&Json::from("A later concurrent edit"))
    );
    let pages: Vec<_> = state
        .records
        .values()
        .filter(|row| {
            row.required("object").unwrap().get("contractKey")
                == Some(&Json::from("task-board/reference-page"))
        })
        .collect();
    assert_eq!(pages.len(), 2);
    assert!(
        pages.iter().all(
            |row| row.required("revision").unwrap().get("revision") == Some(&Json::Number(1.0))
        )
    );
    drop(state);
    // The compact snapshot must still reproduce every original comment and
    // attachment through its immutable root after the journal has reopened.
    let restored = Store::read(&ops.store, "task-board/task", &"task".into(), true)
        .await
        .unwrap();
    assert_eq!(
        restored.value.required("comments").unwrap(),
        proposed.required("comments").unwrap()
    );
    println!(
        "prepared snapshot: {full_bytes} inline bytes, {compact_bytes} compact bytes; 300 comments and attachment pins; no conversation reads during apply/replay"
    );
    drop(ops);
    drop(db);
    std::fs::remove_file(path).unwrap();
}

struct NoPages;
impl ivy_task_board::lifecycle_journal::LifecyclePages for NoPages {
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("No page query is needed for a header edit")
    }
}
fn destination(revision: f64) -> Json {
    Json::object([
        ("objectId", "task".into()),
        ("expectedRevision", Json::Number(revision)),
    ])
}
fn edit_request(id: &str, revision: f64, fields: &Json) -> Json {
    Json::object([
        ("action", "edit".into()),
        ("operationId", id.into()),
        ("taskId", "task".into()),
        ("expectedRevision", Json::Number(revision)),
        ("fields", fields.clone()),
        ("wakeAt", Json::Null),
    ])
}
fn actor() -> Json {
    json(
        r#"{"principalId":"caller","serviceNodeId":"fixture.task-board","generation":1,"source":"user"}"#,
    )
}
fn edit_clock() -> String {
    TIME.into()
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_header_and_system_append_keep_original_pins_and_bound_tree_work() {
    use ivy_task_board::lifecycle_journal::LifecycleJournal;
    for length in [0, 1, 63, 64, 65, 300, 4096] {
        let mut fixture = conversation_fixture(length, if length == 300 { 600 } else { 0 });
        let retained = Json::object([
            ("objectId", "retained-evidence-page".into()),
            ("revision", Json::Number(7.0)),
        ]);
        fixture
            .get_mut("task")
            .unwrap()
            .get_mut("revision")
            .unwrap()
            .get_mut("references")
            .unwrap()
            .insert("reference-page-0", retained.clone());
        let hive = Arc::new(Hive::new(&fixture));
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        let pages = NoPages;
        let store = LifecycleJournal {
            journal: &journal,
            pages: &pages,
        };
        let mut header = store
            .task_header(&"task".into(), true, false)
            .await
            .unwrap()
            .document
            .value;
        let original_tree = header.required("commentStore").unwrap().clone();
        header
            .get_mut("fields")
            .unwrap()
            .insert("title", "Header only".into());
        let ops = Operations {
            store,
            now: edit_clock as fn() -> String,
        };
        if length == 300 {
            let mut reordered = header.clone();
            let fields = reordered
                .required("commentStore")
                .unwrap()
                .as_object()
                .unwrap();
            let reversed = Json::Object(fields.iter().rev().cloned().collect());
            reordered.insert("commentStore", reversed);
            let prepared = ops
                .store
                .prepare_task_change(&reordered, &destination(1.0), TaskChange::Header)
                .await
                .unwrap();
            assert_eq!(
                hash_json(
                    prepared
                        .value
                        .required("content")
                        .unwrap()
                        .required("commentStore")
                        .unwrap()
                )
                .unwrap(),
                hash_json(&original_tree).unwrap()
            );
            let mut foreign = header.clone();
            foreign
                .get_mut("commentStore")
                .unwrap()
                .get_mut("root")
                .unwrap()
                .get_mut("object")
                .unwrap()
                .insert("objectId", "another-task-root".into());
            assert_eq!(
                ops.store
                    .prepare_task_change(&foreign, &destination(1.0), TaskChange::Header)
                    .await
                    .err()
                    .unwrap()
                    .code,
                "task_board_comment_mismatch"
            );
            assert_eq!(hive.state.lock().unwrap().writes, 0);
        }
        let op = ops
            .begin(
                &edit_request("header-only", 1.0, header.required("fields").unwrap()),
                &actor(),
            )
            .await
            .unwrap();
        let id = &op.pin.object_id;
        ops.stage_task_change(id, 0.0, &destination(1.0), &header, TaskChange::Header)
            .await
            .unwrap();
        ops.apply(id, 0).await.unwrap();
        assert_eq!(
            hive.state.lock().unwrap().comment_reads,
            0,
            "A header edit must not fetch any comment node."
        );
        assert_eq!(
            hive.state.lock().unwrap().writes,
            1,
            "Only the Task is written."
        );
        let saved = hive.state.lock().unwrap().records["task"].clone();
        assert_eq!(
            hash_json(
                saved
                    .required("content")
                    .unwrap()
                    .required("value")
                    .unwrap()
                    .required("commentStore")
                    .unwrap()
            )
            .unwrap(),
            hash_json(&original_tree).unwrap()
        );
        assert!(
            saved
                .required("revision")
                .unwrap()
                .required("references")
                .unwrap()
                .as_object()
                .unwrap()
                .iter()
                .any(|(_, pin)| pin == &retained)
        );

        let mut appended = ops
            .store
            .task_header(&"task".into(), true, false)
            .await
            .unwrap()
            .document
            .value;
        let mut notice = comment(length + 1, "Ticket edited".into());
        notice.insert("authorKind", "system".into());
        notice.insert("operationId", "append-notice".into());
        let receipt = Json::object([
            ("commentId", notice.required("commentId").unwrap().clone()),
            ("state", "queued".into()),
            ("operationId", "append-notice".into()),
            ("updatedAt", TIME.into()),
            ("code", Json::Null),
        ]);
        appended.insert("comments", Json::Array(vec![notice.clone()]));
        appended.insert("commentDeliveries", Json::Array(vec![receipt.clone()]));
        let op = ops
            .begin(
                &edit_request("append-notice", 2.0, appended.required("fields").unwrap()),
                &actor(),
            )
            .await
            .unwrap();
        let id = &op.pin.object_id;
        let stage = ops
            .stage_task_change(
                id,
                0.0,
                &destination(2.0),
                &appended,
                TaskChange::AppendSystem,
            )
            .await
            .unwrap();
        let reads = hive.state.lock().unwrap().comment_reads;
        assert!(
            reads <= 3,
            "Appending must read only the right path, length {length}, reads {reads}"
        );
        hive.state.lock().unwrap().lost_task_reply = true;
        assert_eq!(ops.apply(id, 0).await.unwrap_err().code, "transport_lost");
        let writes = hive.state.lock().unwrap().writes;
        assert!(
            writes <= 6,
            "Only the changed path is written, length {length}, writes {writes}"
        );
        assert_eq!(
            ops.stage_task_change(id, 0.0, &destination(2.0), &Json::Null, TaskChange::Header)
                .await
                .unwrap(),
            stage,
            "Saved stages skip even a changed preparation command."
        );
        assert_eq!(
            ops.apply(id, 0).await.unwrap().get("revision"),
            Some(&Json::Number(3.0))
        );
        assert_eq!(hive.state.lock().unwrap().writes, writes);
        assert_eq!(
            hive.state.lock().unwrap().comment_reads,
            reads,
            "Apply and original replay do not read the tree."
        );
        let saved = hive.state.lock().unwrap().records["task"].clone();
        let stored = saved
            .required("content")
            .unwrap()
            .required("value")
            .unwrap();
        assert_eq!(
            stored.required("commentStore").unwrap().get("length"),
            Some(&Json::Number((length + 1) as f64))
        );
        assert_eq!(
            stored
                .required("commentStore")
                .unwrap()
                .required("summary")
                .unwrap()
                .get("directCount"),
            Some(&Json::Number((length + 1) as f64))
        );
        assert!(
            saved
                .required("revision")
                .unwrap()
                .required("references")
                .unwrap()
                .as_object()
                .unwrap()
                .iter()
                .any(|(_, pin)| pin == &retained)
        );
        let full = hydrated(hive.clone(), stored, &"task".into()).await;
        let comments = full.required("comments").unwrap().as_array().unwrap();
        assert_eq!(comments.len(), length + 1);
        assert_eq!(comments.last(), Some(&notice));
        assert_eq!(
            full.required("commentDeliveries").unwrap(),
            &Json::Array(vec![receipt])
        );
        if length > 0 {
            assert_eq!(
                comments[0].required("commentId").unwrap(),
                &Json::from("c1")
            );
        }
        println!(
            "small Task change: {length} retained comments, header reads 0, append reads {reads}, total writes {writes}"
        );
    }
}

struct EditChecks<'a, S>(&'a S, &'a RuntimeContracts);
impl<S: Store> ivy_task_board::engine_validation::ActionValidationChecks for EditChecks<'_, S> {
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()> {
        self.1.validate_task(task)?;
        ivy_task_board::engine_validation::check_aggregate(self.0, task, id).await
    }
    async fn fields(&self, fields: &Json, _: &JsString) -> IvyResult<()> {
        self.1.validate_definition("TaskFields", fields)
    }
    async fn execution_options(&self, _: &Json, _: Option<&Json>) -> IvyResult<()> {
        panic!("Execution options remain unchanged")
    }
    async fn execution(
        &self,
        _: &Json,
        _: &Json,
        _: &JsString,
        _: &JsString,
        _: &Json,
        _: &Json,
    ) -> IvyResult<Json> {
        panic!("No native execution")
    }
    async fn deferral(&self, _: &Json, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("No deferral")
    }
    async fn content(&self, value: &Json) -> IvyResult<()> {
        self.1.validate_definition("ResultContent", value)
    }
    async fn artifact(&self, _: &Json) -> IvyResult<()> {
        panic!("No artifact")
    }
}
struct NoRestore;
impl ivy_task_board::engine_actions::ActionRestore for NoRestore {
    async fn restore_for_execution(
        &self,
        _: &ivy_task_board::operations::Document,
        _: &ivy_task_board::operations::Document,
    ) -> IvyResult<()> {
        panic!("No native execution")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn real_edit_program_uses_header_reads_and_appends_its_original_system_notice() {
    use ivy_contracts::checked_native_contract::CheckedNativeContracts;
    use ivy_task_board::{
        engine_actions::TaskActions,
        engine_publication::{ExistingTaskPublication, empty_outcome},
        engine_validation::ActionValidation,
        lifecycle_journal::LifecycleJournal,
        native_lifecycle::LifecycleDocument,
        publication::Publication,
    };
    for (control, action) in [("user", "edit"), ("agent", "edit"), ("user", "transition")] {
        let mut fixture = generated_fixture();
        let value = fixture
            .get_mut("task")
            .unwrap()
            .get_mut("content")
            .unwrap()
            .get_mut("value")
            .unwrap();
        value
            .get_mut("fields")
            .unwrap()
            .insert("control", control.into());
        let mut fields = value.required("fields").unwrap().clone();
        fields.insert("title", "A small edit of a large conversation".into());
        let hash = hash_json(value).unwrap();
        let bytes = canonical(value, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
        fixture
            .get_mut("task")
            .unwrap()
            .get_mut("revision")
            .unwrap()
            .insert("contentHash", hash.into());
        fixture
            .get_mut("task")
            .unwrap()
            .get_mut("revision")
            .unwrap()
            .insert("byteLength", Json::Number(bytes as f64));
        let hive = Arc::new(Hive::new(&fixture));
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        let pages = NoPages;
        let ops = Operations {
            store: LifecycleJournal {
                journal: &journal,
                pages: &pages,
            },
            now: edit_clock as fn() -> String,
        };
        let request = if action == "edit" {
            edit_request("actual-title-edit", 1.0, &fields)
        } else {
            Json::object([
                ("action", "transition".into()),
                ("operationId", "actual-title-edit".into()),
                ("taskId", "task".into()),
                ("expectedRevision", Json::Number(1.0)),
                ("workflowState", "todo".into()),
                ("detail", Json::Null),
            ])
        };
        let op = ops.begin(&request, &actor()).await.unwrap();
        let read =
            |_: Json,
             _: bool|
             -> futures_util::future::LocalBoxFuture<'_, IvyResult<LifecycleDocument>> {
                Box::pin(async { panic!("This action must not request a hydrated conversation") })
            };
        let checks = EditChecks(&ops.store, &journal.contracts);
        let validation = ActionValidation {
            store: &ops.store,
            checks: &checks,
        };
        let catalogs = CheckedNativeContracts::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        );
        let settings = Json::object([("phoneTarget", Json::Null)]);
        let actions = TaskActions {
            store: &ops.store,
            settings: &settings,
            catalogs: &catalogs,
            policies: &validation,
            lifecycle: &NoRestore,
            task_read: &read,
        };
        let mut steps = Publication {
            operations: &ops,
            operation: &op,
            task_written: None,
            index: 0.0,
        };
        let outcome = actions
            .publish(
                &op,
                &mut steps,
                empty_outcome(&"actual-title-edit".into()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            outcome.required("task").unwrap().get("revision"),
            Some(&Json::Number(3.0))
        );
        assert_eq!(hive.state.lock().unwrap().task_writes, 2);
        let reads = hive.state.lock().unwrap().comment_reads;
        assert_eq!(reads, if control == "agent" { 3 } else { 0 });
        let saved = hive.state.lock().unwrap().records["task"].clone();
        let stored = saved
            .required("content")
            .unwrap()
            .required("value")
            .unwrap();
        if action == "edit" {
            assert_eq!(
                stored.required("fields").unwrap().get("title"),
                fields.get("title")
            );
        } else {
            assert_eq!(stored.get("workflowState"), Some(&Json::from("todo")));
        }
        let full = hydrated(hive, stored, &"task".into()).await;
        let comments = full.required("comments").unwrap().as_array().unwrap();
        assert_eq!(comments.len(), if control == "agent" { 301 } else { 300 });
        if control == "agent" {
            assert_eq!(comments[300].get("sequence"), Some(&Json::Number(301.0)));
            assert_eq!(
                comments[300].get("body"),
                Some(&Json::from("Ticket edited"))
            );
            assert_eq!(
                comments[300].get("operationId"),
                Some(&Json::from("actual-title-edit"))
            );
            assert!(ivy_task_board::wake::comments_pending(&full));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn compact_edit_resumes_after_reservation_and_reopen_without_hydrating_the_conversation() {
    use ivy_contracts::{Outcome, checked_native_contract::CheckedNativeContracts};
    use ivy_task_board::{
        engine_actions::TaskActions,
        engine_publication::{ExistingTaskPublication, empty_outcome},
        engine_validation::ActionValidation,
        lifecycle_journal::LifecycleJournal,
        native_lifecycle::LifecycleDocument,
        publication::Publication,
    };
    use std::{cell::Cell, future::Future, pin::Pin};
    let mut fixture = generated_fixture();
    let task = fixture.get_mut("task").unwrap();
    let value = task.get_mut("content").unwrap().get_mut("value").unwrap();
    value
        .get_mut("fields")
        .unwrap()
        .insert("control", "agent".into());
    let mut fields = value.required("fields").unwrap().clone();
    fields.insert("title", "Resumed header edit".into());
    let hash = hash_json(value).unwrap();
    let bytes = canonical(value, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
    task.get_mut("revision")
        .unwrap()
        .insert("contentHash", hash.into());
    task.get_mut("revision")
        .unwrap()
        .insert("byteLength", Json::Number(bytes as f64));
    let hive = Arc::new(Hive::new(&fixture));
    let path =
        std::env::temp_dir().join(format!("task-board-edit-{}.sqlite", uuid::Uuid::new_v4()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let request = edit_request("resume-edit", 1.0, &fields);
    let mut original_outcome = None;
    let mut original_writes = 0;
    for attempt in 0..3 {
        let db = Connection::open(&path).unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        let pages = NoPages;
        let ops = Operations {
            store: LifecycleJournal {
                journal: &journal,
                pages: &pages,
            },
            now: edit_clock as fn() -> String,
        };
        let op = if attempt == 0 {
            ops.begin(&request, &actor()).await.unwrap()
        } else {
            ops.find(&"caller".into(), &"resume-edit".into())
                .await
                .unwrap()
                .unwrap()
        };
        let read =
            |_: Json,
             _: bool|
             -> futures_util::future::LocalBoxFuture<'_, IvyResult<LifecycleDocument>> {
                Box::pin(async { panic!("Recovery must not hydrate the conversation") })
            };
        let checks = EditChecks(&ops.store, &journal.contracts);
        let validation = ActionValidation {
            store: &ops.store,
            checks: &checks,
        };
        let catalogs = CheckedNativeContracts::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        );
        let settings = Json::object([("phoneTarget", Json::Null)]);
        let actions = TaskActions {
            store: &ops.store,
            settings: &settings,
            catalogs: &catalogs,
            policies: &validation,
            lifecycle: &NoRestore,
            task_read: &read,
        };
        let fail = Cell::new(attempt == 0);
        let callback = |_: Json, task: Json| -> Pin<Box<dyn Future<Output = IvyResult<()>> + '_>> {
            let fail = &fail;
            Box::pin(async move {
                if task.get("publication").is_some_and(|v| !v.is_null()) && fail.replace(false) {
                    return Err(IvyError::new(
                        "callback_lost",
                        "Lost response after the Task reservation.",
                    )
                    .with_outcome(Outcome::Unknown));
                }
                Ok(())
            })
        };
        let mut steps = Publication {
            operations: &ops,
            operation: &op,
            task_written: Some(&callback),
            index: 0.0,
        };
        let result = actions
            .publish(&op, &mut steps, empty_outcome(&"resume-edit".into()), None)
            .await;
        if attempt == 0 {
            assert_eq!(result.unwrap_err().code, "callback_lost");
            assert_eq!(hive.state.lock().unwrap().task_writes, 1);
            assert_eq!(hive.state.lock().unwrap().comment_reads, 0);
        } else {
            let outcome = result.unwrap();
            assert_eq!(
                outcome.required("task").unwrap().get("revision"),
                Some(&Json::Number(3.0))
            );
            if attempt == 1 {
                ops.finish(&op.pin.object_id, &outcome).await.unwrap();
                original_outcome = Some(hash_json(&outcome).unwrap());
                original_writes = hive.state.lock().unwrap().writes;
            } else {
                assert_eq!(Some(hash_json(&outcome).unwrap()), original_outcome);
                assert_eq!(hive.state.lock().unwrap().writes, original_writes);
            }
            assert_eq!(hive.state.lock().unwrap().task_writes, 2);
            assert_eq!(hive.state.lock().unwrap().comment_reads, 3);
        }
    }
    let saved = hive.state.lock().unwrap().records["task"].clone();
    let full = hydrated(
        hive,
        saved
            .required("content")
            .unwrap()
            .required("value")
            .unwrap(),
        &"task".into(),
    )
    .await;
    assert_eq!(
        full.required("comments").unwrap().as_array().unwrap().len(),
        301
    );
    assert_eq!(
        full.required("fields").unwrap().get("title"),
        Some(&Json::from("Resumed header edit"))
    );
    std::fs::remove_file(path).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn archived_header_publication_retains_conversation_and_exact_archive_cas_after_reopen() {
    use ivy_task_board::lifecycle_journal::LifecycleJournal;
    for (length, archive_race) in [(65, false), (300, true), (4096, false)] {
        let mut fixture = conversation_fixture(length, 0);
        let object = fixture.get_mut("task").unwrap().get_mut("object").unwrap();
        object.insert("archivedAt", TIME.into());
        object.insert("effectivelyArchived", true.into());
        let hive = Arc::new(Hive::new(&fixture));
        let original = fixture
            .required("task")
            .unwrap()
            .required("content")
            .unwrap()
            .required("value")
            .unwrap();
        let tree = original.required("commentStore").unwrap().clone();
        let path =
            std::env::temp_dir().join(format!("archived-header-{}.sqlite", uuid::Uuid::new_v4()));
        let operation;
        let prepared;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let lifecycle = LifecycleJournal {
                journal: &journal,
                pages: &NoPages,
            };
            assert_eq!(
                lifecycle
                    .task_header(&"task".into(), true, false)
                    .await
                    .err()
                    .unwrap()
                    .code,
                "task_board_record_archived"
            );
            let current = lifecycle
                .task_header(&"task".into(), true, true)
                .await
                .unwrap();
            assert_eq!(
                current.metadata.get("effectivelyArchived"),
                Some(&true.into())
            );
            assert_eq!(
                current.document.pin.to_json().get("revision"),
                Some(&Json::Number(1.0))
            );
            assert_eq!(
                hash_json(current.document.value.required("commentStore").unwrap()).unwrap(),
                hash_json(&tree).unwrap()
            );
            assert_eq!(hive.state.lock().unwrap().comment_reads, 0);
            let mut header = current.document.value;
            header
                .get_mut("fields")
                .unwrap()
                .insert("title", "Moved retained route".into());
            let ops = Operations {
                store: lifecycle,
                now: edit_clock as fn() -> String,
            };
            let op = ops
                .begin(
                    &edit_request("archived-header", 1.0, header.required("fields").unwrap()),
                    &actor(),
                )
                .await
                .unwrap();
            operation = op.pin.object_id.clone();
            assert_eq!(
                ops.store
                    .prepare_task_change(&header, &destination(1.0), TaskChange::Header)
                    .await
                    .err()
                    .unwrap()
                    .code,
                "task_board_record_archived"
            );
            let mut target = destination(1.0);
            target.insert("expectedArchived", true.into());
            prepared = ops
                .stage_task_change(&operation, 0.0, &target, &header, TaskChange::Header)
                .await
                .unwrap();
            if archive_race {
                hive.state
                    .lock()
                    .unwrap()
                    .records
                    .get_mut("task")
                    .unwrap()
                    .get_mut("object")
                    .unwrap()
                    .insert("effectivelyArchived", false.into());
                assert_eq!(
                    ops.apply(&operation, 0).await.unwrap_err().code,
                    "revision_conflict"
                );
                assert_eq!(hive.state.lock().unwrap().writes, 0);
            } else {
                hive.state.lock().unwrap().lost_task_reply = true;
                assert_eq!(
                    ops.apply(&operation, 0).await.unwrap_err().code,
                    "transport_lost"
                );
                assert_eq!(hive.state.lock().unwrap().writes, 1);
            }
            assert_eq!(hive.state.lock().unwrap().comment_reads, 0);
        }
        if !archive_race {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let ops = Operations {
                store: LifecycleJournal {
                    journal: &journal,
                    pages: &NoPages,
                },
                now: edit_clock as fn() -> String,
            };
            let mut original_target = destination(1.0);
            original_target.insert("expectedArchived", true.into());
            assert_eq!(
                ops.stage_task_change(
                    &operation,
                    0.0,
                    &original_target,
                    &Json::Null,
                    TaskChange::Header
                )
                .await
                .unwrap(),
                prepared
            );
            let result = ops.apply(&operation, 0).await.unwrap();
            assert_eq!(result.get("revision"), Some(&Json::Number(2.0)));
            let state = hive.state.lock().unwrap();
            let stored = state.records["task"]
                .required("content")
                .unwrap()
                .required("value")
                .unwrap();
            assert_eq!(
                hash_json(stored.required("commentStore").unwrap()).unwrap(),
                hash_json(&tree).unwrap()
            );
            assert!(
                stored
                    .required("comments")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert!(
                stored
                    .required("commentDeliveries")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                hash_json(
                    state.records["task"]
                        .required("revision")
                        .unwrap()
                        .required("references")
                        .unwrap()
                )
                .unwrap(),
                hash_json(
                    fixture
                        .required("task")
                        .unwrap()
                        .required("revision")
                        .unwrap()
                        .required("references")
                        .unwrap()
                )
                .unwrap()
            );
            assert_eq!(state.comment_reads, 0);
            assert_eq!(state.writes, 1);
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn manual_result_contact_and_review_keep_history_receipts_after_lost_reply_and_reopen() {
    for (length, contact) in [(65, "ticket"), (300, "chat"), (4096, "ticket")] {
        let mut fixture = conversation_fixture(length, if length == 300 { 600 } else { 1 });
        let task = fixture
            .get_mut("task")
            .unwrap()
            .get_mut("content")
            .unwrap()
            .get_mut("value")
            .unwrap();
        task.get_mut("fields")
            .unwrap()
            .insert("userContact", contact.into());
        task.insert("openQuestionCount", 1.0.into());
        task.get_mut("commentStore")
            .unwrap()
            .get_mut("summary")
            .unwrap()
            .insert(
                "questions",
                Json::Array(vec![json(
                    r#"{"commentId":"c12","sequence":12,"open":true,"recipient":{"kind":"user"}}"#,
                )]),
            );
        let contracts = runtime_inputs::contracts().unwrap();
        let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
            unreachable!()
        };
        for row in nodes {
            let value = row.get_mut("content").unwrap().get_mut("value").unwrap();
            if let Some(entry) = value.get_mut("entry") {
                let sequence = entry
                    .required("comment")
                    .unwrap()
                    .required("sequence")
                    .unwrap()
                    .as_f64()
                    .unwrap();
                if sequence == 1.0 {
                    entry.insert("receipt", json(r#"{"commentId":"c1","state":"queued","operationId":"source-input","updatedAt":"2026-01-02T03:04:05.006Z","code":null}"#));
                }
                if sequence == 12.0 {
                    let comment = entry.get_mut("comment").unwrap();
                    comment.insert("body", "Retained ordinary question".into());
                    comment.insert("purpose", "question".into());
                    comment.insert("recipient", json(r#"{"kind":"user"}"#));
                }
            }
            consumer_reads::refresh(row, &contracts);
        }
        consumer_reads::refresh(fixture.get_mut("task").unwrap(), &contracts);
        let hive = Arc::new(Hive::new(&fixture));
        let original = fixture
            .required("task")
            .unwrap()
            .required("content")
            .unwrap()
            .required("value")
            .unwrap();
        let full = hydrated(hive.clone(), original, &"task".into()).await;
        hive.state.lock().unwrap().comment_reads = 0;
        let path =
            std::env::temp_dir().join(format!("manual-result-{}.sqlite", uuid::Uuid::new_v4()));
        let mut request = edit_request("manual-final", 1.0, original.required("fields").unwrap());
        request.remove("fields");
        request.remove("wakeAt");
        request.insert("action", "saveResult".into());
        request.insert("run", Json::Null);
        request.insert("kind", "final".into());
        request.insert("content", json(r#"{"summary":"Manual result ready","artifacts":[],"codeReferences":[],"limitations":[],"repositoryResult":null}"#));
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            assert_eq!(
                publish_incremental_comment(&journal, &request, true)
                    .await
                    .unwrap_err()
                    .code,
                "transport_lost"
            );
            assert!(
                hive.state.lock().unwrap().comment_reads <= 8,
                "Only the changed append path may be read."
            );
        }
        let result;
        let tree;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            result = publish_incremental_comment(&journal, &request, false)
                .await
                .unwrap();
            let state = hive.state.lock().unwrap();
            assert_eq!(state.task_writes, 2);
            assert!(state.comment_reads <= 8);
            let saved = state.records["task"]
                .required("content")
                .unwrap()
                .required("value")
                .unwrap();
            assert_eq!(saved.get("workflowState"), Some(&Json::from("review")));
            assert_eq!(saved.get("openQuestionCount"), Some(&1.0.into()));
            tree = saved.required("commentStore").unwrap().clone();
            assert_eq!(tree.get("length"), Some(&Json::Number((length + 1) as f64)));
            let deliveries = state
                .records
                .values()
                .filter(|r| {
                    r.required("object").unwrap().get("contractKey")
                        == Some(&Json::from("task-board/delivery"))
                })
                .count();
            assert_eq!(deliveries, usize::from(contact == "chat"));
        }
        let stored = hive.state.lock().unwrap().records["task"]
            .required("content")
            .unwrap()
            .required("value")
            .unwrap()
            .clone();
        let after = hydrated(hive.clone(), &stored, &"task".into()).await;
        let before_comments = full.required("comments").unwrap().as_array().unwrap();
        let comments = after.required("comments").unwrap().as_array().unwrap();
        assert_eq!(comments.len(), length + 1);
        for (before, after) in before_comments.iter().zip(comments) {
            assert_eq!(hash_json(before).unwrap(), hash_json(after).unwrap());
        }
        assert_eq!(
            hash_json(full.required("commentDeliveries").unwrap()).unwrap(),
            hash_json(after.required("commentDeliveries").unwrap()).unwrap()
        );
        let notice = comments.last().unwrap();
        assert_eq!(
            notice.get("sequence"),
            Some(&Json::Number((length + 1) as f64))
        );
        assert_eq!(notice.get("authorKind"), Some(&Json::from("system")));
        assert_eq!(
            notice.get("delivery").is_some_and(|v| !v.is_null()),
            contact == "chat"
        );
        hive.state.lock().unwrap().comment_reads = 0;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let mut review = json(
                r#"{"action":"review","operationId":"manual-review","taskId":"task","expectedRevision":3,"acceptance":"Accepted manual result"}"#,
            );
            review.insert("result", result.required("result").unwrap().clone());
            publish_incremental_comment(&journal, &review, false)
                .await
                .unwrap();
            let state = hive.state.lock().unwrap();
            assert_eq!(state.comment_reads, 0);
            let saved = state.records["task"]
                .required("content")
                .unwrap()
                .required("value")
                .unwrap();
            assert_eq!(saved.get("workflowState"), Some(&Json::from("done")));
            assert_eq!(
                hash_json(saved.required("commentStore").unwrap()).unwrap(),
                hash_json(&tree).unwrap()
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn directed_reply_validates_the_original_question_from_its_exact_root_without_unrelated_bodies()
 {
    for length in [65, 300, 4096] {
        let mut fixture = conversation_fixture(length, 1);
        let contracts = runtime_inputs::contracts().unwrap();
        let source = fixture
            .get_mut("task")
            .unwrap()
            .get_mut("content")
            .unwrap()
            .get_mut("value")
            .unwrap();
        source
            .get_mut("fields")
            .unwrap()
            .insert("control", "agent".into());
        source.get_mut("commentStore").unwrap().get_mut("summary").unwrap().insert("questions", Json::Array(vec![
            json(r#"{"commentId":"c12","sequence":12,"open":true,"recipient":{"kind":"task","taskId":"peer"}}"#)
        ]));
        let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
            unreachable!()
        };
        for row in nodes {
            if let Some(comment) = row
                .get_mut("content")
                .unwrap()
                .get_mut("value")
                .unwrap()
                .get_mut("entry")
                .and_then(|e| e.get_mut("comment"))
            {
                if comment.get("sequence") == Some(&12.0.into()) {
                    comment.insert("body", "Canonical task question".into());
                    comment.insert("purpose", "question".into());
                    comment.insert("recipient", json(r#"{"kind":"task","taskId":"peer"}"#));
                }
            }
            consumer_reads::refresh(row, &contracts);
        }
        consumer_reads::refresh(fixture.get_mut("task").unwrap(), &contracts);
        let hive = Arc::new(Hive::new(&fixture));
        let mut peer = conversation_fixture(0, 0).required("task").unwrap().clone();
        peer.get_mut("object").unwrap().insert("id", "peer".into());
        peer.get_mut("object")
            .unwrap()
            .insert("name", "peer".into());
        peer.get_mut("content")
            .unwrap()
            .get_mut("value")
            .unwrap()
            .insert("taskKey", "TASK-0002".into());
        consumer_reads::refresh(&mut peer, &contracts);
        {
            let mut state = hive.state.lock().unwrap();
            state.records.insert("peer".into(), peer.clone());
            state.revisions.insert(("peer".into(), 1), peer);
        }
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        let header = journal
            .read_task_header(&"task".into(), true)
            .await
            .unwrap();
        journal
            .read_task_comments(&header.value, &"task".into(), &["c12".into()])
            .await
            .unwrap();
        let original_source;
        let removed;
        {
            let mut state = hive.state.lock().unwrap();
            original_source = state.records["task"].clone();
            let ids = state
                .records
                .iter()
                .filter(|(_, row)| {
                    row.required("object").unwrap().get("contractKey")
                        == Some(&Json::from("task-board/comment-node"))
                        && row
                            .required("content")
                            .unwrap()
                            .required("value")
                            .unwrap()
                            .get("kind")
                            == Some(&Json::from("comment"))
                        && row
                            .required("content")
                            .unwrap()
                            .required("value")
                            .unwrap()
                            .required("entry")
                            .unwrap()
                            .required("comment")
                            .unwrap()
                            .get("commentId")
                            != Some(&Json::from("c12"))
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            removed = ids
                .iter()
                .map(|id| (id.clone(), state.records.remove(id).unwrap()))
                .collect::<Vec<_>>();
            state.comment_reads = 0;
        }
        let mut request = append_request("directed-answer", 1.0);
        request.insert("taskId", "peer".into());
        request.insert("sourceTaskId", "task".into());
        request.insert("authorKind", "agent".into());
        request.insert(
            "questionSource",
            json(r#"{"taskId":"task","commentId":"c12"}"#),
        );
        request.insert("agentDelivery", "none".into());
        publish_incremental_comment(&journal, &request, false)
            .await
            .unwrap();
        {
            let state = hive.state.lock().unwrap();
            assert!(
                state.comment_reads <= 6,
                "Only the original canonical question is needed."
            );
            assert_eq!(
                hash_json(&state.records["task"]).unwrap(),
                hash_json(&original_source).unwrap()
            );
            assert_eq!(state.task_writes, 2);
        }
        let mut forged = request.clone();
        forged.insert("operationId", "forged-answer".into());
        forged.insert("commentId", "forged-answer".into());
        forged.insert("expectedRevision", 3.0.into());
        forged
            .get_mut("questionSource")
            .unwrap()
            .insert("commentId", "absent".into());
        assert_eq!(
            publish_incremental_comment(&journal, &forged, false)
                .await
                .unwrap_err()
                .code,
            "task_board_forbidden"
        );
        assert_eq!(hive.state.lock().unwrap().task_writes, 2);
        let mut state = hive.state.lock().unwrap();
        for (id, row) in removed {
            state.records.insert(id, row);
        }
    }
}
