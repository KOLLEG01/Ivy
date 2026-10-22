//! Explicit maintenance entrypoint; no engine/scheduler initialization or worker.
use crate::{
    config::startup_settings,
    engine::validate_settings,
    import_state,
    journal::Limits,
    registry::service_registry,
    retained_import::{ImportContext, step},
    service_handlers::PublicMethod,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::config::native_instance_config;
use ivy_sdk::{
    clock::{Clock, SystemClock},
    rpc::RequestOptions,
    service::{ServiceClient, ServiceOptions, ToolHandler},
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::mpsc;

pub async fn run(
    path: PathBuf,
    operation: JsString,
    cancellation: RequestOptions,
) -> IvyResult<IvyJson> {
    let config = native_instance_config(path).await?;
    let value = config.json();
    let settings = startup_settings(value)?.clone();
    validate_settings(&settings)?;
    let service_node_id = value
        .required("serviceNodeId")?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)?;
    let identity = import_state::import_identity(&settings, &service_node_id)?;
    let prepared = import_state::prepare(
        &config.data_root().join("secretary.sqlite"),
        Limits::default(),
        &identity,
        &operation,
        SystemClock.utc_millis(),
    )?;
    let Some(mut state) = prepared.state else {
        return Ok(IvyJson::object([
            ("complete", true.into()),
            ("alreadyCurrent", true.into()),
        ]));
    };
    let name = state
        .required("source")?
        .required("manifest")?
        .required("filename")?
        .as_str()?;
    let source = import_state::open_source(&config.data_root().join(&name))?;
    let retained = crate::service::retained_native_contracts(config.artifact_root());
    let registry = service_registry(
        settings
            .required("identity")?
            .required("scope")?
            .required("rootObjectId")?,
    )?;
    let credential = value.required("credential")?.as_str()?;
    let (connections, mut pending) = mpsc::unbounded_channel();
    let mut options = ServiceOptions::new(
        value.required("publicBaseUrl")?.as_str()?,
        Arc::new(move || {
            let credential = credential.clone();
            Box::pin(async move { Ok(credential) })
        }),
        crate::service::service_identity(value)?,
        Arc::new(move || {
            let registry = registry.clone();
            Box::pin(async move { Ok(registry) })
        }),
        Arc::new(move |connection| {
            let sent = connections
                .send(connection)
                .map_err(|_| IvyError::internal());
            Box::pin(async move { sent })
        }),
    );
    // Keep the same declared API while maintenance refuses every invocation.
    // The SDK validates concrete bindings even when readiness remains false.
    options.handlers = PublicMethod::ALL
        .into_iter()
        .map(|method| {
            let handler: ToolHandler = Arc::new(|_, _| {
                Box::pin(async {
                    Err(IvyError::new(
                        "secretary_import_required",
                        "Secretary is completing its explicit retained-data import.",
                    ))
                })
            });
            (format!("secretary.{}", method.name()).into(), handler)
        })
        .collect();
    options.readiness = Some(Arc::new(|_| {
        Box::pin(async {
            Ok(IvyJson::object([
                ("ready", false.into()),
                ("diagnostics", IvyJson::Array(vec![])),
            ]))
        })
    }));
    let service = ServiceClient::new(options)?;
    service.start();
    let result = async {
        let connection = tokio::select! {
            _ = cancellation.cancellation.cancelled() => return Err(cancellation.cancel_error.clone()),
            connection = tokio::time::timeout(Duration::from_secs(35), pending.recv()) => {
                connection.map_err(|_| IvyError::new("service_unavailable", "The import connection did not become available."))?.ok_or_else(IvyError::internal)?
            }
        };
        let ctx = ImportContext {
            journal: &prepared.journal,
            source: &source,
            settings: &settings,
            service_node_id: &service_node_id,
            client: Arc::new(connection.clone()),
            retained: &retained,
            cancellation: cancellation.clone(),
            page_size: 100,
        };
        loop {
            connection.check_open()?;
            let progress = step(&ctx, &mut state).await?;
            if progress.complete || !progress.advanced {
                return Ok(IvyJson::object([
                    ("complete", progress.complete.into()),
                    ("stage", progress.stage.into()),
                    ("operationId", IvyJson::String(operation.clone())),
                    ("source", state.required("source")?.clone()),
                ]));
            }
        }
    }.await;
    service.stop().await;
    result
}
