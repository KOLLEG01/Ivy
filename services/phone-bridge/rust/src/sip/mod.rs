//! Phone-owned call policy. The SIP stack owns wire parsing and transactions;
//! the native call owner serializes these transitions and awaits their effects.
mod call;
mod delayed_offer;
mod dialog;
mod endpoint;
mod invite;
mod registration;

pub use call::{
    CallObservation, DelayedInvite, DelayedInviteMedia, DigestCredentials, PreparedInvite,
};
pub use delayed_offer::{CallAction, CallError, CallOutcome, DelayedOfferCall, FinalPhase};
pub use dialog::{Dialog, DialogRequest, ReinviteAdmission};
pub use endpoint::{ClientExchange, SipEndpoint, SipIoError};
pub use invite::{InviteExchange, SipError};
pub use registration::{
    IncomingPeers, RegistrationFlow, RegistrationLease, RegistrationObservation, RegistrationPeer,
    RegistrationPhase, RegistrationTransport, registration_expiry,
};
