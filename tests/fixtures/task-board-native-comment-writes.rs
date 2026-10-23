use super::*;
use crate::{
    engine_read::read_failure::ReadCause,
    journal_store::persistent_reads::{comment_port::CommentPort, task_comments::TaskComments},
    local_executor::LocalExecutor,
};
use ivy_sdk::task_comments::{CancellationToken, CommentReadFuture, CommentSource};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

struct State {
    header: Value,
    records: HashMap<String, Value>,
    writes: usize,
    lose_reply: bool,
}
#[derive(Clone)]
struct Source(Arc<Mutex<State>>);
impl Source {
    fn new(lose_reply: bool) -> Self {
        Self(Arc::new(Mutex::new(State {
            header: task(0),
            records: HashMap::new(),
            writes: 0,
            lose_reply,
        })))
    }
}
impl CommentSource for Source {
    type Error = &'static str;
    fn read<'a>(
        &'a self,
        pin: Value,
        _: Option<CancellationToken>,
    ) -> CommentReadFuture<'a, Self::Error> {
        Box::pin(async move {
            let id = pin
                .get("objectId")
                .and_then(Value::as_js_string)
                .unwrap()
                .to_utf8()
                .unwrap();
            self.0
                .lock()
                .unwrap()
                .records
                .get(&id)
                .cloned()
                .ok_or("record missing")
        })
    }
}
impl LocalMutationSource<&'static str> for Source {
    async fn request_owned(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, &'static str> {
        let params = params.unwrap();
        let mut state = self.0.lock().unwrap();
        match method {
            "objects.read" => {
                let id = params.required("objectId")?.as_str()?;
                if id == "task" {
                    Ok(obj([
                        (
                            "object",
                            obj([
                                ("id", text("task")),
                                ("contractKey", text("task-board/task")),
                            ]),
                        ),
                        (
                            "revision",
                            obj([
                                ("revision", Value::Number(1.0)),
                                ("contractVersion", text("3.1.0")),
                            ]),
                        ),
                        (
                            "content",
                            obj([("encoding", text("json")), ("value", state.header.clone())]),
                        ),
                    ]))
                } else {
                    state.records.get(&id).cloned().ok_or_else(|| {
                        crate::journal_store::mutation_ports::WriteFailure::source("record missing")
                    })
                }
            }
            "objects.query" => {
                let key = params.required("contractKey")?;
                let name = params
                    .required("where")?
                    .required("args")?
                    .as_array()
                    .unwrap()[1]
                    .required("value")?;
                let items = state
                    .records
                    .values()
                    .filter(|saved| {
                        saved.get("object").and_then(|o| o.get("contractKey")) == Some(key)
                            && saved.get("object").and_then(|o| o.get("name")) == Some(name)
                    })
                    .map(|saved| {
                        obj([
                            (
                                "objectId",
                                saved
                                    .required("object")
                                    .unwrap()
                                    .required("id")
                                    .unwrap()
                                    .clone(),
                            ),
                            (
                                "revision",
                                saved
                                    .required("revision")
                                    .unwrap()
                                    .required("revision")
                                    .unwrap()
                                    .clone(),
                            ),
                        ])
                    })
                    .collect::<Vec<_>>();
                Ok(obj([("items", array(items)), ("nextCursor", Value::Null)]))
            }
            "objects.write" => {
                state.writes += 1;
                let id = params
                    .get("objectId")
                    .cloned()
                    .unwrap_or_else(|| text(format!("record{}", state.writes)));
                let existing = state.records.get(&id.as_str()?).cloned();
                let mut metadata = params
                    .get("create")
                    .cloned()
                    .unwrap_or_else(|| existing.unwrap().required("object").unwrap().clone());
                metadata.insert("id", id.clone());
                let saved = obj([
                    ("object", metadata),
                    (
                        "revision",
                        obj([
                            ("revision", Value::Number(1.0)),
                            (
                                "contractVersion",
                                params.required("contractVersion")?.clone(),
                            ),
                        ]),
                    ),
                    ("content", params.required("content")?.clone()),
                ]);
                state.records.insert(id.as_str()?, saved.clone());
                if std::mem::take(&mut state.lose_reply) {
                    Err(crate::journal_store::mutation_ports::WriteFailure::source(
                        "lost write reply",
                    ))
                } else {
                    Ok(saved)
                }
            }
            _ => panic!("unexpected method {method}"),
        }
    }
}
fn comment(sequence: usize) -> Value {
    obj([
        ("commentId", text(format!("c{sequence}"))),
        ("sequence", Value::Number(sequence as f64)),
        ("authorKind", text("user")),
        ("purpose", text("update")),
        ("body", text(format!("comment {sequence}"))),
        ("attachments", array([])),
        ("delivery", Value::Null),
    ])
}
fn task(count: usize) -> Value {
    obj([
        ("comments", array((1..=count).map(comment))),
        ("commentDeliveries", array([])),
        ("attachments", array([])),
    ])
}
fn identity(scope: Value) -> std::future::Ready<Result<Value, &'static str>> {
    std::future::ready(Ok(text(format!("mutation:{}", hash(&scope).unwrap()))))
}

#[tokio::test(flavor = "current_thread")]
async fn retained_question_retarget_preserves_body_identity_and_appended_tail() {
    let source = Source::new(false);
    let mut question = comment(1);
    question.insert("authorKind", text("agent"));
    question.insert("purpose", text("question"));
    question.insert("recipient", obj([("kind", text("user"))]));
    let mut initial = task(2);
    initial.insert("comments", array([question.clone(), comment(2)]));
    write_retained_graph(
        &source,
        &identity,
        &initial,
        &text("task"),
        &text("initial"),
        &text("1.0.0"),
    )
    .await
    .unwrap();
    let mut retargeted = question.clone();
    retargeted.insert(
        "recipient",
        obj([("kind", text("task")), ("taskId", text("peer"))]),
    );
    let mut prefix = task(1);
    prefix.insert("comments", array([retargeted.clone()]));
    write_retained_graph(
        &source,
        &identity,
        &prefix,
        &text("task"),
        &text("retarget"),
        &text("1.0.0"),
    )
    .await
    .unwrap();
    let state = source.0.lock().unwrap();
    assert_eq!(state.writes, 2);
    let comments = state
        .records
        .values()
        .next()
        .unwrap()
        .required("content")
        .unwrap()
        .required("value")
        .unwrap()
        .required("comments")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0], retargeted);
    assert_eq!(comments[0].get("body"), question.get("body"));
    assert_eq!(comments[1], comment(2));
}
#[tokio::test(flavor = "current_thread")]
async fn native_tree_replays_committed_nodes_after_lost_reply_and_drains_admitted_siblings() {
    let source = Source::new(true);
    let comments = TaskComments::new(Arc::new(source.clone()), tokio::runtime::Handle::current());
    let failure = crate::journal_store::mutation_ports::WriteFailure::source;
    let port = CommentPort {
        comments: &comments,
        source_failure: &failure,
    };
    let jobs = tree::CommentWriteJobs::default();
    let executor = LocalExecutor::default();
    let writer = tree::TreeCommentWriter {
        jobs: &jobs,
        executor: &executor,
        source: &source,
        identity: &identity,
        comments: &port,
    };
    let owner = text("task");
    let scope = text("scope");
    let version = text("3.1.0");
    let proposed = task(2);
    let result = executor
        .drive(writer.write_graph(&proposed, &owner, &scope, &version))
        .await;
    assert!(
        matches!(result,Err(ref e) if matches!(e.cause(),ReadCause::Source("lost write reply")))
    );
    executor.drive(jobs.settled()).await;
    assert_eq!(
        source.0.lock().unwrap().writes,
        2,
        "both admitted leaves finish"
    );
    // A new reader has no pins from the failed attempt; replay must find and verify the saved nodes.
    let comments = TaskComments::new(Arc::new(source.clone()), tokio::runtime::Handle::current());
    let port = CommentPort {
        comments: &comments,
        source_failure: &failure,
    };
    let jobs = tree::CommentWriteJobs::default();
    let executor = LocalExecutor::default();
    let writer = tree::TreeCommentWriter {
        jobs: &jobs,
        executor: &executor,
        source: &source,
        identity: &identity,
        comments: &port,
    };
    let stored = executor
        .drive(writer.write_graph(&proposed, &owner, &scope, &version))
        .await
        .unwrap();
    executor.drive(jobs.settled()).await;
    let state = source.0.lock().unwrap();
    assert_eq!(
        state.writes, 3,
        "replay reuses both leaves and creates only the page"
    );
    for record in state.records.values() {
        assert_eq!(
            record.required("revision").unwrap().get("contractVersion"),
            Some(&text("2.0.0"))
        );
    }
    drop(state);
    let full = comments
        .hydrate(&stored, &"task".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(full.get("comments"), proposed.get("comments"));
    assert_eq!(full.get("commentDeliveries"), Some(&array([])));
}
#[tokio::test(flavor = "current_thread")]
async fn native_retained_retry_keeps_an_appended_tail_and_rejects_changed_text() {
    let source = Source::new(true);
    let owner = text("task");
    let scope = text("scope");
    let version = text("1.8.0");
    let result = write_retained_graph(&source, &identity, &task(2), &owner, &scope, &version).await;
    assert!(
        matches!(result,Err(ref e) if matches!(e.cause(),ReadCause::Source("lost write reply")))
    );
    let mut prefix = task(1);
    let stored = write_retained_graph(&source, &identity, &prefix, &owner, &scope, &version)
        .await
        .unwrap();
    assert_eq!(
        stored.required("commentStore").unwrap().get("length"),
        Some(&Value::Number(1.0))
    );
    assert_eq!(
        source.0.lock().unwrap().writes,
        1,
        "retry reuses the already committed body"
    );
    let state = source.0.lock().unwrap();
    let saved = state.records.values().next().unwrap();
    assert_eq!(
        saved
            .required("content")
            .unwrap()
            .required("value")
            .unwrap()
            .required("comments")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    drop(state);
    let mut altered = comment(1);
    altered.insert("body", text("changed text"));
    prefix.insert("comments", array([altered]));
    assert!(
        matches!(write_retained_graph(&source,&identity,&prefix,&owner,&scope,&version).await,Err(ref e) if matches!(e.cause(),ReadCause::Ivy(error) if error.code=="task_board_comment_mismatch"))
    );
    assert_eq!(source.0.lock().unwrap().writes, 1);
}
