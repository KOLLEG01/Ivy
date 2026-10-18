use crate::{IvyError, IvyResult, limits::MANAGEMENT_FRAME_BYTES};
use std::cmp::Ordering;
#[path = "json_number.rs"]
mod number;
#[path = "json_value.rs"]
mod value_traits;
pub use number::number_from_string;

/// ECMAScript string code units, including unpaired surrogates. Ord is UTF-16 order.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JsString(pub Vec<u16>);
impl From<&str> for JsString {
    fn from(s: &str) -> Self {
        Self(s.encode_utf16().collect())
    }
}
impl From<String> for JsString {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}
impl JsString {
    /// English locale lowercase, retaining unpaired UTF-16 units as non-cased
    /// boundaries and preserving context within each well-formed segment.
    pub fn to_english_lowercase(&self) -> Self {
        self.map_case(str::to_lowercase)
    }
    /// ECMAScript's locale-independent full Unicode uppercase mapping.
    pub fn to_uppercase(&self) -> Self {
        self.map_case(str::to_uppercase)
    }
    /// ECMAScript trimStart whitespace, retaining all remaining UTF-16 units.
    pub fn trim_start(&self) -> Self {
        let start = self
            .0
            .iter()
            .position(|unit| !number::space(*unit))
            .unwrap_or(self.0.len());
        Self(self.0[start..].to_vec())
    }
    /// ECMAScript trim whitespace at both ends without decoding or replacing units.
    pub fn trim(&self) -> Self {
        let start = self
            .0
            .iter()
            .position(|unit| !number::space(*unit))
            .unwrap_or(self.0.len());
        let end = self
            .0
            .iter()
            .rposition(|unit| !number::space(*unit))
            .map_or(start, |index| index + 1);
        Self(self.0[start..end].to_vec())
    }
    fn map_case(&self, mapping: fn(&str) -> String) -> Self {
        let mut output = Vec::new();
        let mut segment = String::new();
        for c in char::decode_utf16(self.0.iter().copied()) {
            match c {
                Ok(c) => segment.push(c),
                Err(error) => {
                    output.extend(mapping(&segment).encode_utf16());
                    segment.clear();
                    output.push(error.unpaired_surrogate());
                }
            }
        }
        output.extend(mapping(&segment).encode_utf16());
        Self(output)
    }
    pub fn contains_ascii(&self, needle: &str) -> bool {
        debug_assert!(needle.is_ascii());
        needle.is_empty()
            || self.0.windows(needle.len()).any(|window| {
                window
                    .iter()
                    .zip(needle.bytes())
                    .all(|(a, b)| *a == u16::from(b))
            })
    }
    pub fn to_utf8(&self) -> IvyResult<String> {
        String::from_utf16(&self.0).map_err(|_| invalid("String contains an unpaired surrogate."))
    }
    pub fn utf16_len(&self) -> usize {
        self.0.len()
    }
    pub fn code_point_len(&self) -> usize {
        char::decode_utf16(self.0.iter().copied()).count()
    }
}

/// JavaScript String(value) for JSON values, including failed object coercion.
/// JSON objects cannot supply callable overrides; an own toString shadows the
/// inherited method and makes string-hint conversion fail. Array conversion
/// applies the same rule recursively, but does not inspect object properties.
pub fn try_js_string(value: &IvyJson) -> IvyResult<JsString> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            IvyJson::Array(values) => pending.extend(values.iter()),
            IvyJson::Object(_) if value.get("toString").is_some() => {
                return Err(IvyError::internal());
            }
            _ => {}
        }
    }
    Ok(js_string(value))
}

/// Unchecked JSON string rendering. Call try_js_string at a JavaScript String
/// boundary unless object coercion is already checked or impossible. Arrays join
/// recursively and null array elements become empty strings.
pub fn js_string(value: &IvyJson) -> JsString {
    enum Part<'a> {
        Value(&'a IvyJson),
        Comma,
    }
    let mut pending = vec![Part::Value(value)];
    let mut units = Vec::new();
    while let Some(part) = pending.pop() {
        let value = match part {
            Part::Comma => {
                units.push(44);
                continue;
            }
            Part::Value(value) => value,
        };
        match value {
            IvyJson::Array(values) => {
                for (index, value) in values.iter().enumerate().rev() {
                    if !value.is_null() {
                        pending.push(Part::Value(value));
                    }
                    if index > 0 {
                        pending.push(Part::Comma);
                    }
                }
            }
            IvyJson::String(s) => units.extend_from_slice(&s.0),
            IvyJson::Null => units.extend("null".encode_utf16()),
            IvyJson::Bool(v) => units.extend((if *v { "true" } else { "false" }).encode_utf16()),
            IvyJson::Object(_) => units.extend("[object Object]".encode_utf16()),
            IvyJson::Number(v) => {
                let mut buffer = ryu_js::Buffer::new();
                let text = if v.is_nan() {
                    "NaN"
                } else if *v == f64::INFINITY {
                    "Infinity"
                } else if *v == f64::NEG_INFINITY {
                    "-Infinity"
                } else if *v == 0.0 {
                    "0"
                } else {
                    buffer.format(*v)
                };
                units.extend(text.encode_utf16());
            }
        }
    }
    JsString(units)
}

/// A lossless JSON value with stack-independent cloning and destruction.
/// Use borrowed matches or the `into_*` methods to extract owned fields.
pub enum IvyJson {
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    Array(Vec<IvyJson>),
    Object(Vec<(JsString, IvyJson)>),
}
impl From<&str> for IvyJson {
    fn from(s: &str) -> Self {
        Self::String(s.into())
    }
}
impl From<String> for IvyJson {
    fn from(s: String) -> Self {
        Self::String(s.into())
    }
}
impl From<bool> for IvyJson {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}
impl IvyJson {
    pub fn into_array(mut self) -> Option<Vec<Self>> {
        if let Self::Array(values) = &mut self {
            Some(std::mem::take(values))
        } else {
            None
        }
    }
    pub fn into_object(mut self) -> Option<Vec<(JsString, Self)>> {
        if let Self::Object(pairs) = &mut self {
            Some(std::mem::take(pairs))
        } else {
            None
        }
    }
    pub fn into_js_string(mut self) -> Option<JsString> {
        if let Self::String(value) = &mut self {
            Some(std::mem::take(value))
        } else {
            None
        }
    }
    pub fn object<K: Into<JsString>>(pairs: impl IntoIterator<Item = (K, Self)>) -> Self {
        let mut object = Self::Object(Vec::new());
        for (key, value) in pairs {
            object.insert(key, value);
        }
        object
    }
    pub fn insert(&mut self, key: impl Into<JsString>, value: Self) {
        let Self::Object(pairs) = self else {
            panic!("insert requires an object")
        };
        let key = key.into();
        if let Some(pair) = pairs.iter_mut().find(|p| p.0 == key) {
            pair.1 = value;
        } else {
            pairs.push((key, value));
        }
    }
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.get_key(&JsString::from(key))
    }
    pub fn get_key(&self, key: &JsString) -> Option<&Self> {
        match self {
            Self::Object(pairs) => pairs.iter().find(|p| &p.0 == key).map(|p| &p.1),
            _ => None,
        }
    }
    pub fn as_str(&self) -> IvyResult<String> {
        match self {
            Self::String(s) => s.to_utf8(),
            _ => Err(invalid("Expected a string.")),
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        if let Self::Number(v) = self {
            Some(*v)
        } else {
            None
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(v) = self {
            Some(*v)
        } else {
            None
        }
    }
    pub fn as_array(&self) -> Option<&[Self]> {
        if let Self::Array(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn as_object(&self) -> Option<&[(JsString, Self)]> {
        if let Self::Object(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn as_js_string(&self) -> Option<&JsString> {
        if let Self::String(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    pub fn required(&self, key: &str) -> IvyResult<&Self> {
        self.get(key)
            .ok_or_else(|| invalid(&format!("Missing required property: {key}")))
    }
    pub fn remove(&mut self, key: &str) -> Option<Self> {
        let key = JsString::from(key);
        if let Self::Object(v) = self {
            v.iter().position(|p| p.0 == key).map(|i| v.remove(i).1)
        } else {
            None
        }
    }
    pub fn to_serde(&self) -> IvyResult<serde_json::Value> {
        // Explicit and fallible: never replace lone surrogates or round unsafe integers silently.
        serde_json::from_str(&canonical(self, MANAGEMENT_FRAME_BYTES)?)
            .map_err(|_| invalid("Value cannot be represented by serde JSON."))
    }
    pub fn from_serde(value: &serde_json::Value) -> IvyResult<Self> {
        parse(&value.to_string(), MANAGEMENT_FRAME_BYTES)
    }
}
fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}
fn complexity() -> IvyError {
    IvyError::new("limit_exceeded", "JSON structure is too complex.")
}

/// Parse JSON with JSON.parse's duplicate-key replacement and f64 number semantics.
/// Validation of safe numbers belongs to validate_json/canonical, not JSON.parse.
pub fn parse(text: &str, maximum_bytes: usize) -> IvyResult<IvyJson> {
    parse_with_options(text, maximum_bytes, ParseOptions::default())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ParseOptions {
    pub reject_duplicate_keys: bool,
    /// Root object/array counts as one; scalar leaves do not add a container.
    pub maximum_container_depth: Option<usize>,
}
pub fn parse_with_options(
    text: &str,
    maximum_bytes: usize,
    options: ParseOptions,
) -> IvyResult<IvyJson> {
    if text.len() > maximum_bytes {
        return Err(IvyError::new(
            "content_too_large",
            "JSON exceeds the configured byte limit.",
        ));
    }
    let mut p = Parser {
        text,
        bytes: text.as_bytes(),
        at: 0,
        options,
    };
    let value = p.value()?;
    p.space();
    if p.at != p.bytes.len() {
        return Err(invalid("Unexpected content after JSON value."));
    }
    Ok(value)
}
struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
    options: ParseOptions,
}
impl Parser<'_> {
    fn space(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.at += 1;
        }
    }
    fn take(&mut self, b: u8) -> bool {
        if self.bytes.get(self.at) == Some(&b) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, b: u8) -> IvyResult<()> {
        if self.take(b) {
            Ok(())
        } else {
            Err(invalid("Invalid JSON syntax."))
        }
    }
    fn object_key(&mut self) -> IvyResult<JsString> {
        self.space();
        let key = self.string()?;
        self.space();
        self.expect(b':')?;
        Ok(key)
    }
    fn value(&mut self) -> IvyResult<IvyJson> {
        enum Frame {
            Array(Vec<IvyJson>),
            Object { value: IvyJson, key: JsString },
        }
        let mut frames = Vec::new();
        loop {
            self.space();
            if matches!(self.bytes.get(self.at), Some(b'[' | b'{'))
                && self
                    .options
                    .maximum_container_depth
                    .is_some_and(|maximum| frames.len() >= maximum)
            {
                return Err(complexity());
            }
            let mut value = match self.bytes.get(self.at).copied() {
                Some(b'"') => IvyJson::String(self.string()?),
                Some(b'[') => {
                    self.at += 1;
                    self.space();
                    if self.take(b']') {
                        IvyJson::Array(Vec::new())
                    } else {
                        frames.push(Frame::Array(Vec::new()));
                        continue;
                    }
                }
                Some(b'{') => {
                    self.at += 1;
                    self.space();
                    let value = IvyJson::Object(Vec::new());
                    if self.take(b'}') {
                        value
                    } else {
                        frames.push(Frame::Object {
                            value,
                            key: self.object_key()?,
                        });
                        continue;
                    }
                }
                Some(b'n') => {
                    self.literal(b"null")?;
                    IvyJson::Null
                }
                Some(b't') => {
                    self.literal(b"true")?;
                    IvyJson::Bool(true)
                }
                Some(b'f') => {
                    self.literal(b"false")?;
                    IvyJson::Bool(false)
                }
                Some(b'-' | b'0'..=b'9') => self.number()?,
                _ => return Err(invalid("Invalid JSON syntax.")),
            };
            loop {
                let Some(frame) = frames.last_mut() else {
                    return Ok(value);
                };
                self.space();
                match frame {
                    Frame::Array(values) => {
                        values.push(value);
                        if self.take(b']') {
                            let Some(Frame::Array(values)) = frames.pop() else {
                                unreachable!()
                            };
                            value = IvyJson::Array(values);
                        } else {
                            self.expect(b',')?;
                            break;
                        }
                    }
                    Frame::Object { value: object, key } => {
                        object.insert(std::mem::take(key), value);
                        if self.take(b'}') {
                            let Some(Frame::Object { value: object, .. }) = frames.pop() else {
                                unreachable!()
                            };
                            value = object;
                        } else {
                            self.expect(b',')?;
                            *key = self.object_key()?;
                            if self.options.reject_duplicate_keys && object.get_key(key).is_some() {
                                return Err(invalid("Duplicate JSON object property."));
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    fn literal(&mut self, s: &[u8]) -> IvyResult<()> {
        if self.bytes.get(self.at..self.at + s.len()) == Some(s) {
            self.at += s.len();
            Ok(())
        } else {
            Err(invalid("Invalid JSON literal."))
        }
    }
    fn number(&mut self) -> IvyResult<IvyJson> {
        let start = self.at;
        self.take(b'-');
        if !self.take(b'0') {
            let digits = self.at;
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1;
            }
            if digits == self.at {
                return Err(invalid("Invalid JSON number."));
            }
        }
        if self.take(b'.') {
            let digits = self.at;
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1;
            }
            if digits == self.at {
                return Err(invalid("Invalid JSON number."));
            }
        }
        if self.take(b'e') || self.take(b'E') {
            if !self.take(b'+') {
                self.take(b'-');
            }
            let digits = self.at;
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1;
            }
            if digits == self.at {
                return Err(invalid("Invalid JSON number."));
            }
        }
        let value = std::str::from_utf8(&self.bytes[start..self.at])
            .unwrap()
            .parse::<f64>()
            .map_err(|_| invalid("Invalid JSON number."))?;
        Ok(IvyJson::Number(value))
    }
    fn string(&mut self) -> IvyResult<JsString> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            let b = *self
                .bytes
                .get(self.at)
                .ok_or_else(|| invalid("Unterminated JSON string."))?;
            self.at += 1;
            match b {
                b'"' => return Ok(JsString(out)),
                0..=31 => return Err(invalid("Invalid JSON string control character.")),
                b'\\' => {
                    let escaped = *self
                        .bytes
                        .get(self.at)
                        .ok_or_else(|| invalid("Invalid JSON escape."))?;
                    self.at += 1;
                    out.push(match escaped {
                        b'"' => 34,
                        b'\\' => 92,
                        b'/' => 47,
                        b'b' => 8,
                        b'f' => 12,
                        b'n' => 10,
                        b'r' => 13,
                        b't' => 9,
                        b'u' => {
                            let mut n = 0;
                            for _ in 0..4 {
                                let b = *self
                                    .bytes
                                    .get(self.at)
                                    .ok_or_else(|| invalid("Invalid JSON Unicode escape."))?;
                                self.at += 1;
                                n = n * 16
                                    + (b as char)
                                        .to_digit(16)
                                        .ok_or_else(|| invalid("Invalid JSON Unicode escape."))?
                                        as u16;
                            }
                            n
                        }
                        _ => return Err(invalid("Invalid JSON escape.")),
                    });
                }
                32..=127 => out.push(b as u16),
                _ => {
                    self.at -= 1;
                    // The input is already UTF-8; advance one whole scalar without
                    // revalidating the remaining document for every non-ASCII byte.
                    let c = self.text[self.at..].chars().next().unwrap();
                    self.at += c.len_utf8();
                    let mut units = [0; 2];
                    out.extend_from_slice(c.encode_utf16(&mut units));
                }
            }
        }
    }
}

pub fn canonical(value: &IvyJson, maximum_bytes: usize) -> IvyResult<String> {
    encode(value, maximum_bytes, true, true)
}
pub fn stringify(value: &IvyJson, maximum_bytes: usize) -> IvyResult<String> {
    encode(value, maximum_bytes, false, false)
}
pub fn validate_json(value: &IvyJson, maximum_bytes: usize) -> IvyResult<()> {
    // The JS validator checks the entire domain before checking wire size.
    let encoded = encode(value, usize::MAX, false, true)?;
    if encoded.len() > maximum_bytes {
        return Err(too_large());
    }
    Ok(())
}
fn too_large() -> IvyError {
    IvyError::new(
        "content_too_large",
        "JSON exceeds the configured byte limit.",
    )
}
fn encode(
    value: &IvyJson,
    maximum_bytes: usize,
    sorted: bool,
    validate: bool,
) -> IvyResult<String> {
    let mut encoder = Encoder {
        out: String::new(),
        maximum_bytes,
        nodes: 0,
        sorted,
        validate,
        accounted: 0,
    };
    encoder.value(value)?;
    Ok(encoder.out)
}
struct Encoder {
    out: String,
    maximum_bytes: usize,
    nodes: usize,
    sorted: bool,
    validate: bool,
    accounted: usize,
}
impl Encoder {
    fn emit(&mut self, text: &str) -> IvyResult<()> {
        self.account(text.len())?;
        self.out.push_str(text);
        Ok(())
    }
    fn account(&mut self, bytes: usize) -> IvyResult<()> {
        self.accounted = self.accounted.saturating_add(bytes);
        if self.accounted > self.maximum_bytes {
            return Err(too_large());
        }
        Ok(())
    }
    fn punctuation(&mut self, text: &str) -> IvyResult<()> {
        if self.sorted {
            self.out.push_str(text);
            Ok(())
        } else {
            self.emit(text)
        }
    }
    fn string(&mut self, s: &JsString) -> IvyResult<()> {
        self.emit("\"")?;
        for unit in char::decode_utf16(s.0.iter().copied()) {
            match unit {
                Err(e) => self.emit(&format!("\\u{:04x}", e.unpaired_surrogate()))?,
                Ok(c) => match c {
                    '"' => self.emit("\\\"")?,
                    '\\' => self.emit("\\\\")?,
                    '\n' => self.emit("\\n")?,
                    '\r' => self.emit("\\r")?,
                    '\t' => self.emit("\\t")?,
                    '\u{8}' => self.emit("\\b")?,
                    '\u{c}' => self.emit("\\f")?,
                    c if (c as u32) < 32 => self.emit(&format!("\\u{:04x}", c as u32))?,
                    c => {
                        let mut b = [0; 4];
                        self.emit(c.encode_utf8(&mut b))?;
                    }
                },
            }
        }
        self.emit("\"")
    }
    fn value(&mut self, value: &IvyJson) -> IvyResult<()> {
        enum Part<'a> {
            Value(&'a IvyJson, usize),
            Key(&'a JsString),
            Punctuation(&'static str),
            Colon,
            Account(usize),
        }
        let mut pending = vec![Part::Value(value, 0)];
        while let Some(part) = pending.pop() {
            let (value, depth) = match part {
                Part::Punctuation(text) => {
                    self.punctuation(text)?;
                    continue;
                }
                Part::Colon => {
                    self.emit(":")?;
                    continue;
                }
                Part::Account(bytes) => {
                    self.account(bytes)?;
                    continue;
                }
                Part::Key(key) => {
                    self.string(key)?;
                    continue;
                }
                Part::Value(value, depth) => (value, depth),
            };
            self.nodes += 1;
            if self.validate && (self.nodes > 400_000 || depth > 128) {
                return Err(complexity());
            }
            match value {
                IvyJson::Null => self.emit("null")?,
                IvyJson::Bool(value) => self.emit(if *value { "true" } else { "false" })?,
                IvyJson::String(value) => self.string(value)?,
                IvyJson::Number(number) => {
                    if self.validate
                        && (!number.is_finite()
                            || (number.fract() == 0.0 && number.abs() > 9_007_199_254_740_991.0))
                    {
                        return Err(invalid("Numbers must be finite and integers must be safe."));
                    }
                    if !number.is_finite() {
                        self.emit("null")?;
                    } else if *number == 0.0 {
                        self.emit("0")?;
                    } else {
                        self.emit(ryu_js::Buffer::new().format(*number))?;
                    }
                }
                IvyJson::Array(values) => {
                    self.punctuation("[")?;
                    if self.sorted {
                        pending.push(Part::Account(2 + values.len().saturating_sub(1)));
                    }
                    pending.push(Part::Punctuation("]"));
                    for (index, value) in values.iter().enumerate().rev() {
                        pending.push(Part::Value(value, depth + 1));
                        if index > 0 {
                            pending.push(Part::Punctuation(","));
                        }
                    }
                }
                IvyJson::Object(pairs) => {
                    let mut indices: Vec<_> = (0..pairs.len()).collect();
                    if self.sorted {
                        indices.sort_by(|a, b| pairs[*a].0.cmp(&pairs[*b].0));
                    } else {
                        indices
                            .sort_by_key(|i| array_index(&pairs[*i].0).map_or((1, 0), |n| (0, n)));
                    }
                    self.punctuation("{")?;
                    if self.sorted {
                        pending.push(Part::Account(2 + pairs.len().saturating_sub(1)));
                    }
                    pending.push(Part::Punctuation("}"));
                    for (index, pair_index) in indices.into_iter().enumerate().rev() {
                        let (key, value) = &pairs[pair_index];
                        pending.push(Part::Value(value, depth + 1));
                        pending.push(Part::Colon);
                        pending.push(Part::Key(key));
                        if index > 0 {
                            pending.push(Part::Punctuation(","));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Own entries of a JSON object in ECMAScript enumeration order: numeric index
/// keys first, then other keys in insertion order. Other JSON types have no entries.
pub fn object_entries(value: &IvyJson) -> Vec<(&JsString, &IvyJson)> {
    let mut entries: Vec<_> = value
        .as_object()
        .unwrap_or(&[])
        .iter()
        .map(|(k, v)| (k, v))
        .collect();
    entries.sort_by_key(|(key, _)| array_index(key).map_or((1, 0), |index| (0, index)));
    entries
}

pub(crate) fn array_index(s: &JsString) -> Option<u32> {
    let s = s.to_utf8().ok()?;
    let n = s.parse::<u32>().ok()?;
    if n == u32::MAX || n.to_string() != s {
        None
    } else {
        Some(n)
    }
}

pub fn compare_versions(a: &str, b: &str) -> IvyResult<Ordering> {
    fn parts(s: &str) -> IvyResult<Vec<&str>> {
        let p: Vec<_> = s.split('.').collect();
        if s.len() > 128
            || p.len() != 3
            || p.iter().any(|v| {
                v.is_empty()
                    || !v.bytes().all(|b| b.is_ascii_digit())
                    || (v.len() > 1 && v.starts_with('0'))
            })
        {
            Err(invalid("Expected an exact numeric contract version."))
        } else {
            Ok(p)
        }
    }
    let a = parts(a)?;
    let b = parts(b)?;
    for i in 0..3 {
        let cmp = a[i].len().cmp(&b[i].len()).then_with(|| a[i].cmp(b[i]));
        if cmp != Ordering::Equal {
            return Ok(cmp);
        }
    }
    Ok(Ordering::Equal)
}

/// ECMAScript encodeURIComponent, which rejects lone surrogates.
pub fn encode_uri_component(value: &JsString) -> IvyResult<String> {
    let text = value.to_utf8()?;
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(out)
}
