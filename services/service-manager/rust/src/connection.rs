use crate::{configuration::Reload, health::HealthReporter, reports::Reports};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, core_schema::validate_host};
use ivy_host_runtime::{
    config::InstanceConfig,
    journal::HostJournal,
    node_path::{Style, is_absolute, to_native},
};
use ivy_sdk::service::{ServiceOptions, ToolHandler};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Default)]
pub struct Admission(Arc<AtomicBool>);
impl Admission {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn check(&self) -> IvyResult<()> {
        if self.0.load(Ordering::Acquire) {
            Err(IvyError::new(
                "service_unavailable",
                "ServiceManager is stopping.",
            ))
        } else {
            Ok(())
        }
    }
}

pub(crate) fn validate_config(config: &InstanceConfig) -> IvyResult<()> {
    let value = config.json();
    validate_host("ManagerSettings", value.required("settings")?)?;
    if value.get("componentId") != Some(&IvyJson::from("service-manager"))
        || !value
            .get("credential")
            .is_some_and(|v| v.as_js_string().is_some_and(|v| !v.0.is_empty()))
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "ServiceManager requires its own component identity and Hive credential.",
        ));
    }
    if !is_absolute(
        Style::native(),
        value
            .required("settings")?
            .required("hostConfigPath")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?,
    ) {
        return Err(IvyError::new(
            "invalid_arguments",
            "ServiceManager needs an explicit absolute local host configuration.",
        ));
    }
    Ok(())
}
/// Construct real SDK reconciliation, readiness and tool callbacks. Host startup
/// validation and the shared typed action handlers must be supplied before start.
pub fn options(
    config: &InstanceConfig,
    journal: HostJournal,
    health: HealthReporter,
    mut mutations: HashMap<JsString, ToolHandler>,
    admission: Admission,
    reload: Reload,
) -> IvyResult<ServiceOptions> {
    validate_config(config)?;
    let value = config.json();
    let host_id = value.required("hostId")?.clone();
    let mut handlers = crate::journal::read_handlers(journal.clone(), host_id.clone());
    for name in [
        "host.deploy",
        "host.rollback",
        "host.restart",
        "host.enable",
        "host.disable",
    ] {
        let handler = mutations.remove(&JsString::from(name)).ok_or_else(|| {
            IvyError::new(
                "invalid_arguments",
                "ServiceManager requires the shared host action handlers.",
            )
        })?;
        handlers.insert(name.into(), handler);
    }
    if !mutations.is_empty() {
        return Err(IvyError::new(
            "invalid_arguments",
            "ServiceManager action handlers do not match its registry.",
        ));
    }
    for handler in handlers.values_mut() {
        let inner = handler.clone();
        let admission = admission.clone();
        *handler = Arc::new(move |args, context| {
            let inner = inner.clone();
            let admission = admission.clone();
            Box::pin(async move {
                admission.check()?;
                inner(args, context).await
            })
        });
    }
    let reports = Arc::new(tokio::sync::Mutex::new(Reports::default()));
    let reconcile = {
        let journal = journal.clone();
        let host_id = host_id.clone();
        let reports = reports.clone();
        let admission = admission.clone();
        let reload = reload.clone();
        Arc::new(move |connection: ivy_sdk::service::ServiceConnection| {
            let journal = journal.clone();
            let host_id = host_id.clone();
            let reports = reports.clone();
            let admission = admission.clone();
            let reload = reload.clone();
            Box::pin(async move {
                admission.check()?;
                reload().await?;
                {
                    let mut reports = reports.lock().await;
                    reports.reconcile_from(&connection, host_id.clone()).await?;
                    reports
                        .publish_journal(&connection, &journal, &host_id)
                        .await?;
                }
                let (subscription, configuration_id, catalog_id) =
                    crate::hints::desired_subscription(&connection, host_id).await?;
                let configuration = journal.config().await?;
                let runtime_root = to_native(
                    configuration
                        .required("runtimeRoot")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?,
                );
                admission.check()?;
                if connection.signal().is_cancelled() {
                    return Err(IvyError::new(
                        "service_unavailable",
                        "ServiceManager connection is closed.",
                    ));
                }
                drop(crate::hints::consume_desired(
                    connection,
                    runtime_root,
                    subscription,
                    configuration_id,
                    catalog_id,
                ));
                Ok(())
            }) as ivy_sdk::service::ServiceFuture<()>
        }) as ivy_sdk::service::ConnectionDependency<()>
    };
    let credential = value.required("credential")?.as_str()?;
    let identity = IvyJson::object([
        ("serviceNodeId", value.required("serviceNodeId")?.clone()),
        ("serviceName", "service-manager".into()),
        ("hostId", host_id.clone()),
        ("version", value.required("version")?.clone()),
        ("buildId", value.required("buildId")?.clone()),
        ("hiveProtocol", IvyJson::Number(1.0)),
    ]);
    let mut options = ServiceOptions::new(
        value.required("publicBaseUrl")?.as_str()?,
        Arc::new(move || {
            let credential = credential.clone();
            Box::pin(async move { Ok(credential) })
        }),
        identity,
        Arc::new(|| Box::pin(async { crate::registry::bundled_registry() })),
        reconcile,
    );
    options.handlers = handlers;
    options.heartbeat = Duration::from_secs(2);
    let service_node_id = value.required("serviceNodeId")?.clone();
    let readiness_health = health.clone();
    options.readiness = Some(Arc::new(move |connection| {
        let journal = journal.clone();
        let host_id = host_id.clone();
        let reports = reports.clone();
        let health = readiness_health.clone();
        let admission = admission.clone();
        let service_node_id = service_node_id.clone();
        let reload = reload.clone();
        Box::pin(async move {
            admission.check()?;
            reload().await?;
            let executor = reports
                .lock()
                .await
                .publish_journal(&connection, &journal, &host_id)
                .await?;
            admission.check()?;
            health
                .readiness(
                    executor.as_ref(),
                    host_id,
                    service_node_id,
                    connection.generation(),
                    chrono::Utc::now().timestamp_millis(),
                )
                .await
        })
    }));
    options.on_state = Some(Arc::new(move |state| health.state(state)));
    Ok(options)
}
