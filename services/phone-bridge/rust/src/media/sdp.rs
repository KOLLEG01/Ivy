//! Bounded call-owned SDP. Codec state, address resolution and RTP remain with
//! the media owner; parsing a declaration does not make that codec available.
use std::fmt::Write;

pub const SDP_UNITS: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdpError {
    Empty,
    TooLarge,
    Invalid,
}
impl std::fmt::Display for SdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid_sdp")
    }
}
impl std::error::Error for SdpError {}

/// The original call boundary counts UTF-16 units, independently of UTF-8 bytes.
pub fn bounded_sdp(body: &str) -> bool {
    !body.trim().is_empty() && body.encode_utf16().take(SDP_UNITS + 1).count() <= SDP_UNITS
}
pub fn valid_sdp(content_type: Option<&str>, body: &str) -> bool {
    bounded_sdp(body)
        && content_type.is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/sdp")
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdpDirection {
    SendRecv,
    SendOnly,
    RecvOnly,
    Inactive,
}
impl SdpDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SendRecv => "sendrecv",
            Self::SendOnly => "sendonly",
            Self::RecvOnly => "recvonly",
            Self::Inactive => "inactive",
        }
    }
    fn parse(text: &str) -> Option<Self> {
        [
            Self::SendRecv,
            Self::SendOnly,
            Self::RecvOnly,
            Self::Inactive,
        ]
        .into_iter()
        .find(|direction| direction.as_str().eq_ignore_ascii_case(text))
    }
}

// No Debug/Display: descriptions can contain ICE credentials and peer addresses.
#[derive(Clone, PartialEq, Eq)]
pub struct SdpConnection {
    pub(super) network: String,
    pub(super) address_type: String,
    pub(super) address: String,
}
impl SdpConnection {
    pub fn network(&self) -> &str {
        &self.network
    }
    pub fn address_type(&self) -> &str {
        &self.address_type
    }
    pub fn address(&self) -> &str {
        &self.address
    }
    pub fn to_sdp(&self) -> String {
        format!("c={} {} {}", self.network, self.address_type, self.address)
    }
    fn parse(text: &str) -> Self {
        let mut parts = text.splitn(3, char::is_whitespace);
        Self {
            network: parts.next().unwrap_or("IN").into(),
            address_type: parts.next().unwrap_or("IP4").into(),
            address: parts.next().unwrap_or("").into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SdpOrigin {
    pub(super) username: String,
    pub(super) session_id: String,
    pub(super) version: u64,
    pub(super) network: String,
    pub(super) address_type: String,
    pub(super) address: Option<String>,
}
impl Default for SdpOrigin {
    fn default() -> Self {
        Self {
            username: "-".into(),
            session_id: "-".into(),
            version: 0,
            network: "IN".into(),
            address_type: "IP4".into(),
            address: None,
        }
    }
}
impl SdpOrigin {
    pub fn username(&self) -> &str {
        &self.username
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn network(&self) -> &str {
        &self.network
    }
    pub fn address_type(&self) -> &str {
        &self.address_type
    }
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SdpFormat {
    pub(super) payload: u8,
    pub(super) rtpmap: String,
    pub(super) parameters: Option<String>,
    video: bool,
}
impl SdpFormat {
    pub fn payload(&self) -> u8 {
        self.payload
    }
    pub fn rtpmap(&self) -> &str {
        &self.rtpmap
    }
    pub fn parameters(&self) -> Option<&str> {
        self.parameters.as_deref()
    }
    pub fn name(&self) -> Option<&str> {
        self.parsed_rtpmap().map(|parts| parts.0).or_else(|| {
            Some(match self.payload {
                6 => "DVI4_16K",
                10 => "L16_2",
                11 => "L16",
                16 => "DVI4_11K",
                17 => "DVI4_22K",
                payload => static_map(payload)?.split_once('/')?.0,
            })
        })
    }
    pub(super) fn parsed_rtpmap(&self) -> Option<(&str, i32, i32)> {
        let mut fields = self.rtpmap.trim().split('/').map(str::trim);
        let name = fields.next()?;
        let rate = fields.next()?.parse().ok()?;
        let channels = match fields.next() {
            Some("") | None => 1,
            Some(channels) => channels.parse().ok()?,
        };
        Some((name, rate, channels))
    }
    pub(super) fn audio_metadata(&self) -> Option<(i32, i32)> {
        self.parsed_rtpmap()
            .map(|(_, rate, channels)| (rate, channels))
            .or_else(|| {
                // The SDK falls back to a static audio declaration when RTPMAP
                // parsing fails. Static video formats convert to empty audio.
                if self.payload >= 24 {
                    return None;
                }
                let mut fields = static_map(self.payload)?.split('/');
                fields.next()?;
                Some((
                    fields.next()?.parse().ok()?,
                    fields.next().unwrap_or("1").parse().ok()?,
                ))
            })
    }
    pub fn clock_rate(&self) -> i32 {
        self.rtpmap
            .split('/')
            .nth(1)
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }
    pub fn channels(&self) -> i32 {
        self.rtpmap
            .split('/')
            .nth(2)
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(1)
    }
    pub(super) fn new(payload: u8, rtpmap: String, parameters: Option<String>) -> Self {
        Self {
            payload,
            rtpmap,
            parameters,
            video: matches!(payload, 24 | 26 | 28 | 31..=34),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdpMediaKind {
    Invalid,
    Audio,
    Video,
    Application,
    Data,
    Control,
    Image,
    Message,
    Text,
}
impl SdpMediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Application => "application",
            Self::Data => "data",
            Self::Control => "control",
            Self::Image => "image",
            Self::Message => "message",
            Self::Text => "text",
        }
    }
    fn parse(text: &str) -> Result<Self, SdpError> {
        [
            Self::Invalid,
            Self::Audio,
            Self::Video,
            Self::Application,
            Self::Data,
            Self::Control,
            Self::Image,
            Self::Message,
            Self::Text,
        ]
        .into_iter()
        .find(|kind| kind.as_str().eq_ignore_ascii_case(text))
        .ok_or(SdpError::Invalid)
    }
    fn rtp(self) -> bool {
        matches!(self, Self::Audio | Self::Video | Self::Text)
    }
}

/// Recognised attributes are rendered in the original stack's groups. Unknown
/// attributes remain ordered and may repeat; they are not a map of unique keys.
#[derive(Clone, Default, PartialEq, Eq)]
struct Attributes {
    known: Vec<(String, String)>,
    extra: Vec<String>,
}
impl Attributes {
    fn set(&mut self, key: &str, value: &str) {
        if let Some(pair) = self.known.iter_mut().find(|pair| pair.0 == key) {
            pair.1 = value.into();
        } else {
            self.known.push((key.into(), value.into()));
        }
    }
    fn write(&self, output: &mut String, keys: &[&str]) {
        for key in keys {
            for (_, value) in self.known.iter().filter(|pair| pair.0 == *key) {
                if !value.is_empty() {
                    line(output, &format!("{key}{value}"));
                }
            }
        }
    }
    fn parse_common(&mut self, text: &str) -> bool {
        for key in [
            "a=ice-ufrag:",
            "a=ice-pwd:",
            "a=ice-options:",
            "a=setup:",
            "a=fingerprint:",
        ] {
            if let Some(value) = text.strip_prefix(key) {
                self.set(key, value);
                return true;
            }
        }
        if text.starts_with("a=candidate:") {
            self.known.push(("a=candidate:".into(), text[12..].into()));
            return true;
        }
        // The original parser recognises ice-lite but omits it from ToString.
        text == "a=ice-lite"
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SdpMedia {
    pub(super) kind: SdpMediaKind,
    pub(super) port: i32,
    pub(super) port_count: i32,
    pub(super) transport: String,
    pub(super) connection: Option<SdpConnection>,
    pub(super) direction: Option<SdpDirection>,
    pub(super) formats: Vec<SdpFormat>,
    pub(super) mid: Option<String>,
    description: Option<String>,
    bandwidth: Vec<String>,
    attributes: Attributes,
    application_formats: String,
    ssrc: Vec<String>,
}
impl SdpMedia {
    pub(super) fn local(kind: SdpMediaKind, port: u16, formats: Vec<SdpFormat>) -> Self {
        let mut media = Self::audio(port, formats);
        media.kind = kind;
        media
    }
    pub(super) fn set_ssrc(&mut self, source: u32, cname: &str) {
        self.ssrc = vec![format!("a=ssrc:{source} cname:{cname}")];
    }
    pub(super) fn sources(&self) -> Vec<u32> {
        let mut result = Vec::new();
        for source in &self.ssrc {
            if let Some(source) = source
                .strip_prefix("a=ssrc:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
            {
                if !result.contains(&source) {
                    result.push(source);
                }
            }
        }
        result
    }
    pub(super) fn evs_packet_time(&mut self) {
        if self.kind == SdpMediaKind::Audio
            && self.formats.iter().any(|format| {
                format
                    .name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("EVS"))
            })
        {
            self.attributes.extra.retain(|text| {
                !text.to_ascii_lowercase().starts_with("a=ptime:")
                    && !text.to_ascii_lowercase().starts_with("a=maxptime:")
            });
            self.attributes
                .extra
                .extend(["a=ptime:20".into(), "a=maxptime:120".into()]);
        }
    }
    pub fn kind(&self) -> SdpMediaKind {
        self.kind
    }
    pub fn port(&self) -> i32 {
        self.port
    }
    pub fn port_count(&self) -> i32 {
        self.port_count
    }
    pub fn transport(&self) -> &str {
        &self.transport
    }
    pub fn connection(&self) -> Option<&SdpConnection> {
        self.connection.as_ref()
    }
    pub fn direction(&self) -> Option<SdpDirection> {
        self.direction
    }
    pub fn formats(&self) -> &[SdpFormat] {
        &self.formats
    }
    /// The parser retains video FMTP lines in their canonical wire order.
    /// Codec matching also needs their effective per-payload parameters.
    pub(super) fn codec_formats(&self) -> Vec<SdpFormat> {
        let mut formats = self.formats.clone();
        if self.kind == SdpMediaKind::Video {
            for value in self
                .attributes
                .extra
                .iter()
                .filter_map(|line| line.strip_prefix("a=fmtp:"))
            {
                let (id, parameters) = split_field(value);
                if let Ok(id) = id.parse::<u8>() {
                    if let Some(format) = formats.iter_mut().find(|f| f.payload == id) {
                        format.parameters = Some(parameters.into());
                    }
                }
            }
        }
        formats
    }
    pub fn mid(&self) -> Option<&str> {
        self.mid.as_deref()
    }
    pub fn extra_attributes(&self) -> &[String] {
        &self.attributes.extra
    }
    pub(super) fn audio(port: u16, formats: Vec<SdpFormat>) -> Self {
        Self {
            kind: SdpMediaKind::Audio,
            port: port.into(),
            port_count: 0,
            transport: "RTP/AVP".into(),
            connection: None,
            direction: Some(SdpDirection::SendRecv),
            formats,
            mid: None,
            description: None,
            bandwidth: Vec::new(),
            attributes: Attributes::default(),
            application_formats: String::new(),
            ssrc: Vec::new(),
        }
    }
    fn parse(text: &str) -> Result<Option<Self>, SdpError> {
        let parts: Vec<&str> = text.split_whitespace().collect();
        if parts.len() < 3 {
            return Ok(None);
        }
        let kind = SdpMediaKind::parse(parts[0])?;
        let (port, count) = parts[1].split_once('/').unwrap_or((parts[1], "0"));
        let Ok(port) = port.parse::<i32>() else {
            return Ok(None);
        };
        let mut media = Self::audio(0, Vec::new());
        media.kind = kind;
        media.port = port;
        media.port_count = count.parse().unwrap_or(0);
        media.transport = parts[2].into();
        media.direction = None;
        if !matches!(kind, SdpMediaKind::Application | SdpMediaKind::Message) {
            for payload in parts
                .iter()
                .skip(3)
                .filter_map(|part| part.parse::<u8>().ok())
            {
                if let Some(rtpmap) = static_map(payload) {
                    if !media.formats.iter().any(|format| format.payload == payload) {
                        media
                            .formats
                            .push(SdpFormat::new(payload, rtpmap.into(), None));
                    }
                }
            }
        } else if matches!(kind, SdpMediaKind::Application | SdpMediaKind::Message) {
            media.application_formats = if kind == SdpMediaKind::Message {
                "*".into()
            } else {
                parts.iter().skip(3).copied().collect::<Vec<_>>().join(" ")
            };
        }
        Ok(Some(media))
    }
    fn parse_line(&mut self, text: &str) -> Result<(), SdpError> {
        if let Some(value) = text.strip_prefix("c=") {
            self.connection = Some(SdpConnection::parse(value));
        } else if let Some(value) = text.strip_prefix("i=") {
            self.description = Some(value.into());
        } else if text.starts_with("b=") {
            self.bandwidth.push(text.into());
        } else if let Some(direction) = text.strip_prefix("a=").and_then(SdpDirection::parse) {
            self.direction = Some(direction);
        } else if let Some(value) = text.strip_prefix("a=mid:") {
            self.mid = Some(value.into());
        } else if self.attributes.parse_common(text) {
        } else if let Some(value) = text.strip_prefix("a=ssrc:") {
            let id = value.split_whitespace().next().unwrap_or("");
            let suffix = value
                .split_once(" cname:")
                .map(|pair| format!(" cname:{}", pair.1))
                .unwrap_or_default();
            self.ssrc.push(format!("a=ssrc:{id}{suffix}"));
        } else if let Some(value) = text
            .strip_prefix("a=rtpmap:")
            .filter(|value| value.contains(char::is_whitespace))
        {
            if self.kind == SdpMediaKind::Application {
                self.attributes
                    .known
                    .push(("a=rtpmap:".into(), value.into()));
                return Ok(());
            }
            if !self.kind.rtp() {
                return Ok(());
            }
            let (id, map) = split_field(value);
            if let Ok(id) = id.parse::<i32>() {
                let payload = u8::try_from(id)
                    .ok()
                    .filter(|id| *id < 128)
                    .ok_or(SdpError::Invalid)?;
                if let Some(format) = self
                    .formats
                    .iter_mut()
                    .find(|format| format.payload == payload)
                {
                    format.rtpmap = map.into();
                } else {
                    let mut format = SdpFormat::new(payload, map.into(), None);
                    format.video = self.kind == SdpMediaKind::Video;
                    self.formats.push(format);
                }
            }
        } else if let Some(value) = text
            .strip_prefix("a=fmtp:")
            .filter(|value| value.contains(char::is_whitespace))
        {
            if self.kind == SdpMediaKind::Application {
                self.attributes.known.push(("a=fmtp:".into(), value.into()));
                return Ok(());
            }
            if self.kind == SdpMediaKind::Video {
                self.attributes.extra.push(text.into());
                return Ok(());
            }
            if !self.kind.rtp() {
                return Ok(());
            }
            let (id, parameters) = split_field(value);
            if let Ok(id) = id.parse::<u8>() {
                if let Some(format) = self.formats.iter_mut().find(|format| format.payload == id) {
                    format.parameters = Some(parameters.into());
                }
            }
        } else if let Some(value) = text.strip_prefix("a=crypto:") {
            // Only retain valid declarations, as the original SDP parser does.
            // This is metadata, never activation of SRTP on the plain RTP session.
            if let Some(value) = normalize_crypto(value)? {
                self.attributes.known.push(("a=crypto:".into(), value));
            }
        } else if let Some(key) = [
            "a=sctpmap:",
            "a=sctp-port:",
            "a=max-message-size:",
            "a=ssrc-group:",
        ]
        .into_iter()
        .find(|key| text.starts_with(key))
        {
            self.attributes.set(key, &text[key.len()..]);
        } else {
            self.attributes.extra.push(text.into());
        }
        Ok(())
    }
    fn write(&self, output: &mut String) {
        let formats = if !matches!(self.kind, SdpMediaKind::Application | SdpMediaKind::Message) {
            self.formats
                .iter()
                .map(|format| format.payload.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            self.application_formats.clone()
        };
        line(
            output,
            &format!(
                "m={} {} {} {}",
                self.kind.as_str(),
                self.port,
                self.transport,
                formats
            ),
        );
        if let Some(description) = &self.description {
            line(output, &format!("i={description}"));
        }
        if let Some(connection) = &self.connection {
            line(output, &connection.to_sdp());
        }
        for bandwidth in &self.bandwidth {
            line(output, bandwidth);
        }
        if let Some(mid) = &self.mid {
            if !mid.is_empty() {
                line(output, &format!("a=mid:{mid}"));
            }
        }
        self.attributes.write(
            output,
            &[
                "a=ice-ufrag:",
                "a=ice-pwd:",
                "a=ice-options:",
                "a=candidate:",
                "a=setup:",
                "a=fingerprint:",
            ],
        );
        for format in &self.formats {
            line(
                output,
                &format!("a=rtpmap:{} {}", format.payload, format.rtpmap),
            );
            if let Some(parameters) = &format.parameters {
                if !parameters.is_empty() {
                    line(output, &format!("a=fmtp:{} {}", format.payload, parameters));
                }
            }
            if format.video {
                line(
                    output,
                    &format!("a=rtcp-fb:{} transport-cc", format.payload),
                );
            }
        }
        self.attributes.write(output, &["a=rtpmap:", "a=fmtp:"]);
        for attribute in &self.attributes.extra {
            line(output, attribute);
        }
        self.attributes.write(output, &["a=crypto:"]);
        if let Some(direction) = self.direction {
            line(output, &format!("a={}", direction.as_str()));
        }
        self.attributes.write(output, &["a=ssrc-group:"]);
        for ssrc in &self.ssrc {
            line(output, ssrc);
        }
        self.attributes.write(
            output,
            &["a=sctpmap:", "a=sctp-port:", "a=max-message-size:"],
        );
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SdpDocument {
    pub(super) origin: SdpOrigin,
    pub(super) name: String,
    pub(super) timing: String,
    pub(super) connection: Option<SdpConnection>,
    pub(super) direction: Option<SdpDirection>,
    pub(super) media: Vec<SdpMedia>,
    description: Option<String>,
    bandwidth: Vec<String>,
    attributes: Attributes,
}
impl Default for SdpDocument {
    fn default() -> Self {
        Self {
            origin: SdpOrigin::default(),
            name: "sipsorcery".into(),
            timing: "0 0".into(),
            connection: None,
            direction: None,
            media: Vec::new(),
            description: None,
            bandwidth: Vec::new(),
            attributes: Attributes::default(),
        }
    }
}
impl SdpDocument {
    pub fn origin(&self) -> &SdpOrigin {
        &self.origin
    }
    pub fn session_name(&self) -> &str {
        &self.name
    }
    pub fn timing(&self) -> &str {
        &self.timing
    }
    pub fn connection(&self) -> Option<&SdpConnection> {
        self.connection.as_ref()
    }
    pub fn direction(&self) -> Option<SdpDirection> {
        self.direction
    }
    pub fn media(&self) -> &[SdpMedia] {
        &self.media
    }
    pub fn extra_attributes(&self) -> &[String] {
        &self.attributes.extra
    }
    pub fn media_connection(&self, index: usize) -> Option<&SdpConnection> {
        self.media
            .get(index)?
            .connection
            .as_ref()
            .or(self.connection.as_ref())
    }
    pub fn parse(body: &str) -> Result<Self, SdpError> {
        if body.trim().is_empty() {
            return Err(SdpError::Empty);
        }
        if !bounded_sdp(body) {
            return Err(SdpError::TooLarge);
        }
        Self::parse_lines(body)
    }
    pub(super) fn normalized(&self) -> Result<Self, SdpError> {
        // Rendering expands static payload declarations. The external boundary
        // already bounded the input; do not apply it again to owned expansion.
        Self::parse_lines(&self.to_sdp())
    }
    fn parse_lines(body: &str) -> Result<Self, SdpError> {
        let mut document = Self::default();
        for text in body
            .split(['\r', '\n'])
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if let Some(value) = text.strip_prefix("m=") {
                if let Some(media) = SdpMedia::parse(value)? {
                    document.media.push(media);
                }
            } else if let Some(media) = document.media.last_mut() {
                media.parse_line(text)?;
            } else if let Some(value) = text.strip_prefix("o=") {
                let mut fields = value.splitn(6, char::is_whitespace);
                let parts: Vec<_> = fields.by_ref().collect();
                if parts.len() == 6 {
                    document.origin = SdpOrigin {
                        username: parts[0].into(),
                        session_id: parts[1].into(),
                        version: parts[2].parse().unwrap_or(0),
                        network: parts[3].into(),
                        address_type: parts[4].into(),
                        address: Some(parts[5].into()),
                    };
                }
            } else if text.starts_with("v=") {
            } else if let Some(value) = text.strip_prefix("s=") {
                document.name = value.into();
            } else if let Some(value) = text.strip_prefix("t=") {
                document.timing = value.into();
            } else if let Some(value) = text.strip_prefix("i=") {
                document.description = Some(value.into());
            } else if let Some(value) = text.strip_prefix("c=") {
                document.connection = Some(SdpConnection::parse(value));
            } else if text.starts_with("b=") {
                document.bandwidth.push(text.into());
            } else if let Some(direction) = text.strip_prefix("a=").and_then(SdpDirection::parse) {
                document.direction = Some(direction);
            } else if document.attributes.parse_common(text) {
            } else if let Some(value) = text.strip_prefix("a=group:") {
                document.attributes.set("a=group:", value);
            } else {
                document.attributes.extra.push(text.into());
            }
        }
        for media in &mut document.media {
            // The original message serializer cannot render a path declaration;
            // call negotiation normalises through that serializer before use.
            if media.kind == SdpMediaKind::Message
                && media
                    .attributes
                    .extra
                    .iter()
                    .any(|line| line.starts_with("a=path:"))
            {
                return Err(SdpError::Invalid);
            }
            if media.direction.is_none() {
                media.direction = document
                    .direction
                    .or_else(|| media.kind.rtp().then_some(SdpDirection::SendRecv));
            }
        }
        Ok(document)
    }
    pub fn to_sdp(&self) -> String {
        let mut output = String::new();
        line(&mut output, "v=0");
        let origin = &self.origin;
        line(
            &mut output,
            &format!(
                "o={} {} {} {} {} {}",
                origin.username,
                origin.session_id,
                origin.version,
                origin.network,
                origin.address_type,
                origin.address.as_deref().unwrap_or("")
            ),
        );
        line(&mut output, &format!("s={}", self.name));
        if let Some(connection) = &self.connection {
            line(&mut output, &connection.to_sdp());
        }
        for bandwidth in &self.bandwidth {
            line(&mut output, bandwidth);
        }
        line(&mut output, &format!("t={}", self.timing));
        if let Some(description) = &self.description {
            line(&mut output, &format!("i={description}"));
        }
        self.attributes.write(
            &mut output,
            &[
                "a=ice-ufrag:",
                "a=ice-pwd:",
                "a=ice-options:",
                "a=candidate:",
                "a=setup:",
                "a=fingerprint:",
                "a=group:",
            ],
        );
        if let Some(direction) = self.direction {
            line(&mut output, &format!("a={}", direction.as_str()));
        }
        for attribute in &self.attributes.extra {
            line(&mut output, attribute);
        }
        for media in &self.media {
            media.write(&mut output);
        }
        output
    }
    /// ACK rejection uses the same parser as negotiation, including all media.
    pub fn rejected_answer(body: &str) -> Option<String> {
        let mut answer = Self::parse(body).ok()?;
        answer.origin.session_id = "0".into();
        answer.origin.version = 0;
        answer.connection = Some(SdpConnection::parse("IN IP4 0.0.0.0"));
        for media in &mut answer.media {
            media.port = 0;
            media.direction = Some(SdpDirection::Inactive);
        }
        Some(answer.to_sdp())
    }
}
fn line(output: &mut String, value: &str) {
    let _ = write!(output, "{value}\r\n");
}
fn split_field(value: &str) -> (&str, &str) {
    value
        .find(char::is_whitespace)
        .map(|index| (&value[..index], value[index..].trim()))
        .unwrap_or((value, ""))
}
fn normalize_crypto(value: &str) -> Result<Option<String>, SdpError> {
    let fields: Vec<_> = value.split_whitespace().collect();
    if fields.len() < 3 {
        return Ok(None);
    }
    let tag = fields[0]
        .parse::<u32>()
        .ok()
        .filter(|tag| (1..=999_999_999).contains(tag))
        .ok_or(SdpError::Invalid)?;
    let length = match fields[1] {
        "AES_CM_128_HMAC_SHA1_80" | "AES_CM_128_HMAC_SHA1_32" => 30,
        "AES_256_CM_HMAC_SHA1_80" | "AES_256_CM_HMAC_SHA1_32" => 46,
        _ => return Ok(None),
    };
    let Some(key) = fields[2].strip_prefix("inline:") else {
        return Ok(None);
    };
    let (key, suffix) = key
        .split_once('|')
        .map(|(key, tail)| (key, format!("|{tail}")))
        .unwrap_or((key, String::new()));
    let Some(key) = base64_prefix(key, length) else {
        return Ok(None);
    };
    let extra = if fields.len() > 3 {
        format!(" {}", fields[3..].join(" "))
    } else {
        String::new()
    };
    Ok(Some(format!(
        "{tag} {} inline:{key}{suffix}{extra}",
        fields[1]
    )))
}
fn base64_prefix(value: &str, count: usize) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return None;
    }
    let padding = bytes.iter().rev().take_while(|&&byte| byte == b'=').count();
    if padding > 2
        || bytes[..bytes.len() - padding]
            .iter()
            .any(|byte| !ALPHABET.contains(byte))
        || bytes.len() / 4 * 3 - padding < count
    {
        return None;
    }
    let mut output = bytes[..count.div_ceil(3) * 4].to_vec();
    match count % 3 {
        1 => {
            let index = output.len() - 3;
            let bits = ALPHABET.iter().position(|byte| *byte == output[index])?;
            output[index] = ALPHABET[bits & 0x30];
            output[index + 1] = b'=';
            output[index + 2] = b'=';
        }
        2 => {
            let index = output.len() - 2;
            let bits = ALPHABET.iter().position(|byte| *byte == output[index])?;
            output[index] = ALPHABET[bits & 0x3c];
            output[index + 1] = b'=';
        }
        _ => {}
    }
    String::from_utf8(output).ok()
}
// RFC 3551's static RTP/AVP declarations, including the original parser's use
// of that table for payloads appearing on any audio/video RTP media line.
fn static_map(payload: u8) -> Option<&'static str> {
    Some(match payload {
        0 => "PCMU/8000",
        3 => "GSM/8000",
        4 => "G723/8000",
        5 => "DVI4/8000",
        6 => "DVI4/16000",
        7 => "LPC/8000",
        8 => "PCMA/8000",
        9 => "G722/8000",
        10 => "L16/44100/2",
        11 => "L16/44100",
        12 => "QCELP/8000",
        13 => "CN/8000",
        14 => "MPA/90000",
        15 => "G728/8000",
        16 => "DVI4/11025",
        17 => "DVI4/22050",
        18 => "G729/8000",
        24 => "CELB/90000",
        26 => "JPEG/90000",
        28 => "NV/90000",
        31 => "H261/90000",
        32 => "MPV/90000",
        33 => "MP2T/90000",
        34 => "H263/90000",
        _ => return None,
    })
}
