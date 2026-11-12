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
            "objects.query",
            IvyJson::object([
                ("contractKey", "ivy/package-catalog".into()),
                ("select", IvyJson::Array(vec!["object.id".into()])),
                ("limit", 2.0.into()),
            ]),
            RequestOptions::default(),
        )
        .await?;
    let configuration = connection
        .request(
            "objects.query",
            IvyJson::object([
                ("contractKey", "ivy/host-configuration".into()),
                ("where", IvyJson::object([("op", "eq".into()), ("field", "data:/hostId".into()), ("value", host_id)])),
                ("select", IvyJson::Array(vec!["object.id".into()])),
                ("limit", 2.0.into()),
            ]),
            RequestOptions::default(),
        )
        .await?;
    let configuration_id = single_object_id(&configuration)?;
    let catalog_id = single_object_id(&catalog)?;
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
fn single_object_id(page: &IvyJson) -> IvyResult<IvyJson> {
    let items = page.required("items")?.as_array().ok_or_else(IvyError::internal)?;
    if items.len() != 1 || page.get("nextCursor").is_some_and(|v| !v.is_null()) {
        return Err(IvyError::new("invalid_response", "Desired state must identify one authoritative object."));
    }
    Ok(items[0].required("objectId")?.clone())
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

/// The subscription is advisory and recovers independently of host tools.
/// HostExecutor's periodic authoritative read also catches missed hints.
pub fn consume_desired(
    connection: ServiceConnection,
    runtime_root: PathBuf,
    host_id: IvyJson,
    admission: crate::connection::Admission,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _ = admission.run(Box::pin(async move {
            let signal = connection.signal();
            while !signal.is_cancelled() {
                let attempt = async {
                    let (subscription, configuration_id, catalog_id) =
                        desired_subscription(&connection, host_id.clone()).await?;
                    let runtime_root = runtime_root.clone();
                    let persist = Arc::new(move |batch: IvyJson, _signal| {
                        let path = runtime_root.join("desired-state-hint.json");
                        let configuration_id = configuration_id.clone();
                        let catalog_id = catalog_id.clone();
                        Box::pin(async move {
                            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                            persist_hint(path, &batch, &configuration_id, &catalog_id, &now).await
                        }) as ivy_sdk::service::ServiceFuture<()>
                    });
                    consume_events(
                        &connection,
                        subscription,
                        persist,
                        ConsumeEventsOptions {
                            poll: Duration::from_secs(60),
                            on_failure: None,
                        },
                    ).await
                }.await;
                if attempt.is_ok() || signal.is_cancelled() { break; }
                tokio::select! {
                    _ = signal.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => (),
                }
            }
            Ok(())
        })).await;
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
