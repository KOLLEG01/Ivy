//! Per-call SDP and codec policy. This owns declarations, not encoder instances
//! or a running media session. A media owner must construct the configured
//! codecs before advertising them and apply the negotiated state under its lock.
use super::{
    AudioFormat, Codec, FormatError, SdpConnection, SdpDirection, SdpDocument, SdpError, SdpFormat,
    SdpMedia, SdpMediaKind, evs_channels, evs_offer_parameters, evs_select,
};
use std::{collections::HashMap, net::SocketAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdpType {
    Offer,
    Answer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NegotiationError {
    InvalidSdp,
    NoRemoteMedia,
    NoMatchingMediaType,
    AudioIncompatible,
    Endpoint,
}
impl NegotiationError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSdp => "invalid_sdp",
            Self::NoRemoteMedia => "NoRemoteMedia",
            Self::NoMatchingMediaType => "NoMatchingMediaType",
            Self::AudioIncompatible => "AudioIncompatible",
            Self::Endpoint => "Error",
        }
    }
}
impl From<SdpError> for NegotiationError {
    fn from(_: SdpError) -> Self {
        Self::InvalidSdp
    }
}
impl std::fmt::Display for NegotiationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl std::error::Error for NegotiationError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TelephoneEventFormat {
    pub payload: u8,
    pub clock_rate: i32,
}

/// All identifiers come from the original media owner, including the bound RTP
/// port and its actual RTP source. No second transport is created for SDP.
pub struct SdpIdentity {
    local: SocketAddr,
    session_id: u32,
    version: u64,
    source: u32,
    cname: String,
}
impl SdpIdentity {
    pub fn new(
        local: SocketAddr,
        session_id: u32,
        version: u64,
        source: u32,
        cname: &str,
    ) -> Result<Self, SdpError> {
        if local.port() == 0
            || local.port() == u16::MAX
            || cname.is_empty()
            || cname.len() > 128
            || cname
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(SdpError::Invalid);
        }
        Ok(Self {
            local,
            session_id,
            version,
            source,
            cname: cname.into(),
        })
    }
    fn document(&self, media: Vec<SdpMedia>) -> SdpDocument {
        let address = match self.local {
            SocketAddr::V6(address) if address.scope_id() != 0 => {
                format!("{}%{}", address.ip(), address.scope_id())
            }
            address => address.ip().to_string(),
        };
        let address_type = if self.local.is_ipv4() { "IP4" } else { "IP6" };
        let mut document = SdpDocument::default();
        document.origin.session_id = self.session_id.to_string();
        document.origin.version = self.version;
        document.origin.address_type = address_type.into();
        document.origin.address = Some(address.clone());
        document.connection = Some(SdpConnection {
            network: "IN".into(),
            address_type: address_type.into(),
            address,
        });
        document.media = media;
        document
    }
}

/// Negotiated codec state is replaced only by an accepted description. Remote
/// telephone-event declarations are retained separately from the selected one.
pub struct AudioNegotiation {
    sender: AudioFormat,
    receive: Vec<AudioFormat>,
    offered_events: Vec<u8>,
    remote_events: Vec<TelephoneEventFormat>,
}
impl AudioNegotiation {
    pub fn sender(&self) -> &AudioFormat {
        &self.sender
    }
    pub fn receive_formats(&self) -> &[AudioFormat] {
        &self.receive
    }
    pub fn offered_event_payloads(&self) -> &[u8] {
        &self.offered_events
    }
    pub fn remote_event_formats(&self) -> &[TelephoneEventFormat] {
        &self.remote_events
    }
}

pub struct AudioNegotiator {
    preference: Vec<Codec>,
    configured: Vec<SdpFormat>,
    local: Vec<SdpFormat>,
    challenge: bool,
    state: Option<AudioNegotiation>,
    remote: Option<SdpDocument>,
    evs_answers: HashMap<u8, AudioFormat>,
    event_payload: u8,
    local_events: Vec<TelephoneEventFormat>,
}
impl AudioNegotiator {
    pub fn new(preference: &[Codec]) -> Result<Self, FormatError> {
        if preference.is_empty()
            || preference.len() > 5
            || preference
                .iter()
                .enumerate()
                .any(|(index, codec)| preference[..index].contains(codec))
        {
            return Err(FormatError);
        }
        let mut configured: Vec<_> = preference
            .iter()
            .flat_map(|codec| codec.offered_formats())
            .map(|format| {
                SdpFormat::new(
                    format.payload(),
                    format!(
                        "{}/{}/{}",
                        format.name(),
                        format.rtp_rate(),
                        format.channels()
                    ),
                    format.parameters().map(Into::into),
                )
            })
            .collect();
        // Mono non-EVS offers omit /1. Matching deliberately preserves that
        // distinction: the original stack compares the complete RTP map.
        for format in &mut configured {
            if !format
                .name()
                .is_some_and(|name| name.eq_ignore_ascii_case("EVS"))
                && format.rtpmap.ends_with("/1")
            {
                format.rtpmap.truncate(format.rtpmap.len() - 2);
            }
        }
        configured.push(event_format());
        Ok(Self {
            preference: preference.to_vec(),
            local: configured.clone(),
            configured,
            challenge: false,
            state: None,
            remote: None,
            evs_answers: HashMap::new(),
            event_payload: 101,
            local_events: Vec::new(),
        })
    }
    pub fn state(&self) -> Option<&AudioNegotiation> {
        self.state.as_ref()
    }
    pub fn local_formats(&self) -> &[SdpFormat] {
        &self.local
    }
    pub fn negotiated_event_payload(&self) -> u8 {
        self.event_payload
    }
    pub fn local_event_formats(&self) -> &[TelephoneEventFormat] {
        &self.local_events
    }
    pub fn remote_description(&self) -> Option<&SdpDocument> {
        self.remote.as_ref()
    }
    pub fn challenge_codecs(&mut self, enabled: bool) {
        self.challenge = enabled;
        self.local = if enabled {
            Self::new(&[Codec::Pcma, Codec::Pcmu])
                .expect("fixed challenge codecs")
                .configured
        } else {
            self.configured.clone()
        };
    }
    pub fn create_offer(&mut self, identity: &SdpIdentity) -> SdpDocument {
        for format in &mut self.local {
            let channels = evs_channels(Some(&format.rtpmap));
            if channels > 0 {
                format.rtpmap = format!("EVS/16000/{channels}");
                format.parameters = evs_offer_parameters(channels).ok().map(Into::into);
            }
        }
        let mut media = SdpMedia::audio(identity.local.port(), self.local.clone());
        media.mid = Some("0".into());
        media.set_ssrc(identity.source, &identity.cname);
        media.evs_packet_time();
        let document = identity.document(vec![media]);
        self.local_events = event_formats(&document);
        document
    }
    fn rank(&self, name: &str) -> usize {
        self.preference
            .iter()
            .position(|codec| codec.name().eq_ignore_ascii_case(name))
            .unwrap_or_else(|| {
                if name.eq_ignore_ascii_case("PCMA") {
                    100
                } else if name.eq_ignore_ascii_case("PCMU") {
                    101
                } else {
                    usize::MAX
                }
            })
    }
    pub fn set_remote_description(
        &mut self,
        kind: SdpType,
        description: &SdpDocument,
    ) -> Result<(), NegotiationError> {
        let mut remote = description.normalized()?;
        if remote.media.is_empty() {
            return Err(NegotiationError::NoRemoteMedia);
        }
        let mut offered_events: Vec<_> = event_formats(&remote)
            .into_iter()
            .map(|format| format.payload)
            .collect();
        offered_events.sort_unstable();
        offered_events.dedup();
        let mut answers = HashMap::new();
        let mut maps = HashMap::new();
        for track in remote
            .media
            .iter_mut()
            .filter(|track| track.kind == SdpMediaKind::Audio)
        {
            // Original PrioritizeRemoteAudio requires a name, even for codecs
            // that will not match. Return a controlled parse failure, not panic.
            if track.formats.iter().any(|format| format.name().is_none()) {
                return Err(NegotiationError::InvalidSdp);
            }
            track.formats.retain(|format| {
                if !is_name(format, "EVS") {
                    return true;
                }
                let channels = evs_channels(Some(&format.rtpmap));
                let Some(selected) =
                    evs_select(format.parameters(), kind == SdpType::Offer, channels)
                else {
                    return false;
                };
                let Ok(selected) = AudioFormat::new(
                    format.payload.into(),
                    "EVS",
                    48000,
                    16000,
                    channels,
                    Some(&selected),
                ) else {
                    return false;
                };
                maps.entry(channels)
                    .or_insert_with(|| format.rtpmap.clone());
                answers.insert(format.payload, selected);
                true
            });
            let selected_clock = track
                .formats
                .iter()
                .filter(|format| {
                    self.preference
                        .iter()
                        .any(|codec| is_name(format, codec.name()))
                        && self.configured.iter().any(|local| {
                            local
                                .name()
                                .zip(format.name())
                                .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
                        })
                })
                .min_by_key(|format| self.rank(format.name().unwrap()))
                .map(SdpFormat::clock_rate)
                .unwrap_or(0);
            track.formats.sort_by_key(|format| {
                (
                    self.rank(format.name().unwrap()),
                    is_name(format, "telephone-event")
                        && selected_clock != 0
                        && format.clock_rate() != selected_clock,
                )
            });
        }
        let Some(index) = remote
            .media
            .iter()
            .position(|track| track.kind == SdpMediaKind::Audio)
        else {
            return Err(NegotiationError::NoMatchingMediaType);
        };
        let track = &remote.media[index];
        if track.port == 65_535 {
            return Err(NegotiationError::Endpoint);
        }
        let base = if kind == SdpType::Offer && !self.challenge {
            &self.configured
        } else {
            &self.local
        };
        let mut base = base.clone();
        for format in &mut base {
            if let Some(map) = maps.get(&evs_channels(Some(&format.rtpmap))) {
                format.rtpmap = map.clone();
            }
        }
        let selected_event = track
            .formats
            .iter()
            .find(|format| is_name(format, "telephone-event"));
        // The base stack updates this selection even for an incompatible offer.
        if let Some(format) = selected_event {
            self.event_payload = format.payload;
        }
        let mut agreed: Vec<_> = track
            .formats
            .iter()
            .filter(|format| {
                !is_name(format, "telephone-event")
                    && base
                        .iter()
                        .any(|local| local.rtpmap.eq_ignore_ascii_case(&format.rtpmap))
            })
            .cloned()
            .collect();
        if agreed.is_empty() {
            return Err(NegotiationError::AudioIncompatible);
        }
        agreed.push(selected_event.cloned().unwrap_or_else(|| {
            base.iter()
                .find(|format| is_name(format, "telephone-event"))
                .cloned()
                .unwrap_or_else(event_format)
        }));
        let mut receive = Vec::new();
        for format in &agreed {
            if is_name(format, "telephone-event") {
                continue;
            }
            if let Some(selected) = answers.get(&format.payload) {
                receive.push(selected.clone());
                continue;
            }
            let codec = self
                .preference
                .iter()
                .chain([Codec::Pcma, Codec::Pcmu].iter())
                .find(|codec| is_name(format, codec.name()))
                .copied()
                .ok_or(NegotiationError::AudioIncompatible)?;
            let declared = codec.offered_formats().remove(0);
            receive.push(
                AudioFormat::new(
                    format.payload.into(),
                    codec.name(),
                    declared.pcm_rate(),
                    declared.rtp_rate(),
                    declared.channels(),
                    format.parameters(),
                )
                .map_err(|_| NegotiationError::AudioIncompatible)?,
            );
        }
        let sender = receive
            .iter()
            .min_by_key(|format| (self.rank(format.name()), format.channels()))
            .ok_or(NegotiationError::AudioIncompatible)?
            .clone();
        self.state = Some(AudioNegotiation {
            sender,
            receive,
            offered_events,
            remote_events: event_formats(&remote),
        });
        self.local = agreed;
        self.remote = Some(remote);
        self.evs_answers = answers;
        Ok(())
    }
    pub fn create_answer(&mut self, identity: &SdpIdentity) -> Option<SdpDocument> {
        let remote = self.remote.as_ref()?;
        let mut accepted_audio = false;
        let media = remote
            .media
            .iter()
            .enumerate()
            .filter(|(_, offered)| {
                matches!(offered.kind, SdpMediaKind::Audio | SdpMediaKind::Video)
            })
            .map(|(index, offered)| {
                let accepted = offered.kind == SdpMediaKind::Audio && !accepted_audio;
                if accepted {
                    accepted_audio = true;
                }
                let mut formats = if accepted {
                    self.local.clone()
                } else {
                    offered.formats.clone()
                };
                if offered.kind == SdpMediaKind::Audio
                    && !formats
                        .iter()
                        .any(|format| is_name(format, "telephone-event"))
                {
                    formats.push(event_format());
                }
                for format in &mut formats {
                    if let Some(answer) = self.evs_answers.get(&format.payload) {
                        format.parameters = answer.parameters().map(Into::into);
                    }
                }
                let inactive = !accepted
                    || offered.port == 0
                    || offered.direction == Some(SdpDirection::Inactive)
                    || remote
                        .media_connection(index)
                        .is_some_and(|connection| matches!(connection.address(), "0.0.0.0" | "::"));
                let mut track = SdpMedia::local(
                    offered.kind,
                    if inactive { 0 } else { identity.local.port() },
                    formats,
                );
                track.direction = Some(if inactive {
                    SdpDirection::Inactive
                } else {
                    SdpDirection::SendRecv
                });
                track.mid = offered.mid.clone();
                if accepted {
                    track.set_ssrc(identity.source, &identity.cname);
                }
                track.evs_packet_time();
                track
            })
            .collect();
        let answer = identity.document(media);
        self.local_events = event_formats(&answer);
        Some(answer)
    }
}
fn event_format() -> SdpFormat {
    SdpFormat::new(101, "telephone-event/8000".into(), Some("0-16".into()))
}
fn is_name(format: &SdpFormat, name: &str) -> bool {
    format
        .name()
        .is_some_and(|value| value.eq_ignore_ascii_case(name))
}
fn event_formats(document: &SdpDocument) -> Vec<TelephoneEventFormat> {
    document
        .media
        .iter()
        .filter(|track| track.kind == SdpMediaKind::Audio && track.port > 0)
        .flat_map(|track| &track.formats)
        .filter(|format| is_name(format, "telephone-event"))
        .map(|format| TelephoneEventFormat {
            payload: format.payload,
            clock_rate: format.clock_rate(),
        })
        .collect()
}
