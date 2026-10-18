use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    service::{ConsumeEventsOptions, ServiceConnection, consume_events},
};
use std::{path::PathBuf, sync::Arc, time::Duration};

/// Discover the two authoritative objects. Their order is part of the durable
/// subscription identity, shared with the original manager.
pub async fn desired_subscription(
    connection: &dyn RpcClient,
    host_id: IvyJson,
) -> IvyResult<(IvyJson, IvyJson, IvyJson)> {
    let catalog = connection
        .request(
            "packages.catalog",
            IvyJson::object([("after", IvyJson::Number(0.0)), ("includeUis", true.into())]),
            RequestOptions::default(),
        )
        .await?;
    let configuration = connection
        .request(
            "hostConfigurations.get",
            IvyJson::object([("hostId", host_id)]),
            RequestOptions::default(),
        )
        .await?;
    let configuration_id = configuration.required("objectId")?.clone();
    let catalog_id = catalog.required("objectId")?.clone();
    let joined = format!("{}:{}", configuration_id.as_str()?, catalog_id.as_str()?);
    let digest = ivy_contracts::hash::digest(joined);
    let subscription = IvyJson::object([
        ("name", format!("host-desired-{}", &digest[7..31]).into()),
        (
            "filter",
            IvyJson::object([
                ("topics", IvyJson::Array(vec!["hive.object.changed".into()])),
                (
                    "objectIds",
                    IvyJson::Array(vec![configuration_id.clone(), catalog_id.clone()]),
                ),
            ]),
        ),
    ]);
    Ok((subscription, configuration_id, catalog_id))
}

pub async fn persist_hint(
    path: PathBuf,
    batch: &IvyJson,
    configuration_id: &IvyJson,
    catalog_id: &IvyJson,
    observed_at: &str,
) -> IvyResult<()> {
    if let Some(value) = hint(batch, configuration_id, catalog_id, observed_at)? {
        ivy_host_runtime::config::atomic_json(path, value).await?;
    }
    Ok(())
}

/// Start after reports have reconciled. The SDK acknowledges only after the
/// atomic write returns; a failed consumer retires just its own connection.
pub fn consume_desired(
    connection: ServiceConnection,
    runtime_root: PathBuf,
    subscription: IvyJson,
    configuration_id: IvyJson,
    catalog_id: IvyJson,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let persist = Arc::new(move |batch: IvyJson, _signal| {
            let path = runtime_root.join("desired-state-hint.json");
            let configuration_id = configuration_id.clone();
            let catalog_id = catalog_id.clone();
            Box::pin(async move {
                let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                persist_hint(path, &batch, &configuration_id, &catalog_id, &now).await
            }) as ivy_sdk::service::ServiceFuture<()>
        });
        if consume_events(
            &connection,
            subscription,
            persist,
            ConsumeEventsOptions {
                poll: Duration::from_secs(60),
                on_failure: None,
            },
        )
        .await
        .is_err()
            && !connection.signal().is_cancelled()
        {
            connection.close();
        }
    })
}

/// Construct the existing v1 advisory hint. The event consumer must atomically
/// persist this value before acknowledging its batch; None requires no write.
pub fn hint(
    batch: &IvyJson,
    configuration_id: &IvyJson,
    catalog_id: &IvyJson,
    observed_at: &str,
) -> IvyResult<Option<IvyJson>> {
    let Some(IvyJson::Array(items)) = batch.get("items") else {
        return Err(IvyError::new(
            "invalid_response",
            "Event batch has no items.",
        ));
    };
    let gap = !matches!(
        batch.get("gap"),
        None | Some(IvyJson::Null) | Some(IvyJson::Bool(false))
    );
    if !gap && items.is_empty() {
        return Ok(None);
    }
    let sequence = batch
        .get("throughSequence")
        .ok_or_else(|| IvyError::new("invalid_response", "Event batch has no sequence."))?;
    let contains = |id: &IvyJson| {
        items.iter().any(|item| {
            item.get("payload")
                .and_then(|payload| payload.get("objectId"))
                == Some(id)
        })
    };
    Ok(Some(IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("throughSequence", sequence.clone()),
        ("observedAt", observed_at.into()),
        ("configuration", (gap || contains(configuration_id)).into()),
        ("packages", (gap || contains(catalog_id)).into()),
    ])))
}
