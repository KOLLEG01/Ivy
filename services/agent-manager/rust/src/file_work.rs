//! One coalesced application for managed instructions, MCP and skills files.
use ivy_contracts::{IvyError, IvyResult};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

#[derive(Clone, Default)]
pub struct FileRun(Option<watch::Receiver<bool>>);
impl FileRun {
    pub async fn wait(mut self) {
        if let Some(receiver) = self.0.as_mut() {
            while !*receiver.borrow_and_update() {
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        }
    }
}
struct State<S> {
    status: S,
    running: Option<FileRun>,
}
pub(crate) struct FileWork<S>(Arc<Mutex<State<S>>>);
struct Completion<S, T> {
    owner: Arc<Mutex<State<S>>>,
    sender: Option<watch::Sender<bool>>,
    update: fn(&mut S, IvyResult<T>),
}
impl<S, T> Completion<S, T> {
    fn finish(&mut self, result: IvyResult<T>) {
        let mut state = self.owner.lock().unwrap_or_else(|e| e.into_inner());
        (self.update)(&mut state.status, result);
        state.running = None;
        if let Some(sender) = self.sender.take() {
            sender.send_replace(true);
        }
    }
}
impl<S, T> Drop for Completion<S, T> {
    fn drop(&mut self) {
        if self.sender.is_some() {
            self.finish(Err(IvyError::internal()));
        }
    }
}
impl<S: Clone + Send + 'static> FileWork<S> {
    pub(crate) fn new(status: S) -> Self {
        Self(Arc::new(Mutex::new(State {
            status,
            running: None,
        })))
    }
    pub(crate) fn status(&self) -> S {
        self.read(Clone::clone)
    }
    pub(crate) fn read<T>(&self, read: impl FnOnce(&S) -> T) -> T {
        read(&self.0.lock().unwrap_or_else(|e| e.into_inner()).status)
    }
    pub(crate) fn progress(&self, update: impl FnOnce(&mut S)) {
        update(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()).status);
    }
    pub(crate) fn start<T: Send + 'static, F: Future<Output = IvyResult<T>> + Send + 'static>(
        &self,
        update: fn(&mut S, IvyResult<T>),
        create: impl FnOnce() -> F,
    ) -> FileRun {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = &state.running {
            return running.clone();
        }
        let (sender, receiver) = watch::channel(false);
        let run = FileRun(Some(receiver));
        state.running = Some(run.clone());
        drop(state);
        let mut completion = Completion {
            owner: self.0.clone(),
            sender: Some(sender),
            update,
        };
        // Admission precedes the factory; a joining caller cannot replace the
        // original connection/source. Factory and application never hold the lock.
        let work = create();
        tokio::spawn(async move {
            completion.finish(work.await);
        });
        run
    }
    /// A snapshot, not permanent retirement. Startup/reconnect can apply again.
    pub(crate) fn close(&self) -> FileRun {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .running
            .clone()
            .unwrap_or_default()
    }
}
