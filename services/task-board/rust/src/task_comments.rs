//! Conversation reads and small projections over the shared JSON reader.
use super::comment_projections as projection;
use crate::engine_read::read_policy::{IvyPolicies, QuestionPolicies};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::hash_json,
    json::{canonical, stringify},
    limits::JSON_OBJECT_CONTENT_BYTES,
};
use ivy_sdk::task_comments::{
    CancellationToken, CommentError, CommentReadFuture, CommentReader, CommentSource,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::runtime::Handle;

/// Bound each persisted header and conversation entry independently.
pub fn validate_task_content(task: &Json, task_id: &JsString) -> IvyResult<()> {
    let mut header = task.clone();
    header.insert("comments", Json::Array(vec![]));
    header.insert("commentDeliveries", Json::Array(vec![]));
    header.insert(
        "commentStore",
        Json::object([
            ("kind", "tree".into()),
            ("root", Json::Null),
            ("length", Json::Number(0.0)),
        ]),
    );
    canonical(&header, JSON_OBJECT_CONTENT_BYTES - 1024)?;
    let mut receipts = HashMap::new();
    for receipt in task
        .required("commentDeliveries")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        receipts.insert(
            receipt
                .required("commentId")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
            receipt,
        );
    }
    for comment in task
        .required("comments")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        let id = comment
            .required("commentId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        canonical(
            &Json::object([
                ("schemaVersion", Json::Number(1.0)),
                ("taskId", Json::String(task_id.clone())),
                ("kind", "comment".into()),
                (
                    "entry",
                    Json::object([
                        ("comment", comment.clone()),
                        (
                            "receipt",
                            receipts.get(id).copied().cloned().unwrap_or(Json::Null),
                        ),
                    ]),
                ),
            ]),
            JSON_OBJECT_CONTENT_BYTES - 1024,
        )?;
    }
    Ok(())
}

#[derive(Debug)]
pub enum CommentFailure<E> {
    Source(E),
    Ivy(IvyError),
}
impl<E> From<IvyError> for CommentFailure<E> {
    fn from(error: IvyError) -> Self {
        Self::Ivy(error)
    }
}
impl<E> From<CommentError<CommentFailure<E>>> for CommentFailure<E> {
    fn from(error: CommentError<CommentFailure<E>>) -> Self {
        match error {
            CommentError::Source(error) => error,
            CommentError::Invalid(error) => Self::Ivy(error),
        }
    }
}
struct ReadSource<S: CommentSource + ?Sized>(Arc<S>);
impl<S: CommentSource + ?Sized> CommentSource for ReadSource<S> {
    type Error = CommentFailure<S::Error>;
    fn read<'a>(
        &'a self,
        pin: Json,
        cancellation: Option<CancellationToken>,
    ) -> CommentReadFuture<'a, Self::Error> {
        Box::pin(async move {
            self.0
                .read(pin, cancellation)
                .await
                .map_err(CommentFailure::Source)
        })
    }
}
pub(super) struct ProjectionCache<T = Json> {
    pub(super) values: HashMap<String, T>,
    order: VecDeque<String>,
}
impl<T> ProjectionCache<T> {
    pub(super) fn new() -> Self {
        Self {
            values: HashMap::new(),
            order: VecDeque::new(),
        }
    }
    pub(super) fn remember(&mut self, key: String, value: T, maximum: usize) {
        self.evict_if_full(maximum);
        self.insert(key, value)
    }
    pub(super) fn evict_if_full(&mut self, maximum: usize) {
        if self.values.len() >= maximum {
            if let Some(oldest) = self.order.pop_front() {
                self.values.remove(&oldest);
            }
        }
    }
    pub(super) fn insert(&mut self, key: String, value: T) {
        if !self.values.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        self.values.insert(key, value);
    }
}
struct Fingerprint {
    hash: String,
    attachments: Option<Vec<Json>>,
}
pub struct TaskComments<S: CommentSource + ?Sized> {
    reader: CommentReader<ReadSource<S>>,
    pins: Arc<Mutex<ProjectionCache>>,
    summaries: Mutex<ProjectionCache>,
    fingerprints: Mutex<ProjectionCache<Fingerprint>>,
}
impl<S: CommentSource + ?Sized> TaskComments<S> {
    pub fn new(source: Arc<S>, executor: Handle) -> Self {
        let pins = Arc::new(Mutex::new(ProjectionCache::new()));
        let observed = pins.clone();
        let reader = CommentReader::new(Arc::new(ReadSource(source)), executor).with_node_observer(
            Arc::new(move |pin, node| {
                observed.lock().unwrap_or_else(|p| p.into_inner()).remember(
                    hash_json(node).map_err(CommentFailure::Ivy)?,
                    pin.clone(),
                    16384,
                );
                Ok(())
            }),
        );
        Self {
            reader,
            pins,
            summaries: Mutex::new(ProjectionCache::new()),
            fingerprints: Mutex::new(ProjectionCache::new()),
        }
    }
    /// The generation stops new callers before awaiting detached sibling reads.
    pub async fn settled(&self) {
        self.reader.settled().await;
    }
    pub fn pin_for_hash(&self, hash: &str) -> Option<Json> {
        self.pins
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values
            .get(hash)
            .cloned()
    }
    pub fn remember_node(
        &self,
        hash: String,
        pin: &Json,
        node: Json,
    ) -> Result<(), CommentFailure<S::Error>> {
        self.reader.remember(pin, Arc::new(node))?;
        self.pins
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remember(hash, pin.clone(), 16384);
        Ok(())
    }
    pub async fn node(
        &self,
        pin: &Json,
        task_id: &JsString,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Arc<Json>, CommentFailure<S::Error>> {
        Ok(self.reader.node(pin, task_id, cancellation).await?)
    }
    pub async fn range(
        &self,
        tree: &Json,
        task_id: &JsString,
        start: Option<usize>,
        end: Option<usize>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<Json>, CommentFailure<S::Error>> {
        Ok(self
            .reader
            .range(tree, task_id, start, end, cancellation)
            .await?)
    }
    pub fn summary(
        &self,
        _task: &Json,
        tree: &Json,
        task_id: &JsString,
    ) -> Result<Option<Json>, CommentFailure<S::Error>> {
        Ok(projection::summary_cached(&self.summaries, tree, task_id)?)
    }
    pub async fn read_tree<P: QuestionPolicies>(
        &self,
        value: &Json,
        task_id: &JsString,
        policies: &P,
    ) -> Result<Option<Json>, CommentFailure<S::Error>> {
        self.read_tree_with(value, task_id, &IvyPolicies(policies), &|e| e)
            .await
    }
    pub async fn read_tree_with<E: From<IvyError>, P: QuestionPolicies<E>>(
        &self,
        value: &Json,
        task_id: &JsString,
        policies: &P,
        failure: &impl Fn(CommentFailure<S::Error>) -> E,
    ) -> Result<Option<Json>, E> {
        let Some((tree, header)) = projection::prepare(value) else {
            return Ok(None);
        };
        let entries = self
            .range(&tree, task_id, None, None, None)
            .await
            .map_err(failure)?;
        let mut full = header;
        full.insert(
            "comments",
            Json::Array(
                entries
                    .iter()
                    .map(|e| e.required("comment").cloned())
                    .collect::<IvyResult<_>>()?,
            ),
        );
        let open = policies.open_questions(&full)?;
        let full = projection::finish(&self.summaries, full, &tree, task_id, &entries, &open)?;
        self.remember_tree(&tree, task_id, &full)?;
        Ok(Some(full))
    }
    pub async fn hydrate(
        &self,
        value: &Json,
        owner: &JsString,
    ) -> Result<Option<Json>, CommentFailure<S::Error>> {
        let Some((tree, header)) = projection::prepare(value) else {
            return Ok(None);
        };
        let entries = self.range(&tree, owner, None, None, None).await?;
        let mut full = header;
        full.insert(
            "comments",
            Json::Array(
                entries
                    .iter()
                    .map(|e| e.required("comment").cloned())
                    .collect::<IvyResult<_>>()?,
            ),
        );
        let open = ivy_sdk::open_task_questions(&full, None)?
            .iter()
            .map(|q| {
                q.required("commentId")?
                    .as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)
            })
            .collect::<IvyResult<Vec<_>>>()?;
        let full = projection::finish(&self.summaries, full, &tree, owner, &entries, &open)?;
        self.remember_tree(&tree, owner, &full)?;
        Ok(Some(full))
    }
    pub fn fingerprint(task: &Json) -> IvyResult<String> {
        let hashes = |key| -> IvyResult<Json> {
            Ok(Json::Array(
                task.required(key)?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .map(|v| hash_json(v).map(Json::from))
                    .collect::<IvyResult<_>>()?,
            ))
        };
        hash_json(&Json::object([
            ("comments", hashes("comments")?),
            ("receipts", hashes("commentDeliveries")?),
        ]))
    }
    fn tree_key(tree: &Json, owner: &JsString) -> IvyResult<String> {
        hash_json(&Json::object([
            ("taskId", Json::String(owner.clone())),
            ("root", tree.required("root")?.clone()),
            ("length", tree.required("length")?.clone()),
        ]))
    }
    pub fn remember_tree(&self, tree: &Json, owner: &JsString, task: &Json) -> IvyResult<()> {
        let hash = Self::fingerprint(task)?;
        let mut attachments = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for comment in task
            .required("comments")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            for attachment in comment
                .required("attachments")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                let pin = attachment.required("object")?;
                if seen.insert(hash_json(pin)?) {
                    attachments.push(pin.clone());
                }
            }
        }
        let attachments = stringify(&Json::Array(attachments.clone()), 64 * 1024)
            .ok()
            .map(|_| attachments);
        self.fingerprints
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remember(
                Self::tree_key(tree, owner)?,
                Fingerprint { hash, attachments },
                128,
            );
        Ok(())
    }
    pub fn unchanged_tree(&self, tree: &Json, owner: &JsString, task: &Json) -> IvyResult<bool> {
        let key = Self::tree_key(tree, owner)?;
        let hash = Self::fingerprint(task)?;
        Ok(self
            .fingerprints
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values
            .get(&key)
            .is_some_and(|v| v.hash == hash))
    }
    pub fn prepared_attachments(
        &self,
        task: &Json,
        owner: &JsString,
    ) -> IvyResult<Option<Vec<Json>>> {
        let Some(tree) = task
            .get("commentStore")
            .filter(|v| v.get("kind") == Some(&Json::from("tree")))
        else {
            return Ok(None);
        };
        if !task
            .required("comments")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .is_empty()
            || !task
                .required("commentDeliveries")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .is_empty()
        {
            return Ok(None);
        }
        let key = Self::tree_key(tree, owner)?;
        Ok(self
            .fingerprints
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values
            .get(&key)
            .and_then(|v| v.attachments.clone()))
    }
}
#[allow(async_fn_in_trait)]
pub trait CommentReads {
    type Error;
    fn summary(
        &self,
        task: &Json,
        tree: &Json,
        owner: &JsString,
    ) -> Result<Option<Json>, CommentFailure<Self::Error>>;
    async fn range(
        &self,
        tree: &Json,
        owner: &JsString,
        start: Option<usize>,
        end: Option<usize>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<Json>, CommentFailure<Self::Error>>;
    async fn read_tree_with<E: From<IvyError>, P: QuestionPolicies<E>>(
        &self,
        value: &Json,
        owner: &JsString,
        policies: &P,
        failure: &impl Fn(CommentFailure<Self::Error>) -> E,
    ) -> Result<Option<Json>, E>;
}
impl<S: CommentSource + ?Sized> CommentReads for TaskComments<S> {
    type Error = S::Error;
    fn summary(
        &self,
        task: &Json,
        tree: &Json,
        owner: &JsString,
    ) -> Result<Option<Json>, CommentFailure<Self::Error>> {
        TaskComments::summary(self, task, tree, owner)
    }
    async fn range(
        &self,
        tree: &Json,
        owner: &JsString,
        start: Option<usize>,
        end: Option<usize>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<Json>, CommentFailure<Self::Error>> {
        TaskComments::range(self, tree, owner, start, end, cancellation).await
    }
    async fn read_tree_with<E: From<IvyError>, P: QuestionPolicies<E>>(
        &self,
        value: &Json,
        owner: &JsString,
        policies: &P,
        failure: &impl Fn(CommentFailure<Self::Error>) -> E,
    ) -> Result<Option<Json>, E> {
        TaskComments::read_tree_with(self, value, owner, policies, failure).await
    }
}
