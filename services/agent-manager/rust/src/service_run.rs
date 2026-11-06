//! Shared result ownership. Effect drain and caller result retention end separately.
use ivy_contracts::{IvyError, IvyJson, IvyResult, Outcome};
use std::{
    mem::size_of,
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

const BYTES: usize = 64 * 1024 * 1024;
const SINGLE: usize = 48 * 1024 * 1024;
const CONTROL_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct Reply<T = IvyJson> {
    pub value: IvyResult<T>,
    _bytes: Option<OwnedSemaphorePermit>,
}
struct Cell<T> {
    value: Mutex<Option<Arc<Reply<T>>>>,
    _call: Arc<OwnedSemaphorePermit>,
}
#[derive(Clone)]
pub struct AgentRun<T = IvyJson> {
    cell: Arc<Cell<T>>,
    done: watch::Receiver<bool>,
}
pub(crate) struct Sender<T = IvyJson> {
    cell: Option<Arc<Cell<T>>>,
    done: watch::Sender<bool>,
}
impl<T> Sender<T> {
    pub fn pending(&self) -> bool {
        self.cell.is_some()
    }
    pub fn send(&mut self, value: Arc<Reply<T>>) {
        let cell = self.cell.take().expect("one result per run");
        *cell.value.lock().unwrap_or_else(|error| error.into_inner()) = Some(value);
        // Release the producer's value owner before notifying a sole receiver,
        // so that receiver can always move the original allocation.
        drop(cell);
        self.done.send_replace(true);
    }
}
impl<T> AgentRun<T> {
    pub(crate) fn cache_only(&self) -> bool {
        Arc::strong_count(&self.cell) == 1
    }
    pub(crate) fn channel(call: Arc<OwnedSemaphorePermit>) -> (Sender<T>, Self) {
        let cell = Arc::new(Cell {
            value: Mutex::new(None),
            _call: call,
        });
        let (done, receiver) = watch::channel(false);
        (
            Sender {
                cell: Some(cell.clone()),
                done,
            },
            Self {
                cell,
                done: receiver,
            },
        )
    }
    async fn ready(&mut self) -> IvyResult<()> {
        while !*self.done.borrow_and_update() {
            self.done
                .changed()
                .await
                .map_err(|_| IvyError::internal())?;
        }
        Ok(())
    }
    pub(crate) async fn completed(mut self) -> IvyResult<()> {
        self.ready().await
    }
    pub(crate) async fn shared(mut self) -> IvyResult<Arc<Reply<T>>> {
        self.ready().await?;
        let value = match Arc::try_unwrap(self.cell) {
            Ok(cell) => cell.value.into_inner().map_err(|_| IvyError::internal())?,
            Err(cell) => cell.value.lock().map_err(|_| IvyError::internal())?.clone(),
        };
        value.ok_or_else(IvyError::internal)
    }
    pub async fn wait(self) -> IvyResult<T>
    where
        T: Clone,
    {
        match Arc::try_unwrap(self.shared().await?) {
            Ok(reply) => reply.value,
            Err(reply) => reply.value.clone(),
        }
    }
}
pub(crate) struct Admission {
    pub call: Arc<OwnedSemaphorePermit>,
    pub control: bool,
    _input: OwnedSemaphorePermit,
}
#[derive(Clone)]
pub(crate) struct Budget {
    calls: Arc<Semaphore>,
    inputs: Arc<Semaphore>,
    replies: Arc<Semaphore>,
}
fn capacity() -> IvyError {
    IvyError::new(
        "native_capacity",
        "Agent work or retained-result capacity is occupied.",
    )
}
fn reserve_bytes(
    pool: &Arc<Semaphore>,
    size: usize,
    control: bool,
) -> IvyResult<OwnedSemaphorePermit> {
    if size > SINGLE {
        return Err(capacity());
    }
    let bytes = pool
        .clone()
        .try_acquire_many_owned(size as u32)
        .map_err(|_| capacity())?;
    if !control && pool.available_permits() < CONTROL_BYTES {
        return Err(capacity());
    }
    Ok(bytes)
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            calls: Arc::new(Semaphore::new(64)),
            inputs: Arc::new(Semaphore::new(BYTES)),
            replies: Arc::new(Semaphore::new(BYTES)),
        }
    }
}
impl Budget {
    pub fn reserve(&self, control: bool, input: usize) -> IvyResult<Admission> {
        let call = self
            .calls
            .clone()
            .try_acquire_owned()
            .map_err(|_| capacity())?;
        if !control && self.calls.available_permits() < 8 {
            return Err(capacity());
        }
        let input = reserve_bytes(&self.inputs, input.saturating_add(1024), control)?;
        Ok(Admission {
            call: Arc::new(call),
            control,
            _input: input,
        })
    }
    pub fn retain(&self, value: IvyResult<IvyJson>, control: bool) -> IvyResult<Arc<Reply>> {
        let size = value.as_ref().map_or(0, IvyJson::retained_bytes);
        self.retain_sized(value, control, size)
    }
    fn retain_sized<T>(
        &self,
        value: IvyResult<T>,
        control: bool,
        size: usize,
    ) -> IvyResult<Arc<Reply<T>>> {
        // The 64 admitted call guards bound small original local errors even
        // when large reply payloads have occupied all retained-result bytes.
        let bytes = if value
            .as_ref()
            .is_err_and(|error| error.retained_bytes() <= 4096)
        {
            None
        } else {
            let size = match &value {
                Ok(_) => size,
                Err(error) => error.retained_bytes(),
            }
            .saturating_add(size_of::<Reply<T>>())
            .saturating_add(128);
            Some(reserve_bytes(&self.replies, size, control)?)
        };
        Ok(Arc::new(Reply {
            value,
            _bytes: bytes,
        }))
    }
    pub fn finish(&self, value: IvyResult<IvyJson>, control: bool) -> Arc<Reply> {
        self.retain(value, control)
            .unwrap_or_else(|_| unavailable())
    }
    pub fn finish_unit(&self, value: IvyResult<()>, control: bool) -> Arc<Reply<()>> {
        self.retain_sized(value, control, 0)
            .unwrap_or_else(|_| unavailable())
    }
}
fn unavailable<T>() -> Arc<Reply<T>> {
    Arc::new(Reply {
        value: Err(IvyError::new("agent_reply_unavailable",
            "Accepted work has settled but its result could not be retained; reconcile its original state before another action.")
            .with_outcome(Outcome::Unknown)),
        _bytes: None,
    })
}
