use super::*;
use ivy_contracts::builtin_schema::SchemaBundle;
use ivy_contracts::schema::{CompiledSchema, SchemaValidators};
use ivy_task_board::journal_store::durable_admission::DurableAdmission;
use std::collections::HashMap;

// Compile the supplied published schemas unchanged. Current creation never
// receives the frozen Task1.8/TaskFields guard used by the legacy fixtures.
struct CurrentContracts {
    records: HashMap<String, (String, Arc<CompiledSchema>)>,
    fields: Arc<CompiledSchema>,
    durable: DurableAdmission,
    admitted: RefCell<Vec<(String, String)>>,
}
impl CurrentContracts {
    fn internal_definition(key: &str) -> Option<&'static str> {
        Some(match key {
            "task-board/operation" => "Operation",
            "task-board/coordination" => "Coordination",
            "task-board/native-read-evidence" => "NativeReadEvidence",
            "task-board/native-turn-snapshot" => "NativeTurnSnapshot",
            "task-board/native-call" => "NativeCall",
            "task-board/native-signals" => "NativeSignals",
            "task-board/scheduler-attempt" => "SchedulerAttempt",
            "task-board/native-evidence" => "NativeEvidence",
            _ => return None,
        })
    }
    fn new() -> Self {
        let definitions = ivy_task_board::schema::task_board_contracts().unwrap();
        let compiler = SchemaValidators::new();
        let mut records = HashMap::new();
        for definition in &definitions {
            let key = definition.required("key").unwrap().as_str().unwrap();
            if records.contains_key(&key) {
                continue;
            }
            if let Some(schema) = definition.get("jsonSchema") {
                records.insert(
                    key,
                    (
                        definition.required("version").unwrap().as_str().unwrap(),
                        compiler.compile(schema).unwrap(),
                    ),
                );
            }
        }
        let task = definitions
            .iter()
            .find(|d| {
                d.get("key") == Some(&"task-board/task".into())
                    && d.get("version") == Some(&"3.1.0".into())
            })
            .unwrap();
        let compile = |name: &str| {
            compiler
                .compile(&Json::object([
                    ("$ref", format!("#/$defs/{name}").into()),
                    (
                        "$defs",
                        task.required("jsonSchema")
                            .unwrap()
                            .required("$defs")
                            .unwrap()
                            .clone(),
                    ),
                ]))
                .unwrap()
        };
        Self {
            records,
            fields: compile("TaskBoardTaskFields"),
            durable: current_durable(),
            admitted: RefCell::default(),
        }
    }
    fn check(schema: &CompiledSchema, value: &Json) -> IvyResult<()> {
        if schema.matches(value)? {
            Ok(())
        } else {
            Err(IvyError::new(
                "invalid_arguments",
                "Current published schema admission failed.",
            ))
        }
    }
}
impl Contracts for CurrentContracts {
    fn version(&self, key: &str) -> IvyResult<String> {
        // Internal journal records retain their existing version identities.
        // Operation admission below uses the supplied current durable definition.
        if Self::internal_definition(key).is_some() {
            return Ok("1.0.0".into());
        }
        Ok(self
            .records
            .get(key)
            .unwrap_or_else(|| panic!("Unbound current JSON contract {key}"))
            .0
            .clone())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &JsString::from(self.version(key)?))
    }
    fn validate_record(&self, key: &str, version: &JsString, value: &Json) -> IvyResult<()> {
        assert_eq!(version, &JsString::from(self.version(key)?));
        self.admitted
            .borrow_mut()
            .push((key.to_owned(), version.to_utf8().unwrap()));
        if key == "task-board/operation" {
            return self.durable.operation(value);
        }
        match Self::internal_definition(key) {
            Some(name) => admission(name, value),
            None => Self::check(&self.records.get(key).unwrap().1, value),
        }
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        match name {
            "Task" => Self::check(&self.records.get("task-board/task").unwrap().1, value),
            "TaskFields" => Self::check(&self.fields, value),
            "Settings" => admission(name, value), // This unchanged finite service setup remains explicit.
            _ => panic!("Unbound current creation definition {name}"),
        }
    }
}
fn current_durable() -> DurableAdmission {
    // Reuse the supplied canonical documents and production validator used by
    // the durable-admission fixture. No current schema is rewritten here.
    let root = PathBuf::from(
        std::env::var_os("IVY_TASK_BOARD_CURRENT_CONTRACTS")
            .expect("Supply the qualified current canonical schema directory"),
    );
    let documents: Vec<&'static str> = [
        "task-board",
        "chat",
        "agent",
        "hive-wire",
        "hive-operations",
    ]
    .into_iter()
    .map(|name| {
        let text = std::fs::read_to_string(root.join(format!("{name}.schema.json"))).unwrap();
        &*Box::leak(text.into_boxed_str())
    })
    .collect();
    DurableAdmission::new(SchemaBundle::new(Box::leak(documents.into_boxed_slice())).unwrap())
}
impl PolicyFactory<CurrentContracts, fn() -> String, fn() -> JsString, retention::FactoryClock>
    for Fixture
{
    type Policies<'a> = Policies<'a, CurrentContracts>;
    fn bind<'a>(&'a self, context: Context<'a, CurrentContracts>) -> IvyResult<Self::Policies<'a>> {
        assert!(!self.refusal);
        assert!(self.native.is_none() && self.retirement.is_none() && self.observations.is_none());
        assert!(self.driver.is_none() && self.control.is_none() && self.steering.is_none());
        assert!(self.scheduler.is_none() && self.delivery.is_none() && self.lifecycle.is_none());
        self.connections
            .borrow_mut()
            .push(context.journal.client.service_connection().unwrap().clone());
        let policy = Policies {
            fixture: self,
            context,
        };
        drop(policy.context.calls.recover_publication()?);
        Ok(policy)
    }
}
fn request() -> Json {
    json(
        r#"{"action":"create","operationId":"current-create","workflowState":"review","fields":{"title":"Current manual task","description":"Current durable material","acceptanceCriteria":[],"category":null,"control":"user","priority":2,"executionRequirement":null,"workspaceRequirement":{"kind":"directory_path","path":"/fixture"},"userContact":"ticket","dueAt":null},"initialResult":{"summary":"Current final material","artifacts":[],"codeReferences":[],"repositoryResult":null,"limitations":[]}}"#,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn current_contract_creation_recovers_reads_and_retires_the_actual_factory_graph() {
    let peer = Peer::start_mode(Some("--factory")).await;
    let fixture = Fixture::new(false);
    let contracts = CurrentContracts::new();
    assert_eq!(contracts.version("task-board/task").unwrap(), "3.1.0");
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let factory = EngineFactory {
        maintenance: None,
        contracts: &contracts,
        catalogs: &catalogs,
        policies: &fixture,
        open_journal: |settings: &Json| fixture.open(settings),
        now: clock as fn() -> String,
        new_id: id as fn() -> JsString,
        clock: fixture.clock.clone(),
        service_node_id: "fixture-task-board".into(),
    };
    let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
    let original = request();
    contracts.durable.request(&original).unwrap();
    let mut legacy_content = original.clone();
    let mut content = legacy_content.required("initialResult").unwrap().clone();
    content.insert("checks", Json::Array(vec![]));
    legacy_content.insert("initialResult", content);
    assert!(contracts.durable.request(&legacy_content).is_err());
    let script = async {
        service.start();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        let remote = {
            let first = first.clone();
            let input = original.clone();
            tokio::spawn(async move { rpc(&first, "test.invokeAction", input).await })
        };
        until(|| fixture.creations.borrow().as_slice() == [1] || remote.is_finished()).await;
        if remote.is_finished() {
            panic!(
                "Creation ended before its publication hold: {:?}",
                remote.await
            );
        }
        let retained = fixture.rows("task-board/operation");
        assert_eq!(retained.len(), 1);
        assert_eq!(
            hash_json(retained[0].2.required("request").unwrap()).unwrap(),
            hash_json(&original).unwrap()
        );
        remote.abort();
        assert!(remote.await.unwrap_err().is_cancelled());
        fixture.held.release();
        until(|| {
            fixture.rows("task-board/operation")[0].2.get("phase")
                == Some(&"needs_attention".into())
        })
        .await;
        let first_task = fixture.rows("task-board/task");
        assert_eq!(first_task.len(), 1);
        assert_eq!(first_task[0].1, 1);
        assert_eq!(first_task[0].2.get("wakeAt"), Some(&Json::Null));
        assert_eq!(
            first_task[0].2.get("dependencies"),
            Some(&Json::Array(vec![]))
        );
        assert_eq!(
            first_task[0].2.get("observedDependencies"),
            Some(&Json::object(Vec::<(&str, Json)>::new()))
        );
        assert!(first_task[0].2.get("nextSteps").is_none());
        assert_eq!(first_task[0].2.get("runtimeWait"), Some(&Json::Null));
        assert!(first_task[0].2.get("waiting").is_none());
        let control = service.control.clone();
        std::thread::spawn(move || {
            drop(control.handler("categories")(
                Json::Null,
                c(1, "held-categories"),
            ))
        })
        .join()
        .unwrap();
        until(|| fixture.page_waiting.get()).await;
        bounded(rpc(
            &first,
            "test.close",
            Json::object(Vec::<(&str, Json)>::new()),
        ))
        .await
        .unwrap();
        until(|| peer.methods(2).contains(&"registry.sync".into())).await;
        assert_eq!(fixture.opens.get(), 1);
        assert!(fixture.drops.borrow().is_empty());
        fixture.page_held.release();
        until(|| fixture.opens.get() == 2).await;
        let second = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(second.generation(), 2);
        until(|| {
            fixture.rows("task-board/operation")[0].2.get("phase") == Some(&"succeeded".into())
        })
        .await;
        assert_eq!(fixture.drops.borrow().as_slice(), [1]);
        assert_eq!(fixture.creations.borrow().as_slice(), [1]);
        let saved = fixture.rows("task-board/operation");
        assert_eq!(saved[0].0, retained[0].0);
        assert_eq!(
            saved[0].2.get("requestHash"),
            Some(&hash_json(&original).unwrap().into())
        );
        assert_eq!(saved[0].2.get("actor"), retained[0].2.get("actor"));
        assert_eq!(
            saved[0]
                .2
                .required("writes")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            4
        );
        let task = fixture.rows("task-board/task");
        assert_eq!(
            (task[0].0.as_str(), task[0].1),
            (first_task[0].0.as_str(), 2)
        );
        assert_eq!(task[0].2.get("publication"), Some(&Json::Null));
        assert_eq!(task[0].2.get("taskKey"), Some(&"TASK-0001".into()));
        assert_eq!(fixture.rows("task-board/result").len(), 1);
        assert_eq!(fixture.rows("task-board/history").len(), 1);
        assert_eq!(fixture.rows("task-board/task-key-allocation").len(), 1);
        let revisions: i64 = fixture
            .database()
            .query_row("SELECT count(*) FROM revisions", [], |r| r.get(0))
            .unwrap();
        let notices = fixture.notices.get();
        let replay = service.control.handler("update")(original.clone(), c(2, "current-create"))
            .await
            .unwrap();
        assert_eq!(
            hash_json(&replay).unwrap(),
            hash_json(saved[0].2.required("outcome").unwrap()).unwrap()
        );
        assert_eq!(
            fixture
                .database()
                .query_row("SELECT count(*) FROM revisions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            revisions
        );
        assert_eq!(fixture.notices.get(), notices);
        let read = service.control.handler("read")(
            Json::object([("task", Json::from(task[0].0.clone()))]),
            c(2, "current-read"),
        )
        .await
        .unwrap();
        assert_eq!(read.required("task").unwrap(), &task[0].2);
        assert_eq!(
            read.required("executionCondition").unwrap().get("state"),
            Some(&"needs_user".into())
        );
        assert_eq!(
            read.required("commentPage").unwrap().get("total"),
            Some(&Json::Number(0.0))
        );
        // The current Task read does not warm categories. Invoke that actual
        // projection before asserting its acknowledgement after retirement.
        service.control.handler("categories")(Json::Null, c(2, "current-categories"))
            .await
            .unwrap();
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.opens.get(), 2);
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2]);
    assert_eq!(
        fixture
            .database()
            .query_row("SELECT count(*) FROM active_gates", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        contracts
            .admitted
            .borrow()
            .iter()
            .filter(|(key, _)| key == "task-board/task")
            .count()
            >= 2
    );
    assert!(
        contracts
            .admitted
            .borrow()
            .iter()
            .filter(|(key, _)| key == "task-board/task")
            .all(|(_, version)| version == "3.1.0")
    );
    assert!(peer.methods(2).contains(&"events.ack".into()));
    peer.finish().await;
}
