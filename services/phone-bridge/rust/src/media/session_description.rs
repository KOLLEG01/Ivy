//! Original SIP session endpoint/direction effects around codec negotiation.
use super::transport::MediaDestinations;
use super::{
    AudioNegotiator, Codec, NegotiationError, SdpDirection, SdpDocument, SdpIdentity, SdpMediaKind,
    SdpType,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV6};

pub(super) struct Descriptions {
    pub negotiation: AudioNegotiator,
    pub destinations: MediaDestinations,
    pub local_direction: SdpDirection,
    pub remote_direction: Option<SdpDirection>,
    pub version: u64,
    pub close_control: bool,
}
impl Descriptions {
    pub fn new(preferences: &[Codec]) -> Result<Self, super::FormatError> {
        Ok(Self {
            negotiation: AudioNegotiator::new(preferences)?,
            destinations: MediaDestinations::default(),
            local_direction: SdpDirection::SendRecv,
            remote_direction: None,
            version: 0,
            close_control: false,
        })
    }
    pub fn can_send(&self) -> bool {
        !matches!(
            self.local_direction,
            SdpDirection::RecvOnly | SdpDirection::Inactive
        )
    }
    pub fn set_status(&mut self, status: SdpDirection) {
        self.local_direction = status;
        self.version = self.version.wrapping_add(1);
    }
    pub fn offer(&mut self, identity: &SdpIdentity) -> SdpDocument {
        if self.local_direction == SdpDirection::Inactive {
            self.local_direction = SdpDirection::SendRecv;
        }
        let mut offer = self.negotiation.create_offer(identity);
        if let Some(audio) = offer.media.first_mut() {
            audio.direction = Some(self.local_direction);
        }
        offer
    }
    pub fn answer(&mut self, identity: &SdpIdentity, local_port: u16) -> Option<SdpDocument> {
        let mut answer = self.negotiation.create_answer(identity)?;
        if let Some(audio) = answer
            .media
            .iter_mut()
            .find(|m| m.kind == SdpMediaKind::Audio)
        {
            audio.direction = Some(self.local_direction);
            audio.port = if self.local_direction == SdpDirection::Inactive {
                0
            } else {
                i32::from(local_port)
            };
        }
        Some(answer)
    }
    /// A failed codec offer can still alter the original destination/direction.
    /// Rejected codec state itself is retained by AudioNegotiator.
    pub fn apply(
        &mut self,
        kind: SdpType,
        description: &SdpDocument,
    ) -> Result<(), NegotiationError> {
        self.close_control = false;
        let remote = description.normalized()?;
        if remote.media.is_empty() {
            return Err(NegotiationError::NoRemoteMedia);
        }
        if remote.media.len() == 1 && remote.media[0].kind != SdpMediaKind::Audio {
            return Err(NegotiationError::NoMatchingMediaType);
        }
        if remote
            .media
            .iter()
            .filter(|m| m.kind == SdpMediaKind::Audio)
            .flat_map(|m| &m.formats)
            .any(|f| f.name().is_none())
        {
            return Err(NegotiationError::InvalidSdp);
        }
        // IPAddress.Parse is used before clearing remote tracks, even when a
        // media connection overrides the session's invalid address. No DNS.
        let global = remote
            .connection
            .as_ref()
            .filter(|c| !c.address().is_empty())
            .map(|c| address(c.address(), 0))
            .transpose()?;
        self.remote_direction = None;
        let Some(track) = remote.media.iter().find(|m| m.kind == SdpMediaKind::Audio) else {
            return Err(NegotiationError::NoMatchingMediaType);
        };
        self.remote_direction = Some(track.direction.unwrap_or(SdpDirection::SendRecv));
        let mut candidate = self.negotiation.clone();
        let compatible = candidate.set_remote_description(kind, &remote);
        self.negotiation.retain_event_selection(&candidate);
        let ip = track
            .connection
            .as_ref()
            .map(|c| address(c.address(), 0))
            .transpose()?
            .or(global);
        let rtp = ip.map(|mut ip| {
            ip.set_port(u16::try_from(track.port).unwrap_or(9));
            ip
        });
        if self.local_direction == SdpDirection::Inactive {
            self.local_direction = SdpDirection::SendRecv;
        }
        if self.remote_direction == Some(SdpDirection::Inactive)
            || rtp.is_some_and(|p| p.port() == 0 || p.ip().is_unspecified() && p.port() != 9)
        {
            self.local_direction = SdpDirection::Inactive;
        }
        self.close_control = self.local_direction == SdpDirection::Inactive;
        let control = if self.remote_direction != Some(SdpDirection::Inactive)
            && self.local_direction != SdpDirection::Inactive
        {
            let mut p = rtp.ok_or(NegotiationError::Endpoint)?;
            p.set_port(p.port().checked_add(1).ok_or(NegotiationError::Endpoint)?);
            Some(p)
        } else {
            None
        };
        if let Some(p) = rtp.filter(|p| p.port() != 9) {
            self.destinations.rtp = Some(p);
        }
        if let Some(p) = control.filter(|p| p.port() != 9) {
            self.destinations.rtcp = Some(p);
        }
        compatible?;
        self.negotiation = candidate;
        Ok(())
    }
}
/// IPAddress.Parse's legacy IPv4 forms; never resolve a host name or contact a
/// resolver. IPv6 numeric scopes remain part of the original endpoint identity.
fn address(text: &str, port: u16) -> Result<SocketAddr, NegotiationError> {
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    if text.contains(':') {
        let (ip, scope) = text.rsplit_once('%').ok_or(NegotiationError::Endpoint)?;
        return Ok(SocketAddr::V6(SocketAddrV6::new(
            ip.parse().map_err(|_| NegotiationError::Endpoint)?,
            port,
            0,
            scope.parse().map_err(|_| NegotiationError::Endpoint)?,
        )));
    }
    let parts: Vec<_> = text.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return Err(NegotiationError::Endpoint);
    }
    let mut words = Vec::new();
    for part in parts {
        let (digits, radix) =
            if let Some(s) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
                (s, 16)
            } else if part.len() > 1 && part.starts_with('0') {
                (&part[1..], 8)
            } else {
                (part, 10)
            };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(NegotiationError::Endpoint);
        }
        words.push(u32::from_str_radix(digits, radix).map_err(|_| NegotiationError::Endpoint)?);
    }
    let count = words.len();
    let mut value = 0_u32;
    for (index, word) in words.into_iter().enumerate() {
        let bits = if index + 1 == count {
            8 * (5 - count)
        } else {
            8
        };
        if bits < 32 && word >= (1_u32 << bits) {
            return Err(NegotiationError::Endpoint);
        }
        value = if bits == 32 {
            word
        } else {
            (value << bits) | word
        };
    }
    Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(value)), port))
}
