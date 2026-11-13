//! The listener's owned view of a native notification. Protocol envelopes and
//! unrelated payloads stay at the JSON boundary.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use std::collections::VecDeque;

pub const MAX_NATIVE_EVENTS: usize = 256;
pub const MAX_NATIVE_EVENT_BYTES: usize = 8 * 1024 * 1024;

pub enum NativeEventKind {
    Started,
    Completed,
    Agent {
        id: Option<JsString>,
        text: Option<JsString>,
        commentary: bool,
    },
    User {
        client_id: Option<JsString>,
        source: IvyResult<Option<JsString>>,
    },
}
pub struct NativeEvent {
    pub thread_id: Option<JsString>,
    pub turn_id: Option<JsString>,
    pub kind: NativeEventKind,
}
pub enum EventProjection {
    Ignore,
    Recover,
    Event(NativeEvent),
}
fn string(value: Option<&Json>) -> Option<JsString> {
    value.and_then(Json::as_js_string).cloned()
}
fn string_bytes(value: Option<&Json>) -> usize {
    value
        .and_then(Json::as_js_string)
        .map_or(0, |s| s.0.len().saturating_mul(2))
}
impl NativeEvent {
    pub fn project(value: &Json, budget: usize) -> IvyResult<EventProjection> {
        value.as_object().ok_or_else(IvyError::internal)?;
        let method = value.get("method").and_then(Json::as_js_string);
        let started = method.is_some_and(|m| m.eq_str("turn/started"));
        let completed = method.is_some_and(|m| m.eq_str("turn/completed"));
        let item_completed = method.is_some_and(|m| m.eq_str("item/completed"));
        if !started && !completed && !item_completed {
            return Ok(EventProjection::Ignore);
        }
        let params = value
            .get("params")
            .filter(|p| p.as_object().is_some())
            .ok_or_else(IvyError::internal)?;
        let item = params.get("item");
        let agent = if item_completed {
            let Some(item) = item.filter(|i| i.as_object().is_some()) else {
                return Ok(EventProjection::Ignore);
            };
            let kind = item
                .get("type")
                .and_then(Json::as_js_string)
                .ok_or_else(IvyError::internal)?;
            if kind.eq_str("agentMessage") {
                true
            } else if kind.eq_str("userMessage") || kind.eq_str("steeringUserMessage") {
                false
            } else {
                return Ok(EventProjection::Ignore);
            }
        } else {
            false
        };
        // Reject an oversized selected text before cloning it. Unknown fields,
        // user text/media and the source protocol envelope never enter the queue.
        let selected = string_bytes(params.get("threadId"))
            .saturating_add(string_bytes(params.get("turnId")))
            .saturating_add(if agent {
                item.map_or(0, |i| {
                    string_bytes(i.get("id")).saturating_add(string_bytes(i.get("text")))
                })
            } else {
                0
            })
            .saturating_add(std::mem::size_of::<Self>());
        if selected > budget {
            return Ok(EventProjection::Recover);
        }
        let kind = if started {
            NativeEventKind::Started
        } else if completed {
            NativeEventKind::Completed
        } else if agent {
            let item = item.unwrap();
            NativeEventKind::Agent {
                id: string(item.get("id")),
                text: string(item.get("text")),
                commentary: item
                    .get("phase")
                    .and_then(Json::as_js_string)
                    .is_some_and(|p| p.eq_str("commentary")),
            }
        } else {
            let item = item.unwrap();
            NativeEventKind::User {
                client_id: string(item.get("clientId")),
                // Validation belongs after the bound thread/turn check. Keep
                // its small result, rather than the full delegation payload.
                source: super::listener::delegation_source(item),
            }
        };
        let event = Self {
            thread_id: string(params.get("threadId")),
            turn_id: string(params.get("turnId")),
            kind,
        };
        Ok(if event.retained_bytes() > budget {
            EventProjection::Recover
        } else {
            EventProjection::Event(event)
        })
    }
    pub fn retained_bytes(&self) -> usize {
        let bytes =
            |s: &Option<JsString>| s.as_ref().map_or(0, |s| s.0.capacity().saturating_mul(2));
        std::mem::size_of::<Self>()
            .saturating_add(bytes(&self.thread_id))
            .saturating_add(bytes(&self.turn_id))
            .saturating_add(match &self.kind {
                NativeEventKind::Agent { id, text, .. } => bytes(id).saturating_add(bytes(text)),
                NativeEventKind::User { client_id, source } => {
                    bytes(client_id).saturating_add(match source {
                        Ok(source) => bytes(source),
                        Err(error) => error
                            .retained_bytes()
                            .saturating_sub(std::mem::size_of::<IvyError>()),
                    })
                }
                _ => 0,
            })
    }
}

pub struct NativeEventQueue {
    events: VecDeque<NativeEvent>,
    bytes: usize,
    in_flight: usize,
}
impl Default for NativeEventQueue {
    fn default() -> Self {
        let events = VecDeque::with_capacity(MAX_NATIVE_EVENTS);
        // Include the slot owned by the single delivery lane as well.
        let bytes = (events.capacity() + 1) * std::mem::size_of::<NativeEvent>();
        Self {
            events,
            bytes,
            in_flight: 0,
        }
    }
}
impl NativeEventQueue {
    pub fn push(&mut self, event: NativeEvent) -> bool {
        let bytes = event
            .retained_bytes()
            .saturating_sub(std::mem::size_of::<NativeEvent>());
        if self.events.len() >= MAX_NATIVE_EVENTS
            || self.bytes.saturating_add(bytes) > MAX_NATIVE_EVENT_BYTES
        {
            return false;
        }
        self.bytes += bytes;
        self.events.push_back(event);
        true
    }
    pub fn pop(&mut self) -> Option<NativeEvent> {
        let event = self.events.pop_front()?;
        self.in_flight += event
            .retained_bytes()
            .saturating_sub(std::mem::size_of::<NativeEvent>());
        Some(event)
    }
    pub fn complete(&mut self, event: &NativeEvent) {
        let bytes = event
            .retained_bytes()
            .saturating_sub(std::mem::size_of::<NativeEvent>());
        self.in_flight -= bytes;
        self.bytes -= bytes;
    }
    pub fn clear(&mut self) {
        self.events.clear();
        self.bytes =
            (self.events.capacity() + 1) * std::mem::size_of::<NativeEvent>() + self.in_flight;
    }
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }
}
