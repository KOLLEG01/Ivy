//! Result migration over ordinary JSON records.
use super::{
    findings,
    value::{self, Result, Value, concat},
};
use ivy_contracts::IvyJson as Json;
use std::borrow::Cow;

fn content<'a>(content: Value<'a>) -> Result<Json> {
    let mut current = value::object_rest(&content, "checks")?;
    let checks = value::field(&content, "checks")?;
    if !value::has_items(&checks)? {
        return Ok(value::finish(current));
    }
    let summary = concat([
        value::text(&value::field(&content, "summary")?)?,
        "\n\n".into(),
        findings::text_in(&checks)?,
    ]);
    let mut artifacts = value::array_items(&value::field(&content, "artifacts")?)?;
    artifacts.extend(findings::evidence_in(&checks)?);
    let artifacts = findings::dedup_by(artifacts, value::serialized)?;
    value::insert(&mut current, "summary", value::string_js(summary));
    value::insert(&mut current, "artifacts", value::array(artifacts));
    Ok(value::finish(current))
}

fn record<'a>(result: Value<'a>) -> Result<Json> {
    let mut current = value::copy_object(&result)?;
    value::insert(
        &mut current,
        "content",
        content(value::field(&result, "content")?)?,
    );
    Ok(value::finish(current))
}

pub(super) fn json_content(content_value: &Json) -> Result<Json> {
    content(Some(Cow::Borrowed(content_value)))
}
pub(super) fn json_record(result: &Json) -> Result<Json> {
    record(Some(Cow::Borrowed(result)))
}
