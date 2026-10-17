//! Transient native events. Native turn history owns recovery after a gap.

use ivy_contracts::{
    IvyError, IvyJson, IvyResult, json::stringify, limits::MANAGEMENT_FRAME_BYTES,
};
use std::collections::VecDeque;

struct Event {
    sequence: u64,
    value: IvyJson,
    bytes: usize,
    charge: usize,
}

pub struct NativeNotifications {
    service_node_id: String,
    native_version: String,
    sequence: u64,
    end: u64,
    maximum_bytes: usize,
    bytes: usize,
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
        Self {
            service_node_id,
            native_version,
            sequence: start,
            end,
            maximum_bytes,
            bytes: 0,
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
        if self.sequence >= self.end {
            return Err(IvyError::new(
                "native_notification_capacity",
                "Notification sequence reservation exhausted.",
            ));
        }
        self.sequence += 1;
        let value = IvyJson::object([
            ("sequence", IvyJson::Number(self.sequence as f64)),
            ("serviceNodeId", self.service_node_id.clone().into()),
            ("nativeVersion", self.native_version.clone().into()),
            ("epoch", epoch.into()),
            ("method", method.into()),
            ("params", params),
            ("observedAt", observed_at.into()),
        ]);
        let encoded = stringify(&value, usize::MAX)?;
        let bytes = encoded.len();
        let charge = bytes + stringify(&IvyJson::from(encoded), usize::MAX)?.len();
        if bytes <= self.maximum_bytes {
            self.events.push_back(Event {
                sequence: self.sequence,
                value: value.clone(),
                bytes,
                charge,
            });
            self.bytes += bytes;
        }
        while self.bytes > self.maximum_bytes || self.events.len() > 10_000 {
            self.bytes -= self
                .events
                .pop_front()
                .expect("retained bytes belong to an event")
                .bytes;
        }
        Ok(value)
    }

    /// The handler validates NotificationQuery before passing its typed fields.
    /// A future cursor is a gap, not permission to hide the current sequence.
    pub fn page(
        &self,
        after_sequence: u64,
        limit: usize,
        epoch: Option<&str>,
    ) -> IvyResult<IvyJson> {
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
                    && (bytes + event.bytes > 9 * 1024 * 1024
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
            items.push(event.value.clone());
            last = event.sequence;
            bytes += event.bytes;
            transport += event.charge;
        }
        if !has_more && last != self.sequence {
            gap = true;
        }
        Ok(IvyJson::object([
            ("epoch", epoch.map_or(IvyJson::Null, IvyJson::from)),
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
