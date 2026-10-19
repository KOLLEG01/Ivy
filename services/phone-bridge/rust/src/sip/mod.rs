//! Phone-owned call policy. The SIP stack owns wire parsing and transactions;
//! the native call owner serializes these transitions and awaits their effects.
mod binding;
mod call;
mod delayed_offer;
mod dialog;
mod endpoint;
mod host;
mod invite;
mod registration;
mod registration_client;
mod tls;
pub(crate) use registration::same_ip;

pub use binding::{BindError, BoundSipTransport, SipBoundFlow};
pub use call::{
    CallObservation, DelayedInvite, DelayedInviteMedia, DigestCredentials, PreparedInvite,
};
pub use delayed_offer::{CallAction, CallError, CallOutcome, DelayedOfferCall, FinalPhase};
pub use dialog::{Dialog, DialogRequest, ReinviteAdmission};
pub use endpoint::{ClientExchange, SipEndpoint, SipIoError};
pub use host::{
    HostedSipMedia, SipHost, SipHostBinding, SipHostCall, SipHostCallObservation, SipHostCallPhase,
    SipHostConfiguration, SipHostError, SipHostObservation, SipRegistrationPlan,
};
pub use invite::{InviteExchange, SipError};
pub use registration::{
    IncomingPeers, RegistrationFlow, RegistrationLease, RegistrationObservation, RegistrationPeer,
    RegistrationPhase, RegistrationTransport, registration_expiry,
};
pub use registration_client::{
    RegistrationError, SipRegistration, SipRegistrationObserver, SipRegistrationSettings,
};
