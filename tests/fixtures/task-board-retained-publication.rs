//! Retained publication bytes keep their captured contract and request identity.
use super::*;
use ivy_task_board::{
    engine_read::read_failure::ReadFailure,
    journal_store::persistent_reads::local_reads::SqliteReads,
};

fn json(text: &str) -> Json {
    parse(text, MANAGEMENT_FRAME_BYTES).unwrap()
}
fn pin(id: &str, revision: f64) -> Json {
    Json::object([("objectId", id.into()), ("revision", revision.into())])
}
fn request() -> Json {
    json(
        r#"{"action":"archive","operationId":"original","taskId":"task","expectedRevision":1,"archived":true}"#,
    )
}
fn operation(request: Json, writes: Vec<Json>) -> Json {
    Json::object([
        ("schemaVersion", 1.0.into()),
        ("operationId", "original".into()),
        ("callerPrincipalId", "caller".into()),
        ("requestHash", hash_json(&request).unwrap().into()),
        ("request", request),
        (
            "actor",
            json(
                r#"{"principalId":"caller","serviceNodeId":"task-board","generation":1,"source":"user"}"#,
            ),
        ),
        ("createdAt", TIME.into()),
        ("updatedAt", TIME.into()),
        ("phase", "succeeded".into()),
        ("writes", Json::Array(writes)),
        (
            "outcome",
            json(
                r#"{"operationId":"original","task":{"objectId":"task","revision":2},"plan":null,"run":null,"history":null,"result":null,"review":null,"attachment":null}"#,
            ),
        ),
        ("error", Json::Null),
    ])
}
fn task() -> Json {
    json(
        r#"{
        "schemaVersion":1,"taskKey":"TASK-0123",
        "fields":{"title":"Saved task","description":"","acceptanceCriteria":[],
            "category":null,"control":"user","priority":2,"executionRequirement":null,
            "workspaceRequirement":{"kind":"task_workspace"},"dependencies":[],
            "userContact":"ticket","nextReviewAt":null,"dueAt":null},
        "workflowState":"review","waiting":null,"publication":null,"primaryResourceRef":null,
        "claim":null,"attemptCount":0,"lastRun":null,"lastHistory":null,"latestResult":null,
        "acceptedReview":null,"comments":[],"commentDeliveries":[],"agentCommentCount":0,
        "workRevision":0,"attachments":[],
        "createdAt":"2026-01-02T03:04:05.000Z","updatedAt":"2026-01-02T03:04:05.000Z"
    }"#,
    )
}
fn write(value: &Json, version: &str) -> Json {
    Json::object([
        ("mutationId", "original-write".into()),
        ("contractKey", "task-board/task".into()),
        ("contractVersion", version.into()),
        (
            "payload",
            Json::object([
                ("object", pin("payload", 1.0)),
                ("label", "Prepared task".into()),
                ("mediaType", "application/json".into()),
                ("contentHash", hash_json(value).unwrap().into()),
            ]),
        ),
        (
            "destination",
            json(r#"{"objectId":"task","expectedRevision":1}"#),
        ),
        ("outcome", pin("task", 2.0)),
    ])
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_history_uses_the_captured_published_version_and_rejects_changed_evidence() {
    for version in ["1.5.0", "1.6.0"] {
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        let client = Arc::new(Client {
            input: Json::Null,
            trace: Mutex::default(),
        });
        let contracts = ivy_task_board::runtime_inputs::contracts().unwrap();
        let task = task();
        contracts
            .validate_record("task-board/task", &version.into(), &task)
            .unwrap();
        assert!(
            contracts
                .validate_record(
                    "task-board/task",
                    &contracts.version("task-board/task").unwrap().into(),
                    &task
                )
                .is_err()
        );
        let prepared = write(&task, version);
        let original = operation(request(), vec![prepared.clone()]);
        contracts
            .validate_definition("Operation", &original)
            .unwrap();
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts,
            limits: HistoryLimits::default(),
            now: || TIME.to_owned(),
            new_id: || "unused".into(),
            cache: None,
        };
        store
            .journal
            .create(
                "task-board/operation",
                &"Original".into(),
                Some(&"root".into()),
                &original,
                &TIME.into(),
                &"publication".into(),
            )
            .unwrap();
        store
            .journal
            .create(
                "task-board/task",
                &"Prepared task".into(),
                Some(&"publication".into()),
                &task,
                &TIME.into(),
                &"payload".into(),
            )
            .unwrap();
        let changes = db.total_changes();
        let local = SqliteReads {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            contracts: &store.contracts,
            parse: |raw: &str| parse(raw, MANAGEMENT_FRAME_BYTES).map_err(ReadFailure::<()>::from),
            sqlite_failure: |error| ReadFailure::<()>::from(ivy_store::sqlite::error(error)),
        };
        let raw = local
            .document::<ReadFailure<()>>("task-board/task", &pin("payload", 1.0), false)
            .unwrap();
        for field in ["metadata", "revision"] {
            assert_eq!(
                raw.required(field).unwrap().get("contractVersion"),
                Some(&version.into()),
                "{field}"
            );
        }
        assert_eq!(
            hash_json(raw.required("value").unwrap()).unwrap(),
            hash_json(&task).unwrap()
        );
        let failure = ReadFailure::<()>::source(());
        let failing = SqliteReads {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            contracts: &store.contracts,
            parse: |raw: &str| {
                let value = parse(raw, MANAGEMENT_FRAME_BYTES)?;
                if value.get("operationId").is_some() {
                    Err(failure.clone())
                } else {
                    Ok(value)
                }
            },
            sqlite_failure: |error| ReadFailure::<()>::from(ivy_store::sqlite::error(error)),
        };
        assert!(
            failure.ptr_eq(
                &failing
                    .document::<ReadFailure<()>>("task-board/task", &pin("payload", 1.0), false)
                    .unwrap_err()
            )
        );
        let (saved, metadata) = store
            .read_with_metadata("task-board/task", &pin("payload", 1.0), false, false)
            .await
            .unwrap();
        assert_eq!(hash_json(&saved.value).unwrap(), hash_json(&task).unwrap());
        assert_eq!(metadata.get("contractVersion"), Some(&version.into()));
        assert_eq!(
            hash_json(
                &store
                    .named_record(
                        "task-board/task",
                        &"Prepared task".into(),
                        Some(&"publication".into())
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .value
            )
            .unwrap(),
            hash_json(&task).unwrap()
        );
        let current = Store::read(&store, "task-board/task", &pin("payload", 1.0), false)
            .await
            .unwrap();
        assert_eq!(
            hash_json(&current.value).unwrap(),
            hash_json(&ivy_sdk::upgrade_task_record(&task).unwrap()).unwrap()
        );
        assert_eq!(
            hash_json(
                &store
                    .journal
                    .read("task-board/task", &pin("payload", 1.0), false)
                    .unwrap()
                    .value
            )
            .unwrap(),
            hash_json(&task).unwrap()
        );
        assert_eq!(db.total_changes(), changes);
        assert!(client.trace.lock().unwrap().is_empty());

        for fault in ["hash", "version", "contract", "media", "duplicate"] {
            let mut changed = prepared.clone();
            match fault {
                "hash" => {
                    let mut payload = changed.required("payload").unwrap().clone();
                    payload.insert("contentHash", format!("sha256:{}", "a".repeat(64)).into());
                    changed.insert("payload", payload);
                }
                "version" => {
                    changed.insert("contractVersion", "3.1.0".into());
                }
                "contract" => {
                    changed.insert("contractKey", "task-board/run".into());
                }
                "media" => {
                    let mut payload = changed.required("payload").unwrap().clone();
                    payload.insert("mediaType", "text/plain".into());
                    changed.insert("payload", payload);
                }
                "duplicate" => (),
                _ => unreachable!(),
            }
            let writes = if fault == "duplicate" {
                vec![changed.clone(), changed]
            } else {
                vec![changed]
            };
            let changed = operation(request(), writes);
            // Simulate damaged retained evidence, without exercising a live write.
            db.execute(
                "UPDATE revisions SET value_json=? WHERE id='publication' AND revision=1",
                [canonical(&changed, MANAGEMENT_FRAME_BYTES).unwrap()],
            )
            .unwrap();
            let changes = db.total_changes();
            assert!(
                Store::read(&store, "task-board/task", &pin("payload", 1.0), false)
                    .await
                    .is_err(),
                "{fault}"
            );
            assert!(
                local
                    .document::<ReadFailure<()>>("task-board/task", &pin("payload", 1.0), false)
                    .is_err(),
                "raw {fault}"
            );
            assert_eq!(db.total_changes(), changes);
            assert!(client.trace.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn retained_unaddressed_questions_remain_readable_without_admitting_new_ones() {
    let contracts = ivy_task_board::runtime_inputs::contracts().unwrap();
    let request = json(
        r#"{"action":"comment","operationId":"original","taskId":"task","expectedRevision":1,"commentId":"question","body":"Saved question","requests":[],"responses":[],"attachments":[],"replyTo":null,"authorKind":"agent","purpose":"question","contactUser":true}"#,
    );
    assert!(
        contracts
            .validate_definition("DurableRequest", &request)
            .is_err()
    );
    let saved = operation(request.clone(), vec![]);
    contracts.validate_definition("Operation", &saved).unwrap();
    assert_eq!(saved.get("request"), Some(&request));
    assert_eq!(
        saved.get("requestHash"),
        Some(&hash_json(&request).unwrap().into())
    );
    for field in ["body", "authorKind", "recipient"] {
        let mut invalid = request.clone();
        invalid.insert(field, 42.0.into());
        assert!(
            contracts
                .validate_definition("Operation", &operation(invalid, vec![]))
                .is_err(),
            "{field}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_tree_uses_the_original_task_owner_and_preserves_its_bytes() {
    for version in ["3.2.0", "3.3.0"] {
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        let comment = json(
            r#"{"commentId":"saved","sequence":1,"author":{"principalId":"caller","serviceNodeId":"task-board","generation":1,"source":"user"},"authorKind":"user","purpose":"update","body":"Original comment","requests":[],"responses":[],"delivery":null,"attachments":[],"run":null,"turnId":null,"replyTo":null,"createdAt":"2026-01-02T03:04:05.000Z","operationId":"original-comment"}"#,
        );
        let node = Json::object([
            ("schemaVersion", 1.0.into()),
            ("taskId", "task".into()),
            ("kind", "comment".into()),
            (
                "entry",
                Json::object([("comment", comment.clone()), ("receipt", Json::Null)]),
            ),
        ]);
        let response = Json::object([
            (
                "object",
                json(
                    r#"{"contractKey":"task-board/comment-node","parentId":"task","ownerObjectId":"task"}"#,
                ),
            ),
            (
                "revision",
                json(r#"{"contractVersion":"2.0.0","revision":1}"#),
            ),
            (
                "content",
                Json::object([("encoding", "json".into()), ("value", node)]),
            ),
        ]);
        let client = Arc::new(Client {
            input: Json::object([("readResult", response)]),
            trace: Mutex::default(),
        });
        let contracts = ivy_task_board::runtime_inputs::contracts().unwrap();
        let mut task = ivy_sdk::upgrade_task_record(&task()).unwrap();
        task.insert(
        "commentStore",
        json(r#"{"kind":"tree","root":{"object":{"objectId":"node","revision":1},"length":1},"length":1,"summary":{"directCount":1,"questions":[]}}"#),
    );
        contracts
            .validate_record("task-board/task", &version.into(), &task)
            .unwrap();
        let original = operation(request(), vec![write(&task, version)]);
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts,
            limits: HistoryLimits::default(),
            now: || TIME.to_owned(),
            new_id: || "unused".into(),
            cache: None,
        };
        for (id, key, parent, value) in [
            ("publication", "task-board/operation", "root", &original),
            ("payload", "task-board/task", "publication", &task),
        ] {
            store
                .journal
                .create(
                    key,
                    &id.into(),
                    Some(&parent.into()),
                    value,
                    &TIME.into(),
                    &id.into(),
                )
                .unwrap();
        }
        let changes = db.total_changes();
        let view = Store::read(&store, "task-board/task", &pin("payload", 1.0), false)
            .await
            .unwrap();
        assert_eq!(
            hash_json(view.value.required("comments").unwrap()).unwrap(),
            hash_json(&Json::Array(vec![comment])).unwrap()
        );
        assert_eq!(
            store
                .journal
                .read("task-board/task", &pin("payload", 1.0), false)
                .unwrap()
                .content_hash,
            hash_json(&task).unwrap()
        );
        assert_eq!(db.total_changes(), changes);
        let calls = client.trace.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = calls[0].as_array().unwrap();
        assert_eq!(call[0], Json::from("objects.read"));
        assert_eq!(call[1].get("objectId"), Some(&Json::from("node")));
        drop(calls);
        let mut foreign_request = request();
        foreign_request.insert("taskId", "another-task".into());
        let foreign = operation(foreign_request, vec![write(&task, version)]);
        db.execute(
            "UPDATE revisions SET value_json=? WHERE id='publication' AND revision=1",
            [canonical(&foreign, MANAGEMENT_FRAME_BYTES).unwrap()],
        )
        .unwrap();
        let changes = db.total_changes();
        assert!(
            Store::read(&store, "task-board/task", &pin("payload", 1.0), false)
                .await
                .is_err()
        );
        assert_eq!(db.total_changes(), changes);
    }
}
