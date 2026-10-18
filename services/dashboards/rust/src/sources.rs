//! Fresh dashboard source reads. Hive remains the data store; no local cache.
use futures_util::{StreamExt, stream::FuturesUnordered};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
use std::sync::Arc;

#[derive(Clone)]
pub enum Source {
    Object(JsString),
    Contract { key: JsString, limit: u32 },
}

async fn read(client: &dyn RpcClient, id: JsString, options: RequestOptions) -> IvyResult<IvyJson> {
    let result = client
        .request(
            "objects.read",
            IvyJson::object([("objectId", IvyJson::String(id))]),
            options,
        )
        .await?;
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

async fn source(
    client: &dyn RpcClient,
    source: Source,
    options: RequestOptions,
) -> IvyResult<IvyJson> {
    match source {
        Source::Object(id) => read(client, id, options).await,
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
            // Deliberately first page only, with ordered sequential reads.
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
                values.push(read(client, id, options.clone()).await?);
            }
            Ok(IvyJson::Array(values))
        }
    }
}

/// Names/selectors come from the validated dashboard definition. Start each
/// named source concurrently, retain definition order, and reject on the first
/// failure. Accepted reads keep running if the observer is dropped, matching
/// Promise.all without replaying them or sharing their results with later calls.
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
        pending.push(tokio::spawn(async move {
            (index, source(client.as_ref(), selector, options).await)
        }));
    }
    let mut values = vec![IvyJson::Null; sources.len()];
    while let Some(result) = pending.next().await {
        let (index, value) = result.map_err(|_| IvyError::internal())?;
        values[index] = value?;
    }
    Ok(IvyJson::Object(
        sources
            .iter()
            .map(|(name, _)| name.clone())
            .zip(values)
            .collect(),
    ))
}
