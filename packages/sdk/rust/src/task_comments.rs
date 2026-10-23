//! Immutable, versioned comment-tree reads. Publication remains with the caller.
use crate::{
    IvyError, IvyJson, JsString,
    rpc::{RequestOptions, RpcClient},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use ivy_contracts::json::stringify;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::runtime::Handle;
pub use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

mod policy;
use policy::{NodePin, RangeState};

pub const COMMENT_NODE_KEY: &str = "task-board/comment-node";
const SNAPSHOT: &str = "The conversation snapshot belongs to another ticket.";
const RANGE: &str = "The conversation range is invalid.";
const INDEX: &str = "The conversation index is invalid.";
const SEQUENCE: &str = "The conversation sequence is invalid.";
const INCOMPLETE: &str = "The conversation index is incomplete.";

#[derive(Debug)]
pub enum CommentError<E> {
    Source(E),
    Invalid(IvyError),
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

type Cache = policy::Cache<Arc<IvyJson>>;
pub struct CommentReader<S: CommentSource + ?Sized> {
    source: Arc<S>,
    executor: Handle,
    cache: Arc<Mutex<Cache>>,
    reads: TaskTracker,
    maximum_bytes: usize,
    observer: Option<NodeObserver<S::Error>>,
}
impl<S: CommentSource + ?Sized> Clone for CommentReader<S> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            executor: self.executor.clone(),
            cache: self.cache.clone(),
            reads: self.reads.clone(),
            maximum_bytes: self.maximum_bytes,
            observer: self.observer.clone(),
        }
    }
}
impl<S: CommentSource + ?Sized> CommentReader<S> {
    pub fn new(source: Arc<S>, executor: Handle) -> Self {
        Self::with_maximum_bytes(source, executor, 16 * 1024 * 1024)
    }
    /// Cache capacity is charged by the UTF-8 size of each encoded JSON node.
    pub fn with_maximum_bytes(source: Arc<S>, executor: Handle, maximum_bytes: usize) -> Self {
        Self {
            source,
            executor,
            maximum_bytes,
            cache: Arc::new(Mutex::new(Cache::default())),
            reads: TaskTracker::new(),
            observer: None,
        }
    }
    pub fn with_node_observer(mut self, observer: NodeObserver<S::Error>) -> Self {
        self.observer = Some(observer);
        self
    }
    /// After stopping new calls, wait for all admitted reads across clones,
    /// including sibling work detached by a failed or dropped range caller.
    /// This does not cancel reads or change their errors or cache behavior.
    pub async fn settled(&self) {
        self.reads.close();
        self.reads.wait().await;
    }
    pub fn remember(
        &self,
        pin: &IvyJson,
        node: Arc<IvyJson>,
    ) -> Result<(), CommentError<S::Error>> {
        let key = NodePin::read(pin).map_err(CommentError::Invalid)?;
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if !cache.nodes.contains_key(&key) {
            let bytes = stringify(&node, usize::MAX)
                .map_err(CommentError::Invalid)?
                .len();
            cache.insert(key, node, bytes, self.maximum_bytes);
        }
        Ok(())
    }
    pub async fn node(
        &self,
        pin: &IvyJson,
        task_id: &JsString,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Arc<IvyJson>, CommentError<S::Error>> {
        let _read = self.reads.token();
        let key = NodePin::read(pin).map_err(CommentError::Invalid)?;
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .nodes
            .get(&key)
            .map(|(node, _)| node.clone());
        let node = if let Some(node) = cached {
            node
        } else {
            let saved = self
                .source
                .read(pin.clone(), cancellation.cloned())
                .await
                .map_err(CommentError::Source)?;
            let node = Arc::new(
                policy::snapshot(&saved, &key, task_id)
                    .map_err(CommentError::Invalid)?
                    .clone(),
            );
            self.remember(pin, node.clone())?;
            node
        };
        policy::node_owner(&node, task_id).map_err(CommentError::Invalid)?;
        if let Some(observer) = &self.observer {
            observer(pin, &node).map_err(CommentError::Source)?;
        }
        Ok(node)
    }
    /// Read a half-open range in bounded concurrent batches. In-flight immutable
    /// reads can finish and populate the shared cache after a sibling fails.
    pub async fn range(
        &self,
        tree: &IvyJson,
        task_id: &JsString,
        start: Option<usize>,
        end: Option<usize>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Vec<IvyJson>, CommentError<S::Error>> {
        let _read = self.reads.token();
        let mut state = RangeState::new(tree, start, end).map_err(CommentError::Invalid)?;
        loop {
            let batch = state.batch();
            if batch.is_empty() {
                break;
            }
            let mut running = FuturesUnordered::new();
            for (position, item) in batch.into_iter().enumerate() {
                let reader = self.clone();
                let task_id = task_id.clone();
                let cancellation = cancellation.cloned();
                running.push(self.reads.spawn_on(
                    async move {
                        let result = match RangeState::pin(&item) {
                            Ok(pin) => reader.node(pin, &task_id, cancellation.as_ref()).await,
                            Err(error) => Err(CommentError::Invalid(error)),
                        };
                        (position, item, result)
                    },
                    &self.executor,
                ));
            }
            let mut ordered: Vec<Option<_>> = (0..running.len()).map(|_| None).collect();
            while let Some(joined) = running.next().await {
                let (position, item, node) = joined
                    .map_err(|error| CommentError::Invalid(policy::invalid(&error.to_string())))?;
                ordered[position] = Some((item, node?));
            }
            for (item, node) in ordered.into_iter().flatten() {
                state.complete(item, &node).map_err(CommentError::Invalid)?;
            }
        }
        state.finish().map_err(CommentError::Invalid)
    }
}
