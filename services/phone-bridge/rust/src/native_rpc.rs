//! Native envelope admission and process-local replay receipts. The service's
//! durable journal remains the authority after a native process/epoch is lost.
use crate::ipc::{FRAME_BYTES, FrameError, MAX_RECEIPTS, has_receipt_capacity, is_read_only};
use ivy_contracts::{
    IvyJson,
    hash::digest,
    json::{ParseOptions, parse_with_options},
};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

/// Fields are private so only a bounded, fully validated envelope can be admitted.
/// Raw params are retained for the native retry identity, not canonicalized.
pub struct NativeRequest {
    epoch: String,
    request_id: u64,
    operation_id: Option<String>,
    method: String,
    params: IvyJson,
    raw_params: String,
}

impl NativeRequest {
    pub fn parse(frame: &[u8]) -> Result<Self, FrameError> {
        if frame.is_empty() {
            return Err(FrameError::Empty);
        }
        if frame.len() > FRAME_BYTES {
            return Err(FrameError::Oversized);
        }
        let text = std::str::from_utf8(frame).map_err(|_| FrameError::InvalidUtf8)?;
        let value = parse_with_options(
            text,
            FRAME_BYTES,
            ParseOptions {
                reject_duplicate_keys: true,
                maximum_container_depth: Some(24),
            },
        )
        .map_err(|_| FrameError::InvalidRequest)?;
        let IvyJson::Object(fields) = &value else {
            return Err(FrameError::InvalidRequest);
        };
        const NAMES: [&str; 6] = [
            "version",
            "epoch",
            "requestId",
            "operationId",
            "method",
            "params",
        ];
        if fields.len() != NAMES.len()
            || NAMES.iter().any(|name| value.get(name).is_none())
            || !valid_property_names(&value)
        {
            return Err(FrameError::InvalidRequest);
        }
        let spans = root_values(text);
        let raw = |name: &str| {
            fields
                .iter()
                .position(|(key, _)| key == &name.into())
                .map(|index| spans[index])
        };
        if raw("version") != Some("1") {
            return Err(FrameError::InvalidRequest);
        }
        let request_id = raw("requestId")
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|id| (1..=9_007_199_254_740_991).contains(id))
            .ok_or(FrameError::InvalidRequest)?;
        let string = |name: &str| {
            value
                .get(name)
                .unwrap()
                .as_str()
                .map_err(|_| FrameError::InvalidRequest)
        };
        let epoch = string("epoch")?;
        let method = string("method")?;
        if !guid_d(&epoch) || !known_method(&method) {
            return Err(FrameError::InvalidRequest);
        }
        let operation_id = if is_read_only(&method) {
            if !value.get("operationId").unwrap().is_null() {
                return Err(FrameError::InvalidRequest);
            }
            None
        } else {
            let id = string("operationId")?;
            if !guid_d(&id) {
                return Err(FrameError::InvalidRequest);
            }
            Some(id)
        };
        let params = value.get("params").unwrap();
        if !matches!(params, IvyJson::Object(_)) {
            return Err(FrameError::InvalidRequest);
        }
        Ok(Self {
            epoch,
            request_id,
            operation_id,
            method,
            params: params.clone(),
            raw_params: raw("params").unwrap().into(),
        })
    }
    pub fn epoch(&self) -> &str {
        &self.epoch
    }
    pub fn request_id(&self) -> u64 {
        self.request_id
    }
    pub fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }
    pub fn method(&self) -> &str {
        &self.method
    }
    pub fn params(&self) -> &IvyJson {
        &self.params
    }
    pub fn raw_params(&self) -> &str {
        &self.raw_params
    }
    fn hash(&self) -> String {
        digest(format!("{}\n{}", self.method, self.raw_params))
    }
}

// JsonElement's property-name access rejects escaped lone surrogates; string
// values in params remain opaque until the relevant native DTO consumes them.
fn valid_property_names(value: &IvyJson) -> bool {
    match value {
        IvyJson::Object(fields) => fields
            .iter()
            .all(|(key, value)| key.to_utf8().is_ok() && valid_property_names(value)),
        IvyJson::Array(values) => values.iter().all(valid_property_names),
        _ => true,
    }
}

/// Guid.TryParseExact("D") accepts surrounding whitespace and either hex case.
/// Keep the original spelling: epoch/operation comparisons are ordinal strings.
fn guid_d(value: &str) -> bool {
    let bytes = value.trim().as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn known_method(method: &str) -> bool {
    is_read_only(method)
        || matches!(
            method,
            "registration.reconnect"
                | "configure"
                | "desktop.launch"
                | "call.desktop.launch"
                | "call.desktop.startVoice"
                | "call.desktop.pauseVoice"
                | "call.desktop.resumeVoice"
                | "call.desktop.stopVoice"
                | "call.screening.prepare"
                | "call.screening.bridge"
                | "call.features"
                | "call.windows.connect"
                | "call.codec.upgrade"
                | "call.realtime.prepare"
                | "call.realtime.answer"
                | "call.realtime.stop"
                | "call.prepare"
                | "call.audio.prepare"
                | "call.audio.rebind"
                | "call.dial"
                | "call.claim"
                | "call.answer"
                | "call.waiting.end"
                | "call.command.feedback"
                | "call.hangup"
                | "call.release"
                | "shutdown"
        )
}

// Locate root property value spans AFTER shared JSON validation. This is only a
// token-span walk, not a competing JSON parser. Preserve the exact params text,
// including nested whitespace, escape spelling, number lexemes and property order.
fn root_values(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let space = |at: &mut usize| {
        while bytes.get(*at).is_some_and(u8::is_ascii_whitespace) {
            *at += 1;
        }
    };
    let string_end = |at: &mut usize| {
        *at += 1;
        while bytes[*at] != b'"' {
            if bytes[*at] == b'\\' {
                *at += 1;
            }
            *at += 1;
        }
        *at += 1;
    };
    space(&mut at);
    at += 1;
    let mut values = Vec::with_capacity(6);
    loop {
        space(&mut at);
        if bytes[at] == b'}' {
            break;
        }
        string_end(&mut at);
        space(&mut at);
        at += 1;
        space(&mut at);
        let start = at;
        match bytes[at] {
            b'"' => string_end(&mut at),
            b'{' | b'[' => {
                let mut depth = 0;
                loop {
                    match bytes[at] {
                        b'"' => {
                            string_end(&mut at);
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => depth -= 1,
                        _ => (),
                    }
                    at += 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            _ => {
                while !matches!(bytes[at], b',' | b'}' | b' ' | b'\n' | b'\r' | b'\t') {
                    at += 1;
                }
            }
        }
        values.push(&text[start..at]);
        space(&mut at);
        if bytes[at] == b'}' {
            break;
        }
        at += 1;
    }
    values
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeOutcome {
    result: Option<String>,
    error: Option<String>,
}
impl NativeOutcome {
    /// Handlers serialize before completing receipts. Over-budget serialization
    /// is uncertain because the effect may already have happened.
    pub fn result(json: String) -> Self {
        if json.len() > FRAME_BYTES - 512 {
            return Self::error("operation_outcome_unknown");
        }
        if parse_with_options(
            &json,
            FRAME_BYTES,
            ParseOptions {
                maximum_container_depth: Some(24),
                ..Default::default()
            },
        )
        .is_err()
        {
            return Self::error("invalid_request");
        }
        Self {
            result: Some(json),
            error: None,
        }
    }
    pub fn error(code: &str) -> Self {
        Self {
            result: None,
            error: Some(code.into()),
        }
    }
    pub fn result_json(&self) -> Option<&str> {
        self.result.as_deref()
    }
    pub fn error_code(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// A receipt holds no request/credentials. Replays share the original pending
/// completion; a later completion cannot replace its retained safe outcome.
pub struct NativeReceipt {
    hash: String,
    outcome: OnceLock<NativeOutcome>,
}
impl NativeReceipt {
    pub fn outcome(&self) -> Option<&NativeOutcome> {
        self.outcome.get()
    }
    pub fn complete(&self, outcome: NativeOutcome) -> Result<(), NativeOutcome> {
        self.outcome.set(outcome)
    }
}
pub enum Admission {
    Dispatch(Option<Arc<NativeReceipt>>),
    Replay(Arc<NativeReceipt>),
    Reject(&'static str),
}

/// Owned by the serialized native dispatcher. Admission precedes invocation;
/// pending and completed effects count toward the same non-evicting capacity.
pub struct NativeReceipts {
    epoch: String,
    capacity: usize,
    stopping: bool,
    receipts: HashMap<String, Arc<NativeReceipt>>,
}
impl NativeReceipts {
    pub fn new(epoch: String, capacity: usize) -> Result<Self, FrameError> {
        if !guid_d(&epoch) || !(1..=MAX_RECEIPTS).contains(&capacity) {
            return Err(FrameError::InvalidRequest);
        }
        Ok(Self {
            epoch,
            capacity,
            stopping: false,
            receipts: HashMap::new(),
        })
    }
    pub fn stop(&mut self) {
        self.stopping = true;
    }
    pub fn len(&self) -> usize {
        self.receipts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.receipts.is_empty()
    }
    /// Recheck just before invocation too: Stop may occur after admission while
    /// the dispatcher yields. Read-only calls still require epoch and live owner.
    pub fn may_invoke(&self) -> bool {
        !self.stopping
    }
    pub fn admit(&mut self, request: &NativeRequest, cleanup_slots: usize) -> Admission {
        if request.epoch != self.epoch {
            return Admission::Reject("epoch_mismatch");
        }
        if self.stopping {
            return Admission::Reject("runtime_stopping");
        }
        let Some(operation_id) = &request.operation_id else {
            return Admission::Dispatch(None);
        };
        let hash = request.hash();
        if let Some(receipt) = self.receipts.get(operation_id) {
            return if receipt.hash == hash {
                Admission::Replay(receipt.clone())
            } else {
                Admission::Reject("operation_conflict")
            };
        }
        if !has_receipt_capacity(&request.method, self.len(), self.capacity, cleanup_slots) {
            return Admission::Reject("capacity_exceeded");
        }
        let receipt = Arc::new(NativeReceipt {
            hash,
            outcome: OnceLock::new(),
        });
        self.receipts.insert(operation_id.clone(), receipt.clone());
        Admission::Dispatch(Some(receipt))
    }
}
