//! Shared transport evidence; admission and contact policy remain with the caller.
use ivy_contracts::IvyJson;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Pending,
    Confirmed,
    NotSent,
    Unknown,
}
impl DeliveryOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::NotSent => "not_sent",
            Self::Unknown => "outcome_unknown",
        }
    }
    /// The caller first verifies the original Main reply and its persisted pins.
    pub fn main_reply(data: &IvyJson) -> Self {
        match data.get("state").and_then(|s| s.as_str().ok()).as_deref() {
            Some("confirmed") => Self::Confirmed,
            Some("outcome_unknown") => Self::Unknown,
            _ => Self::Pending,
        }
    }
    /// A Phone receipt is distinct from admission. A failed receipt cannot
    /// confirm delivery; the caller decides whether fallback is appropriate.
    pub fn phone_receipt(operation: &IvyJson, state: &str) -> Self {
        if operation.is_null()
            || operation
                .get("phase")
                .is_some_and(|p| p.is_str("submitted"))
        {
            return Self::Pending;
        }
        if operation
            .get("phase")
            .is_some_and(|p| p.is_str("outcome_unknown"))
        {
            return Self::Unknown;
        }
        let receipt = operation.get("receipt");
        match receipt.and_then(|r| r.get("ok")).and_then(IvyJson::as_bool) {
            Some(true)
                if receipt
                    .and_then(|r| r.get("result"))
                    .and_then(|r| r.get("state"))
                    .is_some_and(|s| s.is_str(state)) =>
            {
                Self::Confirmed
            }
            Some(_) => Self::NotSent,
            None => Self::Unknown,
        }
    }
}
