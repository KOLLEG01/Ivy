use super::{field, text};
use ivy_contracts::{IvyError, IvyJson, IvyResult};

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_audio_setup.rs"]
mod tests;

/// Device and mute observations only; this does not open an audio stream.
pub(super) fn setup(inventory: &IvyJson, profiles: &IvyJson) -> IvyResult<IvyJson> {
    let supported = field(inventory, "processLoopbackSupported")?.clone();
    let devices = field(inventory, "devices")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let mut routes = vec![];
    for profile in profiles.as_array().ok_or_else(IvyError::internal)? {
        let route = field(profile, "route")?.clone();
        let settings = field(profile, "settings")?;
        let policy = settings.get("mutePolicy").filter(|v| !v.is_null());
        let muted_id = policy
            .and_then(|p| p.get("mutedRenderEndpointId"))
            .filter(|v| !v.is_null());
        let check = |role: &str, id: &IvyJson, direction: &str, expected: IvyJson| -> IvyJson {
            let matches = devices
                .iter()
                .filter(|d| d.get("id") == Some(id))
                .collect::<Vec<_>>();
            let device = (matches.len() == 1).then(|| matches[0]);
            let muted = device
                .and_then(|d| d.get("muted"))
                .cloned()
                .unwrap_or(IvyJson::Null);
            let state = if matches.len() > 1 {
                "ambiguous"
            } else if device.is_none() {
                "missing"
            } else if device.and_then(|d| d.get("direction")) != Some(&direction.into()) {
                "wrong_direction"
            } else if device.and_then(|d| d.get("state")) != Some(&"Active".into()) {
                "inactive"
            } else if !expected.is_null() && muted.is_null() {
                "mute_unknown"
            } else if !expected.is_null() && muted != expected {
                "mute_mismatch"
            } else {
                "ready"
            };
            IvyJson::object([
                ("role", role.into()),
                ("id", id.clone()),
                (
                    "name",
                    device
                        .and_then(|d| d.get("name"))
                        .cloned()
                        .unwrap_or(IvyJson::Null),
                ),
                ("state", state.into()),
                ("muted", muted),
                ("expectedMuted", expected),
            ])
        };
        let mode = text(settings, "sourceMode")?;
        let render = field(settings, "renderEndpointId")?;
        let expected = if policy.and_then(|p| p.get("unmuteReceive")) == Some(&true.into()) {
            false.into()
        } else {
            IvyJson::Null
        };
        let mut endpoints = vec![check("receive", render, "render", expected)];
        if route == "voice".into() || mode == "microphone".into() {
            endpoints.insert(
                0,
                check(
                    "capture",
                    field(settings, "captureEndpointId")?,
                    "capture",
                    IvyJson::Null,
                ),
            );
        }
        if mode == "virtual_speaker".into() {
            endpoints.insert(
                0,
                check(
                    "loopback",
                    field(settings, "loopbackRenderEndpointId")?,
                    "render",
                    IvyJson::Null,
                ),
            );
        }
        if let Some(id) = muted_id {
            endpoints.push(check("mutedOutput", id, "render", true.into()));
        }
        let conflict = muted_id == Some(render);
        let ready = !conflict
            && !(mode == "virtual_speaker".into()
                && settings.get("loopbackRenderEndpointId") == Some(render))
            && (mode == "microphone".into()
                || mode == "virtual_speaker".into()
                || supported == true.into())
            && endpoints
                .iter()
                .all(|e| e.get("state") == Some(&"ready".into()));
        routes.push(IvyJson::object([
            ("route", route),
            ("sourceMode", mode.into()),
            ("mutePolicyConflict", conflict.into()),
            ("ready", ready.into()),
            ("endpoints", IvyJson::Array(endpoints)),
        ]));
    }
    Ok(IvyJson::object([
        ("processLoopbackSupported", supported),
        ("routes", IvyJson::Array(routes)),
    ]))
}
