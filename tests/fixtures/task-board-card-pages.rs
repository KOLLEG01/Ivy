use super::*;
use crate::{
    engine_read::{condition_policy::task_condition_snapshot, conversation::ConversationPolicies},
    journal_store::persistent_reads::local_reads::SqliteReads,
    local_journal::{LocalJournal, initialize},
};
use std::{cell::RefCell, collections::VecDeque};

struct Catalog;
impl Contracts for Catalog {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("3.5.0".into())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(false)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        Ok(key == "task-board/task" && ["1.0.0", "3.5.0"].iter().any(|v| version.eq_str(v)))
    }
    fn validate_record(&self, _: &str, _: &JsString, _: &Json) -> IvyResult<()> {
        Ok(())
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        assert_eq!(name, "TaskListQuery");
        assert_eq!(value.get("view"), Some(&"cards".into()));
        Ok(())
    }
}
#[derive(Default)]
struct Source {
    replies: RefCell<VecDeque<(Json, IvyResult<Json>)>>,
    calls: RefCell<Vec<Json>>,
}
impl ObjectReads<IvyError> for Source {
    async fn read(&self, args: Option<Json>) -> IvyResult<Json> {
        let args = args.unwrap();
        self.calls.borrow_mut().push(args.clone());
        let (expected, result) = self
            .replies
            .borrow_mut()
            .pop_front()
            .expect("unexpected body read");
        assert_eq!(args, expected);
        result
    }
}
struct Trees;
impl TreeReads<IvyError> for Trees {
    async fn hydrate(&self, _: Json, _: &Json) -> IvyResult<Option<Json>> {
        panic!("indexed cards must not hydrate history")
    }
    fn summary(&self, _: &Json, tree: &Json, _: &Json) -> IvyResult<Option<Json>> {
        Ok(tree.get("summary").cloned())
    }
    async fn range(&self, _: &Json, _: &Json, _: usize, _: usize) -> IvyResult<Vec<Json>> {
        panic!("cards never read a comment range")
    }
}
fn pin(id: &str, revision: f64) -> Json {
    Json::object([("objectId", id.into()), ("revision", revision.into())])
}
fn task(id: &str) -> Json {
    Json::object([
        ("taskKey", format!("TASK-{id}").into()),
        (
            "fields",
            Json::object([
                ("control", "user".into()),
                ("title", Json::String(JsString(vec![65, 0xd800, 66]))),
                (
                    "description",
                    "unused large header payload".repeat(4096).into(),
                ),
                ("priority", 2.0.into()),
                ("category", Json::Null),
            ]),
        ),
        ("workflowState", "todo".into()),
        ("wakeAt", Json::Null),
        ("dependencies", Json::Array(vec![])),
        ("runtimeWait", Json::Null),
        ("claim", Json::Null),
        ("publication", Json::Null),
        ("latestResult", Json::Null),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        (
            "commentStore",
            Json::object([
                ("kind", "tree".into()),
                ("length", 42.0.into()),
                (
                    "summary",
                    Json::object([
                        ("model", 3.0.into()),
                        ("directCount", 40.0.into()),
                        ("questions", Json::Array(vec![])),
                    ]),
                ),
            ]),
        ),
    ])
}
fn document(id: &str, revision: f64) -> Json {
    Json::object([
        (
            "object",
            Json::object([
                ("id", id.into()),
                ("contractKey", "task-board/task".into()),
                ("parentId", "root".into()),
                ("effectivelyArchived", false.into()),
            ]),
        ),
        (
            "revision",
            Json::object([
                ("objectId", id.into()),
                ("revision", revision.into()),
                ("contractVersion", "3.5.0".into()),
                ("mediaType", "application/json".into()),
                ("contentHash", "original".into()),
            ]),
        ),
        (
            "content",
            Json::object([("encoding", "json".into()), ("value", task(id))]),
        ),
    ])
}
fn row(id: &str, revision: f64, body: bool) -> Json {
    let mut row = pin(id, revision);
    row.insert("contractVersion", "3.5.0".into());
    row.insert(
        "values",
        Json::object([("object.effectivelyArchived", false.into())]),
    );
    if body {
        row.insert("document", document(id, revision));
    }
    row
}
fn page(items: Vec<Json>, cursor: Option<&str>) -> Json {
    Json::object([
        ("items", Json::Array(items)),
        ("nextCursor", cursor.map(Json::from).unwrap_or(Json::Null)),
    ])
}
fn query() -> Json {
    Json::object([("view", "cards".into()), ("limit", 20.0.into())])
}

#[test]
fn public_card_page_contract_preserves_summary_queries_and_bounded_hive_options() {
    let contracts = crate::runtime_inputs::contracts().unwrap();
    let mut cards = query();
    cards.insert(
        "where",
        Json::object([
            ("op", "eq".into()),
            ("field", "data:/workflowState".into()),
            ("value", "todo".into()),
        ]),
    );
    cards.insert(
        "orderBy",
        Json::Array(vec![Json::object([
            ("field", "data:/fields/priority".into()),
            ("direction", "desc".into()),
        ])]),
    );
    cards.insert("includeArchived", true.into());
    cards.insert("cursor", "opaque-keyset".into());
    for valid in [
        cards.clone(),
        Json::object(Vec::<(&str, Json)>::new()),
        Json::object([("status", "todo".into()), ("limit", 100.0.into())]),
    ] {
        contracts
            .validate_definition("TaskListQuery", &valid)
            .unwrap();
    }
    for (field, value) in [
        ("limit", 21.0.into()),
        ("limit", 0.0.into()),
        ("limit", 2.5.into()),
        ("view", "card".into()),
        ("includeContent", true.into()),
        ("status", "todo".into()),
        (
            "orderBy",
            Json::Array(vec![Json::object([
                ("field", "invalid".into()),
                ("direction", "asc".into()),
            ])]),
        ),
    ] {
        let mut invalid = cards.clone();
        invalid.insert(field, value);
        assert!(
            contracts
                .validate_definition("TaskListQuery", &invalid)
                .is_err(),
            "{field}"
        );
    }
    for result in [
        page(vec![], None),
        Json::object([
            ("view", "cards".into()),
            ("items", Json::Array(vec![])),
            ("nextCursor", Json::Null),
        ]),
    ] {
        contracts
            .validate_definition("ListResult", &result)
            .unwrap();
    }
}
async fn run(
    query: Json,
    pages: Vec<IvyResult<Json>>,
    bodies: Vec<(Json, IvyResult<Json>)>,
    cancel_after: Option<usize>,
) -> (IvyResult<Json>, Vec<Json>, Vec<Json>) {
    let db = ivy_store::rusqlite::Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let local = SqliteReads {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        contracts: &Catalog,
        parse: |raw: &str| {
            ivy_contracts::json::parse(raw, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES)
        },
        sqlite_failure: ivy_store::sqlite::error,
    };
    let source = Source {
        replies: RefCell::new(bodies.into()),
        calls: RefCell::new(vec![]),
    };
    let reads = DocumentReads {
        local: &local,
        objects: &source,
        comments: &Trees,
        cache: None,
    };
    let condition = |task: &Json, wake: Option<&Json>| task_condition_snapshot(task, wake, None);
    let policies = ConversationPolicies::with_current_questions(&condition);
    let pending = RefCell::new(VecDeque::from(pages));
    let calls = RefCell::new(vec![]);
    let check = || {
        if cancel_after.is_some_and(|n| calls.borrow().len() >= n) {
            Err(IvyError::new(
                "test_cancelled",
                "Original caller left this page.",
            ))
        } else {
            Ok(())
        }
    };
    let result = reads
        .card_page(query, &policies, &check, |args| {
            calls.borrow_mut().push(args);
            std::future::ready(
                pending
                    .borrow_mut()
                    .pop_front()
                    .expect("unexpected page query"),
            )
        })
        .await;
    assert!(source.replies.borrow().is_empty());
    (result, calls.into_inner(), source.calls.into_inner())
}

#[tokio::test(flavor = "current_thread")]
async fn two_queries_keep_selected_order_and_compact_lossless_cards_without_body_reads() {
    let first = page(
        vec![
            row("b", 2.0, false),
            row("a", 1.0, false),
            row("c", 3.0, false),
        ],
        Some("original-next"),
    );
    let headers = page(
        vec![
            row("a", 1.0, true),
            row("b", 2.0, true),
            row("c", 3.0, true),
        ],
        None,
    );
    let (result, calls, bodies) = run(query(), vec![Ok(first), Ok(headers)], vec![], None).await;
    let result = result.unwrap();
    let cards = result.required("items").unwrap().as_array().unwrap();
    assert_eq!(
        cards
            .iter()
            .map(|c| c.required("object").unwrap().clone())
            .collect::<Vec<_>>(),
        vec![pin("b", 2.0), pin("a", 1.0), pin("c", 3.0)]
    );
    assert_eq!(result.get("nextCursor"), Some(&"original-next".into()));
    assert_eq!(calls.len(), 2);
    assert!(bodies.is_empty());
    assert_eq!(calls[1].get("includeContent"), Some(&true.into()));
    assert_eq!(calls[1].get("includeArchived"), Some(&true.into()));
    for card in cards {
        let task = card.required("task").unwrap();
        let fields = task.required("fields").unwrap();
        assert_eq!(
            fields.get("title"),
            Some(&Json::String(JsString(vec![65, 0xd800, 66])))
        );
        assert!(fields.get("description").is_none());
        assert!(task.get("comments").is_none());
        assert!(task.get("commentStore").is_none());
        assert_eq!(card.get("commentCount"), Some(&42.0.into()));
        assert_eq!(card.get("directCommentCount"), Some(&40.0.into()));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn byte_limited_header_pages_keep_original_page_cursor_and_order() {
    let (result, calls, bodies) = run(
        query(),
        vec![
            Ok(page(
                vec![
                    row("c", 3.0, false),
                    row("a", 1.0, false),
                    row("b", 2.0, false),
                ],
                Some("selected-next"),
            )),
            Ok(page(vec![row("a", 1.0, true)], Some("header-next"))),
            Ok(page(vec![row("b", 2.0, true), row("c", 3.0, true)], None)),
        ],
        vec![],
        None,
    )
    .await;
    let result = result.unwrap();
    assert_eq!(calls.len(), 3);
    assert!(bodies.is_empty());
    assert_eq!(calls[2].get("cursor"), Some(&"header-next".into()));
    assert_eq!(result.get("nextCursor"), Some(&"selected-next".into()));
    assert_eq!(
        result.required("items").unwrap().as_array().unwrap()[0].get("object"),
        Some(&pin("c", 3.0))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn changed_head_uses_the_original_selected_pin_instead_of_its_newer_body() {
    let (result, calls, bodies) = run(
        query(),
        vec![
            Ok(page(vec![row("a", 1.0, false)], None)),
            Ok(page(vec![row("a", 9.0, true)], None)),
        ],
        vec![(pin("a", 1.0), Ok(document("a", 1.0)))],
        None,
    )
    .await;
    assert_eq!(
        result
            .unwrap()
            .required("items")
            .unwrap()
            .as_array()
            .unwrap()[0]
            .get("object"),
        Some(&pin("a", 1.0))
    );
    assert_eq!(calls.len(), 2);
    assert_eq!(bodies, vec![pin("a", 1.0)]);
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_bulk_row_keeps_exact_pin_reads_without_restarting_the_page() {
    let (result, calls, bodies) = run(
        query(),
        vec![
            Ok(page(
                vec![row("b", 2.0, false), row("a", 1.0, false)],
                Some("next"),
            )),
            Err(IvyError::new(
                "result_too_large",
                "One original header exceeds the query page.",
            )),
        ],
        vec![
            (pin("b", 2.0), Ok(document("b", 2.0))),
            (pin("a", 1.0), Ok(document("a", 1.0))),
        ],
        None,
    )
    .await;
    assert_eq!(result.unwrap().get("nextCursor"), Some(&"next".into()));
    assert_eq!(calls.len(), 2);
    assert_eq!(bodies, vec![pin("b", 2.0), pin("a", 1.0)]);
}

#[tokio::test(flavor = "current_thread")]
async fn query_headers_reject_other_roots_contracts_pins_and_duplicate_rows() {
    for (kind, expected) in [
        ("root", "task_board_scope_mismatch"),
        ("contract", "task_board_contract_mismatch"),
        ("revision", "task_board_evidence_mismatch"),
        ("owner", "task_board_evidence_mismatch"),
        ("version", "task_board_evidence_mismatch"),
    ] {
        let mut invalid = row("a", 1.0, true);
        let doc = invalid.get_mut("document").unwrap();
        match kind {
            "root" => {
                doc.get_mut("object")
                    .unwrap()
                    .insert("parentId", "other-root".into());
            }
            "contract" => {
                doc.get_mut("object")
                    .unwrap()
                    .insert("contractKey", "task-board/run".into());
            }
            "revision" => {
                doc.get_mut("revision")
                    .unwrap()
                    .insert("revision", 7.0.into());
            }
            "owner" => {
                doc.get_mut("revision")
                    .unwrap()
                    .insert("objectId", "other".into());
            }
            "version" => {
                invalid.insert("contractVersion", "1.0.0".into());
            }
            _ => unreachable!(),
        }
        let (result, _, bodies) = run(
            query(),
            vec![
                Ok(page(vec![row("a", 1.0, false)], None)),
                Ok(page(vec![invalid], None)),
            ],
            vec![],
            None,
        )
        .await;
        assert!(result.unwrap_err().code_js().eq_str(expected), "{kind}");
        assert!(bodies.is_empty());
    }
    let (result, _, _) = run(
        query(),
        vec![Ok(page(
            vec![row("a", 1.0, false), row("a", 1.0, false)],
            None,
        ))],
        vec![],
        None,
    )
    .await;
    assert!(
        result
            .unwrap_err()
            .code_js()
            .eq_str("task_board_evidence_mismatch")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn page_and_body_failures_preserve_original_details_without_blind_replay() {
    let original =
        IvyError::new("original_read_failure", "Keep this provider failure.").with_details(
            Json::object([("opaque", Json::String(JsString(vec![0xd800])))]),
        );
    for pages in [
        vec![Err(original.clone())],
        vec![
            Ok(page(vec![row("a", 1.0, false)], None)),
            Err(original.clone()),
        ],
    ] {
        let count = pages.len();
        let (result, calls, bodies) = run(query(), pages, vec![], None).await;
        assert_eq!(result.unwrap_err().to_wire(), original.to_wire());
        assert_eq!(calls.len(), count);
        assert!(bodies.is_empty());
    }
    let (result, calls, bodies) = run(
        query(),
        vec![
            Ok(page(vec![row("a", 1.0, false)], None)),
            Ok(page(vec![], None)),
        ],
        vec![(pin("a", 1.0), Err(original.clone()))],
        None,
    )
    .await;
    assert_eq!(result.unwrap_err().to_wire(), original.to_wire());
    assert_eq!(calls.len(), 2);
    assert_eq!(bodies.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn retained_inline_cards_use_the_original_legacy_reader_and_comment_counts() {
    let mut original = document("legacy", 1.0);
    original
        .get_mut("revision")
        .unwrap()
        .insert("contractVersion", "1.0.0".into());
    let value = original
        .get_mut("content")
        .unwrap()
        .get_mut("value")
        .unwrap();
    value.remove("commentStore");
    value.insert(
        "comments",
        Json::Array(
            (1..=2)
                .map(|n| {
                    Json::object([
                        ("commentId", format!("note{n}").into()),
                        ("sequence", Json::Number(n as f64)),
                        ("authorKind", "user".into()),
                        ("purpose", "update".into()),
                        ("body", "Retained note".into()),
                        ("requests", Json::Array(vec![])),
                        ("responses", Json::Array(vec![])),
                        ("attachments", Json::Array(vec![])),
                        ("run", Json::Null),
                    ])
                })
                .collect(),
        ),
    );
    let mut selected = row("legacy", 1.0, false);
    selected.insert("contractVersion", "1.0.0".into());
    let mut bulk = selected.clone();
    bulk.insert("document", original.clone());
    let (result, queries, bodies) = run(
        query(),
        vec![Ok(page(vec![selected], None)), Ok(page(vec![bulk], None))],
        vec![(pin("legacy", 1.0), Ok(original))],
        None,
    )
    .await;
    let result = result.unwrap();
    let card = &result.required("items").unwrap().as_array().unwrap()[0];
    assert_eq!(card.get("commentCount"), Some(&2.0.into()));
    assert_eq!(card.get("directCommentCount"), Some(&2.0.into()));
    assert_eq!(queries.len(), 2);
    assert_eq!(bodies, vec![pin("legacy", 1.0)]);
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_header_rows_and_cursors_stop_before_more_reads() {
    for duplicate in [true, false] {
        let selected = page(
            vec![
                row("a", 1.0, false),
                row("b", 1.0, false),
                row("c", 1.0, false),
            ],
            None,
        );
        let pages = if duplicate {
            vec![
                Ok(selected),
                Ok(page(vec![row("a", 1.0, true), row("a", 1.0, true)], None)),
            ]
        } else {
            vec![
                Ok(selected),
                Ok(page(vec![row("a", 1.0, true)], Some("repeated"))),
                Ok(page(vec![row("b", 1.0, true)], Some("repeated"))),
            ]
        };
        let (result, _, bodies) = run(query(), pages, vec![], None).await;
        assert!(
            result
                .unwrap_err()
                .code_js()
                .eq_str("task_board_evidence_mismatch")
        );
        assert!(bodies.is_empty());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn empty_pages_limits_and_cancellation_never_start_unneeded_header_queries() {
    let (result, calls, bodies) = run(query(), vec![Ok(page(vec![], None))], vec![], None).await;
    assert!(
        result
            .unwrap()
            .required("items")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(calls.len(), 1);
    assert!(bodies.is_empty());
    for limit in [0.0, 20.5, 21.0] {
        let mut q = query();
        q.insert("limit", limit.into());
        let (result, calls, _) = run(q, vec![], vec![], None).await;
        assert!(result.unwrap_err().code_js().eq_str("invalid_arguments"));
        assert!(calls.is_empty());
    }
    let (result, calls, bodies) = run(
        query(),
        vec![Ok(page(vec![row("a", 1.0, false)], None))],
        vec![],
        Some(1),
    )
    .await;
    assert!(result.unwrap_err().code_js().eq_str("test_cancelled"));
    assert_eq!(calls.len(), 1);
    assert!(bodies.is_empty());
}
