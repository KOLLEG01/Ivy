use crate::{
    media::{SipMediaEntropy, SystemSipMediaEntropy},
    sip::{RegistrationPeer, RegistrationTransport},
};
use ivy_contracts::{IvyJson, json::stringify};
use rsipstack::{
    sip::{Header, Request, Transport, prelude::HeadersExt},
    transport::SipAddr,
};

pub(crate) struct NativeIncoming {
    pub sip_call_id: String,
    pub from_uri: String,
    pub peer: RegistrationPeer,
    pub asserted_numbers: Vec<String>,
    pub registered: bool,
}
impl NativeIncoming {
    pub fn parse(request: &Request, source: Option<&SipAddr>) -> Option<Self> {
        let source = source?;
        let address = source.get_socketaddr().ok()?;
        if address.port() == 0 {
            return None;
        }
        let transport = match source.r#type.unwrap_or(Transport::Udp) {
            Transport::Udp => RegistrationTransport::Udp,
            Transport::Tcp => RegistrationTransport::Tcp,
            Transport::Tls => RegistrationTransport::Tls,
            _ => return None,
        };
        let sip_call_id = request.call_id_header().ok()?.value().to_owned();
        let from_uri = request.from_header().ok()?.uri().ok()?.to_string();
        let valid = |s: &str, n| {
            !s.trim().is_empty()
                && s.encode_utf16().count() <= n
                && !s.chars().any(char::is_control)
        };
        if !valid(&sip_call_id, 1024) || !valid(&from_uri, 2048) {
            return None;
        }
        let mut asserted_numbers = Vec::new();
        for identity in request
            .headers
            .iter()
            .filter_map(|h| match h {
                Header::Other(name, value) if name.eq_ignore_ascii_case("P-Asserted-Identity") => {
                    Some(value.as_str())
                }
                _ => None,
            })
            .flat_map(|h| h.split(','))
            .take(16)
        {
            let bytes = identity.as_bytes();
            for (i, byte) in bytes.iter().enumerate() {
                if *byte != b'+' || i > 0 && bytes[i - 1].is_ascii_digit() {
                    continue;
                }
                let n = bytes[i + 1..]
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
                if (7..=15).contains(&n) && bytes[i + 1] != b'0' {
                    let number = identity[i..i + 1 + n].to_owned();
                    if !asserted_numbers.contains(&number) && asserted_numbers.len() < 16 {
                        asserted_numbers.push(number);
                    }
                }
            }
        }
        Some(Self {
            sip_call_id,
            from_uri,
            peer: RegistrationPeer { address, transport },
            asserted_numbers,
            registered: false,
        })
    }
    pub fn json(&self) -> String {
        let address = match self.peer.address {
            std::net::SocketAddr::V6(address) if address.scope_id() != 0 => {
                format!("{}%{}", address.ip(), address.scope_id())
            }
            address => address.ip().to_string(),
        };
        stringify(
            &IvyJson::object([
                ("sipCallId", self.sip_call_id.clone().into()),
                ("fromUri", self.from_uri.clone().into()),
                ("peerAddress", address.into()),
                ("peerPort", IvyJson::Number(self.peer.address.port().into())),
                (
                    "transport",
                    match self.peer.transport {
                        RegistrationTransport::Udp => "udp",
                        RegistrationTransport::Tcp => "tcp",
                        RegistrationTransport::Tls => "tls",
                    }
                    .into(),
                ),
                (
                    "assertedNumbers",
                    IvyJson::Array(
                        self.asserted_numbers
                            .iter()
                            .cloned()
                            .map(Into::into)
                            .collect(),
                    ),
                ),
                ("registrationPeer", self.registered.into()),
            ]),
            crate::ipc::FRAME_BYTES,
        )
        .expect("bounded incoming context")
    }
}
pub(crate) fn call_id() -> Option<String> {
    use std::fmt::Write;
    let mut bytes = [0u8; 16];
    SystemSipMediaEntropy.fill(&mut bytes).ok()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        write!(id, "{byte:02x}").ok()?;
    }
    Some(id)
}
