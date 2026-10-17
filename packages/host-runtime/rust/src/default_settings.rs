//! Portable service policy. Account identities, credentials and devices stay local.
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::parse};
use std::path::Path;

/// Omitted fields inherit defaults. Explicit nulls, arrays and values replace them.
pub fn with_defaults(defaults: &IvyJson, configured: &IvyJson) -> IvyResult<IvyJson> {
    let mut result = defaults.clone();
    if result.as_object().is_none() {
        return Err(IvyError::new(
            "invalid_arguments",
            "Default settings must be an object.",
        ));
    }
    for (key, value) in configured.as_object().ok_or_else(|| {
        IvyError::new(
            "invalid_arguments",
            "Configured settings must be an object.",
        )
    })? {
        let selected = match (result.get_key(key), value) {
            (Some(previous @ IvyJson::Object(_)), IvyJson::Object(_)) => {
                with_defaults(previous, value)?
            }
            _ => value.clone(),
        };
        result.insert(key.clone(), selected);
    }
    Ok(result)
}

fn native_string(path: &Path) -> IvyResult<IvyJson> {
    path.to_str()
        .map(IvyJson::from)
        .ok_or_else(|| IvyError::new("invalid_arguments", "Configured path is not Unicode."))
}

pub fn default_service_settings(
    component: &str,
    settings: &IvyJson,
    ivy_root: &Path,
    config_path: &Path,
) -> IvyResult<IvyJson> {
    let source = match component {
        "host-executor" | "service-manager" => "{}",
        "hive" => {
            r#"{"listenHost":"127.0.0.1","listenPort":39081,"backup":{"enabled":true,"intervalHours":24,"retain":7}}"#
        }
        "agent-manager" => {
            r#"{"patcherEnabled":true,"limits":{"maxOperations":100000,"maxJournalBytes":536870912,"maxPendingInputs":64,"maxNotificationBytes":16777216}}"#
        }
        "task-board" => {
            r#"{"rootObjectId":null,"scheduler":{"enabled":true,"intervalMs":2000,"pageSize":50},"phoneTarget":null}"#
        }
        "chat-bridge" => r#"{"pollMs":1000,"language":"en"}"#,
        "secretary" => {
            r#"{"sources":[],"pollMs":2000,"recordsPerTick":2,"policy":{"minimumMainUrgency":"normal","researchMaxMinutes":5,"silentTime":null,"urgentBypass":false,"voiceEscalation":null,"whatsappQuestionsOnly":true}}"#
        }
        "data-collector" => r#"{"rootName":"DataCollector","maximumMemoryMb":512}"#,
        "dashboards" => r#"{"rootObjectId":null}"#,
        "phone-bridge" => {
            r#"{"native":{"executable":"dist/native/phone/Ivy.PhoneRuntime.exe"},"binding":{"address":"127.0.0.1","port":5070,"transport":"udp","incomingRingSeconds":60,"outgoingOfferMode":"auto"},"codecs":{"packetMs":20,"preferences":["EVS","G722","PCMA","PCMU"],"receiveReorderMs":20},"audio":{"captureBufferMs":20,"queueMs":80,"renderLatencyMs":20,"sourceMode":"system_excluding_runtime"},"policy":{"incoming":[],"recipients":[]},"registration":null,"credentialsPath":null,"incomingPrincipalId":null,"ringSeconds":60,"pollMs":1000}"#
        }
        _ => return Ok(settings.clone()),
    };
    let mut defaults = parse(source, 16384)?;
    match component {
        "host-executor" | "service-manager" => {
            defaults.insert("hostConfigPath", native_string(config_path)?);
        }
        "hive" => {
            let mut backup = defaults.required("backup")?.clone();
            backup.insert("directory", native_string(&ivy_root.join("backups/hive"))?);
            defaults.insert("backup", backup);
        }
        "agent-manager" => {
            let claude = settings
                .get("appServer")
                .and_then(|value| value.get("mode"))
                == Some(&IvyJson::from("claude-adapter"));
            defaults.insert(
                "capabilities",
                IvyJson::Array(vec![IvyJson::object([
                    (
                        "key",
                        IvyJson::from(if claude {
                            "general-claude"
                        } else {
                            "general-codex"
                        }),
                    ),
                    (
                        "label",
                        IvyJson::from(if claude {
                            "General Claude work"
                        } else {
                            "General Codex work"
                        }),
                    ),
                ])]),
            );
        }
        _ => {}
    }
    with_defaults(&defaults, settings)
}
