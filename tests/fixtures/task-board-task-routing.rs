use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json};
use ivy_task_board::{native_lifecycle::LifecycleDocument, operations::Document, task_routing::*};
use std::cell::{Cell, RefCell};
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static N: Json = Json::Null;
    v.get(k).unwrap_or(&N)
}
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn pin(id: &str, rev: f64) -> Json {
    Json::object([("objectId", id.into()), ("revision", Json::Number(rev))])
}
fn task(id: &str) -> Document {
    let value = Json::object([
        ("taskKey", format!("TASK-{id}").into()),
        ("fields", Json::object([("control", "agent".into())])),
        ("workflowState", "todo".into()),
        ("publication", Json::Null),
        ("claim", Json::Null),
        ("latestResult", Json::Null),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
    ]);
    Document {
        pin: pin(id, 1.0),
        content_hash: hash_json(&value).unwrap(),
        value,
        parent_id: Some("root".into()),
    }
}
fn question(id: &str, target: Json) -> Json {
    Json::object([
        ("commentId", id.into()),
        ("purpose", "question".into()),
        ("authorKind", "agent".into()),
        ("body", "Can this task supply its API?".into()),
        ("recipient", target),
        (
            "attachments",
            Json::Array(vec![Json::object([("attachmentId", "proof".into())])]),
        ),
        ("requests", Json::Array(vec![])),
        ("responses", Json::Array(vec![])),
        ("replyTo", Json::Null),
    ])
}
fn recipient(id: &str) -> Json {
    Json::object([("kind", "task".into()), ("taskId", id.into())])
}
fn source() -> Document {
    let mut d = task("source");
    d.value.insert(
        "comments",
        Json::Array(vec![question("q", recipient("target"))]),
    );
    d
}
fn child() -> Document {
    let mut d = task("child");
    d.value.insert("parentTaskId", "parent".into());
    d.value.insert("workflowState", "review".into());
    d.value.insert("latestResult", pin("result", 1.0));
    d
}
struct World {
    routing: QuestionRouting,
    docs: RefCell<Vec<(Document, bool)>>,
    reads: RefCell<Vec<(JsString, bool)>>,
    sent: RefCell<Vec<Json>>,
    scopes: RefCell<Vec<Json>>,
    aggregates: Cell<usize>,
    read_failure: RefCell<Option<IvyError>>,
    aggregate_failure: RefCell<Option<IvyError>>,
    write_failure: RefCell<Option<(IvyError, bool)>>,
}
impl World {
    fn new(docs: Vec<Document>) -> Self {
        Self {
            routing: QuestionRouting::default(),
            docs: RefCell::new(docs.into_iter().map(|d| (d, false)).collect()),
            reads: RefCell::default(),
            sent: RefCell::default(),
            scopes: RefCell::default(),
            aggregates: Cell::new(0),
            read_failure: RefCell::default(),
            aggregate_failure: RefCell::default(),
            write_failure: RefCell::default(),
        }
    }
    fn get(&self, id: &str) -> Document {
        self.docs
            .borrow()
            .iter()
            .find(|(d, _)| f(&d.pin, "objectId") == &Json::from(id))
            .unwrap()
            .0
            .clone()
    }
    async fn route(&self) -> IvyResult<()> {
        self.routing.route(self, &"source".into()).await
    }
    fn put(&self, d: Document, archived: bool) {
        let mut docs = self.docs.borrow_mut();
        docs.retain(|(v, _)| f(&v.pin, "objectId") != f(&d.pin, "objectId"));
        docs.push((d, archived));
    }
    fn append(&self, id: &str, c: Json) {
        let mut d = self.get(id);
        let mut rows = f(&d.value, "comments").as_array().unwrap().to_vec();
        rows.push(c);
        d.value.insert("comments", Json::Array(rows));
        self.put(d, false);
    }
}
impl RoutingEngine for World {
    async fn read_task(&self, id: &JsString, archived: bool) -> IvyResult<LifecycleDocument> {
        self.reads.borrow_mut().push((id.clone(), archived));
        if let Some(error) = self.read_failure.borrow().clone() {
            return Err(error);
        }
        let docs = self.docs.borrow();
        let (d, a) = docs
            .iter()
            .find(|(d, _)| f(&d.pin, "objectId").as_js_string() == Some(id))
            .ok_or_else(|| IvyError::new("not_found", "Exact target not found"))?;
        Ok(LifecycleDocument {
            document: d.clone(),
            metadata: Json::object([("effectivelyArchived", (*a).into())]),
        })
    }
    async fn aggregate(&self, _: &Json, _: &JsString) -> IvyResult<()> {
        self.aggregates.set(self.aggregates.get() + 1);
        if let Some(error) = self.aggregate_failure.borrow().clone() {
            return Err(error);
        }
        Ok(())
    }
    async fn scoped_operation(&self, scope: &Json) -> IvyResult<JsString> {
        self.scopes.borrow_mut().push(scope.clone());
        Ok(hash_json(scope)?.into())
    }
    async fn native_comment(&self, r: &Json) -> IvyResult<()> {
        self.sent.borrow_mut().push(r.clone());
        let failure = self.write_failure.borrow_mut().take();
        if failure.as_ref().is_none_or(|(_, apply)| *apply) {
            let id = s(r, "taskId").to_utf8().unwrap();
            let mut d = self.get(&id);
            assert_eq!(f(&d.pin, "revision"), f(r, "expectedRevision"));
            let mut c = r.clone();
            for k in ["action", "taskId", "expectedRevision"] {
                c.remove(k);
            }
            let mut rows = f(&d.value, "comments").as_array().unwrap().to_vec();
            rows.push(c);
            d.value.insert("comments", Json::Array(rows));
            d.pin.insert(
                "revision",
                Json::Number(f(&d.pin, "revision").as_f64().unwrap() + 1.0),
            );
            self.put(d, false);
        }
        if let Some((error, _)) = failure {
            return Err(error);
        }
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn question_relay_keeps_canonical_source_attachments_and_exact_reply_routing() {
    let w = World::new(vec![source(), task("target")]);
    let before = w.get("source").value;
    w.route().await.unwrap();
    let r = w.sent.borrow()[0].clone();
    assert_eq!(f(&r, "authorKind"), &Json::from("agent"));
    assert_eq!(f(&r, "purpose"), &Json::from("update"));
    assert_eq!(f(&r, "agentDelivery"), &Json::from("queue"));
    assert_eq!(f(&r, "sourceTaskId"), &Json::from("source"));
    assert_eq!(f(&r, "replyTo"), &Json::Null);
    assert_eq!(
        f(&r, "questionSource"),
        &Json::object([("taskId", "source".into()), ("commentId", "q".into())])
    );
    assert_eq!(
        f(&r, "attachments"),
        f(
            &f(&before, "comments").as_array().unwrap()[0],
            "attachments"
        )
    );
    let body = s(&r, "body").to_utf8().unwrap();
    assert!(body.contains("sourceTaskId=target and replyTo=q"));
    assert_eq!(w.get("source").value, before);
    assert_eq!(
        ivy_sdk::open_task_questions(&w.get("source").value, None)
            .unwrap()
            .len(),
        1
    );
    assert!(
        ivy_sdk::open_task_questions(&w.get("target").value, None)
            .unwrap()
            .is_empty()
    );
    w.route().await.unwrap();
    assert_eq!(w.sent.borrow().len(), 1);
    assert!(w.scopes.borrow().is_empty());
    assert_eq!(w.aggregates.get(), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn only_open_task_addressed_questions_are_relayed_and_each_pass_is_bounded() {
    let mut d = source();
    let mut rows = vec![question("user", Json::object([("kind", "user".into())]))];
    for n in 0..12 {
        rows.push(question(&format!("q{n}"), recipient("target")));
    }
    let mut wrong = question("foreign", recipient("target"));
    wrong.insert("sourceTaskId", "foreign".into());
    rows.push(wrong);
    d.value.insert("comments", Json::Array(rows));
    let w = World::new(vec![d, task("target")]);
    w.route().await.unwrap();
    assert_eq!(w.sent.borrow().len(), 10);
    assert_eq!(w.reads.borrow().len(), 11);
    w.reads.borrow_mut().clear();
    w.route().await.unwrap();
    assert_eq!(w.sent.borrow().len(), 12);
    assert_eq!(w.reads.borrow().len(), 3);
    for _ in 0..4 {
        w.reads.borrow_mut().clear();
        w.route().await.unwrap();
        assert!(w.reads.borrow().len() <= 11);
    }
    assert_eq!(w.sent.borrow().len(), 12);
    // Reconnect forgets generation-local progress; retained comments still dedup.
    QuestionRouting::default()
        .route(&w, &"source".into())
        .await
        .unwrap();
    assert_eq!(w.sent.borrow().len(), 12);
    assert!(
        w.sent
            .borrow()
            .iter()
            .all(|r| f(f(r, "questionSource"), "commentId") != &Json::from("user"))
    );
    let mut source = w.get("source");
    source.value.insert("publication", pin("publishing", 1.0));
    w.put(source, false);
    w.route().await.unwrap();
    assert_eq!(w.sent.borrow().len(), 12);
}
#[tokio::test(flavor = "current_thread")]
async fn relay_rotation_handles_resolved_shrinking_and_growing_question_lists() {
    let mut d = source();
    d.value.insert(
        "comments",
        Json::Array(
            (0..25)
                .map(|n| question(&format!("q{n}"), recipient("target")))
                .collect(),
        ),
    );
    let w = World::new(vec![d, task("target")]);
    w.route().await.unwrap();
    assert_eq!(w.sent.borrow().len(), 10);
    // Removing/answering the first 15 leaves ten questions; the old offset must
    // reset rather than skip every remaining question.
    let mut reply = Json::object([
        ("commentId", "answer".into()),
        ("purpose", "update".into()),
        ("authorKind", "agent".into()),
        ("sourceTaskId", "target".into()),
        ("replyTo", "q0".into()),
    ]);
    reply.insert(
        "resolvesQuestions",
        Json::Array((0..15).map(|n| Json::from(format!("q{n}"))).collect()),
    );
    w.append("source", reply);
    w.route().await.unwrap();
    assert_eq!(w.sent.borrow().len(), 20);
    for n in 25..29 {
        w.append("source", question(&format!("q{n}"), recipient("target")));
    }
    w.route().await.unwrap();
    w.route().await.unwrap();
    assert_eq!(w.sent.borrow().len(), 24);
    assert!(w.sent.borrow().iter().all(|r| {
        !(10..15).any(|n| f(f(r, "questionSource"), "commentId") == &Json::from(format!("q{n}")))
    }));
    let mut d = w.get("source");
    d.value.insert("comments", Json::Array(vec![]));
    w.put(d, false);
    w.route().await.unwrap();
    w.append("source", question("new", recipient("target")));
    w.route().await.unwrap();
    assert_eq!(w.sent.borrow().len(), 25);
}
#[tokio::test(flavor = "current_thread")]
async fn shrinking_a_question_list_above_the_batch_limit_resets_before_the_next_relay() {
    let mut d = source();
    d.value.insert(
        "comments",
        Json::Array(
            (0..25)
                .map(|n| question(&format!("q{n}"), recipient("target")))
                .collect(),
        ),
    );
    let w = World::new(vec![d, task("target")]);
    w.route().await.unwrap();
    let mut reply = Json::object([
        ("commentId", "answer".into()),
        ("purpose", "update".into()),
        ("authorKind", "agent".into()),
        ("sourceTaskId", "target".into()),
        ("replyTo", "q0".into()),
    ]);
    reply.insert(
        "resolvesQuestions",
        Json::Array((0..7).map(|n| Json::from(format!("q{n}"))).collect()),
    );
    w.append("source", reply);
    w.reads.borrow_mut().clear();
    w.route().await.unwrap();
    assert_eq!(w.reads.borrow().len(), 11);
    let ids = w
        .sent
        .borrow()
        .iter()
        .map(|r| s(f(r, "questionSource"), "commentId").to_utf8().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, (0..17).map(|n| format!("q{n}")).collect::<Vec<_>>());
    w.route().await.unwrap();
    assert_eq!(w.sent.borrow().len(), 25);
    w.route().await.unwrap();
    assert_eq!(w.sent.borrow().len(), 25);
}
#[tokio::test(flavor = "current_thread")]
async fn addressed_answers_and_explicit_resolutions_stop_relays_but_unrelated_replies_do_not() {
    for (source_task, resolved, expected) in [
        ("target", false, 0),
        ("foreign", false, 1),
        ("foreign", true, 0),
    ] {
        let w = World::new(vec![source(), task("target")]);
        let mut reply = Json::object([
            ("commentId", "answer".into()),
            ("purpose", "update".into()),
            ("authorKind", "agent".into()),
            ("sourceTaskId", source_task.into()),
            ("replyTo", "q".into()),
        ]);
        if resolved {
            reply.insert("resolvesQuestions", Json::Array(vec!["q".into()]));
        }
        w.append("source", reply);
        w.route().await.unwrap();
        assert_eq!(w.sent.borrow().len(), expected);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn relay_retry_uses_current_target_revision_and_lost_reply_never_duplicates_the_comment() {
    for applied in [false, true] {
        let w = World::new(vec![source(), task("target")]);
        *w.write_failure.borrow_mut() = Some((
            IvyError::new("outcome_unknown", "Original relay reply lost")
                .with_outcome(Outcome::Unknown),
            applied,
        ));
        let error = w.route().await.unwrap_err();
        assert_eq!(error.outcome, Outcome::Unknown);
        let first = w.sent.borrow()[0].clone();
        if !applied {
            let mut target = w.get("target");
            target.pin.insert("revision", Json::Number(2.0));
            w.put(target, false);
        }
        w.route().await.unwrap();
        assert_eq!(w.sent.borrow().len(), if applied { 1 } else { 2 });
        if !applied {
            let sent = w.sent.borrow();
            assert_eq!(f(&sent[1], "commentId"), f(&first, "commentId"));
            assert_ne!(f(&sent[1], "operationId"), f(&first, "operationId"));
            assert_eq!(f(&sent[1], "expectedRevision"), &Json::Number(2.0));
        }
        assert_eq!(
            f(&w.get("target").value, "comments")
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
#[tokio::test(flavor = "current_thread")]
async fn relay_body_is_bounded_and_read_failure_is_not_absence() {
    let mut d = source();
    let mut q = question("q", recipient("target"));
    q.insert(
        "body",
        Json::String(JsString(vec![u16::from(b'x'); 65_536])),
    );
    d.value.insert("comments", Json::Array(vec![q]));
    let w = World::new(vec![d, task("target")]);
    w.route().await.unwrap();
    assert_eq!(s(&w.sent.borrow()[0], "body").0.len(), 65_536);
    *w.read_failure.borrow_mut() = Some(IvyError::new(
        "service_unavailable",
        "Original reader failure",
    ));
    let error = w.route().await.unwrap_err();
    assert_eq!(
        error.message_js(),
        JsString::from("Original reader failure")
    );
    assert_eq!(w.sent.borrow().len(), 1);
}
#[tokio::test(flavor = "current_thread")]
async fn delegated_saved_review_is_forwarded_once_per_result_with_parent_owned_acceptance() {
    let w = World::new(vec![child(), task("parent")]);
    let original = w.get("child").value;
    forward_delegated_review(&w, &w.get("child")).await.unwrap();
    let first = w.sent.borrow()[0].clone();
    assert_eq!(f(&first, "sourceTaskId"), &Json::from("child"));
    assert_eq!(f(&first, "taskId"), &Json::from("parent"));
    assert_eq!(f(&first, "purpose"), &Json::from("update"));
    assert_eq!(f(&first, "agentDelivery"), &Json::from("queue"));
    assert!(first.get("handoff").is_none());
    assert!(
        s(&first, "body")
            .to_utf8()
            .unwrap()
            .contains("move the accepted task to Done and archive it")
    );
    assert_eq!(w.get("child").value, original);
    assert_eq!(w.aggregates.get(), 1);
    assert_eq!(w.reads.borrow()[0], ("parent".into(), true));
    forward_delegated_review(&w, &w.get("child")).await.unwrap();
    assert_eq!(w.sent.borrow().len(), 1);
    assert_eq!(w.aggregates.get(), 1);
    let mut next = w.get("child");
    next.pin.insert("revision", Json::Number(20.0));
    next.value.insert("latestResult", pin("result", 2.0));
    w.put(next, false);
    forward_delegated_review(&w, &w.get("child")).await.unwrap();
    assert_eq!(w.sent.borrow().len(), 2);
    assert_ne!(f(&w.sent.borrow()[1], "commentId"), f(&first, "commentId"));
    assert_eq!(w.scopes.borrow().len(), 2);
}
#[tokio::test(flavor = "current_thread")]
async fn delegated_forwarding_requires_a_ready_unclaimed_result_and_available_parent() {
    for field in [
        "parentTaskId",
        "latestResult",
        "claim",
        "publication",
        "workflowState",
    ] {
        let mut d = child();
        d.value.insert(
            field,
            match field {
                "parentTaskId" | "latestResult" => Json::Null,
                "workflowState" => "todo".into(),
                _ => pin("busy", 1.0),
            },
        );
        let w = World::new(vec![d, task("parent")]);
        forward_delegated_review(&w, &w.get("child")).await.unwrap();
        assert!(w.sent.borrow().is_empty());
        assert!(w.reads.borrow().is_empty());
    }
    for state in ["archived", "done", "cancelled", "publishing"] {
        let w = World::new(vec![child(), task("parent")]);
        let mut parent = w.get("parent");
        if state == "publishing" {
            parent.value.insert("publication", pin("busy", 1.0));
        } else if state != "archived" {
            parent.value.insert("workflowState", state.into());
        }
        w.put(parent, state == "archived");
        forward_delegated_review(&w, &w.get("child")).await.unwrap();
        assert!(w.sent.borrow().is_empty());
        assert!(w.scopes.borrow().is_empty());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn delegated_forwarding_propagates_original_aggregate_and_delivery_failures() {
    let w = World::new(vec![child(), task("parent")]);
    *w.aggregate_failure.borrow_mut() = Some(IvyError::new(
        "invalid_result",
        "Original aggregate failure",
    ));
    assert_eq!(
        forward_delegated_review(&w, &w.get("child"))
            .await
            .unwrap_err()
            .code,
        "invalid_result"
    );
    assert!(w.scopes.borrow().is_empty());
    assert!(w.sent.borrow().is_empty());
    *w.aggregate_failure.borrow_mut() = None;
    *w.write_failure.borrow_mut() = Some((
        IvyError::new("outcome_unknown", "Original comment failure").with_outcome(Outcome::Unknown),
        true,
    ));
    assert_eq!(
        forward_delegated_review(&w, &w.get("child"))
            .await
            .unwrap_err()
            .outcome,
        Outcome::Unknown
    );
    forward_delegated_review(&w, &w.get("child")).await.unwrap();
    assert_eq!(w.sent.borrow().len(), 1);
}
