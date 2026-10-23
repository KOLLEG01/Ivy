//! Original in-dialog request fencing from LateOfferSipCall.Requested/Reinvite.
//! Wire parsing, response transactions and SDP remain with their adapters.

pub struct DialogRequest<'a> {
    pub call_id: &'a str,
    pub from_tag: &'a str,
    pub to_tag: &'a str,
    pub sequence: i32,
    pub branch: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReinviteAdmission {
    Accepted,
    Refused(u16),
    Foreign,
}

struct Reinvite {
    token: u64,
    sequence: i32,
    branch: String,
}

/// A dialog never borrows the mutable call owner's replacement identity.
/// No raw request, authorization or SDP is retained here.
pub struct Dialog {
    call_id: String,
    local_tag: String,
    remote_tag: String,
    remote_sequence: i32,
    reinvite: Option<Reinvite>,
}

impl Dialog {
    pub fn new(
        call_id: String,
        local_tag: String,
        remote_tag: String,
        remote_sequence: i32,
    ) -> Self {
        Self {
            call_id,
            local_tag,
            remote_tag,
            remote_sequence,
            reinvite: None,
        }
    }

    fn matches(&self, request: &DialogRequest<'_>) -> bool {
        request.call_id == self.call_id
            && request.from_tag == self.remote_tag
            && request.to_tag == self.local_tag
    }

    pub fn remote_sequence(&self) -> i32 {
        self.remote_sequence
    }

    /// None leaves a foreign dialog to its original owner. A stale BYE receives
    /// 500 and cannot change the call; only a current BYE receives 200.
    pub fn bye(&mut self, request: &DialogRequest<'_>) -> Option<u16> {
        if !self.matches(request) {
            return None;
        }
        if request.sequence <= self.remote_sequence {
            return Some(500);
        }
        self.remote_sequence = request.sequence;
        Some(200)
    }

    pub fn begin_reinvite(
        &mut self,
        request: &DialogRequest<'_>,
        token: u64,
        active: bool,
    ) -> ReinviteAdmission {
        if !self.matches(request) {
            return ReinviteAdmission::Foreign;
        }
        // Preserve the original validation order: stale/ended comes before
        // overlap, so it cannot replace a current negotiation or its timer.
        if !active || request.sequence <= self.remote_sequence {
            return ReinviteAdmission::Refused(481);
        }
        if self.reinvite.is_some() {
            return ReinviteAdmission::Refused(491);
        }
        self.remote_sequence = request.sequence;
        self.reinvite = Some(Reinvite {
            token,
            sequence: request.sequence,
            branch: request.branch.to_owned(),
        });
        ReinviteAdmission::Accepted
    }

    /// Returns the CANCEL response. A matching cancellation may have changed
    /// SDP already, so its caller must end the original call after responding.
    pub fn cancel_reinvite(&self, request: &DialogRequest<'_>) -> Option<u16> {
        if !self.matches(request) {
            return None;
        }
        Some(
            if self.reinvite.as_ref().is_some_and(|current| {
                current.sequence == request.sequence && current.branch == request.branch
            }) {
                200
            } else {
                481
            },
        )
    }

    /// ACKs can have a new branch; match the exact original transaction token,
    /// dialog and CSeq. Invalid SDP must fail the call before completing here.
    pub fn acknowledge_reinvite(&mut self, token: u64, request: &DialogRequest<'_>) -> bool {
        if !self.matches(request)
            || !self.reinvite.as_ref().is_some_and(|current| {
                current.token == token && current.sequence == request.sequence
            })
        {
            return false;
        }
        self.reinvite = None;
        true
    }

    /// Only the exact failed/cancelled transaction can release its timer slot.
    pub fn finish_reinvite(&mut self, token: u64) -> bool {
        if !self
            .reinvite
            .as_ref()
            .is_some_and(|current| current.token == token)
        {
            return false;
        }
        self.reinvite = None;
        true
    }
}
