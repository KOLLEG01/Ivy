//! Arrival bounds and retained request reservations, independent of transport.
mod dispatch;
pub use dispatch::DispatchGate;

use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, limits::CONSUMER_IN_FLIGHT_REQUESTS};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub struct AuthenticationRateOptions {
    pub window: Duration,
    pub maximum: usize,
    pub per_client: usize,
    pub client_maximum: usize,
}
impl Default for AuthenticationRateOptions {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(1),
            maximum: 512,
            per_client: 256,
            client_maximum: 4096,
        }
    }
}
#[derive(Clone, Copy)]
enum ArrivalTime {
    Monotonic(Instant),
    Milliseconds(f64),
}
impl ArrivalTime {
    fn elapsed(self, at: Self, window: Duration) -> bool {
        match (self, at) {
            (Self::Monotonic(now), Self::Monotonic(at)) => {
                now.saturating_duration_since(at) >= window
            }
            (Self::Milliseconds(now), Self::Milliseconds(at)) => {
                now - at >= window.as_secs_f64() * 1000.0
            }
            _ => true,
        }
    }
}
struct Arrivals {
    at: ArrivalTime,
    count: usize,
}
#[derive(Default)]
pub struct AuthenticationRate {
    options: AuthenticationRateOptions,
    global: Option<Arrivals>,
    clients: HashMap<JsString, Arrivals>,
}
impl AuthenticationRate {
    pub fn new(options: AuthenticationRateOptions) -> Self {
        Self {
            options,
            global: None,
            clients: HashMap::new(),
        }
    }
    pub fn admit(&mut self, key: &JsString, now: Instant) -> IvyResult<()> {
        self.admit_time(key, ArrivalTime::Monotonic(now))
    }
    pub(crate) fn admit_at_ms(&mut self, key: &JsString, now: f64) -> IvyResult<()> {
        self.admit_time(key, ArrivalTime::Milliseconds(now))
    }
    fn admit_time(&mut self, key: &JsString, now: ArrivalTime) -> IvyResult<()> {
        let elapsed = |entry: &Arrivals| now.elapsed(entry.at, self.options.window);
        if self.global.as_ref().is_none_or(elapsed) {
            self.global = Some(Arrivals { at: now, count: 0 });
        }
        if self.clients.get(key).is_none_or(elapsed) {
            self.clients.retain(|_, entry| !elapsed(entry));
            if self.clients.len() >= self.options.client_maximum {
                return Err(IvyError::new(
                    "limit_exceeded",
                    "Authentication client budget is full.",
                ));
            }
            self.clients
                .insert(key.clone(), Arrivals { at: now, count: 0 });
        }
        let global = self.global.as_mut().unwrap();
        let client = self.clients.get_mut(key).unwrap();
        let full = global.count >= self.options.maximum;
        if full || client.count >= self.options.per_client {
            return Err(
                IvyError::new("limit_exceeded", "Authentication arrival budget is full.")
                    .with_details(IvyJson::object([
                        ("budget", "authentication_arrivals".into()),
                        ("scope", if full { "global" } else { "client" }.into()),
                        (
                            "active",
                            IvyJson::Number(if full { global.count } else { client.count } as f64),
                        ),
                        (
                            "limit",
                            IvyJson::Number(if full {
                                self.options.maximum
                            } else {
                                self.options.per_client
                            } as f64),
                        ),
                        (
                            "windowMs",
                            IvyJson::Number(self.options.window.as_secs_f64() * 1000.0),
                        ),
                    ])),
            );
        }
        global.count += 1;
        client.count += 1;
        Ok(())
    }
}

pub struct RequestBudgetOptions {
    pub maximum: usize,
    pub maximum_bytes: usize,
    pub per_consumer: usize,
    pub per_consumer_bytes: usize,
    pub name: String,
}
impl Default for RequestBudgetOptions {
    fn default() -> Self {
        Self {
            maximum: 2048,
            maximum_bytes: 128 * 1024 * 1024,
            per_consumer: CONSUMER_IN_FLIGHT_REQUESTS,
            per_consumer_bytes: 64 * 1024 * 1024,
            name: "request".into(),
        }
    }
}
#[derive(Default)]
struct Usage {
    count: usize,
    bytes: usize,
}
struct RequestState {
    options: RequestBudgetOptions,
    usage: Usage,
    consumers: HashMap<JsString, Usage>,
}
impl RequestState {
    fn reserve(&mut self, key: &JsString, amount: usize) -> IvyResult<()> {
        let consumer = self.consumers.get(key).map_or(0, |entry| entry.bytes);
        let global = self
            .usage
            .bytes
            .checked_add(amount)
            .is_none_or(|n| n > self.options.maximum_bytes);
        if global
            || consumer
                .checked_add(amount)
                .is_none_or(|n| n > self.options.per_consumer_bytes)
        {
            return Err(IvyError::new(
                "limit_exceeded",
                format!("Hive retained {} bytes are full.", self.options.name),
            )
            .with_details(IvyJson::object([
                ("budget", format!("{}_bytes", self.options.name).into()),
                ("scope", if global { "global" } else { "consumer" }.into()),
                (
                    "active",
                    IvyJson::Number(if global { self.usage.bytes } else { consumer } as f64),
                ),
                ("requested", IvyJson::Number(amount as f64)),
                (
                    "limit",
                    IvyJson::Number(if global {
                        self.options.maximum_bytes
                    } else {
                        self.options.per_consumer_bytes
                    } as f64),
                ),
            ])));
        }
        self.usage.bytes += amount;
        self.consumers.entry(key.clone()).or_default().bytes += amount;
        Ok(())
    }
}
/// Reservations stay charged until accepted work completes, including when a
/// caller abandons its reply. The work owner explicitly releases its lease.
#[derive(Clone)]
pub struct RequestBudget(Arc<Mutex<RequestState>>);
impl Default for RequestBudget {
    fn default() -> Self {
        Self::new(RequestBudgetOptions::default())
    }
}
impl RequestBudget {
    pub fn new(options: RequestBudgetOptions) -> Self {
        Self(Arc::new(Mutex::new(RequestState {
            options,
            usage: Usage::default(),
            consumers: HashMap::new(),
        })))
    }
    /// None selects the 1024-byte initial reservation.
    pub fn acquire(&self, key: &JsString, bytes: Option<usize>) -> IvyResult<RequestLease> {
        let mut state = self.0.lock().unwrap();
        let count = state.consumers.get(key).map_or(0, |entry| entry.count);
        let global = state.usage.count >= state.options.maximum;
        if global || count >= state.options.per_consumer {
            return Err(IvyError::new(
                "limit_exceeded",
                format!("Hive {} slots are full.", state.options.name),
            )
            .with_details(IvyJson::object([
                ("budget", state.options.name.clone().into()),
                ("scope", if global { "global" } else { "consumer" }.into()),
                (
                    "active",
                    IvyJson::Number(if global { state.usage.count } else { count } as f64),
                ),
                (
                    "limit",
                    IvyJson::Number(if global {
                        state.options.maximum
                    } else {
                        state.options.per_consumer
                    } as f64),
                ),
            ])));
        }
        let amount = bytes.unwrap_or(1024);
        state.reserve(key, amount)?;
        state.usage.count += 1;
        state.consumers.get_mut(key).unwrap().count += 1;
        Ok(RequestLease {
            state: self.0.clone(),
            key: key.clone(),
            held: amount,
            released: false,
        })
    }
}
pub struct RequestLease {
    state: Arc<Mutex<RequestState>>,
    key: JsString,
    held: usize,
    released: bool,
}
impl RequestLease {
    pub fn grow(&mut self, amount: usize) -> IvyResult<()> {
        if self.released {
            return Err(IvyError::new(
                "invalid_arguments",
                "Invalid request reservation.",
            ));
        }
        self.state.lock().unwrap().reserve(&self.key, amount)?;
        self.held += amount;
        Ok(())
    }
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let mut state = self.state.lock().unwrap();
        state.usage.count -= 1;
        state.usage.bytes -= self.held;
        let consumer = state.consumers.get_mut(&self.key).unwrap();
        consumer.count -= 1;
        consumer.bytes -= self.held;
        if consumer.count == 0 {
            state.consumers.remove(&self.key);
        }
    }
}
