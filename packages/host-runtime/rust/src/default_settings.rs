//! Portable service policy. Account identities, credentials and devices stay local.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use crate::node_path::{self, Style};
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

pub fn default_service_settings(
    component: &str,
    settings: &IvyJson,
    ivy_root: &Path,
    config_path: &Path,
) -> IvyResult<IvyJson> {
    default_service_settings_for_paths(component, settings, &node_path::from_native(ivy_root), &node_path::from_native(config_path), Style::native())
}

/// Resolve lexical defaults in the selected host's path flavor, without a
/// lossy round trip through this process's filesystem representation.
pub fn default_service_settings_for_paths(
    component: &str,
    settings: &IvyJson,
    ivy_root: &JsString,
    config_path: &JsString,
    style: Style,
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
            r#"{"rootObjectId":null,"scheduler":{"enabled":true,"intervalMs":2000,"pageSize":50,"stallWarningMs":300000},"phoneTarget":null}"#
        }
        "chat-bridge" => r#"{"pollMs":1000,"language":"en"}"#,
        "secretary" => {
            r#"{"sources":[],"pollMs":2000,"recordsPerTick":2,"policy":{"minimumMainUrgency":"normal","researchMaxMinutes":5,"silentTime":null,"urgentBypass":false,"voiceEscalation":null,"whatsappQuestionsOnly":true}}"#
        }
        "data-collector" => r#"{"rootName":"DataCollector","maximumMemoryMb":512}"#,
        "dashboards" => r#"{"rootObjectId":null}"#,
        "phone-bridge" => {
            r#"{"binding":{"address":"127.0.0.1","port":5070,"transport":"udp","incomingRingSeconds":60,"outgoingOfferMode":"auto"},"codecs":{"packetMs":20,"preferences":["EVS","G722","PCMA","PCMU"],"receiveReorderMs":20},"policy":{"incoming":[],"recipients":[]},"registration":null,"credentialsPath":null,"incomingPrincipalId":null,"ringSeconds":60,"pollMs":1000}"#
        }
        _ => return Ok(settings.clone()),
    };
    let mut defaults = parse(source, 16384)?;
    match component {
        "host-executor" | "service-manager" => {
            defaults.insert("hostConfigPath", IvyJson::String(config_path.clone()));
        }
        "hive" => {
            let mut backup = defaults.required("backup")?.clone();
            backup.insert("directory", IvyJson::String(node_path::join(style, &[ivy_root.clone(), "backups".into(), "hive".into()])));
            defaults.insert("backup", backup);
        }
        "phone-bridge" => {
            let windows = style == Style::Windows;
            defaults.insert("native", IvyJson::object([("executable", if windows {
                "dist/native/phone/Ivy.PhoneRuntime.exe"
            } else {
                "dist/native/linux-x64/ivy-phone-native"
            }.into())]));
            if windows {
                defaults.insert("audio", parse(r#"{"captureBufferMs":20,"queueMs":80,"renderLatencyMs":20,"sourceMode":"system_excluding_runtime"}"#, 1024)?);
            } else {
                defaults.insert("incomingRoute", "voice".into());
                defaults.insert("outgoingRoute", "voice".into());
            }
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
