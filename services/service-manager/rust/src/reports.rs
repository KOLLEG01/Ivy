use ivy_contracts::{
    IvyError, IvyJson, IvyResult, json::stringify, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestOptions, RpcClient};

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
pub fn host_identity(status: &IvyJson) -> IvyResult<String> {
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
    stringify(&stable, MANAGEMENT_FRAME_BYTES)
}

#[derive(Default)]
pub struct Reports {
    minimum_sequence: u64,
    last_host: Option<String>,
    last_deployments: Option<String>,
}

impl Reports {
    /// Publish journal projections in the original host-before-deployments order.
    /// Readiness consumes the same executor observation used for the host report.
    pub async fn publish_journal(
        &mut self,
        connection: &dyn RpcClient,
        journal: &ivy_host_runtime::journal::HostJournal,
        host_id: &IvyJson,
    ) -> IvyResult<Option<IvyJson>> {
        let executor = crate::journal::executor(journal, host_id).await?;
        let status = journal.status(executor.clone()).await?;
        self.publish_host(connection, &status, |minimum| {
            journal.management_snapshot(executor.clone(), minimum as f64, Some(status.clone()))
        })
        .await?;
        let records = journal
            .list(200.0)
            .await?
            .iter()
            .map(|entry| entry.required("record").cloned())
            .collect::<IvyResult<Vec<_>>>()?;
        self.publish_deployments(connection, IvyJson::Array(records))
            .await?;
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
        snapshot: F,
    ) -> IvyResult<()>
    where
        F: FnOnce(u64) -> Fut,
        Fut: std::future::Future<Output = IvyResult<IvyJson>>,
    {
        let identity = host_identity(status)?;
        if self.host_changed(&identity) {
            let snapshot = snapshot(self.minimum_sequence).await?;
            let sequence = sequence(&snapshot)?;
            connection
                .request("hosts.report", snapshot, RequestOptions::default())
                .await?;
            self.host_acknowledged(sequence, identity);
        }
        Ok(())
    }

    /// Pass the journal's latest 200 records in its retained order.
    pub async fn publish_deployments(
        &mut self,
        connection: &dyn RpcClient,
        records: IvyJson,
    ) -> IvyResult<()> {
        let identity = stringify(&records, MANAGEMENT_FRAME_BYTES)?;
        if self.deployments_changed(&identity) {
            connection
                .request(
                    "deployments.report",
                    IvyJson::object([("records", records)]),
                    RequestOptions::default(),
                )
                .await?;
            self.deployments_acknowledged(identity);
        }
        Ok(())
    }

    pub fn minimum_sequence(&self) -> u64 {
        self.minimum_sequence
    }

    /// Connection reconciliation supplies snapshots in the server's order. The
    /// first is the comparison baseline; all sequences constrain allocation.
    pub fn reconcile(&mut self, snapshots: &[(u64, IvyJson)]) -> IvyResult<()> {
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
        Ok(())
    }

    pub fn host_changed(&self, identity: &str) -> bool {
        self.last_host.as_deref() != Some(identity)
    }

    pub fn deployments_changed(&self, identity: &str) -> bool {
        self.last_deployments.as_deref() != Some(identity)
    }

    /// Call only after hosts.report acknowledges on the same connection scope.
    pub fn host_acknowledged(&mut self, sequence: u64, identity: String) {
        self.minimum_sequence = self.minimum_sequence.max(sequence);
        self.last_host = Some(identity);
    }

    /// Call only after deployments.report acknowledges on that connection.
    pub fn deployments_acknowledged(&mut self, identity: String) {
        self.last_deployments = Some(identity);
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
