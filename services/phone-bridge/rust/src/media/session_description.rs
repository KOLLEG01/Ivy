//! Original SIP session endpoint/direction effects around codec negotiation.
use super::negotiation::{DescriptionUpdate, incompatible};
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
    pub remote_changed: bool,
    pub remote_sources: Option<Vec<u32>>,
}
pub(super) struct Applying {
    codecs: DescriptionUpdate,
    global: Option<SocketAddr>,
    pending: Option<Primary>,
}
struct Primary {
    rtp: Option<SocketAddr>,
    remote_direction: SdpDirection,
    compatible: bool,
}
pub(super) enum Advance {
    Audio { inactive: bool },
    Complete { media_valid: bool },
}
impl Descriptions {
    pub fn new(preferences: &[Codec]) -> Result<Self, super::FormatError> {
        Ok(Self {
            negotiation: AudioNegotiator::new(preferences)?,
            destinations: MediaDestinations::default(),
            local_direction: SdpDirection::SendRecv,
            remote_direction: None,
            version: 0,
            remote_changed: false,
            remote_sources: None,
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
    /// Application rollback and SDK stream effects are separate. A suspended
    /// RTCP notification can re-enter this owner before endpoints or codecs
    /// are committed; its changes remain visible to the outer continuation.
    pub fn begin(
        &mut self,
        kind: SdpType,
        description: &SdpDocument,
    ) -> Result<Applying, NegotiationError> {
        let codecs = self.negotiation.begin_description(kind, description)?;
        let global = (|| {
            self.negotiation.preflight(&codecs)?;
            codecs
                .remote
                .connection
                .as_ref()
                .filter(|c| !c.address().is_empty())
                .map(|c| address(c.address(), 0))
                .transpose()
        })();
        let global = match global {
            Ok(global) => global,
            Err(error) => {
                self.negotiation.reject_description(codecs);
                return Err(error);
            }
        };
        self.remote_direction = None;
        self.remote_sources = None;
        self.remote_changed = true;
        Ok(Applying {
            codecs,
            global,
            pending: None,
        })
    }
    pub fn advance(
        &mut self,
        applying: &mut Applying,
        media_open: bool,
    ) -> Result<Advance, NegotiationError> {
        if let Some(primary) = applying.pending.take() {
            let control = if primary.remote_direction != SdpDirection::Inactive
                && self.local_direction != SdpDirection::Inactive
            {
                let mut p = primary.rtp.ok_or(NegotiationError::Endpoint)?;
                p.set_port(p.port().checked_add(1).ok_or(NegotiationError::Endpoint)?);
                Some(p)
            } else {
                None
            };
            if let Some(p) = primary.rtp.filter(|p| p.port() != 9) {
                self.destinations.rtp = Some(p);
            }
            if let Some(p) = control.filter(|p| p.port() != 9) {
                self.destinations.rtcp = Some(p);
            }
            if !primary.compatible {
                return Err(NegotiationError::AudioIncompatible);
            }
        }
        while let Some(next) = self.negotiation.next_track(&mut applying.codecs) {
            if next.primary {
                let remote_direction = next.track.direction.unwrap_or(SdpDirection::SendRecv);
                self.remote_direction = Some(remote_direction);
                self.remote_sources = Some(next.track.sources());
                self.remote_changed = true;
                let rtp = destination(&next.track, applying.global)?;
                if self.local_direction == SdpDirection::Inactive {
                    self.local_direction = SdpDirection::SendRecv;
                }
                if remote_direction == SdpDirection::Inactive
                    || rtp
                        .is_some_and(|p| p.port() == 0 || p.ip().is_unspecified() && p.port() != 9)
                {
                    self.local_direction = SdpDirection::Inactive;
                }
                applying.pending = Some(Primary {
                    rtp,
                    remote_direction,
                    compatible: next.compatible,
                });
                return Ok(Advance::Audio {
                    inactive: self.local_direction == SdpDirection::Inactive,
                });
            }
            // A newly unsupported stream gets an inactive local track without
            // opening RTP/RTCP. Subsequent descriptions still validate its IP.
            if next.existing {
                let _ = destination(&next.track, applying.global)?;
            }
            if !next.compatible {
                return Err(incompatible(next.track.kind));
            }
        }
        let media_valid = self.negotiation.negotiate_current(
            &applying.codecs,
            self.remote_sources.as_deref(),
            media_open,
        )?;
        Ok(Advance::Complete { media_valid })
    }
    pub fn publish(&mut self, applying: Applying) {
        self.negotiation.publish_description(applying.codecs);
    }
    pub fn reject(&mut self, applying: Applying) {
        self.negotiation.reject_description(applying.codecs);
    }
}
fn destination(
    track: &super::SdpMedia,
    global: Option<SocketAddr>,
) -> Result<Option<SocketAddr>, NegotiationError> {
    let ip = track
        .connection
        .as_ref()
        .map(|c| address(c.address(), 0))
        .transpose()?
        .or(global);
    Ok(ip.map(|mut ip| {
        ip.set_port(u16::try_from(track.port).unwrap_or(9));
        ip
    }))
}
/// IPAddress.Parse's legacy IPv4 forms; never resolve a host name or contact a
/// resolver. IPv6 numeric scopes remain part of the original endpoint identity.
fn address(text: &str, port: u16) -> Result<SocketAddr, NegotiationError> {
    let text = text
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(text);
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    if text.contains(':') {
        let (ip, scope) = text.split_once('%').ok_or(NegotiationError::Endpoint)?;
        return Ok(SocketAddr::V6(SocketAddrV6::new(
            ip.parse().map_err(|_| NegotiationError::Endpoint)?,
            port,
            0,
            // The sealed IPAddress parser retains a numeric scope and treats
            // an unresolvable textual scope as0; it does not resolve SDP DNS.
            scope.parse().unwrap_or(0),
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
