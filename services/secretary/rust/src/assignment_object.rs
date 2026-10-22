//! Object event projections and freshness policy using shared Date.parse semantics.
use ivy_contracts::{IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::clock::parse_date;

pub const OBJECT_WINDOW_KEY: &str = "secretary/object-window";

pub fn pointer(value: &IvyJson, path: &JsString) -> IvyJson {
    let Ok(path) = path.to_utf8() else {
        return IvyJson::Null;
    };
    if path.is_empty() {
        return value.clone();
    }
    let Some(path) = path.strip_prefix('/') else {
        return IvyJson::Null;
    };
    let mut current = value;
    for token in path.split('/') {
        let token = token.replace("~1", "/").replace("~0", "~");
        let selected = match current {
            IvyJson::Object(_) => current.get(&token),
            IvyJson::Array(values)
                if token == "0"
                    || (!token.is_empty()
                        && !token.starts_with('0')
                        && token.bytes().all(|byte| byte.is_ascii_digit())) =>
            {
                token
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| values.get(index))
            }
            _ => None,
        };
        let Some(selected) = selected else {
            return IvyJson::Null;
        };
        current = selected;
    }
    current.clone()
}

pub fn object_projection(value: &IvyJson, paths: &[JsString]) -> IvyJson {
    if paths.is_empty() {
        return value.clone();
    }
    let mut projection = IvyJson::Object(Vec::new());
    for path in paths {
        projection.insert(path.clone(), pointer(value, path));
    }
    projection
}

pub struct Observation {
    pub complete_path: JsString,
    pub observed_at_path: JsString,
    pub maximum_age_seconds: f64,
}

pub fn observation_ready(
    value: &IvyJson,
    observation: Option<&Observation>,
    due_at: &JsString,
    now: f64,
) -> bool {
    let Some(condition) = observation else {
        return true;
    };
    let at = pointer(value, &condition.observed_at_path);
    let timestamp = at.as_js_string().map(parse_date).unwrap_or(f64::NAN);
    pointer(value, &condition.complete_path).as_bool() == Some(true)
        && timestamp.is_finite()
        && timestamp >= parse_date(due_at)
        && timestamp <= now + 5000.0
        && now - timestamp <= condition.maximum_age_seconds * 1000.0
}

pub fn projection_changed(left: &IvyJson, right: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(left)? != hash_json(right)?)
}
