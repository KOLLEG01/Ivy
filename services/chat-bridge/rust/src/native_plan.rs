//! Chat-specific native preferences. Retained-plan loading and native schema
//! validation use the shared SDK; this module owns the domain overlay only.
use crate::main_context::{receiver, spread, truthy};
use ivy_contracts::{IvyJson as Json, IvyResult};

/// Native collaboration settings take precedence over top-level model/effort.
/// A falsey preference leaves its corresponding retained collaboration setting.
pub fn chat_turn_options(template: &Json, options: Option<&Json>) -> IvyResult<Json> {
    receiver(template)?;
    let mut result = spread(template);
    if let Some(options) = options {
        for (key, value) in spread(options).as_object().unwrap() {
            result.insert(key.clone(), value.clone());
        }
    }
    if let Some(mode) = template
        .get("collaborationMode")
        .filter(|v| v.as_object().is_some())
    {
        if let Some(settings) = mode.get("settings").filter(|v| v.as_object().is_some()) {
            let mut settings = settings.clone();
            for (source, target) in [("model", "model"), ("effort", "reasoning_effort")] {
                if let Some(value) = options.and_then(|v| v.get(source)).filter(|v| truthy(v)) {
                    settings.insert(target, value.clone());
                }
            }
            let mut mode = mode.clone();
            mode.insert("settings", settings);
            result.insert("collaborationMode", mode);
        }
    }
    Ok(result)
}
