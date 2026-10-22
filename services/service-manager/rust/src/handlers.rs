//! The five mutations share the CLI's typed, durable acceptance implementation.
use crate::configuration::Reload;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use ivy_host_runtime::{
    actions::{Action, LocalAction, accept_local_action},
    journal::HostJournal,
};
use ivy_sdk::service::ToolHandler;
use std::{collections::HashMap, path::PathBuf, sync::Arc};

pub fn mutations(reload: Reload, bootstrap_root: PathBuf) -> HashMap<JsString, ToolHandler> {
    [Action::Deploy, Action::Rollback, Action::Restart, Action::Enable, Action::Disable]
        .into_iter().map(|action| {
            let reload = reload.clone();
            let bootstrap_root = bootstrap_root.clone();
            let handler: ToolHandler = Arc::new(move |args, context| {
                let reload = reload.clone();
                let bootstrap_root = bootstrap_root.clone();
                Box::pin(async move {
                    // Operation binding precedes configuration IO, as in the
                    // original manager. Cancellation after admission cannot
                    // discard capture ownership or skip request-journal cleanup.
                    let action = LocalAction::from_manager(action, &args, context.operation_id.as_ref())?;
                    let configuration = reload().await?;
                    tokio::spawn(async move {
                        let journal = HostJournal::open(configuration).await?;
                        let accepted = accept_local_action(journal.clone(), action, bootstrap_root).await;
                        let closed = journal.close().await;
                        match accepted {
                            Ok(entry) => {
                                closed?;
                                record(entry)
                            }
                            Err(mut error) => {
                                error.message = "Local deployment acceptance failed; inspect the exact host request.".into();
                                error.wire_message = None;
                                error.details = None;
                                Err(error)
                            }
                        }
                    }).await.map_err(|_| IvyError::internal())?
                })
            });
            (format!("host.{}", action.as_str()).into(), handler)
        }).collect()
}
fn record(entry: IvyJson) -> IvyResult<IvyJson> {
    entry
        .get("record")
        .filter(|record| record.get("deploymentId").is_some())
        .cloned()
        .ok_or_else(|| {
            IvyError::new(
                "outcome_unknown",
                "Local command did not return a durable deployment identity.",
            )
            .with_outcome(Outcome::Unknown)
        })
}
