//! Selective consumers retain the same immutable comments and wake decisions.
use super::*;
use ivy_contracts::checked_native_contract::CheckedNativeContracts;
use ivy_sdk::tools::ToolClients;
use ivy_task_board::{
    checks::TaskChecks,
    engine_checks::{CheckPolicy, EngineChecks},
    engine_invocations::EngineInvocations,
    journal_store::ReadScopes,
    lifecycle_journal::LifecycleJournal,
    local_executor::LocalExecutor,
    native_lifecycle::LifecycleDocument,
    scheduler::queued_input_ids,
    task_routing::{RoutingEngine, RoutingRuntime},
};

pub(super) fn refresh(row: &mut Json, contracts: &RuntimeContracts) {
    let key = row
        .required("object")
        .unwrap()
        .required("contractKey")
        .unwrap()
        .as_str()
        .unwrap();
    let version = row
        .required("revision")
        .unwrap()
        .required("contractVersion")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let value = row.required("content").unwrap().required("value").unwrap();
    contracts.validate_record(&key, &version, value).unwrap();
    let hash = hash_json(value).unwrap();
    let bytes = canonical(value, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
    row.get_mut("revision")
        .unwrap()
        .insert("contentHash", hash.into());
    row.get_mut("revision")
        .unwrap()
        .insert("byteLength", Json::Number(bytes as f64));
}
fn fixture(length: usize, repeated: bool, queued: bool) -> Json {
    let mut fixture = conversation_fixture(length, 1);
    let contracts = runtime_inputs::contracts().unwrap();
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    for row in nodes {
        let value = row.get_mut("content").unwrap().get_mut("value").unwrap();
        if let Some(entry) = value.get_mut("entry") {
            let comment = entry.get_mut("comment").unwrap();
            let sequence = comment.required("sequence").unwrap().as_f64().unwrap() as usize;
            match sequence {
                2 | 6 => {
                    comment.insert("authorKind", "agent".into());
                }
                3 => {
                    comment.insert("authorKind", "agent".into());
                    comment.insert("sourceTaskId", "peer".into());
                }
                4 | 5 => {
                    comment.insert("authorKind", "system".into());
                    comment.insert(
                        "body",
                        if sequence == 4 {
                            "Ticket edited"
                        } else {
                            "Other notice"
                        }
                        .into(),
                    );
                }
                12 => {
                    comment.insert("authorKind", "agent".into());
                    comment.insert("purpose", "question".into());
                    comment.insert(
                        "recipient",
                        Json::object([("kind", "task".into()), ("taskId", "peer".into())]),
                    );
                }
                _ => {}
            }
            if repeated && sequence == 2 {
                comment.insert("commentId", "c1".into());
            }
            let comment_id = comment.required("commentId").unwrap().clone();
            if queued && (sequence <= 6 || sequence == length) && !(repeated && sequence == 1) {
                entry.insert(
                    "receipt",
                    Json::object([
                        ("commentId", comment_id),
                        ("operationId", format!("original-{sequence}").into()),
                        ("state", "queued".into()),
                        ("updatedAt", TIME.into()),
                        ("code", Json::Null),
                    ]),
                );
            }
        }
        refresh(row, &contracts);
    }
    let row = fixture.get_mut("task").unwrap();
    let task = row.get_mut("content").unwrap().get_mut("value").unwrap();
    task.get_mut("fields")
        .unwrap()
        .insert("control", "agent".into());
    task.get_mut("commentStore").unwrap().get_mut("summary").unwrap().insert("questions", Json::Array(vec![json(
        r#"{"commentId":"c12","sequence":12,"open":true,"recipient":{"kind":"task","taskId":"peer"}}"#
    )]));
    refresh(row, &contracts);
    fixture
}

#[tokio::test(flavor = "current_thread")]
async fn queued_inputs_keep_receipt_order_and_eligibility_after_journal_reopen() {
    for (length, repeated) in [(65, false), (300, true), (4096, false)] {
        let hive = Arc::new(Hive::new(&fixture(length, repeated, true)));
        let path =
            std::env::temp_dir().join(format!("consumer-queue-{}.sqlite", uuid::Uuid::new_v4()));
        let header;
        let expected: Vec<Json> = vec![
            "c1".into(),
            "c3".into(),
            "c4".into(),
            format!("c{length}").into(),
        ];
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let pages = NoPages;
            let store = LifecycleJournal {
                journal: &journal,
                pages: &pages,
            };
            header = store.read_task_header(&"task".into(), true).await.unwrap();
            assert_eq!(hive.state.lock().unwrap().comment_reads, 0);
            let queued = queued_input_ids(&store, &header).await.unwrap();
            assert_eq!(queued, expected);
            let reads = hive.state.lock().unwrap().comment_reads;
            assert_eq!(queued_input_ids(&store, &header).await.unwrap(), expected);
            assert!(
                repeated || hive.state.lock().unwrap().comment_reads - reads <= 30,
                "Warm selection reads the queued entries, not the full history"
            );
            assert!(
                store
                    .has_task_comment(&header.value, &"task".into(), &"c12".into())
                    .await
                    .unwrap()
            );
            let reads = hive.state.lock().unwrap().comment_reads;
            assert!(
                !store
                    .has_task_comment(&header.value, &"task".into(), &"absent".into())
                    .await
                    .unwrap()
            );
            assert_eq!(hive.state.lock().unwrap().comment_reads, reads);
        }
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let pages = NoPages;
            let store = LifecycleJournal {
                journal: &journal,
                pages: &pages,
            };
            let reads = hive.state.lock().unwrap().comment_reads;
            assert_eq!(queued_input_ids(&store, &header).await.unwrap(), expected);
            assert!(repeated || hive.state.lock().unwrap().comment_reads - reads <= 30);
            let selected = store
                .read_task_comments(&header.value, &"task".into(), &["c12".into(), "c1".into()])
                .await
                .unwrap();
            let sequences: Vec<_> = selected
                .iter()
                .map(|comment| comment.required("sequence").unwrap().as_f64().unwrap() as usize)
                .collect();
            assert_eq!(
                sequences,
                if repeated {
                    vec![1, 2, 12]
                } else {
                    vec![1, 12]
                }
            );
            assert!(
                selected
                    .iter()
                    .any(|comment| comment.get("commentId") == Some(&Json::from("c12"))),
                "Questions without delivery receipts remain readable"
            );
            let receipts = store
                .comment_entries(&header.value, &"task".into(), &["c12".into()])
                .await
                .unwrap();
            assert!(
                receipts.is_empty(),
                "Receipt mutation still receives receipt-bearing entries only"
            );
            println!(
                "consumer selection: {length} comments, repeated={repeated}, queued={}",
                expected.len()
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn missing_cold_comment_root_never_proves_absence_or_empty_queue() {
    let hive = Arc::new(Hive::new(&fixture(65, false, true)));
    hive.state.lock().unwrap().records.remove("node65");
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    let pages = NoPages;
    let store = LifecycleJournal {
        journal: &journal,
        pages: &pages,
    };
    let header = store.read_task_header(&"task".into(), true).await.unwrap();
    assert!(
        store
            .has_task_comment(&header.value, &"task".into(), &"absent".into())
            .await
            .is_err()
    );
    assert!(queued_input_ids(&store, &header).await.is_err());
    assert!(
        store
            .read_task_comments(&header.value, &"task".into(), &["absent".into()])
            .await
            .is_err()
    );
}

struct Policy;
impl CheckPolicy for Policy {
    async fn aggregate(&self, _: &Json, _: &JsString) -> IvyResult<()> {
        panic!("No routing write");
    }
    async fn fields(&self, _: &Json, _: Option<&JsString>) -> IvyResult<()> {
        panic!("No field change");
    }
    async fn creation(&self, _: &Json, _: &Json, _: &Json, _: &JsString) -> IvyResult<()> {
        panic!("No creation");
    }
}
#[tokio::test(flavor = "current_thread")]
async fn routing_runtime_selects_original_question_without_receipt_and_wake_uses_its_summary() {
    let hive = Arc::new(Hive::new(&fixture(4096, false, false)));
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    let pages = NoPages;
    let store = LifecycleJournal {
        journal: &journal,
        pages: &pages,
    };
    let header = store.read_task_header(&"task".into(), true).await.unwrap();
    assert_eq!(hive.state.lock().unwrap().comment_reads, 0);
    assert!(
        !ivy_task_board::wake::should_wake(
            &store,
            &header.value,
            &"2026-01-02T03:05:05.006Z".into()
        )
        .await
        .unwrap(),
        "An open task-recipient question prevents orphan-wait retry even with no user questions"
    );
    assert_eq!(hive.state.lock().unwrap().comment_reads, 0);
    let catalogs = CheckedNativeContracts::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
    );
    let tools = ToolClients::new(hive.clone());
    let checks = EngineChecks {
        core: TaskChecks {
            store: &store,
            catalogs: &catalogs,
            tools: &tools,
        },
        policy: &Policy,
    };
    let executor = LocalExecutor::default();
    let scopes = ReadScopes::default();
    let invocations = EngineInvocations::new(&executor);
    let (calls, _owner) = invocations.internal_calls(&scopes);
    let read = |_: JsString,
                _: bool|
     -> futures_util::future::LocalBoxFuture<'_, IvyResult<LifecycleDocument>> {
        Box::pin(async { panic!("The retained question uses its original header") })
    };
    let routing = RoutingRuntime {
        read: &read,
        checks: &checks,
        client: hive.clone(),
        calls,
    };
    let question = header
        .value
        .required("commentStore")
        .unwrap()
        .required("summary")
        .unwrap()
        .required("questions")
        .unwrap()
        .as_array()
        .unwrap()[0]
        .clone();
    let selected = routing.question_comment(&header, &question).await.unwrap();
    assert_eq!(selected.get("commentId"), Some(&Json::from("c12")));
    assert_eq!(selected.get("body"), Some(&Json::from("retained ")));
    let reads = hive.state.lock().unwrap().comment_reads;
    assert!(routing.has_comment(&header, "c12").await.unwrap());
    assert!(!routing.has_comment(&header, "absent").await.unwrap());
    assert_eq!(hive.state.lock().unwrap().comment_reads, reads);
    let selected_again = routing.question_comment(&header, &question).await.unwrap();
    assert_eq!(selected_again, selected);
    assert!(hive.state.lock().unwrap().comment_reads - reads <= 3);
    let mut wrong = question;
    wrong.insert("sequence", 13.0.into());
    assert_eq!(
        routing
            .question_comment(&header, &wrong)
            .await
            .unwrap_err()
            .code,
        "task_board_comment_mismatch"
    );
    let full = store
        .read("task-board/task", &"task".into(), true)
        .await
        .unwrap();
    assert_eq!(
        ivy_sdk::open_task_questions(&full.value, None).unwrap(),
        vec![selected]
    );
    assert!(
        !ivy_task_board::wake::should_wake(&store, &full.value, &"2026-01-02T03:05:05.006Z".into())
            .await
            .unwrap()
    );
    assert_eq!(hive.state.lock().unwrap().writes, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn recovery_question_guard_keeps_actual_native_run_filter_on_compact_headers() {
    use ivy_task_board::native_recovery::ContextRecovery;
    for (length, native_run) in [(65, None), (300, Some("run")), (4096, Some("older-run"))] {
        let mut fixture = claimed_conversation_fixture(length);
        let contracts = runtime_inputs::contracts().unwrap();
        let recipient = if native_run.is_some() {
            json(r#"{"kind":"user"}"#)
        } else {
            json(r#"{"kind":"task","taskId":"peer"}"#)
        };
        let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
            unreachable!()
        };
        for row in nodes {
            if row.required("object").unwrap().get("id") != Some(&Json::from("node12")) {
                continue;
            }
            let comment = row
                .get_mut("content")
                .unwrap()
                .get_mut("value")
                .unwrap()
                .get_mut("entry")
                .unwrap()
                .get_mut("comment")
                .unwrap();
            comment.insert("authorKind", "agent".into());
            comment.insert("purpose", "question".into());
            comment.insert("body", "Original recovery question".into());
            comment.insert("recipient", recipient.clone());
            if let Some(run) = native_run {
                comment.insert(
                    "run",
                    Json::object([("objectId", run.into()), ("revision", 1.0.into())]),
                );
                comment.insert("turnId", "original-turn".into());
                comment.insert("nativeInput", json(r#"{"identity":{"serviceNodeId":"native-agent","epoch":"epoch","requestId":"input"},"method":"item/tool/requestUserInput","params":{},"observedAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","threadId":"original-thread","turnId":"original-turn","state":"pending","answerOperationId":null,"answerCallerPrincipalId":null,"reply":null,"code":null}"#));
            }
            refresh(row, &contracts);
        }
        let row = fixture.get_mut("task").unwrap();
        let task = row.get_mut("content").unwrap().get_mut("value").unwrap();
        task.insert("claim", Json::Null);
        task.insert("workflowState", "waiting".into());
        task.insert(
            "openQuestionCount",
            if native_run.is_some() { 1.0 } else { 0.0 }.into(),
        );
        task.get_mut("commentStore")
            .unwrap()
            .get_mut("summary")
            .unwrap()
            .insert(
                "questions",
                Json::Array(vec![Json::object([
                    ("commentId", "c12".into()),
                    ("sequence", 12.0.into()),
                    ("open", true.into()),
                    ("recipient", recipient),
                ])]),
            );
        refresh(row, &contracts);
        let hive = Arc::new(Hive::new(&fixture));
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        // Deliberately invalid downstream proof: the current resolver must
        // return before reading it for an ordinary/current-Run question, but
        // must continue to the checked proof for a different Run's input.
        journal
            .journal
            .create(
                "task-board/run",
                &"Probe".into(),
                None,
                &Json::Object(vec![]),
                &TIME.into(),
                &"run".into(),
            )
            .unwrap();
        let header = journal
            .read_task_header(&"task".into(), true)
            .await
            .unwrap();
        assert_eq!(hive.state.lock().unwrap().comment_reads, 0);
        let catalogs = CheckedNativeContracts::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        );
        let recovery = ContextRecovery::current(&journal, &catalogs, "caller".into());
        let missing = recovery.unstarted_missing_context(&header).await;
        let reads = hive.state.lock().unwrap().comment_reads;
        let archived = recovery.unstarted_archived_context(&header).await;
        if native_run == Some("older-run") {
            assert!(
                missing.is_err() && archived.is_err(),
                "A foreign Run's input must not suppress checked recovery proof"
            );
        } else {
            assert_eq!(missing.unwrap(), false);
            assert_eq!(archived.unwrap(), false);
        }
        assert!(
            hive.state.lock().unwrap().comment_reads - reads <= 3,
            "Warm recovery selects only its open question"
        );
        assert_eq!(
            header
                .value
                .required("comments")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(hive.state.lock().unwrap().writes, 0);
        println!(
            "recovery guard: {length} comments, native_run={native_run:?}, original question/Run scope retained"
        );
    }
}

struct Contact {
    owners: std::cell::Cell<usize>,
    prompts: std::cell::RefCell<Vec<Json>>,
    principal: JsString,
    phone: Json,
}
impl ivy_task_board::deliveries::DeliveryPolicies for Contact {
    fn now(&self) -> JsString {
        TIME.into()
    }
    fn phone_target(&self) -> &Json {
        &self.phone
    }
    fn chat_readable_version(&self, _: &JsString) -> bool {
        panic!("No channel read");
    }
    fn handoff(&self, task: &Json, comment: &Json) -> IvyResult<JsString> {
        self.prompts.borrow_mut().push(Json::object([
            ("task", task.clone()),
            ("comment", comment.clone()),
        ]));
        ivy_task_board::prompts::handoff(task, comment)
    }
    fn voice_prompt(&self, _: &Json, _: &Json) -> IvyResult<JsString> {
        panic!("No voice");
    }
}
impl ivy_task_board::deliveries::DeliveryEngine for Contact {
    fn client(&self) -> Arc<dyn RpcClient> {
        panic!("No external contact is dispatched");
    }
    fn principal_id(&self) -> &JsString {
        &self.principal
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        let count = self.owners.get();
        self.owners.set(count + 1);
        if count == 0 {
            Ok(())
        } else {
            Err(IvyError::new(
                "fixture_owner_retired",
                "The test stops before contacting a channel.",
            ))
        }
    }
    async fn new_chat_operation(&self) -> IvyResult<JsString> {
        panic!("Keep original operation");
    }
    fn new_phone_operation(&self) -> IvyResult<JsString> {
        panic!("No phone");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn delivery_reads_its_original_compact_publication_after_reopen_without_current_task_or_old_leaves()
 {
    use ivy_task_board::{
        deliveries::{Deliveries, initial_delivery},
        engine_publication::empty_outcome,
        publication::Publication,
    };
    for length in [65, 300, 4096] {
        let hive = Arc::new(Hive::new(&conversation_fixture(length, 1)));
        let path =
            std::env::temp_dir().join(format!("consumer-contact-{}.sqlite", uuid::Uuid::new_v4()));
        let delivery_pin;
        let original_comment;
        let retained_request;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let ops = Operations {
                store: journal,
                now: edit_clock as fn() -> String,
            };
            let header = ops
                .store
                .read_task_header(&"task".into(), true)
                .await
                .unwrap();
            assert!(
                !ops.store
                    .has_task_comment(&header.value, &"task".into(), &"contact".into())
                    .await
                    .unwrap()
            );
            let mut request = append_request("original-contact", 1.0);
            request.insert("commentId", "contact".into());
            request.insert("body", "Original published contact".into());
            let operation = ops.begin(&request, &actor()).await.unwrap();
            let mut steps = Publication {
                operations: &ops,
                operation: &operation,
                task_written: None,
                index: 0.0,
            };
            let initial = initial_delivery(
                &Json::Null,
                &steps.pin().unwrap(),
                &"task".into(),
                &"contact".into(),
                &"chat".into(),
                &TIME.into(),
            );
            delivery_pin = steps
                .create("task-board/delivery", &initial, &"Original contact".into())
                .await
                .unwrap();
            let mut comment = comment(length + 1, "Original published contact".into());
            comment.insert("commentId", "contact".into());
            comment.insert("delivery", delivery_pin.clone());
            comment.insert("operationId", "original-contact".into());
            let mut task = header.value;
            task.insert("comments", Json::Array(vec![comment.clone()]));
            task.insert(
                "lastHistory",
                json(r#"{"objectId":"original-history","revision":1}"#),
            );
            let written = steps
                .write_task_change(
                    &task,
                    &json(r#"{"objectId":"task","expectedRevision":1}"#),
                    TaskChange::AppendComment,
                )
                .await
                .unwrap();
            let mut outcome = empty_outcome(&"original-contact".into());
            outcome.insert("task", written);
            ops.finish(
                operation
                    .pin
                    .to_json()
                    .required("objectId")
                    .unwrap()
                    .as_js_string()
                    .unwrap(),
                &outcome,
            )
            .await
            .unwrap();
            let target = json(
                r#"{"serviceNodeId":"chat-node","expectedBridge":{"principalId":"chat-owner","callerPrincipalId":"caller","workspaceId":"workspace","definitionHash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"channel":{"adapter":"whatsapp","accountId":"fixture","channelId":"main"}}"#,
            );
            retained_request = Json::object([
                ("action", "notify".into()),
                ("operationId", "original-channel-operation".into()),
                (
                    "expectedBridge",
                    target.required("expectedBridge").unwrap().clone(),
                ),
                ("channel", target.required("channel").unwrap().clone()),
                ("source", task.required("lastHistory").unwrap().clone()),
                (
                    "text",
                    Json::String(ivy_task_board::prompts::handoff(&task, &comment).unwrap()),
                ),
            ]);
            let mut current = initial;
            current.insert("state", "dispatching".into());
            current.insert("chatTarget", target);
            current.insert("operationId", "original-channel-operation".into());
            current.insert("chatRequest", retained_request.clone());
            let delivery_id = delivery_pin
                .required("objectId")
                .unwrap()
                .as_js_string()
                .unwrap();
            let version = ops.store.contract_version("task-board/delivery").unwrap();
            ops.store
                .contracts
                .validate_record("task-board/delivery", &version.into(), &current)
                .unwrap();
            ops.store
                .write(
                    "task-board/delivery",
                    &current,
                    "fixture-contact-state",
                    &Json::object([
                        ("objectId", Json::String(delivery_id.clone())),
                        (
                            "expectedRevision",
                            delivery_pin.required("revision").unwrap().clone(),
                        ),
                    ]),
                )
                .await
                .unwrap();
            original_comment = comment;
        }
        {
            let mut state = hive.state.lock().unwrap();
            state.records.remove("task");
            for n in 1..=length {
                state.records.remove(&format!("node{n}"));
            }
        }
        let db = Connection::open(&path).unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        let contact = Contact {
            owners: std::cell::Cell::new(0),
            prompts: std::cell::RefCell::default(),
            principal: "caller".into(),
            phone: Json::Null,
        };
        let id = delivery_pin
            .required("objectId")
            .unwrap()
            .as_js_string()
            .unwrap();
        let before = journal
            .read("task-board/delivery", &Json::String(id.clone()), true)
            .await
            .unwrap();
        let reads = hive.state.lock().unwrap().comment_reads;
        let writes = hive.state.lock().unwrap().writes;
        let error = Deliveries {
            store: &journal,
            engine: &contact,
        }
        .step(id)
        .await
        .unwrap_err();
        assert_eq!(error.code, "fixture_owner_retired");
        assert_eq!(contact.prompts.borrow().len(), 1);
        let prompts = contact.prompts.borrow();
        let prompt = &prompts[0];
        assert_eq!(prompt.required("comment").unwrap(), &original_comment);
        assert_eq!(
            prompt
                .required("task")
                .unwrap()
                .required("comments")
                .unwrap(),
            &Json::Array(vec![])
        );
        assert!(
            hive.state.lock().unwrap().comment_reads - reads <= 4,
            "Delivery selects its original leaf from the retained root"
        );
        assert_eq!(hive.state.lock().unwrap().writes, writes);
        let after = journal
            .read("task-board/delivery", &Json::String(id.clone()), true)
            .await
            .unwrap();
        assert_eq!(after.pin.to_json(), before.pin.to_json());
        assert_eq!(after.value, before.value);
        assert_eq!(
            after.value.required("chatRequest").unwrap(),
            &retained_request
        );
        drop(prompts);
        drop(journal);
        drop(db);
        std::fs::remove_file(path).unwrap();
        println!(
            "contact publication: {length} original comments, no receipt, original prompt/operation after reopen, no current Task or old leaves required"
        );
    }
}
