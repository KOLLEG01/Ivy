use ivy_contracts::{IvyJson as Json, IvyResult, JsString};
use ivy_task_board::engine_read::{
    comment_reads::{CommentReadStore, read_task},
    condition_policy::{ConditionStore, task_condition},
    conversation::ConversationPolicies,
    read_policy::TaskReadStore,
};
use std::cell::{Cell, RefCell};
fn pin() -> Json {
    Json::object([("objectId", "self".into()), ("revision", Json::Number(1.0))])
}
fn comment(i: usize) -> Json {
    Json::object([
        ("commentId", format!("c{i}").into()),
        ("sequence", Json::Number(i as f64)),
        ("authorKind", "agent".into()),
        ("purpose", "question".into()),
        ("body", "A retained question".into()),
        (
            "recipient",
            if i % 2 == 0 {
                Json::object([("kind", "task".into()), ("taskId", "peer".into())])
            } else {
                Json::object([("kind", "user".into())])
            },
        ),
        ("run", Json::Null),
        ("requests", Json::Array(vec![])),
        ("responses", Json::Array(vec![])),
        ("attachments", Json::Array(vec![])),
    ])
}
struct Reads {
    header: Json,
    entries: Vec<Json>,
    ranges: RefCell<Vec<(usize, usize)>>,
    active: Cell<usize>,
    maximum: Cell<usize>,
}
impl Reads {
    fn new() -> Self {
        let comments: Vec<_> = (1..=20).map(comment).collect();
        let questions = comments
            .iter()
            .map(|c| {
                Json::object([
                    ("commentId", c.get("commentId").unwrap().clone()),
                    ("sequence", c.get("sequence").unwrap().clone()),
                    (
                        "open",
                        (c.get("sequence").unwrap().as_f64().unwrap() != 2.0).into(),
                    ),
                    ("recipient", c.get("recipient").unwrap().clone()),
                ])
            })
            .collect();
        let tree = Json::object([
            ("kind", "tree".into()),
            ("length", Json::Number(20.0)),
            (
                "summary",
                Json::object([
                    ("model", Json::Number(3.0)),
                    ("directCount", Json::Number(20.0)),
                    ("questions", Json::Array(questions)),
                ]),
            ),
        ]);
        let task = Json::object([
            ("fields", Json::object([("control", "agent".into())])),
            ("workflowState", "waiting".into()),
            ("wakeAt", "2026-12-01T00:00:00.000Z".into()),
            ("dependencies", Json::Array(vec!["peer".into()])),
            ("runtimeWait", Json::Null),
            ("claim", Json::Null),
            ("publication", Json::Null),
            ("latestResult", Json::Null),
            ("comments", Json::Array(vec![])),
            ("commentDeliveries", Json::Array(vec![])),
            ("commentStore", tree),
        ]);
        Self {
            header: Json::object([
                ("pin", pin()),
                (
                    "metadata",
                    Json::object([("effectivelyArchived", true.into())]),
                ),
                ("value", task),
            ]),
            entries: comments
                .into_iter()
                .map(|c| Json::object([("comment", c), ("receipt", Json::Null)]))
                .collect(),
            ranges: RefCell::default(),
            active: Cell::new(0),
            maximum: Cell::new(0),
        }
    }
}
impl TaskReadStore for Reads {
    async fn read(&self, key: &str, address: &Json, header_only: bool) -> IvyResult<Json> {
        assert_eq!(key, "task-board/task");
        assert_eq!(address, &pin());
        assert!(!header_only);
        let mut task = self.header.required("value")?.clone();
        assert!(
            task.required("commentStore")?.get("summary").is_none(),
            "Indexed cards/pages must not hydrate the conversation"
        );
        task.remove("commentStore");
        task.insert(
            "comments",
            Json::Array(
                self.entries
                    .iter()
                    .map(|entry| entry.required("comment").unwrap().clone())
                    .collect(),
            ),
        );
        Ok(task)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_ids_keep_the_original_open_sequence_in_cached_and_fallback_pages() {
    for indexed in [false, true] {
        let mut store = Reads::new();
        for entry in &mut store.entries {
            let c = entry.get_mut("comment").unwrap();
            c.insert("purpose", "comment".into());
            c.insert("recipient", Json::Null);
        }
        for at in [1usize, 12] {
            let c = store.entries[at - 1].get_mut("comment").unwrap();
            c.insert("commentId", "same-id".into());
            c.insert("purpose", "question".into());
            c.insert("recipient", Json::object([("kind", "user".into())]));
            c.insert("body", format!("Original question at {at}").into());
        }
        let reply = store.entries[1].get_mut("comment").unwrap();
        reply.insert("replyTo", "same-id".into());
        reply.insert("authorKind", "user".into());
        // An unrelated non-question with the same ID must not be projected either.
        store.entries[2]
            .get_mut("comment")
            .unwrap()
            .insert("commentId", "same-id".into());
        let task = store.header.get_mut("value").unwrap();
        let tree = task.get_mut("commentStore").unwrap();
        tree.remove("summary");
        if indexed {
            tree.insert(
                "summary",
                Json::object([
                    ("directCount", 20.0.into()),
                    (
                        "questions",
                        Json::Array(vec![Json::object([
                            ("commentId", "same-id".into()),
                            ("sequence", 12.0.into()),
                            ("open", true.into()),
                            ("recipient", Json::object([("kind", "user".into())])),
                        ])]),
                    ),
                ]),
            );
        }
        let condition = |_: &Json, _: Option<&Json>| Ok(Json::Null);
        let policies = ConversationPolicies::with_current_questions(&condition);
        for query in [
            Json::object([(
                "comments",
                Json::object([("limit", 1.0.into()), ("before", 2.0.into())]),
            )]),
            Json::object([(
                "comments",
                Json::object([
                    ("limit", 1.0.into()),
                    ("offset", 0.0.into()),
                    ("questionsOnly", true.into()),
                ]),
            )]),
        ] {
            let page = read_task(&store, &policies, &store.header, &query)
                .await
                .unwrap();
            let open = page.required("openQuestions").unwrap().as_array().unwrap();
            assert_eq!(
                open,
                &[store.entries[11].required("comment").unwrap().clone()]
            );
            if query
                .required("comments")
                .unwrap()
                .get("questionsOnly")
                .is_some()
            {
                assert_eq!(
                    page.required("task")
                        .unwrap()
                        .required("comments")
                        .unwrap()
                        .as_array()
                        .unwrap(),
                    open
                );
                assert_eq!(
                    page.required("commentPage").unwrap().get("total"),
                    Some(&1.0.into())
                );
            } else {
                assert_eq!(
                    page.required("task")
                        .unwrap()
                        .required("comments")
                        .unwrap()
                        .as_array()
                        .unwrap()[0]
                        .get("sequence"),
                    Some(&1.0.into())
                );
            }
        }
        let card = read_task(
            &store,
            &policies,
            &store.header,
            &Json::object([("view", "card".into())]),
        )
        .await
        .unwrap();
        assert_eq!(
            card.required("task").unwrap().get("openQuestionCount"),
            Some(&1.0.into())
        );
        assert!(store.maximum.get() <= 8);
    }
}
impl CommentReadStore for Reads {
    fn comment_summary(
        &self,
        task: &Json,
        tree: &Json,
        owner: &JsString,
    ) -> IvyResult<Option<Json>> {
        assert_eq!(owner, &JsString::from("self"));
        assert!(task.get("nextSteps").is_none());
        Ok(tree.get("summary").cloned())
    }
    async fn comment_range(
        &self,
        _: &Json,
        owner: &JsString,
        start: usize,
        end: usize,
    ) -> IvyResult<Vec<Json>> {
        assert_eq!(owner, &JsString::from("self"));
        self.ranges.borrow_mut().push((start, end));
        self.active.set(self.active.get() + 1);
        self.maximum.set(self.maximum.get().max(self.active.get()));
        tokio::task::yield_now().await;
        self.active.set(self.active.get() - 1);
        Ok(self.entries[start..end].to_vec())
    }
}
impl ConditionStore for Reads {
    fn failed_handoff(&self, _: &JsString) -> IvyResult<Option<Json>> {
        panic!("No completion issue")
    }
}
#[tokio::test(flavor = "current_thread")]
async fn indexed_cards_use_local_wake_facts_and_no_conversation_or_foreign_reads() {
    let store = Reads::new();
    let condition = |task: &Json, wake: Option<&Json>| task_condition(&store, task, wake);
    let policies = ConversationPolicies::with_current_questions(&condition);
    let card = read_task(
        &store,
        &policies,
        &store.header,
        &Json::object([("view", "card".into())]),
    )
    .await
    .unwrap();
    assert_eq!(
        card.required("task").unwrap().get("wakeAt"),
        store.header.required("value").unwrap().get("wakeAt")
    );
    assert_eq!(
        card.required("task").unwrap().get("openQuestionCount"),
        Some(&Json::Number(10.0))
    );
    assert!(card.required("task").unwrap().get("nextStepAt").is_none());
    assert_eq!(
        card.required("executionCondition").unwrap().get("state"),
        Some(&"needs_user".into())
    );
    assert_eq!(card.get("archived"), None);
    assert!(store.ranges.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn current_archive_observations_reach_cards_and_bounded_task_reads_without_changing_the_task_pin()
 {
    let store = Reads::new();
    let condition = |task: &Json, wake: Option<&Json>| task_condition(&store, task, wake);
    let policies = ConversationPolicies::with_current_questions(&condition);
    let mut header = store.header.clone();
    let observation = Json::object([
        ("runtimeEpoch", "same-hive".into()),
        ("sequence", 41.0.into()),
    ]);
    header
        .get_mut("metadata")
        .unwrap()
        .insert("observation", observation.clone());
    for query in [
        Json::object([("view", "card".into())]),
        Json::object([("comments", Json::object([("limit", 0.0.into())]))]),
    ] {
        let view = read_task(&store, &policies, &header, &query).await.unwrap();
        assert_eq!(view.get("observation"), Some(&observation));
        assert_eq!(view.get("archived"), Some(&true.into()));
        assert_eq!(view.get("object"), header.get("pin"));
    }
    let restored = Json::object([
        ("runtimeEpoch", "same-hive".into()),
        ("sequence", 42.0.into()),
    ]);
    header
        .get_mut("metadata")
        .unwrap()
        .insert("observation", restored.clone());
    header
        .get_mut("metadata")
        .unwrap()
        .insert("effectivelyArchived", false.into());
    let view = read_task(
        &store,
        &policies,
        &header,
        &Json::object([("view", "card".into())]),
    )
    .await
    .unwrap();
    assert_eq!(view.get("object"), store.header.get("pin"));
    assert_eq!(view.get("observation"), Some(&restored));
    assert_eq!(view.get("archived"), Some(&false.into()));
}
#[tokio::test(flavor = "current_thread")]
async fn question_pages_keep_sequence_cursors_routes_and_bounded_reads() {
    let store = Reads::new();
    let condition = |task: &Json, wake: Option<&Json>| task_condition(&store, task, wake);
    let policies = ConversationPolicies::with_current_questions(&condition);
    let query = Json::object([(
        "comments",
        Json::object([("questionsOnly", true.into()), ("limit", Json::Number(3.0))]),
    )]);
    let page = read_task(&store, &policies, &store.header, &query)
        .await
        .unwrap();
    let comments = page
        .required("task")
        .unwrap()
        .required("comments")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        comments
            .iter()
            .map(|c| c.required("sequence").unwrap().as_f64().unwrap() as usize)
            .collect::<Vec<_>>(),
        vec![20, 19, 18]
    );
    assert_eq!(
        page.required("commentPage").unwrap().get("nextBefore"),
        Some(&Json::Number(18.0))
    );
    assert_eq!(
        page.required("commentPage").unwrap().get("total"),
        Some(&Json::Number(19.0))
    );
    assert_eq!(
        page.required("openQuestions")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        10
    );
    assert!(store.maximum.get() <= 8);
    assert_eq!(page.get("archived"), Some(&true.into()));
    let query = Json::object([(
        "comments",
        Json::object([("before", Json::Number(18.0)), ("limit", Json::Number(3.0))]),
    )]);
    let older = read_task(&store, &policies, &store.header, &query)
        .await
        .unwrap();
    assert_eq!(
        older
            .required("task")
            .unwrap()
            .required("comments")
            .unwrap()
            .as_array()
            .unwrap()[0]
            .get("sequence"),
        Some(&Json::Number(17.0))
    );
}
