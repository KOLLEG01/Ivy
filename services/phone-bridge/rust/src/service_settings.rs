//! Original service configuration validation. Protected values travel only to
//! their configured native owners; public tool arguments never supply them.
use crate::{admission::PhoneAdmission, contracts::validate_service};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    codex_home::{HomeContext, HomeSettings, resolve_codex_home},
    config::json_file,
    node_path,
};
use std::collections::BTreeSet;

fn require(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("invalid_arguments", message))
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn array<'a>(value: &'a IvyJson, key: &str) -> &'a [IvyJson] {
    value.get(key).and_then(IvyJson::as_array).unwrap_or(&[])
}
fn string<'a>(value: &'a IvyJson, key: &str) -> Option<&'a JsString> {
    value.get(key).and_then(IvyJson::as_js_string)
}
pub fn phone_settings(value: &IvyJson, context: &HomeContext<'_>) -> IvyResult<IvyJson> {
    validate_service("PhoneServiceSettings", value)?;
    let policy = field(value, "policy")?;
    PhoneAdmission::new(policy.clone())?;
    let absolute = |s: &JsString| node_path::is_absolute(context.paths.style, s);
    require(
        array(policy, "challengedIncoming").is_empty()
            || string(value, "accessCodePath").is_some_and(absolute),
        "Challenged incoming access requires an absolute protected code settings path.",
    )?;
    let incoming = value
        .get("incomingRoute")
        .cloned()
        .unwrap_or_else(|| "voice".into());
    let outgoing = value
        .get("outgoingRoute")
        .cloned()
        .unwrap_or_else(|| "windows".into());
    let codex = field(value, "codexVoice")?;
    require(
        incoming != "voice".into() && outgoing != "voice".into() || !codex.is_null(),
        "Codex Voice routing requires its configured CLI runtime.",
    )?;
    if !codex.is_null() {
        let queue = codex
            .get("queueMs")
            .and_then(IvyJson::as_f64)
            .unwrap_or(80.0);
        require(
            codex
                .get("playbackPrebufferMs")
                .and_then(IvyJson::as_f64)
                .is_none_or(|n| n <= queue),
            "Voice playout prebuffer cannot exceed its audio queue.",
        )?;
        resolve_codex_home(
            &HomeSettings {
                codex_home: string(codex, "codexHome").cloned(),
                native_home: string(codex, "nativeHome").cloned(),
            },
            context,
        )?;
        require(
            string(codex, "nativeExecutable").is_some_and(absolute)
                && string(codex, "cwd").is_some_and(absolute),
            "Codex Voice executable and working directory must be absolute.",
        )?;
    }
    require(
        incoming != "windows".into()
            && outgoing != "windows".into()
            && value.get("speech").is_none()
            || value.get("audio").is_some_and(|v| !v.is_null()),
        "Windows call routing requires explicit Windows audio settings.",
    )?;
    require(
        string(value, "incomingInitialPrompt")
            .is_none_or(|s| String::from_utf16_lossy(&s.0).len() <= 32000),
        "Initial Voice prompt exceeds the native session text bound.",
    )?;
    let credentials = field(value, "credentialsPath")?;
    require(
        credentials.is_null() || credentials.as_js_string().is_some_and(absolute),
        "Phone credentials need an explicit absolute protected file path.",
    )?;
    require(
        field(value, "registration")?.is_null() || !credentials.is_null(),
        "SIP registration requires its protected credential file.",
    )?;
    require(
        field(value, "incomingPrincipalId")?.is_null()
            || !array(policy, "incoming").is_empty()
            || !array(policy, "challengedIncoming").is_empty(),
        "Automatic incoming acceptance requires a configured incoming policy.",
    )?;
    let peers: BTreeSet<_> = array(policy, "incoming")
        .iter()
        .chain(array(policy, "challengedIncoming"))
        .filter_map(|v| string(v, "peerAddress"))
        .collect();
    require(
        peers.len() <= 32,
        "Native incoming peer inventory exceeds its bound.",
    )?;
    Ok(value.clone())
}
#[derive(Clone, Copy)]
pub enum PhoneConfigurationSection {
    Sip,
    Access,
    Speech,
}
impl PhoneConfigurationSection {
    fn name(self) -> &'static str {
        match self {
            Self::Sip => "sip",
            Self::Access => "access",
            Self::Speech => "speech",
        }
    }
}
pub async fn phone_configuration_section(
    path: JsString,
    section: PhoneConfigurationSection,
) -> IvyResult<IvyJson> {
    require(
        node_path::is_absolute(node_path::Style::native(), &path),
        "Phone configuration requires an absolute protected path.",
    )?;
    let path = node_path::node_filesystem_path(&path).map_err(|_| IvyError::internal())?;
    let value = json_file(path, 128 * 1024).await?;
    let section = value.get(section.name());
    require(
        value.get("schemaVersion") == Some(&1.0.into())
            && section.is_some_and(|s| s.as_object().is_some()),
        "Phone service configuration is invalid.",
    )?;
    Ok(section.unwrap().clone())
}
pub async fn phone_credentials(path: Option<JsString>) -> IvyResult<IvyJson> {
    let Some(path) = path else {
        return Ok(IvyJson::object([
            ("username", IvyJson::Null),
            ("password", IvyJson::Null),
        ]));
    };
    let value = phone_configuration_section(path, PhoneConfigurationSection::Sip).await?;
    validate_service("PhoneCredentials", &value)?;
    Ok(value)
}

#[cfg(test)]
#[path = "../../../../tests/rust/phone_service_settings.rs"]
mod tests;
