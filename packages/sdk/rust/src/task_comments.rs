//! Immutable comment-tree reads. Publication hashes and summaries stay with the caller.
use crate::{
    IvyError, IvyJson, JsString,
    rpc::{RequestOptions, RpcClient},
    tools::truthy,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use ivy_contracts::json::{js_string, number_from_string, stringify, try_js_string};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

pub const COMMENT_NODE_KEY: &str = "task-board/comment-node";
const SNAPSHOT: &str = "The conversation snapshot belongs to another ticket.";
const RANGE: &str = "The conversation range is invalid.";
const INDEX: &str = "The conversation index is invalid.";
const SEQUENCE: &str = "The conversation sequence is invalid.";
const INCOMPLETE: &str = "The conversation index is incomplete.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentException {
    pub kind: &'static str,
    pub message: JsString,
}
#[derive(Debug)]
pub enum CommentError<E> {
    Source(E),
    JavaScript(CommentException),
}
pub type CommentReadFuture<'a, E> = Pin<Box<dyn Future<Output = Result<IvyJson, E>> + Send + 'a>>;
pub type NodeObserver<E> = Arc<dyn Fn(&IvyJson, &IvyJson) -> Result<(), E> + Send + Sync + 'static>;

pub trait CommentSource: Send + Sync + 'static {
    type Error: Send + 'static;
    fn read<'a>(
        &'a self,
        pin: IvyJson,
        cancellation: Option<CancellationToken>,
    ) -> CommentReadFuture<'a, Self::Error>;
}

pub struct RpcCommentSource {
    client: Arc<dyn RpcClient>,
}
impl RpcCommentSource {
    pub fn new(client: Arc<dyn RpcClient>) -> Self {
        Self { client }
    }
}
impl CommentSource for RpcCommentSource {
    type Error = IvyError;
    fn read<'a>(
        &'a self,
        pin: IvyJson,
        cancellation: Option<CancellationToken>,
    ) -> CommentReadFuture<'a, IvyError> {
        Box::pin(async move {
            let mut options = RequestOptions::default();
            if let Some(cancellation) = cancellation {
                options.cancellation = cancellation;
            }
            self.client.request("objects.read", pin, options).await
        })
    }
}

#[derive(Default)]
struct Cache {
    nodes: HashMap<JsString, (Arc<IvyJson>, usize)>,
    order: VecDeque<JsString>,
    bytes: usize,
}
pub struct CommentReader<S: CommentSource + ?Sized> {
    source: Arc<S>,
    executor: Handle,
    cache: Arc<Mutex<Cache>>,
    maximum_bytes: usize,
    observer: Option<NodeObserver<S::Error>>,
}
impl<S: CommentSource + ?Sized> Clone for CommentReader<S> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            executor: self.executor.clone(),
            cache: self.cache.clone(),
            maximum_bytes: self.maximum_bytes,
            observer: self.observer.clone(),
        }
    }
}
impl<S: CommentSource + ?Sized> CommentReader<S> {
    /// The supplied executor must outlive initiated reads, including detached siblings.
    pub fn new(source: Arc<S>, executor: Handle) -> Self {
        Self::with_maximum_bytes(source, executor, 16 * 1024 * 1024)
    }
    pub fn with_maximum_bytes(source: Arc<S>, executor: Handle, maximum_bytes: usize) -> Self {
        Self {
            source,
            executor,
            maximum_bytes,
            cache: Arc::new(Mutex::new(Cache::default())),
            observer: None,
        }
    }
    pub fn with_node_observer(mut self, observer: NodeObserver<S::Error>) -> Self {
        self.observer = Some(observer);
        self
    }
    pub fn remember(
        &self,
        pin: &IvyJson,
        node: Arc<IvyJson>,
    ) -> Result<(), CommentError<S::Error>> {
        let key = pin_key(pin)?;
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.nodes.contains_key(&key) {
            return Ok(());
        }
        // No JSON validation or wire-size limit is imposed by JSON.stringify here.
        let json = stringify(&node, usize::MAX).map_err(|error| {
            CommentError::JavaScript(CommentException {
                kind: "Error",
                message: error.message_js(),
            })
        })?;
        let bytes = json.encode_utf16().count().saturating_mul(2);
        while cache.bytes.saturating_add(bytes) > self.maximum_bytes && !cache.order.is_empty() {
            if let Some(oldest) = cache.order.pop_front() {
                if let Some((_, charged)) = cache.nodes.remove(&oldest) {
                    cache.bytes -= charged;
                }
            }
        }
        if bytes <= self.maximum_bytes {
            cache.order.push_back(key.clone());
            cache.nodes.insert(key, (node, bytes));
            cache.bytes += bytes;
        }
        Ok(())
    }
    pub async fn node(
        &self,
        pin: &IvyJson,
        task_id: &JsString,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Arc<IvyJson>, CommentError<S::Error>> {
        let key = pin_key(pin)?;
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .nodes
            .get(&key)
            .map(|(node, _)| node.clone())
            .filter(|node| truthy(Some(node)));
        let node = if let Some(node) = cached {
            node
        } else {
            let saved = self
                .source
                .read(pin.clone(), cancellation.cloned())
                .await
                .map_err(CommentError::Source)?;
            let object = property(Some(&saved), "object")?;
            if !is_text(
                property(object.as_deref(), "contractKey")?.as_deref(),
                COMMENT_NODE_KEY,
            ) {
                return Err(exception(SNAPSHOT));
            }
            let revision = property(Some(&saved), "revision")?;
            if !is_text(
                property(revision.as_deref(), "contractVersion")?.as_deref(),
                "1.0.0",
            ) || property(object.as_deref(), "parentId")?
                .as_deref()
                .and_then(IvyJson::as_js_string)
                != Some(task_id)
                || property(object.as_deref(), "ownerObjectId")?
                    .as_deref()
                    .and_then(IvyJson::as_js_string)
                    != Some(task_id)
            {
                return Err(exception(SNAPSHOT));
            }
            let content = property(Some(&saved), "content")?;
            if !is_text(property(content.as_deref(), "encoding")?.as_deref(), "json")
                || !strict_equal(
                    property(revision.as_deref(), "revision")?.as_deref(),
                    property(Some(pin), "revision")?.as_deref(),
                )
            {
                return Err(exception(SNAPSHOT));
            }
            let value = property(content.as_deref(), "value")?;
            if property(value.as_deref(), "taskId")?
                .as_deref()
                .and_then(IvyJson::as_js_string)
                != Some(task_id)
            {
                return Err(exception(SNAPSHOT));
            }
            // taskId access above has already rejected null/undefined.
            let node = Arc::new(value.map(Cow::into_owned).unwrap_or(IvyJson::Null));
            self.remember(pin, node.clone())?;
            node
        };
        if property(Some(&node), "taskId")?
            .as_deref()
            .and_then(IvyJson::as_js_string)
            != Some(task_id)
        {
            return Err(exception(SNAPSHOT));
        }
        if let Some(observer) = &self.observer {
            observer(pin, &node).map_err(CommentError::Source)?;
        }
        Ok(node)
    }

    /// Half-open positions cover validated native callers. Negative/malformed JS arguments
    /// are outside the usize API; stored JSON is visited in the original validation order.
    pub async fn range(
        &self,
        tree: &IvyJson,
        task_id: &JsString,
        start: Option<usize>,
        end: Option<usize>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<IvyJson>, CommentError<S::Error>> {
        let length = property(Some(tree), "length")?;
        let start = IvyJson::Number(start.unwrap_or(0) as f64);
        let end = end
            .map(|end| IvyJson::Number(end as f64))
            .or_else(|| length.as_deref().cloned());
        if less(length.as_deref(), end.as_ref())? || less(end.as_ref(), Some(&start))? {
            return Err(exception(RANGE));
        }
        let root = property(Some(tree), "root")?;
        let root_length = if root.as_deref().is_none_or(IvyJson::is_null) {
            None
        } else {
            property(root.as_deref(), "length")?
        };
        let zero = IvyJson::Number(0.0);
        let root_length = root_length
            .as_deref()
            .filter(|v| !v.is_null())
            .unwrap_or(&zero);
        if !strict_equal(Some(root_length), length.as_deref()) {
            return Err(exception(RANGE));
        }
        let mut pending = VecDeque::new();
        if truthy(root.as_deref()) {
            enqueue(
                &mut pending,
                root.as_deref().cloned().unwrap_or(IvyJson::Null),
                zero,
                0,
                &start,
                end.as_ref(),
            )?;
        }
        let mut result = Vec::new();
        while !pending.is_empty() {
            let mut ready = Vec::new();
            let mut suspended = Vec::new();
            // Poll every initiated read once before observing a refusal, as Promise.all's map does.
            for position in 0..pending.len().min(8) {
                let item = pending.pop_front().unwrap();
                let reader = self.clone();
                let task_id = task_id.clone();
                let signal = cancellation.cloned();
                let mut future = Box::pin(async move {
                    let outcome = async {
                        if item.depth > 12
                            || less(
                                property(Some(&item.index), "length")?.as_deref(),
                                Some(&IvyJson::Number(1.0)),
                            )?
                        {
                            return Err(exception(INDEX));
                        }
                        let pin = property(Some(&item.index), "object")?;
                        // A missing pin must fail at objectId access, before any request.
                        if pin.is_none() {
                            return Err(type_error(
                                "Cannot read properties of undefined (reading 'objectId')",
                            ));
                        }
                        reader
                            .node(pin.as_deref().unwrap(), &task_id, signal.as_ref())
                            .await
                    }
                    .await;
                    (position, item, outcome)
                });
                match poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await {
                    Poll::Ready(value) => ready.push(value),
                    Poll::Pending => suspended.push(future),
                }
            }
            let mut running = FuturesUnordered::new();
            for future in suspended {
                running.push(self.executor.spawn(future));
            }
            let count = ready.len() + running.len();
            let mut ordered: Vec<Option<(Item, Arc<IvyJson>)>> = (0..count).map(|_| None).collect();
            for (position, item, value) in ready {
                ordered[position] = Some((item, value?));
            }
            while let Some(joined) = running.next().await {
                let (position, item, value) = joined.map_err(|error| {
                    CommentError::JavaScript(CommentException {
                        kind: "Error",
                        message: error.to_string().into(),
                    })
                })?;
                ordered[position] = Some((item, value?));
            }
            // Dropping JoinHandles above detaches; it never aborts initiated sibling reads.
            for completed in ordered.into_iter().flatten() {
                let (item, node) = completed;
                let index_length = property(Some(&item.index), "length")?;
                if is_text(property(Some(&node), "kind")?.as_deref(), "comment") {
                    if !strict_equal(index_length.as_deref(), Some(&IvyJson::Number(1.0))) {
                        return Err(exception(SEQUENCE));
                    }
                    let entry = property(Some(&node), "entry")?;
                    let comment = property(entry.as_deref(), "comment")?;
                    if !strict_equal(
                        property(comment.as_deref(), "sequence")?.as_deref(),
                        Some(&add(Some(&item.offset), Some(&IvyJson::Number(1.0)))?),
                    ) {
                        return Err(exception(SEQUENCE));
                    }
                    let position = number(Some(&item.offset))? - number(Some(&start))?;
                    if position >= 0.0 && position.fract() == 0.0 && position < u32::MAX as f64 {
                        let position = position as usize;
                        if result.len() <= position {
                            result.resize_with(position + 1, || IvyJson::Null);
                        }
                        result[position] = entry.map(Cow::into_owned).unwrap_or(IvyJson::Null);
                    }
                } else {
                    let children = property(Some(&node), "children")?;
                    let size = property(children.as_deref(), "length")?;
                    if !truthy(size.as_deref()) {
                        return Err(exception(INCOMPLETE));
                    }
                    let Some(children) = children.as_deref().and_then(IvyJson::as_array) else {
                        return Err(type_error("node.children.reduce is not a function"));
                    };
                    let mut sum = IvyJson::Number(0.0);
                    for child in children {
                        sum = add(Some(&sum), property(Some(child), "length")?.as_deref())?;
                    }
                    if !strict_equal(Some(&sum), index_length.as_deref()) {
                        return Err(exception(INCOMPLETE));
                    }
                    let mut offset = item.offset;
                    for child in children {
                        enqueue(
                            &mut pending,
                            child.clone(),
                            offset.clone(),
                            item.depth + 1,
                            &start,
                            end.as_ref(),
                        )?;
                        offset = add(Some(&offset), property(Some(child), "length")?.as_deref())?;
                    }
                }
            }
        }
        Ok(result)
    }
}

struct Item {
    index: IvyJson,
    offset: IvyJson,
    depth: usize,
}
fn enqueue<E>(
    pending: &mut VecDeque<Item>,
    index: IvyJson,
    offset: IvyJson,
    depth: usize,
    start: &IvyJson,
    end: Option<&IvyJson>,
) -> Result<(), CommentError<E>> {
    if less(Some(&offset), end)?
        && less(
            Some(start),
            Some(&add(
                Some(&offset),
                property(Some(&index), "length")?.as_deref(),
            )?),
        )?
    {
        pending.push_back(Item {
            index,
            offset,
            depth,
        });
    }
    Ok(())
}
fn exception<E>(message: &str) -> CommentError<E> {
    CommentError::JavaScript(CommentException {
        kind: "Error",
        message: message.into(),
    })
}
fn type_error<E>(message: &str) -> CommentError<E> {
    CommentError::JavaScript(CommentException {
        kind: "TypeError",
        message: message.into(),
    })
}
fn property<'a, E>(
    value: Option<&'a IvyJson>,
    name: &str,
) -> Result<Option<Cow<'a, IvyJson>>, CommentError<E>> {
    match value {
        None | Some(IvyJson::Null) => Err(type_error(&format!(
            "Cannot read properties of {} (reading '{name}')",
            if value.is_none() { "undefined" } else { "null" }
        ))),
        Some(IvyJson::Array(values)) if name == "length" => {
            Ok(Some(Cow::Owned(IvyJson::Number(values.len() as f64))))
        }
        Some(IvyJson::String(value)) if name == "length" => {
            Ok(Some(Cow::Owned(IvyJson::Number(value.utf16_len() as f64))))
        }
        Some(value) => Ok(value.get(name).map(Cow::Borrowed)),
    }
}
fn is_text(value: Option<&IvyJson>, text: &str) -> bool {
    value.and_then(IvyJson::as_js_string) == Some(&JsString::from(text))
}
fn strict_equal(a: Option<&IvyJson>, b: Option<&IvyJson>) -> bool {
    match (a, b) {
        (None, None) | (Some(IvyJson::Null), Some(IvyJson::Null)) => true,
        (Some(IvyJson::Number(a)), Some(IvyJson::Number(b))) => a == b,
        (Some(IvyJson::String(a)), Some(IvyJson::String(b))) => a == b,
        (Some(IvyJson::Bool(a)), Some(IvyJson::Bool(b))) => a == b,
        (Some(a), Some(b)) if a.as_object().is_some() || a.as_array().is_some() => {
            std::ptr::eq(a, b)
        }
        _ => false,
    }
}
fn primitive<E>(value: Option<&IvyJson>) -> Result<Option<Cow<'_, IvyJson>>, CommentError<E>> {
    match value {
        Some(value @ (IvyJson::Array(_) | IvyJson::Object(_))) => {
            Ok(Some(Cow::Owned(IvyJson::String(
                try_js_string(value)
                    .map_err(|_| type_error("Cannot convert object to primitive value"))?,
            ))))
        }
        _ => Ok(value.map(Cow::Borrowed)),
    }
}
fn text(value: Option<&IvyJson>) -> JsString {
    value.map(js_string).unwrap_or_else(|| "undefined".into())
}
fn number<E>(value: Option<&IvyJson>) -> Result<f64, CommentError<E>> {
    let value = primitive(value)?;
    Ok(match value.as_deref() {
        None => f64::NAN,
        Some(IvyJson::Null) => 0.0,
        Some(IvyJson::Bool(v)) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        Some(IvyJson::Number(v)) => *v,
        Some(IvyJson::String(v)) => number_from_string(v),
        _ => f64::NAN,
    })
}
fn less<E>(a: Option<&IvyJson>, b: Option<&IvyJson>) -> Result<bool, CommentError<E>> {
    let (a, b) = (primitive(a)?, primitive(b)?);
    match (a.as_deref(), b.as_deref()) {
        (Some(IvyJson::String(a)), Some(IvyJson::String(b))) => Ok(a < b),
        (a, b) => Ok(number(a)? < number(b)?),
    }
}
fn add<E>(a: Option<&IvyJson>, b: Option<&IvyJson>) -> Result<IvyJson, CommentError<E>> {
    let (a, b) = (primitive(a)?, primitive(b)?);
    if matches!(a.as_deref(), Some(IvyJson::String(_)))
        || matches!(b.as_deref(), Some(IvyJson::String(_)))
    {
        let mut units = text(a.as_deref()).0;
        units.extend(text(b.as_deref()).0);
        Ok(IvyJson::String(JsString(units)))
    } else {
        Ok(IvyJson::Number(
            number(a.as_deref())? + number(b.as_deref())?,
        ))
    }
}
fn pin_key<E>(pin: &IvyJson) -> Result<JsString, CommentError<E>> {
    let id = property(Some(pin), "objectId")?;
    let prefix = add(id.as_deref(), Some(&IvyJson::from("@")))?;
    let revision = property(Some(pin), "revision")?;
    Ok(text(Some(&add(Some(&prefix), revision.as_deref())?)))
}
