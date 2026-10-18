//! Phone-owned call policy. The SIP stack owns wire parsing and transactions;
//! the native call owner serializes these transitions and awaits their effects.
mod delayed_offer;
mod dialog;
mod invite;

pub use delayed_offer::{CallAction, CallError, CallOutcome, DelayedOfferCall, FinalPhase};
pub use dialog::{Dialog, DialogRequest, ReinviteAdmission};
pub use invite::{InviteExchange, SipError};
