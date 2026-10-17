use crate::{IvyError, IvyJson, IvyResult};
use ivy_contracts::{json::canonical, limits::MANAGEMENT_FRAME_BYTES};

/// Best-effort restrictions of the existing host, not tool isolation.
pub fn restrict_assessment_parameters(original: &IvyJson) -> IvyResult<IvyJson> {
    let mut value = original.clone();
    for name in ["threadStart", "threadResume", "turnStart"] {
        let mut params = value.required(name)?.clone();
        if params.as_object().is_none()
            || params.get("sandbox").is_some()
            || params.get("sandboxPolicy").is_some()
        {
            return Err(IvyError::new(
                "assessment_policy_invalid",
                "Assessment uses the named permissions profile without legacy sandbox fields.",
            ));
        }
        params.insert("permissions", ":danger-full-access".into());
        params.insert("approvalPolicy", "never".into());
        if name != "turnStart" {
            let mut config = params
                .get("config")
                .cloned()
                .unwrap_or_else(|| IvyJson::Object(Vec::new()));
            if config.as_object().is_none() {
                return Err(IvyError::new(
                    "assessment_policy_invalid",
                    "Assessment config must be an object.",
                ));
            }
            for key in [
                "features.shell_tool",
                "features.apps",
                "features.multi_agent",
            ] {
                config.insert(key, false.into());
            }
            config.insert("web_search", "disabled".into());
            params.insert("config", config);
        }
        if name != "threadResume" {
            params.insert("environments", IvyJson::Array(Vec::new()));
        }
        if name == "threadStart" {
            params.insert("selectedCapabilityRoots", IvyJson::Array(Vec::new()));
            params.insert("dynamicTools", IvyJson::Array(Vec::new()));
        }
        value.insert(name, params);
    }
    Ok(value)
}

pub fn assert_assessment_policy(params: &IvyJson) -> IvyResult<()> {
    if canonical(params, MANAGEMENT_FRAME_BYTES)?
        != canonical(
            &restrict_assessment_parameters(params)?,
            MANAGEMENT_FRAME_BYTES,
        )?
    {
        return Err(IvyError::new(
            "assessment_policy_changed",
            "Original assessment parameters lack the configured restrictions; do not dispatch them.",
        ));
    }
    Ok(())
}
