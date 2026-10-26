#[path = "reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::{hash::digest, json::canonical, limits::MANAGEMENT_FRAME_BYTES};
use ivy_task_board::{engine_invocations::InvocationRequest, evidence_bytes::ChunkKey};

pub(super) struct Scenario {
    plan: Json,
    resume: bool,
    pub(super) ready: Gate,
    prompts: Cell<u64>,
    replay: RefCell<Option<Json>>,
    response: RefCell<Option<InvocationRequest>>,
}
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn pin(id: &str, revision: f64) -> Json {
    Json::object([
        ("objectId", id.into()),
        ("revision", Json::Number(revision)),
    ])
}
fn prompt() -> JsString {
    JsString("driver prompt".encode_utf16().chain([0xd800]).collect())
}
pub(super) const ATTACHMENT: &str = "initial-attachment";
// An already running turn has its original confirmed thread receipt. Retain it
// through the real parameter/evidence writers so control fixtures exercise
// current attachment rules rather than a history-only, incomplete Run seed.
pub(super) fn seed_attachment(context: &Context<'_>) -> IvyResult<Json> {
    use futures_util::FutureExt;
    use ivy_task_board::{
        native_evidence::NativeEvidence,
        native_intent::{NativeIntent, NativeSlot},
        operations::Store,
    };
    async {
        let journal = context.journal;
        let run = Store::read(journal, "task-board/run", &"run".into(), true).await?;
        let intent = NativeIntent {
            store: journal,
            catalogs: context.catalogs,
        };
        let mut call = intent
            .intended_call(
                &run.value,
                NativeSlot::Thread,
                None,
                &ivy_task_board::prompts::CurrentTaskPrompts,
            )
            .await?;
        let operation_id = JsString::from("initial-attachment-operation");
        let request = intent.retain_native_request(call.required("request")?, &operation_id)?;
        let resolved = intent.resolve_native_request(&request)?;
        assert_eq!(request.get("method"), Some(&"thread/resume".into()));
        assert_eq!(
            resolved.required("params")?.get("threadId"),
            Some(&"thread".into())
        );
        for (key, value) in [
            ("request", request),
            ("origin", Json::object([("kind", "initial".into())])),
            ("operationId", Json::String(operation_id.clone())),
            ("state", "prepared".into()),
            ("preparedAt", clock().into()),
            ("observedAt", Json::Null),
            ("ownerPhase", Json::Null),
            ("epoch", Json::Null),
            ("evidence", Json::Null),
            ("code", Json::Null),
        ] {
            call.insert(key, value);
        }
        let version = journal.contracts.version("task-board/native-call")?;
        journal.contracts.validate_record(
            "task-board/native-call",
            &version.clone().into(),
            &call,
        )?;
        let pin = journal.journal.create(
            "task-board/native-call",
            &ATTACHMENT.into(),
            None,
            &call,
            &clock().into(),
            &ATTACHMENT.into(),
        )?;
        let source = context.catalogs.checked_native_contract(
            run.value
                .required("plan")?
                .required("nativeVersion")?
                .as_js_string()
                .unwrap(),
            Some(&Json::object([(
                "sourceHash",
                run.value
                    .required("plan")?
                    .required("catalogSourceHash")?
                    .clone(),
            )])),
        )?;
        let params = resolved.required("params")?.clone();
        let operation = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("operationId", Json::String(operation_id)),
            (
                "callerPrincipalId",
                context.settings.required("principalId")?.clone(),
            ),
            (
                "serviceNodeId",
                run.value
                    .required("target")?
                    .required("serviceNodeId")?
                    .clone(),
            ),
            (
                "nativeVersion",
                call.required("request")?.required("nativeVersion")?.clone(),
            ),
            (
                "nativeExecutableHash",
                source.catalog().required("nativeExecutableHash")?.clone(),
            ),
            ("method", "thread/resume".into()),
            ("params", params.clone()),
            (
                "requestHash",
                hash_json(&Json::object([
                    ("method", "thread/resume".into()),
                    ("params", params),
                ]))?
                .into(),
            ),
            ("phase", "succeeded".into()),
            ("createdAt", clock().into()),
            ("updatedAt", clock().into()),
            ("epoch", "epoch".into()),
            ("requestId", Json::Number(1.0)),
            (
                "reply",
                Json::object([(
                    "result",
                    Json::object([(
                        "thread",
                        Json::object([("id", "thread".into()), ("ephemeral", false.into())]),
                    )]),
                )]),
            ),
            ("code", Json::Null),
        ]);
        let identity = intent.evidence_identity(
            &run.value,
            &call,
            context
                .settings
                .required("principalId")?
                .as_js_string()
                .unwrap(),
        )?;
        let proof = NativeEvidence {
            store: journal,
            catalogs: context.catalogs,
        }
        .save(&pin, &operation, &identity)
        .await?;
        for (key, value) in [
            ("state", "observed".into()),
            ("ownerPhase", "succeeded".into()),
            ("observedAt", clock().into()),
            ("epoch", "epoch".into()),
            ("evidence", proof),
        ] {
            call.insert(key, value);
        }
        journal
            .contracts
            .validate_record("task-board/native-call", &version.into(), &call)?;
        let saved = journal.journal.update(
            "task-board/native-call",
            &ATTACHMENT.into(),
            1.0,
            &call,
            &clock().into(),
        )?;
        assert_eq!(
            saved,
            Json::object([
                ("objectId", ATTACHMENT.into()),
                ("revision", Json::Number(2.0))
            ])
        );
        Ok(operation)
    }
    .now_or_never()
    .expect("Local attachment seeding must not perform remote work")
}
impl Scenario {
    fn records(&self) -> (Json, Json) {
        let (mut task, mut run) = native::baseline();
        if !self.resume {
            task.insert("primaryResourceRef", Json::Null);
        }
        let mut claim = f(&task, "claim").clone();
        claim.insert("phase", "starting".into());
        claim.insert("primaryAtStart", f(&task, "primaryResourceRef").clone());
        task.insert("claim", claim);
        run.insert("plan", self.plan.clone());
        run.insert("phase", "starting".into());
        for key in ["primaryResourceRef", "turnId", "nativeEpoch"] {
            run.insert(key, Json::Null);
        }
        run.insert(
            "externalOutcome",
            json(r#"{"state":"not_started","observedAt":null,"evidence":null,"code":null}"#),
        );
        admission("Task", &task).unwrap();
        admission("Run", &run).unwrap();
        (task, run)
    }
    pub(super) fn bind(&self, context: &Context<'_>) -> IvyResult<()> {
        if context.owner.generation == 3.0 {
            *self.response.borrow_mut() = Some(
                context
                    .calls
                    .native_update(self.replay.borrow().as_ref().unwrap())?,
            );
        }
        if context.owner.generation != 1.0 {
            return Ok(());
        }
        let (task, run) = self.records();
        for (key, value, id) in [
            ("task-board/task", task, "task"),
            ("task-board/run", run, "run"),
        ] {
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
        context.journal.journal.connection.execute("INSERT INTO workspace_reservations VALUES('fixture-native-host','/fixture','task','run')", []).unwrap();
        Ok(())
    }
    pub(super) fn prompt(
        &self,
        task: &Json,
        owner: &JsString,
        coordination: Option<&Json>,
    ) -> IvyResult<JsString> {
        assert_eq!(
            hash_json(task).unwrap(),
            hash_json(&self.records().0).unwrap(),
            "The prompt lost the immutable original Task"
        );
        assert_eq!(owner, &JsString::from("fixture-task-board"));
        assert_eq!(coordination, None);
        self.prompts.set(self.prompts.get() + 1);
        Ok(prompt())
    }
}
pub(super) fn count(fixture: &Fixture, sql: &str) -> i64 {
    fixture.database().query_row(sql, [], |r| r.get(0)).unwrap()
}
pub(super) fn frames(peer: &Peer, kind: &str) -> Vec<Json> {
    peer.frames
        .lock()
        .unwrap()
        .iter()
        .filter(|v| f(v, "kind") == &Json::from(kind))
        .cloned()
        .collect()
}
pub(super) async fn reached(
    fixture: &Fixture,
    peer: &Peer,
    stage: &str,
    mut condition: impl FnMut() -> bool,
) {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "Driver stage {stage}; held projection {}; pages {}; {}; {}",
        fixture.page_waiting.get(),
        canonical(
            &Json::Array(
                fixture
                    .pages
                    .borrow()
                    .iter()
                    .rev()
                    .take(4)
                    .map(|(_, key, options)| Json::object([
                        ("key", key.clone().into()),
                        ("options", options.clone())
                    ]))
                    .collect()
            ),
            MANAGEMENT_FRAME_BYTES
        )
        .unwrap(),
        canonical(
            &Json::Array(
                fixture
                    .rows("task-board/operation")
                    .into_iter()
                    .map(|(id, revision, v)| Json::object([
                        ("object", pin(&id, revision as f64)),
                        ("phase", f(&v, "phase").clone()),
                        ("error", f(&v, "error").clone())
                    ]))
                    .collect()
            ),
            MANAGEMENT_FRAME_BYTES
        )
        .unwrap(),
        canonical(
            &Json::Array(
                peer.frames
                    .lock()
                    .unwrap()
                    .iter()
                    .rev()
                    .take(12)
                    .cloned()
                    .collect()
            ),
            MANAGEMENT_FRAME_BYTES
        )
        .unwrap()
    );
}
pub(super) fn evidence(fixture: &Fixture) -> Vec<(String, String, Json)> {
    let db = fixture.database();
    let mut stmt=db.prepare("SELECT d.id,d.parent_id,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='task-board/native-evidence' ORDER BY d.id").unwrap();
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, json(&r.get::<_, String>(2)?)))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}
pub(super) fn exact_evidence(fixture: &Fixture, peer: &Peer, expected: usize, phase: &str) {
    let manifests = evidence(fixture);
    let attachment = fixture.attachment.borrow().clone();
    let expected = expected + usize::from(attachment.is_some());
    assert_eq!(manifests.len(), expected);
    let calls = fixture.rows("task-board/native-call");
    let mut originals = frames(peer, "committed-operation");
    if let Some(operation) = attachment {
        originals.push(Json::object([("operation", operation)]));
    }
    assert_eq!(originals.len(), expected);
    for (id, parent, manifest) in manifests {
        let call = &calls.iter().find(|r| r.0 == parent).unwrap().2;
        assert_eq!(
            f(f(f(call, "evidence"), "object"), "objectId"),
            &Json::from(id)
        );
        assert_eq!(
            f(&manifest, "format"),
            &"agent-operation/canonical-json".into()
        );
        let mut bytes = Vec::new();
        for part in f(&manifest, "chunks").as_array().unwrap() {
            let (key, owner, chunk): (String, String, Vec<u8>) = fixture
                .database()
                .query_row(
                    "SELECT key,parent_id,bytes FROM blobs WHERE id=?",
                    [f(f(part, "object"), "objectId").as_str().unwrap()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(key, ChunkKey::NativeEvidence.as_str());
            assert_eq!(owner, parent);
            assert_eq!(f(part, "byteLength"), &Json::Number(chunk.len() as f64));
            assert_eq!(f(part, "contentHash"), &Json::from(digest(&chunk)));
            bytes.extend(chunk);
        }
        assert_eq!(
            f(&manifest, "byteLength"),
            &Json::Number(bytes.len() as f64)
        );
        assert_eq!(f(&manifest, "contentHash"), &Json::from(digest(&bytes)));
        let observed = json(std::str::from_utf8(&bytes).unwrap());
        let original = originals
            .iter()
            .map(|v| f(v, "operation"))
            .find(|v| f(v, "operationId") == f(call, "operationId"))
            .unwrap();
        assert_eq!(
            canonical(original, MANAGEMENT_FRAME_BYTES)
                .unwrap()
                .as_bytes(),
            bytes
        );
        for key in [
            "operationId",
            "callerPrincipalId",
            "serviceNodeId",
            "nativeVersion",
            "nativeExecutableHash",
            "method",
            "requestHash",
            "phase",
            "epoch",
        ] {
            assert_eq!(f(&manifest, key), f(&observed, key));
        }
        assert_eq!(
            hash_json(f(&observed, "params")).unwrap(),
            hash_json(f(f(f(call, "request"), "params"), "params")).unwrap()
        );
        assert_eq!(
            f(&observed, "phase"),
            &Json::from(if parent == ATTACHMENT {
                "succeeded"
            } else {
                phase
            })
        );
        assert_eq!(
            f(&observed, "requestHash"),
            &Json::from(
                hash_json(&Json::object([
                    ("method", f(&observed, "method").clone()),
                    ("params", f(&observed, "params").clone())
                ]))
                .unwrap()
            )
        );
    }
}
fn data() -> Json {
    let output = reference_data::read(include_bytes!("task-board-native-driver-cases.json.gz"));
    json(std::str::from_utf8(&output).unwrap())
}
#[tokio::test(flavor = "current_thread")]
async fn production_factory_recovers_committed_native_calls_without_redispatch_and_starts_the_exact_turn()
 {
    let data = data();
    let base = data
        .as_array()
        .unwrap()
        .iter()
        .find(|g| f(f(f(g, "base"), "run"), "plan").get("nativeVersion") == Some(&"0.159.2".into()))
        .unwrap();
    let plan = f(f(f(base, "base"), "run"), "plan").clone();
    for resume in [false, true] {
        let peer = Peer::start_mode(Some("--factory-driver")).await;
        let mut fixture = Fixture::new(false);
        fixture.driver = Some(Scenario {
            plan: plan.clone(),
            resume,
            ready: Gate::default(),
            prompts: Cell::new(0),
            replay: RefCell::default(),
            response: RefCell::default(),
        });
        let scenario = fixture.driver.as_ref().unwrap();
        let original = scenario.records().0;
        let contracts = ContractsBinding;
        let catalogs =
            CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        let factory = make_factory(&fixture, &contracts, &catalogs);
        let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
        let script = async {
            service.start();
            let first = bounded(service.client.wait_ready(None)).await.unwrap();
            rpc(
                &first,
                "test.driver",
                Json::object([("plan", plan.clone()), ("resume", resume.into())]),
            )
            .await
            .unwrap();
            scenario.ready.release();
            reached(&fixture, &peer, "lost committed reply", || {
                !frames(&peer, "receipt-unavailable").is_empty()
            })
            .await;
            let first_calls = fixture.rows("task-board/native-call");
            assert_eq!(first_calls.len(), 1);
            assert_eq!(first_calls[0].1, 1);
            assert_eq!(f(&first_calls[0].2, "state"), &"prepared".into());
            assert_eq!(f(&first_calls[0].2, "evidence"), &Json::Null);
            assert_eq!(
                f(f(&first_calls[0].2, "request"), "method"),
                &Json::from(if resume {
                    "thread/resume"
                } else {
                    "thread/start"
                })
            );
            assert_eq!(fixture.rows("task-board/task")[0].1, 3);
            assert_eq!(fixture.rows("task-board/run")[0].1, 2);
            assert_eq!(fixture.rows("task-board/operation").len(), 1);
            assert!(evidence(&fixture).is_empty());
            assert_eq!(frames(&peer, "committed-operation").len(), 1);
            let control = service.control.clone();
            std::thread::spawn(move || {
                drop(control.handler("categories")(
                    Json::Null,
                    c(1, "driver-drain"),
                ))
            })
            .join()
            .unwrap();
            reached(&fixture, &peer, "held projection", || {
                fixture.page_waiting.get()
            })
            .await;
            rpc(
                &first,
                "test.close",
                Json::object(Vec::<(&str, Json)>::new()),
            )
            .await
            .unwrap();
            until(|| peer.methods(2).contains(&"registry.sync".into())).await;
            assert_eq!(fixture.opens.get(), 1);
            assert!(fixture.drops.borrow().is_empty());
            fixture.page_held.release();
            let second = bounded(service.client.wait_ready(None)).await.unwrap();
            assert_eq!(second.generation(), 2);
            reached(&fixture, &peer, "original receipt and exact turn", || {
                fixture
                    .rows("task-board/run")
                    .first()
                    .is_some_and(|r| r.1 == 5 && f(&r.2, "phase") == &"running".into())
                    && fixture.rows("task-board/task")[0].1 == 9
                    && fixture
                        .rows("task-board/operation")
                        .iter()
                        .all(|r| f(&r.2, "phase") == &"succeeded".into())
            })
            .await;
            let run = fixture.rows("task-board/run")[0].clone();
            assert_eq!(f(&run.2, "turnId"), &"turn".into());
            assert_eq!(f(&run.2, "nativeEpoch"), &"epoch".into());
            assert_eq!(
                f(f(&run.2, "primaryResourceRef"), "nativeId"),
                &"thread".into()
            );
            let calls = fixture.rows("task-board/native-call");
            assert_eq!(calls.len(), 2);
            for (id, revision, call) in &calls {
                assert_eq!(*revision, 2);
                assert_eq!(f(call, "state"), &"observed".into());
                assert_eq!(f(call, "ownerPhase"), &"succeeded".into());
                let slot = if f(f(call, "request"), "method") == &"turn/start".into() {
                    "turn"
                } else {
                    "thread"
                };
                assert_eq!(f(f(&run.2, "calls"), slot), &pin(id, 2.0));
            }
            let retained = calls.iter().find(|r| r.0 == first_calls[0].0).unwrap();
            for key in [
                "request",
                "expectedDefinitionHash",
                "operationId",
                "origin",
                "preparedAt",
            ] {
                assert_eq!(
                    hash_json(f(&retained.2, key)).unwrap(),
                    hash_json(f(&first_calls[0].2, key)).unwrap()
                );
            }
            exact_evidence(&fixture, &peer, 2, "succeeded");
            let task = fixture.rows("task-board/task")[0].clone();
            assert_eq!(task.1, 9);
            assert_eq!(f(f(&task.2, "claim"), "run"), &pin("run", 5.0));
            assert_eq!(f(f(&task.2, "claim"), "phase"), &"running".into());
            assert_eq!(f(&task.2, "workflowState"), &"in_progress".into());
            assert_eq!(f(&task.2, "waiting"), &Json::Null);
            for key in [
                "fields",
                "comments",
                "commentDeliveries",
                "agentCommentCount",
                "workRevision",
                "attemptCount",
                "latestResult",
                "acceptedReview",
            ] {
                assert_eq!(
                    hash_json(f(&task.2, key)).unwrap(),
                    hash_json(f(&original, key)).unwrap()
                );
            }
            assert_eq!(
                scenario.prompts.get(),
                1,
                "Turn preparation renders the immutable original Task once"
            );
            assert!(fixture.rows("task-board/result").is_empty());
            assert_eq!(
                count(
                    &fixture,
                    "SELECT count(*) FROM cleanup_roots WHERE root_id='run'"
                ),
                0
            );
            assert_eq!(
                count(
                    &fixture,
                    "SELECT count(*) FROM workspace_reservations WHERE task_id='task' AND operation_id='run'"
                ),
                1
            );
            assert_eq!(frames(&peer, "absence").len(), 2);
            assert_eq!(frames(&peer, "fresh-primary").len(), 1);
            let committed = frames(&peer, "committed-operation");
            assert_eq!(committed.len(), 2);
            assert_eq!(f(&committed[0], "generation"), &Json::Number(1.0));
            assert_eq!(f(&committed[1], "generation"), &Json::Number(2.0));
            let recovered = frames(&peer, "original-operation");
            assert_eq!(recovered.len(), 1);
            assert_eq!(
                hash_json(f(&recovered[0], "operation")).unwrap(),
                hash_json(f(&committed[0], "operation")).unwrap()
            );
            let operations = fixture.rows("task-board/operation");
            assert_eq!(operations.len(), 4);
            assert!(
                operations
                    .iter()
                    .all(|r| f(&r.2, "phase") == &"succeeded".into()
                        && f(f(&r.2, "actor"), "source") == &"native".into())
            );
            let completed = operations
                .iter()
                .find(|r| {
                    f(f(f(&r.2, "request"), "change"), "kind") == &"observeCall".into()
                        && f(f(f(&r.2, "request"), "change"), "slot") == &"turn".into()
                })
                .unwrap();
            let outcome = f(&completed.2, "outcome").clone();
            *scenario.replay.borrow_mut() = Some(f(&completed.2, "request").clone());
            let revisions = count(&fixture, "SELECT count(*) FROM revisions");
            let blobs = count(&fixture, "SELECT count(*) FROM blobs");
            let notices = fixture.notices.get();
            rpc(
                &second,
                "test.close",
                Json::object(Vec::<(&str, Json)>::new()),
            )
            .await
            .unwrap();
            until(|| scenario.response.borrow().is_some()).await;
            let third = bounded(service.client.wait_ready(None)).await.unwrap();
            assert_eq!(third.generation(), 3);
            let response = scenario.response.borrow_mut().take().unwrap();
            assert_eq!(
                hash_json(&bounded(response).await.unwrap()).unwrap(),
                hash_json(&outcome).unwrap()
            );
            assert_eq!(count(&fixture, "SELECT count(*) FROM revisions"), revisions);
            assert_eq!(count(&fixture, "SELECT count(*) FROM blobs"), blobs);
            assert_eq!(fixture.notices.get(), notices);
            assert_eq!(frames(&peer, "committed-operation").len(), 2);
            assert_eq!(frames(&peer, "original-operation").len(), 1);
            assert_eq!(scenario.prompts.get(), 1);
            exact_evidence(&fixture, &peer, 2, "succeeded");
            bounded(service.close()).await.unwrap();
        };
        let (result, ()) = tokio::join!(owner.run(&factory), script);
        result.unwrap();
        assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
        assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
        peer.finish().await;
    }
}
