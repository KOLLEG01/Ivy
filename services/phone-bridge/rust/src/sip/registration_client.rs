//! One registration owner drives refresh, challenge and removal transactions.
//! Observers may disappear; the original wire work and socket fence remain.
use super::invite::{bounded_secret, protected};
use super::{
    BindError, BoundSipTransport, RegistrationLease, RegistrationObservation, RegistrationPeer,
    RegistrationTransport, SipBoundFlow, registration_expiry,
};
use rsipstack::{
    dialog::authenticate::{Credential, handle_client_authenticate},
    sip::{
        Header, Method, Request, Response, Scheme, Transport, Uri, headers::*, prelude::HeadersExt,
    },
    transaction::{make_via_branch, random_text},
    transport::SipAddr,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

const ATTEMPT_LIMIT: u8 = 3;
const CYCLE_TIME: Duration = Duration::from_secs(10);
const REMOVE_TIME: Duration = Duration::from_secs(3);
// Captured original wire values, retained for compatibility during the port.
const USER_AGENT: &str = "sipsorcery_v10.0.15.0";
const ALLOW: &str =
    "ACK, BYE, CANCEL, INFO, INVITE, NOTIFY, OPTIONS, PRACK, REFER, REGISTER, SUBSCRIBE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationError {
    InvalidSettings,
    Binding(BindError),
    InUse,
    DriverFailed,
}
impl std::fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for RegistrationError {}

// No Debug: printable configuration must never include protected credentials.
pub struct SipRegistrationSettings {
    account: Uri,
    registrar: SipAddr,
    contact_user: String,
    auth_username: String,
    expiry: u64,
    retry: Duration,
    keepalive: u32,
    protocol: Transport,
}
impl SipRegistrationSettings {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        account_uri: &str,
        registrar_uri: &str,
        contact_user: String,
        auth_username: String,
        realm: Option<&str>,
        expiry_seconds: u32,
        retry_seconds: u32,
        keepalive_seconds: u32,
        protocol: Transport,
    ) -> Result<Self, RegistrationError> {
        if !matches!(protocol, Transport::Udp | Transport::Tcp | Transport::Tls)
            || !protected(account_uri, 2048)
            || !protected(registrar_uri, 2048)
            || !protected(&contact_user, 256)
            || !protected(&auth_username, 256)
            || realm.is_some_and(|realm| !protected(realm, 256))
            || !(10..=7200).contains(&expiry_seconds)
            || !(10..=600).contains(&retry_seconds)
            || keepalive_seconds > 300
        {
            return Err(RegistrationError::InvalidSettings);
        }
        let account: Uri = account_uri
            .try_into()
            .map_err(|_| RegistrationError::InvalidSettings)?;
        let registrar_uri: Uri = registrar_uri
            .try_into()
            .map_err(|_| RegistrationError::InvalidSettings)?;
        let mut registrar =
            SipAddr::try_from(&registrar_uri).map_err(|_| RegistrationError::InvalidSettings)?;
        // The stack's generic SipAddr conversion omits the sips scheme.
        // Leave absent DNS ports absent so resolution can still use SRV.
        if registrar_uri.scheme == Some(Scheme::Sips) {
            registrar.r#type = Some(Transport::Tls);
        }
        if account
            .auth
            .as_ref()
            .is_none_or(|auth| auth.user.trim().is_empty())
            || registrar.r#type.unwrap_or_default() != protocol
            || !matches!(account.scheme, Some(Scheme::Sip | Scheme::Sips))
            || !matches!(registrar_uri.scheme, Some(Scheme::Sip | Scheme::Sips))
        {
            return Err(RegistrationError::InvalidSettings);
        }
        // The original validates its Realm setting but derives the actual
        // digest realm from each challenge. It is not a caller identity.
        Ok(Self {
            account,
            registrar,
            contact_user,
            auth_username,
            expiry: expiry_seconds.into(),
            retry: Duration::from_secs(retry_seconds.into()),
            keepalive: keepalive_seconds,
            protocol,
        })
    }
}
struct State {
    lease: RegistrationLease,
    flow: Option<SipBoundFlow>,
}
struct Shared {
    state: Mutex<State>,
    origin: Instant,
    stop: CancellationToken,
}
impl Shared {
    fn now(&self) -> Duration {
        Instant::now().saturating_duration_since(self.origin)
    }
    fn stop(&self) {
        self.state.lock().unwrap().lease.stop();
        self.stop.cancel();
    }
    async fn keepalive(&self) {
        let selected = {
            let mut state = self.state.lock().unwrap();
            state
                .lease
                .keepalive_flow(self.now())
                .and_then(|token| state.flow.clone().map(|flow| (token, flow)))
        };
        if let Some((token, flow)) = selected {
            let success = flow.keepalive().await.is_ok();
            let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
                Ok(time) => i64::try_from(time.as_millis()).unwrap_or(i64::MAX),
                Err(time) => -i64::try_from(time.duration().as_millis()).unwrap_or(i64::MAX),
            };
            self.state
                .lock()
                .unwrap()
                .lease
                .keepalive_completed(token, now, success);
        }
    }
}
pub struct SipRegistration {
    shared: Arc<Shared>,
    driver: Option<JoinHandle<()>>,
    closed: bool,
}
impl SipRegistration {
    /// Native configure/reconnect owns admission of exactly one registration.
    /// A proxy is a separately validated next-hop authority, never the AOR.
    pub fn start(
        bound: Arc<BoundSipTransport>,
        settings: SipRegistrationSettings,
        password: String,
        proxy: Option<SipAddr>,
    ) -> Result<Self, RegistrationError> {
        if bound.protocol() != settings.protocol
            || !bounded_secret(&password, 4096)
            || proxy.as_ref().is_some_and(|proxy| {
                proxy.r#type.unwrap_or_default() != settings.protocol
                    || proxy
                        .addr
                        .port
                        .as_ref()
                        .is_some_and(|port| port.value() == 0)
            })
        {
            return Err(RegistrationError::InvalidSettings);
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                lease: RegistrationLease::default(),
                flow: None,
            }),
            origin: Instant::now(),
            stop: CancellationToken::new(),
        });
        let owner = shared.clone();
        let driver = tokio::spawn(async move {
            run(bound, settings, password, proxy, owner).await;
        });
        Ok(Self {
            shared,
            driver: Some(driver),
            closed: false,
        })
    }
    pub fn observation(&self) -> RegistrationObservation {
        self.shared
            .state
            .lock()
            .unwrap()
            .lease
            .observation(self.shared.now())
    }
    pub fn matches_peer(&self, peer: RegistrationPeer) -> bool {
        self.shared
            .state
            .lock()
            .unwrap()
            .lease
            .matches_peer(self.shared.now(), peer)
    }
    pub fn request_stop(&self) {
        self.shared.stop();
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    /// Dropping this waiter does not drop its driver or authorize rebinding.
    pub async fn close(&mut self) -> Result<(), RegistrationError> {
        self.request_stop();
        if let Some(driver) = self.driver.as_mut() {
            let result = tokio::time::timeout(CYCLE_TIME + REMOVE_TIME, driver)
                .await
                .map_err(|_| RegistrationError::InUse)?;
            self.driver.take();
            result.map_err(|_| RegistrationError::DriverFailed)?;
        }
        self.closed = true;
        Ok(())
    }
}
impl Drop for SipRegistration {
    fn drop(&mut self) {
        self.request_stop();
    }
}

struct WireState {
    call_id: String,
    sequence: u32,
    expiry: u64,
    registered: bool,
}
impl WireState {
    fn sequence(&mut self) -> Option<u32> {
        self.sequence = self.sequence.checked_add(1)?;
        Some(self.sequence)
    }
    fn request(
        &mut self,
        settings: &SipRegistrationSettings,
        flow: &SipBoundFlow,
    ) -> Option<(Request, Uri)> {
        let sequence = self.sequence()?;
        let mut target = settings.account.clone();
        target.auth = None;
        let scheme = settings.account.scheme.clone().unwrap_or_default();
        let transport = if settings.protocol == Transport::Udp {
            String::new()
        } else {
            format!(";transport={}", settings.protocol)
        };
        let contact: Uri = format!(
            "{scheme}:{}@{}{transport}",
            settings.contact_user,
            flow.local_addr()
        )
        .try_into()
        .ok()?;
        let tag = random_text(10);
        let branch = make_via_branch();
        let request = Request {
            method: Method::Register,
            uri: target,
            version: Default::default(),
            body: Vec::new(),
            headers: vec![
                Via::from(format!(
                    "SIP/2.0/{} {}{};rport",
                    settings.protocol,
                    flow.local_addr(),
                    branch
                ))
                .into(),
                To::from(format!("<{}>", settings.account)).into(),
                From::from(format!("<{}>;tag={tag}", settings.account)).into(),
                CallId::from(self.call_id.clone()).into(),
                CSeq::from(format!("{sequence} REGISTER")).into(),
                Contact::from(format!("<{contact}>")).into(),
                MaxForwards::from(70).into(),
                UserAgent::from(USER_AGENT).into(),
                Expires::from(self.expiry.to_string()).into(),
                Allow::from(ALLOW).into(),
                ContentLength::from(0).into(),
            ]
            .into(),
        };
        Some((request, contact))
    }
}
enum Cycle {
    Success(Response, SipBoundFlow, Uri),
    Failed(Option<u16>, bool),
    Interrupted,
}
async fn cycle(
    bound: &BoundSipTransport,
    settings: &SipRegistrationSettings,
    password: &str,
    proxy: Option<&SipAddr>,
    wire: &mut WireState,
    stop: &CancellationToken,
    deadline: Instant,
) -> Cycle {
    let next_hop = proxy.unwrap_or(&settings.registrar).clone();
    let flow = tokio::select! {
        biased;
        _=stop.cancelled()=>return Cycle::Interrupted,
        _=tokio::time::sleep_until(deadline)=>return Cycle::Failed(None,true),
        result=bound.connect_next_hop(next_hop,proxy.is_some())=>match result {Ok(flow)=>flow,Err(_)=>return Cycle::Failed(None,true)},
    };
    let mut attempts = 0;
    'initial: loop {
        if stop.is_cancelled() {
            return Cycle::Interrupted;
        }
        if attempts >= ATTEMPT_LIMIT {
            return Cycle::Failed(None, true);
        }
        attempts += 1;
        let Some((request, contact)) = wire.request(settings, &flow) else {
            return Cycle::Failed(None, true);
        };
        let mut exchange = match flow.client(request) {
            Ok(exchange) => exchange,
            Err(_) => return Cycle::Failed(None, true),
        };
        let mut authenticated = false;
        loop {
            let response = loop {
                let next = tokio::select! {
                    biased;
                    _=stop.cancelled()=>{let _=exchange.retire().await;return Cycle::Interrupted;},
                    _=tokio::time::sleep_until(deadline)=>{let _=exchange.retire().await;return Cycle::Failed(None,true);},
                    result=exchange.next_response()=>result,
                };
                match next {
                    Ok(response) if response.status_code.code() >= 200 => break response,
                    Ok(_) => {}
                    Err(_) => {
                        let _ = exchange.retire().await;
                        return Cycle::Failed(None, true);
                    }
                }
            };
            let code = response.status_code.code();
            if code == 200 {
                let confirmed = exchange
                    .final_source()
                    .and_then(|source| flow.response_flow(source).ok());
                if exchange.retire().await.is_err() {
                    return Cycle::Failed(None, true);
                }
                return match confirmed {
                    Some(flow) => Cycle::Success(response, flow, contact),
                    None => Cycle::Failed(None, true),
                };
            }
            if code == 423 && wire.expiry != 0 {
                let minimum = response.headers.iter().find_map(|h| {
                    if let Header::MinExpires(value) = h {
                        lifetime(value.value())
                    } else {
                        None
                    }
                });
                wire.expiry =
                    if minimum.is_some_and(|minimum| minimum != 0 && minimum > wire.expiry) {
                        minimum.unwrap().min(7200)
                    } else if wire.expiry < 7200 {
                        wire.expiry * 2
                    } else {
                        wire.expiry
                    };
                if exchange.retire().await.is_err() {
                    return Cycle::Failed(None, true);
                }
                continue 'initial;
            }
            if matches!(code, 401 | 407) && !authenticated {
                let header_present = response.headers.iter().any(|h| {
                    matches!(h, Header::WwwAuthenticate(_) | Header::ProxyAuthenticate(_))
                });
                if !header_present || attempts >= ATTEMPT_LIMIT {
                    let _ = exchange.retire().await;
                    return Cycle::Failed(Some(code), true);
                }
                attempts += 1;
                let Some(sequence) = wire.sequence() else {
                    let _ = exchange.retire().await;
                    return Cycle::Failed(None, true);
                };
                let credential = Credential {
                    username: settings.auth_username.clone(),
                    password: password.into(),
                    realm: None,
                    auth_username: None,
                };
                let transaction = match exchange.completed().await {
                    Ok(original) => {
                        handle_client_authenticate(sequence, original, response, &credential).await
                    }
                    Err(_) => {
                        let _ = exchange.retire().await;
                        return Cycle::Failed(None, true);
                    }
                };
                if exchange.retire().await.is_err() {
                    return Cycle::Failed(None, true);
                }
                if stop.is_cancelled() {
                    return Cycle::Interrupted;
                }
                if Instant::now() >= deadline {
                    return Cycle::Failed(None, true);
                }
                exchange = match transaction
                    .ok()
                    .and_then(|transaction| flow.endpoint().continuation(transaction, None).ok())
                {
                    Some(exchange) => exchange,
                    None => return Cycle::Failed(Some(code), false),
                };
                authenticated = true;
                continue;
            }
            let _ = exchange.retire().await;
            // Preserve the original agent's distinction: its unequivocal
            // failure branch does not erase an earlier registered flag.
            let unequivocal =
                matches!(code, 403 | 404) || authenticated && matches!(code, 401 | 402 | 407);
            return Cycle::Failed(Some(code), !unequivocal);
        }
    }
}

async fn run(
    bound: Arc<BoundSipTransport>,
    settings: SipRegistrationSettings,
    password: String,
    proxy: Option<SipAddr>,
    shared: Arc<Shared>,
) {
    let keepalive = if settings.protocol == Transport::Udp && settings.keepalive > 0 {
        let owner = shared.clone();
        let interval = Duration::from_secs(settings.keepalive.into());
        Some(tokio::spawn(async move {
            let mut timer = tokio::time::interval_at(Instant::now() + interval, interval);
            timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {biased;_=owner.stop.cancelled()=>break,_=timer.tick()=>owner.keepalive().await}
            }
        }))
    } else {
        None
    };
    let mut wire = WireState {
        call_id: random_text(32),
        sequence: 0,
        expiry: settings.expiry,
        registered: false,
    };
    loop {
        if shared.stop.is_cancelled() {
            break;
        }
        let result = cycle(
            &bound,
            &settings,
            &password,
            proxy.as_ref(),
            &mut wire,
            &shared.stop,
            Instant::now() + CYCLE_TIME,
        )
        .await;
        match result {
            Cycle::Success(response, flow, contact) => {
                let (granted, refresh) = expiry(&response, &contact, settings.expiry, wire.expiry);
                wire.expiry = refresh;
                wire.registered = true;
                let mut state = shared.state.lock().unwrap();
                state.lease.confirmed(
                    shared.now(),
                    200,
                    granted,
                    peer(flow.local_addr(), flow.protocol()),
                    peer(flow.remote_addr(), flow.protocol()),
                );
                state.flow = Some(flow);
            }
            Cycle::Failed(code, clear) => {
                if clear {
                    wire.registered = false;
                }
                shared.state.lock().unwrap().lease.failed(code);
            }
            Cycle::Interrupted => break,
        }
        let delay = if wire.registered {
            Duration::from_secs(wire.expiry.saturating_sub(5))
        } else {
            settings.retry
        };
        tokio::select! {biased;_=shared.stop.cancelled()=>break,_=tokio::time::sleep(delay)=>{}}
    }
    shared.stop();
    if let Some(keepalive) = keepalive {
        let _ = keepalive.await;
    }
    // Capture the lease token even for TCP/TLS and after local stop.
    let original = shared.state.lock().unwrap().lease.original_flow();
    if wire.registered {
        wire.expiry = 0;
        let removing = CancellationToken::new();
        if let Cycle::Success(response, _, _) = cycle(
            &bound,
            &settings,
            &password,
            proxy.as_ref(),
            &mut wire,
            &removing,
            Instant::now() + REMOVE_TIME,
        )
        .await
        {
            if let Some(original) = original {
                shared
                    .state
                    .lock()
                    .unwrap()
                    .lease
                    .removal_confirmed(&original, response.status_code.code());
            }
        }
    }
    shared.state.lock().unwrap().flow.take();
}
fn peer(address: std::net::SocketAddr, protocol: Transport) -> RegistrationPeer {
    RegistrationPeer {
        address,
        transport: match protocol {
            Transport::Tcp => RegistrationTransport::Tcp,
            Transport::Tls => RegistrationTransport::Tls,
            _ => RegistrationTransport::Udp,
        },
    }
}
fn lifetime(value: &str) -> Option<u64> {
    value
        .trim()
        .trim_matches('"')
        .parse::<i64>()
        .ok()
        .filter(|value| *value >= 0)
        .map(|value| value as u64)
}
fn contact_parts(value: &str) -> Vec<&str> {
    let (mut quote, mut escaped, mut angle, mut start) = (false, false, false, 0);
    let mut result = Vec::new();
    for (at, byte) in value.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote && byte == b'\\' {
            escaped = true;
            continue;
        }
        if byte == b'"' {
            quote = !quote;
        }
        if !quote {
            match byte {
                b'<' => angle = true,
                b'>' => angle = false,
                b',' if !angle => {
                    result.push(value[start..at].trim());
                    start = at + 1;
                }
                _ => {}
            }
        }
    }
    result.push(value[start..].trim());
    result
}
fn contact(value: &str) -> (Option<Uri>, Option<u64>) {
    let (uri, parameters) = if let Some(start) = value.find('<') {
        match value[start + 1..].find('>') {
            Some(end) => (
                &value[start + 1..start + 1 + end],
                &value[start + end + 2..],
            ),
            None => (value, ""),
        }
    } else {
        let host = value.rfind('@').map_or(0, |at| at + 1);
        let end = value[host..].find(';').map_or(value.len(), |at| host + at);
        (&value[..end], &value[end..])
    };
    let expiry = parameters
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| {
            key.eq_ignore_ascii_case("expires")
                .then(|| lifetime(value))
                .flatten()
        })
        // The original Contact accessor caps its long parameter at uint32,
        // while the general Expires header retains its full signed 64 bits.
        .map(|seconds| seconds.min(u32::MAX.into()));
    let mut uri = Uri::try_from(uri.trim()).ok();
    if let Some(uri) = &mut uri {
        uri.params.clear();
        uri.headers.clear();
    }
    (uri, expiry)
}
fn expiry(response: &Response, ours: &Uri, requested: u64, current: u64) -> (u64, u64) {
    let general = response
        .expires_header()
        .and_then(|header| lifetime(header.value()));
    let contacts = response
        .headers
        .iter()
        .filter_map(|header| {
            if let Header::Contact(value) = header {
                Some(value.value())
            } else {
                None
            }
        })
        .flat_map(contact_parts)
        .map(contact)
        .collect::<Vec<_>>();
    let observed = registration_expiry(
        requested,
        general,
        contacts.iter().map(|(_, expires)| *expires),
    );
    let mut ours = ours.clone();
    ours.params.clear();
    ours.headers.clear();
    // The original refresh agent selects FIRST matching parameterless Contact;
    // the public lease observation intentionally uses ALL Contact lifetimes.
    let refresh = contacts
        .iter()
        .find(|(uri, _)| uri.as_ref() == Some(&ours))
        .map_or_else(
            || {
                general
                    .map(|seconds| seconds.min(u32::MAX.into()))
                    .unwrap_or(current)
            },
            |(_, expires)| expires.unwrap_or(current),
        );
    (observed, refresh.clamp(10, 7200))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_public_and_refresh_lifetimes_remain_distinct() {
        let ours: Uri = "sip:contact-user@127.0.0.1:1234;transport=tcp"
            .try_into()
            .unwrap();
        for (headers, expected) in [
            (
                "Expires: 30\r\nContact: <sip:contact-user@127.0.0.1:1234;transport=udp>, <sip:other@127.0.0.2>;expires=20\r\n",
                (20, 120),
            ),
            (
                "Expires: 30\r\nContact: <sip:contact-user@127.0.0.1:1234>;expires=10, <sip:contact-user@127.0.0.1:1234>;expires=2\r\n",
                (2, 10),
            ),
            (
                "Expires: 30\r\nContact: <sip:other@127.0.0.2>;expires=8000000000\r\n",
                (u32::MAX.into(), 30),
            ),
            ("Expires: 9000000000\r\n", (9_000_000_000, 7200)),
            (
                "Expires: -1\r\nContact: <sip:other@127.0.0.2>\r\n",
                (60, 120),
            ),
            (
                "Expires: 60\r\nContact: \"Other, person\" <sip:other@127.0.0.2>;expires=0\r\n",
                (0, 60),
            ),
        ] {
            let response: Response =
                format!("SIP/2.0 200 OK\r\n{headers}Content-Length: 0\r\n\r\n")
                    .try_into()
                    .unwrap();
            assert_eq!(expiry(&response, &ours, 60, 120), expected);
        }
    }

    #[test]
    fn original_sips_transport_takes_precedence_without_bypassing_dns_port_selection() {
        for (uri, protocol) in [
            ("sip:registrar.example.test", Transport::Udp),
            ("sip:registrar.example.test;transport=tcp", Transport::Tcp),
            ("sips:registrar.example.test", Transport::Tls),
            ("sips:registrar.example.test;transport=tcp", Transport::Tls),
            ("sip:registrar.example.test;transport=tls", Transport::Tls),
        ] {
            let settings = SipRegistrationSettings::new(
                "sip:public-user@account.example.test",
                uri,
                "contact-user".into(),
                "digest-user".into(),
                None,
                60,
                10,
                0,
                protocol,
            )
            .unwrap();
            assert_eq!(settings.registrar.r#type.unwrap_or_default(), protocol);
            assert!(settings.registrar.addr.port.is_none());
        }
    }
}
