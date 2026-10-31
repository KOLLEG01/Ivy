//! Transient native events. Native turn history owns recovery after a gap.

use crate::native_journal::NativeJournal;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    agent_schema::validate_agent,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::service::ToolHandler;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

pub fn notification_handler(
    events: Arc<Mutex<NativeNotifications>>,
    journal: Arc<Mutex<NativeJournal>>,
) -> ToolHandler {
    Arc::new(move |args, _| {
        let events = events.clone();
        let journal = journal.clone();
        Box::pin(async move {
            let epoch = journal
                .lock()
                .map_err(|_| IvyError::internal())?
                .epoch()
                .cloned();
            let result = events
                .lock()
                .map_err(|_| IvyError::internal())?
                .page_json(&args, epoch.as_ref());
            result
        })
    })
}

struct Event {
    sequence: u64,
    json: Box<str>,
    charge: usize,
}

fn transport_charge(encoded: &str) -> usize {
    encoded.len()
        + 2
        + encoded
            .bytes()
            .map(|byte| match byte {
                b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 2,
                0..=31 => 6,
                _ => 1,
            })
            .sum::<usize>()
}

pub struct NativeNotifications {
    service_node_id: JsString,
    native_version: JsString,
    sequence: u64,
    end: u64,
    maximum_bytes: usize,
    json_bytes: usize,
    events: VecDeque<Event>,
}

impl NativeNotifications {
    pub fn new(
        service_node_id: String,
        native_version: String,
        start: u64,
        end: u64,
        maximum_bytes: usize,
    ) -> Self {
        Self::new_js(
            service_node_id.into(),
            native_version.into(),
            start,
            end,
            maximum_bytes,
        )
    }

    pub fn new_js(
        service_node_id: JsString,
        native_version: JsString,
        start: u64,
        end: u64,
        maximum_bytes: usize,
    ) -> Self {
        Self {
            service_node_id,
            native_version,
            sequence: start,
            end,
            maximum_bytes,
            json_bytes: 0,
            events: VecDeque::new(),
        }
    }

    pub fn observe(
        &mut self,
        epoch: &str,
        method: &str,
        params: IvyJson,
        observed_at: &str,
    ) -> IvyResult<IvyJson> {
        self.observe_js(&epoch.into(), &method.into(), params, &observed_at.into())
    }

    pub fn observe_js(
        &mut self,
        epoch: &JsString,
        method: &JsString,
        params: IvyJson,
        observed_at: &JsString,
    ) -> IvyResult<IvyJson> {
        if self.sequence >= self.end {
            return Err(IvyError::new(
                "native_notification_capacity",
                "Notification sequence reservation exhausted.",
            ));
        }
        self.sequence += 1;
        let value = IvyJson::object([
            ("sequence", IvyJson::Number(self.sequence as f64)),
            (
                "serviceNodeId",
                IvyJson::String(self.service_node_id.clone()),
            ),
            (
                "nativeVersion",
                IvyJson::String(self.native_version.clone()),
            ),
            ("epoch", IvyJson::String(epoch.clone())),
            ("method", IvyJson::String(method.clone())),
            ("params", params),
            ("observedAt", IvyJson::String(observed_at.clone())),
        ]);
        let encoded = stringify(&value, usize::MAX)?;
        let bytes = encoded.len();
        if bytes.saturating_add(std::mem::size_of::<Event>()) <= self.maximum_bytes {
            let charge = transport_charge(&encoded);
            self.events.push_back(Event {
                sequence: self.sequence,
                json: encoded.into_boxed_str(),
                charge,
            });
            self.json_bytes += bytes;
            while self.retained_bytes() > self.maximum_bytes || self.events.len() > 10_000 {
                // Release spare slots before discarding a row that would fit.
                if self.events.len() <= self.events.capacity() / 2 {
                    self.events.shrink_to_fit();
                    if self.retained_bytes() <= self.maximum_bytes && self.events.len() <= 10_000 {
                        break;
                    }
                }
                let event = self
                    .events
                    .pop_front()
                    .expect("retained bytes belong to an event");
                self.json_bytes -= event.json.len();
                if self.events.is_empty() {
                    self.events = VecDeque::new();
                }
            }
        }
        Ok(value)
    }

    fn retained_bytes(&self) -> usize {
        self.json_bytes + self.events.capacity() * std::mem::size_of::<Event>()
    }

    /// The handler validates NotificationQuery before passing its typed fields.
    /// A future cursor is a gap, not permission to hide the current sequence.
    pub fn page(
        &self,
        after_sequence: u64,
        limit: usize,
        epoch: Option<&str>,
    ) -> IvyResult<IvyJson> {
        self.page_value(
            after_sequence,
            limit,
            epoch.map_or(IvyJson::Null, IvyJson::from),
        )
    }

    pub fn page_json(&self, query: &IvyJson, epoch: Option<&JsString>) -> IvyResult<IvyJson> {
        validate_agent("NotificationQuery", query)?;
        self.page_value(
            query.get("afterSequence").unwrap().as_f64().unwrap() as u64,
            query
                .get("limit")
                .and_then(IvyJson::as_f64)
                .unwrap_or(100.0) as usize,
            epoch.cloned().map(IvyJson::String).unwrap_or(IvyJson::Null),
        )
    }

    fn page_value(&self, after_sequence: u64, limit: usize, epoch: IvyJson) -> IvyResult<IvyJson> {
        let first = self
            .events
            .front()
            .map_or(self.sequence + 1, |event| event.sequence);
        let mut items = Vec::new();
        let mut last = after_sequence;
        let (mut bytes, mut transport) = (0, 0);
        let mut has_more = false;
        let mut gap = last < first - 1 || last > self.sequence;
        for event in &self.events {
            if event.sequence <= after_sequence {
                continue;
            }
            if items.len() == limit
                || (!items.is_empty()
                    && (bytes + event.json.len() > 9 * 1024 * 1024
                        || transport + event.charge > MANAGEMENT_FRAME_BYTES - 1024 * 1024))
            {
                has_more = true;
                break;
            }
            if event.charge > MANAGEMENT_FRAME_BYTES - 1024 * 1024 {
                return Err(IvyError::new(
                    "native_notification_too_large",
                    "Notification cannot fit a transport-safe page.",
                ));
            }
            if event.sequence != last + 1 {
                gap = true;
            }
            items.push(parse(&event.json, usize::MAX)?);
            last = event.sequence;
            bytes += event.json.len();
            transport += event.charge;
        }
        if !has_more && last != self.sequence {
            gap = true;
        }
        Ok(IvyJson::object([
            ("epoch", epoch),
            ("firstAvailableSequence", IvyJson::Number(first as f64)),
            (
                "throughSequence",
                IvyJson::Number(if has_more { last } else { self.sequence } as f64),
            ),
            ("gap", gap.into()),
            ("hasMore", has_more.into()),
            ("items", IvyJson::Array(items)),
        ]))
    }
}
