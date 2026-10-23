//! Configuration migration over ordinary JSON records.
use super::value::{self, Result, Value};
use ivy_contracts::IvyJson as Json;
use std::borrow::Cow;

fn upgrade<'a>(configuration: Value<'a>) -> Result<Option<Json>> {
    let defaults = value::field(&configuration, "defaults")?;
    let requirement = value::field(&defaults, "executionRequirement")?;
    if value::nullish(&requirement)
        || !value::is_text(&value::field(&requirement, "kind")?, "capability")
    {
        return Ok(None);
    }
    let mut upgraded_defaults = value::copy_object(&defaults)?;
    value::insert(&mut upgraded_defaults, "executionRequirement", automatic());
    let mut upgraded = value::copy_object(&configuration)?;
    value::insert(&mut upgraded, "defaults", value::finish(upgraded_defaults));
    Ok(Some(value::finish(upgraded)))
}

fn automatic() -> Json {
    let mut value = value::empty();
    value::insert(&mut value, "kind", value::string("automatic"));
    value::finish(value)
}
pub(super) fn requirement_json(value: Option<&Json>) -> Option<Cow<'_, Json>> {
    value.map(|value| {
        if value.get("kind").and_then(Json::as_js_string) == Some(&"capability".into()) {
            Cow::Owned(automatic())
        } else {
            Cow::Borrowed(value)
        }
    })
}

pub(super) fn json(configuration: &Json) -> Result<Cow<'_, Json>> {
    upgrade(Some(Cow::Borrowed(configuration))).map(|changed| match changed {
        Some(value) => Cow::Owned(value),
        None => Cow::Borrowed(configuration),
    })
}
