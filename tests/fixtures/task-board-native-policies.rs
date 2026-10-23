use super::*;
use futures_util::FutureExt;
use ivy_sdk::collation::LocaleRuntime;
use ivy_task_board::{
    native_journal::NativeJournal,
    native_policies::{NativePolicies, NativePolicyFactory},
    schema::RuntimeContracts,
};
#[path = "task-board-registry.rs"]
mod registry;

struct PromptBoundary;
impl TaskPrompts for PromptBoundary {
    fn task_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        flag: Option<&Json>,
    ) -> IvyResult<JsString> {
        Ok(ivy_contracts::json::canonical(
            &Json::Array(vec![
                task.clone(),
                Json::String(owner.clone()),
                flag.cloned().unwrap_or_else(|| "omitted".into()),
            ]),
            1024,
        )?
        .into())
    }
    fn task_update_prompt(&self, task: &Json, owner: &JsString, flag: bool) -> IvyResult<JsString> {
        self.task_prompt(task, owner, Some(&flag.into()))
    }
}
struct Probe {
    native: NativePolicyFactory<PromptBoundary>,
    bound: Cell<usize>,
    file: bool,
}
impl PolicyFactory<RuntimeContracts, fn() -> String, fn() -> JsString, SystemClock> for Probe {
    type Policies<'a> =
        NativePolicies<'a, fn() -> String, fn() -> JsString, SystemClock, PromptBoundary>;
    fn bind<'a>(
        &'a self,
        context: PolicyContext<'a, RuntimeContracts, fn() -> String, fn() -> JsString, SystemClock>,
    ) -> IvyResult<Self::Policies<'a>> {
        let policy = self.native.bind(context)?;
        assert!(!policy.scheduler_enabled());
        assert_eq!(policy.scheduler_interval_ms(), 4321.0);
        assert_eq!(policy.page_size(), 37.0);
        assert_eq!(
            SchedulerJournalBindings::free_bytes(&policy)?.is_some(),
            self.file
        );
        if let Some(bytes) = SchedulerJournalBindings::free_bytes(&policy)? {
            assert!(bytes > 0.0);
        }
        assert_eq!(
            CategoryLocale::lowercase_default(&policy, &JsString(vec![0xd800, 65, 0]))?,
            JsString(vec![0xd800, 97, 0])
        );
        assert_eq!(
            CategoryLocale::compare_default(&policy, &"z".into(), &"a".into()),
            Ordering::Greater
        );
        assert_eq!(NativePublicationPolicies::now(&policy), clock().into());
        assert_eq!(DeliveryPolicies::now(&policy), clock().into());
        assert_eq!(DeliveryPolicies::phone_target(&policy), &Json::Null);
        assert!(policy.chat_readable_version(&"fixture-chat-reader".into()));
        assert!(!policy.chat_readable_version(&"1.0.0".into()));

        // These schema and local-query methods are synchronous native work.
        // A pending result would expose an accidental RPC or async fallback.
        let fields = json(
            r#"{"title":"Native policies","description":"","acceptanceCriteria":[],"category":null,"control":"user","priority":2,"executionRequirement":null,"workspaceRequirement":{"kind":"directory_path","path":"/fixture"},"userContact":"ticket","dueAt":null}"#,
        );
        CheckPolicy::fields(&policy, &fields, None)
            .now_or_never()
            .unwrap()?;
        let request = Json::object([
            ("action", "create".into()),
            ("operationId", "native-policy-create".into()),
            ("fields", fields.clone()),
        ]);
        CheckPolicy::creation(&policy, &request, &Json::Null, &Json::Null, &clock().into())
            .now_or_never()
            .unwrap()?;
        assert_eq!(
            CheckPolicy::fields(&policy, &Json::Null, None)
                .now_or_never()
                .unwrap()
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
        assert_eq!(
            CheckPolicy::creation(
                &policy,
                &Json::Null,
                &Json::Null,
                &Json::Null,
                &clock().into()
            )
            .now_or_never()
            .unwrap()
            .unwrap_err()
            .code,
            "invalid_arguments"
        );
        assert_eq!(
            CheckPolicy::aggregate(&policy, &Json::Null, &"task".into())
                .now_or_never()
                .unwrap()
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
        assert_eq!(
            NativePublicationPolicies::aggregate(&policy, &Json::Null, &"task".into())
                .now_or_never()
                .unwrap()
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
        let mut aggregate = json(
            r#"{"schemaVersion":1,"taskKey":"TASK-0001","fields":{},"workflowState":"todo","publication":null,"primaryResourceRef":null,"claim":null,"attemptCount":0,"lastRun":null,"lastHistory":null,"latestResult":null,"acceptedReview":null,"comments":[],"commentDeliveries":[],"agentCommentCount":0,"workRevision":0,"attachments":[],"createdAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","nextSteps":[],"runtimeWait":null}"#,
        );
        aggregate.insert("fields", fields);
        CheckPolicy::aggregate(&policy, &aggregate, &"task".into())
            .now_or_never()
            .unwrap()?;
        NativePublicationPolicies::aggregate(&policy, &aggregate, &"task".into())
            .now_or_never()
            .unwrap()?;
        let page = LifecyclePages::page(
            &policy,
            "task-board/native-call",
            &Json::object([("limit", Json::Number(1.0))]),
        )
        .now_or_never()
        .unwrap()?;
        assert_eq!(page.required("items")?, &Json::Array(vec![]));

        let task =
            json(r#"{"taskKey":"TASK-0001","fields":{"userContact":"ticket"},"comments":[]}"#);
        assert!(TaskNoticePolicy::comment(&policy, &task, &"op".into())?.is_none());
        let comment = json(
            r#"{"purpose":"question","authorKind":"agent","body":"Exact retained text","requests":[]}"#,
        );
        let mut contact_task = task.clone();
        contact_task.insert("fields", Json::object([("userContact", "chat".into())]));
        let mut contact_comment = comment.clone();
        contact_comment.insert("operationId", "op".into());
        contact_task.insert("comments", Json::Array(vec![contact_comment.clone()]));
        assert_eq!(
            TaskNoticePolicy::comment(&policy, &contact_task, &"op".into())?,
            Some(contact_comment)
        );
        assert_eq!(
            DeliveryPolicies::handoff(&policy, &task, &comment)?,
            ivy_task_board::prompts::handoff(&task, &comment)?
        );
        assert_eq!(
            DeliveryPolicies::voice_prompt(&policy, &task, &comment)?,
            ivy_task_board::prompts::voice_prompt(&task, &comment)?
        );
        for flag in [
            None,
            Some(Json::Null),
            Some(Json::Bool(false)),
            Some(Json::Bool(true)),
        ] {
            assert_eq!(
                policy.task_prompt(&task, &"owner".into(), flag.as_ref())?,
                self.native
                    .prompts
                    .task_prompt(&task, &"owner".into(), flag.as_ref())?
            );
        }
        for flag in [false, true] {
            assert_eq!(
                policy.task_update_prompt(&task, &"owner".into(), flag)?,
                self.native
                    .prompts
                    .task_update_prompt(&task, &"owner".into(), flag)?
            );
        }
        self.bound.set(self.bound.get() + 1);
        Ok(policy)
    }
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "task-board-native-policies-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn contracts() -> RuntimeContracts {
    let root = PathBuf::from(
        std::env::var_os("IVY_TASK_BOARD_CURRENT_CONTRACTS")
            .expect("Supply the finite packaged schema fixture"),
    );
    let documents = [
        "task-board",
        "chat",
        "agent",
        "hive-wire",
        "hive-operations",
    ]
    .map(|name| json(&std::fs::read_to_string(root.join(format!("{name}.schema.json"))).unwrap()));
    RuntimeContracts::from_documents(documents.into()).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn native_policy_factory_runs_the_empty_graph_with_actual_journal_and_locale_owners() {
    let runtime = LocaleRuntime::load(&PathBuf::from(
        std::env::var_os("IVY_TEST_LOCALE_RUNTIME").expect("Supply the packaged ICU runtime"),
    ))
    .unwrap();
    for file in [false, true] {
        let peer = Peer::start_mode(Some("--native-policies")).await;
        let root = Root::new();
        let probe = Probe {
            native: NativePolicyFactory {
                journal: NativeJournal::new(file.then(|| root.0.clone()), HistoryLimits::default()),
                locale: runtime.executing_default().unwrap(),
                prompts: PromptBoundary,
                chat_readable_versions: vec!["fixture-chat-reader".into()],
            },
            bound: Cell::new(0),
            file,
        };
        let contracts = contracts();
        let catalogs =
            CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        let factory = EngineFactory {
            contracts: &contracts,
            catalogs: &catalogs,
            policies: &probe,
            open_journal: |_: &Json| probe.native.journal.open(),
            now: clock as fn() -> String,
            new_id: id as fn() -> JsString,
            clock: SystemClock,
            service_node_id: "fixture-task-board".into(),
        };
        let mut setup = configured(&peer);
        let advertisement = registry::fixture(&contracts);
        setup.registry = advertisement.dependency();
        setup.tools = advertisement.tool_names().to_vec();
        setup.settings = json(
            r#"{"principalId":"fixture-principal","rootObjectId":null,"scheduler":{"enabled":false,"intervalMs":4321,"pageSize":37},"phoneTarget":null}"#,
        );
        let (service, owner) = make_service(setup, &contracts).unwrap();
        let script = async {
            service.start();
            until(|| service.client.ready() && peer.methods(1).contains(&"objects.query".into()))
                .await;
            let workspace = service.control.handler("workspace")(Json::Null, context(1))
                .await
                .unwrap();
            assert_eq!(
                workspace
                    .required("scheduler")
                    .unwrap()
                    .required("pageSize")
                    .unwrap(),
                &Json::Number(37.0)
            );
            bounded(service.close()).await.unwrap();
        };
        let (result, ()) = tokio::join!(owner.run(&factory), script);
        result.unwrap();
        let sync = peer
            .frames
            .lock()
            .unwrap()
            .iter()
            .find(|frame| frame.get("method") == Some(&"registry.sync".into()))
            .unwrap()
            .required("params")
            .unwrap()
            .clone();
        assert_eq!(sync, *registry::fixture(&contracts).value());
        assert_eq!(probe.bound.get(), 1);
        let queries = peer
            .frames
            .lock()
            .unwrap()
            .iter()
            .filter(|frame| frame.get("method") == Some(&"objects.query".into()))
            .map(|frame| frame.required("params").unwrap().clone())
            .collect::<Vec<_>>();
        assert!(!queries.is_empty());
        for query in queries {
            let key = query.required("contractKey").unwrap().as_str().unwrap();
            let expected = Json::Array(
                contracts
                    .readable_versions(&key)
                    .unwrap()
                    .iter()
                    .cloned()
                    .map(Json::String)
                    .collect(),
            );
            assert_eq!(query.required("contractVersions").unwrap(), &expected);
        }
        assert_eq!(root.0.join("task-board.sqlite").exists(), file);
        if file {
            let db = Connection::open(root.0.join("task-board.sqlite")).unwrap();
            assert!(db.is_autocommit());
            db.execute_batch("BEGIN EXCLUSIVE; ROLLBACK;").unwrap();
        }
        peer.finish().await;
    }
}
