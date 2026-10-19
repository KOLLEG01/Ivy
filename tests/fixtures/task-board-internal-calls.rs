use super::*;
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    engine_execution::ReadScopeExecutor,
    inspection_journal::InspectionJournal,
    journal_store::{JournalStore, ReadScopes},
    local_executor::LocalExecutor,
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    native_comments::{NativeCommentJournal, NativeCommentStore},
    native_driver::{DriverRuntime, NativeDriverRuntime},
};

fn probe_pin() -> Json {
    Json::object([
        ("objectId", "probe".into()),
        ("revision", Json::Number(1.0)),
    ])
}
fn probe_value() -> Json {
    Json::object([("probe", true.into())])
}
// A finite public read admission/transport, independent of the action replay.
struct ProbeContracts;
impl Contracts for ProbeContracts {
    fn version(&self, key: &str) -> IvyResult<String> {
        assert_eq!(key, "task-board/task");
        Ok("1.8.0".into())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        self.version(key)?;
        Ok(false)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &JsString::from(self.version(key)?))
    }
    fn validate_record(&self, key: &str, _: &JsString, value: &Json) -> IvyResult<()> {
        self.version(key)?;
        assert_eq!(value, &probe_value());
        Ok(())
    }
    fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
        panic!("No probe action")
    }
}
#[derive(Default)]
struct ProbeClient(AtomicUsize);
impl RpcClient for ProbeClient {
    fn request<'a>(&'a self, method: &'a str, args: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(method, "objects.read");
            assert_eq!(args, probe_pin());
            self.0.fetch_add(1, Ordering::SeqCst);
            let value = probe_value();
            Ok(Json::object([
                (
                    "object",
                    Json::object([
                        ("id", "probe".into()),
                        ("contractKey", "task-board/task".into()),
                        ("parentId", "root".into()),
                        ("effectivelyArchived", false.into()),
                    ]),
                ),
                (
                    "revision",
                    Json::object([
                        ("revision", Json::Number(1.0)),
                        ("contractVersion", "1.8.0".into()),
                        ("mediaType", "application/json".into()),
                        ("contentHash", hash_json(&value)?.into()),
                        (
                            "byteLength",
                            Json::Number(
                                ivy_contracts::json::canonical(&value, MANAGEMENT_FRAME_BYTES)?
                                    .len() as f64,
                            ),
                        ),
                    ]),
                ),
                (
                    "content",
                    Json::object([("encoding", "json".into()), ("value", value)]),
                ),
            ]))
        })
    }
}

// The execution graph owns the driver whose callback calls back into that graph.
// Only the last publication is a finite source replay; both admissions and
// execution queues, the callback bridge and their local executor are production.
struct DriverProgram<'a, D, S> {
    program: Program<'a>,
    driver: D,
    probe: &'a S,
}
impl<D, S: Store> ExecutionProgram for DriverProgram<'_, D, S> {
    async fn run(&self, operation: Document) -> IvyResult<Json> {
        for _ in 0..2 {
            assert_eq!(
                self.probe
                    .read("task-board/task", &probe_pin(), true)
                    .await?
                    .value,
                probe_value()
            );
        }
        self.program.run(operation).await
    }
}
#[tokio::test(flavor = "current_thread")]
async fn cyclic_driver_graph_keeps_queued_originals_across_retirement_and_abandoned_replies() {
    for (method, name) in [
        ("nativeUpdate", "internal nativeUpdate"),
        ("nativeComment", "internal nativeComment"),
        ("schedule", "internal schedule"),
        ("recoverOperation", "recover exact operation"),
        (
            "recoverPublication",
            "pending publication resumes exact operation",
        ),
    ] {
        let row = data()
            .as_array()
            .unwrap()
            .iter()
            .find(|r| s(r, "name") == &JsString::from(name))
            .unwrap();
        let replay = Arc::new(Replay {
            steps: Mutex::new(
                f(row, "boundaries")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            name: s(row, "name").clone(),
        });
        let owner_gate = Arc::new(Gate::default());
        let client = Arc::new(PendingClient {
            client: Client(replay.clone()),
            first: AtomicBool::new(true),
            gate: owner_gate.clone(),
        });
        let catalogs =
            CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        let bindings = Bindings(replay.clone());
        let runtime = AdmissionRuntime {
            client: client.clone(),
            contracts: &bindings,
            catalogs: &catalogs,
        };
        let owner = EngineOwner {
            service_node_id: "task-node".into(),
            generation: 7.0,
        };
        let operations = Operations {
            store: World {
                replay: replay.clone(),
                root: f(row, "root").as_js_string().cloned(),
            },
            now: || "2026-01-02T03:04:05.006Z".to_owned(),
        };
        let execution_gate = Gate::default();
        let scopes = ReadScopes::default();
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let initialized_changes = db.total_changes();
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: Arc::new(ProbeClient::default()),
            contracts: ProbeContracts,
            limits: HistoryLimits::default(),
            now: || "2026-01-02T03:04:05.006Z".to_owned(),
            new_id: || -> JsString { panic!("No probe write") },
            cache: Some(scopes.cache()),
        };
        let inspection = InspectionJournal::new(&journal, "owner".into(), "task-node".into());
        // Declare in journal-release order, initialize in dependency order.
        // The callback handle contains no engine/executor reference.
        let program;
        let execution_executor = LocalExecutor::default();
        let execution_scoped = ReadScopeExecutor {
            scopes: &scopes,
            executor: &execution_executor,
        };
        let execution;
        let engine;
        let executor = LocalExecutor::default();
        let scoped = ReadScopeExecutor {
            scopes: &scopes,
            executor: &executor,
        };
        let invocations = EngineInvocations::new(&scoped);
        let (internal, internal_owner) = invocations.internal_calls(&scopes);
        let update = internal.clone();
        program = DriverProgram {
            program: Program {
                bindings: &bindings,
                gate: &execution_gate,
                calls: Cell::new(0),
            },
            driver: DriverRuntime::new(
                client,
                "owner".into(),
                "task-node".into(),
                move |request: Json| {
                    let observer = update.native_update(&request);
                    async move { observer?.await }
                },
                || "2026-01-02T03:04:05.006Z".into(),
            ),
            probe: &journal,
        };
        execution = ExecutionQueue::new(&program, &execution_scoped);
        engine = EngineAdmission::new(
            &operations,
            f(row, "settings"),
            &owner,
            &runtime,
            &execution,
        )
        .unwrap();
        let step = &f(row, "steps").as_array().unwrap()[0];
        let mut request = f(step, "request").clone();
        let mut object_id = f(step, "objectId")
            .as_js_string()
            .cloned()
            .unwrap_or_default();
        // Exercise DriverRuntime's actual required nativeUpdate callback.
        // Other methods submit before the receiver/root has ever been polled.
        let observer = scopes
            .with_read_cache(async {
                journal
                    .read("task-board/task", &probe_pin(), true)
                    .await
                    .unwrap();
                if method == "nativeUpdate" {
                    let mut call = Box::pin(program.driver.native_update(&request));
                    poll_fn(|cx| {
                        assert!(call.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                    drop(call);
                    None
                } else if method == "nativeComment" {
                    let comments = NativeCommentJournal {
                        inspection: &inspection,
                        native_comment: |request: Json| {
                            let observer = internal.native_comment(&request);
                            async move { observer?.await }
                        },
                    };
                    let mut call = Box::pin(comments.native_comment(&request));
                    poll_fn(|cx| {
                        assert!(call.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                    drop(call);
                    None
                } else {
                    Some(
                        match method {
                            "schedule" => internal.schedule(&request),
                            "recoverOperation" => internal.recover_operation(&object_id),
                            "recoverPublication" => internal.recover_publication(),
                            _ => unreachable!(),
                        }
                        .unwrap(),
                    )
                }
            })
            .await;
        // A distinct action must not reuse the submitted action's map. Its map
        // must not replace the queued call's original context either.
        scopes
            .with_read_cache(journal.read("task-board/task", &probe_pin(), true))
            .await
            .unwrap();
        assert_eq!(journal.client.0.load(Ordering::SeqCst), 2);
        if matches!(request, Json::Object(_)) {
            request.insert("reason", "changed after submission".into());
        }
        object_id.0.clear();
        let stop = invocations.stop();
        assert_eq!(
            internal.native_update(&Json::Null).err().unwrap().code,
            "service_not_ready"
        );
        assert_eq!(
            internal.native_comment(&Json::Null).err().unwrap().code,
            "service_not_ready"
        );
        assert_eq!(
            internal.schedule(&Json::Null).err().unwrap().code,
            "service_not_ready"
        );
        assert_eq!(
            internal
                .recover_operation(&JsString(vec![0xd800]))
                .err()
                .unwrap()
                .code,
            "service_not_ready"
        );
        assert_eq!(
            internal.recover_publication().err().unwrap().code,
            "service_not_ready"
        );
        let mut driver =
            Box::pin(executor.drive(execution_executor.drive(internal_owner.run(&engine, stop))));
        poll_fn(|cx| {
            assert!(driver.as_mut().poll(cx).is_pending());
            if owner_gate.entered.load(Ordering::SeqCst) == 1 {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
        drop(observer);
        owner_gate.release();
        poll_fn(|cx| {
            assert!(driver.as_mut().poll(cx).is_pending());
            if execution_gate.entered.load(Ordering::SeqCst) == 1 {
                assert_eq!(program.program.calls.get(), 1);
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
        execution_gate.release();
        driver.await;
        execution.settled().await;
        assert_eq!(program.program.calls.get(), 1);
        assert_eq!(
            journal.client.0.load(Ordering::SeqCst),
            2,
            "{method}: nested execution lost original cache"
        );
        journal
            .read("task-board/task", &probe_pin(), true)
            .await
            .unwrap();
        assert_eq!(journal.client.0.load(Ordering::SeqCst), 3);
        assert_eq!(
            db.total_changes(),
            initialized_changes,
            "The probe journal was not written"
        );
        assert!(
            replay.steps.lock().unwrap().is_empty(),
            "{method}: unused source effects"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn losing_unbound_receiver_refuses_fresh_work_and_reports_queued_calls_unknown() {
    let scopes = ReadScopes::default();
    let executor = LocalExecutor::default();
    let invocations = EngineInvocations::new(&executor);
    let (internal, owner) = invocations.internal_calls(&scopes);
    let queued = [
        internal.native_update(&Json::Null).unwrap(),
        internal.native_comment(&Json::Null).unwrap(),
        internal.schedule(&Json::Null).unwrap(),
        internal.recover_operation(&JsString(vec![0xd800])).unwrap(),
        internal.recover_publication().unwrap(),
    ];
    let mut drain = Box::pin(invocations.settled());
    poll_fn(|cx| {
        assert!(drain.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(owner);
    for observer in queued {
        let failure = observer.await.unwrap_err();
        assert_eq!(failure.code_js(), JsString::from("internal_error"));
        assert_eq!(
            failure.message_js(),
            JsString::from("Service invocation failed.")
        );
        assert_eq!(failure.outcome, Outcome::Unknown);
    }
    assert_eq!(
        internal.native_update(&Json::Null).err().unwrap().code,
        "service_not_ready"
    );
    assert_eq!(
        invocations
            .run(async { Ok(Json::Null) })
            .err()
            .unwrap()
            .code,
        "service_not_ready"
    );
    drain.await;
}
