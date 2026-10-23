//! Immutable conversation writes with bounded batches and producer draining.
use super::{Result, array, hash, immutable, obj, require, spread, text, write_retained_graph};
use crate::{
    journal_store::{
        mutation_ports::{LocalMutationSource, WriteFailure},
        persistent_reads::{
            comment_port::CommentPort,
            json_values::{self as v, Value},
        },
    },
    reconciler::ReconcileExecutor,
};
use ivy_contracts::{IvyError, Outcome};
use ivy_sdk::task_comments::CommentSource;
use std::{
    cell::RefCell,
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};
struct Reply<E> {
    result: Option<Result<Value, E>>,
    waiters: Vec<Waker>,
}
pub(crate) struct CommentWriteJobs<E> {
    active: RefCell<Vec<Rc<RefCell<Reply<E>>>>>,
    waiters: RefCell<Vec<Waker>>,
}
impl<E> Default for CommentWriteJobs<E> {
    fn default() -> Self {
        Self {
            active: RefCell::default(),
            waiters: RefCell::default(),
        }
    }
}
struct Job<'a, E> {
    jobs: &'a CommentWriteJobs<E>,
    reply: Rc<RefCell<Reply<E>>>,
}
impl<E> Drop for Job<'_, E> {
    fn drop(&mut self) {
        let wake = {
            let mut reply = self.reply.borrow_mut();
            reply.result.get_or_insert_with(|| {
                Err(
                    IvyError::new("internal_error", "Service invocation failed.")
                        .with_outcome(Outcome::Unknown)
                        .into(),
                )
            });
            std::mem::take(&mut reply.waiters)
        };
        let idle = {
            let mut active = self.jobs.active.borrow_mut();
            active.retain(|reply| !Rc::ptr_eq(reply, &self.reply));
            active.is_empty()
        };
        for waker in wake {
            waker.wake();
        }
        if idle {
            for waker in std::mem::take(&mut *self.jobs.waiters.borrow_mut()) {
                waker.wake();
            }
        }
    }
}
struct Request<E>(Rc<RefCell<Reply<E>>>);
impl<E> Future for Request<E> {
    type Output = Result<Value, E>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut reply = self.0.borrow_mut();
        if let Some(result) = &reply.result {
            return Poll::Ready(result.clone());
        }
        if !reply.waiters.iter().any(|w| w.will_wake(cx.waker())) {
            reply.waiters.push(cx.waker().clone());
        }
        Poll::Pending
    }
}
impl<E> CommentWriteJobs<E> {
    fn submit<'job, X: ReconcileExecutor<'job>, F: Future<Output = Result<Value, E>> + 'job>(
        &'job self,
        executor: &X,
        work: F,
    ) -> Request<E> {
        let reply = Rc::new(RefCell::new(Reply {
            result: None,
            waiters: Vec::new(),
        }));
        self.active.borrow_mut().push(reply.clone());
        let guard = Job {
            jobs: self,
            reply: reply.clone(),
        };
        let work = Box::pin(async move {
            let result = work.await;
            guard.reply.borrow_mut().result = Some(result);
            drop(guard);
        });
        executor.spawn(work);
        Request(reply)
    }
    pub(crate) async fn settled(&self) {
        poll_fn(|cx| {
            if self.active.borrow().is_empty() {
                return Poll::Ready(());
            }
            let mut waiters = self.waiters.borrow_mut();
            if !waiters.iter().any(|w| w.will_wake(cx.waker())) {
                waiters.push(cx.waker().clone());
            }
            Poll::Pending
        })
        .await
    }
}

pub(crate) struct TreeCommentWriter<'exec, 'job, 'comments, C: CommentSource + ?Sized, S, I, X, E> {
    pub(crate) jobs: &'job CommentWriteJobs<E>,
    pub(crate) executor: &'exec X,
    pub(crate) source: &'job S,
    pub(crate) identity: &'job I,
    pub(crate) comments: &'job CommentPort<'comments, C, WriteFailure<E>>,
}
impl<C: CommentSource + ?Sized, S, I, X, E> Copy for TreeCommentWriter<'_, '_, '_, C, S, I, X, E> {}
impl<C: CommentSource + ?Sized, S, I, X, E> Clone for TreeCommentWriter<'_, '_, '_, C, S, I, X, E> {
    fn clone(&self) -> Self {
        *self
    }
}
fn pin(saved: &Value) -> std::result::Result<Value, IvyError> {
    Ok(obj([
        (
            "objectId",
            saved.required("object")?.required("id")?.clone(),
        ),
        (
            "revision",
            saved.required("revision")?.required("revision")?.clone(),
        ),
    ]))
}
fn references(node: &Value) -> std::result::Result<Value, IvyError> {
    if node.get("kind") != Some(&text("page")) {
        return Ok(obj([]));
    }
    Ok(v::object(
        v::items(node.required("children")?)?
            .iter()
            .enumerate()
            .map(|(i, child)| {
                Ok((
                    format!("child-{i}").into(),
                    child.required("object")?.clone(),
                ))
            })
            .collect::<std::result::Result<Vec<_>, IvyError>>()?,
    ))
}
async fn save_node<C, S, I, E, Fut>(
    source: &S,
    identity: &I,
    comments: &CommentPort<'_, C, WriteFailure<E>>,
    task_id: Value,
    node: Value,
    node_version: Value,
) -> Result<Value, E>
where
    C: CommentSource + ?Sized,
    S: LocalMutationSource<E>,
    I: Fn(Value) -> Fut,
    Fut: Future<Output = Result<Value, E>>,
{
    let node_hash = hash(&node)?;
    if let Some(pin) = comments.cached_write_pin(&node_hash) {
        return Ok(pin);
    }
    let name = text(format!("Conversation {}", &node_hash[7..]));
    let page = source
        .request_owned(
            "objects.query",
            Some(obj([
                ("contractKey", text("task-board/comment-node")),
                (
                    "where",
                    obj([
                        ("op", text("and")),
                        (
                            "args",
                            array([
                                obj([
                                    ("field", text("object.parentId")),
                                    ("op", text("eq")),
                                    ("value", task_id.clone()),
                                ]),
                                obj([
                                    ("field", text("object.name")),
                                    ("op", text("eq")),
                                    ("value", name.clone()),
                                ]),
                            ]),
                        ),
                    ]),
                ),
                ("limit", Value::Number(1.0)),
                ("includeArchived", Value::Bool(true)),
            ])),
        )
        .await?;
    let items = v::items(page.required("items")?)?;
    let saved_pin = if let Some(found) = items.first() {
        let saved_pin = obj([
            ("objectId", found.required("objectId")?.clone()),
            ("revision", found.required("revision")?.clone()),
        ]);
        require(
            hash(&comments.node_for_write(&saved_pin, &task_id).await?)? == node_hash,
            "task_board_evidence_mismatch",
            "The immutable conversation record differs from its identity.",
        )?;
        saved_pin
    } else {
        let owner = v::string(&task_id)?.to_utf8()?;
        let mutation = identity(text(format!("conversation:{owner}:{node_hash}"))).await?;
        let saved = source
            .request_owned(
                "objects.write",
                Some(obj([
                    ("contractVersion", node_version),
                    ("mutationId", mutation),
                    ("references", references(&node)?),
                    (
                        "create",
                        obj([
                            ("name", name),
                            ("parentId", task_id.clone()),
                            ("ownerObjectId", task_id),
                            ("contractKey", text("task-board/comment-node")),
                        ]),
                    ),
                    (
                        "content",
                        obj([("encoding", text("json")), ("value", node.clone())]),
                    ),
                ])),
            )
            .await?;
        pin(&saved)?
    };
    comments.written_node(node_hash, &saved_pin, node)?;
    Ok(saved_pin)
}
impl<'exec, 'job, 'comments, C, S, I, X, E, Fut>
    TreeCommentWriter<'exec, 'job, 'comments, C, S, I, X, E>
where
    C: CommentSource + ?Sized,
    S: LocalMutationSource<E>,
    I: Fn(Value) -> Fut,
    Fut: Future<Output = Result<Value, E>> + 'job,
    X: ReconcileExecutor<'job>,
{
    pub(crate) async fn write_graph(
        self,
        task: &Value,
        task_id: &Value,
        scope: &Value,
        selected_version: &Value,
    ) -> Result<Value, E> {
        let version = if selected_version.is_null() {
            text("3.1.0")
        } else {
            selected_version.clone()
        };
        let current_format = version == text("3.0.0") || version == text("3.1.0");
        if !current_format && version != text("2.2.0") && version != text("2.3.0") {
            return write_retained_graph(
                self.source,
                self.identity,
                task,
                task_id,
                scope,
                &version,
            )
            .await;
        }
        let node_version = text(if current_format { "2.0.0" } else { "1.0.0" });
        let owner = v::string(task_id)?;
        let current = self
            .source
            .request_owned("objects.read", Some(obj([("objectId", task_id.clone())])))
            .await?;
        require(
            current.required("content")?.get("encoding") == Some(&text("json"))
                && current.required("object")?.get("contractKey") == Some(&text("task-board/task")),
            "task_board_contract_mismatch",
            "A conversation needs its owning ticket.",
        )?;
        let header = current.required("content")?.required("value")?;
        let reusable = |header: &Value| -> std::result::Result<bool, IvyError> {
            let Some(tree) = header
                .get("commentStore")
                .filter(|tree| tree.get("kind") == Some(&text("tree")))
            else {
                return Ok(false);
            };
            Ok(
                current.required("revision")?.get("contractVersion") == Some(&version)
                    && self.comments.comments.unchanged_tree(tree, &owner, task)?,
            )
        };
        let reuse = || -> std::result::Result<Value, IvyError> {
            Ok(spread(
                task,
                [
                    ("comments", array([])),
                    ("commentDeliveries", array([])),
                    ("commentStore", header.required("commentStore")?.clone()),
                ],
            ))
        };
        if reusable(header)? {
            return Ok(reuse()?);
        }
        let prior = self
            .comments
            .read_for_write(self.source, header, task_id)
            .await?;
        if reusable(header)? {
            return Ok(reuse()?);
        }
        let proposed = v::items(task.required("comments")?)?;
        for (before, after) in v::items(prior.required("comments")?)?.iter().zip(&proposed) {
            require(
                hash(&immutable(before))? == hash(&immutable(after))?,
                "task_board_comment_mismatch",
                "Published comment content is immutable.",
            )?;
        }
        let mut receipts = std::collections::HashMap::new();
        for receipt in v::items(task.required("commentDeliveries")?)? {
            receipts.insert(v::string(receipt.required("commentId")?)?, receipt);
        }
        let mut level = Vec::new();
        for (offset, chunk) in proposed.chunks(8).enumerate() {
            let mut batch: Vec<Pin<Box<dyn Future<Output = Result<Value, E>> + 'job>>> = Vec::new();
            for (index, comment) in chunk.iter().enumerate() {
                require(
                    comment.get("sequence")
                        == Some(&Value::Number((offset * 8 + index + 1) as f64)),
                    "task_board_comment_mismatch",
                    "Conversation sequence must remain contiguous.",
                )?;
                let receipt = receipts
                    .get(&v::string(comment.required("commentId")?)?)
                    .cloned()
                    .unwrap_or(Value::Null);
                let node = obj([
                    ("schemaVersion", Value::Number(1.0)),
                    ("taskId", task_id.clone()),
                    ("kind", text("comment")),
                    (
                        "entry",
                        obj([("comment", comment.clone()), ("receipt", receipt)]),
                    ),
                ]);
                let (source, identity, comments) = (self.source, self.identity, self.comments);
                let task_id = task_id.clone();
                let version = node_version.clone();
                batch.push(Box::pin(self.jobs.submit(self.executor, async move {
                    let pin = save_node(source, identity, comments, task_id, node, version).await?;
                    Ok(obj([("object", pin), ("length", Value::Number(1.0))]))
                })));
            }
            level.extend(futures_util::future::try_join_all(batch).await?);
        }
        while level.len() > 1 {
            let mut parents = Vec::new();
            for batch in level.chunks(64) {
                let pin = save_node(
                    self.source,
                    self.identity,
                    self.comments,
                    task_id.clone(),
                    obj([
                        ("schemaVersion", Value::Number(1.0)),
                        ("taskId", task_id.clone()),
                        ("kind", text("page")),
                        ("children", array(batch.iter().cloned())),
                    ]),
                    node_version.clone(),
                )
                .await?;
                let length = batch
                    .iter()
                    .map(|child| v::number(child.required("length")?))
                    .collect::<std::result::Result<Vec<_>, IvyError>>()?
                    .iter()
                    .sum();
                parents.push(obj([("object", pin), ("length", Value::Number(length))]));
            }
            level = parents;
        }
        let mut tree = obj([
            ("kind", text("tree")),
            ("root", level.into_iter().next().unwrap_or(Value::Null)),
            ("length", Value::Number(proposed.len() as f64)),
        ]);
        if version != text("2.2.0") {
            tree.insert("summary", self.comments.summary_for_write(task, task_id)?);
        }
        self.comments.comments.remember_tree(&tree, &owner, task)?;
        Ok(spread(
            task,
            [
                ("comments", array([])),
                ("commentDeliveries", array([])),
                ("commentStore", tree),
            ],
        ))
    }
}
