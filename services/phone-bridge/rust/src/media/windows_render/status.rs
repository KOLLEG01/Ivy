//! Original managed render status vocabulary. Mix descriptions are formatting
//! only; they never change native format admission or the byte conversion plan.
use super::{
    RenderError, Result,
    device::{Info, Unavailable},
};
mod names;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::media) struct Status {
    pub latency_ms: u32,
    pub low_latency: bool,
    pub unavailable: Option<String>,
}
impl Status {
    pub(super) fn new(info: &Info) -> Result<Self> {
        let unavailable=match info.unavailable {
            None=>None,
            Some(Unavailable::Client3)=>Some("IAudioClient3 is not supported on this device (requires Windows 10 version 1607 or later)".into()),
            Some(Unavailable::Engine)=>Some("the audio engine declined the low-latency request".into()),
            Some(Unavailable::Format)=>Some(format!("source format could not be adapted to the engine mix format ({}) without resampling",description(&info.mix)?)),
        };
        // Every published name and numeric/GUID description here is ASCII and
        // bounded well below the route's 1024 UTF-16-code-unit limit.
        Ok(Self {
            latency_ms: info.latency_ms,
            low_latency: info.low_latency,
            unavailable,
        })
    }
}
fn description(bytes: &[u8]) -> Result<String> {
    if bytes.len() < 18 {
        return Err(RenderError::Configuration);
    }
    let word = |i| u16::from_le_bytes(bytes[i..i + 2].try_into().unwrap());
    let tag = word(0);
    let channels = word(2) as i16;
    let rate = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let bits = word(14) as i16;
    Ok(match tag {
        1 => format!("{bits} bit PCM: {rate}Hz {channels} channels"),
        3 => format!("{bits} bit IEEEFloat: {rate}Hz {channels} channels"),
        65534 => {
            if bytes.len() < 40 {
                return Err(RenderError::Configuration);
            }
            let guid = (u128::from(u32::from_le_bytes(bytes[24..28].try_into().unwrap())) << 96)
                | (u128::from(word(28)) << 80)
                | (u128::from(word(30)) << 64)
                | u128::from(u64::from_be_bytes(bytes[32..40].try_into().unwrap()));
            format!(
                "WAVE_FORMAT_EXTENSIBLE {} {rate}Hz {channels} channels {bits} bit",
                names::subtype(guid)
            )
        }
        2 => {
            if bytes.len() < 20 {
                return Err(RenderError::Configuration);
            }
            let samples = word(18) as i16;
            format!(
                "Microsoft ADPCM {rate} Hz {channels} channels {bits} bits per sample {samples} samples per block"
            )
        }
        _ => names::encoding(tag),
    })
}
#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_windows_render_status.rs"
    ));
}
