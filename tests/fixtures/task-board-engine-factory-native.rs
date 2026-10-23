use super::*;
use ivy_contracts::{json::canonical, limits::MANAGEMENT_FRAME_BYTES};

pub(super) struct Scenario {
    row: Json,
    pub(super) inputs_ready: Gate,
}
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn pin(id: &str) -> Json {
    Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))])
}
pub(super) fn baseline() -> (Json, Json) {
    let owner = json(
        r#"{"principalId":"fixture-principal","serviceNodeId":"fixture-task-board","generation":7,"source":"user"}"#,
    );
    let target = json(r#"{"hostId":"fixture-native-host","serviceNodeId":"native-agent"}"#);
    let workspace = json(
        r#"{"hostId":"fixture-native-host","serviceNodeId":"native-agent","canonicalCwd":"/fixture","taskRoot":"/fixture","bootstrapPath":"/fixture","intendedPath":"/fixture","sourcePath":null,"project":null,"useWorktree":false,"repository":null}"#,
    );
    let request = Json::object([
        ("action", "start".into()),
        ("operationId", "fixture-start".into()),
        ("taskId", "task".into()),
        ("expectedRevision", Json::Number(1.0)),
        ("target", target.clone()),
        ("intent", pin("plan")),
        ("workspace", workspace.clone()),
        ("commentIds", Json::Array(vec![])),
    ]);
    let hash = hash_json(&request).unwrap();
    let primary = json(
        r#"{"serviceNodeId":"native-agent","namespace":"codex","kind":"thread","nativeId":"thread"}"#,
    );
    // This exact finite plan satisfies the frozen record shape. Native status
    // refuses before plan/catalog/evidence interpretation in this fixture.
    let h = Json::from(format!("sha256:{}", "a".repeat(64)));
    let plan = Json::object([
        ("nativeVersion", "0.159.2".into()),
        ("catalogSourceHash", h.clone()),
        (
            "definitions",
            Json::object([
                ("threadStart", h.clone()),
                ("threadResume", h.clone()),
                ("turnStart", h.clone()),
                ("turnInterrupt", h),
            ]),
        ),
        ("threadStart", Json::object(Vec::<(&str, Json)>::new())),
        ("threadResume", Json::object(Vec::<(&str, Json)>::new())),
        ("turnStart", Json::object(Vec::<(&str, Json)>::new())),
    ]);
    let run = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskId", "task".into()),
        ("attempt", Json::Number(1.0)),
        ("operationId", "fixture-start".into()),
        ("callerPrincipalId", "fixture-principal".into()),
        ("originalRequest", request.clone()),
        ("requestHash", hash.clone().into()),
        ("target", target.clone()),
        ("workspace", workspace.clone()),
        ("plan", plan),
        ("owner", owner.clone()),
        ("phase", "waiting_input".into()),
        ("primaryResourceRef", primary.clone()),
        ("turnId", "turn".into()),
        ("nativeEpoch", "epoch".into()),
        (
            "calls",
            Json::object([
                ("thread", Json::Null),
                ("turn", Json::Null),
                ("interrupt", Json::Null),
            ]),
        ),
        ("notificationCursor", Json::Number(0.0)),
        ("result", Json::Null),
        ("cancellation", Json::Null),
        (
            "externalOutcome",
            json(r#"{"state":"active","observedAt":null,"evidence":null,"code":null}"#),
        ),
        ("createdAt", clock().into()),
        ("updatedAt", clock().into()),
        ("finishedAt", Json::Null),
    ]);
    let mut fields = create().required("fields").unwrap().clone();
    fields.insert("control", "agent".into());
    fields.insert("userContact", "ticket".into());
    fields.insert("allowParallel", false.into());
    let task = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskKey", "TASK-0001".into()),
        ("fields", fields),
        ("workflowState", "in_progress".into()),
        ("wakeAt", Json::Null),
        ("dependencies", Json::Array(vec![])),
        ("runtimeWait", Json::Null),
        ("publication", Json::Null),
        ("primaryResourceRef", primary),
        (
            "claim",
            Json::object([
                ("operationId", "fixture-start".into()),
                ("callerPrincipalId", "fixture-principal".into()),
                ("attempt", Json::Number(1.0)),
                ("owner", owner),
                ("request", request),
                ("requestHash", hash.into()),
                ("target", target),
                ("workspace", workspace),
                ("primaryAtStart", Json::Null),
                ("claimedAt", clock().into()),
                ("phase", "waiting_input".into()),
                ("run", pin("run")),
            ]),
        ),
        ("attemptCount", Json::Number(1.0)),
        ("lastRun", pin("run")),
        ("lastHistory", Json::Null),
        ("latestResult", Json::Null),
        ("acceptedReview", Json::Null),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        ("agentCommentCount", Json::Number(0.0)),
        ("workRevision", Json::Number(0.0)),
        ("attachments", Json::Array(vec![])),
        ("createdAt", clock().into()),
        ("updatedAt", clock().into()),
    ]);
    admission("Task", &task).unwrap();
    admission("Run", &run).unwrap();
    (task, run)
}
impl Scenario {
    pub(super) fn seed(&self, context: &Context<'_>) -> IvyResult<()> {
        if context.owner.generation != 1.0 {
            return Ok(());
        }
        let (task, run) = baseline();
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
        Ok(())
    }
    pub(super) fn notice(
        &self,
        generation: f64,
        task: &Json,
        operation: &JsString,
    ) -> IvyResult<Option<Json>> {
        if generation == 1.0
            && f(task, "comments")
                .as_array()
                .unwrap()
                .iter()
                .any(|c| f(c, "operationId").as_js_string() == Some(operation))
        {
            return Err(
                IvyError::new("fixture_notice", "Uncertain native question observation.")
                    .with_outcome(Outcome::Unknown),
            );
        }
        TaskCommentNotices.comment(task, operation)
    }
}
fn comments(fixture: &Fixture) -> Vec<Json> {
    fixture.rows("task-board/task")[0]
        .2
        .required("comments")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec()
}
fn operations(fixture: &Fixture, purpose: &str) -> Vec<(String, u64, Json)> {
    fixture
        .rows("task-board/operation")
        .into_iter()
        .filter(|(_, _, o)| f(f(o, "request"), "purpose") == &Json::from(purpose))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn native_mirroring_uses_factory_recovery_original_identity_and_current_confidential_schema_gate()
 {
    for name in ["answered choices", "secret answer"] {
        // Reuse unmodified original input rows, including their exact identity,
        // thread/turn/epoch, state, answer and secret marker.
        let row = crate::native_comments::data()
            .required("rows")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r.get("name") == Some(&Json::from(name)))
            .unwrap()
            .clone();
        let secret = name == "secret answer";
        let peer = Peer::start_mode(Some("--factory")).await;
        let mut fixture = Fixture::new(false);
        fixture.native = Some(Scenario {
            row,
            inputs_ready: Gate::default(),
        });
        let scenario = fixture.native.as_ref().unwrap();
        let input = scenario.row.required("input").unwrap();
        let inputs = input.required("items").unwrap().clone();
        let identity = inputs.as_array().unwrap()[0]
            .required("identity")
            .unwrap()
            .clone();
        let question = format!("native-question-{}", &hash_json(&identity).unwrap()[7..]);
        let response = format!("native-response-{}", &hash_json(&identity).unwrap()[7..]);
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
                "test.inputs",
                Json::object([("items", inputs.clone())]),
            )
            .await
            .unwrap();
            scenario.inputs_ready.release();
            until(|| {
                operations(&fixture, "question")
                    .first()
                    .is_some_and(|(_, _, o)| f(o, "phase") == &"needs_attention".into())
            })
            .await;
            let original = operations(&fixture, "question")[0].clone();
            let current = comments(&fixture);
            assert_eq!(current.len(), 1);
            assert_eq!(f(&current[0], "commentId"), &Json::from(question.clone()));
            assert_eq!(
                hash_json(f(f(&current[0], "nativeInput"), "identity")).unwrap(),
                hash_json(&identity).unwrap()
            );
            assert_eq!(f(f(&current[0], "nativeInput"), "state"), &"pending".into());
            assert_eq!(f(f(&current[0], "nativeInput"), "reply"), &Json::Null);
            let first_task = fixture.rows("task-board/task")[0].clone();
            assert_eq!(first_task.1, 3);
            let control = service.control.clone();
            std::thread::spawn(move || {
                drop(control.handler("categories")(
                    Json::Null,
                    c(1, "native-drain"),
                ))
            })
            .join()
            .unwrap();
            until(|| fixture.page_waiting.get()).await;
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
            until(|| {
                operations(&fixture, "question")[0].2.get("phase") == Some(&"succeeded".into())
            })
            .await;
            if secret {
                // The current PendingInput schema requires a reply for answered
                // inputs; source mirroring deliberately redacts a secret reply.
                // The current contract must still refuse that invalid record.
                until(|| {
                    peer.frames
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|v| {
                            v.get("generation") == Some(&Json::Number(2.0))
                                && v.get("method") == Some(&"tools.call".into())
                                && f(f(v, "params"), "qualifiedName") == &"agent.status".into()
                        })
                        .count()
                        > 0
                })
                .await;
                let diagnostics = service.control.readiness(&second).await.unwrap();
                let ds = f(&diagnostics, "diagnostics").as_array().unwrap();
                assert!(
                    ds.iter()
                        .any(|d| f(d, "code") == &"invalid_arguments".into()),
                    "Secret response must retain its exact current-schema refusal: {diagnostics:?}"
                );
                assert!(operations(&fixture, "update").is_empty());
                assert!(
                    ds.iter()
                        .any(|d| f(d, "code") == &"invalid_arguments".into()
                            && f(d, "message")
                                == &"Value does not match its TaskBoard contract.".into())
                );
                assert_eq!(comments(&fixture).len(), 1);
                assert_eq!(fixture.rows("task-board/task")[0].1, 3);
            } else {
                until(|| comments(&fixture).len() == 2).await;
                until(|| {
                    operations(&fixture, "update")
                        .first()
                        .is_some_and(|(_, _, o)| f(o, "phase") == &"succeeded".into())
                })
                .await;
                let current = comments(&fixture);
                assert_eq!(f(&current[1], "commentId"), &Json::from(response.clone()));
                assert_eq!(f(&current[1], "replyTo"), &Json::from(question.clone()));
                assert_eq!(f(&current[1], "body"), &"First\n\nSecond".into());
                assert_eq!(f(&current[1], "authorKind"), &"system".into());
                assert_eq!(
                    hash_json(f(f(&current[1], "nativeInput"), "identity")).unwrap(),
                    hash_json(&identity).unwrap()
                );
                assert_eq!(
                    hash_json(f(f(&current[1], "nativeInput"), "reply")).unwrap(),
                    hash_json(f(&inputs.as_array().unwrap()[0], "reply")).unwrap()
                );
                assert_eq!(
                    f(f(&current[1], "author"), "generation"),
                    &Json::Number(2.0)
                );
                assert_eq!(f(f(&current[1], "author"), "source"), &"native".into());
                assert_eq!(fixture.rows("task-board/task")[0].1, 5);
            }
            let recovered = operations(&fixture, "question")[0].clone();
            assert_eq!(recovered.0, original.0);
            for key in ["actor", "request", "requestHash", "writes"] {
                assert_eq!(
                    hash_json(f(&recovered.2, key)).unwrap(),
                    hash_json(f(&original.2, key)).unwrap()
                );
            }
            let current = comments(&fixture);
            assert_eq!(f(&current[0], "authorKind"), &"agent".into());
            assert_eq!(f(f(&current[0], "author"), "source"), &"native".into());
            assert_eq!(
                f(f(&current[0], "author"), "generation"),
                &Json::Number(1.0)
            );
            assert_eq!(
                f(f(&current[0], "author"), "principalId"),
                &"fixture-principal".into()
            );
            assert_eq!(f(&current[0], "run"), &pin("run"));
            let task = fixture.rows("task-board/task")[0].2.clone();
            assert_eq!(f(&task, "workRevision"), &Json::Number(0.0));
            assert_eq!(f(&task, "agentCommentCount"), &Json::Number(1.0));
            assert_eq!(f(&task, "commentDeliveries"), &Json::Array(vec![]));
            assert_eq!(f(&task, "publication"), &Json::Null);
            assert_eq!(
                fixture.rows("task-board/history").len(),
                if secret { 1 } else { 2 }
            );
            assert_eq!(fixture.rows("task-board/run").len(), 1);
            assert_eq!(fixture.rows("task-board/run")[0].1, 1);
            assert!(fixture.rows("task-board/delivery").is_empty());
            let count: i64 = fixture
                .database()
                .query_row("SELECT count(*) FROM revisions", [], |r| r.get(0))
                .unwrap();
            let callbacks = fixture.notices.get();
            // Retained internal operation lookup uses the public
            // owner with the original native caller, rather than a new action.
            let mut caller = c(2, "native-replay");
            caller.caller_principal_id = "fixture-principal".into();
            let saved = service.control.handler("operation")(
                Json::object([("operationId", f(&recovered.2, "operationId").clone())]),
                caller,
            )
            .await
            .unwrap();
            assert_eq!(hash_json(&saved).unwrap(), hash_json(&recovered.2).unwrap());
            assert_eq!(
                fixture
                    .database()
                    .query_row("SELECT count(*) FROM revisions", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                count
            );
            assert_eq!(fixture.notices.get(), callbacks);
            // A third real graph re-reads the same native input page. Identity
            // deduplication must survive a fresh cache, owner and tool binding.
            rpc(
                &second,
                "test.close",
                Json::object(Vec::<(&str, Json)>::new()),
            )
            .await
            .unwrap();
            until(|| fixture.opens.get() == 3).await;
            let third = bounded(service.client.wait_ready(None)).await.unwrap();
            assert_eq!(third.generation(), 3);
            until(|| {
                peer.frames.lock().unwrap().iter().any(|v| {
                    v.get("generation") == Some(&Json::Number(3.0))
                        && v.get("method") == Some(&"tools.call".into())
                        && f(f(v, "params"), "qualifiedName") == &"agent.status".into()
                })
            })
            .await;
            assert_eq!(comments(&fixture).len(), if secret { 1 } else { 2 });
            assert_eq!(
                fixture
                    .database()
                    .query_row("SELECT count(*) FROM revisions", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                count
            );
            assert_eq!(fixture.notices.get(), callbacks);
            let text = canonical(
                &Json::Array(
                    fixture
                        .rows("task-board/operation")
                        .into_iter()
                        .map(|(_, _, o)| o)
                        .collect(),
                ),
                MANAGEMENT_FRAME_BYTES,
            )
            .unwrap();
            if secret {
                assert!(!text.contains("First"));
                assert!(!text.contains("Second"));
            }
            bounded(service.close()).await.unwrap();
        };
        let (result, ()) = tokio::join!(owner.run(&factory), script);
        result.unwrap();
        assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
        assert_eq!(
            fixture
                .database()
                .query_row("SELECT count(*) FROM active_gates", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        peer.finish().await;
    }
}
