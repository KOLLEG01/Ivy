use super::*;
use driver::{count, frames, reached};

pub(super) struct Scenario {
    row: Json,
    phone: bool,
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
    fn new(phone: bool) -> Self {
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
        let name = if phone {
            "factory phone question lost admission recovers confirmed"
        } else {
            "factory question lost notify recovers confirmed"
        };
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| f(row, "name") == &Json::from(name))
            .unwrap()
            .clone();
        Self {
            row,
            phone,
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
    fn request_key(&self) -> &'static str {
        if self.phone {
            "phoneRequest"
        } else {
            "chatRequest"
        }
    }
    fn target_key(&self) -> &'static str {
        if self.phone {
            "phoneTarget"
        } else {
            "chatTarget"
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
        assert!(!self.phone);
        assert_eq!(version, &JsString::from("1.3.0"));
        true
    }
    pub(super) fn handoff(&self, task: &Json, comment: &Json) -> IvyResult<JsString> {
        assert!(!self.phone);
        self.verify_prompt(task, comment);
        Ok(f(self.source_request(), "text")
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
fn verify_request(scenario: &Scenario, retained: &Json, history: &Json) {
    let mut expected = scenario.source_request().clone();
    expected.insert("operationId", f(retained, "operationId").clone());
    if !scenario.phone {
        expected.insert("source", history.clone());
        let mut bridge = f(&expected, "expectedBridge").clone();
        bridge.insert("callerPrincipalId", "fixture-principal".into());
        expected.insert("expectedBridge", bridge);
    }
    assert_eq!(hash_json(retained).unwrap(), hash_json(&expected).unwrap());
    let id = f(retained, "operationId").as_str().unwrap();
    if scenario.phone {
        assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
        return;
    }
    let parts: Vec<_> = id.split(':').collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], "11111111-1111-4111-8111-111111111111");
    assert!(parts[1].parse::<u64>().is_ok());
    assert!(uuid::Uuid::parse_str(parts[2]).is_ok());
}

async fn run_delivery(phone: bool) {
    let peer = Peer::start_mode(Some("--factory-delivery")).await;
    let mut fixture = Fixture::new(false);
    fixture.delivery = Some(Scenario::new(phone));
    let scenario = fixture.delivery.as_ref().unwrap();
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
            "lost original channel admission receipt",
            || {
                delivery(&fixture)
                    .is_some_and(|row| f(&row.2, "state") == &"outcome_unknown".into())
            },
        )
        .await;
        let uncertain = delivery(&fixture).unwrap();
        assert_eq!(uncertain.0, initial.0);
        assert_eq!(uncertain.1, 3);
        assert_eq!(
            f(&uncertain.2, "code"),
            &Json::from(if phone {
                "service_unavailable"
            } else {
                "outcome_unknown"
            })
        );
        let request = f(&uncertain.2, scenario.request_key()).clone();
        verify_request(scenario, &request, f(&written_task, "lastHistory"));
        assert_eq!(f(&uncertain.2, "operationId"), f(&request, "operationId"));
        if phone {
            assert_eq!(frames(&peer, "delivery-phone-first-admission").len(), 1);
            assert_eq!(f(&uncertain.2, "phoneCallId"), &Json::Null);
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
        assert_eq!(confirmed.1, if phone { 5 } else { 4 });
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
        for key in ["operationId", scenario.request_key(), scenario.target_key()] {
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
        if phone {
            verify_phone_receipts(scenario, &peer, &request);
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
            if phone { 0 } else { 1 }
        );
        if phone {
            verify_phone_receipts(scenario, &peer, &request);
        }
        assert!(
            peer.frames
                .lock()
                .unwrap()
                .iter()
                .filter(|frame| f(frame, "method") == &"tools.call".into())
                .all(|frame| f(f(frame, "params"), "qualifiedName")
                    .as_str()
                    .unwrap()
                    .starts_with(if phone { "phone." } else { "chat." })
                    && f(frame, "generation") != &Json::Number(3.0))
        );
        scenario.preserved(&fixture, &written_task);
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = tokio::join!(owner.run(&factory), script);
    result.unwrap();
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
    assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
    peer.finish().await;
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
    run_delivery(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn delivery_recovers_original_phone_after_lost_admission_without_answering_question_or_replacing_review()
 {
    run_delivery(true).await;
}
