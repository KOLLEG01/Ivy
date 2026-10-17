use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};

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
