//! Native resource cleanup cannot change a saved business completion.
use ivy_contracts::{IvyJson, JsString};

#[derive(Clone, Copy)]
pub enum CleanupAction { Archive, Delete }
impl CleanupAction {
    pub fn for_execution(value: &IvyJson) -> Option<Self> {
        match value.get("phase")?.as_str().ok()?.as_str() {
            "archiving" => Some(Self::Archive),
            "archived" | "deleting" => Some(Self::Delete),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        match self { Self::Archive => "archive", Self::Delete => "delete" }
    }
    pub fn issue(self, value: &IvyJson, code: &str, now: &JsString, blocked: bool) -> IvyJson {
        let state = if blocked { "blocked" } else if matches!(code,
            "outcome_unknown" | "request_timeout" | "transport_error" | "native_outcome_unknown") {
            "outcome_unknown"
        } else { "pending" };
        if value.get("cleanup").is_some_and(|current| current.get("action") == Some(&self.name().into())
            && current.get("state") == Some(&state.into()) && current.get("errorCode") == Some(&code.into())) {
            return value.clone();
        }
        let mut next = value.clone();
        next.insert("cleanup", IvyJson::object([
            ("action", self.name().into()),
            ("state", state.into()),
            ("errorCode", code.into()),
            ("updatedAt", IvyJson::String(now.clone())),
        ]));
        next.insert("updatedAt", IvyJson::String(now.clone()));
        next
    }
    pub fn confirmed(self, value: &mut IvyJson, now: &JsString) {
        // Normal archive/deletion timestamps already confirm success. Replace
        // an earlier issue explicitly so its failure is never shown as current.
        if value.get("cleanup").is_some() {
            value.insert("cleanup", IvyJson::object([
                ("action", self.name().into()), ("state", "confirmed".into()),
                ("errorCode", IvyJson::Null), ("updatedAt", IvyJson::String(now.clone())),
            ]));
        }
    }
}
pub fn blocked(value: &IvyJson) -> bool {
    value.get("cleanup").and_then(|c| c.get("state")) == Some(&"blocked".into())
}
pub fn identity_missing(code: &str) -> bool {
    matches!(code, "native_backend_required" | "native_backend_not_found" | "native_backend_mismatch"
        | "native_owner_mismatch" | "native_evidence_mismatch" | "tool_definition_changed")
}
/// A cleanup error can repair only the immediately preceding, successful
/// completion of this same execution. Account and operation identities stay exact.
pub fn failed_delete_matches(prior: &IvyJson, current: &IvyJson) -> bool {
    prior.get("phase") == Some(&"deleting".into())
        && current.get("phase") == Some(&"failed".into())
        && current.get("errorCode") == Some(&"native_backend_required".into())
        && prior.get("errorCode").is_some_and(IvyJson::is_null)
        && ["result", "completedAt", "archivedAt"].iter().all(|key| prior.get(key).is_some_and(|v| !v.is_null()))
        && prior.get("nativeOperations").and_then(|v| v.get("delete")).and_then(IvyJson::as_js_string).is_some_and(|s| !s.0.is_empty())
        && ["executionId", "assignment", "serviceNodeId", "result", "completedAt", "archivedAt", "nativeTarget", "nativeOperations", "threadId", "turnId"]
            .iter().all(|key| prior.get(key) == current.get(key))
}
