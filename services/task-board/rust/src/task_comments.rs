//! Service-owned conversation projections over the actual Shared tree reader.
//! Failures stay lossless until the caller's corresponding original catch site.
use crate::{
    coordination::next_steps::{IvyPolicies, NextStepPolicies},
    engine_read::comment_reads::comment_summary,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, JsString,
    hash::hash_json,
    json::{stringify, try_js_string},
};
use ivy_sdk::task_comments::CancellationToken;
use ivy_sdk::task_comments::{
    CommentError, CommentException, CommentReadFuture, CommentReader, CommentSource,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::runtime::Handle;

/// No conversion to IvyError is supplied: source identity, ordinary JavaScript
/// exceptions and typed owner failures have different original catch behavior.
#[derive(Debug)]
pub enum CommentFailure<E> {
    Source(E),
    Ivy(IvyError),
    JavaScript(CommentException),
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
            CommentError::JavaScript(error) => Self::JavaScript(error),
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
struct ProjectionCache {
    values: HashMap<String, Json>,
    order: VecDeque<String>,
}
impl ProjectionCache {
    fn new() -> Self {
        Self {
            values: HashMap::new(),
            order: VecDeque::new(),
        }
    }
    fn remember(&mut self, key: String, value: Json, maximum: usize) {
        // Match Map.delete(oldest) before Map.set, including replacement at
        // capacity. Updating a surviving entry never refreshes its FIFO order.
        if self.values.len() >= maximum {
            if let Some(oldest) = self.order.pop_front() {
                self.values.remove(&oldest);
            }
        }
        if !self.values.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        self.values.insert(key, value);
    }
}
pub struct TaskComments<S: CommentSource + ?Sized> {
    reader: CommentReader<ReadSource<S>>,
    pins: Arc<Mutex<ProjectionCache>>,
    summaries: Mutex<ProjectionCache>,
}
impl<S: CommentSource + ?Sized> TaskComments<S> {
    pub fn new(source: Arc<S>, executor: Handle) -> Self {
        let pins = Arc::new(Mutex::new(ProjectionCache::new()));
        let observed = pins.clone();
        let reader = CommentReader::new(Arc::new(ReadSource(source)), executor).with_node_observer(
            Arc::new(move |pin, node| {
                let hash = hash_json(node).map_err(CommentFailure::Ivy)?;
                observed.lock().unwrap_or_else(|p| p.into_inner()).remember(
                    hash,
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
        }
    }
    /// Publication may reuse only pins observed after successful node checks.
    pub fn pin_for_hash(&self, hash: &str) -> Option<Json> {
        self.pins
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values
            .get(hash)
            .cloned()
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
        task: &Json,
        tree: &Json,
        task_id: &JsString,
    ) -> Result<Option<Json>, CommentFailure<S::Error>> {
        if let Some(summary) = property(Some(tree), "summary")?.filter(|s| !s.is_null()) {
            return Ok(Some(summary.clone()));
        }
        let key = summary_key(task, tree, task_id)?;
        Ok(self
            .summaries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values
            .get(&key)
            .cloned())
    }
    /// `None` selects the separately qualified inline/retained-prefix path.
    /// This does not supply a full persistent Store or a record-upgrade fallback.
    pub async fn read_tree<P: NextStepPolicies>(
        &self,
        value: &Json,
        task_id: &JsString,
        policies: &P,
    ) -> Result<Option<Json>, CommentFailure<S::Error>> {
        self.read_tree_with(value, task_id, &IvyPolicies(policies), &|error| error)
            .await
    }
    /// The same hydration/summary core with an explicit source-error binding.
    /// This leaves raw policies and failures intact through owner read ports.
    pub async fn read_tree_with<E: From<IvyError>, P: NextStepPolicies<E>>(
        &self,
        value: &Json,
        task_id: &JsString,
        policies: &P,
        failure: &impl Fn(CommentFailure<S::Error>) -> E,
    ) -> Result<Option<Json>, E> {
        let Some(tree) = property::<S::Error>(Some(value), "commentStore")
            .map_err(failure)?
            .filter(|v| super::truthy(v))
        else {
            return Ok(None);
        };
        if tree.get("kind").is_none() {
            return Ok(None);
        }
        let entries = self
            .range(tree, task_id, None, None, None)
            .await
            .map_err(failure)?;
        let mut comments = Vec::new();
        let mut receipts = Vec::new();
        for entry in entries {
            comments.push(
                property::<S::Error>(Some(&entry), "comment")
                    .map_err(failure)?
                    .cloned()
                    .unwrap_or(Json::Null),
            );
            if let Some(receipt) = property::<S::Error>(Some(&entry), "receipt")
                .map_err(failure)?
                .filter(|v| super::truthy(v))
            {
                receipts.push(receipt.clone());
            }
        }
        let mut full = super::spread(value);
        full.remove("commentStore");
        full.insert("comments", Json::Array(comments));
        full.insert("commentDeliveries", Json::Array(receipts));
        if !tree.get("summary").is_some_and(super::truthy) {
            let summary = comment_summary(policies, &full, task_id)?;
            if stringify(&summary, usize::MAX)?.len() <= 64 * 1024 {
                let key = summary_key::<S::Error>(&full, tree, task_id).map_err(failure)?;
                self.summaries
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remember(key, summary, 128);
            }
        }
        Ok(Some(full))
    }
}
fn type_error<E>(message: impl Into<JsString>) -> CommentFailure<E> {
    CommentFailure::JavaScript(CommentException {
        kind: "TypeError",
        message: message.into(),
    })
}
fn property<'a, E>(
    value: Option<&'a Json>,
    key: &str,
) -> Result<Option<&'a Json>, CommentFailure<E>> {
    match value {
        None | Some(Json::Null) => Err(type_error(format!(
            "Cannot read properties of {} (reading '{key}')",
            if value.is_none() { "undefined" } else { "null" }
        ))),
        Some(value) => Ok(value.get(key)),
    }
}
fn flat_map<'a, E>(
    value: Option<&'a Json>,
    expression: &str,
) -> Result<&'a [Json], CommentFailure<E>> {
    property(value, "flatMap")?;
    value
        .and_then(Json::as_array)
        .ok_or_else(|| type_error(format!("{expression}.flatMap is not a function")))
}
fn summary_key<E>(
    task: &Json,
    tree: &Json,
    task_id: &JsString,
) -> Result<String, CommentFailure<E>> {
    let mut ids = Vec::new();
    for step in flat_map(property(Some(task), "nextSteps")?, "task.nextSteps")? {
        for condition in flat_map(property(Some(step), "requires")?, "step.requires")? {
            let kind = property(Some(condition), "kind")?;
            if kind == Some(&Json::from("question")) || kind == Some(&Json::from("user")) {
                let id = property(Some(condition), "questionId")?.ok_or_else(|| {
                    IvyError::new("invalid_arguments", "Value is outside the JSON domain.")
                })?;
                if !ids
                    .iter()
                    .any(|prior| super::map_key_equal(Some(*prior), Some(id)))
                {
                    ids.push(id);
                }
            }
        }
    }
    let mut sorted = ids
        .into_iter()
        .map(|id| {
            try_js_string(id)
                .map(|text| (text, id.clone()))
                .map_err(|_| type_error("Cannot convert object to primitive value"))
        })
        .collect::<Result<Vec<_>, CommentFailure<E>>>()?;
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let root = property(Some(tree), "root")?
        .ok_or_else(|| IvyError::new("invalid_arguments", "Value is outside the JSON domain."))?;
    Ok(hash_json(&Json::object([
        ("taskId", Json::String(task_id.clone())),
        ("root", root.clone()),
        (
            "questions",
            Json::Array(sorted.into_iter().map(|(_, id)| id).collect()),
        ),
    ]))?)
}
