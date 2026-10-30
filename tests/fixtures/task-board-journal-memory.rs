use ivy_contracts::{IvyJson as Json, json::canonical, limits::MANAGEMENT_FRAME_BYTES};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{
    local_journal::{self, LocalJournal},
    local_retention::{HistoryLimits, LocalRetention},
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

fn seed(db: &Connection) {
    local_journal::initialize(db).unwrap();
    let encoded = canonical(
        &Json::object([("body", "x".repeat(1024 * 1024).into())]),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for index in 0..32 {
        let id = format!("record-{index:02}");
        db.execute(
            "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
            params![
                id,
                "task-board/task",
                id,
                Option::<String>::None,
                1,
                TIME,
                TIME
            ],
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
fn local_pages_drop_record_bodies_before_sorting() {
    if !isolated("journal_memory::local_pages_drop_record_bodies_before_sorting") {
        return;
    }
    let temp = super::Temporary::new();
    let db = Connection::open(temp.0.join("pages.sqlite")).unwrap();
    seed(&db);
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
                ("op", "contains".into()),
                ("field", "data:/body".into()),
                ("value", "xx".into()),
            ]),
        ),
        (
            "orderBy",
            Json::Array(vec![Json::object([
                ("field", "object.id".into()),
                ("direction", "desc".into()),
            ])]),
        ),
    ]);
    for expected in ["record-31", "record-30"] {
        let page = journal.page("task-board/task", &query).unwrap();
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
    fn request<'a>(&'a self, _: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        panic!("This local graph has no remote owner")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn retention_streams_current_record_bodies() {
    if !isolated("journal_memory::retention_streams_current_record_bodies") {
        return;
    }
    let temp = super::Temporary::new();
    let db = Connection::open(temp.0.join("retention.sqlite")).unwrap();
    seed(&db);
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
    let retention = LocalRetention {
        journal,
        client: &NoRemote,
        limits: HistoryLimits { count: 0, bytes: 0 },
    };
    retention
        .complete_workflow(&"unused".into(), &TIME.into(), None)
        .unwrap();
    let before = peak_bytes();
    retention.collect(0, 0, None, None).await.unwrap();
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
