//! Immutable conversation writes with bounded batches and producer draining.
use super::{Result, array, hash, immutable, obj, require, spread, text, write_retained_graph};
use crate::{
    journal_store::{
        mutation_ports::{LocalMutationSource, WriteFailure},
        persistent_reads::{
            comment_port::CommentPort,
            json_values::{self as v, Value},
            task_comments::comment_node_version,
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
#[derive(Clone, Copy)]
enum EntryChange {
    Receipt,
    HandoffDelivery,
    Recipient,
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
fn count(value: &Value) -> std::result::Result<usize, IvyError> {
    let n = value.as_f64().ok_or_else(IvyError::internal)?;
    require(
        n.is_finite()
            && n >= 0.0
            && n.fract() == 0.0
            && n <= 9_007_199_254_740_991.0
            && n < usize::MAX as f64,
        "task_board_comment_mismatch",
        "The conversation index is invalid.",
    )?;
    Ok(n as usize)
}
pub(crate) async fn save_node<C, S, I, E, Fut>(
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
    async fn page(self, task_id: &Value, children: &[Value], version: &Value) -> Result<Value, E> {
        let length = children.iter().try_fold(0usize, |total, child| {
            total
                .checked_add(count(child.required("length")?)?)
                .ok_or_else(IvyError::internal)
        })?;
        let pin = save_node(
            self.source,
            self.identity,
            self.comments,
            task_id.clone(),
            obj([
                ("schemaVersion", Value::Number(1.0)),
                ("taskId", task_id.clone()),
                ("kind", text("page")),
                ("children", array(children.iter().cloned())),
            ]),
            version.clone(),
        )
        .await?;
        Ok(obj([
            ("object", pin),
            ("length", Value::Number(length as f64)),
        ]))
    }

    async fn append_child(
        self,
        child: &Value,
        added: &Value,
        task_id: &Value,
        version: &Value,
        offset: usize,
        depth: usize,
    ) -> Result<Vec<Value>, E> {
        require(
            depth <= 12,
            "task_board_comment_mismatch",
            "The conversation index is invalid.",
        )?;
        let length = count(child.required("length")?)?;
        let node = self
            .comments
            .node_for_write(child.required("object")?, task_id)
            .await?;
        if node.get("kind") == Some(&text("comment")) {
            require(
                length == 1
                    && node.required("entry")?.required("comment")?.get("sequence")
                        == Some(&Value::Number((offset + 1) as f64)),
                "task_board_comment_mismatch",
                "Conversation sequence must remain contiguous.",
            )?;
            return Ok(vec![child.clone(), added.clone()]);
        }
        require(
            node.get("kind") == Some(&text("page")),
            "task_board_comment_mismatch",
            "The conversation index is invalid.",
        )?;
        let mut children = v::items(node.required("children")?)?.to_vec();
        let total = children.iter().try_fold(0usize, |total, child| {
            let length = count(child.required("length")?)?;
            require(
                length > 0,
                "task_board_comment_mismatch",
                "The conversation index is incomplete.",
            )?;
            total.checked_add(length).ok_or_else(IvyError::internal)
        })?;
        require(
            !children.is_empty() && children.len() <= 64 && total == length,
            "task_board_comment_mismatch",
            "The conversation index is incomplete.",
        )?;
        let last = children.pop().ok_or_else(IvyError::internal)?;
        let prefix = total - count(last.required("length")?)?;
        children.extend(
            Box::pin(self.append_child(&last, added, task_id, version, offset + prefix, depth + 1))
                .await?,
        );
        let mut result = Vec::new();
        for chunk in children.chunks(64) {
            result.push(self.page(task_id, chunk, version).await?);
        }
        Ok(result)
    }

    async fn replace_entries(
        self,
        child: &Value,
        changes: &[(usize, Value)],
        task_id: &Value,
        version: &Value,
        offset: usize,
        depth: usize,
        mode: EntryChange,
    ) -> Result<Value, E> {
        require(
            depth <= 12,
            "task_board_comment_mismatch",
            "The conversation index is invalid.",
        )?;
        let length = count(child.required("length")?)?;
        let mut node = self
            .comments
            .node_for_write(child.required("object")?, task_id)
            .await?;
        if node.get("kind") == Some(&text("comment")) {
            require(
                length == 1 && changes.len() == 1 && changes[0].0 == offset + 1,
                "task_board_comment_mismatch",
                "The conversation sequence is invalid.",
            )?;
            let previous = node.required("entry")?;
            let change = &changes[0].1;
            let receipt = previous.required("receipt")?;
            let next = change.get("receipt").unwrap_or(receipt);
            let old_comment = previous.required("comment")?;
            let comment = change.required("comment")?;
            let same_comment = hash(old_comment)? == hash(comment)?;
            let handoff_delivery = matches!(mode, EntryChange::HandoffDelivery);
            let delivery_only = handoff_delivery
                && old_comment.get("purpose") == Some(&text("handoff"))
                && old_comment.get("delivery").is_none_or(Value::is_null)
                && comment.get("delivery").is_some_and(|v| !v.is_null())
                && hash(&spread(
                    old_comment,
                    [("delivery", comment.required("delivery")?.clone())],
                ))? == hash(comment)?;
            let recipient_only = matches!(mode, EntryChange::Recipient)
                && change.get("receipt").is_none()
                && hash(&spread(
                    old_comment,
                    [("recipient", comment.required("recipient")?.clone())],
                ))? == hash(comment)?;
            require(
                (same_comment || delivery_only || recipient_only)
                    && old_comment.get("sequence") == Some(&Value::Number((offset + 1) as f64))
                    && ((handoff_delivery || recipient_only) && change.get("receipt").is_none()
                        || !receipt.is_null()
                            && receipt.get("commentId") == next.get("commentId")
                            && receipt.get("operationId") == next.get("operationId")),
                "task_board_comment_mismatch",
                "A receipt update must retain its original comment, sequence and operation.",
            )?;
            if same_comment && hash(receipt)? == hash(next)? {
                return Ok(child.clone());
            }
            node.insert(
                "entry",
                obj([("comment", comment.clone()), ("receipt", next.clone())]),
            );
        } else {
            require(
                node.get("kind") == Some(&text("page")),
                "task_board_comment_mismatch",
                "The conversation index is invalid.",
            )?;
            let mut children = v::items(node.required("children")?)?.to_vec();
            let total = children.iter().try_fold(0usize, |sum, c| {
                let n = count(c.required("length")?)?;
                require(
                    n > 0,
                    "task_board_comment_mismatch",
                    "The conversation index is incomplete.",
                )?;
                sum.checked_add(n).ok_or_else(IvyError::internal)
            })?;
            require(
                !children.is_empty() && children.len() <= 64 && total == length,
                "task_board_comment_mismatch",
                "The conversation index is incomplete.",
            )?;
            let mut at = offset;
            let mut next_change = 0;
            let mut changed = false;
            for child in &mut children {
                let end = at + count(child.required("length")?)?;
                let until = changes.partition_point(|(position, _)| *position <= end);
                if until > next_change {
                    let updated = Box::pin(self.replace_entries(
                        child,
                        &changes[next_change..until],
                        task_id,
                        version,
                        at,
                        depth + 1,
                        mode,
                    ))
                    .await?;
                    changed |= updated != *child;
                    *child = updated;
                }
                next_change = until;
                at = end;
            }
            require(
                next_change == changes.len(),
                "task_board_comment_mismatch",
                "The receipt sequence is outside its original root.",
            )?;
            if !changed {
                return Ok(child.clone());
            }
            node.insert("children", array(children));
        }
        let pin = save_node(
            self.source,
            self.identity,
            self.comments,
            task_id.clone(),
            node,
            version.clone(),
        )
        .await?;
        Ok(obj([
            ("object", pin),
            ("length", Value::Number(length as f64)),
        ]))
    }

    /// The paired arrays contain only original comments and their changed receipts.
    /// No identities, comment bodies, question summaries or leaf order may change.
    pub(crate) async fn receipts(
        self,
        task: &Value,
        task_id: &Value,
        version: &str,
    ) -> Result<Value, E> {
        let comments = task
            .required("comments")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let receipts = task
            .required("commentDeliveries")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        require(
            comments.len() == receipts.len(),
            "task_board_comment_mismatch",
            "A receipt update needs its paired original comments.",
        )?;
        let mut tree = task.required("commentStore")?.clone();
        let length = count(tree.required("length")?)?;
        let root = tree.required("root")?;
        require(
            root.is_null() && length == 0
                || !root.is_null() && count(root.required("length")?)? == length,
            "task_board_comment_mismatch",
            "The conversation index is incomplete.",
        )?;
        let mut changes = comments
            .iter()
            .zip(receipts)
            .map(|(comment, receipt)| {
                require(
                    comment.get("commentId") == receipt.get("commentId"),
                    "task_board_comment_mismatch",
                    "A receipt belongs to its original comment.",
                )?;
                let position = count(comment.required("sequence")?)?;
                require(
                    position > 0 && position <= length,
                    "task_board_comment_mismatch",
                    "The receipt sequence is outside its original root.",
                )?;
                Ok((
                    position,
                    obj([("comment", comment.clone()), ("receipt", receipt.clone())]),
                ))
            })
            .collect::<std::result::Result<Vec<_>, IvyError>>()?;
        changes.sort_unstable_by_key(|(at, _)| *at);
        require(
            changes.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "task_board_comment_mismatch",
            "A receipt sequence can change only once.",
        )?;
        if !changes.is_empty() {
            let version = text(comment_node_version(version).ok_or_else(IvyError::internal)?);
            let next = self
                .replace_entries(
                    root,
                    &changes,
                    task_id,
                    &version,
                    0,
                    0,
                    EntryChange::Receipt,
                )
                .await?;
            tree.insert("root", next);
        }
        Ok(spread(
            task,
            [
                ("comments", array([])),
                ("commentDeliveries", array([])),
                ("commentStore", tree),
            ],
        ))
    }

    /// Original input receipts form a paired prefix. At most one remaining
    /// entry adds a generated result or adds delivery to the original handoff.
    pub(crate) async fn completion(
        self,
        task: &Value,
        task_id: &Value,
        version: &str,
        summary: Option<Value>,
    ) -> Result<Value, E> {
        let comments = task
            .required("comments")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let receipts = task
            .required("commentDeliveries")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        require(
            receipts.len() <= comments.len() && comments.len() <= receipts.len() + 1,
            "task_board_comment_mismatch",
            "A completion retains its paired input receipts and at most one result.",
        )?;
        let mut tree = task.required("commentStore")?.clone();
        let length = count(tree.required("length")?)?;
        let root = tree.required("root")?;
        require(
            root.is_null() && length == 0
                || !root.is_null() && count(root.required("length")?)? == length,
            "task_board_comment_mismatch",
            "The conversation index is incomplete.",
        )?;
        let mut changes = Vec::new();
        let mut added = Vec::new();
        for (at, comment) in comments.iter().enumerate() {
            let position = count(comment.required("sequence")?)?;
            let mut entry = obj([("comment", comment.clone())]);
            if let Some(receipt) = receipts.get(at) {
                require(
                    comment.get("commentId") == receipt.get("commentId") && position <= length,
                    "task_board_comment_mismatch",
                    "A completion receipt belongs to its original comment.",
                )?;
                entry.insert("receipt", receipt.clone());
            }
            if position > 0 && position <= length {
                changes.push((position, entry));
            } else {
                require(
                    position == length + 1
                        && added.is_empty()
                        && at >= receipts.len()
                        && comment.get("authorKind") == Some(&text("agent"))
                        && comment.get("purpose") == Some(&text("update"))
                        && ["requests", "responses", "attachments"].iter().all(|key| {
                            comment
                                .get(*key)
                                .and_then(Value::as_array)
                                .is_some_and(|v| v.is_empty())
                        })
                        && [
                            "nativeInput",
                            "recipient",
                            "sourceTaskId",
                            "questionSource",
                            "handoff",
                            "resolvesQuestions",
                        ]
                        .iter()
                        .all(|key| comment.get(*key).is_none_or(Value::is_null)),
                    "task_board_comment_mismatch",
                    "A completion appends only its generated result.",
                )?;
                added.push(comment.clone());
            }
        }
        changes.sort_unstable_by_key(|(position, _)| *position);
        require(
            changes.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "task_board_comment_mismatch",
            "A completion sequence can change only once.",
        )?;
        if !changes.is_empty() {
            let version = text(comment_node_version(version).ok_or_else(IvyError::internal)?);
            let next = self
                .replace_entries(
                    root,
                    &changes,
                    task_id,
                    &version,
                    0,
                    0,
                    EntryChange::HandoffDelivery,
                )
                .await?;
            tree.insert("root", next);
        }
        let next = spread(
            task,
            [
                ("comments", array(added.clone())),
                ("commentDeliveries", array([])),
                ("commentStore", tree),
            ],
        );
        if added.is_empty() {
            return Ok(next);
        }
        require(
            summary.is_some(),
            "task_board_comment_mismatch",
            "A completion append requires its question summary.",
        )?;
        self.append(&next, task_id, version, false, summary).await
    }

    /// Change only recipients at their original sequences, retaining each receipt.
    pub(crate) async fn retarget(
        self,
        task: &Value,
        task_id: &Value,
        version: &str,
        summary: Value,
    ) -> Result<Value, E> {
        let mut tree = task.required("commentStore")?.clone();
        let length = count(tree.required("length")?)?;
        let root = tree.required("root")?;
        require(
            !root.is_null()
                && count(root.required("length")?)? == length
                && task
                    .required("commentDeliveries")?
                    .as_array()
                    .is_some_and(|v| v.is_empty()),
            "task_board_comment_mismatch",
            "A recipient change retains its original conversation and receipts.",
        )?;
        let mut changes = task
            .required("comments")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|comment| {
                let position = count(comment.required("sequence")?)?;
                require(
                    position > 0 && position <= length,
                    "task_board_comment_mismatch",
                    "The question sequence is outside its original root.",
                )?;
                Ok((position, obj([("comment", comment.clone())])))
            })
            .collect::<std::result::Result<Vec<_>, IvyError>>()?;
        changes.sort_unstable_by_key(|(at, _)| *at);
        require(
            !changes.is_empty() && changes.windows(2).all(|p| p[0].0 != p[1].0),
            "task_board_comment_mismatch",
            "A question sequence can change only once.",
        )?;
        let node_version = text(comment_node_version(version).ok_or_else(IvyError::internal)?);
        let next = self
            .replace_entries(
                root,
                &changes,
                task_id,
                &node_version,
                0,
                0,
                EntryChange::Recipient,
            )
            .await?;
        tree.insert("root", next);
        tree.insert("summary", summary);
        Ok(spread(
            task,
            [
                ("comments", array([])),
                ("commentDeliveries", array([])),
                ("commentStore", tree),
            ],
        ))
    }

    /// The owner validates the new identities and supplies an updated summary
    /// for ordinary comments. Neither mode replaces existing entries.
    pub(crate) async fn append(
        self,
        task: &Value,
        task_id: &Value,
        version: &str,
        system_only: bool,
        summary: Option<Value>,
    ) -> Result<Value, E> {
        let mut tree = task.required("commentStore")?.clone();
        let mut length = count(tree.required("length")?)?;
        let comments = v::items(task.required("comments")?)?;
        let receipts = v::items(task.required("commentDeliveries")?)?;
        require(
            !comments.is_empty(),
            "task_board_comment_mismatch",
            "An append needs its new conversation entries.",
        )?;
        for (index, comment) in comments.iter().enumerate() {
            require(
                comment.get("sequence") == Some(&Value::Number((length + index + 1) as f64))
                    && (!system_only
                        || comment.get("authorKind") == Some(&text("system"))
                            && comment
                                .get("purpose")
                                .is_none_or(|v| v.is_null() || v == &text("update"))
                            && ["requests", "responses", "attachments"].iter().all(|key| {
                                comment
                                    .get(*key)
                                    .and_then(Value::as_array)
                                    .is_some_and(|v| v.is_empty())
                            })
                            && [
                                "replyTo",
                                "nativeInput",
                                "resolvesQuestions",
                                "questionSource",
                                "sourceTaskId",
                                "handoff",
                            ]
                            .iter()
                            .all(|key| comment.get(*key).is_none_or(Value::is_null))),
                "task_board_comment_mismatch",
                "Only generated edit notices use this append path.",
            )?;
        }
        require(
            receipts.iter().all(|r| {
                comments
                    .iter()
                    .any(|c| c.get("commentId") == r.get("commentId"))
            }),
            "task_board_comment_mismatch",
            "An append cannot change a previous receipt.",
        )?;
        let node_version = text(comment_node_version(version).ok_or_else(IvyError::internal)?);
        let mut root = tree.required("root")?.clone();
        require(
            root.is_null() && length == 0
                || !root.is_null() && count(root.required("length")?)? == length,
            "task_board_comment_mismatch",
            "The conversation index is incomplete.",
        )?;
        for comment in comments {
            let receipt = receipts
                .iter()
                .find(|r| r.get("commentId") == comment.get("commentId"))
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
            let pin = save_node(
                self.source,
                self.identity,
                self.comments,
                task_id.clone(),
                node,
                node_version.clone(),
            )
            .await?;
            let added = obj([("object", pin), ("length", Value::Number(1.0))]);
            root = if root.is_null() {
                added
            } else {
                let children = self
                    .append_child(&root, &added, task_id, &node_version, 0, 0)
                    .await?;
                if children.len() == 1 {
                    children.into_iter().next().unwrap()
                } else {
                    self.page(task_id, &children, &node_version).await?
                }
            };
            length += 1;
        }
        let summary = if let Some(summary) = summary {
            summary
        } else {
            let mut summary = tree.required("summary")?.clone();
            let direct = count(summary.required("directCount")?)? + comments.len();
            summary.insert("directCount", Value::Number(direct as f64));
            summary
        };
        tree.insert("summary", summary);
        tree.insert("root", root);
        tree.insert("length", Value::Number(length as f64));
        Ok(spread(
            task,
            [
                ("comments", array([])),
                ("commentDeliveries", array([])),
                ("commentStore", tree),
            ],
        ))
    }

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
        let Some(node_version) = version
            .as_str()
            .ok()
            .as_deref()
            .and_then(comment_node_version)
        else {
            return write_retained_graph(
                self.source,
                self.identity,
                task,
                task_id,
                scope,
                &version,
            )
            .await;
        };
        let node_version = text(node_version);
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
        let previous = v::items(prior.required("comments")?)?;
        require(
            proposed.len() >= previous.len(),
            "task_board_comment_mismatch",
            "Published conversation entries cannot be removed.",
        )?;
        for (before, after) in previous.iter().zip(proposed) {
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
                    .copied()
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
