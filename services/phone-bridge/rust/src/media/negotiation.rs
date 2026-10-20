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
    VideoIncompatible,
    TextIncompatible,
    Endpoint,
}
impl NegotiationError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSdp => "invalid_sdp",
            Self::NoRemoteMedia => "NoRemoteMedia",
            Self::NoMatchingMediaType => "NoMatchingMediaType",
            Self::AudioIncompatible => "AudioIncompatible",
            Self::VideoIncompatible => "VideoIncompatible",
            Self::TextIncompatible => "TextIncompatible",
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
#[derive(Clone)]
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
        // RTPSession constructs SDP(IPAddress.Loopback), independently of its
        // selected connection address, including scoped IPv6 connections.
        document.origin.address_type = "IP4".into();
        document.origin.address = Some("127.0.0.1".into());
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
#[derive(Clone)]
pub struct AudioNegotiation {
    sender: AudioFormat,
    receive: Vec<AudioFormat>,
    offered_events: Vec<u8>,
    remote_events: Vec<TelephoneEventFormat>,
    remote_sources: Vec<u32>,
    remote_track: bool,
    selected_event: u8,
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
    pub fn remote_sources(&self) -> &[u32] {
        &self.remote_sources
    }
    pub(super) fn remote_sources_option(&self) -> Option<&[u32]> {
        self.remote_track.then_some(self.remote_sources.as_slice())
    }
    pub fn selected_event_payload(&self) -> u8 {
        self.selected_event
    }
}

#[derive(Clone)]
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
    inactive: Vec<InactiveTrack>,
}
#[derive(Clone)]
struct InactiveTrack {
    kind: SdpMediaKind,
    ordinal: usize,
    formats: Vec<SdpFormat>,
}
/// Saved application rollback state surrounds the SDK's incremental track
/// effects. Secondary tracks and selected events survive a rejected offer.
pub(super) struct DescriptionUpdate {
    pub remote: SdpDocument,
    kind: SdpType,
    next: usize,
    counts: [usize; 3],
    previous_formats: Vec<SdpFormat>,
    previous_answers: HashMap<u8, AudioFormat>,
}
pub(super) struct TrackUpdate {
    pub track: SdpMedia,
    pub primary: bool,
    pub existing: bool,
    pub compatible: bool,
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
            inactive: Vec::new(),
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
        let mut media = vec![self.describe_track(
            SdpMediaKind::Audio,
            0,
            identity,
            true,
            self.mid(SdpMediaKind::Audio, 0, 0),
            false,
        )];
        for (index, track) in self.inactive.iter().enumerate() {
            media.push(self.describe_track(
                track.kind,
                track.ordinal,
                identity,
                false,
                self.mid(track.kind, track.ordinal, index + 1),
                false,
            ));
        }
        let document = identity.document(media);
        self.local_events = event_formats(&document);
        document
    }
    fn mid(&self, kind: SdpMediaKind, ordinal: usize, index: usize) -> Option<String> {
        match &self.remote {
            Some(remote) => remote
                .media
                .iter()
                .filter(|m| m.kind == kind)
                .nth(ordinal)
                .map(|m| m.mid.clone())
                .unwrap_or_else(|| Some(String::new())),
            None => Some(index.to_string()),
        }
    }
    fn describe_track(
        &self,
        kind: SdpMediaKind,
        ordinal: usize,
        identity: &SdpIdentity,
        active: bool,
        mid: Option<String>,
        answer: bool,
    ) -> SdpMedia {
        let primary = kind == SdpMediaKind::Audio && ordinal == 0;
        let mut formats = if primary {
            self.local.clone()
        } else {
            self.inactive
                .iter()
                .find(|m| m.kind == kind && m.ordinal == ordinal)
                .map(|m| m.formats.clone())
                .unwrap_or_default()
        };
        if answer && kind == SdpMediaKind::Audio {
            for format in &mut formats {
                if let Some(answer) = self.evs_answers.get(&format.payload) {
                    format.parameters = answer.parameters().map(Into::into);
                }
            }
        }
        let mut track = SdpMedia::local(
            kind,
            if active && !formats.is_empty() {
                identity.local.port()
            } else {
                0
            },
            formats,
        );
        track.direction = Some(if active {
            SdpDirection::SendRecv
        } else {
            SdpDirection::Inactive
        });
        track.mid = mid;
        if primary {
            track.set_ssrc(identity.source, &identity.cname);
        }
        track.evs_packet_time();
        track
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
    pub(super) fn begin_description(
        &mut self,
        kind: SdpType,
        description: &SdpDocument,
    ) -> Result<DescriptionUpdate, NegotiationError> {
        let mut remote = description.normalized()?;
        if remote.media.is_empty() {
            return Err(NegotiationError::NoRemoteMedia);
        }
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
        let previous_formats = self.local.clone();
        let previous_answers = std::mem::replace(&mut self.evs_answers, answers);
        if kind == SdpType::Offer && !self.challenge {
            self.local = self.configured.clone();
        }
        for format in &mut self.local {
            if let Some(map) = maps.get(&evs_channels(Some(&format.rtpmap))) {
                format.rtpmap = map.clone();
            }
        }
        Ok(DescriptionUpdate {
            remote,
            kind,
            next: 0,
            counts: [0; 3],
            previous_formats,
            previous_answers,
        })
    }
    pub(super) fn preflight(&self, update: &DescriptionUpdate) -> Result<(), NegotiationError> {
        if update.remote.media.len() == 1 {
            let kind = update.remote.media[0].kind;
            if matches!(kind, SdpMediaKind::Video | SdpMediaKind::Text)
                && !self
                    .inactive
                    .iter()
                    .any(|track| track.kind == kind && track.ordinal == 0)
            {
                return Err(NegotiationError::NoMatchingMediaType);
            }
        }
        Ok(())
    }
    pub(super) fn next_track(&mut self, update: &mut DescriptionUpdate) -> Option<TrackUpdate> {
        while let Some(mut track) = update.remote.media.get(update.next).cloned() {
            update.next += 1;
            track.formats = track.codec_formats();
            let kind = match track.kind {
                SdpMediaKind::Audio => 0,
                SdpMediaKind::Video => 1,
                SdpMediaKind::Text => 2,
                _ => continue,
            };
            let ordinal = update.counts[kind];
            update.counts[kind] += 1;
            if kind == 0 && ordinal == 0 {
                let (formats, compatible, event) = match_track(&track, &self.local, update.kind);
                self.local = formats;
                if let Some(event) = event {
                    self.event_payload = event;
                }
                return Some(TrackUpdate {
                    track,
                    primary: true,
                    existing: true,
                    compatible,
                });
            }
            if let Some(local) = self
                .inactive
                .iter_mut()
                .find(|local| local.kind == track.kind && local.ordinal == ordinal)
            {
                let (formats, compatible, _) = match_track(&track, &local.formats, update.kind);
                local.formats = formats;
                return Some(TrackUpdate {
                    track,
                    primary: false,
                    existing: true,
                    compatible,
                });
            }
            let mut formats = track.formats.clone();
            if kind == 0 && !formats.iter().any(|f| f.payload == 101) {
                formats.push(event_format());
            }
            let compatible = formats
                .iter()
                .any(|f| kind != 0 || !is_name(f, "telephone-event"));
            self.inactive.push(InactiveTrack {
                kind: track.kind,
                ordinal,
                formats,
            });
            return Some(TrackUpdate {
                track,
                primary: false,
                existing: false,
                compatible,
            });
        }
        None
    }
    pub(super) fn reject_description(&mut self, update: DescriptionUpdate) {
        self.local = update.previous_formats;
        self.evs_answers = update.previous_answers;
    }
    pub(super) fn negotiate_current(
        &mut self,
        update: &DescriptionUpdate,
        sources: Option<&[u32]>,
        media_open: bool,
    ) -> Result<bool, NegotiationError> {
        // Closure detaches the SDK format callback, so its arguments are not
        // evaluated by the null-conditional event invocation.
        if !media_open {
            return Ok(true);
        }
        // All SDK metadata is converted before its callback checks closure
        // or constructs a codec. A later invalid clock still rejects the SDP.
        for format in self.local.iter().filter(|f| !is_name(f, "telephone-event")) {
            if format
                .audio_metadata()
                .is_some_and(|(rate, channels)| rate <= 0 || channels <= 0)
            {
                return Err(NegotiationError::Endpoint);
            }
        }
        let mut receive = Vec::new();
        for format in &self.local {
            if is_name(format, "telephone-event") {
                continue;
            }
            if let Some(selected) = self.evs_answers.get(&format.payload) {
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
            // Static IDs can match before metadata construction. The SDK
            // throws for invalid clocks before its application callback;
            // unsupported positive formats fail the media constructor there.
            let Some((rate, channels)) = format.audio_metadata() else {
                return Ok(false);
            };
            let rate = u32::try_from(rate).map_err(|_| NegotiationError::Endpoint)?;
            let pcm = if codec == Codec::Evs {
                48000
            } else if codec == Codec::G722 && rate == 8000 {
                16000
            } else {
                rate
            };
            let Some(channels) = u8::try_from(channels).ok() else {
                return Ok(false);
            };
            let Ok(format) = AudioFormat::new(
                format.payload.into(),
                codec.name(),
                pcm,
                rate,
                channels,
                if format.parsed_rtpmap().is_some() {
                    format.parameters()
                } else {
                    None
                },
            ) else {
                return Ok(false);
            };
            receive.push(format);
        }
        let sender = receive
            .iter()
            .min_by_key(|format| (self.rank(format.name()), format.channels()))
            .ok_or(NegotiationError::AudioIncompatible)?
            .clone();
        let remote_events = event_formats(&update.remote);
        let mut offered_events: Vec<_> = remote_events.iter().map(|f| f.payload).collect();
        offered_events.sort_unstable();
        offered_events.dedup();
        self.state = Some(AudioNegotiation {
            sender,
            receive,
            offered_events,
            remote_events,
            remote_sources: sources.unwrap_or(&[]).to_vec(),
            remote_track: sources.is_some(),
            selected_event: self.event_payload,
        });
        Ok(true)
    }
    pub(super) fn publish_description(&mut self, update: DescriptionUpdate) {
        self.remote = Some(update.remote);
    }
    pub fn set_remote_description(
        &mut self,
        kind: SdpType,
        description: &SdpDocument,
    ) -> Result<(), NegotiationError> {
        let mut update = self.begin_description(kind, description)?;
        let result = (|| {
            self.preflight(&update)?;
            let mut sources = None;
            while let Some(next) = self.next_track(&mut update) {
                if next.primary {
                    sources = Some(next.track.sources());
                    if next.track.port == 65_535 {
                        return Err(NegotiationError::Endpoint);
                    }
                }
                if !next.compatible {
                    return Err(incompatible(next.track.kind));
                }
            }
            self.negotiate_current(&update, sources.as_deref(), true)
                .map(|_| ())
        })();
        if result.is_ok() {
            self.publish_description(update);
        } else {
            self.reject_description(update);
        }
        result
    }
    pub fn create_answer(&mut self, identity: &SdpIdentity) -> Option<SdpDocument> {
        let remote = self.remote.as_ref()?;
        let mut counts = [0; 3];
        let mut media = Vec::new();
        for (index, offered) in remote.media.iter().enumerate() {
            let kind = match offered.kind {
                SdpMediaKind::Audio => 0,
                SdpMediaKind::Video => 1,
                SdpMediaKind::Text => 2,
                _ => continue,
            };
            let ordinal = counts[kind];
            counts[kind] += 1;
            let active = kind == 0
                && ordinal == 0
                && offered.port != 0
                && offered.direction != Some(SdpDirection::Inactive)
                && !remote
                    .media_connection(index)
                    .is_some_and(|c| matches!(c.address(), "0.0.0.0" | "::"));
            media.push(self.describe_track(
                offered.kind,
                ordinal,
                identity,
                active,
                offered.mid.clone(),
                true,
            ));
        }
        let answer = identity.document(media);
        self.local_events = event_formats(&answer);
        Some(answer)
    }
}

pub(super) fn incompatible(kind: SdpMediaKind) -> NegotiationError {
    match kind {
        SdpMediaKind::Video => NegotiationError::VideoIncompatible,
        SdpMediaKind::Text => NegotiationError::TextIncompatible,
        _ => NegotiationError::AudioIncompatible,
    }
}
// SDP format matching follows the retained SDK policy, including static IDs
// and the video packetization-mode check; FMTP otherwise does not match codecs.
fn match_track(
    track: &SdpMedia,
    local: &[SdpFormat],
    kind: SdpType,
) -> (Vec<SdpFormat>, bool, Option<u8>) {
    let mut agreed: Vec<_> = track
        .formats
        .iter()
        .filter(|remote| {
            local.iter().any(|local| {
                !remote.rtpmap.is_empty()
                    && !local.rtpmap.is_empty()
                    && remote
                        .rtpmap
                        .trim()
                        .eq_ignore_ascii_case(local.rtpmap.trim())
                    || remote.payload < 96
                        && remote.payload == local.payload
                        && remote
                            .name()
                            .zip(local.name())
                            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            }) && compatible_parameters(remote)
        })
        .cloned()
        .collect();
    let priority = if kind == SdpType::Offer {
        &track.formats
    } else {
        local
    };
    agreed.sort_by_key(|f| {
        priority
            .iter()
            .position(|p| p.payload == f.payload)
            .unwrap_or(usize::MAX)
    });
    let audio = track.kind == SdpMediaKind::Audio;
    let compatible = agreed
        .iter()
        .any(|f| !audio || !is_name(f, "telephone-event"));
    let mut event = None;
    if audio {
        let previous = agreed
            .iter()
            .find(|f| is_name(f, "telephone-event"))
            .cloned()
            .unwrap_or_else(event_format);
        agreed.retain(|f| !is_name(f, "telephone-event"));
        let selected = track
            .formats
            .iter()
            .find(|f| is_name(f, "telephone-event"))
            .cloned();
        event = selected.as_ref().map(|f| f.payload);
        agreed.push(selected.unwrap_or(previous));
    }
    (agreed, compatible, event)
}
fn compatible_parameters(format: &SdpFormat) -> bool {
    if !["H264", "H265", "JPEG"].iter().any(|name| {
        format
            .rtpmap
            .trim()
            .to_ascii_lowercase()
            .starts_with(&name.to_ascii_lowercase())
    }) {
        return true;
    }
    // Last duplicate parameter wins in the original parser.
    format
        .parameters()
        .unwrap_or("")
        .split(';')
        .filter_map(|part| part.split_once('='))
        .filter(|(_, value)| !value.contains('='))
        .filter(|(key, _)| key.trim().eq_ignore_ascii_case("packetization-mode"))
        .last()
        .is_none_or(|(_, value)| value.trim() == "1")
}
fn event_format() -> SdpFormat {
    SdpFormat::new(101, "telephone-event/8000".into(), Some("0-16".into()))
}
fn is_name(format: &SdpFormat, name: &str) -> bool {
    format
        .name()
        .is_some_and(|value| value.eq_ignore_ascii_case(name))
}
pub(super) fn event_formats(document: &SdpDocument) -> Vec<TelephoneEventFormat> {
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
