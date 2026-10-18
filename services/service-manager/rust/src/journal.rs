use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{journal::HostJournal, node_path::to_native};
use ivy_sdk::service::ToolHandler;
use std::{collections::HashMap, sync::Arc};

/// Refresh from the currently accepted local configuration for every observation.
pub async fn executor(journal: &HostJournal, host_id: &IvyJson) -> IvyResult<Option<IvyJson>> {
    let configuration = journal.config().await?;
    let root = configuration
        .required("runtimeRoot")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    Ok(crate::health::refresh(
        &to_native(root),
        host_id,
        chrono::Utc::now().timestamp_millis(),
    )
    .await)
}

pub async fn status(journal: &HostJournal, host_id: &IvyJson) -> IvyResult<IvyJson> {
    journal.status(executor(journal, host_id).await?).await
}

/// Read handlers use the same journal as reporting and action acceptance.
pub fn read_handlers(journal: HostJournal, host_id: IvyJson) -> HashMap<JsString, ToolHandler> {
    let status_journal = journal.clone();
    let status: ToolHandler = Arc::new(move |_, _| {
        let journal = status_journal.clone();
        let host_id = host_id.clone();
        Box::pin(async move { self::status(&journal, &host_id).await })
    });
    let deployment: ToolHandler = Arc::new(move |args, _| {
        let journal = journal.clone();
        Box::pin(async move { journal.get(&args.required("deploymentId")?.as_str()?).await })
    });
    HashMap::from([
        ("host.status".into(), status),
        ("host.deployment".into(), deployment),
    ])
}
