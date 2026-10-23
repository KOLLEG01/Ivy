use super::*;
use ivy_store::rusqlite::params;

fn frames(peer: &Peer, kind: &str) -> Vec<Json> {
    peer.frames
        .lock()
        .unwrap()
        .iter()
        .filter(|row| row.get("kind") == Some(&kind.into()))
        .cloned()
        .collect()
}
fn index(id: &str, length: usize) -> Json {
    Json::object([
        (
            "object",
            Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))]),
        ),
        ("length", Json::Number(length as f64)),
    ])
}
fn snapshot(node: Json) -> Json {
    Json::object([
        (
            "object",
            Json::object([
                ("contractKey", "task-board/comment-node".into()),
                ("parentId", "current-task".into()),
                ("ownerObjectId", "current-task".into()),
            ]),
        ),
        (
            "revision",
            Json::object([
                ("contractVersion", "1.0.0".into()),
                ("revision", Json::Number(1.0)),
            ]),
        ),
        (
            "content",
            Json::object([("encoding", "json".into()), ("value", node)]),
        ),
    ])
}
fn leaf(sequence: usize) -> Json {
    snapshot(Json::object([
        ("kind", "comment".into()),
        ("taskId", "current-task".into()),
        (
            "entry",
            Json::object([
                (
                    "comment",
                    Json::object([
                        ("sequence", Json::Number(sequence as f64)),
                        ("commentId", format!("comment-{sequence}").into()),
                        ("authorKind", "user".into()),
                        ("purpose", "update".into()),
                    ]),
                ),
                ("receipt", Json::Null),
            ]),
        ),
    ]))
}
fn seed(fixture: &Fixture) {
    let task = Json::object([
        ("taskKey", "TASK-0001".into()),
        ("workflowState", "review".into()),
        (
            "fields",
            Json::object([("control", "user".into()), ("category", Json::Null)]),
        ),
        ("publication", Json::Null),
        ("claim", Json::Null),
        ("runtimeWait", Json::Null),
        ("nextSteps", Json::Array(vec![])),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        ("createdAt", clock().into()),
        (
            "commentStore",
            Json::object([
                ("kind", "tree".into()),
                ("root", index("branch", 2)),
                ("length", Json::Number(2.0)),
                (
                    "summary",
                    Json::object([
                        ("directCount", Json::Number(2.0)),
                        ("questions", Json::Array(vec![])),
                    ]),
                ),
            ]),
        ),
    ]);
    let db = fixture.database();
    db.execute(
        "INSERT INTO documents VALUES('current-task','task-board/task','current',NULL,1,?,?)",
        params![clock(), clock()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO revisions VALUES('current-task',1,?,?)",
        params![
            ivy_contracts::json::stringify(&task, usize::MAX).unwrap(),
            clock()
        ],
    )
    .unwrap();
}
fn query() -> Json {
    Json::object([
        ("task", "current-task".into()),
        ("comments", Json::object([("limit", Json::Number(2.0))])),
    ])
}
fn branch() -> Json {
    snapshot(Json::object([
        ("kind", "branch".into()),
        ("taskId", "current-task".into()),
        (
            "children",
            Json::Array(vec![index("refuse", 1), index("held", 1)]),
        ),
    ]))
}
async fn configure(peer: &ServiceConnection, refusal: bool, hold_second: bool) {
    let error = Json::object([
        ("code", "fixture_current_refusal".into()),
        ("message", "Exact current node refusal.".into()),
        ("outcome", "not_executed".into()),
        (
            "details",
            Json::object([("scope", "canonical-local-node".into())]),
        ),
    ]);
    rpc(
        peer,
        "test.currentReadConfigure",
        Json::object([(
            "snapshots",
            Json::object([
                ("branch", Json::object([("saved", branch())])),
                (
                    "refuse",
                    if refusal {
                        Json::object([("error", error)])
                    } else {
                        Json::object([("saved", leaf(1))])
                    },
                ),
                (
                    "held",
                    Json::object([("saved", leaf(2)), ("hold", hold_second.into())]),
                ),
            ]),
        )]),
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn current_factory_keeps_abandoned_comment_flights_until_replacement_graph_can_open() {
    let peer = Peer::start_mode(Some("--factory-current-read")).await;
    let fixture = Fixture::new(false);
    fixture.page_held.release();
    let contracts = ContractsBinding;
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let factory = make_factory(&fixture, &contracts, &catalogs);
    let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
    let script = async {
        service.start();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        seed(&fixture);
        configure(&first, true, true).await;
        // Drop only the public observer. The accepted invocation and canonical
        // sibling flight remain owned by the real factory's borrowed graph.
        drop(service.control.handler("read")(query(), context(1)));
        until(|| frames(&peer, "current-node").len() == 3).await;
        // A second live call uses the cached branch but must retain the exact
        // source refusal, not convert it before the existing provider catch.
        let refusal = bounded(service.control.handler("read")(query(), context(1)))
            .await
            .unwrap_err();
        assert_eq!(refusal.code, "fixture_current_refusal");
        assert_eq!(refusal.message, "Exact current node refusal.");
        assert_eq!(refusal.outcome, Outcome::NotExecuted);
        assert_eq!(
            refusal.details,
            Some(Json::object([("scope", "canonical-local-node".into())]))
        );
        until(|| {
            frames(&peer, "current-node")
                .iter()
                .filter(|r| r.get("key") == Some(&"held".into()))
                .count()
                == 2
        })
        .await;
        // Re-attach the same open SDK connection to isolate graph retirement
        // from transport cancellation. No new graph may open while either
        // admitted, abandoned node request is suspended.
        let replacement = service.control.reconcile(first.clone());
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        assert_eq!(fixture.opens.get(), 1);
        assert!(fixture.drops.borrow().is_empty());
        assert_eq!(
            service.control.handler("read")(query(), context(1))
                .await
                .unwrap_err()
                .code,
            "service_not_ready"
        );
        let released = rpc(
            &first,
            "test.currentReadRelease",
            Json::object([("key", "held".into())]),
        )
        .await
        .unwrap();
        assert_eq!(released.get("released"), Some(&Json::Number(2.0)));
        bounded(replacement).await.unwrap();
        assert_eq!(fixture.opens.get(), 2);
        assert_eq!(fixture.drops.borrow().as_slice(), [1]);
        configure(&first, false, false).await;
        let read = bounded(service.control.handler("read")(query(), context(1)))
            .await
            .unwrap();
        let comments = read
            .required("task")
            .unwrap()
            .required("comments")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].get("sequence"), Some(&Json::Number(2.0)));
        assert_eq!(comments[1].get("sequence"), Some(&Json::Number(1.0)));
        assert_eq!(read.get("openQuestions"), Some(&Json::Array(vec![])));
        assert_eq!(
            read.required("executionCondition").unwrap().get("state"),
            Some(&"needs_user".into())
        );
        assert_eq!(
            frames(&peer, "current-node")
                .iter()
                .filter(|r| r.get("key") == Some(&"branch".into()))
                .count(),
            2
        );
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    result.unwrap();
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 1]);
    println!(
        "factory-current-read {}",
        ivy_contracts::json::stringify(
            &Json::object([
                ("nodes", Json::Array(frames(&peer, "current-node"))),
                ("completed", Json::Array(frames(&peer, "current-node-done"))),
                ("opens", Json::Number(fixture.opens.get() as f64)),
                (
                    "drops",
                    Json::Array(
                        fixture
                            .drops
                            .borrow()
                            .iter()
                            .map(|n| Json::Number(*n as f64))
                            .collect()
                    )
                ),
            ]),
            usize::MAX
        )
        .unwrap()
    );
    peer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn current_factory_history_query_settles_before_its_sqlite_owner_is_released() {
    let peer = Peer::start_mode(Some("--factory-current-read")).await;
    let mut fixture = Fixture::new(false);
    fixture.retention = Some(retention::Case::RowPressure);
    fixture.clock = retention::FactoryClock::manual();
    fixture.page_held.release();
    let contracts = ContractsBinding;
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let factory = make_factory(&fixture, &contracts, &catalogs);
    // A selected local workflow root may itself be the preparing operation.
    // This lets the real current Store resolve operation.request.taskId while
    // retaining its primary-scope guard, without a permissive read adapter.
    let mut setup = configured(&peer);
    setup.settings.insert("rootObjectId", "prepared-op".into());
    let (service, owner) = make_service(setup, &contracts).unwrap();
    let script = async {
        service.start();
        let connection = bounded(service.client.wait_ready(None)).await.unwrap();
        configure(&connection, false, true).await;
        rpc(
            &connection,
            "test.currentReadConfigure",
            Json::object([("holdQuery", true.into())]),
        )
        .await
        .unwrap();
        seed(&fixture);
        {
            let db = fixture.database();
            let mut task: Json = json(
                &db.query_row::<String, _, _>(
                    "SELECT value_json FROM revisions WHERE id='current-task'",
                    [],
                    |r| r.get(0),
                )
                .unwrap(),
            );
            let mut tree=task.required("commentStore").unwrap().clone();
            tree.remove("summary");
            task.insert("commentStore",tree);
            task.insert(
                "claim",
                Json::object([("run", Json::object([("objectId", "run".into())]))]),
            );
            db.execute(
                "UPDATE documents SET parent_id='prepared-op' WHERE id='current-task'",
                [],
            )
            .unwrap();
            db.execute(
                "UPDATE revisions SET value_json=? WHERE id='current-task'",
                [ivy_contracts::json::stringify(&task, usize::MAX).unwrap()],
            )
            .unwrap();
            for (id, parent, handoff) in [
                ("prepared-op", None, false),
                ("cleanup-op", Some("prepared-op"), true),
            ] {
                let mut operation = Json::object([
                    ("phase", "succeeded".into()),
                    ("request", Json::object([("taskId", "current-task".into())])),
                    ("actor", Json::object([("source", "user".into())])),
                    ("writes", Json::Array(vec![])),
                ]);
                if handoff {
                    operation.insert("handoffRun", Json::object([("objectId", "run".into())]));
                }
                db.execute(
                    "INSERT INTO documents VALUES(?,'task-board/operation',?,?,1,?,?)",
                    params![id, id, parent, clock(), clock()],
                )
                .unwrap();
                db.execute(
                    "INSERT INTO revisions VALUES(?,1,?,?)",
                    params![
                        id,
                        ivy_contracts::json::stringify(&operation, usize::MAX).unwrap(),
                        clock()
                    ],
                )
                .unwrap();
            }
            db.execute(
                "INSERT INTO cleanup_roots VALUES('cleanup-op',?)",
                [clock()],
            )
            .unwrap();
        }
        retention::drive(&fixture, || !frames(&peer, "current-query").is_empty()).await;
        let close = service.control.close();
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        assert!(fixture.drops.borrow().is_empty());
        assert_eq!(fixture.opens.get(), 1);
        assert_eq!(frames(&peer, "current-query-done").len(), 0);
        let released = rpc(
            &connection,
            "test.currentReadRelease",
            Json::object([("key", "query".into())]),
        )
        .await
        .unwrap();
        assert_eq!(released.get("released"), Some(&Json::Number(1.0)));
        // The accepted cleanup resumes after eager retirement and starts a
        // full Task read, including operation owner lookup and tree hydration.
        // Comment admission must still be open at this later producer point.
        until(|| frames(&peer, "current-node").len() == 3).await;
        assert!(fixture.drops.borrow().is_empty());
        assert_eq!(frames(&peer, "current-node-done").len(), 2);
        let released = rpc(
            &connection,
            "test.currentReadRelease",
            Json::object([("key", "held".into())]),
        )
        .await
        .unwrap();
        assert_eq!(released.get("released"), Some(&Json::Number(1.0)));
        bounded(close).await.unwrap();
        assert_eq!(fixture.drops.borrow().as_slice(), [1]);
        assert_eq!(
            fixture
                .database()
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM documents WHERE id='cleanup-op'",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
            1
        );
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    result.unwrap();
    println!(
        "factory-current-history {}",
        ivy_contracts::json::stringify(
            &Json::object([
                ("queries", Json::Array(frames(&peer, "current-query"))),
                ("nodes", Json::Array(frames(&peer, "current-node"))),
                ("completed", Json::Array(frames(&peer, "current-node-done"))),
                ("opens", Json::Number(fixture.opens.get() as f64)),
                (
                    "drops",
                    Json::Array(
                        fixture
                            .drops
                            .borrow()
                            .iter()
                            .map(|n| Json::Number(*n as f64))
                            .collect()
                    )
                ),
            ]),
            usize::MAX
        )
        .unwrap()
    );
    peer.finish().await;
}
