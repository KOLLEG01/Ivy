use crate::{IvyJson, JsString};
use serde::{Deserialize, Serialize};
use std::fmt;

pub type IvyResult<T> = Result<T, IvyError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NotExecuted,
    Unknown,
    Completed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotExecuted => "not_executed",
            Self::Unknown => "unknown",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IvyError {
    pub code: String,
    pub message: String,
    pub outcome: Outcome,
    pub details: Option<IvyJson>,
    /// Exact JS boundary text, when it is not representable as Rust UTF-8.
    pub wire_code: Option<JsString>,
    pub wire_message: Option<JsString>,
}

impl IvyError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            outcome: Outcome::NotExecuted,
            details: None,
            wire_code: None,
            wire_message: None,
        }
    }
    pub fn new_js(code: JsString, message: JsString) -> Self {
        // Lossy forms are diagnostic conveniences only; serialization uses exact code units.
        let mut error = Self::new(
            String::from_utf16_lossy(&code.0),
            String::from_utf16_lossy(&message.0),
        );
        error.wire_code = Some(code);
        error.wire_message = Some(message);
        error
    }
    pub fn with_js_message(mut self, message: JsString) -> Self {
        self.message = String::from_utf16_lossy(&message.0);
        self.wire_message = Some(message);
        self
    }
    pub fn message_js(&self) -> JsString {
        self.wire_message
            .clone()
            .unwrap_or_else(|| self.message.clone().into())
    }
    pub fn code_js(&self) -> JsString {
        self.wire_code
            .clone()
            .unwrap_or_else(|| self.code.clone().into())
    }
    pub fn with_outcome(mut self, outcome: Outcome) -> Self {
        self.outcome = outcome;
        self
    }
    pub fn with_details(mut self, details: IvyJson) -> Self {
        self.details = Some(details);
        self
    }
    pub fn internal() -> Self {
        Self::new("internal_error", "The operation failed internally.")
            .with_outcome(Outcome::Unknown)
    }
    pub fn http_status(&self) -> u16 {
        match self.code.as_str() {
            "unauthenticated" => 401,
            "forbidden" => 403,
            "not_found" | "contract_not_found" => 404,
            "revision_conflict"
            | "mutation_conflict"
            | "contract_version_conflict"
            | "contract_definition_conflict"
            | "contract_owner_mismatch"
            | "stale_generation"
            | "target_conflict"
            | "ambiguous_service_node"
            | "tool_definition_changed"
            | "release_conflict"
            | "subscription_filter_conflict" => 409,
            "revision_pruned" | "mutation_expired" | "operation_expired" => 410,
            "content_too_large" | "frame_too_large" | "result_too_large" => 413,
            "limit_exceeded" | "capacity_exceeded" => 429,
            "service_unavailable" | "service_not_ready" | "storage_unavailable" => 503,
            "deadline_exceeded" | "outcome_unknown" => 504,
            "internal_error" => 500,
            "provider_contract_error" => 502,
            _ => 400,
        }
    }
    pub fn to_wire(&self) -> IvyJson {
        let mut data = IvyJson::object([
            ("code", IvyJson::String(self.code_js())),
            ("outcome", self.outcome.as_str().into()),
        ]);
        if let Some(details) = &self.details {
            data.insert("details", details.clone());
        }
        IvyJson::object([
            (
                "code",
                IvyJson::Number(match self.code.as_str() {
                    "invalid_frame" => -32600.0,
                    "invalid_arguments" => -32602.0,
                    _ => -32000.0,
                }),
            ),
            (
                "message",
                IvyJson::String(JsString(
                    self.message_js().0.into_iter().take(2048).collect(),
                )),
            ),
            ("data", data),
        ])
    }
}
impl fmt::Display for IvyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for IvyError {}
