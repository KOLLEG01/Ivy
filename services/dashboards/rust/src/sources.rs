//! Fresh dashboard source reads. Hive remains the data store; no local cache.
use futures_util::{StreamExt, stream::FuturesUnordered};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone)]
pub enum Source {
    Object(JsString),
    Objects(Vec<JsString>),
    Contract { key: JsString, limit: u32 },
}

async fn read(
    client: &dyn RpcClient,
    id: JsString,
    revision: Option<f64>,
    options: RequestOptions,
) -> IvyResult<IvyJson> {
    let mut params = IvyJson::object([("objectId", IvyJson::String(id))]);
    if let Some(revision) = revision {
        params.insert("revision", IvyJson::Number(revision));
    }
    let result = client.request("objects.read", params, options).await?;
    let object = result.required("object")?;
    if object.get("effectivelyArchived").and_then(IvyJson::as_bool) == Some(true) {
        return Err(IvyError::new(
            "not_found",
            "A dashboard source is archived.",
        ));
    }
    let contract = object.required("contractKey")?.as_js_string();
    if contract == Some(&JsString::from("ivy/host-configuration")) {
        return Err(IvyError::new(
            "forbidden",
            "System configuration is not a dashboard source.",
        ));
    }
    let content = result.required("content")?;
    let encoding = content.required("encoding")?.as_str()?;
    if encoding != "json" && encoding != "text" {
        return Err(IvyError::new(
            "invalid_arguments",
            "Dashboard sources must be JSON or text.",
        ));
    }
    let value = content.required("value")?;
    if contract == Some(&JsString::from("dashboards/dashboard")) && encoding == "json" {
        let mut value = value.clone();
        value.remove("imageToken");
        return Ok(value);
    }
    Ok(value.clone())
}

async fn selectors(
    client: &dyn RpcClient,
    source: Source,
    options: RequestOptions,
) -> IvyResult<Vec<(JsString, Option<f64>)>> {
    match source {
        Source::Object(id) => Ok(vec![(id, None)]),
        Source::Objects(ids) => Ok(ids.into_iter().map(|id| (id, None)).collect()),
        Source::Contract { key, limit } => {
            let page = client
                .request(
                    "objects.query",
                    IvyJson::object([
                        ("contractKey", IvyJson::String(key)),
                        ("limit", IvyJson::Number(f64::from(limit))),
                    ]),
                    options.clone(),
                )
                .await?;
            let mut values = Vec::new();
            // A contract selector deliberately selects one bounded, ordered page.
            for item in page
                .required("items")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                let id = item
                    .required("objectId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?
                    .clone();
                values.push((id, item.get("revision").and_then(IvyJson::as_f64)));
            }
            Ok(values)
        }
    }
}

/// Expand selectors first, then read each selected object/revision once. Futures
/// belong to this request; dropping it cancels pending reads instead of detaching
/// tasks. Contract order and named-source errors remain observable.
pub async fn snapshot(
    client: Arc<dyn RpcClient>,
    sources: &[(JsString, Source)],
    options: RequestOptions,
) -> IvyResult<IvyJson> {
    let mut pending = FuturesUnordered::new();
    for (index, (_, selector)) in sources.iter().enumerate() {
        let client = client.clone();
        let selector = selector.clone();
        let options = options.clone();
        pending.push(async move { (index, selectors(client.as_ref(), selector, options).await) });
    }
    let mut selected = vec![vec![]; sources.len()];
    while let Some((index, value)) = pending.next().await {
        selected[index] = value?;
    }
    let revisions: HashMap<_, _> = selected
        .iter()
        .flatten()
        .filter_map(|(id, rev)| rev.map(|r| (id.clone(), r)))
        .collect();
    let mut unique = HashMap::new();
    for (id, revision) in selected.iter_mut().flatten() {
        if revision.is_none() {
            *revision = revisions.get(id).copied();
        }
        unique.insert((id.clone(), revision.map(f64::to_bits)), IvyJson::Null);
    }
    let keys: Vec<_> = unique.keys().cloned().collect();
    let mut reads = futures_util::stream::iter(keys.into_iter().map(|key| {
        let client = client.clone();
        let options = options.clone();
        async move {
            let value = read(
                client.as_ref(),
                key.0.clone(),
                key.1.map(f64::from_bits),
                options,
            )
            .await;
            (key, value)
        }
    }))
    .buffer_unordered(16);
    while let Some((key, value)) = reads.next().await {
        unique.insert(key, value?);
    }
    let values = selected.into_iter().zip(sources).map(|(ids, (_, source))| {
        let mut values: Vec<_> = ids
            .into_iter()
            .map(|(id, rev)| unique[&(id, rev.map(f64::to_bits))].clone())
            .collect();
        match source {
            Source::Object(_) => values.remove(0),
            _ => IvyJson::Array(values),
        }
    });
    Ok(IvyJson::Object(
        sources
            .iter()
            .map(|(name, _)| name.clone())
            .zip(values)
            .collect(),
    ))
}
