//! Storage request admission, deadlines and generation fencing.
//!
//! The process adapter supplies monotonic milliseconds and delivers returned
//! settlements once. A caller timeout does not remove posted work. A replacement
//! can start only after the adapter confirms that the previous owner was reaped.
use ivy_contracts::{IvyError, IvyJson, IvyResult, Outcome};
use std::collections::BTreeMap;

pub const MAX_PENDING: usize = 512;
pub const MAX_QUEUED_BYTES: u64 = 64 * 1024 * 1024;
pub const STARTUP_TIMEOUT_MS: u64 = 120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Generation(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequestId {
    pub generation: Generation,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Stopped,
    Starting(Generation),
    Ready(Generation),
    Retiring(Generation),
    /// A failed stop is not permission to open the database with another writer.
    StopUnconfirmed(Generation),
}

#[derive(Clone, Copy, Debug)]
pub struct RequestOptions {
    pub timeout_ms: u64,
    pub terminate_on_timeout: bool,
    pub reserved_bytes: u64,
    pub backup: bool,
}
impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            timeout_ms: 30_000,
            terminate_on_timeout: true,
            reserved_bytes: 4096,
            backup: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settlement {
    pub id: RequestId,
    pub result: IvyResult<IvyJson>,
}

#[derive(Default, Debug)]
pub struct Effects {
    pub settlements: Vec<Settlement>,
    pub retire: Option<Generation>,
    pub failure: Option<IvyError>,
}

struct Pending {
    options: RequestOptions,
    bytes: u64,
    // None means the timer fired. The retained payload still occupies its slot.
    deadline: Option<u64>,
    started: bool,
    settled: bool,
}

pub struct WorkerClient {
    lifecycle: Lifecycle,
    generation: u64,
    sequence: u64,
    startup_deadline: Option<u64>,
    queued_bytes: u64,
    pending: BTreeMap<RequestId, Pending>,
}
impl Default for WorkerClient {
    fn default() -> Self {
        Self {
            lifecycle: Lifecycle::Stopped,
            generation: 0,
            sequence: 0,
            startup_deadline: None,
            queued_bytes: 0,
            pending: BTreeMap::new(),
        }
    }
}

fn unknown(code: &str, message: &str) -> IvyError {
    IvyError::new(code, message).with_outcome(Outcome::Unknown)
}
fn not_ready() -> IvyError {
    IvyError::new("service_unavailable", "Hive storage is not ready.")
}

impl WorkerClient {
    pub fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }
    pub fn ready(&self) -> bool {
        matches!(self.lifecycle, Lifecycle::Ready(_))
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn queued_bytes(&self) -> u64 {
        self.queued_bytes
    }
    pub fn next_deadline(&self) -> Option<u64> {
        self.startup_deadline
            .into_iter()
            .chain(self.pending.values().filter_map(|p| p.deadline))
            .min()
    }
    /// Returns the existing generation for concurrent start requests.
    pub fn start(&mut self, now: u64) -> IvyResult<Generation> {
        match self.lifecycle {
            Lifecycle::Starting(generation) | Lifecycle::Ready(generation) => Ok(generation),
            Lifecycle::Stopped => {
                self.generation = self.generation.checked_add(1).ok_or_else(not_ready)?;
                let generation = Generation(self.generation);
                self.lifecycle = Lifecycle::Starting(generation);
                self.startup_deadline = Some(now.saturating_add(STARTUP_TIMEOUT_MS));
                Ok(generation)
            }
            Lifecycle::Retiring(_) | Lifecycle::StopUnconfirmed(_) => Err(not_ready()),
        }
    }
    pub fn started(&mut self, generation: Generation) -> bool {
        if self.lifecycle != Lifecycle::Starting(generation) {
            return false;
        }
        self.startup_deadline = None;
        self.lifecycle = Lifecycle::Ready(generation);
        true
    }
    /// Reserve immediately before posting. A failed pipe write must call fail:
    /// partial delivery cannot establish that the request was not executed.
    pub fn post(&mut self, now: u64, options: RequestOptions) -> IvyResult<RequestId> {
        let Lifecycle::Ready(generation) = self.lifecycle else {
            return Err(not_ready());
        };
        let bytes = options.reserved_bytes.max(1024);
        if self.pending.len() >= MAX_PENDING
            || bytes > MAX_QUEUED_BYTES.saturating_sub(self.queued_bytes)
        {
            return Err(IvyError::new(
                "limit_exceeded",
                "Hive storage request queue is full.",
            ));
        }
        self.sequence = self.sequence.checked_add(1).ok_or_else(not_ready)?;
        let id = RequestId {
            generation,
            sequence: self.sequence,
        };
        self.pending.insert(
            id,
            Pending {
                options,
                bytes,
                deadline: Some(now.saturating_add(options.timeout_ms)),
                started: false,
                settled: false,
            },
        );
        self.queued_bytes += bytes;
        Ok(id)
    }
    pub fn request_started(&mut self, id: RequestId, now: u64) {
        if self.lifecycle != Lifecycle::Ready(id.generation) {
            return;
        }
        if let Some(pending) = self.pending.get_mut(&id) {
            pending.started = true;
            pending.deadline = Some(now.saturating_add(pending.options.timeout_ms));
        }
    }
    pub fn progress(&mut self, id: RequestId, now: u64) {
        if self.pending.get(&id).is_some_and(|p| p.options.backup) {
            self.request_started(id, now);
        }
    }
    pub fn reply(&mut self, id: RequestId, result: IvyResult<IvyJson>) -> Option<Settlement> {
        if self.lifecycle != Lifecycle::Ready(id.generation) {
            return None;
        }
        let pending = self.pending.remove(&id)?;
        self.queued_bytes -= pending.bytes;
        (!pending.settled).then_some(Settlement { id, result })
    }
    pub fn expire(&mut self, now: u64) -> Effects {
        if self
            .startup_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            let Lifecycle::Starting(generation) = self.lifecycle else {
                unreachable!()
            };
            return self.fail(
                generation,
                IvyError::new(
                    "deadline_exceeded",
                    "Hive storage startup exceeded its deadline.",
                ),
            );
        }
        // Settle expired queue waits before retiring an active request at the
        // same deadline, so each caller receives its relevant timeout reason.
        let mut due: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(id, p)| {
                p.deadline
                    .filter(|d| *d <= now)
                    .map(|d| (d, p.started, *id))
            })
            .collect();
        due.sort_unstable();
        let mut effects = Effects::default();
        for (_, _, id) in due {
            let pending = self.pending.get_mut(&id).expect("due request exists");
            pending.deadline = None;
            let error = unknown(
                "deadline_exceeded",
                if pending.started {
                    "Hive storage response deadline exceeded; reconcile operation identity."
                } else {
                    "Hive storage queue wait expired; reconcile operation identity."
                },
            );
            if pending.started && pending.options.terminate_on_timeout {
                let failed = self.fail(id.generation, error);
                effects.settlements.extend(failed.settlements);
                effects.retire = failed.retire;
                effects.failure = failed.failure;
                break;
            }
            if !pending.settled {
                pending.settled = true;
                effects.settlements.push(Settlement {
                    id,
                    result: Err(error),
                });
            }
        }
        effects
    }
    pub fn fail(&mut self, generation: Generation, error: IvyError) -> Effects {
        if !matches!(self.lifecycle, Lifecycle::Starting(g) | Lifecycle::Ready(g) if g == generation)
        {
            return Effects::default();
        }
        self.lifecycle = Lifecycle::Retiring(generation);
        self.startup_deadline = None;
        let mut effects = Effects {
            retire: Some(generation),
            failure: Some(error.clone()),
            ..Effects::default()
        };
        for (id, pending) in &mut self.pending {
            pending.deadline = None;
            if !pending.settled {
                effects.settlements.push(Settlement {
                    id: *id,
                    result: Err(error.clone()),
                });
                pending.settled = true;
            }
        }
        effects
    }
    pub fn connection_failed(&mut self, generation: Generation) -> Effects {
        self.fail(
            generation,
            unknown("storage_unavailable", "Hive storage connection failed."),
        )
    }
    /// Call after the cooperative `close` request, or if that request expires.
    pub fn shutdown(&mut self) -> Effects {
        match self.lifecycle {
            Lifecycle::Starting(g) | Lifecycle::Ready(g) => {
                let mut effects =
                    self.fail(g, unknown("service_unavailable", "Hive is shutting down."));
                effects.failure = None;
                effects
            }
            _ => Effects::default(),
        }
    }
    /// Confirmation refers to the database-owning child, not merely pipe EOF or
    /// the exit of an intermediate launcher. False fences the database closed.
    pub fn retired(&mut self, generation: Generation, confirmed: bool) -> bool {
        if !matches!(self.lifecycle, Lifecycle::Retiring(g) | Lifecycle::StopUnconfirmed(g) if g == generation)
        {
            return false;
        }
        if !confirmed {
            self.lifecycle = Lifecycle::StopUnconfirmed(generation);
            return false;
        }
        self.pending.clear();
        self.queued_bytes = 0;
        self.lifecycle = Lifecycle::Stopped;
        true
    }
}
