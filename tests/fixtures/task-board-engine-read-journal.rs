use super::*;
use ivy_contracts::builtin_schema::SchemaBundle;
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    journal_store::JournalStore,
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
};
fn admission(n: &str, v: &Json) -> IvyResult<()> {
    static B: OnceLock<SchemaBundle> = OnceLock::new();
    let b = B.get_or_init(|| {
        SchemaBundle::new(&[
            include_str!("../../specs/schemas/task-board.schema.json"),
            include_str!("../../specs/schemas/chat.schema.json"),
            include_str!("../../specs/schemas/agent.schema.json"),
            include_str!("../../specs/schemas/hive-wire.schema.json"),
        ])
        .unwrap()
    });
    if b.matches(
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{n}"),
        v,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Value does not match its TaskBoard workflow contract.",
        ))
    }
}
struct Policies {
    task: Json,
}
impl Contracts for Policies {
    fn version(&self, k: &str) -> IvyResult<String> {
        Ok(if k == "task-board/task" {
            "1.8.0"
        } else {
            "1.0.0"
        }
        .into())
    }
    // A finite local route exercises the real journal without a live Hive. It
    // does not qualify production public-record routing or the full Task schema.
    fn local(&self, k: &str) -> IvyResult<bool> {
        assert!(matches!(
            k,
            "task-board/task" | "task-board/comment-read-state"
        ));
        Ok(true)
    }
    fn readable_version(&self, k: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &JsString::from(self.version(k)?))
    }
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        admission(n, v)
    }
    fn validate_record(&self, k: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        if k == "task-board/task" {
            assert_eq!(hash_json(v)?, hash_json(&self.task)?);
            Ok(())
        } else {
            assert_eq!(k, "task-board/comment-read-state");
            admission("CommentReadState", v)
        }
    }
}
struct Client;
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            Ok(match m {
                "serviceNodes.get" => Json::object([
                    ("principalId", "board".into()),
                    ("serviceName", "task-board".into()),
                    ("connected", true.into()),
                    ("synced", true.into()),
                ]),
                "system.status" => Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )]),
                _ => panic!("No live Hive or native effect permitted: {m}"),
            })
        })
    }
}
struct NoPages;
impl LifecyclePages for NoPages {
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("This exact marker/read case must not page")
    }
}
struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let p = std::path::PathBuf::from(format!("{}{suffix}", self.0.display()));
            let _ = std::fs::remove_file(p);
        }
    }
}
fn rows(c: &Connection) -> (i64, i64) {
    (
        c.query_row("SELECT count(*) FROM documents", [], |r| r.get(0))
            .unwrap(),
        c.query_row("SELECT count(*) FROM revisions", [], |r| r.get(0))
            .unwrap(),
    )
}
#[tokio::test(flavor = "current_thread")]
async fn actual_journal_reopen_preserves_marker_identity_count_and_timestamp() {
    let path = std::env::temp_dir().join(format!(
        "ivy-task-board-read-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let _cleanup = Cleanup(path.clone());
    let task = data().as_array().unwrap()[0]
        .required("expected")
        .unwrap()
        .required("outcomes")
        .unwrap()
        .as_array()
        .unwrap()[0]
        .required("ok")
        .unwrap()
        .required("task")
        .unwrap()
        .clone();
    let client = Arc::new(Client);
    let principal = JsString::from("board");
    let node = JsString::from("task-node");
    let caller = JsString::from("caller");
    let operation = JsString::from("mark");
    let cancelled = || Ok(());
    let mut task_pin: Option<Json> = None;
    let mut state_pin: Option<Json> = None;
    for iteration in 0..2 {
        let connection = Connection::open(&path).unwrap();
        initialize(&connection).unwrap();
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &connection,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Policies { task: task.clone() },
            limits: HistoryLimits {
                count: 10000,
                bytes: 256 * 1024 * 1024,
            },
            now: || {
                if iteration == 0 {
                    "2026-01-02T03:04:05.006Z"
                } else {
                    "2026-01-03T03:04:05.006Z"
                }
                .to_owned()
            },
            new_id: || uuid::Uuid::new_v4().to_string().into(),
            cache: None,
        };
        let pin = match &task_pin {
            Some(p) => p.clone(),
            None => journal
                .write(
                    "task-board/task",
                    &task,
                    "seed-task",
                    &Json::object([(
                        "create",
                        Json::object([("parentId", "root".into()), ("name", "Task".into())]),
                    )]),
                )
                .await
                .unwrap(),
        };
        task_pin = Some(pin.clone());
        let pages = NoPages;
        let operations = Operations {
            store: LifecycleJournal {
                journal: &journal,
                pages: &pages,
            },
            now: || {
                if iteration == 0 {
                    "2026-01-02T03:04:05.006Z"
                } else {
                    "2026-01-03T03:04:05.006Z"
                }
                .to_owned()
            },
        };
        let runtime = ReadRuntime {
            client: client.as_ref(),
            contracts: &journal.contracts,
            principal_id: &principal,
            service_node_id: &node,
            generation: 7.0,
        };
        let reads = EngineReads {
            operations: &operations,
            runtime: &runtime,
        };
        let request = Json::object([
            ("operationId", Json::String(operation.clone())),
            ("taskId", pin.required("objectId").unwrap().clone()),
            (
                "expectedTaskRevision",
                pin.required("revision").unwrap().clone(),
            ),
        ]);
        let context = ReadContext {
            caller_principal_id: &caller,
            operation_id: &operation,
            generation: 7.0,
            check_cancelled: &cancelled,
        };
        let marked = reads.mark_comments_read(&request, &context).await.unwrap();
        let marker = marked.required("readState").unwrap().clone();
        if let Some(prior) = &state_pin {
            assert_eq!(&marker, prior)
        } else {
            state_pin = Some(marker.clone())
        }
        assert_eq!(rows(&connection), (2, 2));
        assert_eq!(
            reads.mark_comments_read(&request, &context).await.unwrap(),
            marked
        );
        assert_eq!(rows(&connection), (2, 2));
        let stored = operations
            .store
            .read("task-board/comment-read-state", &marker, true)
            .await
            .unwrap();
        assert_eq!(
            f(&stored.value, "readAgentCommentCount"),
            &Json::Number(3.0)
        );
        assert_eq!(
            f(&stored.value, "updatedAt"),
            &Json::from("2026-01-02T03:04:05.006Z")
        );
        let query = Json::object([("task", pin.required("objectId").unwrap().clone())]);
        assert_eq!(
            f(
                &reads.task(&query, &caller).await.unwrap(),
                "unreadAgentComments"
            ),
            &Json::Number(0.0)
        );
        assert_eq!(
            f(
                &reads.task(&query, &"other".into()).await.unwrap(),
                "unreadAgentComments"
            ),
            &Json::Number(3.0)
        );
        assert_eq!(rows(&connection), (2, 2));
        // The first reply is deliberately not retained by an external caller.
        // Reopening later must recover the same marker without a new revision.
    }
}
