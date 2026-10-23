use super::{evs_header_full, evs_offer_parameters};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatError;
impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid_format")
    }
}
impl std::error::Error for FormatError {}

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
    fn parse(name: &str) -> Result<Self, FormatError> {
        [Self::G722, Self::Pcma, Self::Pcmu, Self::Opus, Self::Evs]
            .into_iter()
            .find(|codec| codec.name().eq_ignore_ascii_case(name))
            .ok_or(FormatError)
    }
    /// Static SDP declarations. The media owner must construct and qualify the
    /// corresponding encoder/decoder before advertising a usable session.
    pub fn offered_formats(self) -> Vec<AudioFormat> {
        let specs: &[(i32, u32, u32, u8, Option<&str>)] = match self {
            Self::G722 => &[(9, 16000, 8000, 1, None)],
            Self::Pcma => &[(8, 8000, 8000, 1, None)],
            Self::Pcmu => &[(0, 8000, 8000, 1, None)],
            Self::Opus => &[(111, 48000, 48000, 2, Some("useinbandfec=1"))],
            Self::Evs => &[
                (126, 48000, 16000, 1, Some(evs_offer_parameters(1).unwrap())),
                (125, 48000, 16000, 2, Some(evs_offer_parameters(2).unwrap())),
            ],
        };
        specs
            .iter()
            .map(|&(id, pcm, rtp, channels, params)| {
                AudioFormat::new(id, self.name(), pcm, rtp, channels, params)
                    .expect("fixed offered audio format")
            })
            .collect()
    }
}

/// One negotiated direction's metadata. No encoder state is shared or implied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    payload: u8,
    name: String,
    codec: Codec,
    pcm_rate: u32,
    rtp_rate: u32,
    channels: u8,
    parameters: Option<String>,
}
impl AudioFormat {
    pub fn new(
        payload: i32,
        name: &str,
        pcm_rate: u32,
        rtp_rate: u32,
        channels: u8,
        parameters: Option<&str>,
    ) -> Result<Self, FormatError> {
        let codec = Codec::parse(name)?;
        let valid = match codec {
            Codec::G722 => pcm_rate == 16000 && rtp_rate == 8000 && channels == 1,
            Codec::Pcma | Codec::Pcmu => pcm_rate == 8000 && rtp_rate == 8000 && channels == 1,
            Codec::Opus => pcm_rate == 48000 && rtp_rate == 48000 && matches!(channels, 1 | 2),
            Codec::Evs => {
                matches!(pcm_rate, 16000 | 48000)
                    && rtp_rate == 16000
                    && matches!(channels, 1 | 2)
                    && (channels == 1 || evs_header_full(parameters)?)
            }
        };
        if !valid || !(0..=127).contains(&payload) {
            return Err(FormatError);
        }
        Ok(Self {
            payload: payload as u8,
            name: name.into(),
            codec,
            pcm_rate,
            rtp_rate,
            channels,
            parameters: parameters.map(Into::into),
        })
    }
    pub fn payload(&self) -> u8 {
        self.payload
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn codec(&self) -> Codec {
        self.codec
    }
    pub fn pcm_rate(&self) -> u32 {
        self.pcm_rate
    }
    pub fn rtp_rate(&self) -> u32 {
        self.rtp_rate
    }
    pub fn channels(&self) -> u8 {
        self.channels
    }
    pub fn parameters(&self) -> Option<&str> {
        self.parameters.as_deref()
    }
    pub fn rtp_duration(&self) -> u32 {
        self.rtp_rate / 50
    }
}
