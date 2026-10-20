use super::*;
use driver::{count, frames, reached};

pub(super) struct Scenario {
    row: Json,
    phone: bool,
    fallback: bool,
    forwarded: bool,
    pub(super) ready: Gate,
}
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn pin(id: &str, revision: u64) -> Json {
    Json::object([
        ("objectId", id.into()),
        ("revision", Json::Number(revision as f64)),
    ])
}
fn current(fixture: &Fixture, key: &str, id: &str) -> (String, u64, Json) {
    fixture
        .rows(key)
        .into_iter()
        .find(|row| row.0 == id)
        .unwrap()
}
fn delivery(fixture: &Fixture) -> Option<(String, u64, Json)> {
    let rows = fixture.rows("task-board/delivery");
    assert!(rows.len() <= 1);
    rows.into_iter().next()
}
fn publication(fixture: &Fixture) -> (String, u64, Json) {
    let rows = fixture.rows("task-board/operation");
    assert_eq!(rows.len(), 1);
    rows.into_iter().next().unwrap()
}
impl Scenario {
    fn new(name: &str) -> Self {
        let output = std::process::Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-deliveries-oracle.mjs",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows = json(std::str::from_utf8(&output.stdout).unwrap());
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| f(row, "name") == &Json::from(name))
            .unwrap()
            .clone();
        let input = f(&row, "input");
        let phone = f(input, "mode") == &"phonePrepared".into();
        let fallback = phone && f(f(input, "fault"), "outcome") == &"not_executed".into();
        let forwarded = phone && f(input, "differentAdmission") == &true.into();
        assert!(!fallback || !forwarded);
        Self {
            row,
            phone,
            fallback,
            forwarded,
            ready: Gate::default(),
        }
    }
    fn source_task(&self) -> &Json {
        f(f(f(&self.row, "initial"), "task"), "value")
    }
    fn source_comment(&self) -> &Json {
        &f(self.source_task(), "comments").as_array().unwrap()[0]
    }
    fn source_request(&self) -> &Json {
        f(
            f(f(f(&self.row, "initial"), "current"), "value"),
            self.request_key(),
        )
    }
    fn chat_request(&self) -> &Json {
        if !self.fallback {
            return self.source_request();
        }
        f(&self.row, "boundaries")
            .as_array()
            .unwrap()
            .iter()
            .find_map(|b| {
                (f(b, "kind") == &"write".into())
                    .then(|| f(&f(b, "args").as_array().unwrap()[1], "chatRequest"))
                    .filter(|request| !request.is_null())
            })
            .unwrap()
    }
    fn request_key(&self) -> &'static str {
        if self.phone {
            "phoneRequest"
        } else {
            "chatRequest"
        }
    }
    fn phone_target(&self) -> Json {
        if self.phone {
            f(f(f(&self.row, "initial"), "first"), "phoneTarget").clone()
        } else {
            Json::Null
        }
    }
    fn records(&self) -> Vec<(&'static str, Json, &'static str)> {
        let mut records: Vec<_> = retirement::reviewed_records()
            .into_iter()
            .filter(|row| row.2 != "run")
            .collect();
        records[2].1.insert("lastRun", pin("newer-run", 1));
        let mut task = records[2].1.clone();
        task.insert("taskKey", f(self.source_task(), "taskKey").clone());
        task.insert("workflowState", "todo".into());
        task.insert("attemptCount", Json::Number(0.0));
        for key in [
            "latestResult",
            "acceptedReview",
            "lastRun",
            "primaryResourceRef",
            "lastHistory",
        ] {
            task.insert(key, Json::Null);
        }
        let mut fields = f(&task, "fields").clone();
        fields.insert("control", "user".into());
        fields.insert(
            "userContact",
            if self.phone { "phone" } else { "chat" }.into(),
        );
        fields.insert("executionRequirement", Json::Null);
        task.insert("fields", fields);
        records.push(("task-board/task", task, "contact-task"));
        records
    }
    pub(super) fn bind(&self, context: &Context<'_>) -> IvyResult<()> {
        if context.owner.generation != 1.0 {
            return Ok(());
        }
        for (key, value, id) in self.records() {
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
    pub(super) fn check_page(&self, options: &Json) -> IvyResult<()> {
        let expected = json(
            r#"{"where":{"op":"in","field":"data:/state","value":["pending","dispatching","outcome_unknown"]},"limit":50,"orderBy":[{"field":"object.id","direction":"asc"}]}"#,
        );
        assert_eq!(hash_json(options)?, hash_json(&expected)?);
        Ok(())
    }
    pub(super) fn tasks(&self, fixture: &Fixture, options: &Json) -> IvyResult<Json> {
        let expected = json(
            r#"{"where":{"op":"and","args":[{"op":"eq","field":"data:/fields/control","value":"agent"},{"op":"in","field":"data:/workflowState","value":["todo","waiting","review"]}]},"limit":50,"orderBy":[{"field":"data:/fields/priority","direction":"desc"},{"field":"object.id","direction":"asc"}]}"#,
        );
        assert_eq!(hash_json(options)?, hash_json(&expected)?);
        assert_eq!(fixture.rows("task-board/task").len(), 2);
        Ok(Json::object([
            ("items", Json::Array(vec![pin("task", 1)])),
            ("nextCursor", Json::Null),
        ]))
    }
    pub(super) fn chat_readable_version(&self, version: &JsString) -> bool {
        assert!(!self.phone || self.fallback);
        assert_eq!(version, &JsString::from("1.3.0"));
        true
    }
    pub(super) fn handoff(&self, task: &Json, comment: &Json) -> IvyResult<JsString> {
        assert!(!self.phone || self.fallback);
        self.verify_prompt(task, comment);
        Ok(f(self.chat_request(), "text")
            .as_js_string()
            .unwrap()
            .clone())
    }
    pub(super) fn voice_prompt(&self, task: &Json, comment: &Json) -> IvyResult<JsString> {
        assert!(self.phone);
        self.verify_prompt(task, comment);
        Ok(f(self.source_request(), "voicePrompt")
            .as_js_string()
            .unwrap()
            .clone())
    }
    fn verify_prompt(&self, task: &Json, comment: &Json) {
        assert_eq!(f(task, "taskKey"), f(self.source_task(), "taskKey"));
        for key in [
            "commentId",
            "body",
            "authorKind",
            "purpose",
            "requests",
            "nativeInput",
        ] {
            assert_eq!(f(comment, key), f(self.source_comment(), key));
        }
        assert!(!f(task, "lastHistory").is_null());
    }
    fn comment_request(&self) -> Json {
        let request = Json::object([
            ("action", "comment".into()),
            ("operationId", "factory-question".into()),
            ("taskId", "contact-task".into()),
            ("expectedRevision", Json::Number(1.0)),
            ("commentId", f(self.source_comment(), "commentId").clone()),
            ("authorKind", "agent".into()),
            ("body", f(self.source_comment(), "body").clone()),
            ("purpose", "question".into()),
            ("contactUser", true.into()),
            ("agentDelivery", "none".into()),
            ("requests", Json::Array(vec![])),
            ("responses", Json::Array(vec![])),
            ("attachments", Json::Array(vec![])),
            ("replyTo", Json::Null),
        ]);
        admission("ActionRequest", &request).unwrap();
        admission("DurableRequest", &request).unwrap();
        request
    }
    fn preserved(&self, fixture: &Fixture, published_task: &Json) {
        for (key, value, id) in self.records().iter().take(3) {
            let row = current(fixture, key, id);
            assert_eq!(row.1, 1);
            assert_eq!(hash_json(&row.2).unwrap(), hash_json(value).unwrap());
        }
        let task = current(fixture, "task-board/task", "contact-task");
        assert_eq!(task.1, 3);
        assert_eq!(
            hash_json(&task.2).unwrap(),
            hash_json(published_task).unwrap()
        );
        let comments = f(&task.2, "comments").as_array().unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(f(&comments[0], "purpose"), &"question".into());
        assert_eq!(f(&comments[0], "responses"), &Json::Array(vec![]));
        assert_eq!(f(&comments[0], "replyTo"), &Json::Null);
        assert_eq!(fixture.rows("task-board/result").len(), 1);
        assert_eq!(fixture.rows("task-board/run").len(), 1);
        assert_eq!(fixture.rows("task-board/history").len(), 1);
        for key in [
            "task-board/native-plan",
            "task-board/native-call",
            "task-board/native-evidence",
            "task-board/scheduler-attempt",
        ] {
            assert!(fixture.rows(key).is_empty());
        }
        assert_eq!(
            count(
                fixture,
                "SELECT count(*) FROM workspace_reservations WHERE task_id='task' AND operation_id='current-workspace'"
            ),
            1
        );
    }
}
fn verify_route_request(scenario: &Scenario, retained: &Json, history: &Json, phone: bool) {
    let mut expected = if phone {
        scenario.source_request()
    } else {
        scenario.chat_request()
    }
    .clone();
    expected.insert("operationId", f(retained, "operationId").clone());
    if !phone {
        expected.insert("source", history.clone());
        let mut bridge = f(&expected, "expectedBridge").clone();
        bridge.insert("callerPrincipalId", "fixture-principal".into());
        expected.insert("expectedBridge", bridge);
    }
    assert_eq!(hash_json(retained).unwrap(), hash_json(&expected).unwrap());
    let id = f(retained, "operationId").as_str().unwrap();
    if phone {
        assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
        return;
    }
    let parts: Vec<_> = id.split(':').collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], "11111111-1111-4111-8111-111111111111");
    assert!(parts[1].parse::<u64>().is_ok());
    assert!(uuid::Uuid::parse_str(parts[2]).is_ok());
}

async fn run_delivery(name: &str) {
    let peer = Peer::start_mode(Some("--factory-delivery")).await;
    let mut fixture = Fixture::new(false);
    fixture.delivery = Some(Scenario::new(name));
    let scenario = fixture.delivery.as_ref().unwrap();
    let (phone, fallback, forwarded) = (scenario.phone, scenario.fallback, scenario.forwarded);
    let contracts = ContractsBinding;
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let factory = make_factory(&fixture, &contracts, &catalogs);
    let mut setup = configured(&peer);
    setup
        .settings
        .insert("phoneTarget", scenario.phone_target());
    let (service, owner) = make_service(setup, &contracts).unwrap();
    let original = scenario.comment_request();
    let script = async {
        service.start();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        rpc(
            &first,
            "test.deliveryFixture",
            Json::object([("row", scenario.row.clone())]),
        )
        .await
        .unwrap();
        let outcome = bounded(service.control.handler("update")(
            original.clone(),
            c(1, "factory-question"),
        ))
        .await
        .unwrap();
        let published = publication(&fixture);
        assert_eq!(f(&published.2, "phase"), &"succeeded".into());
        assert_eq!(
            hash_json(f(&published.2, "request")).unwrap(),
            hash_json(&original).unwrap()
        );
        assert_eq!(
            f(&published.2, "requestHash"),
            &hash_json(&original).unwrap().into()
        );
        assert_eq!(f(f(&published.2, "actor"), "source"), &"worker".into());
        assert_eq!(
            f(f(&published.2, "actor"), "generation"),
            &Json::Number(1.0)
        );
        assert_eq!(
            f(f(&published.2, "actor"), "principalId"),
            &Json::String(context(1).caller_principal_id)
        );
        assert_eq!(
            hash_json(f(&published.2, "outcome")).unwrap(),
            hash_json(&outcome).unwrap()
        );
        let writes = f(&published.2, "writes").as_array().unwrap();
        assert_eq!(
            writes
                .iter()
                .map(|w| f(w, "contractKey").as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "task-board/task",
                "task-board/delivery",
                "task-board/history",
                "task-board/task"
            ]
        );
        let initial = delivery(&fixture).unwrap();
        assert_eq!(initial.1, 1);
        let mut expected_initial = f(f(&scenario.row, "initial"), "first").clone();
        expected_initial.insert("taskId", "contact-task".into());
        expected_initial.insert("publication", pin(&published.0, 1));
        expected_initial.insert("phoneTarget", scenario.phone_target());
        assert_eq!(
            hash_json(&initial.2).unwrap(),
            hash_json(&expected_initial).unwrap()
        );
        let written_task = current(&fixture, "task-board/task", "contact-task").2;
        let comment = &f(&written_task, "comments").as_array().unwrap()[0];
        assert_eq!(f(comment, "delivery"), &pin(&initial.0, 1));
        assert_eq!(f(&outcome, "task"), &pin("contact-task", 3));
        scenario.preserved(&fixture, &written_task);
        scenario.ready.release();
        reached(
            &fixture,
            &peer,
            "original channel outcome remains unknown",
            || {
                delivery(&fixture)
                    .is_some_and(|row| f(&row.2, "state") == &"outcome_unknown".into())
            },
        )
        .await;
        let uncertain = delivery(&fixture).unwrap();
        assert_eq!(uncertain.0, initial.0);
        assert_eq!(
            uncertain.1,
            if forwarded {
                4
            } else if fallback {
                5
            } else {
                3
            }
        );
        assert_eq!(
            f(&uncertain.2, "code"),
            &Json::from(if forwarded {
                "phone_prompt_outcome_unknown"
            } else if phone && !fallback {
                "service_unavailable"
            } else {
                "outcome_unknown"
            })
        );
        let active_phone = phone && !fallback;
        let request = f(
            &uncertain.2,
            if active_phone {
                "phoneRequest"
            } else {
                "chatRequest"
            },
        )
        .clone();
        verify_route_request(
            scenario,
            &request,
            f(&written_task, "lastHistory"),
            active_phone,
        );
        assert_eq!(f(&uncertain.2, "operationId"), f(&request, "operationId"));
        if active_phone {
            assert_eq!(frames(&peer, "delivery-phone-first-admission").len(), 1);
            assert_eq!(
                f(&uncertain.2, "phoneCallId"),
                &if forwarded { "call".into() } else { Json::Null }
            );
            assert_eq!(f(&uncertain.2, "activeRoute"), &"phone".into());
            assert!(f(&uncertain.2, "chatRequest").is_null());
        } else {
            let notifications = frames(&peer, "delivery-notify");
            assert_eq!(notifications.len(), 1);
            assert_eq!(
                hash_json(f(&notifications[0], "request")).unwrap(),
                hash_json(&request).unwrap()
            );
        }
        if fallback {
            verify_phone_refusal(scenario, &peer, f(&uncertain.2, "phoneRequest"));
            verify_fallback_history(scenario, &fixture, &initial.0, "lost-chat-reply");
        }
        if forwarded {
            verify_forwarded_receipts(scenario, &peer, &request, false);
            verify_forwarded_history(scenario, &fixture, &initial.0, "unknown-forward");
        }
        scenario.preserved(&fixture, &written_task);
        rpc(
            &first,
            "test.close",
            Json::object(Vec::<(&str, Json)>::new()),
        )
        .await
        .unwrap();
        until(|| peer.methods(2).contains(&"registry.sync".into())).await;
        let second = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(second.generation(), 2);
        reached(
            &fixture,
            &peer,
            "original channel receipt confirmation after reconnect",
            || delivery(&fixture).is_some_and(|row| f(&row.2, "state") == &"confirmed".into()),
        )
        .await;
        let confirmed = delivery(&fixture).unwrap();
        assert_eq!(confirmed.0, initial.0);
        assert_eq!(
            confirmed.1,
            if fallback {
                6
            } else if phone {
                5
            } else {
                4
            }
        );
        for key in [
            "schemaVersion",
            "taskId",
            "commentId",
            "publication",
            "requestedRoute",
            "createdAt",
        ] {
            assert_eq!(f(&confirmed.2, key), f(&initial.2, key));
        }
        for key in [
            "operationId",
            "phoneRequest",
            "phoneTarget",
            "chatRequest",
            "chatTarget",
        ] {
            assert_eq!(
                hash_json(f(&confirmed.2, key)).unwrap(),
                hash_json(f(&uncertain.2, key)).unwrap()
            );
        }
        assert_eq!(
            f(&confirmed.2, "deliveryEvidence"),
            f(
                f(f(&scenario.row, "expected"), "final")
                    .required("value")
                    .unwrap(),
                "deliveryEvidence"
            )
        );
        assert_eq!(f(&confirmed.2, "code"), &Json::Null);
        if active_phone {
            if forwarded {
                verify_forwarded_receipts(scenario, &peer, &request, true);
            } else {
                verify_phone_receipts(scenario, &peer, &request);
            }
            assert_eq!(f(&confirmed.2, "phoneCallId"), &"call".into());
            assert_eq!(f(&confirmed.2, "activeRoute"), &"phone".into());
            assert!(f(&confirmed.2, "chatRequest").is_null());
        } else {
            let proofs = frames(&peer, "delivery-proof");
            assert_eq!(proofs.len(), 2);
            assert!(
                proofs
                    .iter()
                    .all(|p| f(p, "generation") == &Json::Number(2.0))
            );
            let operations = frames(&peer, "delivery-operation");
            assert_eq!(operations.len(), 1);
            let operation = f(&operations[0], "operation");
            assert_eq!(
                hash_json(f(operation, "request")).unwrap(),
                hash_json(&request).unwrap()
            );
            assert_eq!(
                f(operation, "requestHash"),
                &hash_json(&request).unwrap().into()
            );
        }
        if fallback {
            assert_eq!(f(&confirmed.2, "activeRoute"), &"chat".into());
            assert_eq!(f(&confirmed.2, "requestedRoute"), &"phone".into());
            assert_eq!(f(&confirmed.2, "phoneCallId"), &Json::Null);
            verify_phone_refusal(scenario, &peer, f(&confirmed.2, "phoneRequest"));
            verify_fallback_history(scenario, &fixture, &initial.0, "confirmed");
        }
        if forwarded {
            verify_forwarded_history(scenario, &fixture, &initial.0, "confirmed");
        }
        assert_eq!(
            hash_json(&publication(&fixture).2).unwrap(),
            hash_json(&published.2).unwrap()
        );
        scenario.preserved(&fixture, &written_task);
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
        let third = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(third.generation(), 3);
        let replay = bounded(service.control.handler("update")(
            original.clone(),
            c(3, "factory-question"),
        ))
        .await
        .unwrap();
        assert_eq!(hash_json(&replay).unwrap(), hash_json(&outcome).unwrap());
        assert_eq!(count(&fixture, "SELECT count(*) FROM revisions"), revisions);
        assert_eq!(count(&fixture, "SELECT count(*) FROM blobs"), blobs);
        assert_eq!(fixture.notices.get(), notices);
        assert_eq!(
            hash_json(&delivery(&fixture).unwrap().2).unwrap(),
            hash_json(&confirmed.2).unwrap()
        );
        assert_eq!(
            frames(&peer, "delivery-notify").len(),
            if active_phone { 0 } else { 1 }
        );
        if active_phone {
            if forwarded {
                verify_forwarded_receipts(scenario, &peer, &request, true);
            } else {
                verify_phone_receipts(scenario, &peer, &request);
            }
        }
        assert!(
            peer.frames
                .lock()
                .unwrap()
                .iter()
                .filter(|frame| f(frame, "method") == &"tools.call".into())
                .all(|frame| {
                    let name = f(f(frame, "params"), "qualifiedName").as_str().unwrap();
                    (name.starts_with(if active_phone { "phone." } else { "chat." })
                        || fallback && name == "phone.request")
                        && f(frame, "generation") != &Json::Number(3.0)
                })
        );
        scenario.preserved(&fixture, &written_task);
        if fallback {
            verify_phone_refusal(scenario, &peer, f(&confirmed.2, "phoneRequest"));
            verify_fallback_history(scenario, &fixture, &initial.0, "cached-replay");
        }
        if forwarded {
            verify_forwarded_history(scenario, &fixture, &initial.0, "cached-replay");
        }
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
    assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
    peer.finish().await;
    if forwarded {
        verify_forwarded_history(scenario, &fixture, &delivery(&fixture).unwrap().0, "closed");
    }
}

fn verify_forwarded_receipts(scenario: &Scenario, peer: &Peer, request: &Json, confirmed: bool) {
    let admissions = frames(peer, "delivery-phone-request");
    let replies = frames(peer, "delivery-phone-admission-reply");
    assert_eq!(admissions.len(), 1);
    assert_eq!(replies.len(), 1);
    assert_eq!(frames(peer, "delivery-phone-first-admission").len(), 1);
    for frame in [&admissions[0], &replies[0]] {
        assert_eq!(f(frame, "generation"), &Json::Number(1.0));
    }
    assert_eq!(
        hash_json(f(&admissions[0], "request")).unwrap(),
        hash_json(request).unwrap()
    );
    assert!(frames(peer, "delivery-phone-operation").is_empty());
    assert!(frames(peer, "delivery-phone-refusal").is_empty());
    assert!(frames(peer, "delivery-notify").is_empty());
    let rpc: Vec<_> = f(&scenario.row, "boundaries")
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| {
            f(b, "kind") == &"rpc".into()
                && f(b, "args")
                    .as_array()
                    .is_some_and(|a| a[0] == "tools.call".into())
        })
        .collect();
    let admitted = f(
        rpc.iter()
            .find(|b| {
                f(&f(b, "args").as_array().unwrap()[1], "qualifiedName") == &"phone.request".into()
            })
            .unwrap(),
        "value",
    );
    assert_eq!(
        hash_json(f(&replies[0], "value")).unwrap(),
        hash_json(admitted).unwrap()
    );
    assert_ne!(f(admitted, "operationId"), f(request, "operationId"));
    let source: Vec<_> = rpc
        .iter()
        .copied()
        .filter(|b| {
            f(&f(b, "args").as_array().unwrap()[1], "qualifiedName") == &"phone.operation".into()
        })
        .collect();
    assert_eq!(source.len(), 3);
    assert_eq!(
        source
            .iter()
            .map(|b| f(f(b, "value"), "phase").as_str().unwrap())
            .collect::<Vec<_>>(),
        ["submitted", "outcome_unknown", "result"]
    );
    let forwarded = frames(peer, "delivery-phone-forwarded");
    let first_generation: Vec<_> = forwarded
        .iter()
        .filter(|fwd| f(fwd, "generation") == &Json::Number(1.0))
        .collect();
    assert!(first_generation.len() >= 2);
    assert_eq!(
        forwarded
            .iter()
            .filter(|fwd| f(fwd, "generation") == &Json::Number(2.0))
            .count(),
        if confirmed { 1 } else { 0 }
    );
    for (index, frame) in forwarded.iter().enumerate() {
        let generation = f(frame, "generation");
        assert!(generation == &Json::Number(1.0) || generation == &Json::Number(2.0));
        let original = source[if generation == &Json::Number(2.0) {
            2
        } else if index == 0 {
            0
        } else {
            1
        }];
        let mut args = f(&f(original, "args").as_array().unwrap()[1], "arguments").clone();
        args.insert("operationId", f(request, "operationId").clone());
        assert_eq!(
            hash_json(f(frame, "arguments")).unwrap(),
            hash_json(&args).unwrap()
        );
        let mut value = f(original, "value").clone();
        let mut intent = f(&value, "intent").clone();
        intent.insert("operationId", f(request, "operationId").clone());
        assert_eq!(f(&intent, "prompt"), f(request, "voicePrompt"));
        assert_eq!(f(&intent, "callId"), f(admitted, "callId"));
        value.insert("intent", intent);
        assert_eq!(
            hash_json(f(frame, "value")).unwrap(),
            hash_json(&value).unwrap()
        );
    }
}

fn verify_forwarded_history(scenario: &Scenario, fixture: &Fixture, id: &str, stage: &str) {
    let db = fixture.database();
    let mut query = db
        .prepare("SELECT revision,value_json FROM revisions WHERE id=? ORDER BY revision")
        .unwrap();
    let versions: Vec<(u64, Json)> = query
        .query_map([id], |row| {
            Ok((
                u64::try_from(row.get::<_, i64>(0)?).unwrap(),
                json(&row.get::<_, String>(1)?),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let initial = &versions[0].1;
    let request = f(&versions.last().unwrap().1, "phoneRequest");
    let mut original = vec![
        f(f(&scenario.row, "initial"), "first"),
        f(f(f(&scenario.row, "initial"), "current"), "value"),
    ];
    original.extend(
        f(&scenario.row, "boundaries")
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| {
                (f(b, "kind") == &"write".into()).then(|| &f(b, "args").as_array().unwrap()[1])
            }),
    );
    assert_eq!(original.len(), 5);
    assert_eq!(
        versions.len(),
        if stage == "unknown-forward" { 4 } else { 5 }
    );
    for (index, ((revision, actual), source)) in versions.iter().zip(original).enumerate() {
        assert_eq!(*revision, index as u64 + 1);
        let mut expected = source.clone();
        for key in ["taskId", "publication"] {
            expected.insert(key, f(initial, key).clone());
        }
        if !f(&expected, "phoneRequest").is_null() {
            let mut phone = f(&expected, "phoneRequest").clone();
            phone.insert("operationId", f(request, "operationId").clone());
            expected.insert("phoneRequest", phone);
            expected.insert("operationId", f(request, "operationId").clone());
        }
        assert_eq!(
            hash_json(actual).unwrap(),
            hash_json(&expected).unwrap(),
            "Original forwarded Delivery revision {revision}"
        );
    }
    assert_eq!(f(&versions[2].1, "phoneCallId"), &"call".into());
    assert_eq!(f(&versions[2].1, "state"), &"dispatching".into());
    assert_eq!(f(&versions[3].1, "state"), &"outcome_unknown".into());
    assert_eq!(
        f(&versions[3].1, "code"),
        &"phone_prompt_outcome_unknown".into()
    );
    let proof = Json::object([
        ("stage", stage.into()),
        (
            "versions",
            Json::Array(
                versions
                    .iter()
                    .map(|(revision, value)| {
                        Json::object([
                            ("revision", Json::Number(*revision as f64)),
                            ("value", value.clone()),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("publication", publication(fixture).2),
        (
            "activeGates",
            Json::Number(count(fixture, "SELECT count(*) FROM active_gates") as f64),
        ),
        ("opens", Json::Number(fixture.opens.get() as f64)),
        (
            "drops",
            Json::Array(
                fixture
                    .drops
                    .borrow()
                    .iter()
                    .map(|generation| Json::Number(*generation as f64))
                    .collect(),
            ),
        ),
    ]);
    println!(
        "factory-delivery-forwarded {}",
        ivy_contracts::json::stringify(&proof, 32 * 1024 * 1024).unwrap()
    );
}

fn verify_phone_refusal(scenario: &Scenario, peer: &Peer, request: &Json) {
    let admissions = frames(peer, "delivery-phone-request");
    let refusals = frames(peer, "delivery-phone-refusal");
    assert_eq!(admissions.len(), 1);
    assert_eq!(refusals.len(), 1);
    assert_eq!(frames(peer, "delivery-phone-first-admission").len(), 1);
    assert!(frames(peer, "delivery-phone-admission-reply").is_empty());
    assert!(frames(peer, "delivery-phone-operation").is_empty());
    assert_eq!(f(&admissions[0], "generation"), &Json::Number(1.0));
    assert_eq!(
        hash_json(f(&admissions[0], "request")).unwrap(),
        hash_json(request).unwrap()
    );
    let original = f(&scenario.row, "boundaries")
        .as_array()
        .unwrap()
        .iter()
        .find(|b| {
            f(b, "kind") == &"rpc".into()
                && f(b, "args").as_array().is_some_and(|a| {
                    a[0] == "tools.call".into()
                        && f(&a[1], "qualifiedName") == &"phone.request".into()
                })
        })
        .unwrap();
    assert_eq!(
        hash_json(f(&refusals[0], "error")).unwrap(),
        hash_json(f(original, "error")).unwrap()
    );
    assert_eq!(
        f(f(&refusals[0], "error"), "outcome"),
        &"not_executed".into()
    );
    let task = f(f(f(&scenario.row, "initial"), "task"), "value");
    verify_route_request(scenario, request, f(task, "lastHistory"), true);
}

fn verify_fallback_history(scenario: &Scenario, fixture: &Fixture, id: &str, stage: &str) {
    let db = fixture.database();
    let mut statement = db
        .prepare("SELECT revision,value_json FROM revisions WHERE id=? ORDER BY revision")
        .unwrap();
    let versions: Vec<(u64, Json)> = statement
        .query_map([id], |r| {
            Ok((
                u64::try_from(r.get::<_, i64>(0)?).unwrap(),
                json(&r.get::<_, String>(1)?),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let initial = &versions[0].1;
    let latest = &versions.last().unwrap().1;
    let phone = f(latest, "phoneRequest");
    let chat = f(latest, "chatRequest");
    assert_ne!(f(phone, "operationId"), f(chat, "operationId"));
    verify_route_request(scenario, chat, f(chat, "source"), false);
    let source_initial = f(&scenario.row, "initial");
    let mut original = vec![
        f(source_initial, "first"),
        f(f(source_initial, "current"), "value"),
    ];
    original.extend(
        f(&scenario.row, "boundaries")
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| {
                (f(b, "kind") == &"write".into()).then(|| &f(b, "args").as_array().unwrap()[1])
            }),
    );
    assert_eq!(original.len(), 6);
    assert_eq!(
        versions.len(),
        if stage == "lost-chat-reply" { 5 } else { 6 }
    );
    for (index, ((revision, actual), source)) in versions.iter().zip(original).enumerate() {
        assert_eq!(*revision, index as u64 + 1);
        let mut expected = source.clone();
        for key in ["taskId", "publication"] {
            expected.insert(key, f(initial, key).clone());
        }
        for key in ["phoneRequest", "chatRequest"] {
            let mut request = f(&expected, key).clone();
            if request.is_null() {
                continue;
            }
            request.insert(
                "operationId",
                f(
                    if key == "phoneRequest" { phone } else { chat },
                    "operationId",
                )
                .clone(),
            );
            if key == "chatRequest" {
                request.insert("source", f(chat, "source").clone());
                let mut bridge = f(&request, "expectedBridge").clone();
                bridge.insert("callerPrincipalId", "fixture-principal".into());
                request.insert("expectedBridge", bridge);
            }
            expected.insert(key, request);
        }
        if !f(&expected, "operationId").is_null() {
            let request = if f(&expected, "activeRoute") == &"phone".into() {
                phone
            } else {
                chat
            };
            expected.insert("operationId", f(request, "operationId").clone());
        }
        let mut target = f(&expected, "chatTarget").clone();
        if !target.is_null() {
            let mut bridge = f(&target, "expectedBridge").clone();
            bridge.insert("callerPrincipalId", "fixture-principal".into());
            target.insert("expectedBridge", bridge);
            expected.insert("chatTarget", target);
        }
        assert_eq!(
            hash_json(actual).unwrap(),
            hash_json(&expected).unwrap(),
            "Original fallback revision {revision}"
        );
    }
    let refused = &versions[2].1;
    assert_eq!(f(refused, "state"), &"pending".into());
    assert_eq!(f(refused, "activeRoute"), &"chat".into());
    assert_eq!(f(refused, "requestedRoute"), &"phone".into());
    assert_eq!(f(refused, "code"), &"service_unavailable".into());
    for key in [
        "operationId",
        "chatRequest",
        "chatTarget",
        "deliveryEvidence",
        "phoneCallId",
    ] {
        assert_eq!(f(refused, key), &Json::Null);
    }
    assert_eq!(
        hash_json(f(refused, "phoneRequest")).unwrap(),
        hash_json(phone).unwrap()
    );
    let proof = Json::object([
        ("stage", stage.into()),
        (
            "versions",
            Json::Array(
                versions
                    .iter()
                    .map(|(revision, value)| {
                        Json::object([
                            ("revision", Json::Number(*revision as f64)),
                            ("value", value.clone()),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("publication", publication(fixture).2),
        (
            "activeGates",
            Json::Number(count(fixture, "SELECT count(*) FROM active_gates") as f64),
        ),
    ]);
    println!(
        "factory-delivery-fallback {}",
        ivy_contracts::json::stringify(&proof, 32 * 1024 * 1024).unwrap()
    );
}

fn verify_phone_receipts(scenario: &Scenario, peer: &Peer, request: &Json) {
    let admissions = frames(peer, "delivery-phone-request");
    assert!(admissions.len() >= 2);
    assert_eq!(frames(peer, "delivery-phone-first-admission").len(), 1);
    for generation in [1.0, 2.0] {
        assert!(
            admissions
                .iter()
                .any(|frame| f(frame, "generation") == &Json::Number(generation))
        );
    }
    for frame in admissions {
        assert_ne!(f(&frame, "generation"), &Json::Number(3.0));
        assert_eq!(
            hash_json(f(&frame, "request")).unwrap(),
            hash_json(request).unwrap()
        );
    }
    let replies = frames(peer, "delivery-phone-admission-reply");
    assert_eq!(replies.len(), 1);
    assert_eq!(f(&replies[0], "generation"), &Json::Number(2.0));
    let original = f(&scenario.row, "boundaries")
        .as_array()
        .unwrap()
        .iter()
        .find(|b| {
            f(b, "kind") == &"rpc".into()
                && f(b, "args").as_array().is_some_and(|args| {
                    args[0] == "tools.call".into()
                        && f(&args[1], "qualifiedName") == &"phone.request".into()
                })
                && b.get("value").is_some()
        })
        .unwrap();
    let mut expected_reply = f(original, "value").clone();
    expected_reply.insert("operationId", f(request, "operationId").clone());
    assert_eq!(
        hash_json(f(&replies[0], "value")).unwrap(),
        hash_json(&expected_reply).unwrap()
    );
    let operations = frames(peer, "delivery-phone-operation");
    let source: Vec<_> = f(&scenario.row, "boundaries")
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| {
            f(b, "kind") == &"rpc".into()
                && f(b, "args").as_array().is_some_and(|args| {
                    args[0] == "tools.call".into()
                        && f(&args[1], "qualifiedName") == &"phone.operation".into()
                })
        })
        .collect();
    assert_eq!(operations.len(), 3);
    assert_eq!(operations.len(), source.len());
    for (frame, expected) in operations.iter().zip(source) {
        assert_eq!(f(frame, "generation"), &Json::Number(2.0));
        let mut args = f(&f(expected, "args").as_array().unwrap()[1], "arguments").clone();
        if args.get("operationId").is_some() {
            args.insert("operationId", f(request, "operationId").clone());
        }
        assert_eq!(
            hash_json(f(frame, "arguments")).unwrap(),
            hash_json(&args).unwrap()
        );
        assert_eq!(
            hash_json(f(frame, "value")).unwrap(),
            hash_json(f(expected, "value")).unwrap()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn delivery_recovers_original_chat_after_lost_notify_without_answering_question_or_replacing_review()
 {
    run_delivery("factory question lost notify recovers confirmed").await;
}

#[tokio::test(flavor = "current_thread")]
async fn delivery_recovers_original_phone_after_lost_admission_without_answering_question_or_replacing_review()
 {
    run_delivery("factory phone question lost admission recovers confirmed").await;
}

#[tokio::test(flavor = "current_thread")]
async fn delivery_refused_phone_recovers_original_chat_without_answering_question_or_replacing_review()
 {
    run_delivery("factory phone refused question falls back and recovers original chat").await;
}

#[tokio::test(flavor = "current_thread")]
async fn delivery_recovers_original_forwarded_prompt_after_different_admission_without_repeating_contact_or_replacing_review()
 {
    run_delivery("factory phone different admission recovers original forwarded prompt").await;
}
