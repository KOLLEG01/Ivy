//! Lifecycle from LateOfferSipCall.cs, independent of SDP and transport I/O.
//! A call stays owned until protocol cleanup is confirmed. In particular, a
//! cancelled INVITE can still establish a dialog which needs an ACK then BYE.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallAction {
    Cancel,
    /// The call owner supplies either its negotiated answer or rejected media.
    Acknowledge {
        reject_media: bool,
    },
    StartMedia,
    Bye {
        authenticated: bool,
    },
    Ended,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallOutcome {
    Connected,
    Ended,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalPhase {
    Waiting,
    Negotiating,
    Acknowledging,
    AcknowledgementFailed,
    StartingMedia,
    Confirmed,
    Refused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallError {
    AlreadyAttempted,
    StaleTransaction,
    WrongPhase,
    CleanupUnconfirmed,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CallError {}

/// No credential, SDP, raw SIP or audio is retained in lifecycle observations.
/// Transaction identities are local monotonically assigned tokens, never CSeq
/// alone. Authentication keeps the original call and replaces only its token.
pub struct DelayedOfferCall {
    attempted: bool,
    sent: bool,
    transaction: u64,
    authenticated: bool,
    provisional: bool,
    ending: bool,
    active: bool,
    failed: bool,
    closed: bool,
    cancel_sent: bool,
    bye_sent: bool,
    bye_authenticated: bool,
    remote_ended: bool,
    settled: bool,
    phase: FinalPhase,
    outcome: Option<CallOutcome>,
}

impl Default for DelayedOfferCall {
    fn default() -> Self {
        Self {
            attempted: false,
            sent: false,
            transaction: 0,
            authenticated: false,
            provisional: false,
            ending: false,
            active: false,
            failed: false,
            closed: false,
            cancel_sent: false,
            bye_sent: false,
            bye_authenticated: false,
            remote_ended: false,
            settled: false,
            phase: FinalPhase::Waiting,
            outcome: None,
        }
    }
}

impl DelayedOfferCall {
    pub fn start(&mut self) -> Result<(), CallError> {
        if self.attempted || self.ending || self.closed {
            return Err(CallError::AlreadyAttempted);
        }
        self.attempted = true;
        Ok(())
    }

    /// Call only after resolution and before the original INVITE send attempt.
    /// Once true, a failed or uncertain send still owns cleanup.
    pub fn sending(&mut self, transaction: u64) -> Result<bool, CallError> {
        if !self.attempted || self.sent {
            return Err(CallError::WrongPhase);
        }
        if self.ending || self.closed {
            self.finish_answer(CallOutcome::Ended);
            self.settled = true;
            return Ok(false);
        }
        self.transaction = transaction;
        self.sent = true;
        Ok(true)
    }

    /// A resolver returning no destination has made no external SIP attempt.
    pub fn no_destination(&mut self) -> Result<(), CallError> {
        if !self.attempted || self.sent {
            return Err(CallError::WrongPhase);
        }
        self.finish_answer(CallOutcome::Ended);
        self.settled = true;
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.active && !self.ending && !self.failed
    }
    pub fn outcome(&self) -> Option<CallOutcome> {
        self.outcome
    }
    pub fn phase(&self) -> FinalPhase {
        self.phase
    }
    pub fn cleanup_confirmed(&self) -> bool {
        self.settled
    }
    pub fn is_ending(&self) -> bool {
        self.ending
    }

    fn is_current(&self, transaction: u64) -> bool {
        self.sent && self.transaction == transaction && !self.closed
    }

    pub fn provisional(&mut self, transaction: u64, requires_100rel: bool) -> Vec<CallAction> {
        if !self.is_current(transaction) || self.phase != FinalPhase::Waiting {
            return vec![];
        }
        self.provisional = true;
        if requires_100rel {
            let mut actions = self.hangup();
            actions.push(CallAction::Ended);
            actions
        } else {
            self.stop_if_requested()
        }
    }

    /// Invoke after the exchange has validated the original 401/407 and built
    /// its single authenticated continuation. No automatic credentials retry.
    pub fn authenticated(&mut self, expected: u64, next: u64) -> Result<(), CallError> {
        if !self.is_current(expected) {
            return Err(CallError::StaleTransaction);
        }
        if self.authenticated
            || self.ending
            || self.phase != FinalPhase::Waiting
            || next == expected
        {
            return Err(CallError::WrongPhase);
        }
        self.transaction = next;
        self.authenticated = true;
        self.provisional = false;
        Ok(())
    }

    /// Returns true only for a new successful final requiring SDP negotiation.
    /// A duplicate final belongs to the exchange's saved ACK, not a second
    /// negotiation or media start. Foreign transaction events are ignored.
    pub fn final_response(&mut self, transaction: u64, status: u16) -> bool {
        if !self.is_current(transaction) || self.phase != FinalPhase::Waiting {
            return false;
        }
        if !(200..=299).contains(&status) {
            self.phase = FinalPhase::Refused;
            self.ending = true;
            self.active = false;
            self.finish_answer(CallOutcome::Ended);
            self.settled = true;
            false
        } else {
            self.phase = FinalPhase::Negotiating;
            true
        }
    }

    pub fn negotiated(
        &mut self,
        accepted: bool,
        media_closed: bool,
    ) -> Result<CallAction, CallError> {
        if self.closed || self.phase != FinalPhase::Negotiating {
            return Err(CallError::WrongPhase);
        }
        self.ending |= self.failed || media_closed || !accepted;
        self.phase = FinalPhase::Acknowledging;
        Ok(CallAction::Acknowledge {
            reject_media: self.ending,
        })
    }

    /// An ACK effect must be awaited to completion by the native call owner.
    /// Cancellation sets ending but cannot overtake this completion with BYE.
    pub fn acknowledged(
        &mut self,
        success: bool,
        media_closed: bool,
    ) -> Result<Vec<CallAction>, CallError> {
        if self.closed || self.phase != FinalPhase::Acknowledging {
            return Err(CallError::WrongPhase);
        }
        if !success {
            self.phase = FinalPhase::AcknowledgementFailed;
            return Ok(self.fail());
        }
        self.phase = FinalPhase::Confirmed;
        self.ending |= self.failed || media_closed;
        if self.ending {
            self.finish_answer(CallOutcome::Ended);
            Ok(self.stop_if_requested())
        } else {
            self.phase = FinalPhase::StartingMedia;
            Ok(vec![CallAction::StartMedia])
        }
    }

    pub fn media_started(
        &mut self,
        success: bool,
        media_closed: bool,
    ) -> Result<Vec<CallAction>, CallError> {
        if self.closed || self.phase != FinalPhase::StartingMedia {
            return Err(CallError::WrongPhase);
        }
        self.phase = FinalPhase::Confirmed;
        if !success {
            return Ok(self.fail());
        }
        self.ending |= self.failed || media_closed;
        if self.ending {
            self.finish_answer(CallOutcome::Ended);
            Ok(self.stop_if_requested())
        } else {
            self.active = true;
            self.finish_answer(CallOutcome::Connected);
            Ok(vec![])
        }
    }

    pub fn hangup(&mut self) -> Vec<CallAction> {
        if self.closed {
            return vec![];
        }
        self.ending = true;
        self.active = false;
        if !self.sent {
            self.finish_answer(CallOutcome::Ended);
            self.settled = true;
        }
        self.stop_if_requested()
    }

    pub fn ring_deadline(&mut self) -> Vec<CallAction> {
        if self.phase != FinalPhase::Waiting || self.ending || self.closed {
            return vec![];
        }
        let mut actions = self.hangup();
        actions.push(CallAction::Ended);
        actions
    }

    fn stop_if_requested(&mut self) -> Vec<CallAction> {
        if !self.ending || self.closed || !self.sent {
            return vec![];
        }
        if matches!(
            self.phase,
            FinalPhase::Confirmed | FinalPhase::StartingMedia
        ) {
            if self.bye_sent || self.remote_ended {
                return vec![];
            }
            self.bye_sent = true;
            vec![CallAction::Bye {
                authenticated: false,
            }]
        } else if self.phase == FinalPhase::Waiting && self.provisional && !self.cancel_sent {
            self.cancel_sent = true;
            vec![CallAction::Cancel]
        } else {
            vec![]
        }
    }

    /// The wire adapter must first match Call-ID, both dialog tags and a newer
    /// remote CSeq. This transition never accepts a replacement dialog.
    pub fn remote_bye(&mut self) -> Vec<CallAction> {
        if self.closed || matches!(self.phase, FinalPhase::Waiting | FinalPhase::Refused) {
            return vec![];
        }
        self.remote_ended = true;
        self.ending = true;
        self.active = false;
        self.finish_answer(CallOutcome::Ended);
        self.settled = true;
        vec![CallAction::Ended]
    }

    pub fn bye_response(
        &mut self,
        status: u16,
        has_password: bool,
    ) -> Result<Vec<CallAction>, CallError> {
        if self.closed || !self.bye_sent {
            return Err(CallError::WrongPhase);
        }
        if (status == 401 || status == 407) && !self.bye_authenticated && has_password {
            self.bye_authenticated = true;
            Ok(vec![CallAction::Bye {
                authenticated: true,
            }])
        } else if (200..=299).contains(&status) || status == 481 {
            self.settled = true;
            Ok(vec![])
        } else {
            Ok(self.fail())
        }
    }

    pub fn fail(&mut self) -> Vec<CallAction> {
        if self.closed || self.settled {
            return vec![];
        }
        let notify = !self.failed;
        self.failed = true;
        self.ending = true;
        self.active = false;
        self.finish_answer(CallOutcome::Unknown);
        if !self.sent {
            self.settled = true;
        }
        if notify {
            vec![CallAction::Uncertain]
        } else {
            vec![]
        }
    }

    /// Disposal first awaits remote settlement, then any final-response work.
    /// A timeout leaves the original owner and transaction alive for cleanup.
    pub fn close(&mut self) -> Result<(), CallError> {
        if !self.settled
            || matches!(
                self.phase,
                FinalPhase::Negotiating | FinalPhase::Acknowledging | FinalPhase::StartingMedia
            )
        {
            return Err(CallError::CleanupUnconfirmed);
        }
        self.closed = true;
        self.active = false;
        Ok(())
    }

    fn finish_answer(&mut self, outcome: CallOutcome) {
        if self.outcome.is_none() {
            self.outcome = Some(outcome);
        }
    }
}
