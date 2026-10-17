use ivy_contracts::{
    IvyError, IvyJson, IvyResult, json::stringify, limits::MANAGEMENT_FRAME_BYTES,
};

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
