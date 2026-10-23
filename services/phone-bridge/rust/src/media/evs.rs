//! The Phone EVS Primary CBR SDP policy. This describes formats, not codec availability.
use super::FormatError;
use std::collections::HashMap;

pub const EVS_OFFER_PARAMETERS: &str = "br=7.2-128;bw=nb-fb;max-red=0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvsProfile {
    pub bitrate: u32,
    /// Reference codec bandwidth: NB=0, WB=1, SWB=2, FB=3.
    pub bandwidth: u8,
}

pub fn evs_offer_parameters(channels: u8) -> Result<&'static str, FormatError> {
    match channels {
        1 => Ok(EVS_OFFER_PARAMETERS),
        2 => Ok("br=7.2-128;bw=nb-fb;max-red=0;channels=2;hf-only=1"),
        _ => Err(FormatError),
    }
}

pub fn evs_channels(rtpmap: Option<&str>) -> u8 {
    match rtpmap {
        Some(value)
            if value.eq_ignore_ascii_case("EVS/16000")
                || value.eq_ignore_ascii_case("EVS/16000/1") =>
        {
            1
        }
        Some(value) if value.eq_ignore_ascii_case("EVS/16000/2") => 2,
        _ => 0,
    }
}

fn fields(text: Option<&str>) -> Result<HashMap<String, String>, FormatError> {
    let mut values = HashMap::new();
    let text = text.unwrap_or_default();
    // The original accepts whitespace-only input before applying size/control limits.
    if text.trim().is_empty() {
        return Ok(values);
    }
    if text.encode_utf16().count() > 1024 || text.chars().any(char::is_control) {
        return Err(FormatError);
    }
    for field in text.split(';').filter(|field| !field.is_empty()) {
        let (name, value) = field.split_once('=').ok_or(FormatError)?;
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty()
            || name.len() > 64
            || value.is_empty()
            || value.encode_utf16().count() > 128
            || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || values
                .insert(name.to_ascii_lowercase(), value.to_lowercase())
                .is_some()
            || values.len() > 24
        {
            return Err(FormatError);
        }
    }
    Ok(values)
}

pub fn evs_header_full(text: Option<&str>) -> Result<bool, FormatError> {
    match fields(text)?.get("hf-only").map(String::as_str) {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => Err(FormatError),
    }
}

const RATES: [u16; 12] = [59, 72, 80, 96, 132, 164, 244, 320, 480, 640, 960, 1280];

/// A permitted decimal bitrate, including the original .NET Decimal display scale.
#[derive(Clone, Copy)]
struct RateValue {
    tenths: u16,
    scale: u8,
}
impl RateValue {
    fn parse(text: &str) -> Result<Self, FormatError> {
        let mut parts = text.split('.');
        let integer = parts.next().unwrap_or_default();
        let fraction = parts.next().unwrap_or_default();
        if parts.next().is_some()
            || integer.is_empty() && fraction.is_empty()
            || !integer
                .bytes()
                .chain(fraction.bytes())
                .all(|c| c.is_ascii_digit())
        {
            return Err(FormatError);
        }
        let integer = integer.trim_start_matches('0');
        const MAX: u128 = (1_u128 << 96) - 1;
        // Decimal permits scale <=28 and a 96-bit coefficient, with ties-to-even
        // rounding when a longer fraction is supplied. Do not use binary floats.
        for scale in (0..=fraction.len().min(28)).rev() {
            let coefficient = integer
                .bytes()
                .chain(fraction.bytes().take(scale))
                .try_fold(0_u128, |n, c| {
                    n.checked_mul(10)?.checked_add((c - b'0').into())
                });
            let Some(mut coefficient) = coefficient else {
                continue;
            };
            let discarded = &fraction.as_bytes()[scale..];
            if let Some(&first) = discarded.first() {
                if first > b'5'
                    || first == b'5'
                        && (coefficient % 2 != 0 || discarded[1..].iter().any(|c| *c != b'0'))
                {
                    coefficient = coefficient.checked_add(1).ok_or(FormatError)?;
                }
            }
            if coefficient > MAX {
                continue;
            }
            let tenths = RATES
                .into_iter()
                .find(|rate| {
                    if scale == 0 {
                        coefficient * 10 == u128::from(*rate)
                    } else {
                        coefficient == u128::from(*rate) * 10_u128.pow(scale as u32 - 1)
                    }
                })
                .ok_or(FormatError)?;
            return Ok(Self {
                tenths,
                scale: scale as u8,
            });
        }
        Err(FormatError)
    }
    fn text(self) -> String {
        if self.scale == 0 {
            return (self.tenths / 10).to_string();
        }
        format!(
            "{}.{}{}",
            self.tenths / 10,
            self.tenths % 10,
            "0".repeat(self.scale as usize - 1)
        )
    }
}

#[derive(Clone, Copy)]
struct Rate {
    low: RateValue,
    high: RateValue,
}
impl Rate {
    fn parse(text: Option<&str>) -> Result<Self, FormatError> {
        let Some(text) = text else {
            return Ok(Self {
                low: RateValue {
                    tenths: 59,
                    scale: 1,
                },
                high: RateValue {
                    tenths: 1280,
                    scale: 0,
                },
            });
        };
        let mut parts = text.split('-');
        let low = RateValue::parse(parts.next().unwrap_or_default())?;
        let high = match parts.next() {
            Some(value) => {
                let high = RateValue::parse(value)?;
                if high.tenths <= low.tenths {
                    return Err(FormatError);
                }
                high
            }
            None => low,
        };
        if parts.next().is_some() {
            return Err(FormatError);
        }
        Ok(Self { low, high })
    }
    fn receive(self) -> Self {
        Self {
            low: if self.low.tenths <= 72 {
                RateValue {
                    tenths: 72,
                    scale: 1,
                }
            } else {
                self.low
            },
            ..self
        }
    }
    fn same(self, other: Self) -> bool {
        self.low.tenths == other.low.tenths && self.high.tenths == other.high.tenths
    }
    fn text(self) -> String {
        if self.low.tenths == self.high.tenths {
            self.low.text()
        } else {
            format!("{}-{}", self.low.text(), self.high.text())
        }
    }
}

fn bandwidth(text: Option<&str>) -> Result<u8, FormatError> {
    match text {
        None | Some("nb-fb") => Ok(15),
        Some("nb") => Ok(1),
        Some("wb") => Ok(2),
        Some("swb") => Ok(4),
        Some("fb") => Ok(8),
        Some("nb-wb") => Ok(3),
        Some("nb-swb") => Ok(7),
        _ => Err(FormatError),
    }
}
fn bandwidth_text(value: u8) -> &'static str {
    match value {
        1 => "nb",
        2 => "wb",
        4 => "swb",
        8 => "fb",
        3 => "nb-wb",
        7 => "nb-swb",
        15 => "nb-fb",
        _ => unreachable!("validated EVS bandwidth"),
    }
}
fn profile(rate: Rate, bandwidths: u8) -> Result<EvsProfile, FormatError> {
    for bitrate in [
        128000, 96000, 64000, 48000, 32000, 24400, 16400, 13200, 9600, 8000, 7200,
    ] {
        for bandwidth in (0..=3).rev() {
            if u32::from(rate.low.tenths) * 100 <= bitrate
                && bitrate <= u32::from(rate.high.tenths) * 100
                && bandwidths & (1 << bandwidth) != 0
                && !(bandwidth == 0 && bitrate > 24400
                    || bandwidth == 2 && bitrate < 9600
                    || bandwidth == 3 && bitrate < 16400)
            {
                return Ok(EvsProfile { bitrate, bandwidth });
            }
        }
    }
    Err(FormatError)
}

pub fn evs_sender(parameters: Option<&str>) -> Result<EvsProfile, FormatError> {
    let values = fields(parameters)?;
    let get = |key: &str| values.get(key).map(String::as_str);
    profile(
        Rate::parse(get("br-send").or_else(|| get("br")))?,
        bandwidth(get("bw-send").or_else(|| get("bw")))?,
    )
}

/// Returns the exact answer parameters, or None when EVS must be removed from
/// this intersection. Other agreed codecs remain available to the SDP owner.
pub fn evs_select(remote: Option<&str>, answering_offer: bool, channels: u8) -> Option<String> {
    select(remote, answering_offer, channels).ok()
}
fn select(
    remote: Option<&str>,
    answering_offer: bool,
    channels: u8,
) -> Result<String, FormatError> {
    let values = fields(remote)?;
    let get = |key: &str| values.get(key).map(String::as_str);
    for (key, allowed) in [
        ("evs-mode-switch", &["0"][..]),
        ("dtx", &["0"][..]),
        ("dtx-recv", &["0"][..]),
        ("cmr", &["-1"][..]),
        ("ch-aw-recv", &["0", "-1"][..]),
        ("max-red", &["0"][..]),
        ("hf-only", &["0", "1"][..]),
    ] {
        if get(key).is_some_and(|value| !allowed.contains(&value)) {
            return Err(FormatError);
        }
    }
    let channel = match channels {
        1 => "1",
        2 => "2",
        _ => return Err(FormatError),
    };
    for key in ["channels", "ch-send", "ch-recv"] {
        if get(key).is_some_and(|value| value != channel) {
            return Err(FormatError);
        }
    }
    if channels == 2 && (get("hf-only") != Some("1") || get("channels") != Some("2")) {
        return Err(FormatError);
    }
    for prefix in ["br", "bw"] {
        if let Some(both) = get(prefix) {
            for direction in ["send", "recv"] {
                if let Some(value) = get(&format!("{prefix}-{direction}")) {
                    let same = if prefix == "br" {
                        Rate::parse(Some(value))?.same(Rate::parse(Some(both))?)
                    } else {
                        bandwidth(Some(value))? == bandwidth(Some(both))?
                    };
                    if !same {
                        return Err(FormatError);
                    }
                }
            }
        }
    }
    let remote_receive = Rate::parse(get("br-recv").or_else(|| get("br")))?;
    let remote_send = Rate::parse(get("br-send").or_else(|| get("br")))?;
    let receive = remote_send.receive();
    let remote_receive_bandwidth = bandwidth(get("bw-recv").or_else(|| get("bw")))?;
    let receive_bandwidth = bandwidth(get("bw-send").or_else(|| get("bw")))?;
    let send = profile(remote_receive.receive(), remote_receive_bandwidth)?;
    profile(receive, receive_bandwidth)?;
    if !answering_offer && (remote_send.low.tenths < 72 || remote_receive.low.tenths < 72) {
        return Err(FormatError);
    }
    let mut result = Vec::new();
    if get("br").is_none() && (get("br-send").is_some() || get("br-recv").is_some()) {
        let rate = RateValue {
            tenths: (send.bitrate / 100) as u16,
            scale: if send.bitrate % 1000 == 0 { 0 } else { 1 },
        };
        result.push(format!("br-send={}", rate.text()));
        result.push(format!("br-recv={}", receive.text()));
    } else {
        result.push(format!("br={}", receive.text()));
        if get("br-send").is_some() {
            result.push(format!("br-recv={}", receive.text()));
        }
        if get("br-recv").is_some() {
            result.push(format!("br-send={}", receive.text()));
        }
    }
    if get("bw").is_none() && (get("bw-send").is_some() || get("bw-recv").is_some()) {
        result.push(format!("bw-send={}", bandwidth_text(1 << send.bandwidth)));
        result.push(format!("bw-recv={}", bandwidth_text(receive_bandwidth)));
    } else {
        result.push(format!("bw={}", bandwidth_text(receive_bandwidth)));
        if get("bw-send").is_some() {
            result.push(format!("bw-recv={}", bandwidth_text(receive_bandwidth)));
        }
        if get("bw-recv").is_some() {
            result.push(format!("bw-send={}", bandwidth_text(receive_bandwidth)));
        }
    }
    result.push("max-red=0".into());
    for key in ["hf-only", "evs-mode-switch", "channels"] {
        if let Some(value) = get(key) {
            result.push(format!("{key}={value}"));
        }
    }
    if get("ch-send").is_some() {
        result.push(format!("ch-recv={channel}"));
    }
    if get("ch-recv").is_some() {
        result.push(format!("ch-send={channel}"));
    }
    Ok(result.join(";"))
}
