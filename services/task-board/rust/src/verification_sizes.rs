//! Backfill retained evidence sizes using its immutable Hive revision.
use ivy_contracts::{IvyError, IvyJson as Json};
use std::future::Future;

fn property_mut<'a>(value: &'a mut Json, name: &str) -> Option<&'a mut Json> {
    let Json::Object(fields) = value else {
        return None;
    };
    let name = ivy_contracts::JsString::from(name);
    fields
        .iter_mut()
        .find(|(key, _)| key == &name)
        .map(|(_, value)| value)
}
fn array_mut(value: Option<&mut Json>) -> Result<&mut [Json], IvyError> {
    match value {
        None | Some(Json::Null) => Ok(&mut []),
        Some(Json::Array(values)) => Ok(values),
        _ => Err(IvyError::new(
            "invalid_arguments",
            "Retained evidence collections must be arrays.",
        )),
    }
}
async fn fill_artifacts<E, F, Fut>(artifacts: &mut [Json], read: &mut F) -> Result<(), E>
where
    E: From<IvyError>,
    F: FnMut(Option<Json>) -> Fut,
    Fut: Future<Output = Result<Json, E>>,
{
    for artifact in artifacts {
        if artifact.get("byteLength").is_some() {
            continue;
        }
        let saved = read(Some(artifact.required("object")?.clone())).await?;
        let revision = saved.required("revision")?;
        if revision.required("contentHash")? != artifact.required("contentHash")?
            || revision.required("mediaType")? != artifact.required("mediaType")?
        {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "Retained verification evidence differs from its immutable pin.",
            )
            .into());
        }
        artifact.insert("byteLength", revision.required("byteLength")?.clone());
    }
    Ok(())
}
async fn fill_findings<E, F, Fut>(findings: Option<&mut Json>, read: &mut F) -> Result<(), E>
where
    E: From<IvyError>,
    F: FnMut(Option<Json>) -> Fut,
    Fut: Future<Output = Result<Json, E>>,
{
    for finding in array_mut(findings)? {
        fill_artifacts(array_mut(property_mut(finding, "evidence"))?, read).await?;
    }
    Ok(())
}
pub async fn fill_verification_sizes_with<E, F, Fut>(
    key: &str,
    value: &Json,
    mut read: F,
) -> Result<Json, E>
where
    E: From<IvyError>,
    F: FnMut(Option<Json>) -> Fut,
    Fut: Future<Output = Result<Json, E>>,
{
    if key != "task-board/task" {
        return Ok(value.clone());
    }
    let mut stored = value.clone();
    fill_findings(property_mut(&mut stored, "latestChecks"), &mut read).await?;
    if let Some(content) = property_mut(&mut stored, "content") {
        fill_findings(property_mut(content, "checks"), &mut read).await?;
    }
    for comment in array_mut(property_mut(&mut stored, "comments"))? {
        if let Some(handoff) = property_mut(comment, "handoff") {
            fill_findings(property_mut(handoff, "checks"), &mut read).await?;
        }
    }
    for receipt in array_mut(property_mut(&mut stored, "deliveries"))? {
        if let Some(delivery) = property_mut(receipt, "delivery") {
            fill_artifacts(array_mut(property_mut(delivery, "artifacts"))?, &mut read).await?;
        }
    }
    Ok(stored)
}
