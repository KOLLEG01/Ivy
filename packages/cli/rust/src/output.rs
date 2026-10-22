use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, core_schema::validate_host};

#[derive(Debug, Clone)]
pub struct CliResult {
    pub exit_code: u8,
    pub output: IvyJson,
}

impl CliResult {
    pub fn success(output: IvyJson) -> IvyResult<Self> {
        validate_host("CliOutput", &output)?;
        let code = output.required("code")?.as_str()?;
        let exit_code = if matches!(
            code.as_str(),
            "deployment_needs_attention" | "bootstrap_needs_attention"
        ) {
            3
        } else if output.get("ok").and_then(IvyJson::as_bool) == Some(true) {
            0
        } else {
            1
        };
        Ok(Self { exit_code, output })
    }

    /// Keep the accepted operation identity even if a later wait/display read
    /// fails. The caller owns that identity and never cancels work on timeout.
    pub fn failure(error: &IvyError, deployment_id: Option<&JsString>) -> Self {
        let mut data = IvyJson::object([
            ("message", IvyJson::String(error.message_js())),
            ("outcome", error.outcome_json()),
        ]);
        if let Some(details) = &error.details {
            data.insert("details", details.clone());
        }
        let mut output = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("ok", false.into()),
            ("code", IvyJson::String(error.code_js())),
        ]);
        if let Some(id) = deployment_id {
            output.insert("deploymentId", IvyJson::String(id.clone()));
        }
        output.insert("data", data);
        let exit_code = match error.code_js().to_utf8().as_deref() {
            Ok("needs_attention") => 3,
            Ok("invalid_arguments") => 2,
            _ => 1,
        };
        Self { exit_code, output }
    }

    pub fn malformed_configuration(deployment_id: Option<&JsString>) -> Self {
        Self::failure(
            &IvyError::new(
                "invalid_arguments",
                "Configuration contains malformed JSON.",
            ),
            deployment_id,
        )
    }
}
