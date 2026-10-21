//! Arrival bounds and retained request reservations, independent of transport.
mod dispatch;
pub use dispatch::DispatchGate;

use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, limits::CONSUMER_IN_FLIGHT_REQUESTS};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub struct AuthenticationRateOptions {
    pub window_ms: f64,
    pub maximum: f64,
    pub per_client: f64,
    pub client_maximum: f64,
}
impl Default for AuthenticationRateOptions {
    fn default() -> Self {
        Self {
            window_ms: 1000.0,
            maximum: 512.0,
            per_client: 256.0,
            client_maximum: 4096.0,
        }
    }
}
#[derive(Default)]
struct Arrivals {
    at: f64,
    count: f64,
}
#[derive(Default)]
pub struct AuthenticationRate {
    options: AuthenticationRateOptions,
    global: Arrivals,
    clients: HashMap<JsString, Arrivals>,
}
impl AuthenticationRate {
    pub fn new(options: AuthenticationRateOptions) -> Self {
        Self {
            options,
            global: Arrivals::default(),
            clients: HashMap::new(),
        }
    }
    /// The request owner supplies its captured wall clock in milliseconds.
    pub fn admit(&mut self, key: &JsString, now: f64) -> IvyResult<()> {
        if now - self.global.at >= self.options.window_ms {
            self.global = Arrivals {
                at: now,
                count: 0.0,
            };
        }
        if self
            .clients
            .get(key)
            .is_none_or(|entry| now - entry.at >= self.options.window_ms)
        {
            self.clients
                .retain(|_, entry| !(now - entry.at >= self.options.window_ms));
            if !((self.clients.len() as f64) < self.options.client_maximum) {
                return Err(IvyError::new(
                    "limit_exceeded",
                    "Authentication client budget is full.",
                ));
            }
            self.clients.insert(
                key.clone(),
                Arrivals {
                    at: now,
                    count: 0.0,
                },
            );
        }
        let client = self.clients.get_mut(key).unwrap();
        let global = self.global.count >= self.options.maximum;
        if global || client.count >= self.options.per_client {
            return Err(
                IvyError::new("limit_exceeded", "Authentication arrival budget is full.")
                    .with_details(IvyJson::object([
                        ("budget", "authentication_arrivals".into()),
                        ("scope", if global { "global" } else { "client" }.into()),
                        (
                            "active",
                            IvyJson::Number(if global {
                                self.global.count
                            } else {
                                client.count
                            }),
                        ),
                        (
                            "limit",
                            IvyJson::Number(if global {
                                self.options.maximum
                            } else {
                                self.options.per_client
                            }),
                        ),
                        ("windowMs", IvyJson::Number(self.options.window_ms)),
                    ])),
            );
        }
        self.global.count += 1.0;
        client.count += 1.0;
        Ok(())
    }
}

pub struct RequestBudgetOptions {
    pub maximum: f64,
    pub maximum_bytes: f64,
    pub per_consumer: f64,
    pub per_consumer_bytes: f64,
    pub name: JsString,
}
impl Default for RequestBudgetOptions {
    fn default() -> Self {
        Self {
            maximum: 2048.0,
            maximum_bytes: 128.0 * 1024.0 * 1024.0,
            per_consumer: CONSUMER_IN_FLIGHT_REQUESTS as f64,
            per_consumer_bytes: 64.0 * 1024.0 * 1024.0,
            name: "request".into(),
        }
    }
}
#[derive(Default)]
struct Usage {
    count: f64,
    bytes: f64,
}
struct RequestState {
    options: RequestBudgetOptions,
    usage: Usage,
    consumers: HashMap<JsString, Usage>,
}
fn named_message(prefix: &str, name: &JsString, suffix: &str) -> JsString {
    let mut units: Vec<_> = prefix.encode_utf16().collect();
    units.extend_from_slice(&name.0);
    units.extend(suffix.encode_utf16());
    JsString(units)
}
fn invalid_reservation() -> IvyError {
    IvyError::new("invalid_arguments", "Invalid request reservation.")
}
impl RequestState {
    fn reserve(&mut self, key: &JsString, amount: f64) -> IvyResult<()> {
        // Each reservation is a JS safe integer; aggregate accounting retains
        // the original IEEE-754 arithmetic, including its rounding behavior.
        if !amount.is_finite()
            || amount.fract() != 0.0
            || !(0.0..=9_007_199_254_740_991.0).contains(&amount)
        {
            return Err(invalid_reservation());
        }
        let consumer = self.consumers.get(key).map_or(0.0, |entry| entry.bytes);
        let global = self.usage.bytes + amount > self.options.maximum_bytes;
        if global || consumer + amount > self.options.per_consumer_bytes {
            let mut budget = self.options.name.clone();
            budget.0.extend("_bytes".encode_utf16());
            return Err(IvyError::new_js(
                "limit_exceeded".into(),
                named_message("Hive retained ", &self.options.name, " bytes are full."),
            )
            .with_details(IvyJson::object([
                ("budget", IvyJson::String(budget)),
                ("scope", if global { "global" } else { "consumer" }.into()),
                (
                    "active",
                    IvyJson::Number(if global { self.usage.bytes } else { consumer }),
                ),
                ("requested", IvyJson::Number(amount)),
                (
                    "limit",
                    IvyJson::Number(if global {
                        self.options.maximum_bytes
                    } else {
                        self.options.per_consumer_bytes
                    }),
                ),
            ])));
        }
        self.usage.bytes += amount;
        self.consumers.entry(key.clone()).or_default().bytes += amount;
        Ok(())
    }
}
/// Shared reservations remain charged through queueing and provider completion.
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
    /// None selects the original 1024-byte initial reservation.
    pub fn acquire(&self, key: &JsString, bytes: Option<f64>) -> IvyResult<RequestLease> {
        let mut state = self.0.lock().unwrap();
        let count = state.consumers.get(key).map_or(0.0, |entry| entry.count);
        let global = state.usage.count >= state.options.maximum;
        if global || count >= state.options.per_consumer {
            return Err(IvyError::new_js(
                "limit_exceeded".into(),
                named_message("Hive ", &state.options.name, " slots are full."),
            )
            .with_details(IvyJson::object([
                ("budget", IvyJson::String(state.options.name.clone())),
                ("scope", if global { "global" } else { "consumer" }.into()),
                (
                    "active",
                    IvyJson::Number(if global { state.usage.count } else { count }),
                ),
                (
                    "limit",
                    IvyJson::Number(if global {
                        state.options.maximum
                    } else {
                        state.options.per_consumer
                    }),
                ),
            ])));
        }
        let amount = bytes.unwrap_or(1024.0);
        state.reserve(key, amount)?;
        state.usage.count += 1.0;
        state.consumers.get_mut(key).unwrap().count += 1.0;
        Ok(RequestLease {
            state: self.0.clone(),
            key: key.clone(),
            held: 0.0 + amount,
            released: false,
        })
    }
}
/// Release is explicit and idempotent, as in the original request owner. Dropping
/// a handle does not silently refund work that may still be running elsewhere.
pub struct RequestLease {
    state: Arc<Mutex<RequestState>>,
    key: JsString,
    held: f64,
    released: bool,
}
impl RequestLease {
    pub fn grow(&mut self, amount: f64) -> IvyResult<()> {
        if self.released {
            return Err(invalid_reservation());
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
        state.usage.count -= 1.0;
        state.usage.bytes -= self.held;
        let consumer = state.consumers.get_mut(&self.key).unwrap();
        consumer.count -= 1.0;
        consumer.bytes -= self.held;
        if consumer.count == 0.0 {
            state.consumers.remove(&self.key);
        }
    }
}
