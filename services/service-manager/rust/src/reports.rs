use ivy_contracts::{IvyError, IvyJson, IvyResult, core_schema::validate_host};
use ivy_host_runtime::{config, journal::HostJournal, layout::service_paths};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
use std::{sync::Arc, time::{Duration, Instant}};

fn path(value: &IvyJson) -> IvyResult<std::path::PathBuf> {
    Ok(ivy_host_runtime::node_path::to_native(value.as_js_string().ok_or_else(IvyError::internal)?))
}

fn object(value: &IvyJson) -> IvyResult<IvyJson> {
    match value {
        IvyJson::Object(_) => Ok(value.clone()),
        _ => Err(IvyError::new(
            "invalid_response",
            "Expected schema-validated host state.",
        )),
    }
}

/// Match the old report identity: suppress only timestamp pulses, preserving
/// optional fields, array order, integer-key ordering and all UTF-16 strings.
pub fn host_identity(status: &IvyJson) -> IvyResult<IvyJson> {
    let IvyJson::Object(pairs) = status else {
        return Err(IvyError::new("invalid_response", "Expected host status."));
    };
    let mut stable = IvyJson::Object(
        pairs
            .iter()
            .filter(|(key, _)| {
                !["observedAt", "executor", "instances"]
                    .iter()
                    .any(|name| key == &(*name).into())
            })
            .cloned()
            .collect(),
    );
    let executor = match status.get("executor") {
        Some(IvyJson::Null) => IvyJson::Null,
        Some(value) => {
            let mut value = object(value)?;
            value.insert("observedAt", "".into());
            value
        }
        None => {
            return Err(IvyError::new(
                "invalid_response",
                "Host status has no executor field.",
            ));
        }
    };
    let Some(IvyJson::Array(instances)) = status.get("instances") else {
        return Err(IvyError::new(
            "invalid_response",
            "Host status has no instance list.",
        ));
    };
    let instances = instances
        .iter()
        .map(|instance| {
            let mut instance = object(instance)?;
            instance.insert("observedAt", IvyJson::Null);
            Ok(instance)
        })
        .collect::<IvyResult<Vec<_>>>()?;
    stable.insert("executor", executor);
    stable.insert("instances", IvyJson::Array(instances));
    Ok(stable)
}

struct Signals {
    configuration: Arc<IvyJson>,
    host_revision: i64,
    executor: Option<IvyJson>,
    health: Vec<Option<(bool, IvyJson)>>,
}
impl PartialEq for Signals {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.configuration, &other.configuration)
            && self.host_revision == other.host_revision
            && self.executor == other.executor
            && self.health == other.health
    }
}
impl Signals {
    async fn read(journal: &HostJournal, executor: &Option<IvyJson>, host_revision: i64) -> IvyResult<Self> {
        let configuration = journal.configuration().await?;
        let now = chrono::Utc::now().timestamp_millis() as f64;
        let mut health = Vec::new();
        for instance in configuration.required("instances")?.as_array().ok_or_else(IvyError::internal)? {
            if instance.get("componentId") == Some(&"host-executor".into()) { continue; }
            let data = service_paths(&configuration, instance)?;
            let path = path(data.required("data")?)?.join("health.json");
            let signal = async {
                let mut value = config::json_file(path, config::CONFIGURATION_BYTES).await?;
                validate_host("Health", &value)?;
                let observed = ivy_sdk::clock::parse_date(value.required("observedAt")?.as_js_string().ok_or_else(IvyError::internal)?);
                let current = observed.is_finite() && now - observed < 15_000.0 && observed <= now + 1_000.0;
                value.remove("observedAt");
                Ok::<_,IvyError>((current, value))
            }.await.ok();
            health.push(signal);
        }
        let mut executor = executor.clone();
        if let Some(value) = &mut executor { value.remove("observedAt"); }
        Ok(Self { configuration, host_revision, executor, health })
    }
}

#[derive(Default)]
pub struct Reports {
    minimum_sequence: u64,
    last_host: Option<IvyJson>,
    last_deployments: Option<i64>,
    pending_host: Option<(IvyJson, IvyJson)>,
    pending_deployments: Option<(i64, IvyJson)>,
    signals: Option<Signals>,
    next_metrics: Option<Instant>,
}

impl Reports {
    pub async fn restore_pending(&mut self, journal: &HostJournal) -> IvyResult<()> {
        if let Some(snapshot) = journal.pending_management_snapshot().await? {
            self.pending_host = Some((host_identity(snapshot.required("status")?)?, snapshot));
        }
        Ok(())
    }
    /// Publish journal projections in the original host-before-deployments order.
    /// Readiness consumes the same executor observation used for the host report.
    pub async fn publish_journal(
        &mut self,
        connection: &dyn RpcClient,
        journal: &HostJournal,
        host_id: &IvyJson,
    ) -> IvyResult<Option<IvyJson>> {
        let executor = crate::journal::executor(journal, host_id).await?;
        let versions = journal.report_versions().await?;
        let signals = Signals::read(journal, &executor, versions.host).await?;
        let process_changed = self.signals.as_ref().is_some_and(|prior| prior.health != signals.health || prior.executor != signals.executor);
        let metrics_due = self.next_metrics.is_none_or(|due| Instant::now() >= due);
        if self.pending_host.is_some() || metrics_due || self.signals.as_ref() != Some(&signals) {
            let status = journal.status(executor.clone()).await?;
            let published = self.publish_host(connection, &status, process_changed, |minimum| {
                journal.management_snapshot(executor.clone(), minimum as f64, Some(status.clone()))
            }).await;
            if published.is_ok() || self.pending_host.is_some() { self.signals = Some(signals); }
            published?;
            journal.acknowledge_management_snapshot(self.minimum_sequence).await?;
            if metrics_due { self.next_metrics = Some(Instant::now() + Duration::from_secs(60)); }
        }
        if self.pending_deployments.is_some() || self.deployments_changed(versions.deployments) {
            let (revision, records) = journal.deployment_report().await?;
            self.publish_deployments(connection, revision, records).await?;
        }
        Ok(executor)
    }

    /// Reconcile against the current connection before publishing on that connection.
    pub async fn reconcile_from(
        &mut self,
        connection: &dyn RpcClient,
        host_id: IvyJson,
    ) -> IvyResult<()> {
        let prior = connection
            .request(
                "hosts.observations",
                IvyJson::object([("hostId", host_id)]),
                RequestOptions::default(),
            )
            .await?;
        let snapshots = prior
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|item| {
                let snapshot = item.required("snapshot")?;
                Ok((sequence(snapshot)?, snapshot.required("status")?.clone()))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        self.reconcile(&snapshots)
    }

    /// The journal allocates the snapshot sequence durably. A failed send must
    /// neither acknowledge its identity nor suppress the next reconciliation.
    pub async fn publish_host<F, Fut>(
        &mut self,
        connection: &dyn RpcClient,
        status: &IvyJson,
        process_changed: bool,
        snapshot: F,
    ) -> IvyResult<()>
    where
        F: FnOnce(u64) -> Fut,
        Fut: std::future::Future<Output = IvyResult<IvyJson>>,
    {
        let identity = host_identity(status)?;
        if let Some((_, pending)) = &self.pending_host {
            connection.request("hosts.report", pending.clone(), RequestOptions::default()).await?;
            let (identity, pending) = self.pending_host.take().unwrap();
            self.host_acknowledged(sequence(&pending)?, identity);
        }
        if process_changed || self.host_changed(&identity) {
            let snapshot = snapshot(self.minimum_sequence).await?;
            let sequence = sequence(&snapshot)?;
            // Retain the exact allocated sequence and payload before sending.
            // Dropped replies and cancelled waiters cannot create a new identity.
            self.pending_host = Some((identity.clone(), snapshot.clone()));
            connection
                .request("hosts.report", snapshot, RequestOptions::default())
                .await?;
            self.host_acknowledged(sequence, identity);
            self.pending_host = None;
        }
        Ok(())
    }

    /// Pass the journal's latest 200 records in its retained order.
    pub async fn publish_deployments(
        &mut self,
        connection: &dyn RpcClient,
        revision: i64,
        records: IvyJson,
    ) -> IvyResult<()> {
        if let Some((_, pending)) = &self.pending_deployments {
            connection.request("deployments.report", IvyJson::object([("records", pending.clone())]), RequestOptions::default()).await?;
            let (revision, _) = self.pending_deployments.take().unwrap();
            self.deployments_acknowledged(revision);
        }
        if self.deployments_changed(revision) {
            self.pending_deployments = Some((revision, records.clone()));
            connection
                .request(
                    "deployments.report",
                    IvyJson::object([("records", records)]),
                    RequestOptions::default(),
                )
                .await?;
            self.deployments_acknowledged(revision);
            self.pending_deployments = None;
        }
        Ok(())
    }

    pub fn minimum_sequence(&self) -> u64 {
        self.minimum_sequence
    }

    /// Connection reconciliation supplies snapshots in the server's order. The
    /// first is the comparison baseline; all sequences constrain allocation.
    pub fn reconcile(&mut self, snapshots: &[(u64, IvyJson)]) -> IvyResult<()> {
        if let Some((_, pending)) = &self.pending_host {
            let pending_sequence = sequence(pending)?;
            if let Some((seen, status)) = snapshots.iter().find(|(seen, _)| *seen >= pending_sequence) {
                if *seen == pending_sequence && ivy_contracts::hash::hash_json(status)? != ivy_contracts::hash::hash_json(pending.required("status")?)? {
                    return Err(IvyError::new("mutation_conflict", "A host report sequence identifies different content."));
                }
                self.pending_host = None;
            }
        }
        self.minimum_sequence = snapshots
            .iter()
            .map(|(seq, _)| *seq)
            .chain([self.minimum_sequence])
            .max()
            .unwrap_or(0);
        self.last_host = snapshots
            .first()
            .map(|(_, status)| host_identity(status))
            .transpose()?;
        self.last_deployments = None;
        self.signals = None;
        self.next_metrics = None;
        Ok(())
    }

    pub fn host_changed(&self, identity: &IvyJson) -> bool {
        self.last_host.as_ref() != Some(identity)
    }

    pub fn deployments_changed(&self, revision: i64) -> bool {
        self.last_deployments != Some(revision)
    }

    /// Call only after hosts.report acknowledges on the same connection scope.
    pub fn host_acknowledged(&mut self, sequence: u64, identity: IvyJson) {
        self.minimum_sequence = self.minimum_sequence.max(sequence);
        self.last_host = Some(identity);
    }

    /// Call only after deployments.report acknowledges on that connection.
    pub fn deployments_acknowledged(&mut self, revision: i64) {
        self.last_deployments = Some(revision);
    }
}

fn sequence(snapshot: &IvyJson) -> IvyResult<u64> {
    let value = snapshot
        .required("sequence")?
        .as_f64()
        .ok_or_else(IvyError::internal)?;
    if !value.is_finite()
        || value.fract() != 0.0
        || !(0.0..=9_007_199_254_740_991.0).contains(&value)
    {
        return Err(IvyError::new(
            "invalid_response",
            "Invalid host snapshot sequence.",
        ));
    }
    Ok(value as u64)
}

/// Coalesce process/health pulses at the existing five-second freshness cadence.
/// Individual file pulses must not each rescan every instance. Heartbeats remain
/// independent; metrics and deployment bodies retain their own change conditions.
pub fn consume_reports(
    connection: ivy_sdk::service::ServiceConnection,
    journal: HostJournal,
    host_id: IvyJson,
    reports: Arc<tokio::sync::Mutex<Reports>>,
    admission: crate::connection::Admission,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _ = admission.run(Box::pin(async move {
            let signal = connection.signal();
            let mut ticks = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_secs(5),
                Duration::from_secs(5),
            );
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = signal.cancelled() => break,
                    _ = ticks.tick() => (),
                }
                // A failed publication retains its exact payload. Healthy host
                // tools remain available while this connection retries it.
                let _ = reports.lock().await.publish_journal(&connection, &journal, &host_id).await;
            }
            Ok(())
        })).await;
    })
}
