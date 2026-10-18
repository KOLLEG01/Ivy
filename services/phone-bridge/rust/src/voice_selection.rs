use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceSelection {
    pub model: JsString,
    pub reasoning_effort: JsString,
}

impl Default for VoiceSelection {
    fn default() -> Self {
        Self {
            model: "gpt-6.1-sol".into(),
            reasoning_effort: "high".into(),
        }
    }
}

impl VoiceSelection {
    pub fn to_wire(&self) -> IvyJson {
        IvyJson::object([
            ("model", IvyJson::String(self.model.clone())),
            (
                "reasoningEffort",
                IvyJson::String(self.reasoning_effort.clone()),
            ),
        ])
    }

    /// Validate against the advertised catalog, including models unknown to this
    /// build. Stable keypad aliases are not the catalog's allowlist.
    pub fn validate(&self, models: &IvyJson) -> IvyResult<()> {
        let model = models
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|item| item.get("model").and_then(IvyJson::as_js_string) == Some(&self.model));
        let supported = match model {
            None => false,
            Some(model) => model
                .get("supportedReasoningEfforts")
                .and_then(IvyJson::as_array)
                .ok_or_else(IvyError::internal)?
                .iter()
                .any(|item| {
                    item.get("reasoningEffort").and_then(IvyJson::as_js_string)
                        == Some(&self.reasoning_effort)
                }),
        };
        if supported {
            Ok(())
        } else {
            Err(IvyError::new(
                "phone_voice_selection_invalid",
                "Select a model and reasoningEffort advertised by phone_bridge_status.voiceModels.",
            ))
        }
    }
}

pub fn selected_keypad_model(
    model: Option<&JsString>,
    reasoning_effort: Option<&JsString>,
) -> IvyResult<VoiceSelection> {
    let model = match model.and_then(|value| value.to_utf8().ok()).as_deref() {
        Some("luna") => Some("gpt-6-luna"),
        Some("sol") => Some("gpt-6-sol"),
        Some("astra") => Some("gpt-6-astra"),
        _ => None,
    };
    match (model, reasoning_effort.filter(|value| !value.0.is_empty())) {
        (Some(model), Some(reasoning_effort)) => Ok(VoiceSelection {
            model: model.into(),
            reasoning_effort: reasoning_effort.clone(),
        }),
        _ => Err(IvyError::new(
            "phone_voice_selection_invalid",
            "Voice model and reasoning selection is unsupported.",
        )),
    }
}
