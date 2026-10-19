//! Instance-local message selection and callback-style interpolation.
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{js_string, parse},
};
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
        let template = selected
            .and_then(Json::as_js_string)
            .ok_or_else(IvyError::internal)?;
        let mut result = Vec::with_capacity(template.0.len());
        let mut offset = 0;
        while offset < template.0.len() {
            if template.0[offset] == u16::from(b'{') {
                let start = offset + 1;
                let mut end = start;
                while end < template.0.len() && matches!(template.0[end], 65..=90 | 97..=122) {
                    end += 1;
                }
                if end > start && template.0.get(end) == Some(&u16::from(b'}')) {
                    let name = JsString(template.0[start..end].to_vec());
                    let value = match parameters {
                        Some(Json::Null) => return Err(IvyError::internal()),
                        Some(parameters) => parameters.get_key(&name),
                        None => None,
                    };
                    if let Some(value) = value {
                        result.extend(js_string(value).0);
                    } else {
                        result.extend_from_slice(&template.0[offset..=end]);
                    }
                    offset = end + 1;
                    continue;
                }
            }
            result.push(template.0[offset]);
            offset += 1;
        }
        Ok(JsString(result))
    }
}
