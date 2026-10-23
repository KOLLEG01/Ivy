//! The firmware RPC state on one HID epoch. Replies acknowledge host requests;
//! they never authorize a Desktop action.
use super::{MAXIMUM_MESSAGE_BYTES, Report, frame, payload};
use crate::native_rpc::root_values;
use ivy_contracts::{
    IvyJson,
    json::{ParseOptions, parse, parse_with_options},
};
use serde_json::value::RawValue;
use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroProtocolError {
    message: String,
    argument: bool,
}
impl std::fmt::Display for MicroProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for MicroProtocolError {}
impl MicroProtocolError {
    fn argument(message: &str) -> Self {
        Self {
            message: message.into(),
            argument: true,
        }
    }
    pub fn is_argument(&self) -> bool {
        self.argument
    }
}

#[derive(Default)]
pub struct MicroProtocol {
    pending: Vec<u8>,
    saw_rgb_configuration: bool,
    saw_status: bool,
    status_requests: i64,
    dictation_sequence: i64,
    lighting: Option<&'static str>,
    last_method: Option<String>,
}
impl MicroProtocol {
    pub fn saw_rgb_configuration(&self) -> bool {
        self.saw_rgb_configuration
    }
    pub fn saw_status(&self) -> bool {
        self.saw_status
    }
    pub fn status_requests(&self) -> i64 {
        self.status_requests
    }
    pub fn dictation_sequence(&self) -> i64 {
        self.dictation_sequence
    }
    pub fn lighting_state(&self) -> &'static str {
        self.lighting.unwrap_or("unknown")
    }
    pub fn last_method(&self) -> Option<&str> {
        self.last_method.as_deref()
    }

    pub fn receive(&mut self, report: &[u8]) -> Result<Vec<Report>, MicroProtocolError> {
        let part = payload(report).map_err(|e| MicroProtocolError::argument(&e.to_string()))?;
        if self.pending.len() + part.len() > MAXIMUM_MESSAGE_BYTES {
            return Err(MicroProtocolError::argument(
                "Micro RPC message limit exceeded.",
            ));
        }
        self.pending.extend_from_slice(part);
        let mut consumed = 0;
        let mut replies = Vec::new();
        while consumed < self.pending.len() {
            while self
                .pending
                .get(consumed)
                .is_some_and(|b| matches!(b, 9 | 10 | 13 | 32))
            {
                consumed += 1;
            }
            if consumed == self.pending.len() {
                break;
            }
            // Borrowed RawValue only finds the complete JSON boundary. Shared
            // parsing retains UTF-16 strings, duplicate replacement and lexemes
            // remain available below for JsonElement's integer conversions.
            let bytes = &self.pending[consumed..];
            let tokens = opaque_utf8(bytes);
            let mut stream = serde_json::Deserializer::from_str(&tokens).into_iter::<&RawValue>();
            let raw = match stream.next() {
                Some(Ok(raw)) => raw.get(),
                Some(Err(e)) if e.is_eof() => {
                    check_partial_depth(bytes)?;
                    break;
                }
                _ => return Err(MicroProtocolError::argument("Invalid Micro RPC JSON.")),
            };
            let text = raw.to_owned();
            let count = stream.byte_offset();
            // A non-final Utf8JsonReader waits for a number's delimiter.
            if count == bytes.len() && matches!(text.as_bytes()[0], b'-' | b'0'..=b'9') {
                break;
            }
            let original = bytes[..count].to_vec();
            let message = parse_with_options(
                &text,
                MAXIMUM_MESSAGE_BYTES,
                ParseOptions {
                    reject_duplicate_keys: false,
                    maximum_container_depth: Some(32),
                },
            )
            .map_err(|_| MicroProtocolError::argument("Invalid Micro RPC JSON."))?;
            replies.extend(self.reply(&message, &text, &original)?);
            consumed += count;
        }
        // As in the original reader, a later failing message retains the whole
        // pending input, including earlier replies' already applied state.
        self.pending.drain(..consumed);
        Ok(replies)
    }
    fn reply(
        &mut self,
        message: &IvyJson,
        raw: &str,
        original: &[u8],
    ) -> Result<Vec<Report>, MicroProtocolError> {
        if !matches!(message, IvyJson::Object(_)) {
            return Err(MicroProtocolError::argument("Micro RPC object required."));
        }
        let key = if message.get("id").is_some() {
            "id"
        } else {
            "i"
        };
        let Some(id) = message.get(key) else {
            return Ok(Vec::new());
        };
        let id = match id {
            IvyJson::Number(_) => property(raw, key).and_then(|s| s.parse::<i64>().ok()),
            IvyJson::String(s) => {
                accessed_utf8(raw, original, key)?;
                Some(s.to_utf8().map_err(|_| invalid_surrogate())?)
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|s| s.parse::<i64>().ok())
            }
            _ => None,
        }
        .ok_or_else(|| MicroProtocolError::argument("Integral Micro RPC id required."))?;
        if !(0..=9_007_199_254_740_991).contains(&id) {
            return Err(MicroProtocolError::argument(
                "Micro RPC id outside exact JSON range.",
            ));
        }
        let method_key = if message.get("method").is_some() {
            "method"
        } else {
            "m"
        };
        let method = message.get(method_key);
        let Some(IvyJson::String(method)) = method else {
            return Err(MicroProtocolError::argument("Micro RPC method required."));
        };
        accessed_utf8(raw, original, method_key)?;
        let method_text = method.to_utf8().map_err(|_| invalid_surrogate())?;
        if !(1..=128).contains(&method.utf16_len()) {
            return Err(MicroProtocolError::argument("Micro RPC method required."));
        }
        let method = method_text;
        self.last_method = Some(method.clone());
        let result = match method.as_str() {
            "device.status" => {
                self.saw_status = true;
                self.status_requests = self.status_requests.wrapping_add(1);
                "{\"version\":\"1.0.0\",\"profile_index\":0,\"layer_index\":0,\"battery\":100,\"is_charging\":false}"
            }
            "sys.version" => "\"1.0.0\"",
            "v.oai.rgbcfg" => {
                self.saw_rgb_configuration = true;
                self.observe_lighting(message, raw)?;
                "true"
            }
            _ => "true",
        };
        Ok(
            frame(format!("{{\"id\":{id},\"result\":{result}}}\n").as_bytes())
                .expect("bounded firmware response"),
        )
    }
    fn observe_lighting(&mut self, message: &IvyJson, raw: &str) -> Result<(), MicroProtocolError> {
        let key = if message.get("params").is_some() {
            "params"
        } else {
            "p"
        };
        let Some(parameters @ IvyJson::Object(_)) = message.get(key) else {
            return Ok(());
        };
        let Some(ambient @ IvyJson::Object(_)) = parameters.get("ambient") else {
            return Ok(());
        };
        let effect = if ambient.get("e").is_some() {
            "e"
        } else {
            "effect"
        };
        let color = if ambient.get("c").is_some() {
            "c"
        } else {
            "color"
        };
        let (Some(effect_value), Some(color_value)) = (ambient.get(effect), ambient.get(color))
        else {
            return Ok(());
        };
        let ambient_raw = property(raw, key)
            .and_then(|p| property(p, "ambient"))
            .expect("validated object property");
        let Some(effect) = int32(effect_value, property(ambient_raw, effect).unwrap())? else {
            return Ok(());
        };
        let Some(color) = int32(color_value, property(ambient_raw, color).unwrap())? else {
            return Ok(());
        };
        let recording = effect == 2 && color == 3_050_327;
        if recording && self.lighting_state() != "dictation_recording" {
            self.dictation_sequence = self.dictation_sequence.wrapping_add(1);
        }
        self.lighting = Some(if recording {
            "dictation_recording"
        } else {
            "other"
        });
        Ok(())
    }
}

fn invalid_surrogate() -> MicroProtocolError {
    MicroProtocolError {
        argument: false,
        message: "Cannot read incomplete UTF-16 JSON text as string with missing low surrogate."
            .into(),
    }
}
// JsonDocument keeps unused string bytes opaque. Replace invalid UTF-8 only in
// the parser's temporary view, one byte for one byte, so token offsets remain
// exact. Original bytes stay buffered and every string actually read by the
// firmware is validated before conversion. This is not replacement decoding
// of a method or request ID.
fn opaque_utf8(bytes: &[u8]) -> Cow<'_, str> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(text);
    }
    let mut tokens = bytes.to_vec();
    let mut at = 0;
    while let Err(error) = std::str::from_utf8(&tokens[at..]) {
        at += error.valid_up_to();
        let count = error.error_len().unwrap_or(tokens.len() - at);
        tokens[at..at + count].fill(b'?');
        at += count;
    }
    Cow::Owned(String::from_utf8(tokens).expect("validated token view"))
}
fn accessed_utf8(raw: &str, original: &[u8], key: &str) -> Result<(), MicroProtocolError> {
    let token = property(raw, key).expect("validated string property");
    let at = token.as_ptr() as usize - raw.as_ptr() as usize;
    if std::str::from_utf8(&original[at..at + token.len()]).is_err() {
        return Err(MicroProtocolError {
            argument: false,
            message: "Cannot transcode invalid UTF-8 JSON text to UTF-16 string.".into(),
        });
    }
    Ok(())
}
fn int32(value: &IvyJson, raw: &str) -> Result<Option<i32>, MicroProtocolError> {
    if matches!(value, IvyJson::Number(_)) {
        return Ok(raw.parse().ok());
    }
    let kind = match value {
        IvyJson::String(_) => "String",
        IvyJson::Object(_) => "Object",
        IvyJson::Array(_) => "Array",
        IvyJson::Bool(true) => "True",
        IvyJson::Bool(false) => "False",
        _ => "Null",
    };
    Err(MicroProtocolError {
        argument: false,
        message: format!(
            "The requested operation requires an element of type 'Number', but the target element has type '{kind}'."
        ),
    })
}
// Locate last-wins property lexemes after full shared JSON validation. Unlike
// the native envelope, the firmware permits duplicate property names.
fn property<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let mut at = text.find('{')? + 1;
    let mut found = None;
    for value in root_values(text) {
        while text.as_bytes()[at].is_ascii_whitespace() || text.as_bytes()[at] == b',' {
            at += 1;
        }
        let start = at;
        at += 1;
        while text.as_bytes()[at] != b'"' {
            if text.as_bytes()[at] == b'\\' {
                at += 1;
            }
            at += 1;
        }
        at += 1;
        if parse(&text[start..at], MAXIMUM_MESSAGE_BYTES).ok().as_ref()
            == Some(&IvyJson::from(name))
        {
            found = Some(value);
        }
        at = value.as_ptr() as usize - text.as_ptr() as usize + value.len();
    }
    found
}
// A non-final Utf8JsonReader rejects depth 33 even before its document ends.
// The common parser checks complete values; this walk checks only that one
// early bound, leaving syntax and UTF-8 decisions to the JSON implementations.
fn check_partial_depth(bytes: &[u8]) -> Result<(), MicroProtocolError> {
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for &b in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                quoted = false;
            }
        } else {
            match b {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 32 {
                        return Err(MicroProtocolError::argument("Invalid Micro RPC JSON."));
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => (),
            }
        }
    }
    Ok(())
}
