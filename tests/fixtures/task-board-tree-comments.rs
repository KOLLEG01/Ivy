use ivy_contracts::{IvyError, IvyJson as Json, JsString};
use ivy_sdk::task_comments::{CancellationToken, CommentReadFuture, CommentSource};
use ivy_task_board::journal_store::persistent_reads::task_comments::{
    CommentFailure, TaskComments,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
fn pin(id: &str) -> Json {
    Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))])
}
fn index(id: &str, length: usize) -> Json {
    Json::object([("object", pin(id)), ("length", Json::Number(length as f64))])
}
fn tree() -> Json {
    Json::object([
        ("kind", "tree".into()),
        ("root", index("root", 5)),
        ("length", Json::Number(5.0)),
    ])
}
fn comment(sequence: usize, author: &str, purpose: &str) -> Json {
    Json::object([
        ("commentId", format!("c{sequence}").into()),
        ("sequence", Json::Number(sequence as f64)),
        ("authorKind", author.into()),
        ("purpose", purpose.into()),
        ("body", "retained text".into()),
        ("attachments", Json::Array(vec![])),
        ("requests", Json::Array(vec![])),
        ("responses", Json::Array(vec![])),
    ])
}
fn saved(id: &str, node: Json) -> Json {
    Json::object([
        (
            "object",
            Json::object([
                ("id", id.into()),
                ("contractKey", "task-board/comment-node".into()),
                ("parentId", "task".into()),
                ("ownerObjectId", "task".into()),
            ]),
        ),
        (
            "revision",
            Json::object([
                ("revision", Json::Number(1.0)),
                ("contractVersion", "2.0.0".into()),
            ]),
        ),
        (
            "content",
            Json::object([("encoding", "json".into()), ("value", node)]),
        ),
    ])
}
struct Source {
    nodes: HashMap<String, Json>,
    reads: AtomicUsize,
    error: Option<IvyError>,
}
impl CommentSource for Source {
    type Error = IvyError;
    fn read<'a>(
        &'a self,
        pin: Json,
        _: Option<CancellationToken>,
    ) -> CommentReadFuture<'a, Self::Error> {
        Box::pin(async move {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            Ok(self.nodes[&pin.required("objectId")?.as_str()?].clone())
        })
    }
}
fn source() -> Arc<Source> {
    let mut comments = vec![
        comment(1, "agent", "question"),
        comment(2, "user", "update"),
        comment(3, "agent", "question"),
        comment(4, "agent", "update"),
        comment(5, "agent", "update"),
    ];
    comments[0].insert("recipient", Json::object([("kind", "user".into())]));
    comments[1].insert("replyTo", "c1".into());
    comments[2].insert(
        "recipient",
        Json::object([("kind", "task".into()), ("taskId", "peer".into())]),
    );
    comments[3].insert("sourceTaskId", "peer".into());
    let attachment = Json::object([
        ("object", pin("artifact")),
        ("filename", "evidence.txt".into()),
    ]);
    comments[4].insert(
        "attachments",
        Json::Array(vec![attachment.clone(), attachment]),
    );
    let mut nodes = HashMap::new();
    for (index, comment) in comments.into_iter().enumerate() {
        let id = format!("node{index}");
        let receipt = if index == 3 {
            Json::object([("commentId", "c4".into()), ("state", "queued".into())])
        } else {
            Json::Null
        };
        let node = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("taskId", "task".into()),
            ("kind", "comment".into()),
            (
                "entry",
                Json::object([("comment", comment), ("receipt", receipt)]),
            ),
        ]);
        nodes.insert(id.clone(), saved(&id, node));
    }
    let root = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskId", "task".into()),
        ("kind", "page".into()),
        (
            "children",
            Json::Array((0..5).map(|i| index(&format!("node{i}"), 1)).collect()),
        ),
    ]);
    nodes.insert("root".into(), saved("root", root));
    Arc::new(Source {
        nodes,
        reads: AtomicUsize::new(0),
        error: None,
    })
}
fn header() -> Json {
    Json::object([
        ("fields", Json::object([("title", "conversation".into())])),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        ("commentStore", tree()),
    ])
}
#[tokio::test]
async fn hydrated_conversations_keep_question_routes_receipts_and_prepared_attachment_pins() {
    let source = source();
    let reader = TaskComments::new(source.clone(), tokio::runtime::Handle::current());
    let owner = JsString::from("task");
    let full = reader.hydrate(&header(), &owner).await.unwrap().unwrap();
    assert!(full.get("commentStore").is_none());
    assert_eq!(
        full.required("comments").unwrap().as_array().unwrap().len(),
        5
    );
    assert_eq!(
        full.required("commentDeliveries")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let summary = reader.summary(&header(), &tree(), &owner).unwrap().unwrap();
    assert_eq!(summary.get("directCount"), Some(&Json::Number(3.0)));
    let questions = summary.required("questions").unwrap().as_array().unwrap();
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].get("commentId"), Some(&Json::from("c3")));
    assert_eq!(
        questions[0].required("recipient").unwrap().get("taskId"),
        Some(&Json::from("peer"))
    );
    assert_eq!(
        reader.prepared_attachments(&header(), &owner).unwrap(),
        Some(vec![pin("artifact")])
    );
    assert!(reader.unchanged_tree(&tree(), &owner, &full).unwrap());
    let mut changed = full.clone();
    let mut comments = full
        .required("comments")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    comments[0].insert("body", "changed".into());
    changed.insert("comments", Json::Array(comments));
    assert!(!reader.unchanged_tree(&tree(), &owner, &changed).unwrap());
    let mut unrelated_header = header();
    unrelated_header.insert(
        "nextSteps",
        Json::Array(vec![Json::object([("requires", Json::Array(vec![]))])]),
    );
    assert_eq!(
        reader.summary(&unrelated_header, &tree(), &owner).unwrap(),
        Some(summary)
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 6);
    let entries = reader
        .range(&tree(), &owner, Some(3), Some(4), None)
        .await
        .unwrap();
    assert_eq!(
        entries[0].required("comment").unwrap().get("commentId"),
        Some(&Json::from("c4"))
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 6);
}
#[tokio::test]
async fn source_failures_and_wrong_owners_never_become_empty_conversations() {
    let error = IvyError::new("read_unavailable", "original source")
        .with_details(Json::object([("cause", "saved".into())]));
    let source = Arc::new(Source {
        nodes: HashMap::new(),
        reads: AtomicUsize::new(0),
        error: Some(error.clone()),
    });
    let reader = TaskComments::new(source, tokio::runtime::Handle::current());
    match reader.hydrate(&header(), &"task".into()).await {
        Err(CommentFailure::Source(original)) => assert_eq!(original.to_wire(), error.to_wire()),
        other => panic!("Unexpected {other:?}"),
    }
    let reader = TaskComments::new(self::source(), tokio::runtime::Handle::current());
    assert!(matches!(
        reader.hydrate(&header(), &"other".into()).await,
        Err(CommentFailure::Ivy(_))
    ));
    assert!(
        reader
            .summary(&header(), &tree(), &"task".into())
            .unwrap()
            .is_none()
    );
}
