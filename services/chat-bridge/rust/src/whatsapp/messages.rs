//! Instance-local message selection with typed text and number parameters.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, json::parse};
use std::sync::OnceLock;

fn dictionaries() -> IvyResult<&'static Json> {
    static VALUE: OnceLock<IvyResult<Json>> = OnceLock::new();
    VALUE
        .get_or_init(|| parse(include_str!("messages.json"), usize::MAX))
        .as_ref()
        .map_err(Clone::clone)
}

#[derive(Clone, Copy, Default)]
pub struct Translator {
    german: bool,
}

impl Translator {
    pub fn new(language: Option<&JsString>) -> Self {
        Self {
            german: language == Some(&JsString::from("de")),
        }
    }

    /// Keys and string/number parameters belong to the service's message catalog.
    pub fn translate(&self, key: &str, parameters: Option<&Json>) -> IvyResult<JsString> {
        let dictionaries = dictionaries()?;
        let english = dictionaries.required("english")?.get(key);
        let selected = if self.german {
            dictionaries
                .required("german")?
                .get(key)
                .filter(|value| !value.is_null())
                .or(english)
        } else {
            english
        };
        let template = selected.ok_or_else(IvyError::internal)?.as_str()?;
        let mut result = Vec::with_capacity(template.len());
        for segment in template.split_inclusive('}') {
            if let Some(open) = segment.rfind('{').filter(|_| segment.ends_with('}')) {
                let name = &segment[open + 1..segment.len() - 1];
                if !name.is_empty() && name.bytes().all(|value| value.is_ascii_alphabetic()) {
                    if let Some(value) = parameters.and_then(|values| values.get(name)) {
                        result.extend(segment[..open].encode_utf16());
                        match value {
                            Json::String(value) => result.extend_from_slice(&value.0),
                            Json::Number(value) if value.is_finite() => {
                                result.extend(value.to_string().encode_utf16())
                            }
                            _ => return Err(IvyError::internal()),
                        }
                        continue;
                    }
                }
            }
            result.extend(segment.encode_utf16());
        }
        Ok(JsString(result))
    }
}
