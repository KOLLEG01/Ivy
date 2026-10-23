use super::error::{AdapterError, Result};
use ivy_contracts::{
    IvyJson, JsString,
    json::{parse, stringify},
};
use std::{fs, io::Write, path::Path, sync::Arc};

pub type Compare = Arc<dyn Fn(&JsString, &JsString) -> std::cmp::Ordering + Send + Sync>;
pub type IdSource = Arc<dyn Fn() -> JsString + Send + Sync>;
/// Explicit lossless scalar construction; no UTF-8 conversion or rounding.
pub trait JsonScalar {
    fn into_json(self) -> IvyJson;
}
impl JsonScalar for JsString {
    fn into_json(self) -> IvyJson {
        IvyJson::String(self)
    }
}
impl JsonScalar for f64 {
    fn into_json(self) -> IvyJson {
        IvyJson::Number(self)
    }
}
pub static NULL: IvyJson = IvyJson::Null;
pub fn field<'a>(v: &'a IvyJson, key: &str) -> &'a IvyJson {
    v.get(key).unwrap_or(&NULL)
}
pub fn number(v: &IvyJson) -> f64 {
    v.as_f64().unwrap_or_default()
}
pub fn nonempty_string(v: &IvyJson) -> bool {
    v.as_js_string().is_some_and(|value| !value.0.is_empty())
}
pub fn string(v: &IvyJson) -> JsString {
    v.as_js_string().cloned().unwrap_or_default()
}
pub fn cursor_offset(value: &IvyJson) -> Result<f64> {
    let invalid = || {
        AdapterError::typed(
            "invalid_arguments",
            "Cursor must be a nonnegative decimal offset.",
        )
    };
    let offset = match value {
        IvyJson::Null => 0.0,
        IvyJson::Number(value) => *value,
        IvyJson::String(value) => value
            .to_utf8()
            .map_err(|_| invalid())?
            .parse::<u64>()
            .map_err(|_| invalid())? as f64,
        _ => return Err(invalid()),
    };
    if offset.is_finite() && offset >= 0.0 && safe_integer(offset) {
        Ok(offset)
    } else {
        Err(invalid())
    }
}
pub fn timestamp_cursor(value: &JsString) -> Result<f64> {
    let invalid = || {
        AdapterError::typed(
            "invalid_arguments",
            "Cursor must contain a finite decimal timestamp.",
        )
    };
    value
        .to_utf8()
        .map_err(|_| invalid())?
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(invalid)
}
pub fn is(v: &IvyJson, s: &str) -> bool {
    v.as_js_string().is_some_and(|v| *v == JsString::from(s))
}
pub fn str_or(v: &IvyJson, fallback: &IvyJson) -> IvyJson {
    if v.as_js_string().is_some_and(|v| !v.0.is_empty()) {
        v.clone()
    } else {
        fallback.clone()
    }
}
pub fn coalesce(v: &IvyJson, fallback: &IvyJson) -> IvyJson {
    if v.is_null() {
        fallback.clone()
    } else {
        v.clone()
    }
}
pub fn concat(parts: &[&JsString]) -> JsString {
    JsString(parts.iter().flat_map(|p| p.0.iter().copied()).collect())
}
pub fn join(parts: &[JsString], separator: &str) -> JsString {
    let mut units = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            units.extend(separator.encode_utf16());
        }
        units.extend(&part.0);
    }
    JsString(units)
}
pub fn slice(v: &JsString, start: usize, end: usize) -> JsString {
    JsString(v.0[start.min(v.0.len())..end.min(v.0.len()).max(start.min(v.0.len()))].to_vec())
}
pub fn safe_integer(n: f64) -> bool {
    n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0
}
pub fn object() -> IvyJson {
    IvyJson::object([] as [(&str, IvyJson); 0])
}
pub fn encode(v: &IvyJson) -> Result<String> {
    stringify(v, usize::MAX).map_err(|e| AdapterError::typed("invalid_data", e.message))
}
pub fn decode(bytes: &[u8]) -> Result<IvyJson> {
    parse(&String::from_utf8_lossy(bytes), usize::MAX)
        .map_err(|e| AdapterError::typed("invalid_data", e.message))
}
pub fn minimum(a: f64, b: f64) -> f64 {
    a.min(b)
}
pub fn maximum(a: f64, b: f64) -> f64 {
    a.max(b)
}
pub fn round(v: f64) -> f64 {
    v.round()
}
pub fn array_slice(values: &[IvyJson], start: f64, end: f64) -> Vec<IvyJson> {
    let index = |v: f64| (v.max(0.0) as usize).min(values.len());
    let start = index(start);
    let end = index(end).max(start);
    values[start..end].to_vec()
}
/// Preserve the original fixed temporary name, mode, write/rename ordering and
/// retained failure evidence. Callers decide when their memory state advances.
pub fn save_json(path: &Path, value: &IvyJson) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(".tmp");
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(Path::new(&temporary))?;
    file.write_all((encode(value)? + "\n").as_bytes())?;
    drop(file);
    fs::rename(Path::new(&temporary), path)?;
    Ok(())
}
