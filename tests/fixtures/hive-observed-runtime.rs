//! Finite original observations for tests only; missing observations fail.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_hive::retention::Runtime;
use std::{cmp::Ordering, collections::HashMap};
fn units(value: &IvyJson) -> JsString {
    JsString(
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as u16)
            .collect(),
    )
}
pub struct ObservedRuntime {
    dates: HashMap<JsString, f64>,
    iso: HashMap<i64, JsString>,
    locale: HashMap<JsString, i64>,
    lowercase: HashMap<JsString, JsString>,
    discovery_lowercase: HashMap<JsString, JsString>,
    word_characters: HashMap<u32, bool>,
    english: HashMap<JsString, i64>,
    uppercase: HashMap<JsString, JsString>,
}
impl ObservedRuntime {
    pub fn from_json(profile: &IvyJson) -> Self {
        let empty = IvyJson::Null;
        let discovery = profile.get("discovery").unwrap_or(&empty);
        Self {
            dates: profile
                .required("dates")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|v| {
                    let v = v.as_array().unwrap();
                    (units(&v[0]), v[1].as_f64().unwrap_or(f64::NAN))
                })
                .collect(),
            iso: profile
                .required("iso")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|v| {
                    let v = v.as_array().unwrap();
                    (
                        v[0].as_f64().unwrap() as i64,
                        v[1].as_js_string().unwrap().clone(),
                    )
                })
                .collect(),
            locale: profile
                .required("localeOrder")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|v| {
                    let v = v.as_array().unwrap();
                    (units(&v[0]), v[1].as_f64().unwrap() as i64)
                })
                .collect(),
            lowercase: profile
                .get("lowercase")
                .and_then(IvyJson::as_array)
                .map(|v| {
                    v.iter()
                        .map(|v| {
                            let v = v.as_array().unwrap();
                            (units(&v[0]), units(&v[1]))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            discovery_lowercase: discovery
                .get("lowercase")
                .and_then(IvyJson::as_array)
                .map(|v| {
                    v.iter()
                        .map(|v| {
                            let v = v.as_array().unwrap();
                            (units(&v[0]), units(&v[1]))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            word_characters: discovery
                .get("wordCharacters")
                .and_then(IvyJson::as_array)
                .map(|v| {
                    v.iter()
                        .map(|v| {
                            let v = v.as_array().unwrap();
                            (v[0].as_f64().unwrap() as u32, v[1].as_bool().unwrap())
                        })
                        .collect()
                })
                .unwrap_or_default(),
            english: discovery
                .get("englishOrder")
                .and_then(IvyJson::as_array)
                .map(|v| {
                    v.iter()
                        .map(|v| {
                            let v = v.as_array().unwrap();
                            (units(&v[0]), v[1].as_f64().unwrap() as i64)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            uppercase: discovery
                .get("uppercase")
                .and_then(IvyJson::as_array)
                .map(|v| {
                    v.iter()
                        .map(|v| {
                            let v = v.as_array().unwrap();
                            (units(&v[0]), units(&v[1]))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}
impl ivy_hive::mcp::Runtime for ObservedRuntime {
    fn uppercase(&self, value: &JsString) -> IvyResult<JsString> {
        Ok(self
            .uppercase
            .get(value)
            .unwrap_or_else(|| panic!("Unobserved MCP uppercase {value:?}"))
            .clone())
    }
}
impl ivy_hive::discovery::Runtime for ObservedRuntime {
    fn lowercase(&self, value: &JsString) -> IvyResult<JsString> {
        Ok(self
            .discovery_lowercase
            .get(value)
            .unwrap_or_else(|| panic!("Unobserved discovery lowercase {value:?}"))
            .clone())
    }
    fn word_character(&self, code: u32) -> bool {
        *self
            .word_characters
            .get(&code)
            .unwrap_or_else(|| panic!("Unobserved Unicode word character {code:x}"))
    }
    fn compare_english(&self, a: &JsString, b: &JsString) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        self.english
            .get(a)
            .unwrap_or_else(|| panic!("Unobserved English collation {a:?}"))
            .cmp(
                self.english
                    .get(b)
                    .unwrap_or_else(|| panic!("Unobserved English collation {b:?}")),
            )
    }
}
impl ivy_hive::store::QueryRuntime for ObservedRuntime {
    fn parse_date(&self, value: &JsString) -> f64 {
        Runtime::parse_date(self, value)
    }
    fn locale_lowercase(&self, value: &JsString) -> IvyResult<JsString> {
        Ok(self
            .lowercase
            .get(value)
            .unwrap_or_else(|| panic!("Unobserved locale lowercase {value:?}"))
            .clone())
    }
}
impl Runtime for ObservedRuntime {
    fn parse_date(&self, value: &JsString) -> f64 {
        *self
            .dates
            .get(value)
            .unwrap_or_else(|| panic!("Unobserved registry Date.parse {value:?}"))
    }
    fn iso_date(&self, ms: f64) -> IvyResult<JsString> {
        if !ms.is_finite() || ms.abs() > 8.64e15 {
            return Err(IvyError::internal());
        }
        self.iso
            .get(&(ms.trunc() as i64))
            .cloned()
            .ok_or_else(IvyError::internal)
    }
    fn compare_contract_keys(&self, a: &JsString, b: &JsString) -> Ordering {
        self.locale
            .get(a)
            .unwrap_or_else(|| panic!("Unobserved locale string {a:?}"))
            .cmp(
                self.locale
                    .get(b)
                    .unwrap_or_else(|| panic!("Unobserved locale string {b:?}")),
            )
    }
}
