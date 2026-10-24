//! Connection-local native request ownership. No request is retried here.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Background,
    Interaction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    Capacity,
    InvalidDeadline,
    DuplicateId,
}

impl AdmissionError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Capacity => "native_capacity",
            Self::InvalidDeadline => "invalid_arguments",
            Self::DuplicateId => "native_state_conflict",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRequest {
    pub method: String,
    pub timeout_ms: u64,
    pub detach_on_timeout: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Timeout {
    /// Reject this observation with an unknown outcome; keep the owner alive.
    Detached(PendingRequest),
    /// The RPC owner must close the connection and reject all pending requests.
    CloseConnection,
    /// A canceled or already completed timer has no effect.
    Inactive,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResponseOwner<'a> {
    Pending(&'a PendingRequest),
    /// Still validate the response envelope, then discard the late reply.
    Expired,
    Unknown,
}

#[derive(Default)]
pub struct NativeAdmission {
    // At most 64 entries; preserve the native Map's insertion order on close.
    pending: Vec<(String, PendingRequest)>,
    expired: BTreeSet<String>,
}

impl NativeAdmission {
    pub fn check_capacity(
        &self,
        detach_on_timeout: bool,
        priority: Priority,
    ) -> Result<(), AdmissionError> {
        let interaction = priority == Priority::Interaction;
        if self.pending.len() >= if interaction { 64 } else { 56 } {
            return Err(AdmissionError::Capacity);
        }
        if detach_on_timeout
            && self
                .pending
                .iter()
                .filter(|(_, p)| p.detach_on_timeout)
                .count()
                >= if interaction { 64 } else { 16 }
        {
            return Err(AdmissionError::Capacity);
        }
        Ok(())
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn expired_count(&self) -> usize {
        self.expired.len()
    }

    /// Reserve before the durable before-send hook and before writing any bytes.
    /// The caller validates catalog membership, frame size and request-ID shape.
    pub fn admit(
        &mut self,
        id: String,
        request: PendingRequest,
        priority: Priority,
    ) -> Result<(), AdmissionError> {
        self.check_capacity(request.detach_on_timeout, priority)?;
        if request.timeout_ms == 0 || request.timeout_ms > 600_000 {
            return Err(AdmissionError::InvalidDeadline);
        }
        if self.pending.iter().any(|(key, _)| key == &id) || self.expired.contains(&id) {
            return Err(AdmissionError::DuplicateId);
        }
        self.pending.push((id, request));
        Ok(())
    }

    /// A failed before-send hook releases its reservation without expiring it:
    /// no bytes were sent and the ID is not a late-response identity.
    pub fn cancel_before_send(&mut self, id: &str) -> Option<PendingRequest> {
        self.pending
            .iter()
            .position(|(key, _)| key == id)
            .map(|index| self.pending.remove(index).1)
    }

    pub fn timeout(&mut self, id: &str) -> Timeout {
        match self
            .pending
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, item)| item)
        {
            None => Timeout::Inactive,
            Some(request) if !request.detach_on_timeout => Timeout::CloseConnection,
            Some(_) => {
                let request = self
                    .cancel_before_send(id)
                    .expect("pending request was inspected");
                self.expired.insert(id.to_owned());
                Timeout::Detached(request)
            }
        }
    }

    /// Inspect without removing: before-resolve must commit before completion.
    pub fn response_owner(&self, id: &str) -> ResponseOwner<'_> {
        if let Some((_, request)) = self.pending.iter().find(|(key, _)| key == id) {
            ResponseOwner::Pending(request)
        } else if self.expired.contains(id) {
            ResponseOwner::Expired
        } else {
            ResponseOwner::Unknown
        }
    }

    /// Call only after response validation and a successful before-resolve hook.
    pub fn complete(&mut self, id: &str) -> Option<PendingRequest> {
        self.expired.remove(id);
        self.cancel_before_send(id)
    }

    pub fn close(&mut self) -> Vec<(String, PendingRequest)> {
        self.expired.clear();
        std::mem::take(&mut self.pending)
    }
}
