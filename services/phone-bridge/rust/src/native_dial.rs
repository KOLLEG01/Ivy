//! Protected native dial input. Decode the DTO before looking up the call;
//! validate the descriptor before reserving its one original attempt.
use crate::{
    native_config::NativeConfigError,
    native_rpc::{NativeRequest, root_values},
    native_sip::NativeSipError,
    sip::{DigestCredentials, SipRegistrationSettings},
};
use rsipstack::{
    sip::{Method, Request, Scheme, Transport, Uri, headers::*},
    transaction::{make_via_branch, random_text},
    transport::SipAddr,
};
use std::net::SocketAddr;

// No Debug: these inputs include credentials and are never status/receipt data.
pub(crate) struct NativeDial {
    destination: Option<String>,
    username: Option<String>,
    password: Option<String>,
    pub ring_seconds: i32,
    pub waiting: bool,
}
fn invalid() -> NativeSipError {
    NativeConfigError::InvalidRequest.into()
}
fn unknown() -> NativeSipError {
    NativeConfigError::OutcomeUnknown.into()
}
impl NativeDial {
    pub fn parse(request: &NativeRequest) -> Result<Self, NativeSipError> {
        let fields = request.params().as_object().ok_or_else(invalid)?;
        let names = [
            "callId",
            "destination",
            "username",
            "password",
            "ringSeconds",
            "waiting",
        ];
        if fields.len() != names.len() || names.iter().any(|n| request.params().get(n).is_none()) {
            return Err(invalid());
        }
        let string = |name| {
            let v = request.params().get(name).unwrap();
            if v.is_null() {
                Ok(None)
            } else {
                v.as_str().map(Some).map_err(|_| invalid())
            }
        };
        string("callId")?;
        let raw = root_values(request.raw_params());
        let i = fields
            .iter()
            .position(|(k, _)| k == &"ringSeconds".into())
            .unwrap();
        Ok(Self {
            destination: string("destination")?,
            username: string("username")?,
            password: string("password")?,
            ring_seconds: raw[i].parse::<i32>().map_err(|_| invalid())?,
            waiting: request
                .params()
                .get("waiting")
                .unwrap()
                .as_bool()
                .ok_or_else(invalid)?,
        })
    }
    pub fn descriptor(
        &self,
        registration: Option<&SipRegistrationSettings>,
    ) -> Result<DialDescriptor, NativeSipError> {
        let bounded =
            |s: &str, n| s.encode_utf16().count() <= n && !s.chars().any(char::is_control);
        let destination = self
            .destination
            .as_deref()
            .filter(|s| bounded(s, 2048))
            .ok_or_else(invalid)?;
        if !(1..=120).contains(&self.ring_seconds)
            || self.username.as_ref().is_some_and(|s| !bounded(s, 256))
            || self.password.as_ref().is_some_and(|s| !bounded(s, 4096))
        {
            return Err(invalid());
        }
        // ParseSIPURI requires an explicit scheme; a malformed URI is a native
        // operation error, distinct from the bounded argument validation above.
        let uri: Uri = destination.try_into().map_err(|_| unknown())?;
        if !matches!(uri.scheme, Some(Scheme::Sip | Scheme::Sips)) {
            return Err(unknown());
        }
        let registered = registration.map(SipRegistrationSettings::dial_identity);
        let username = registered
            .map(|(_, u, _)| u)
            .or(self.username.as_deref())
            .unwrap_or("Anonymous");
        let auth = registered.map(|(_, _, u)| u).or(self.username.as_deref());
        let from = if let Some((account, _, _)) = registered {
            account.to_string()
        } else if self.username.as_ref().is_none_or(|s| s.trim().is_empty()) {
            "sip:thisis@anonymous.invalid".into()
        } else {
            format!(
                "{}:{}@{}",
                uri.scheme.as_ref().unwrap(),
                self.username.as_ref().unwrap(),
                uri.host_with_port
            )
        };
        let password = if self.password.as_ref().is_none_or(String::is_empty)
            && auth.is_some_and(|a| a.eq_ignore_ascii_case("anonymous@t-online.de"))
        {
            "ivy-access-line-auth"
        } else {
            self.password.as_deref().unwrap_or("")
        };
        let credentials = auth
            .filter(|s| !s.trim().is_empty())
            .map(|u| DigestCredentials::new(u.into(), password.into()).map_err(|_| invalid()))
            .transpose()?;
        let mut canonical = uri.clone();
        canonical.params.clear();
        canonical.headers.clear();
        if canonical.host_with_port.port.is_none() {
            canonical.host_with_port.port = Some(
                (if uri.scheme == Some(Scheme::Sips) {
                    5061
                } else {
                    5060
                })
                .into(),
            );
        }
        let mut next_hop = SipAddr::try_from(&uri).map_err(|_| unknown())?;
        if uri.scheme == Some(Scheme::Sips) {
            next_hop.r#type = Some(Transport::Tls);
        }
        Ok(DialDescriptor {
            uri,
            from,
            to: canonical.to_string(),
            username: username.into(),
            credentials,
            next_hop,
        })
    }
}
pub(crate) struct DialDescriptor {
    uri: Uri,
    from: String,
    to: String,
    username: String,
    pub credentials: Option<DigestCredentials>,
    pub next_hop: SipAddr,
}
impl DialDescriptor {
    pub fn request(
        &self,
        id: &str,
        local: SocketAddr,
        protocol: Transport,
        body: String,
    ) -> Request {
        let tag = random_text(10);
        let transport = if protocol == Transport::Udp {
            String::new()
        } else {
            format!(";transport={}", protocol.to_string().to_ascii_lowercase())
        };
        let mut headers = vec![
            Via::from(format!(
                "SIP/2.0/{protocol} {local}{};rport",
                make_via_branch()
            ))
            .into(),
            From::from(format!("<{}>;tag={tag}", self.from)).into(),
            To::from(format!("<{}>", self.to)).into(),
            CallId::from(id).into(),
            CSeq::from("1 INVITE").into(),
            Contact::from(format!(
                "<{}:{}@{local}{transport}>",
                self.uri.scheme.as_ref().unwrap(),
                self.username
            ))
            .into(),
            MaxForwards::from(70).into(),
            UserAgent::from(if body.is_empty() {
                "IvyPhone"
            } else {
                "sipsorcery_v10.0.15.0"
            })
            .into(),
            ContentLength::from(body.len() as u32).into(),
        ];
        if !body.is_empty() {
            headers.push(ContentType::from("application/sdp").into());
        }
        Request {
            method: Method::Invite,
            uri: self.uri.clone(),
            version: Default::default(),
            headers: headers.into(),
            body: body.into_bytes(),
        }
    }
}
