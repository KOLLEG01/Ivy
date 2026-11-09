use ivy_contracts::{IvyJson as Json, json::canonical, limits::MANAGEMENT_FRAME_BYTES};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
};

const TIME: &str = "2026-01-02T03:04:05.006Z";

fn isolated(name: &str) -> bool {
    const CHILD: &str = "IVY_JOURNAL_MEMORY_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

fn peak_bytes() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<usize>()
        .unwrap()
        * 1024
}

fn seed(db: &Connection, key: &str) {
    local_journal::initialize(db).unwrap();
    let encoded = canonical(
        &Json::object([
            ("phase", "accepted".into()),
            ("body", "x".repeat(1024 * 1024).into()),
        ]),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for index in 0..32 {
        let id = format!("record-{index:02}");
        db.execute(
            "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
            params![id, key, id, Option::<String>::None, 1, TIME, TIME],
        )
        .unwrap();
        db.execute(
            "INSERT INTO revisions VALUES(?,?,?,?)",
            params![id, 1, encoded, TIME],
        )
        .unwrap();
    }
}

#[test]
fn local_pages_read_only_indexed_operation_heads() {
    if !isolated("journal_memory::local_pages_read_only_indexed_operation_heads") {
        return;
    }
    let temp = super::Temporary::new();
    let db = Connection::open(temp.0.join("pages.sqlite")).unwrap();
    seed(&db, "task-board/operation");
    let journal = LocalJournal {
        connection: &db,
        root_object_id: None,
    };
    let before = peak_bytes();
    let mut query = Json::object([
        ("limit", Json::Number(1.0)),
        (
            "where",
            Json::object([
                ("op", "in".into()),
                ("field", "data:/phase".into()),
                (
                    "value",
                    Json::Array(
                        ["accepted", "preparing", "publishing", "needs_attention"]
                            .map(Json::from)
                            .to_vec(),
                    ),
                ),
            ]),
        ),
        (
            "orderBy",
            Json::Array(vec![Json::object([
                ("field", "object.id".into()),
                ("direction", "asc".into()),
            ])]),
        ),
    ]);
    for expected in ["record-00", "record-01"] {
        let page = journal.page("task-board/operation", &query).unwrap();
        let items = page.required("items").unwrap().as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].required("objectId").unwrap(), &expected.into());
        query.insert("cursor", page.required("nextCursor").unwrap().clone());
    }
    let after = peak_bytes();
    assert!(
        after < before + 16 * 1024 * 1024,
        "before={before}, after={after}"
    );
    assert!(db.is_autocommit());
}

struct NoRemote;
impl RpcClient for NoRemote {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        assert_eq!(method, "objects.query");
        assert_eq!(params.get("contractKey"), Some(&"task-board/task".into()));
        Box::pin(async {
            Ok(Json::object([
                ("items", Json::Array(vec![])),
                ("nextCursor", Json::Null),
            ]))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn retention_streams_current_record_bodies() {
    if !isolated("journal_memory::retention_streams_current_record_bodies") {
        return;
    }
    let temp = super::Temporary::new();
    let db = Connection::open(temp.0.join("retention.sqlite")).unwrap();
    seed(&db, "task-board/task");
    let journal = LocalJournal {
        connection: &db,
        root_object_id: None,
    };
    journal
        .create(
            "task-board/coordination",
            &"unused".into(),
            None,
            &Json::object([("activeOperation", Json::Null)]),
            &TIME.into(),
            &"unused".into(),
        )
        .unwrap();
    journal
        .complete_workflow(&"unused".into(), &TIME.into(), None)
        .unwrap();
    let before = peak_bytes();
    super::retention::collect(
        &db,
        &NoRemote,
        HistoryLimits { count: 0, bytes: 0 },
        0,
        0,
        None,
        None,
    )
    .await
    .unwrap();
    let after = peak_bytes();
    assert!(
        after < before + 16 * 1024 * 1024,
        "before={before}, after={after}"
    );
    let remaining: i64 = db
        .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 32);
    assert!(db.is_autocommit());
}

impl ivy_sdk::rpc::OptionalRpcClient for NoRemote {
    fn request_optional<'a>(
        &'a self,
        method: &'a str,
        params: Option<Json>,
        options: ivy_sdk::rpc::RequestOptions,
    ) -> ivy_sdk::rpc::RequestFuture<'a> {
        match params {
            Some(params) => ivy_sdk::rpc::RpcClient::request(self, method, params, options),
            None => Box::pin(async { Err(ivy_contracts::IvyError::internal()) }),
        }
    }
}
