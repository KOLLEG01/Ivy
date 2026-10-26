use ivy_contracts::{
    IvyError, IvyJson as Json, JsString, Outcome,
    hash::{digest, hash_json},
    json::{parse, stringify},
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    task_comments::RpcCommentSource,
};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    journal_store::{Contracts, JournalStore},
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    maintenance::*,
    schema::RuntimeContracts,
};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

fn json(text: &str) -> Json {
    parse(text, usize::MAX).unwrap()
}
fn field_mut<'a>(value: &'a mut Json, key: &str) -> &'a mut Json {
    let Json::Object(fields) = value else {
        panic!("Expected record")
    };
    &mut fields
        .iter_mut()
        .find(|(name, _)| name == &JsString::from(key))
        .unwrap()
        .1
}
fn clock() -> String {
    "2026-01-02T03:04:05.006Z".into()
}
fn id() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
fn pin(id: &str) -> Json {
    Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))])
}
fn actor() -> Json {
    json(
        r#"{"principalId":"task-board-record-upgrade","serviceNodeId":"fixture.task-board","generation":1,"source":"recovery"}"#,
    )
}
fn attachment(id: &str) -> Json {
    Json::object([
        ("attachmentId", id.into()),
        ("object", pin(id)),
        ("filename", format!("{id}.txt").into()),
        ("mediaType", "text/plain".into()),
        ("byteLength", Json::Number(8.0)),
        ("contentHash", digest(b"exported").into()),
        ("uploader", actor()),
        ("createdAt", clock().into()),
    ])
}
fn task() -> Json {
    let mut task = json(
        r#"{"schemaVersion":1,"taskKey":"TASK-0001","fields":{"title":"Keep data","description":"","acceptanceCriteria":[],"category":null,"control":"user","priority":2,"executionRequirement":null,"workspaceRequirement":{"kind":"task_workspace"},"userContact":"ticket","dueAt":null},"workflowState":"done","publication":null,"primaryResourceRef":null,"claim":null,"attemptCount":0,"lastRun":null,"lastHistory":null,"latestResult":null,"acceptedReview":null,"comments":[],"commentDeliveries":[],"agentCommentCount":0,"workRevision":0,"attachments":[],"createdAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","wakeAt":null,"dependencies":[],"runtimeWait":null}"#,
    );
    let mut user = json(
        r#"{"commentId":"user","sequence":1,"authorKind":"user","purpose":"update","body":"User text\ud800 stays","requests":[],"responses":[],"delivery":null,"attachments":[],"run":null,"turnId":null,"replyTo":null,"operationId":"user-operation","createdAt":"2026-01-02T03:04:05.006Z"}"#,
    );
    user.insert("author", actor());
    let mut generated = user.clone();
    generated.insert("commentId", "retained-findings-task-import".into());
    generated.insert("sequence", Json::Number(2.0));
    generated.insert("authorKind", "system".into());
    generated.insert("body", "Original generated import dump".into());
    task.insert("comments", Json::Array(vec![user, generated]));
    task.insert(
        "attachments",
        Json::Array(vec![attachment("exported"), attachment("unexported")]),
    );
    task.insert("commentDeliveries",json(r#"[{"commentId":"user","state":"delivered","operationId":"delivery-original","updatedAt":"2026-01-02T03:04:05.006Z","code":null}]"#));
    task
}
fn record(id: &str, key: &str, value: Json, parent: Json, owner: Json, references: Json) -> Json {
    Json::object([
        (
            "object",
            Json::object([
                ("id", id.into()),
                ("contractKey", key.into()),
                ("name", id.into()),
                ("parentId", parent),
                ("ownerObjectId", owner),
                ("currentRevision", Json::Number(1.0)),
                ("effectivelyArchived", false.into()),
            ]),
        ),
        (
            "revision",
            Json::object([
                ("objectId", id.into()),
                ("revision", Json::Number(1.0)),
                (
                    "contractVersion",
                    if key == "task-board/comment-node" {
                        "2.0.0"
                    } else {
                        "3.1.0"
                    }
                    .into(),
                ),
                ("mediaType", "application/json".into()),
                ("contentHash", hash_json(&value).unwrap().into()),
                (
                    "byteLength",
                    Json::Number(stringify(&value, usize::MAX).unwrap().len() as f64),
                ),
                ("references", references),
            ]),
        ),
        (
            "content",
            Json::object([("encoding", "json".into()), ("value", value)]),
        ),
    ])
}
#[derive(Default)]
struct State {
    records: HashMap<String, Json>,
    history: Vec<Json>,
    writes: usize,
    prunes: usize,
    deletes: Vec<String>,
}
#[derive(Default)]
struct Client {
    state: Mutex<State>,
    lose_task_reply: AtomicBool,
    block_progress_save: Mutex<Option<PathBuf>>,
}
impl Client {
    fn get(&self, id: &str) -> Json {
        self.state.lock().unwrap().records[id].clone()
    }
    fn task(&self) -> Json {
        self.get("task")
            .required("content")
            .unwrap()
            .required("value")
            .unwrap()
            .clone()
    }
    fn change_task(&self, change: impl FnOnce(&mut Json)) {
        let mut state = self.state.lock().unwrap();
        let value = state.records.get_mut("task").unwrap();
        change(field_mut(field_mut(value, "content"), "value"));
    }
}
// This transport models exact pinned reads, CAS and incoming-reference vetoes.
// Contract validation and the TaskBoard SQLite/publication logic are real.
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "system.status" {
                return Ok(Json::object([("runtimeEpoch", "fixture-epoch".into())]));
            }
            if method == "serviceNodes.get" {
                return Ok(json(
                    r#"{"principalId":"task-board-record-upgrade","serviceName":"task-board","connected":true,"synced":true}"#,
                ));
            }
            let mut state = self.state.lock().unwrap();
            match method {
                "objects.read" | "objects.stat" | "objects.history" => {
                    let id = params.required("objectId")?.as_str()?;
                    let value = state
                        .records
                        .get(&id)
                        .ok_or_else(|| IvyError::new("not_found", "Missing fixture object."))?;
                    if method == "objects.stat" {
                        return Ok(value.required("object")?.clone());
                    }
                    if method == "objects.history" {
                        return Ok(Json::object([
                            (
                                "items",
                                Json::Array(vec![value.required("revision")?.clone()]),
                            ),
                            ("nextCursor", Json::Null),
                        ]));
                    }
                    if let Some(revision) = params.get("revision") {
                        assert_eq!(Some(revision), value.required("revision")?.get("revision"));
                    }
                    Ok(value.clone())
                }
                "objects.query" => {
                    let key = params.required("contractKey")?.as_str()?;
                    let mut rows = vec![];
                    if key != "task-board/run" {
                        for value in state.records.values() {
                            if value.required("object")?.get("contractKey")
                                != Some(&key.clone().into())
                            {
                                continue;
                            }
                            let clauses = params
                                .required("where")?
                                .required("args")?
                                .as_array()
                                .unwrap();
                            if clauses.iter().all(|clause| {
                                let field = clause.get("field").unwrap().as_str().unwrap();
                                value
                                    .required("object")
                                    .unwrap()
                                    .get(field.strip_prefix("object.").unwrap())
                                    == clause.get("value")
                            }) {
                                rows.push(Json::object([
                                    (
                                        "objectId",
                                        value.required("object")?.required("id")?.clone(),
                                    ),
                                    (
                                        "revision",
                                        value.required("revision")?.required("revision")?.clone(),
                                    ),
                                ]));
                            }
                        }
                    }
                    Ok(Json::object([
                        ("items", Json::Array(rows)),
                        ("nextCursor", Json::Null),
                    ]))
                }
                "objects.write" => {
                    let value = params.required("content")?.required("value")?.clone();
                    if let Some(create) = params.get("create") {
                        let id = format!("node-{}", state.writes);
                        let mut saved = record(
                            &id,
                            &create.required("contractKey")?.as_str()?,
                            value,
                            create.required("parentId")?.clone(),
                            create.required("ownerObjectId")?.clone(),
                            params.required("references")?.clone(),
                        );
                        field_mut(&mut saved, "object")
                            .insert("name", create.required("name")?.clone());
                        state.writes += 1;
                        state.records.insert(id, saved.clone());
                        Ok(saved)
                    } else {
                        let id = params.required("objectId")?.as_str()?;
                        let previous = state.records[&id].clone();
                        if params.get("expectedRevision")
                            != previous.required("revision")?.get("revision")
                        {
                            return Err(IvyError::new("revision_conflict", "CAS changed."));
                        }
                        let revision = previous
                            .required("revision")?
                            .required("revision")?
                            .as_f64()
                            .unwrap()
                            + 1.0;
                        let mut saved = record(
                            &id,
                            "task-board/task",
                            value,
                            Json::Null,
                            Json::Null,
                            params.required("references")?.clone(),
                        );
                        field_mut(&mut saved, "revision")
                            .insert("revision", Json::Number(revision));
                        field_mut(&mut saved, "object")
                            .insert("currentRevision", Json::Number(revision));
                        state.history.push(previous);
                        state.writes += 1;
                        state.records.insert(id, saved.clone());
                        if let Some(path) = self.block_progress_save.lock().unwrap().take() {
                            fs::rename(&path, path.with_extension("saved")).unwrap();
                            fs::create_dir(&path).unwrap();
                        }
                        if self.lose_task_reply.swap(false, Ordering::SeqCst) {
                            return Err(IvyError::new(
                                "connection_closed",
                                "Lost reply after commit.",
                            )
                            .with_outcome(Outcome::Unknown));
                        }
                        Ok(saved)
                    }
                }
                "objects.pruneRevisions" => {
                    let deleted = state.history.len();
                    state.history.clear();
                    state.prunes += 1;
                    Ok(Json::object([("deleted", Json::Number(deleted as f64))]))
                }
                "objects.delete" => {
                    let id = params.required("objectId")?.as_str()?;
                    let referenced =
                        state
                            .records
                            .values()
                            .chain(state.history.iter())
                            .any(|value| {
                                value
                                    .required("revision")
                                    .unwrap()
                                    .required("references")
                                    .unwrap()
                                    .as_object()
                                    .unwrap()
                                    .iter()
                                    .any(|(_, pin)| pin.get("objectId") == Some(&id.clone().into()))
                            });
                    if referenced {
                        return Err(IvyError::new(
                            "target_conflict",
                            "An exact incoming pin still protects these bytes.",
                        ));
                    }
                    state
                        .records
                        .remove(&id)
                        .ok_or_else(|| IvyError::new("not_found", "Already removed."))?;
                    state.deletes.push(id);
                    Ok(Json::Null)
                }
                _ => panic!("Unexpected method {method}"),
            }
        })
    }
}
struct Fixture {
    root: PathBuf,
    client: Arc<Client>,
    contracts: RuntimeContracts,
    db: Option<Connection>,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-task-maintenance-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let client = Arc::new(Client::default());
        let task = task();
        let contracts = ivy_task_board::runtime_inputs::contracts().unwrap();
        contracts.validate_definition("Task", &task).unwrap();
        {
            let mut state = client.state.lock().unwrap();
            state.records.insert(
                "task".into(),
                record(
                    "task",
                    "task-board/task",
                    task,
                    Json::Null,
                    Json::Null,
                    Json::object([
                        ("attachment-0", pin("exported")),
                        ("attachment-1", pin("unexported")),
                    ]),
                ),
            );
            for id in ["exported", "unexported"] {
                let mut file = record(
                    id,
                    "task-board/attachment",
                    Json::Null,
                    "task".into(),
                    "task".into(),
                    Json::object([] as [(&str, Json); 0]),
                );
                let revision = field_mut(&mut file, "revision");
                revision.insert("contentHash", digest(b"exported").into());
                revision.insert("byteLength", Json::Number(8.0));
                state.records.insert(id.into(), file);
            }
        }
        let db = Connection::open(root.join("task-board.sqlite")).unwrap();
        initialize(&db).unwrap();
        Self {
            root,
            client,
            contracts,
            db: Some(db),
        }
    }
    fn context(&self) -> MaintenanceContext {
        MaintenanceContext {
            data_root: self.root.clone(),
            host_id: "host".into(),
            service_node_id: "fixture.task-board".into(),
            principal_id: "task-board-record-upgrade".into(),
        }
    }
    fn store(
        &self,
    ) -> JournalStore<'_, Client, &RuntimeContracts, fn() -> String, fn() -> JsString> {
        JournalStore {
            journal: LocalJournal {
                connection: self.db.as_ref().unwrap(),
                root_object_id: None,
            },
            client: self.client.clone(),
            contracts: &self.contracts,
            limits: HistoryLimits::default(),
            now: clock,
            new_id: id,
            cache: None,
        }
    }
    fn manifest(&self) -> ArchiveManifest {
        ArchiveManifest {
            host_id: "host".into(),
            path: self
                .root
                .join("verified-manifest.json")
                .to_string_lossy()
                .into_owned(),
            content_hash: digest(b"verified"),
        }
    }
    fn archive(&self) -> ArchivedAttachment {
        ArchivedAttachment {
            object: ObjectPin {
                object_id: "exported".into(),
                revision: 1,
            },
            content_hash: digest(b"exported"),
            byte_length: 8,
            local_path: self
                .root
                .join("exported.txt")
                .to_string_lossy()
                .into_owned(),
        }
    }
    fn plan(&self) -> ArtifactPlan {
        ArtifactPlan {
            schema_version: 1,
            host_id: "host".into(),
            service_node_id: "fixture.task-board".into(),
            scope: "verified-export".into(),
            archive_manifest: self.manifest(),
            task_ids: vec!["task".into()],
            files: vec![PlannedAttachment {
                archive: self.archive(),
                owner_task_id: "task".into(),
            }],
            reference_pages: vec![],
        }
    }
    fn comment(&self, id: &str) -> ArchivedCommentBody {
        let task = self.client.task();
        let comment = task
            .required("comments")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v.get("commentId") == Some(&id.into()))
            .unwrap();
        ArchivedCommentBody {
            comment_id: id.into(),
            body_hash: hash_json(comment.required("body").unwrap()).unwrap(),
            local_path: self
                .root
                .join("comment.json")
                .to_string_lossy()
                .into_owned(),
        }
    }
    async fn full(&self) -> Json {
        let reader =
            ivy_task_board::journal_store::persistent_reads::task_comments::TaskComments::new(
                Arc::new(RpcCommentSource::new(self.client.clone())),
                tokio::runtime::Handle::current(),
            );
        let header = self.client.task();
        let full = reader
            .hydrate(&header, &"task".into())
            .await
            .unwrap()
            .unwrap_or(header);
        reader.settled().await;
        full
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(db) = self.db.take() {
            db.close().unwrap();
        }
        let root = fs::canonicalize(&self.root).unwrap();
        assert_eq!(
            root.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-task-maintenance-")
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn interrupted_retirement_resumes_without_pruning_a_live_native_source() {
    let f = Fixture::new();
    let store = f.store();
    let original = f.client.task();
    store
        .journal
        .create(
            "task-board/run",
            &"Run".into(),
            None,
            &json(r#"{"taskId":"task","phase":"running"}"#),
            &clock().into(),
            &"live-run".into(),
        )
        .unwrap();
    let maintenance = Maintenance::new(&store, f.context());
    f.client.lose_task_reply.store(true, Ordering::SeqCst);
    assert_eq!(
        maintenance
            .admit_artifacts(f.plan())
            .await
            .unwrap_err()
            .outcome,
        Outcome::Unknown
    );
    maintenance.retention.settled().await;
    drop(maintenance);
    let resumed = Maintenance::new(&store, f.context());
    let mut changed_plan = f.plan();
    changed_plan.files.clear();
    assert_eq!(
        resumed
            .admit_artifacts(changed_plan)
            .await
            .unwrap_err()
            .code,
        "mutation_conflict"
    );
    resumed.pass().await.unwrap();
    assert_eq!(f.client.state.lock().unwrap().prunes, 0);
    assert!(f.client.state.lock().unwrap().deletes.is_empty());
    let job = records::load::<ArtifactJob>(f.root.join("artifact-cleanup.json"))
        .await
        .unwrap()
        .unwrap();
    assert!(job.retire_pending.is_empty());
    assert_eq!(job.remaining.len(), 1);
    store
        .journal
        .update(
            "task-board/run",
            &"live-run".into(),
            1.0,
            &json(r#"{"taskId":"task","phase":"completed"}"#),
            &clock().into(),
        )
        .unwrap();
    resumed.pass().await.unwrap();
    let state = f.client.state.lock().unwrap();
    assert_eq!(state.deletes, ["exported"]);
    assert!(state.records.contains_key("unexported"));
    drop(state);
    let full = f.full().await;
    assert_eq!(full.get("comments"), original.get("comments"));
    assert_eq!(
        full.get("commentDeliveries"),
        original.get("commentDeliveries")
    );
    assert_eq!(full.get("workRevision"), original.get("workRevision"));
    resumed.retention.settled().await;
}

#[tokio::test]
async fn only_exact_generated_imports_and_scoped_verified_attachments_can_change() {
    let f = Fixture::new();
    let store = f.store();
    let retention = Retention::new(&store);
    let before = f.client.task();
    let user = f.comment("user");
    assert_eq!(
        retention
            .archive_comments("task", &[user], "archive")
            .await
            .unwrap_err()
            .code,
        "task_board_evidence_mismatch"
    );
    let mut wrong = f.comment("retained-findings-task-import");
    wrong.body_hash = digest(b"wrong");
    assert_eq!(
        retention
            .archive_comments("task", &[wrong], "archive")
            .await
            .unwrap_err()
            .code,
        "task_board_evidence_mismatch"
    );
    let mut archive = f.archive();
    archive.content_hash = digest(b"different");
    assert_eq!(
        retention
            .retire_attachments("task", &[archive], "archive")
            .await
            .unwrap_err()
            .code,
        "task_board_evidence_mismatch"
    );
    assert_eq!(f.client.task(), before);
    assert_eq!(f.client.state.lock().unwrap().writes, 0);
    let mut foreign = f.store();
    foreign.journal.root_object_id = Some("other-workspace".into());
    let foreign = Retention::new(&foreign);
    assert_eq!(
        foreign
            .retire_attachments("task", &[f.archive()], "archive")
            .await
            .unwrap_err()
            .code,
        "task_board_scope_mismatch"
    );
    foreign.settled().await;
    {
        let result = record(
            "result",
            "task-board/result",
            Json::Null,
            Json::Null,
            Json::Null,
            Json::object([("protected", pin("exported"))]),
        );
        f.client
            .state
            .lock()
            .unwrap()
            .records
            .insert("result".into(), result);
    }
    f.client
        .change_task(|task| task.insert("latestResult", pin("result")));
    assert_eq!(
        retention
            .retire_attachments("task", &[f.archive()], "archive")
            .await
            .unwrap(),
        0
    );
    let maintenance = Maintenance::new(&store, f.context());
    let mut plan = f.plan();
    plan.service_node_id = "another.task-board".into();
    assert_eq!(
        maintenance.admit_artifacts(plan).await.unwrap_err().code,
        "task_board_scope_mismatch"
    );
    maintenance.retention.settled().await;
    retention.settled().await;
}

#[tokio::test]
async fn comment_job_recovers_a_failed_progress_save_and_the_operator_refuses_a_live_owner() {
    let f = Fixture::new();
    let store = f.store();
    let original = f.client.task();
    let archive = f.comment("retained-findings-task-import");
    let job = CommentJob {
        schema_version: 1,
        scope: "verified-comments".into(),
        archive_manifest: f.manifest(),
        remaining: vec![CommentTask {
            task_id: "task".into(),
            comments: vec![archive.clone()],
        }],
        completed_tasks: 0,
        rewritten_comments: 0,
        removed_characters: 0,
        updated_at: clock(),
        completed_at: None,
        last_error: None,
    };
    records::save(&f.root.join("comment-cleanup.json"), &job)
        .await
        .unwrap();
    let maintenance = Maintenance::new(&store, f.context());
    let path = f.root.join("comment-cleanup.json");
    *f.client.block_progress_save.lock().unwrap() = Some(path.clone());
    assert_eq!(
        maintenance.pass().await.unwrap_err().code,
        "storage_unavailable"
    );
    fs::remove_dir(&path).unwrap();
    fs::rename(path.with_extension("saved"), &path).unwrap();
    maintenance.pass().await.unwrap();
    let job = records::load::<CommentJob>(f.root.join("comment-cleanup.json"))
        .await
        .unwrap()
        .unwrap();
    assert!(job.remaining.is_empty() && job.completed_at.is_some());
    assert_eq!(job.completed_tasks, 1);
    let full = f.full().await;
    let comments = full.required("comments").unwrap().as_array().unwrap();
    assert_eq!(
        &comments[0],
        &original.required("comments").unwrap().as_array().unwrap()[0]
    );
    assert_eq!(
        comments[1].get("body"),
        Some(&archived_comment_note(&archive).into())
    );
    assert_eq!(
        full.get("commentDeliveries"),
        original.get("commentDeliveries")
    );
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_ivy-task-board-maintenance"))
        .args([
            "--config",
            f.root.join("absent-config.json").to_str().unwrap(),
            "--plan",
            f.root.join("absent-plan.json").to_str().unwrap(),
            "--stopped-pid",
            &std::process::id().to_string(),
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("task_board_owner_running")
    );
    maintenance.retention.settled().await;
}
