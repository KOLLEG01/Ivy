use super::*;

pub(super) struct Scenario {
    pub(super) creations: [Gate; 2],
    scans: [Gate; 2],
    scanning: [Cell<bool>; 2],
    categories: [Gate; 2],
    reading: [Cell<bool>; 2],
    reads_done: [Cell<bool>; 2],
    scans_done: [Cell<bool>; 2],
}
impl Scenario {
    fn new() -> Self {
        Self {
            creations: Default::default(),
            scans: Default::default(),
            scanning: Default::default(),
            categories: Default::default(),
            reading: Default::default(),
            reads_done: Default::default(),
            scans_done: Default::default(),
        }
    }
    fn reviewed(&self) -> Vec<(&'static str, Json, &'static str)> {
        retirement::reviewed_records()
            .into_iter()
            .filter_map(|(key, mut value, id)| {
                if id == "run" {
                    return None;
                }
                if id == "task" {
                    value.insert(
                        "lastRun",
                        Json::object([
                            ("objectId", "newer-run".into()),
                            ("revision", Json::Number(1.0)),
                        ]),
                    );
                }
                Some((key, value, id))
            })
            .collect()
    }
    pub(super) fn bind(&self, context: &Context<'_>) -> IvyResult<()> {
        if context.owner.generation != 1.0 {
            return Ok(());
        }
        for (key, value, id) in self.reviewed() {
            let version = context.journal.contracts.version(key)?;
            context
                .journal
                .contracts
                .validate_record(key, &version.into(), &value)?;
            context.journal.journal.create(
                key,
                &id.into(),
                None,
                &value,
                &clock().into(),
                &id.into(),
            )?;
        }
        context.journal.journal.connection.execute("INSERT INTO workspace_reservations VALUES('fixture-native-host','/fixture','task','current-workspace')", []).unwrap();
        Ok(())
    }
    fn preserved(&self, fixture: &Fixture) {
        let records = self.reviewed();
        retirement::assert_reviewed_current(fixture, &records);
        for (key, value, id) in records {
            let current = fixture.rows(key).into_iter().find(|r| r.0 == id).unwrap();
            assert_eq!(current.1, 1);
            assert_eq!(hash_json(&current.2).unwrap(), hash_json(&value).unwrap());
        }
    }
    pub(super) async fn page(&self, generation: usize, key: &str, options: &Json) {
        let g = generation - 1;
        assert!(g < 2, "Closed service must not open another graph");
        if key == "task-board/operation" {
            assert_eq!(
                options.required("where").unwrap().get("field"),
                Some(&"data:/phase".into())
            );
            self.scanning[g].set(true);
            self.scans[g].wait().await;
            self.scans_done[g].set(true);
        } else if options.get("select").is_some() {
            assert_eq!(key, "task-board/task");
            self.reading[g].set(true);
            self.categories[g].wait().await;
            self.reads_done[g].set(true);
        }
    }
}
fn count(fixture: &Fixture, table: &str) -> i64 {
    assert!(["revisions", "blobs", "active_gates"].contains(&table));
    fixture
        .database()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn operation(fixture: &Fixture, id: &str) -> (String, u64, Json) {
    fixture
        .rows("task-board/operation")
        .into_iter()
        .find(|r| r.2.get("operationId") == Some(&id.into()))
        .unwrap()
}
fn snapshot(fixture: &Fixture, stage: &str) {
    let records = [
        "task-board/operation",
        "task-board/task",
        "task-board/history",
        "task-board/result",
    ]
    .into_iter()
    .flat_map(|key| {
        fixture
            .rows(key)
            .into_iter()
            .map(move |(id, revision, value)| {
                Json::object([
                    ("key", key.into()),
                    ("id", id.into()),
                    ("revision", Json::Number(revision as f64)),
                    ("value", value),
                ])
            })
    })
    .collect();
    let proof = Json::object([
        ("stage", stage.into()),
        ("records", Json::Array(records)),
        (
            "activeGates",
            Json::Number(count(fixture, "active_gates") as f64),
        ),
        ("opens", Json::Number(fixture.opens.get() as f64)),
        (
            "drops",
            Json::Array(
                fixture
                    .drops
                    .borrow()
                    .iter()
                    .map(|g| Json::Number(*g as f64))
                    .collect(),
            ),
        ),
    ]);
    println!(
        "factory-lifecycle {}",
        ivy_contracts::json::stringify(&proof, 32 * 1024 * 1024).unwrap()
    );
}
fn original_lifecycle() -> Json {
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-service-lifecycle-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json(std::str::from_utf8(&output.stdout).unwrap())
}
fn assert_error(error: IvyError, expected: &Json) {
    assert_eq!(
        Json::object([
            ("code", Json::String(error.code_js())),
            ("message", Json::String(error.message_js())),
            ("outcome", error.outcome.as_str().into())
        ]),
        *expected
    );
}

#[tokio::test(flavor = "current_thread")]
async fn offline_and_close_refuse_late_admission_and_drain_factory_work_before_releasing_journal() {
    let source = original_lifecycle();
    let peer = Peer::start_mode(Some("--factory")).await;
    let mut fixture = Fixture::new(false);
    fixture.lifecycle = Some(Scenario::new());
    let scenario = fixture.lifecycle.as_ref().unwrap();
    let contracts = ContractsBinding;
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let factory = make_factory(&fixture, &contracts, &catalogs);
    let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
    let script = async {
        service.start();
        for generation in 1..=2 {
            let index = generation as usize - 1;
            let connection = bounded(service.client.wait_ready(None)).await.unwrap();
            assert_eq!(connection.generation(), generation);
            until(|| scenario.scanning[index].get()).await;
            let id = if generation == 1 {
                "factory-create"
            } else {
                "factory-close"
            };
            let mut request = create();
            request.insert("operationId", id.into());
            admission("CreateRequest", &request).unwrap();
            let remote = {
                let connection = connection.clone();
                let request = request.clone();
                tokio::spawn(async move { rpc(&connection, "test.invokeAction", request).await })
            };
            until(|| fixture.creations.borrow().len() == generation as usize).await;
            let accepted = operation(&fixture, id);
            snapshot(
                &fixture,
                if generation == 1 {
                    "offline-accepted"
                } else {
                    "close-accepted"
                },
            );
            assert_eq!(accepted.2.get("phase"), Some(&"accepted".into()));
            assert_eq!(
                hash_json(accepted.2.required("request").unwrap()).unwrap(),
                hash_json(&request).unwrap()
            );
            assert_eq!(
                accepted.2.get("requestHash"),
                Some(&hash_json(&request).unwrap().into())
            );
            assert_eq!(
                hash_json(accepted.2.required("actor").unwrap()).unwrap(),
                hash_json(&Json::object([
                    (
                        "principalId",
                        Json::String(context(generation).caller_principal_id)
                    ),
                    ("serviceNodeId", "fixture-task-board".into()),
                    ("generation", Json::Number(generation as f64)),
                    ("source", "user".into()),
                ]))
                .unwrap()
            );
            assert_eq!(count(&fixture, "active_gates"), 1);
            // A lost public observer still leaves an admitted borrowed read.
            drop(service.control.handler("categories")(
                Json::Null,
                c(generation, "lost-category-observer"),
            ));
            until(|| scenario.reading[index].get()).await;
            let revisions = count(&fixture, "revisions");
            let expected = source
                .required(if generation == 1 {
                    "offline"
                } else {
                    "closing"
                })
                .unwrap();
            if generation == 1 {
                rpc(&connection, "test.close", Json::Null).await.unwrap();
                bounded(connection.closed()).await;
                until(|| peer.methods(2).contains(&"registry.sync".into())).await;
            } else {
                // The actual SDK stop and local drain survive a lost observer.
                drop(service.close());
                bounded(connection.closed()).await;
            }
            assert_error(
                service.control.handler("update")(request.clone(), c(generation, id))
                    .await
                    .unwrap_err(),
                expected.required("handler").unwrap(),
            );
            assert_error(
                service.control.readiness(&connection).await.unwrap_err(),
                expected.required("readiness").unwrap(),
            );
            assert_eq!(count(&fixture, "revisions"), revisions);
            assert_eq!(
                fixture.rows("task-board/operation").len(),
                generation as usize
            );
            assert_eq!(fixture.opens.get(), generation);
            assert_eq!(fixture.drops.borrow().len(), index);
            assert!(!service.client.ready());
            assert!(
                remote.await.unwrap().is_err(),
                "Lost provider reply is not a domain failure"
            );
            scenario.creations[index].release();
            until(|| operation(&fixture, id).2.get("phase") == Some(&"failed".into())).await;
            let completed = operation(&fixture, id);
            assert_eq!(completed.0, accepted.0);
            for key in [
                "operationId",
                "callerPrincipalId",
                "actor",
                "request",
                "requestHash",
                "createdAt",
            ] {
                assert_eq!(
                    completed.2.get(key),
                    accepted.2.get(key),
                    "Changed accepted {key}"
                );
            }
            assert_eq!(completed.2.get("writes"), Some(&Json::Array(vec![])));
            assert_eq!(completed.2.get("outcome"), Some(&Json::Null));
            assert_eq!(
                hash_json(completed.2.required("error").unwrap()).unwrap(),
                hash_json(
                    source
                        .required("domain")
                        .unwrap()
                        .required("terminal")
                        .unwrap()
                        .required("value")
                        .unwrap()
                        .required("error")
                        .unwrap()
                )
                .unwrap()
            );
            scenario.preserved(&fixture);
            assert_eq!(count(&fixture, "active_gates"), 0);
            assert_eq!(
                fixture.drops.borrow().len(),
                index,
                "Borrowed read/tick still hold the graph"
            );
            scenario.categories[index].release();
            until(|| scenario.reads_done[index].get()).await;
            assert_eq!(
                fixture.drops.borrow().len(),
                index,
                "Active tick still holds the graph"
            );
            snapshot(
                &fixture,
                if generation == 1 {
                    "offline-refused-before-drain"
                } else {
                    "close-refused-before-drain"
                },
            );
            scenario.scans[index].release();
            until(|| fixture.drops.borrow().len() == generation as usize).await;
            assert!(scenario.scans_done[index].get());
            if generation == 1 {
                let second = bounded(service.client.wait_ready(None)).await.unwrap();
                assert_eq!(second.generation(), 2);
                let before = (
                    count(&fixture, "revisions"),
                    count(&fixture, "blobs"),
                    fixture.notices.get(),
                );
                let replay = service.control.handler("update")(request.clone(), c(2, id))
                    .await
                    .unwrap_err();
                assert_error(
                    replay,
                    source
                        .required("domain")
                        .unwrap()
                        .required("replay")
                        .unwrap()
                        .required("error")
                        .unwrap(),
                );
                assert_eq!(
                    before,
                    (
                        count(&fixture, "revisions"),
                        count(&fixture, "blobs"),
                        fixture.notices.get()
                    )
                );
            } else {
                bounded(service.close()).await.unwrap();
                assert_error(
                    service.control.reconcile(connection).await.unwrap_err(),
                    expected.required("handler").unwrap(),
                );
            }
            scenario.preserved(&fixture);
        }
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.opens.get(), 2);
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2]);
    assert_eq!(fixture.creations.borrow().as_slice(), [1, 2]);
    assert_eq!(count(&fixture, "active_gates"), 0);
    assert_eq!(fixture.rows("task-board/task").len(), 1);
    assert!(fixture.rows("task-board/history").is_empty());
    assert_eq!(fixture.rows("task-board/result").len(), 1);
    assert_eq!(fixture.rows("task-board/task-key-allocation").len(), 2);
    assert_eq!(fixture.rows("task-board/run").len(), 1);
    assert!(fixture.rows("task-board/native-call").is_empty());
    assert!(fixture.rows("task-board/delivery").is_empty());
    assert!(!peer.methods(1).contains(&"tools.call".into()));
    assert!(!peer.methods(2).contains(&"tools.call".into()));
    snapshot(&fixture, "closed");
    peer.finish().await;
}
