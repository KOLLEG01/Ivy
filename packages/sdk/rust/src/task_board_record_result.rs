//! Result migration over ordinary JSON records.
use super::{
    findings,
    value::{self, Result, concat},
};
use ivy_contracts::{IvyJson as Json, json};

pub(super) fn json_content(mut content: Json) -> Result<Json> {
    content.as_object().ok_or_else(value::invalid)?;
    let checks = value::field(&value::borrow(&content), "checks")?;
    if !value::has_items(&checks)? {
        content.remove("checks");
        return Ok(content);
    }
    let summary = concat([
        value::text(&value::field(&value::borrow(&content), "summary")?)?,
        "\n\n".into(),
        findings::text_in(&checks)?,
    ]);
    content
        .get("artifacts")
        .and_then(Json::as_array)
        .ok_or_else(value::invalid)?;
    let evidence = findings::evidence_in(&checks)?
        .into_iter()
        .map(value::retain)
        .collect::<Vec<_>>();
    let mut artifacts = std::mem::replace(content.get_mut("artifacts").unwrap(), Json::Null)
        .into_array()
        .unwrap();
    artifacts.extend(evidence);
    let artifacts = findings::dedup_by(artifacts, |value| {
        json::stringify(value, usize::MAX).map(Some)
    })?;
    content.remove("checks");
    content.insert("summary", Json::String(summary));
    content.insert("artifacts", Json::Array(artifacts));
    Ok(content)
}

pub(super) fn json_record(mut result: Json) -> Result<Json> {
    result.as_object().ok_or_else(value::invalid)?;
    let content = result.get_mut("content").ok_or_else(value::invalid)?;
    *content = json_content(std::mem::replace(content, Json::Null))?;
    Ok(result)
}
