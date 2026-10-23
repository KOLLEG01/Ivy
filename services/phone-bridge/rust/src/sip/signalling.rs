//! Bounded safe signalling observations on the original endpoint inspector.
use rsipstack::sip::{SipMessage, prelude::*};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, Weak},
    time::SystemTime,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SipSignallingEvent {
    pub at: SystemTime,
    pub direction: &'static str,
    pub method: String,
    pub status: Option<u16>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SipSignallingObservation {
    pub sent: i64,
    pub received: i64,
    pub events: Vec<SipSignallingEvent>,
}
struct State {
    closed: bool,
    sent: i64,
    received: i64,
    events: VecDeque<SipSignallingEvent>,
}
struct Original {
    call_id: String,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct SipSignalling(Arc<Original>);
impl SipSignalling {
    pub fn observation(&self) -> SipSignallingObservation {
        let s = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        SipSignallingObservation {
            sent: s.sent,
            received: s.received,
            events: s.events.iter().cloned().collect(),
        }
    }
    pub(crate) fn close(&self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .closed = true;
    }
}
#[derive(Clone, Default)]
pub(super) struct SignallingObservers(Arc<Mutex<Vec<Weak<Original>>>>);
impl SignallingObservers {
    pub fn observe(&self, call_id: &str) -> SipSignalling {
        let original = Arc::new(Original {
            call_id: call_id.into(),
            state: Mutex::new(State {
                closed: false,
                sent: 0,
                received: 0,
                events: VecDeque::with_capacity(16),
            }),
        });
        let mut observers = self.0.lock().unwrap_or_else(|p| p.into_inner());
        observers.retain(|o| o.strong_count() > 0);
        observers.push(Arc::downgrade(&original));
        SipSignalling(original)
    }
    pub fn record(&self, message: &SipMessage, sent: bool) {
        let (call_id, method, status) = match message {
            SipMessage::Request(request) => {
                let Ok(id) = request.call_id_header() else {
                    return;
                };
                (id.value(), request.method.to_string(), None)
            }
            SipMessage::Response(response) => {
                let (Ok(id), Ok(method)) = (
                    response.call_id_header(),
                    response.cseq_header().and_then(|h| h.method()),
                ) else {
                    return;
                };
                (
                    id.value(),
                    method.to_string(),
                    Some(response.status_code.code()),
                )
            }
        };
        let observers = self.0.lock().unwrap_or_else(|p| p.into_inner());
        for original in observers
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|o| o.call_id == call_id)
        {
            let mut s = original.state.lock().unwrap_or_else(|p| p.into_inner());
            if s.closed {
                continue;
            }
            if sent {
                s.sent = s.sent.wrapping_add(1);
            } else {
                s.received = s.received.wrapping_add(1);
            }
            if s.events.len() == 16 {
                s.events.pop_front();
            }
            s.events.push_back(SipSignallingEvent {
                at: SystemTime::now(),
                direction: if sent { "sent" } else { "received" },
                method: method.clone(),
                status,
            });
        }
    }
}
