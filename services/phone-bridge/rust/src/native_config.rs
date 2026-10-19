//! The native configuration DTO, decoded before the configured-once latch.
//! Credentials and installed paths are deliberately not Debug/serializable.
use crate::{
    native_rpc::{NativeRequest, root_values},
    sip::{
        RegistrationPeer, SipHost, SipHostBinding, SipHostConfiguration, SipRegistrationPlan,
        SipRegistrationSettings, same_ip,
    },
};
use ivy_contracts::IvyJson;
use rsipstack::{sip::Transport, transport::SipAddr};
use std::{
    collections::HashSet,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeConfigError {
    InvalidRequest,
    OutcomeUnknown,
}
impl NativeConfigError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::OutcomeUnknown => "operation_outcome_unknown",
        }
    }
}
type Result<T> = std::result::Result<T, NativeConfigError>;
static NULL: IvyJson = IvyJson::Null;
use NativeConfigError::{InvalidRequest, OutcomeUnknown};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    G722,
    Pcma,
    Pcmu,
    Opus,
    Evs,
}
impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Self::G722 => "G722",
            Self::Pcma => "PCMA",
            Self::Pcmu => "PCMU",
            Self::Opus => "OPUS",
            Self::Evs => "EVS",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCodecSettings {
    preferences: Vec<Codec>,
    receive_reorder_ms: u8,
}
impl NativeCodecSettings {
    pub fn preferences(&self) -> &[Codec] {
        &self.preferences
    }
    pub fn packet_ms(&self) -> u8 {
        20
    }
    pub fn receive_reorder_ms(&self) -> u8 {
        self.receive_reorder_ms
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferMode {
    Initial,
    Delayed,
    Auto,
}
pub struct NativeSipBinding {
    endpoint: SocketAddr,
    transport: Transport,
    ring_seconds: u8,
    offer: OfferMode,
    rtp_start: u16,
    rtp_end: u16,
    proxy: Option<SipAddr>,
    max_calls: u8,
}
impl NativeSipBinding {
    pub fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }
    pub fn transport(&self) -> Transport {
        self.transport
    }
    pub fn incoming_ring_seconds(&self) -> u8 {
        self.ring_seconds
    }
    pub fn offer_mode(&self) -> OfferMode {
        self.offer
    }
    pub fn rtp_port_range(&self) -> (u16, u16) {
        (self.rtp_start, self.rtp_end)
    }
    pub fn outbound_proxy(&self) -> Option<&SipAddr> {
        self.proxy.as_ref()
    }
    pub fn max_concurrent_calls(&self) -> u8 {
        self.max_calls
    }
}
pub struct NativeIncomingPeers {
    addresses: Vec<SocketAddr>,
    registered: bool,
}
impl NativeIncomingPeers {
    /// Ports do not affect admission. IPv6 scope remains part of IP identity.
    pub fn allows(&self, peer: RegistrationPeer, host: &SipHost) -> bool {
        self.addresses
            .iter()
            .any(|address| same_ip(*address, peer.address))
            || self.registered && host.matches_registered_peer(peer)
    }
    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }
    pub fn uses_registered_peer(&self) -> bool {
        self.registered
    }
}
pub struct NativeMicroSettings {
    executable: String,
    hash: String,
    port: u16,
}
impl NativeMicroSettings {
    pub fn executable(&self) -> &str {
        &self.executable
    }
    pub fn executable_hash(&self) -> &str {
        &self.hash
    }
    pub fn port(&self) -> u16 {
        self.port
    }
}
/// All pieces must be retained by the native operations owner. A caller must
/// prepare optional Micro after binding and before starting registration.
/// This DTO does not silently discard native media/admission configuration.
pub struct NativeSipConfiguration {
    binding: NativeSipBinding,
    codecs: NativeCodecSettings,
    incoming: NativeIncomingPeers,
    registration: Option<SipRegistrationPlan>,
    micro: Option<NativeMicroSettings>,
}
pub struct NativeConfigurationParts {
    pub host: SipHostConfiguration,
    pub binding: NativeSipBinding,
    pub codecs: NativeCodecSettings,
    pub incoming: NativeIncomingPeers,
    pub micro: Option<NativeMicroSettings>,
}
impl NativeSipConfiguration {
    pub fn binding(&self) -> &NativeSipBinding {
        &self.binding
    }
    pub fn codecs(&self) -> &NativeCodecSettings {
        &self.codecs
    }
    pub fn incoming_peers(&self) -> &NativeIncomingPeers {
        &self.incoming
    }
    pub fn micro(&self) -> Option<&NativeMicroSettings> {
        self.micro.as_ref()
    }
    pub fn uses_delayed_offer(&self) -> bool {
        self.binding.offer == OfferMode::Delayed
            || self.binding.offer == OfferMode::Auto && self.codecs.preferences[0] == Codec::Evs
    }
    pub fn has_registration(&self) -> bool {
        self.registration.is_some()
    }
    /// Only a validated native envelope supplies raw Int32 lexemes. JSON
    /// floating/exponent spellings cannot become integers through f64 rounding.
    pub fn parse(request: &NativeRequest) -> Result<Self> {
        if request.method() != "configure" {
            return Err(InvalidRequest);
        }
        let fields = Fields::new(
            request.params(),
            request.raw_params(),
            &[
                "binding",
                "codecs",
                "incomingPeers",
                "registration",
                "password",
                "micro",
            ],
        )?;
        for name in [
            "binding",
            "codecs",
            "incomingPeers",
            "registration",
            "password",
        ] {
            fields.required(name)?;
        }
        // JsonSerializer converts all record fields before any Validate call.
        shapes(&fields)?;
        if fields.required("binding")?.is_null() || fields.required("codecs")?.is_null() {
            return Err(InvalidRequest);
        }
        let peers = fields
            .required("incomingPeers")?
            .as_array()
            .ok_or(InvalidRequest)?;
        if peers.len() > 32 {
            return Err(InvalidRequest);
        }
        let mut seen = HashSet::new();
        for peer in peers {
            if !seen.insert(if peer.is_null() {
                None
            } else {
                Some(peer.as_str().map_err(|_| InvalidRequest)?)
            }) {
                return Err(InvalidRequest);
            }
        }
        let mut addresses = Vec::new();
        let mut registered = false;
        for peer in peers {
            let peer = peer.as_str().map_err(|_| InvalidRequest)?;
            if peer == "registered" {
                registered = true;
            } else {
                let address = parse_ip(&peer).ok_or(OutcomeUnknown)?;
                if !addresses.contains(&address) {
                    addresses.push(address);
                }
            }
        }
        let binding = parse_binding(fields.child("binding", BINDING_FIELDS)?)?;
        let codecs = parse_codecs(
            fields.child("codecs", &["preferences", "packetMs", "receiveReorderMs"])?,
        )?;
        let micro = if fields.required_or_null("micro").is_null() {
            None
        } else {
            Some(parse_micro(fields.child(
                "micro",
                &["usbipExecutable", "executableHash", "port"],
            )?)?)
        };
        let password = fields.nullable_string("password")?;
        let registration = if fields.required("registration")?.is_null() {
            if password.is_some() {
                return Err(InvalidRequest);
            }
            None
        } else {
            let reg = fields.child("registration", REGISTRATION_FIELDS)?;
            let account = reg.string("accountUri")?;
            let registrar = reg.string("registrarUri")?;
            let contact = reg.string("contactUser")?;
            let auth = reg.string("authUsername")?;
            let realm = reg.nullable_string("realm")?;
            let expiry = reg.int("expirySeconds", 0)?;
            let retry = reg.int("retrySeconds", 0)?;
            let keepalive = reg.int("keepAliveIntervalSeconds", 20)?;
            if !(10..=7200).contains(&expiry)
                || !(10..=600).contains(&retry)
                || !(0..=300).contains(&keepalive)
            {
                return Err(InvalidRequest);
            }
            let settings = SipRegistrationSettings::new(
                &account,
                &registrar,
                contact,
                auth,
                realm.as_deref(),
                expiry as u32,
                retry as u32,
                keepalive as u32,
                binding.transport,
            )
            .map_err(|_| InvalidRequest)?;
            Some(
                SipRegistrationPlan::new(settings, password.ok_or(InvalidRequest)?)
                    .map_err(|_| InvalidRequest)?,
            )
        };
        Ok(Self {
            binding,
            codecs,
            incoming: NativeIncomingPeers {
                addresses,
                registered,
            },
            registration,
            micro,
        })
    }
    pub fn into_parts(self) -> NativeConfigurationParts {
        let host = SipHostConfiguration::new(
            SipHostBinding::new(
                self.binding.endpoint,
                self.binding.transport,
                self.binding.proxy.clone(),
                self.binding.max_calls.into(),
            )
            .expect("validated binding"),
            self.registration,
        )
        .expect("validated registration transport");
        NativeConfigurationParts {
            host,
            binding: self.binding,
            codecs: self.codecs,
            incoming: self.incoming,
            micro: self.micro,
        }
    }
}
const BINDING_FIELDS: &[&str] = &[
    "address",
    "port",
    "transport",
    "incomingRingSeconds",
    "outgoingOfferMode",
    "rtpPortRangeStart",
    "rtpPortRangeEnd",
    "outboundProxy",
    "maxConcurrentCalls",
];
const REGISTRATION_FIELDS: &[&str] = &[
    "accountUri",
    "registrarUri",
    "contactUser",
    "authUsername",
    "realm",
    "expirySeconds",
    "retrySeconds",
    "keepAliveIntervalSeconds",
];
fn shapes(root: &Fields<'_>) -> Result<()> {
    root.nullable_string("password")?;
    for name in ["incomingPeers"] {
        let array = root.required_or_null(name);
        if !array.is_null() {
            for value in array.as_array().ok_or(InvalidRequest)? {
                if !value.is_null() {
                    value.as_str().map_err(|_| InvalidRequest)?;
                }
            }
        }
    }
    for (name, allowed, strings, integers) in [
        (
            "binding",
            BINDING_FIELDS,
            &["address", "transport", "outgoingOfferMode", "outboundProxy"][..],
            &[
                "port",
                "incomingRingSeconds",
                "rtpPortRangeStart",
                "rtpPortRangeEnd",
                "maxConcurrentCalls",
            ][..],
        ),
        (
            "codecs",
            &["preferences", "packetMs", "receiveReorderMs"][..],
            &[][..],
            &["packetMs", "receiveReorderMs"][..],
        ),
        (
            "registration",
            REGISTRATION_FIELDS,
            &[
                "accountUri",
                "registrarUri",
                "contactUser",
                "authUsername",
                "realm",
            ][..],
            &["expirySeconds", "retrySeconds", "keepAliveIntervalSeconds"][..],
        ),
        (
            "micro",
            &["usbipExecutable", "executableHash", "port"][..],
            &["usbipExecutable", "executableHash"][..],
            &["port"][..],
        ),
    ] {
        if root.required_or_null(name).is_null() {
            continue;
        }
        let fields = root.child(name, allowed)?;
        for field in strings {
            fields.nullable_string(field)?;
        }
        for field in integers {
            fields.int(field, 0)?;
        }
        if name == "codecs" {
            let prefs = fields.required_or_null("preferences");
            if !prefs.is_null() {
                for value in prefs.as_array().ok_or(InvalidRequest)? {
                    if !value.is_null() {
                        value.as_str().map_err(|_| InvalidRequest)?;
                    }
                }
            }
        }
    }
    Ok(())
}
struct Fields<'a> {
    value: &'a IvyJson,
    raw: Vec<&'a str>,
}
impl<'a> Fields<'a> {
    fn new(value: &'a IvyJson, raw: &'a str, allowed: &[&str]) -> Result<Self> {
        let object = value.as_object().ok_or(InvalidRequest)?;
        if object
            .iter()
            .any(|(key, _)| !allowed.iter().any(|name| key == &(*name).into()))
        {
            return Err(InvalidRequest);
        }
        Ok(Self {
            value,
            raw: root_values(raw),
        })
    }
    fn required(&self, name: &str) -> Result<&'a IvyJson> {
        self.value.get(name).ok_or(InvalidRequest)
    }
    fn required_or_null(&self, name: &str) -> &'a IvyJson {
        self.value.get(name).unwrap_or(&NULL)
    }
    fn child(&self, name: &str, allowed: &[&str]) -> Result<Fields<'a>> {
        let index = self
            .value
            .as_object()
            .unwrap()
            .iter()
            .position(|(key, _)| key == &name.into())
            .ok_or(InvalidRequest)?;
        Self::new(self.required(name)?, self.raw[index], allowed)
    }
    fn string(&self, name: &str) -> Result<String> {
        self.required(name)?.as_str().map_err(|_| InvalidRequest)
    }
    fn nullable_string(&self, name: &str) -> Result<Option<String>> {
        let value = self.required_or_null(name);
        if value.is_null() {
            Ok(None)
        } else {
            value.as_str().map(Some).map_err(|_| InvalidRequest)
        }
    }
    fn int(&self, name: &str, default: i32) -> Result<i32> {
        let Some(index) = self
            .value
            .as_object()
            .unwrap()
            .iter()
            .position(|(key, _)| key == &name.into())
        else {
            return Ok(default);
        };
        self.raw[index].parse::<i32>().map_err(|_| InvalidRequest)
    }
}
fn parse_binding(fields: Fields<'_>) -> Result<NativeSipBinding> {
    let address = fields.string("address")?;
    let port = fields.int("port", 0)?;
    let transport = match fields.string("transport")?.as_str() {
        "udp" => Transport::Udp,
        "tcp" => Transport::Tcp,
        "tls" => Transport::Tls,
        _ => return Err(InvalidRequest),
    };
    let ring = fields.int("incomingRingSeconds", 30)?;
    let offer = match fields.nullable_string("outgoingOfferMode")?.as_deref() {
        None if fields.value.get("outgoingOfferMode").is_none() => OfferMode::Initial,
        Some("initial") => OfferMode::Initial,
        Some("delayed") => OfferMode::Delayed,
        Some("auto") => OfferMode::Auto,
        _ => return Err(InvalidRequest),
    };
    let start = fields.int("rtpPortRangeStart", 30000)?;
    let end = fields.int("rtpPortRangeEnd", 31000)?;
    let calls = fields.int("maxConcurrentCalls", 1)?;
    if !(0..=65535).contains(&port)
        || !(1..=120).contains(&ring)
        || !(1024..=65534).contains(&start)
        || !(start..=65534).contains(&end)
        || start % 2 != 0
        || end % 2 != 0
        || !(1..=32).contains(&calls)
    {
        return Err(InvalidRequest);
    }
    let mut endpoint = parse_ip(&address).ok_or(InvalidRequest)?;
    endpoint.set_port(port as u16);
    let proxy = fields
        .nullable_string("outboundProxy")?
        .map(|value| parse_proxy(&value, transport))
        .transpose()?;
    Ok(NativeSipBinding {
        endpoint,
        transport,
        ring_seconds: ring as u8,
        offer,
        rtp_start: start as u16,
        rtp_end: end as u16,
        proxy,
        max_calls: calls as u8,
    })
}
fn parse_codecs(fields: Fields<'_>) -> Result<NativeCodecSettings> {
    let values = fields
        .required("preferences")?
        .as_array()
        .ok_or(InvalidRequest)?;
    let packet = fields.int("packetMs", 20)?;
    let reorder = fields.int("receiveReorderMs", 20)?;
    if packet != 20 || !(0..=60).contains(&reorder) || !(1..=5).contains(&values.len()) {
        return Err(InvalidRequest);
    }
    let mut preferences = Vec::new();
    for value in values {
        let codec = match value.as_str().map_err(|_| InvalidRequest)?.as_str() {
            "G722" => Codec::G722,
            "PCMA" => Codec::Pcma,
            "PCMU" => Codec::Pcmu,
            "OPUS" => Codec::Opus,
            "EVS" => Codec::Evs,
            _ => return Err(InvalidRequest),
        };
        if preferences.contains(&codec) {
            return Err(InvalidRequest);
        }
        preferences.push(codec);
    }
    Ok(NativeCodecSettings {
        preferences,
        receive_reorder_ms: reorder as u8,
    })
}
fn parse_micro(fields: Fields<'_>) -> Result<NativeMicroSettings> {
    let executable = fields.string("usbipExecutable")?;
    let hash = fields.string("executableHash")?;
    let port = fields.int("port", 0)?;
    let bytes = executable.as_bytes();
    let sep = |b: u8| b == b'\\' || b == b'/';
    let qualified = bytes.len() >= 2 && sep(bytes[0]) && sep(bytes[1])
        || bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && sep(bytes[2]);
    let name = windows_filename(&executable);
    // .NET's $ anchor also accepts a single final LF; later pin enforcement
    // still compares the original string, not this validation-only view.
    let hash_value = hash.strip_suffix('\n').unwrap_or(&hash);
    if executable.encode_utf16().count() > 32767
        || !qualified
        || !name.eq_ignore_ascii_case("usbip.exe")
        || hash_value.len() != 71
        || !hash_value.starts_with("sha256:")
        || !hash_value[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !(1024..=65535).contains(&port)
    {
        return Err(InvalidRequest);
    }
    Ok(NativeMicroSettings {
        executable,
        hash,
        port: port as u16,
    })
}
fn windows_filename(path: &str) -> &str {
    let bytes = path.as_bytes();
    let sep = |b: u8| b == b'\\' || b == b'/';
    let mut root = if bytes.len() >= 2 && bytes[1] == b':' {
        2
    } else {
        0
    };
    if bytes.len() >= 2 && sep(bytes[0]) && sep(bytes[1]) {
        let device = bytes.len() >= 4 && matches!(bytes[2], b'?' | b'.') && sep(bytes[3]);
        let unc = if device
            && bytes
                .get(4..7)
                .is_some_and(|part| part.eq_ignore_ascii_case(b"UNC"))
            && bytes.get(7).is_some_and(|b| sep(*b))
        {
            Some(8)
        } else if device {
            root = 4;
            None
        } else {
            Some(2)
        };
        if let Some(start) = unc {
            root = bytes.len();
            if let Some(server) = bytes[start..].iter().position(|b| sep(*b)) {
                let share = start + server + 1;
                if let Some(end) = bytes[share..].iter().position(|b| sep(*b)) {
                    root = share + end;
                }
            }
        }
    }
    let start = bytes
        .iter()
        .rposition(|b| sep(*b))
        .map_or(root, |at| root.max(at + 1));
    &path[start..]
}
// Windows System.Net's literal parser, including its legacy IPv4 forms. Scope
// IDs remain attached to IPv6; a nonnumeric scope has the original zero value.
fn parse_ip(value: &str) -> Option<SocketAddr> {
    if !value.contains(':') && !value.starts_with('[') {
        return ipv4(value).map(|ip| SocketAddr::new(ip.into(), 0));
    }
    let value = if let Some(value) = value.strip_prefix('[') {
        let (address, suffix) = value.split_once(']')?;
        if !suffix.is_empty()
            && (!suffix.starts_with(':') || !suffix[1..].bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        address
    } else {
        value
    };
    let (ip, scope) = value.split_once('%').map_or((value, 0), |(ip, scope)| {
        (ip, scope.parse::<u32>().unwrap_or(0))
    });
    let ip = if ip.contains('.') {
        let colon = ip.rfind(':')?;
        let v4 = ipv4_decimal(&ip[colon + 1..])?;
        format!("{}{}", &ip[..=colon], v4)
            .parse::<Ipv6Addr>()
            .ok()?
    } else {
        ip.parse::<Ipv6Addr>().ok()?
    };
    Some(SocketAddr::V6(SocketAddrV6::new(ip, 0, 0, scope)))
}
fn ipv4(value: &str) -> Option<Ipv4Addr> {
    let parts = value.split('.').collect::<Vec<_>>();
    if !(1..=4).contains(&parts.len()) {
        return None;
    }
    let mut total = 0u32;
    for (index, part) in parts.iter().enumerate() {
        let (radix, digits) =
            if let Some(rest) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
                (16, rest)
            } else if part.len() > 1 && part.starts_with('0') {
                (8, &part[1..])
            } else {
                (10, *part)
            };
        if digits.is_empty()
            || !digits.bytes().all(|b| match radix {
                16 => b.is_ascii_hexdigit(),
                8 => (b'0'..=b'7').contains(&b),
                _ => b.is_ascii_digit(),
            })
        {
            return None;
        }
        let parsed = u32::from_str_radix(digits, radix).ok()?;
        if index + 1 == parts.len() {
            let bits = 8 * (5 - parts.len());
            if u64::from(parsed) > ((1u64 << bits) - 1) {
                return None;
            }
            total |= parsed;
        } else {
            if parsed > 255 {
                return None;
            }
            total |= parsed << (8 * (3 - index));
        }
    }
    Some(total.into())
}
fn ipv4_decimal(value: &str) -> Option<Ipv4Addr> {
    let parts = value
        .split('.')
        .map(|v| {
            if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
                v.parse::<u8>().ok()
            } else {
                None
            }
        })
        .collect::<Option<Vec<_>>>()?;
    (parts.len() == 4).then(|| Ipv4Addr::new(parts[0], parts[1], parts[2], parts[3]))
}
fn parse_proxy(value: &str, expected: Transport) -> Result<SipAddr> {
    if value.encode_utf16().count() > 256 || value.chars().any(char::is_control) {
        return Err(InvalidRequest);
    }
    let mut address = value.split(';').next().unwrap().trim();
    let mut protocol = Transport::Udp;
    for (prefix, transport) in [
        ("udp:", Transport::Udp),
        ("tcp:", Transport::Tcp),
        ("tls:", Transport::Tls),
        ("sip:", Transport::Udp),
        ("sips:", Transport::Tls),
    ] {
        if address
            .get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        {
            address = &address[prefix.len()..];
            protocol = transport;
            break;
        }
    }
    if protocol != expected {
        return Err(InvalidRequest);
    }
    let (ip, port) = if address.starts_with('[') {
        let end = address.find(']').ok_or(OutcomeUnknown)?;
        (&address[..=end], address[end + 1..].strip_prefix(':'))
    } else if address.matches(':').count() == 1 {
        let (ip, port) = address.split_once(':').unwrap();
        (ip, Some(port))
    } else {
        (address, None)
    };
    let mut ip = parse_ip(ip).ok_or(OutcomeUnknown)?;
    let port = match port {
        None | Some("") => 0,
        Some(port) => port.parse::<u16>().map_err(|_| OutcomeUnknown)?,
    };
    ip.set_port(if port == 0 {
        if protocol == Transport::Tls {
            5061
        } else {
            5060
        }
    } else {
        port
    });
    Ok(SipAddr::new(protocol, ip.into()))
}
