//! Native observation writes use original receipt leaves and retained stages.
use super::*;
use ivy_task_board::{
    lifecycle_journal::LifecycleJournal,
    native_lifecycle::LifecycleStore,
    native_publication::{
        NativePublicationEngine, NativePublicationEvidence, NativePublicationPolicies,
        NativePublisher,
    },
    publication::Publication,
};

fn fixture(length: usize, duplicate: bool, selected: bool) -> Json {
    let mut fixture = claimed_conversation_fixture(length);
    let contracts = runtime_inputs::contracts().unwrap();
    let ids = if !selected {
        vec![]
    } else if duplicate {
        vec!["c1".into()]
    } else {
        vec!["c1".into(), format!("c{length}").into()]
    };
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    for row in nodes {
        let name = row
            .required("object")
            .unwrap()
            .required("id")
            .unwrap()
            .as_str()
            .unwrap();
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
        let value = row.get_mut("content").unwrap().get_mut("value").unwrap();
        if value.get("kind").is_some_and(|v| v.is_str("comment")) {
            let comment = value.get_mut("entry").unwrap().get_mut("comment").unwrap();
            if comment.get("body").is_some_and(|v| v.is_str("")) {
                comment.insert("body", format!("Retained comment {name}").into());
            }
        }
        if name == "node1" || name == format!("node{length}") {
            let entry = value.get_mut("entry").unwrap();
            if duplicate {
                entry
                    .get_mut("comment")
                    .unwrap()
                    .insert("commentId", "c1".into());
            }
            entry.insert(
                "receipt",
                Json::object([
                    (
                        "commentId",
                        entry
                            .required("comment")
                            .unwrap()
                            .required("commentId")
                            .unwrap()
                            .clone(),
                    ),
                    ("operationId", format!("original-{name}").into()),
                    ("state", "queued".into()),
                    ("updatedAt", TIME.into()),
                    ("code", Json::Null),
                ]),
            );
        }
        if name == "node2" {
            let comment = value.get_mut("entry").unwrap().get_mut("comment").unwrap();
            comment.insert("purpose", "question".into());
            comment.insert("recipient", Json::object([("kind", "user".into())]));
        }
        if name == "run" {
            value
                .get_mut("originalRequest")
                .unwrap()
                .insert("commentIds", Json::Array(ids.clone()));
            value.insert(
                "requestHash",
                hash_json(value.required("originalRequest").unwrap())
                    .unwrap()
                    .into(),
            );
        }
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
    let row = fixture.get_mut("task").unwrap();
    let task = row.get_mut("content").unwrap().get_mut("value").unwrap();
    let claim = task.get_mut("claim").unwrap();
    claim
        .get_mut("request")
        .unwrap()
        .insert("commentIds", Json::Array(ids));
    claim.insert(
        "requestHash",
        hash_json(claim.required("request").unwrap())
            .unwrap()
            .into(),
    );
    task.get_mut("commentStore")
        .unwrap()
        .get_mut("summary")
        .unwrap()
        .insert(
            "questions",
            Json::Array(vec![json(
                r#"{"commentId":"c2","sequence":2,"open":true,"recipient":{"kind":"user"}}"#,
            )]),
        );
    task.insert("openQuestionCount", Json::Number(1.0));
    runtime_inputs::contracts()
        .unwrap()
        .validate_task(task)
        .unwrap();
    let hash = hash_json(task).unwrap();
    let bytes = canonical(task, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
    row.get_mut("revision")
        .unwrap()
        .insert("contentHash", hash.into());
    row.get_mut("revision")
        .unwrap()
        .insert("byteLength", Json::Number(bytes as f64));
    fixture
}

struct Engine<'a, S> {
    store: &'a S,
    contracts: &'a RuntimeContracts,
    principal: JsString,
    phone_target: Json,
}
impl<S: Store> NativePublicationPolicies for Engine<'_, S> {
    fn now(&self) -> JsString {
        TIME.into()
    }
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()> {
        self.contracts.validate_task(task)?;
        ivy_task_board::engine_validation::check_aggregate(self.store, task, id).await
    }
}
impl<S: Store> NativePublicationEngine for Engine<'_, S> {
    fn principal_id(&self) -> &JsString {
        &self.principal
    }
    fn phone_target(&self) -> &Json {
        &self.phone_target
    }
    fn retry_at(&self, turn: &Json, at: &JsString) -> IvyResult<Json> {
        Ok(
            ivy_task_board::native_rate_limit::native_rate_limit_retry_at(turn, at)?
                .map(Json::String)
                .unwrap_or(Json::Null),
        )
    }
    async fn workspace(&self, _: &Json, run: &Json) -> IvyResult<Json> {
        run.required("workspace").cloned()
    }
}
struct Snapshot;
impl NativePublicationEvidence for Snapshot {
    fn adoption(&self, _: &Json, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No adoption")
    }
    // Evidence verification has its own existing cases. This fixture supplies
    // the admitted observation, while all publication/storage work is real.
    async fn snapshot(&self, _: &Json, _: &Json) -> IvyResult<Json> {
        Ok(json(
            r#"{"document":{"value":{"status":"inProgress","epoch":"epoch"}},"thread":{"status":{"type":"active","activeFlags":[]}}}"#,
        ))
    }
    async fn completion(&self, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No completion")
    }
    fn identity(
        &self,
        _: &Json,
        _: &Json,
        _: &JsString,
    ) -> IvyResult<ivy_task_board::native_intent::EvidenceIdentity> {
        panic!("No native call")
    }
    async fn read(
        &self,
        _: &Json,
        _: &Json,
        _: &ivy_task_board::native_intent::EvidenceIdentity,
    ) -> IvyResult<Json> {
        panic!("No native call")
    }
    async fn intended(&self, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No native call")
    }
    fn resolve(&self, _: &Json) -> IvyResult<Json> {
        panic!("No native call")
    }
}
async fn publish(
    journal: &JournalStore<'_, Hive, RuntimeContracts, fn() -> String, fn() -> JsString>,
    request: &Json,
    lose: bool,
) -> IvyResult<Json> {
    publish_evidence(journal, request, lose, &Snapshot).await
}
async fn publish_evidence<V: NativePublicationEvidence>(
    journal: &JournalStore<'_, Hive, RuntimeContracts, fn() -> String, fn() -> JsString>,
    request: &Json,
    lose: bool,
    evidence: &V,
) -> IvyResult<Json> {
    let ops = Operations {
        store: LifecycleJournal {
            journal,
            pages: &NoPages,
        },
        now: edit_clock as fn() -> String,
    };
    let mut caller = actor();
    caller.insert("source", "native".into());
    let op = ops.begin(request, &caller).await?;
    let engine = Engine {
        store: &ops.store,
        contracts: &journal.contracts,
        principal: "caller".into(),
        phone_target: Json::Null,
    };
    let publisher = NativePublisher {
        store: &ops.store,
        engine: &engine,
        evidence,
    };
    let callback =
        |_: Json, value: Json| -> futures_util::future::LocalBoxFuture<'_, IvyResult<()>> {
            Box::pin(async move {
                if lose && value.get("publication").is_some_and(|v| !v.is_null()) {
                    journal.client.state.lock().unwrap().lost_task_reply = true;
                }
                Ok(())
            })
        };
    let mut steps = Publication {
        operations: &ops,
        operation: &op,
        task_written: Some(&callback),
        index: 0.0,
    };
    let out = publisher.publish(&op, &mut steps, None).await?;
    ops.finish(
        op.pin
            .to_json()
            .required("objectId")?
            .as_js_string()
            .unwrap(),
        &out,
    )
    .await?;
    Ok(out)
}
fn request() -> Json {
    let mut artifact = json(
        r#"{"object":{"objectId":"observation","revision":1},"label":"Native observation","mediaType":"application/json","contentHash":""}"#,
    );
    artifact.insert("contentHash", format!("sha256:{}", "a".repeat(64)).into());
    Json::object([
        ("action", "nativeUpdate".into()),
        ("operationId", "receipt-observation".into()),
        ("taskId", "task".into()),
        ("expectedRevision", Json::Number(1.0)),
        ("run", json(r#"{"objectId":"run","revision":1}"#)),
        (
            "change",
            Json::object([
                ("kind", "readSnapshot".into()),
                ("snapshot", artifact),
                ("notificationCursor", Json::Number(1.0)),
            ]),
        ),
    ])
}

#[tokio::test(flavor = "current_thread")]
async fn nonterminal_host_wait_keeps_the_original_root_without_reading_missing_history() {
    for length in [65, 300, 4096] {
        let hive = Arc::new(Hive::new(&fixture(length, false, false)));
        let original = hive.state.lock().unwrap().records["task"].clone();
        // No unrelated conversation leaf is available. Header-only publication
        // must still retain the root and its references without interpreting absence.
        hive.state.lock().unwrap().records.retain(|_, row| {
            row.required("object").unwrap().get("contractKey")
                != Some(&Json::from("task-board/comment-node"))
        });
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        let mut request = request();
        request.insert("operationId", "nonterminal-host-wait".into());
        request.insert("change", json(r#"{"kind":"unavailable","reason":"host","code":"owner_offline","detail":"The original owner is temporarily unavailable."}"#));
        let out = publish(&journal, &request, false).await.unwrap();
        let writes = hive.state.lock().unwrap().writes;
        assert_eq!(publish(&journal, &request, false).await.unwrap(), out);
        assert_eq!(hive.state.lock().unwrap().writes, writes);
        let state = hive.state.lock().unwrap();
        assert_eq!(state.comment_reads, 0);
        let saved = &state.records["task"];
        for field in [
            "commentStore",
            "fields",
            "attachments",
            "primaryResourceRef",
            "workRevision",
        ] {
            assert_eq!(
                hash_json(
                    saved
                        .required("content")
                        .unwrap()
                        .required("value")
                        .unwrap()
                        .required(field)
                        .unwrap()
                )
                .unwrap(),
                hash_json(
                    original
                        .required("content")
                        .unwrap()
                        .required("value")
                        .unwrap()
                        .required(field)
                        .unwrap()
                )
                .unwrap()
            );
        }
        let references = saved
            .required("revision")
            .unwrap()
            .required("references")
            .unwrap();
        assert_eq!(
            references.get("conversation"),
            original
                .required("revision")
                .unwrap()
                .required("references")
                .unwrap()
                .get("conversation")
        );
        assert_eq!(
            references.get("run"),
            saved
                .required("content")
                .unwrap()
                .required("value")
                .unwrap()
                .get("lastRun")
        );
        println!(
            "native host wait: {length} comments; zero body reads with unavailable leaves; original root/references and replay retained"
        );
    }
}

struct Capture(std::cell::RefCell<Vec<Json>>);
impl ivy_task_board::native_driver::NativeDriverRuntime for Capture {
    async fn verify_owner(&self) -> IvyResult<()> {
        Ok(())
    }
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString> {
        Ok(hash_json(scope)?.into())
    }
    async fn native_update(&self, request: &Json) -> IvyResult<Json> {
        self.0.borrow_mut().push(request.clone());
        Ok(Json::Null)
    }
    fn now(&self) -> JsString {
        TIME.into()
    }
}
struct SteerEvidence<'a, S> {
    intent: ivy_task_board::native_intent::NativeIntent<'a, S>,
    observation: Json,
}
impl<S: Store + ivy_task_board::read_evidence::ReadEvidenceStore> NativePublicationEvidence
    for SteerEvidence<'_, S>
{
    fn adoption(&self, _: &Json, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No adoption")
    }
    async fn snapshot(&self, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No snapshot")
    }
    async fn completion(&self, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No completion")
    }
    fn identity(
        &self,
        run: &Json,
        call: &Json,
        principal: &JsString,
    ) -> IvyResult<ivy_task_board::native_intent::EvidenceIdentity> {
        self.intent.evidence_identity(run, call, principal)
    }
    async fn read(
        &self,
        _: &Json,
        _: &Json,
        _: &ivy_task_board::native_intent::EvidenceIdentity,
    ) -> IvyResult<Json> {
        // Supply the admitted owner observation. This probe performs real local
        // publication and storage, with no provider dispatch or native input.
        Ok(self.observation.clone())
    }
    async fn intended(&self, _: &Json, run: &Json, origin: &Json) -> IvyResult<Json> {
        self.intent
            .intended_call(
                run,
                ivy_task_board::native_intent::NativeSlot::Steer,
                Some(origin),
                &ivy_task_board::prompts::CurrentTaskPrompts,
            )
            .await
    }
    fn resolve(&self, request: &Json) -> IvyResult<Json> {
        self.intent.resolve_native_request(request)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn steering_receipt_reservation_keeps_its_new_root_after_lost_reply_and_reopen() {
    use ivy_contracts::checked_native_contract::CheckedNativeContracts;
    use ivy_task_board::{
        native_driver::NativeDriver,
        native_intent::{NativeIntent, NativeSlot},
        prompts::CurrentTaskPrompts,
    };
    let catalogs = CheckedNativeContracts::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
    );
    for (length, lose_first) in [(65, true), (300, false), (4096, true)] {
        let hive = Arc::new(Hive::new(&fixture(length, false, false)));
        let original = hive.state.lock().unwrap().records["task"]
            .required("content")
            .unwrap()
            .required("value")
            .unwrap()
            .clone();
        let full_before = hydrated(hive.clone(), &original, &"task".into()).await;
        let path = std::env::temp_dir().join(format!("steer-root-{}.sqlite", uuid::Uuid::new_v4()));
        let request;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let lifecycle = LifecycleJournal {
                journal: &journal,
                pages: &NoPages,
            };
            lifecycle
                .comment_entries(&original, &"task".into(), &["c1".into()])
                .await
                .unwrap();
            let capture = Capture(std::cell::RefCell::default());
            let driver = NativeDriver {
                store: &journal,
                runtime: &capture,
                prompts: &CurrentTaskPrompts,
                catalogs: &catalogs,
            };
            let mut origin =
                json(r#"{"kind":"steer","predecessor":null,"workRevision":0,"commentIds":["c1"]}"#);
            origin.insert("peersHash", hash_json(&Json::Array(vec![])).unwrap().into());
            let call = driver
                .prepare(&"run".into(), NativeSlot::Steer, Some(&origin))
                .await
                .unwrap();
            let evidence = SteerEvidence {
                intent: NativeIntent {
                    store: &journal,
                    catalogs: &catalogs,
                },
                observation: json(
                    r#"{"phase":"succeeded","epoch":"epoch","code":null,"reply":{"result":{"turnId":"original-turn"}}}"#,
                ),
            };
            let reads = hive.state.lock().unwrap().comment_reads;
            publish_evidence(&journal, &capture.0.borrow()[0], false, &evidence)
                .await
                .unwrap();
            assert!(
                hive.state.lock().unwrap().comment_reads - reads <= 4,
                "Steer attachment reads only its queued original entry"
            );
            let saved_call = Store::read(&journal, "task-board/native-call", &call, false)
                .await
                .unwrap();
            let mut observed = saved_call.value.clone();
            observed.insert("state", "observed".into());
            observed.insert("ownerPhase", "succeeded".into());
            observed.insert("observedAt", TIME.into());
            observed.insert("epoch", "epoch".into());
            observed.insert("evidence", request_artifact());
            let next_call = journal
                .write(
                    "task-board/native-call",
                    &observed,
                    "observed-steer",
                    &Json::object([
                        ("objectId", call.required("objectId").unwrap().clone()),
                        (
                            "expectedRevision",
                            call.required("revision").unwrap().clone(),
                        ),
                    ]),
                )
                .await
                .unwrap();
            let task = lifecycle
                .task_header(&"task".into(), true, false)
                .await
                .unwrap();
            let run = Store::read(&journal, "task-board/run", &"run".into(), false)
                .await
                .unwrap();
            request = Json::object([
                ("action", "nativeUpdate".into()),
                ("operationId", "observe-original-steer".into()),
                ("taskId", "task".into()),
                (
                    "expectedRevision",
                    task.document
                        .pin
                        .to_json()
                        .required("revision")
                        .unwrap()
                        .clone(),
                ),
                ("run", run.pin.to_json()),
                (
                    "change",
                    Json::object([
                        ("kind", "observeCall".into()),
                        ("slot", "steer".into()),
                        ("call", next_call),
                    ]),
                ),
            ]);
            if lose_first {
                hive.state.lock().unwrap().lost_task_reply = true;
            }
            assert_eq!(
                publish_evidence(&journal, &request, !lose_first, &evidence)
                    .await
                    .unwrap_err()
                    .code,
                "transport_lost"
            );
        }
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let evidence = SteerEvidence {
                intent: NativeIntent {
                    store: &journal,
                    catalogs: &catalogs,
                },
                observation: json(
                    r#"{"phase":"succeeded","epoch":"epoch","code":null,"reply":{"result":{"turnId":"original-turn"}}}"#,
                ),
            };
            let out = publish_evidence(&journal, &request, false, &evidence)
                .await
                .unwrap();
            let writes = hive.state.lock().unwrap().writes;
            assert_eq!(
                publish_evidence(&journal, &request, false, &evidence)
                    .await
                    .unwrap(),
                out
            );
            assert_eq!(hive.state.lock().unwrap().writes, writes);
            let saved = hive.state.lock().unwrap().records["task"]
                .required("content")
                .unwrap()
                .required("value")
                .unwrap()
                .clone();
            assert_eq!(saved.get("openQuestionCount"), Some(&1.0.into()));
            assert_ne!(
                hash_json(saved.required("commentStore").unwrap()).unwrap(),
                hash_json(original.required("commentStore").unwrap()).unwrap()
            );
            let full = hydrated(hive.clone(), &saved, &"task".into()).await;
            assert_eq!(
                hash_json(full.required("comments").unwrap()).unwrap(),
                hash_json(full_before.required("comments").unwrap()).unwrap()
            );
            let receipts = full
                .required("commentDeliveries")
                .unwrap()
                .as_array()
                .unwrap();
            assert_eq!(receipts.len(), 2);
            assert_eq!(receipts[0].get("state"), Some(&"delivered".into()));
            assert_eq!(receipts[1].get("state"), Some(&"queued".into()));
            for (old, new) in full_before
                .required("commentDeliveries")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .zip(receipts)
            {
                assert_eq!(old.get("operationId"), new.get("operationId"));
            }
            println!(
                "native steering: {length} comments; exact new reservation root, loss at first={lose_first}, reopen/replay, original bodies/receipt identities and unrelated queued input retained"
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}
fn request_artifact() -> Json {
    request()
        .required("change")
        .unwrap()
        .required("snapshot")
        .unwrap()
        .clone()
}

fn completion_fixture(length: usize, mode: &str) -> Json {
    let mut value = fixture(
        length,
        false,
        matches!(mode, "answered" | "foreign" | "retry"),
    );
    let contracts = runtime_inputs::contracts().unwrap();
    let mut agent_count = 0;
    let Json::Array(nodes) = value.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    for row in nodes {
        let body = row.get_mut("content").unwrap().get_mut("value").unwrap();
        if let Some(entry) = body.get_mut("entry") {
            let comment = entry.get_mut("comment").unwrap();
            let at = comment.required("sequence").unwrap().as_f64().unwrap() as usize;
            if mode == "handoff" && [3, length - 1, length].contains(&at) {
                comment.insert("authorKind", "agent".into());
                comment
                    .get_mut("author")
                    .unwrap()
                    .insert("source", "worker".into());
                comment.insert("purpose", "handoff".into());
                comment.insert(
                    "run",
                    json(if at == length {
                        r#"{"objectId":"foreign-run","revision":1}"#
                    } else {
                        r#"{"objectId":"run","revision":1}"#
                    }),
                );
                comment.insert(
                    "handoff",
                    Json::object([(
                        "workflowState",
                        if at == length - 1 { "review" } else { "todo" }.into(),
                    )]),
                );
                if at == length - 1 {
                    entry.insert("receipt", json(&format!(r#"{{"commentId":"c{at}","operationId":"original-handoff-receipt","state":"queued","updatedAt":"{TIME}","code":null}}"#)));
                }
                agent_count += 1;
            } else if matches!(mode, "answered" | "foreign") && at == length - 1 {
                comment.insert("authorKind", "agent".into());
                comment
                    .get_mut("author")
                    .unwrap()
                    .insert("source", "worker".into());
                comment.insert("replyTo", "c1".into());
                comment.insert(
                    "run",
                    json(if mode == "foreign" {
                        r#"{"objectId":"foreign-run","revision":1}"#
                    } else {
                        r#"{"objectId":"run","revision":1}"#
                    }),
                );
                agent_count += 1;
            }
            if mode == "retry" {
                if at == 1 || at == length {
                    entry
                        .get_mut("receipt")
                        .unwrap()
                        .insert("state", "delivered".into());
                } else if at == 3 {
                    entry.insert("receipt", json(&format!(r#"{{"commentId":"c3","operationId":"original-unrelated","state":"delivered","updatedAt":"{TIME}","code":null}}"#)));
                }
            }
        }
        super::consumer_reads::refresh(row, &contracts);
    }
    let row = value.get_mut("task").unwrap();
    let task = row.get_mut("content").unwrap().get_mut("value").unwrap();
    task.insert("agentCommentCount", Json::Number(agent_count as f64));
    if mode == "handoff" {
        task.get_mut("fields")
            .unwrap()
            .insert("userContact", "chat".into());
    }
    if mode == "retry" {
        task.insert("executionChangeRequested", true.into());
    }
    super::consumer_reads::refresh(row, &contracts);
    value
}

struct Completed(&'static str);
impl NativePublicationEvidence for Completed {
    fn adoption(&self, _: &Json, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No adoption")
    }
    async fn snapshot(&self, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No snapshot")
    }
    async fn completion(&self, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        Ok(Json::object([
            ("status", self.0.into()),
            ("summary", "Verified original completion".into()),
            (
                "snapshot",
                Json::object([
                    ("document", json(r#"{"value":{"epoch":"epoch"}}"#)),
                    (
                        "turn",
                        Json::object([("status", self.0.into()), ("error", Json::Null)]),
                    ),
                ]),
            ),
        ]))
    }
    fn identity(
        &self,
        _: &Json,
        _: &Json,
        _: &JsString,
    ) -> IvyResult<ivy_task_board::native_intent::EvidenceIdentity> {
        panic!("No native call")
    }
    async fn read(
        &self,
        _: &Json,
        _: &Json,
        _: &ivy_task_board::native_intent::EvidenceIdentity,
    ) -> IvyResult<Json> {
        panic!("No native call")
    }
    async fn intended(&self, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No native call")
    }
    fn resolve(&self, _: &Json) -> IvyResult<Json> {
        panic!("No native call")
    }
}
fn completion_request() -> Json {
    let mut value = request();
    value.insert("operationId", "original-completion".into());
    value.insert(
        "change",
        Json::object([
            ("kind", "completion".into()),
            ("snapshot", request_artifact()),
            ("output", Json::Null),
            ("notificationCursor", 1.0.into()),
        ]),
    );
    value
}

#[tokio::test(flavor = "current_thread")]
async fn compact_completion_keeps_history_receipts_and_handoff_contact_after_lost_reply() {
    for length in [65, 300, 4096] {
        for mode in ["append", "handoff", "answered", "foreign", "retry"] {
            let hive = Arc::new(Hive::new(&completion_fixture(length, mode)));
            let original = hive.state.lock().unwrap().records["task"]
                .required("content")
                .unwrap()
                .required("value")
                .unwrap()
                .clone();
            let full_before = hydrated(hive.clone(), &original, &"task".into()).await;
            let path = std::env::temp_dir()
                .join(format!("native-completion-{}.sqlite", uuid::Uuid::new_v4()));
            let request = completion_request();
            let evidence = Completed(if mode == "retry" {
                "failed"
            } else {
                "completed"
            });
            let lost_first = matches!(mode, "append" | "retry") && length != 300;
            {
                let db = Connection::open(&path).unwrap();
                initialize(&db).unwrap();
                let journal = snapshot_store(&db, hive.clone());
                // Establish the disposable exact-root index, then remove unrelated
                // bodies: the warm publication must not read them.
                journal
                    .read_completion_comments(
                        &original,
                        &"task".into(),
                        &"run".into(),
                        &["c1".into(), format!("c{length}").into()],
                    )
                    .await
                    .unwrap();
                let hidden = {
                    let mut state = hive.state.lock().unwrap();
                    let names = (4..length - 2)
                        .map(|at| format!("node{at}"))
                        .collect::<Vec<_>>();
                    names
                        .into_iter()
                        .map(|name| {
                            let row = state.records.remove(&name).unwrap();
                            (name, row)
                        })
                        .collect::<Vec<_>>()
                };
                let reads = hive.state.lock().unwrap().comment_reads;
                if lost_first {
                    hive.state.lock().unwrap().lost_task_reply = true;
                }
                assert_eq!(
                    publish_evidence(&journal, &request, !lost_first, &evidence)
                        .await
                        .unwrap_err()
                        .code,
                    "transport_lost"
                );
                let warm_reads = hive.state.lock().unwrap().comment_reads - reads;
                assert!(
                    warm_reads <= 32,
                    "{mode}/{length}: warm completion read {warm_reads} nodes"
                );
                // Recovery may cold-index the original immutable root after
                // preparation moved the single-root index to the final root.
                hive.state.lock().unwrap().records.extend(hidden);
            }
            {
                let db = Connection::open(&path).unwrap();
                initialize(&db).unwrap();
                let journal = snapshot_store(&db, hive.clone());
                let out = publish_evidence(&journal, &request, false, &evidence)
                    .await
                    .unwrap();
                let writes = hive.state.lock().unwrap().writes;
                assert_eq!(
                    publish_evidence(&journal, &request, false, &evidence)
                        .await
                        .unwrap(),
                    out
                );
                assert_eq!(hive.state.lock().unwrap().writes, writes);
                let saved = hive.state.lock().unwrap().records["task"]
                    .required("content")
                    .unwrap()
                    .required("value")
                    .unwrap()
                    .clone();
                let full = hydrated(hive.clone(), &saved, &"task".into()).await;
                let old = full_before
                    .required("comments")
                    .unwrap()
                    .as_array()
                    .unwrap();
                let comments = full.required("comments").unwrap().as_array().unwrap();
                let appended = !matches!(mode, "handoff" | "answered");
                assert_eq!(comments.len(), length + usize::from(appended));
                for (before, after) in old.iter().zip(comments) {
                    let mut expected = before.clone();
                    if mode == "handoff"
                        && before.get("sequence") == Some(&Json::Number((length - 1) as f64))
                    {
                        assert!(!after.required("delivery").unwrap().is_null());
                        expected.insert("delivery", after.required("delivery").unwrap().clone());
                    }
                    assert_eq!(hash_json(&expected).unwrap(), hash_json(after).unwrap());
                }
                let old_receipts = full_before
                    .required("commentDeliveries")
                    .unwrap()
                    .as_array()
                    .unwrap();
                let receipts = full
                    .required("commentDeliveries")
                    .unwrap()
                    .as_array()
                    .unwrap();
                assert_eq!(receipts.len(), old_receipts.len());
                for (before, after) in old_receipts.iter().zip(receipts) {
                    let mut expected = before.clone();
                    if mode == "retry"
                        && before.get("operationId") != Some(&Json::from("original-unrelated"))
                    {
                        expected.insert("state", "queued".into());
                        expected.insert("updatedAt", TIME.into());
                        expected.insert("code", after.required("code").unwrap().clone());
                    }
                    assert_eq!(hash_json(&expected).unwrap(), hash_json(after).unwrap());
                }
                assert_eq!(saved.get("openQuestionCount"), Some(&1.0.into()));
                assert_eq!(ivy_sdk::open_task_questions(&full, None).unwrap().len(), 1);
                assert!(saved.required("claim").unwrap().is_null());
                assert_eq!(
                    hash_json(saved.required("primaryResourceRef").unwrap()).unwrap(),
                    hash_json(original.required("primaryResourceRef").unwrap()).unwrap()
                );
                assert!(
                    saved
                        .required("comments")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                if mode == "retry" {
                    assert_eq!(
                        saved.required("runtimeWait").unwrap().get("reason"),
                        Some(&"host".into())
                    );
                } else {
                    assert_eq!(
                        saved.get("workflowState"),
                        Some(&Json::from(if mode == "handoff" {
                            "review"
                        } else {
                            "todo"
                        }))
                    );
                }
                let deliveries = hive
                    .state
                    .lock()
                    .unwrap()
                    .records
                    .values()
                    .filter(|row| {
                        row.required("object").unwrap().get("contractKey")
                            == Some(&Json::from("task-board/delivery"))
                    })
                    .count();
                assert_eq!(deliveries, usize::from(mode == "handoff"));
                println!(
                    "native completion: {length}/{mode}; bounded warm read with unrelated bodies unavailable, firstLoss={lost_first}, reopen/exact replay, all original bodies/receipt operations and open ordinary question retained"
                );
            }
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn completion_projection_keeps_latest_run_handoff_and_never_infers_absence_from_missing_history()
 {
    for length in [65, 300, 4096] {
        let hive = Arc::new(Hive::new(&completion_fixture(length, "handoff")));
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        let original = hive.state.lock().unwrap().records["task"]
            .required("content")
            .unwrap()
            .required("value")
            .unwrap()
            .clone();
        let full = hydrated(hive.clone(), &original, &"task".into()).await;
        let selected = journal
            .read_completion_comments(&original, &"task".into(), &"run".into(), &[])
            .await
            .unwrap();
        assert_eq!(
            selected.handoff,
            ivy_task_board::engine_read::conversation::run_handoff(&full, &"run".into())
        );
        assert!(!selected.answered);
        assert_eq!(
            selected.handoff.as_ref().unwrap().get("sequence"),
            Some(&Json::Number((length - 1) as f64))
        );
        let reads = hive.state.lock().unwrap().comment_reads;
        let foreign = journal
            .read_completion_comments(&original, &"task".into(), &"foreign-run".into(), &[])
            .await
            .unwrap();
        assert_eq!(
            foreign.handoff.unwrap().get("sequence"),
            Some(&Json::Number(length as f64))
        );
        assert!(hive.state.lock().unwrap().comment_reads - reads <= 4);
        let mut legacy = full.clone();
        legacy.remove("commentStore");
        assert_eq!(
            journal
                .read_completion_comments(&legacy, &"task".into(), &"run".into(), &[])
                .await
                .unwrap()
                .handoff,
            selected.handoff
        );
        db.execute("DELETE FROM comment_index_roots", []).unwrap();
        let missing = hive
            .state
            .lock()
            .unwrap()
            .records
            .remove(&format!("node{length}"))
            .unwrap();
        assert!(
            journal
                .read_completion_comments(&original, &"task".into(), &"absent-run".into(), &[])
                .await
                .is_err()
        );
        hive.state
            .lock()
            .unwrap()
            .records
            .insert(format!("node{length}"), missing);
        assert!(
            journal
                .read_completion_comments(&original, &"task".into(), &"absent-run".into(), &[])
                .await
                .unwrap()
                .handoff
                .is_none()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn completion_delta_refuses_changed_handoff_body_or_original_receipt_operation() {
    for body_change in [true, false] {
        let hive = Arc::new(Hive::new(&completion_fixture(65, "handoff")));
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        let mut delta = journal
            .read_task_header(&"task".into(), true)
            .await
            .unwrap()
            .value;
        if body_change {
            let mut handoff = journal
                .read_completion_comments(&delta, &"task".into(), &"run".into(), &[])
                .await
                .unwrap()
                .handoff
                .unwrap();
            handoff.insert("body", "A changed historical body".into());
            handoff.insert("delivery", json(r#"{"objectId":"delivery","revision":1}"#));
            delta.insert("comments", Json::Array(vec![handoff]));
        } else {
            let entries = LifecycleJournal {
                journal: &journal,
                pages: &NoPages,
            }
            .comment_entries(&delta, &"task".into(), &["c1".into()])
            .await
            .unwrap();
            let comment = entries[0].required("comment").unwrap().clone();
            let mut receipt = entries[0].required("receipt").unwrap().clone();
            receipt.insert("operationId", "another-operation".into());
            delta.insert("comments", Json::Array(vec![comment]));
            delta.insert("commentDeliveries", Json::Array(vec![receipt]));
        }
        let writes = hive.state.lock().unwrap().writes;
        let error = match journal
            .prepare_task_change(&delta, &destination(1.0), TaskChange::NativeCompletion)
            .await
        {
            Ok(_) => panic!("The completion changed its historical authority"),
            Err(error) => error,
        };
        assert_eq!(error.code, "task_board_comment_mismatch");
        assert_eq!(hive.state.lock().unwrap().writes, writes);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn snapshot_receipts_preserve_history_questions_and_original_replay() {
    for (length, duplicate, selected) in [
        (65, false, true),
        (300, false, true),
        (4096, false, true),
        (300, true, true),
        (4096, false, false),
    ] {
        let hive = Arc::new(Hive::new(&fixture(length, duplicate, selected)));
        let original = hive.state.lock().unwrap().records["task"]
            .required("content")
            .unwrap()
            .required("value")
            .unwrap()
            .clone();
        let full_before = hydrated(hive.clone(), &original, &"task".into()).await;
        let history_reads = hive.state.lock().unwrap().comment_reads;
        let path =
            std::env::temp_dir().join(format!("receipt-test-{}.sqlite", uuid::Uuid::new_v4()));
        let request = request();
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let error = publish(&journal, &request, true).await.unwrap_err();
            assert_eq!(
                error.code, "transport_lost",
                "{length}, duplicate={duplicate}: {error:?}"
            );
        }
        if !selected {
            assert_eq!(
                hive.state.lock().unwrap().comment_reads,
                history_reads,
                "An observation without delivery IDs must not read comment nodes"
            );
        }
        let changed_nodes = hive
            .state
            .lock()
            .unwrap()
            .records
            .iter()
            .filter(|(id, row)| {
                id.starts_with("created")
                    && row.required("object").unwrap().get("contractKey")
                        == Some(&Json::from("task-board/comment-node"))
            })
            .count();
        assert_eq!(
            changed_nodes,
            if selected { 5 } else { 0 },
            "Only the two receipt leaves, their two pages and shared root may change"
        );
        let writes = hive.state.lock().unwrap().writes;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let reads = hive.state.lock().unwrap().comment_reads;
            let out = publish(&journal, &request, false).await.unwrap();
            assert_eq!(publish(&journal, &request, false).await.unwrap(), out);
            assert_eq!(hive.state.lock().unwrap().writes, writes);
            assert_eq!(
                hive.state.lock().unwrap().comment_reads,
                reads,
                "Retained replay must not read history"
            );
            let saved = hive.state.lock().unwrap().records["task"]
                .required("content")
                .unwrap()
                .required("value")
                .unwrap()
                .clone();
            assert_eq!(saved.get("openQuestionCount"), Some(&Json::Number(1.0)));
            assert_eq!(
                hash_json(
                    saved
                        .required("commentStore")
                        .unwrap()
                        .required("summary")
                        .unwrap()
                )
                .unwrap(),
                hash_json(
                    original
                        .required("commentStore")
                        .unwrap()
                        .required("summary")
                        .unwrap()
                )
                .unwrap()
            );
            let full = hydrated(hive.clone(), &saved, &"task".into()).await;
            assert_eq!(
                hash_json(full.required("comments").unwrap()).unwrap(),
                hash_json(full_before.required("comments").unwrap()).unwrap()
            );
            let receipts = full
                .required("commentDeliveries")
                .unwrap()
                .as_array()
                .unwrap();
            assert_eq!(receipts.len(), 2);
            assert!(receipts.iter().all(|r| r.get("state")
                == Some(&Json::from(if selected { "delivered" } else { "queued" }))));
            if !selected {
                assert_eq!(
                    hash_json(saved.required("commentStore").unwrap()).unwrap(),
                    hash_json(original.required("commentStore").unwrap()).unwrap()
                );
            }
            assert_ne!(
                receipts[0].get("operationId"),
                receipts[1].get("operationId")
            );
            let ops = LifecycleJournal {
                journal: &journal,
                pages: &NoPages,
            };
            if !selected {
                // No receipt lookup has populated this root's disposable index.
                ops.comment_entries(&saved, &"task".into(), &["c1".into()])
                    .await
                    .unwrap();
            }
            let reads = hive.state.lock().unwrap().comment_reads;
            let entries = ops
                .comment_entries(&saved, &"task".into(), &["c1".into()])
                .await
                .unwrap();
            if !duplicate {
                assert!(
                    hive.state.lock().unwrap().comment_reads - reads
                        <= if length == 4096 { 4 } else { 3 }
                );
                assert_eq!(entries.len(), 1);
            } else {
                assert_eq!(entries.len(), 2);
            }
            println!(
                "native receipts: {length} comments, repeatedID={duplicate}, selected={selected}; original bodies/questions/operations preserved; lost reply/reopen/replay without history reads"
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn receipt_delta_refuses_changed_comment_sequence_operation_or_summary() {
    for change in ["body", "sequence", "operation", "root"] {
        let hive = Arc::new(Hive::new(&fixture(300, false, true)));
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        let store = LifecycleJournal {
            journal: &journal,
            pages: &NoPages,
        };
        let header = store
            .task_header(&"task".into(), true, false)
            .await
            .unwrap()
            .document
            .value;
        let entries = store
            .comment_entries(&header, &"task".into(), &["c1".into()])
            .await
            .unwrap();
        let mut comment = entries[0].required("comment").unwrap().clone();
        let mut receipt = entries[0].required("receipt").unwrap().clone();
        receipt.insert("state", "delivered".into());
        match change {
            "body" => {
                comment.insert("body", "changed".into());
            }
            "sequence" => {
                comment.insert("sequence", Json::Number(2.0));
            }
            "operation" => {
                receipt.insert("operationId", "replacement".into());
            }
            _ => {}
        }
        let mut value = header;
        value.insert("comments", Json::Array(vec![comment]));
        value.insert("commentDeliveries", Json::Array(vec![receipt]));
        if change == "root" {
            value
                .get_mut("commentStore")
                .unwrap()
                .insert("summary", json(r#"{"directCount":0,"questions":[]}"#));
        }
        assert_eq!(
            store
                .prepare_task_change(&value, &destination(1.0), TaskChange::Receipts)
                .await
                .err()
                .unwrap()
                .code,
            "task_board_comment_mismatch"
        );
        assert_eq!(hive.state.lock().unwrap().task_writes, 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn queued_receipts_follow_exact_roots_changes_reopen_and_partial_reads() {
    for (length, duplicate) in [(65, false), (300, true), (4096, false)] {
        let hive = Arc::new(Hive::new(&fixture(length, duplicate, true)));
        let original = hive.state.lock().unwrap().records["task"]
            .required("content")
            .unwrap()
            .required("value")
            .unwrap()
            .clone();
        let expected = vec![
            Json::from("c1"),
            if duplicate {
                "c1".into()
            } else {
                format!("c{length}").into()
            },
        ];
        let path =
            std::env::temp_dir().join(format!("queued-receipts-{}.sqlite", uuid::Uuid::new_v4()));
        let saved;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let store = LifecycleJournal {
                journal: &journal,
                pages: &NoPages,
            };
            assert_eq!(
                store
                    .queued_comment_ids(&original, &"task".into())
                    .await
                    .unwrap(),
                expected
            );
            let reads = hive.state.lock().unwrap().comment_reads;
            assert_eq!(
                store
                    .queued_comment_ids(&original, &"task".into())
                    .await
                    .unwrap(),
                expected
            );
            assert_eq!(
                hive.state.lock().unwrap().comment_reads,
                reads,
                "A complete queued projection reads no comment bodies"
            );
            publish(&journal, &request(), false).await.unwrap();
            saved = hive.state.lock().unwrap().records["task"]
                .required("content")
                .unwrap()
                .required("value")
                .unwrap()
                .clone();
            let reads = hive.state.lock().unwrap().comment_reads;
            assert!(
                store
                    .queued_comment_ids(&saved, &"task".into())
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                hive.state.lock().unwrap().comment_reads,
                reads,
                "Receipt replacements maintain the derived queue without a history scan"
            );
            let (accounted, actual): (i64,i64) = db.query_row(
                "SELECT bytes,(SELECT COALESCE(SUM(length(CAST(identity AS BLOB))+64),0) FROM comment_index_ids) FROM comment_index_roots", [],
                |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
            assert_eq!(accounted, actual);
        }
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let store = LifecycleJournal {
                journal: &journal,
                pages: &NoPages,
            };
            let reads = hive.state.lock().unwrap().comment_reads;
            assert!(
                store
                    .queued_comment_ids(&saved, &"task".into())
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(hive.state.lock().unwrap().comment_reads, reads);
            assert_eq!(
                store
                    .queued_comment_ids(&original, &"task".into())
                    .await
                    .unwrap(),
                expected,
                "The retained original root has its own actual queued receipts"
            );
            db.execute("DELETE FROM comment_index_roots", []).unwrap();
            let missing = hive
                .state
                .lock()
                .unwrap()
                .records
                .remove(&format!("node{length}"))
                .unwrap();
            assert!(
                store
                    .queued_comment_ids(&original, &"task".into())
                    .await
                    .is_err(),
                "A partial scan cannot publish an empty or incomplete queue"
            );
            assert_eq!(
                db.query_row("SELECT complete FROM comment_index_roots", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            hive.state
                .lock()
                .unwrap()
                .records
                .insert(format!("node{length}"), missing);
            assert_eq!(
                store
                    .queued_comment_ids(&original, &"task".into())
                    .await
                    .unwrap(),
                expected
            );
            println!(
                "queued receipts: {length} comments, repeatedID={duplicate}; cold/warm, receipt replacement, exact historical root, reopen and failed partial read verified"
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}
