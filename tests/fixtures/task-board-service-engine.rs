use super::*;
use ivy_contracts::{checked_native_contract::CheckedNativeContracts, hash::hash_json};
use ivy_task_board::{
    engine_admission::{AdmissionRuntime, EngineAdmission, EngineOwner},
    engine_execution::{ExecutionProgram, ExecutionQueue, ReadScopeExecutor},
    engine_invocations::InternalCalls,
    engine_read::{EngineReads, ReadRuntime, read_failure::ReadFailure},
    engine_service::{EngineService, LocalReadFuture, ReadService, ServiceCategories},
    generation::GenerationServiceRuntime,
    journal_store::{JournalStore, ReadScopes},
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_retention::HistoryLimits,
    operations::{Document, Operations, Store},
};
use std::rc::Rc;

// The handler may retain a borrowed non-Send cause until its provider catch.
struct LocalReadCause<'a> {
    label: &'a str,
    identity: Rc<()>,
    drops: &'a Cell<usize>,
}
impl Drop for LocalReadCause<'_> {
    fn drop(&mut self) {
        assert_eq!(self.label, "borrowed read cause");
        assert_eq!(Rc::strong_count(&self.identity), 2);
        self.drops.set(self.drops.get() + 1);
    }
}

// Exact finite admission for this isolated lifecycle fixture. It does not
// advertise the normative current Settings, Task or action schema/registry.
struct FiniteContracts;
impl Contracts for FiniteContracts {
    fn version(&self, key: &str) -> IvyResult<String> {
        assert_eq!(key, "task-board/operation");
        Ok("1.8.0".into())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        self.version(key)?;
        Ok(true)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &JsString::from(self.version(key)?))
    }
    fn validate_record(&self, key: &str, version: &JsString, value: &Json) -> IvyResult<()> {
        assert!(self.readable_version(key, version)?);
        let request = value.required("request")?;
        assert!(
            ["edit", "nativeUpdate"]
                .iter()
                .any(|a| request.get("action") == Some(&Json::from(*a)))
        );
        assert_eq!(
            value.required("requestHash")?,
            &Json::from(hash_json(request)?)
        );
        Ok(())
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        assert_eq!(name, "Settings");
        assert_eq!(value.get("principalId"), Some(&"fixture-principal".into()));
        Ok(())
    }
}
struct NoPages;
impl LifecyclePages for NoPages {
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("No lifecycle page in finite action execution")
    }
}
struct NoCategories;
impl ServiceCategories for NoCategories {
    async fn categories(&self) -> IvyResult<Json> {
        panic!("No category query in finite action execution")
    }
}
struct EngineRuntime<'a> {
    base: Runtime<'a>,
    internal: InternalCalls<'a>,
}
impl GenerationRuntime for EngineRuntime<'_> {
    fn recovered(&self) -> bool {
        self.base.recovered()
    }
    fn diagnostics(&self) -> IvyResult<Vec<Json>> {
        self.base.diagnostics()
    }
    fn task_changed(&self) {
        self.base.task_changed();
    }
    fn notification(&self, value: &Json) -> bool {
        self.base.notification(value)
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        self.base.verify_owner().await
    }
    async fn tick(&self) {
        assert!(!self.base.stopping.get());
        self.base.event("tick");
        let request = Json::object([
            ("action", "nativeUpdate".into()),
            (
                "operationId",
                format!("private-{}", self.base.connection.generation()).into(),
            ),
        ]);
        // An abandoned internal observer must remain in the activation drain.
        drop(self.internal.native_update(&request).unwrap());
        self.base.event("internal.queued");
        self.base.plan.tick.wait().await;
        self.base.event("tick.done");
    }
    fn stop_admissions(&self) {
        self.base.stop_admissions();
    }
    async fn stop(&self) {
        self.base.stop().await;
    }
}
struct Program<'a, S, C> {
    operations: &'a Operations<S, C>,
    factory: &'a Factory,
    plan: &'a Plan,
    private_gate: &'a Gate,
    generation: u64,
}
impl<S: Store, C: Fn() -> String> ExecutionProgram for Program<'_, S, C> {
    async fn run(&self, operation: Document) -> IvyResult<Json> {
        let native =
            operation.value.required("request")?.get("action") == Some(&"nativeUpdate".into());
        self.factory.event(
            self.generation,
            if native { "private.execute" } else { "call" },
        );
        if native {
            self.private_gate.wait().await;
        } else {
            self.plan.call.wait().await;
        }
        let id = operation.pin.required("objectId")?.as_js_string().unwrap();
        let result = Json::object([
            (
                "operationId",
                operation.value.required("operationId")?.clone(),
            ),
            ("original", operation.value.required("request")?.clone()),
            ("generation", Json::Number(self.generation as f64)),
        ]);
        // Actual retained Operations/CAS/journal terminal write. Domain action
        // publication is deliberately finite here, not a Task policy fallback.
        self.operations.finish(id, &result).await?;
        self.factory.event(
            self.generation,
            if native { "private.write" } else { "write" },
        );
        Ok(result)
    }
}
struct EngineFactory {
    base: Factory,
    private_gates: Vec<Gate>,
    read_gates: Vec<Gate>,
    saved: RefCell<Vec<Json>>,
}
impl GenerationFactory for EngineFactory {
    async fn run(&self, launch: GenerationLaunch) -> IvyResult<()> {
        let GenerationLaunch {
            connection,
            control,
            generation,
            activation,
        } = launch;
        let number = connection.generation();
        let plan = &self.base.plans[number as usize - 1];
        self.base.event(number, "create");
        self.base.connections.borrow_mut().push(connection.clone());
        let database = Connection::open_in_memory().unwrap();
        initialize(&database)?;
        let scopes = ReadScopes::default();
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &database,
                root_object_id: None,
            },
            client: Arc::new(connection.clone()),
            contracts: FiniteContracts,
            limits: HistoryLimits::default(),
            now: || "2026-01-02T00:00:00.000Z".to_owned(),
            new_id: || uuid::Uuid::new_v4().to_string().into(),
            cache: Some(scopes.cache()),
        };
        let pages = NoPages;
        let operations = Operations {
            store: LifecycleJournal {
                journal: &journal,
                pages: &pages,
            },
            now: || "2026-01-02T00:00:00.000Z".to_owned(),
        };
        let catalogs =
            CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        let admission_runtime = AdmissionRuntime {
            client: journal.client.clone(),
            contracts: &journal.contracts,
            catalogs: &catalogs,
            handoff: &|_, _| Box::pin(async { panic!("No handoff in this fixture") }),
        };
        let engine_owner = EngineOwner {
            service_node_id: "fixture-task-board".into(),
            generation: number as f64,
        };
        let read = ReadRuntime {
            client: journal.client.as_ref(),
            contracts: &journal.contracts,
            principal_id: &"fixture-principal".into(),
            service_node_id: &engine_owner.service_node_id,
            generation: number as f64,
        };
        // The graph's references outlive each borrowed executor layer. Only
        // internal handles, containing no engine reference, cross the cycle.
        let now = || JsString::from("2026-01-02T00:00:00.000Z");
        let read_label = String::from("borrowed read cause");
        let read_identity = Rc::new(());
        let cause_drops = Cell::new(0);
        let owner_read;
        let read_service;
        let program;
        let execution_jobs = LocalExecutor::default();
        let execution_scoped = ReadScopeExecutor {
            scopes: &scopes,
            executor: &execution_jobs,
        };
        let execution;
        let admission;
        let runtime;
        let service_runtime;
        let public;
        let invocation_jobs = LocalExecutor::default();
        let invocation_scoped = ReadScopeExecutor {
            scopes: &scopes,
            executor: &invocation_jobs,
        };
        let invocations = EngineInvocations::new(&invocation_scoped);
        let (internal, internal_owner) = invocations.internal_calls(&scopes);
        runtime = EngineRuntime {
            base: Runtime {
                factory: &self.base,
                plan,
                connection: &connection,
                stopping: Cell::new(false),
            },
            internal,
        };
        program = Program {
            operations: &operations,
            factory: &self.base,
            plan,
            private_gate: &self.private_gates[number as usize - 1],
            generation: number,
        };
        execution = ExecutionQueue::new(&program, &execution_scoped);
        admission = EngineAdmission::new(
            &operations,
            activation.settings(),
            &engine_owner,
            &admission_runtime,
            &execution,
        )?;
        service_runtime = GenerationServiceRuntime {
            runtime: &runtime,
            control: &control,
            now: &now,
        };
        public = EngineService {
            admission: &admission,
            reads: EngineReads {
                operations: &operations,
                runtime: &read,
            },
            runtime: &service_runtime,
            categories: &NoCategories,
        };
        owner_read = |arguments: Json,
                      context: InvocationContext|
         -> LocalReadFuture<'_, LocalReadCause<'_>> {
            let label = read_label.as_str();
            let identity = &read_identity;
            let drops = &cause_drops;
            Box::pin(async move {
                self.base.event(number, "read.enter");
                self.read_gates[number as usize - 1].wait().await;
                self.base.event(number, "read.done");
                if arguments.get("refuse") == Some(&Json::Bool(true)) {
                    return Err(ReadFailure::source(LocalReadCause {
                        label,
                        identity: identity.clone(),
                        drops,
                    }));
                }
                Ok(Json::object([
                    ("arguments", arguments),
                    ("generation", Json::Number(context.generation as f64)),
                    ("caller", Json::String(context.caller_principal_id)),
                ]))
            })
        };
        read_service = ReadService {
            read: &owner_read,
            other: &public,
        };
        let result = invocation_jobs
            .drive(execution_jobs.drive(activation.run_engine_with_handler(
                &control,
                generation,
                &read_service,
                &admission,
                &invocations,
                internal_owner,
                &runtime,
                &SystemClock,
                async {
                    execution.settled().await;
                    self.base.event(number, "drain");
                    plan.drain.wait().await;
                    self.base.event(number, "drain.done");
                    Ok(())
                },
            )))
            .await;
        assert_eq!(cause_drops.get(), if number == 1 { 2 } else { 0 });
        assert_eq!(Rc::strong_count(&read_identity), 1);
        drop(invocations);
        drop(invocation_jobs);
        drop(execution);
        drop(execution_jobs);
        let mut statement = database.prepare("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision ORDER BY d.id").unwrap();
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| json(&r.unwrap()))
            .collect::<Vec<_>>();
        let revisions: i64 = database
            .query_row("SELECT count(*) FROM revisions", [], |r| r.get(0))
            .unwrap();
        self.saved.borrow_mut().push(Json::object([
            ("generation", Json::Number(number as f64)),
            ("operations", Json::Array(rows)),
            ("revisions", Json::Number(revisions as f64)),
        ]));
        drop(statement);
        drop(program);
        drop(operations);
        drop(journal);
        database.close().unwrap();
        self.base.event(number, "release");
        result
    }
}
fn action(id: &str) -> Json {
    Json::object([("action", "edit".into()), ("operationId", id.into())])
}
#[tokio::test(flavor = "current_thread")]
async fn engine_activation_reconnect_drains_public_and_abandoned_internal_operations_before_sqlite_release()
 {
    let peer = Peer::start_with_engine(true).await;
    let mut options = setup(&peer, false);
    options
        .settings
        .insert("principalId", "fixture-principal".into());
    options.settings.insert("phoneTarget", Json::Null);
    options.tools.push("update".into());
    options.tools.push("read".into());
    let (service, owner) = make_service(options, &FiniteContracts).unwrap();
    let open = Gate::default();
    open.release();
    let factory = EngineFactory {
        base: Factory::new(vec![Plan::default(), Plan::opened()]),
        private_gates: vec![Gate::default(), open],
        read_gates: vec![Gate::default(), {
            let gate = Gate::default();
            gate.release();
            gate
        }],
        saved: RefCell::default(),
    };
    let script = async {
        service.start();
        until(|| factory.base.count(1, "verify") == 1).await;
        factory.base.plans[0].verify.release();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        until(|| factory.base.count(1, "private.execute") == 1).await;
        let read =
            service.control.handler("read")(Json::object([("refuse", true.into())]), context(1));
        drop(service.control.handler("read")(
            Json::object([("refuse", true.into())]),
            context(1),
        ));
        until(|| factory.base.count(1, "read.enter") == 2).await;
        let workspace = rpc(&first, "test.invoke", json("{}")).await.unwrap();
        let workspace = workspace.get("result").unwrap();
        assert_eq!(
            workspace.get("principalId"),
            Some(&"fixture-principal".into())
        );
        assert_eq!(workspace.get("generation"), Some(&Json::Number(1.0)));
        assert_eq!(
            workspace.get("callerPrincipalId"),
            Some(&Json::String(context(1).caller_principal_id))
        );
        let remote = {
            let selected = first.clone();
            tokio::spawn(
                async move { rpc(&selected, "test.invokeAction", action("public-1")).await },
            )
        };
        until(|| factory.base.count(1, "call") == 1).await;
        let entry = service.control.handler("update");
        std::thread::spawn(move || {
            let mut c = context(1);
            c.operation_id = Some("abandoned-1".into());
            drop(entry(action("abandoned-1"), c));
        })
        .join()
        .unwrap();
        until(|| factory.base.count(1, "call") == 2).await;
        rpc(&first, "test.close", Json::Null).await.unwrap();
        bounded(first.closed()).await;
        until(|| peer.methods(2).contains(&"notifications.subscribe".into())).await;
        assert_eq!(factory.base.count(2, "create"), 0);
        assert!(remote.await.unwrap().is_err());
        factory.base.plans[0].call.release();
        until(|| factory.base.count(1, "write") == 2).await;
        // Accepted reads are public handlers, so they settle before stop begins.
        assert_eq!(factory.base.count(1, "stop"), 0);
        assert_eq!(factory.base.count(1, "drain"), 0);
        assert_eq!(factory.base.count(1, "release"), 0);
        assert_eq!(factory.base.count(2, "create"), 0);
        assert_eq!(
            service.control.handler("read")(Json::Null, context(1))
                .await
                .unwrap_err()
                .code,
            "service_not_ready"
        );
        assert_eq!(factory.base.count(1, "read.enter"), 2);
        factory.read_gates[0].release();
        let error = read.await.unwrap_err();
        assert_eq!(error.code, "internal_error");
        assert_eq!(error.message, "Service invocation failed.");
        assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
        until(|| factory.base.count(1, "read.done") == 2 && factory.base.count(1, "stop") == 1)
            .await;
        factory.base.plans[0].stop.release();
        factory.base.plans[0].tick.release();
        until(|| {
            factory.base.count(1, "stop.done") == 1 && factory.base.count(1, "tick.done") == 1
        })
        .await;
        assert_eq!(factory.base.count(1, "private.write"), 0);
        assert_eq!(factory.base.count(1, "drain"), 0);
        assert_eq!(factory.base.count(2, "create"), 0);
        factory.private_gates[0].release();
        until(|| factory.base.count(1, "drain") == 1).await;
        assert_eq!(factory.base.count(1, "private.write"), 1);
        assert_eq!(factory.base.count(1, "release"), 0);
        factory.base.plans[0].drain.release();
        let second = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(second.generation(), 2);
        assert_eq!(factory.base.count(1, "release"), 1);
        let arguments = Json::object([("fresh", true.into())]);
        assert_eq!(
            service.control.handler("read")(arguments.clone(), context(2))
                .await
                .unwrap(),
            Json::object([
                ("arguments", arguments),
                ("generation", Json::Number(2.0)),
                ("caller", Json::String(context(2).caller_principal_id))
            ])
        );
        let result = rpc(&second, "test.invokeAction", action("public-2"))
            .await
            .unwrap();
        assert_eq!(
            result.get("result").unwrap().get("original"),
            Some(&action("public-2"))
        );
        until(|| factory.base.count(2, "private.write") == 1).await;
        assert!(
            service.control.handler("workspace")(Json::Null, context(1))
                .await
                .is_err()
        );
        drop(service.close());
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    result.unwrap();
    let saved = factory.saved.borrow();
    let rows = saved[0].get("operations").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(saved[0].get("revisions"), Some(&Json::Number(6.0)));
    let mut identities = rows
        .iter()
        .map(|row| row.required("operationId").unwrap().as_str().unwrap())
        .collect::<Vec<_>>();
    identities.sort();
    assert_eq!(identities, ["abandoned-1", "private-1", "public-1"]);
    for row in rows {
        assert_eq!(row.get("phase"), Some(&"succeeded".into()));
        assert_eq!(
            row.required("requestHash").unwrap(),
            &Json::from(hash_json(row.required("request").unwrap()).unwrap())
        );
        let native = row.required("operationId").unwrap() == &Json::from("private-1");
        let expected = if native {
            Json::object([
                ("action", "nativeUpdate".into()),
                ("operationId", "private-1".into()),
            ])
        } else {
            action(&row.required("operationId").unwrap().as_str().unwrap())
        };
        assert_eq!(row.get("request"), Some(&expected));
        assert_eq!(
            row.required("outcome").unwrap().get("original"),
            Some(&expected)
        );
        let actor = row.required("actor").unwrap();
        assert_eq!(
            actor.get("source"),
            Some(&Json::from(if native { "native" } else { "user" }))
        );
        assert_eq!(actor.get("generation"), Some(&Json::Number(1.0)));
        assert_eq!(
            actor.get("principalId"),
            Some(&Json::String(if native {
                "fixture-principal".into()
            } else {
                context(1).caller_principal_id
            }))
        );
    }
    assert_eq!(
        saved[1]
            .get("operations")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(saved[1].get("revisions"), Some(&Json::Number(4.0)));
    drop(saved);
    peer.finish().await;
}
