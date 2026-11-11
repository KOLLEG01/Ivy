//! One Hive connection, health/control loop and shutdown for the physical host.
use crate::{
    agent_health::AgentHealth,
    backend_routes::BackendFailure,
    backend_runtime::{BackendResources, Slot, get},
    control::AgentControl,
    service_kernel::now,
    shutdown::{AgentShutdown, ShutdownPhases},
};
use ivy_contracts::{IvyError, IvyJson, JsString};
use ivy_sdk::service::{ConnectionDependency, ServiceClient, ServiceFuture};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;

pub(crate) struct HostResources {
    pub(crate) backends: Mutex<Vec<Arc<BackendResources>>>,
    pub(crate) preparation_failures: Mutex<Vec<Arc<BackendFailure>>>,
    pub(crate) service: Slot<Arc<ServiceClient>>,
    pub(crate) health: AgentHealth,
    pub(crate) stop_timer: watch::Sender<bool>,
}
// Start every concrete cleanup before awaiting any, retaining the first settled
// failure while still draining all other backends.
fn settle(runs: Vec<ServiceFuture<()>>) -> ServiceFuture<()> {
    Box::pin(async move {
        let mut pending = tokio::task::JoinSet::new();
        for run in runs {
            pending.spawn(run);
        }
        let mut first = None;
        while let Some(result) = pending.join_next().await {
            if let Err(error) = result.map_err(|_| IvyError::internal()).and_then(|v| v) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    })
}
impl HostResources {
    pub(crate) fn backends(&self) -> Vec<Arc<BackendResources>> {
        self.backends
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn shutdown(self: &Arc<Self>, closing: Arc<AtomicBool>) -> AgentShutdown {
        let timer = self.clone();
        let admissions = self.clone();
        let native = self.clone();
        let work = self.clone();
        let service = self.clone();
        let locations = self.clone();
        let state = self.clone();
        let health = self.clone();
        let final_health = self.clone();
        let journal = self.clone();
        let environment_step = |kind| -> Arc<crate::shutdown::ShutdownStep> {
            let host = self.clone();
            Arc::new(move || {
                settle(
                    host.backends()
                        .iter()
                        .map(|owner| owner.environment_step(kind))
                        .collect(),
                )
            })
        };
        AgentShutdown::new(
            ShutdownPhases {
                stop_timer: Arc::new(move || {
                    timer.stop_timer.send_replace(true);
                }),
                admissions: Arc::new(move || {
                    settle(
                        admissions
                            .backends()
                            .iter()
                            .map(|owner| owner.retire_admission())
                            .collect(),
                    )
                }),
                native: Arc::new(move || {
                    let runs = native
                        .backends()
                        .iter()
                        .map(|owner| {
                            owner.closing.store(true, Ordering::Release);
                            let _ = owner.readiness.disconnect();
                            let native = get(&owner.native).map(|v| v.map(|owner| owner.close()));
                            Box::pin(async move {
                                if let Some(run) = native? {
                                    run.await?;
                                }
                                Ok(())
                            }) as ServiceFuture<()>
                        })
                        .collect();
                    settle(runs)
                }),
                work: Arc::new(move || {
                    settle(
                        work.backends()
                            .iter()
                            .map(|owner| {
                                let receipts = owner.receipts.clone();
                                let inventory = get(&owner.inventory);
                                let reconciliation = owner.readiness.drain();
                                Box::pin(async move {
                                    let (receipts, reconciliation, inventory) =
                                        tokio::join!(receipts.drain(), reconciliation, async {
                                            if let Some(inventory) = inventory? {
                                                inventory.drain().await?;
                                            }
                                            Ok::<_, IvyError>(())
                                        });
                                    // Settle every writer before forwarding an owner error.
                                    receipts.and(reconciliation).and(inventory)
                                }) as ServiceFuture<()>
                            })
                            .collect(),
                    )
                }),
                service: Arc::new(move || {
                    let service = get(&service.service);
                    Box::pin(async move {
                        if let Some(service) = service? {
                            service.stop().await;
                        }
                        Ok(())
                    })
                }),
                instructions: environment_step(0),
                mcp: environment_step(1),
                skills: environment_step(2),
                locations: Arc::new(move || {
                    let mut first = None;
                    for owner in locations.backends() {
                        if let Err(error) = get(&owner.locations)
                            .and_then(|location| location.map_or(Ok(()), |v| v.close()))
                        {
                            first.get_or_insert(error);
                        }
                    }
                    let result = first.map_or(Ok(()), Err);
                    Box::pin(async move { result })
                }),
                state: Arc::new(move || {
                    let owners = state.backends();
                    Box::pin(async move {
                        tokio::task::spawn_blocking(move || {
                            let mut first = None;
                            for owner in owners {
                                if let Err(error) = owner
                                    .state
                                    .lock()
                                    .map_err(|_| IvyError::internal())
                                    .and_then(|mut state| state.close_shared())
                                {
                                    first.get_or_insert(error);
                                }
                            }
                            first.map_or(Ok(()), Err)
                        })
                        .await
                        .map_err(|_| IvyError::internal())?
                    })
                }),
                pending_health: Arc::new(move || health.health.drain_snapshot()),
                final_health: Arc::new(move |code| {
                    let file = final_health.health.file().clone();
                    let mut message = JsString::from("AgentManager stopped: ");
                    message.0.extend(code.0);
                    message.0.push(46);
                    Box::pin(async move { file.write(false, message, None).await })
                }),
                journal: Arc::new(move || {
                    let mut first = None;
                    for owner in journal.backends() {
                        match owner.journal.lock() {
                            Ok(mut journal) => journal.close(),
                            Err(poisoned) => {
                                poisoned.into_inner().close();
                                first.get_or_insert_with(IvyError::internal);
                            }
                        }
                    }
                    first.map_or(Ok(()), Err)
                }),
            },
            closing,
        )
    }
    pub(crate) fn dependencies(
        self: &Arc<Self>,
        service_node: JsString,
    ) -> (ConnectionDependency<()>, ConnectionDependency<IvyJson>) {
        let reconcile = Arc::downgrade(self);
        let readiness = Arc::downgrade(self);
        (
            Arc::new(move |connection| {
                let host = reconcile.clone();
                Box::pin(async move {
                    let host = host.upgrade().ok_or_else(IvyError::internal)?;
                    let epoch = crate::reconciliation::native_transport_epoch(&connection).await?;
                    for backend in host.backends() {
                        backend
                            .readiness
                            .connect(connection.clone(), epoch.clone())?;
                    }
                    Ok(())
                })
            }),
            Arc::new(move |connection| {
                let host = readiness.clone();
                let service_node = service_node.clone();
                Box::pin(async move {
                    let host = host.upgrade().ok_or_else(IvyError::internal)?;
                    let mut ready = false;
                    let mut diagnostics = host
                        .preparation_failures
                        .lock()
                        .map_err(|_| IvyError::internal())?
                        .iter()
                        .map(|failure| failure.diagnostic())
                        .collect::<Vec<_>>();
                    for backend in host.backends() {
                        let observation = backend.readiness.observation()?;
                        let available = backend.readiness.ready();
                        ready |= available;
                        let observation = observation.unwrap_or_else(|| {
                            let mut snapshot = crate::reconciliation::readiness_snapshot(
                                &service_node,
                                false,
                                backend
                                    .failure
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .clone()
                                    .or(backend.readiness.code().unwrap_or(None))
                                    .or(Some("native_initializing".into())),
                                now(),
                            );
                            if let Some(IvyJson::Array(diagnostics)) =
                                snapshot.get_mut("diagnostics")
                            {
                                for diagnostic in diagnostics {
                                    diagnostic.insert(
                                        "message",
                                        "Native backend startup or reconciliation is unavailable."
                                            .into(),
                                    );
                                }
                            }
                            snapshot
                        });
                        if let Some(values) =
                            observation.get("diagnostics").and_then(IvyJson::as_array)
                        {
                            for mut diagnostic in values.to_vec() {
                                if let Some(identity) = &backend.backend {
                                    if let Some(resource) = diagnostic.get_mut("resource") {
                                        resource.insert("backend", identity.json());
                                    }
                                    let mut message =
                                        JsString::from(format!("{}: ", identity.id()));
                                    if let Some(text) =
                                        diagnostic.get("message").and_then(IvyJson::as_js_string)
                                    {
                                        message.0.extend_from_slice(&text.0);
                                    }
                                    diagnostic.insert("message", IvyJson::String(message));
                                }
                                diagnostics.push(diagnostic);
                            }
                        }
                    }
                    host.health
                        .write(
                            ready,
                            Some(connection.generation()),
                            if ready {
                                "Native backend available; other backends reconcile independently."
                            } else {
                                "No native backend is available."
                            }
                            .into(),
                        )?
                        .wait()
                        .await?;
                    Ok(IvyJson::object([
                        ("ready", ready.into()),
                        ("diagnostics", IvyJson::Array(diagnostics)),
                    ]))
                })
            }),
        )
    }
}

pub(crate) fn control_timer(
    health: AgentHealth,
    resources: Arc<HostResources>,
    mut stopped: watch::Receiver<bool>,
    shutdown: std::sync::Weak<AgentShutdown>,
    closing: Arc<AtomicBool>,
) {
    let file = health.file().clone();
    let mut events = ivy_host_runtime::file_events::FileEvents::new([file.control_path()]);
    let control = AgentControl::new(
        Arc::new(move || {
            let file = file.clone();
            Box::pin(async move { Ok(file.control().await?.is_some()) })
        }),
        Arc::new(move |code| {
            if let Some(shutdown) = shutdown.upgrade() {
                let _ = shutdown.close(code);
            }
        }),
        closing,
    );
    tokio::spawn(async move {
        loop {
            if *stopped.borrow() {
                break;
            }
            for backend in resources.backends() {
                if let Ok(Some(interactions)) = get(&backend.interactions) {
                    let _ = interactions.prune();
                }
            }
            control.tick();
            let delay = events.fallback();
            tokio::select! {
                biased;
                _ = stopped.changed() => break,
                _ = events.changed() => (),
                _ = tokio::time::sleep(delay) => (),
            }
        }
    });
}
