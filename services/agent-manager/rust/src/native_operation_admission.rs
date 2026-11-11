//! Work retained before the journal and native transport have admitted it.
//! Inputs end with the original task; count and results also follow its handles.
use super::{Active, Identity};
use crate::service_work::run::{Admission as Reservation, Budget};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use std::mem::size_of;
pub(super) struct Admission {
    pub budget: Budget,
}
fn capacity(duplicate: bool) -> IvyError {
    IvyError::new(
        "native_capacity",
        if duplicate {
            "Native observation capacity is full; reconcile the original operation."
        } else {
            "Native operation work is full; no new work or claim was accepted."
        },
    )
    .with_outcome(if duplicate {
        Outcome::Unknown
    } else {
        Outcome::NotExecuted
    })
}
fn string_bytes(value: &JsString) -> usize {
    value.0.capacity().saturating_mul(size_of::<u16>())
}
impl Admission {
    pub fn new() -> Self {
        Self {
            budget: Budget::default(),
        }
    }
    pub fn reserve(
        &self,
        identity: &Identity,
        method: &JsString,
        params: &IvyJson,
        duplicate: bool,
        prevent: bool,
    ) -> IvyResult<Reservation> {
        let control = prevent
            || [
                "agent.answer",
                "agent.release",
                "turn/interrupt",
                "thread/unsubscribe",
            ]
            .iter()
            .any(|name| method.eq_str(name));
        // Include owned identity capacity, task/watch/index/hash metadata and
        // known copies. Duplicate tasks retain neither params nor the action.
        // Opaque action captures retain their own lifetime and are not measured.
        let mut size = string_bytes(&identity.caller_principal_id)
            .saturating_add(string_bytes(&identity.operation_id))
            .saturating_add(size_of::<Active>())
            .saturating_add(1024);
        if !duplicate {
            size = size
                .saturating_add(params.retained_bytes())
                .saturating_add(string_bytes(method))
                .saturating_add(method.0.len().saturating_mul(2))
                .saturating_add(
                    params
                        .get("threadId")
                        .and_then(IvyJson::as_js_string)
                        .map_or(0, |id| id.0.len().saturating_mul(4)),
                );
        }
        self.budget
            .reserve(control, size)
            .map_err(|_| capacity(duplicate))
    }
}
