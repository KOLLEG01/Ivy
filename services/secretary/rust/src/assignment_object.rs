//! Object event projections and freshness policy using shared Date.parse semantics.
use ivy_contracts::{IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::clock::parse_date;

pub const OBJECT_WINDOW_KEY: &str = "secretary/object-window";

pub fn pointer(value: &IvyJson, path: &JsString) -> IvyJson {
    if path.0.is_empty() {
        return value.clone();
    }
    let mut current = value;
    let mut tokens = path.0[1..].split(|c| *c == 47).peekable();
    while let Some(part) = tokens.next() {
        let mut token = Vec::with_capacity(part.len());
        let mut index = 0;
        while index < part.len() {
            if part[index] == 126 && matches!(part.get(index + 1), Some(48 | 49)) {
                token.push(if part[index + 1] == 49 { 47 } else { 126 });
                index += 2;
            } else {
                token.push(part[index]);
                index += 1;
            }
        }
        let token = JsString(token);
        current = match current {
            IvyJson::Object(_) => match current.get_key(&token) {
                Some(value) => value,
                None => return IvyJson::Null,
            },
            IvyJson::Array(values) => {
                // Object.hasOwn on an array also exposes its own length property.
                if token == JsString::from("length") {
                    return if tokens.peek().is_none() {
                        IvyJson::Number(values.len() as f64)
                    } else {
                        IvyJson::Null
                    };
                }
                let index = if token.0.is_empty() || (token.0.len() > 1 && token.0[0] == 48) {
                    None
                } else {
                    token.0.iter().try_fold(0usize, |n, c| {
                        if (48..=57).contains(c) {
                            n.checked_mul(10)?.checked_add(usize::from(*c - 48))
                        } else {
                            None
                        }
                    })
                };
                match index.and_then(|index| values.get(index)) {
                    Some(value) => value,
                    None => return IvyJson::Null,
                }
            }
            _ => return IvyJson::Null,
        };
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
