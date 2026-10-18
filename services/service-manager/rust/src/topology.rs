use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{node_runtime::NodeRuntime, runtime_owner::RuntimeOwner};
use std::path::PathBuf;

/// Construction order is retained for reverse cleanup. Existing owners retain
/// their own startup fallback and never restart for unrelated topology edits.
pub struct Owners {
    owners: Vec<(JsString, RuntimeOwner)>,
    bootstrap_root: PathBuf,
    node: NodeRuntime,
    closed: bool,
}
impl Owners {
    pub fn new(bootstrap_root: PathBuf, node: NodeRuntime) -> Self {
        Self {
            owners: Vec::new(),
            bootstrap_root,
            node,
            closed: false,
        }
    }
    pub fn ids(&self) -> Vec<JsString> {
        self.owners.iter().map(|(id, _)| id.clone()).collect()
    }
    pub async fn reconcile(&mut self, platform: &str, accepted: &IvyJson) -> IvyResult<()> {
        if self.closed {
            return Err(IvyError::new(
                "service_unavailable",
                "ServiceManager owner admission is stopped.",
            ));
        }
        let desired = owner_ids(platform, accepted)?;
        let mut removed = Vec::new();
        self.owners.retain(|(id, owner)| {
            if desired.contains(id) {
                true
            } else {
                removed.push(owner.clone());
                false
            }
        });
        let mut first = None;
        for owner in removed.into_iter().rev() {
            if let Err(error) = owner.close().await {
                first.get_or_insert(error);
            }
        }
        let mut added = Vec::new();
        for id in desired
            .iter()
            .filter(|id| !self.owners.iter().any(|(current, _)| current == *id))
        {
            match RuntimeOwner::for_host(
                accepted.clone(),
                id.to_utf8()?,
                self.bootstrap_root.clone(),
                self.node.clone(),
            )
            .await
            {
                Ok(owner) => added.push((id.clone(), owner)),
                Err(error) => {
                    first.get_or_insert(error);
                    for (_, owner) in added.into_iter().rev() {
                        let _ = owner.close().await;
                    }
                    return Err(first.unwrap());
                }
            }
        }
        for (_, owner) in &added {
            if let Err(error) = owner.start() {
                for (_, owner) in added.iter().rev() {
                    let _ = owner.close().await;
                }
                return Err(error);
            }
        }
        self.owners.extend(added);
        first.map_or(Ok(()), Err)
    }
    pub async fn close(&mut self) -> IvyResult<()> {
        self.closed = true;
        let mut first = None;
        for (_, owner) in std::mem::take(&mut self.owners).into_iter().rev() {
            if let Err(error) = owner.close().await {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }
}

/// Select from accepted local configuration, never directly from a Hive hint.
/// Disabled process instances still need an owner to observe a later target.
pub fn owner_ids(platform: &str, configuration: &IvyJson) -> IvyResult<Vec<JsString>> {
    if platform != "win32" {
        return Ok(Vec::new());
    }
    let Some(IvyJson::Array(instances)) = configuration.get("instances") else {
        return Err(IvyError::new(
            "invalid_arguments",
            "Host configuration has no instances.",
        ));
    };
    instances
        .iter()
        .filter(|instance| {
            instance.get("engine") == Some(&IvyJson::from("process"))
                && !["hive", "host-executor", "service-manager"]
                    .iter()
                    .any(|component| {
                        instance.get("componentId") == Some(&IvyJson::from(*component))
                    })
        })
        .map(|instance| match instance.get("instanceId") {
            Some(IvyJson::String(id)) => Ok(id.clone()),
            _ => Err(IvyError::new(
                "invalid_arguments",
                "Host instance has no identity.",
            )),
        })
        .collect()
}

pub struct Changes {
    pub added: Vec<JsString>,
    pub removed: Vec<JsString>,
}

/// Existing owners remain alive. Configuration refresh must not tear down an
/// unrelated child or replace its accepted target settings with desired settings.
pub fn changes(current: &[JsString], desired: &[JsString]) -> Changes {
    Changes {
        added: desired
            .iter()
            .filter(|id| !current.contains(id))
            .cloned()
            .collect(),
        removed: current
            .iter()
            .rev()
            .filter(|id| !desired.contains(id))
            .cloned()
            .collect(),
    }
}
