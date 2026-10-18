//! Original INVITE identity, its one authentication continuation and saved ACK.
//! Sending, cancellation and SDP negotiation belong to the native call owner.
use rsipstack::{
    dialog::authenticate::{Credential, handle_client_authenticate},
    sip::{Header, Method, Request, Response, headers::*, prelude::HeadersExt},
    transaction::{
        endpoint::EndpointInner,
        key::{TransactionKey, TransactionRole},
        transaction::Transaction,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SipError {
    InvalidInvite,
    InvalidChallenge,
    InvalidCredentials,
    InvalidAcknowledgement,
}
impl std::fmt::Display for SipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SipError {}

// Deliberately no Debug: requests may contain protected digest headers.
pub struct InviteExchange {
    original: Request,
    key: TransactionKey,
    authenticated: bool,
    final_response: Option<Response>,
    acknowledgement: Option<Request>,
}

impl InviteExchange {
    pub fn new(original: Request) -> Result<Self, SipError> {
        let valid = original.method == Method::Invite
            && original.cseq_header().and_then(|h| h.method()).ok() == Some(Method::Invite)
            && original
                .call_id_header()
                .is_ok_and(|h| !h.value().trim().is_empty())
            && original
                .from_header()
                .and_then(|h| h.tag())
                .ok()
                .flatten()
                .is_some_and(|tag| !tag.value().trim().is_empty())
            && (original.body.is_empty()
                || !String::from_utf8_lossy(&original.body).trim().is_empty());
        if !valid {
            return Err(SipError::InvalidInvite);
        }
        let key = TransactionKey::from_request(&original, TransactionRole::Client)
            .map_err(|_| SipError::InvalidInvite)?;
        Ok(Self {
            original,
            key,
            authenticated: false,
            final_response: None,
            acknowledgement: None,
        })
    }

    pub fn request(&self) -> &Request {
        &self.original
    }

    /// Uses the stack's digest implementation; it never sends the continuation.
    /// The caller retains both transactions until the original cleanup settles.
    pub async fn authenticate(
        &mut self,
        expected: &Transaction,
        challenge: &Response,
        username: &str,
        password: &str,
    ) -> Result<Transaction, SipError> {
        if expected.key != self.key
            || self.authenticated
            || !matches!(challenge.status_code.code(), 401 | 407)
            || !self.matches(challenge)
            || !challenge
                .headers
                .iter()
                .any(|h| matches!(h, Header::WwwAuthenticate(_) | Header::ProxyAuthenticate(_)))
        {
            return Err(SipError::InvalidChallenge);
        }
        if !protected(username, 256) || !protected(password, 4096) {
            return Err(SipError::InvalidCredentials);
        }
        let sequence = expected
            .original
            .cseq_header()
            .and_then(|h| h.seq())
            .ok()
            .and_then(|s| s.checked_add(1))
            .ok_or(SipError::InvalidChallenge)?;
        let credential = Credential {
            username: username.to_owned(),
            password: password.to_owned(),
            realm: None,
            auth_username: None,
        };
        let continuation =
            handle_client_authenticate(sequence, expected, challenge.clone(), &credential)
                .await
                .map_err(|_| SipError::InvalidChallenge)?;
        self.key = continuation.key.clone();
        self.original = continuation.original.clone();
        self.authenticated = true;
        self.final_response = None;
        self.acknowledgement = None;
        Ok(continuation)
    }

    fn matches(&self, response: &Response) -> bool {
        TransactionKey::from_response(response, TransactionRole::Client)
            .is_ok_and(|key| key == self.key)
    }

    /// Select exactly one successful dialog. Retransmissions reuse its ACK;
    /// a foreign/forked dialog cannot borrow this answer or media authority.
    pub fn select_final(&mut self, response: &Response) -> Result<(), SipError> {
        if !self.matches(response)
            || !(200..=299).contains(&response.status_code.code())
            || self.final_response.is_some()
        {
            return Err(SipError::InvalidAcknowledgement);
        }
        self.final_response = Some(response.clone());
        Ok(())
    }

    fn matches_final(&self, response: &Response) -> bool {
        self.matches(response)
            && (200..=299).contains(&response.status_code.code())
            && self.final_response.as_ref().is_some_and(|selected| {
                selected.to_header().and_then(|h| h.tag()).ok().flatten()
                    == response.to_header().and_then(|h| h.tag()).ok().flatten()
            })
    }

    /// Save the exact ACK before its send is attempted. The call owner awaits
    /// that send before media startup/BYE; a failed send cannot authorize them.
    /// Initial offers use an empty ACK; only a delayed offer can supply SDP.
    pub fn prepare_ack(
        &mut self,
        response: &Response,
        body: Option<&str>,
        endpoint: &EndpointInner,
    ) -> Result<Request, SipError> {
        if !self.matches_final(response)
            || self.acknowledgement.is_some()
            || (!self.original.body.is_empty() && body.is_some())
        {
            return Err(SipError::InvalidAcknowledgement);
        }
        let mut selected = response.clone();
        // Preserve the original fallback when a peer omitted Contact.
        if selected.contact_header().is_err() {
            selected
                .headers
                .unique_push(Contact::new(format!("<{}>", self.original.uri)).into());
        }
        let mut ack = endpoint
            .make_ack(&self.original, &selected)
            .map_err(|_| SipError::InvalidAcknowledgement)?;
        // From belongs to the original request, even if a response repeats a
        // different display name/URI with the same transaction tag.
        ack.headers.unique_push(
            self.original
                .from_header()
                .map_err(|_| SipError::InvalidAcknowledgement)?
                .clone()
                .into(),
        );
        if !response
            .headers
            .iter()
            .any(|h| matches!(h, Header::RecordRoute(_)))
        {
            ack.headers.retain(|h| !matches!(h, Header::Route(_)));
            ack.headers.extend(
                self.original
                    .headers
                    .iter()
                    .filter(|h| matches!(h, Header::Route(_)))
                    .cloned()
                    .collect(),
            );
        }
        ack.body = body.unwrap_or("").as_bytes().to_vec();
        ack.headers
            .retain(|h| !matches!(h, Header::ContentType(_) | Header::ContentLength(_)));
        ack.headers
            .unique_push(ContentLength::from(ack.body.len() as u32).into());
        if body.is_some() {
            ack.headers
                .unique_push(ContentType::new("application/sdp").into());
        }
        self.acknowledgement = Some(ack.clone());
        Ok(ack)
    }

    pub fn ack_for_retransmission(&self, response: &Response) -> Option<&Request> {
        if self.matches_final(response) {
            self.acknowledgement.as_ref()
        } else {
            None
        }
    }

    /// RFC3261 CANCEL keeps the original branch/CSeq and contains no SDP or
    /// authorization. The lifecycle permits it only after a provisional reply.
    pub fn cancel_request(&self) -> Result<Request, SipError> {
        let mut cancel = self.original.clone();
        let sequence = cancel
            .cseq_header()
            .and_then(|h| h.seq())
            .map_err(|_| SipError::InvalidInvite)?;
        cancel.method = Method::Cancel;
        cancel.body.clear();
        cancel.headers.retain(|h| {
            !matches!(
                h,
                Header::Authorization(_)
                    | Header::ProxyAuthorization(_)
                    | Header::WwwAuthenticate(_)
                    | Header::ProxyAuthenticate(_)
                    | Header::ContentType(_)
                    | Header::ContentLength(_)
            )
        });
        cancel
            .headers
            .unique_push(CSeq::new(format!("{sequence} CANCEL")).into());
        cancel.headers.unique_push(ContentLength::from(0).into());
        Ok(cancel)
    }
}

pub(super) fn protected(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty()
        && value.encode_utf16().count() <= maximum
        && !value.chars().any(char::is_control)
}
