//! Native media DTOs deserialize before call guards; routing/WAV validation
//! follows those guards. Keep Int32 token spelling and nullable/default fields.
use crate::{
    media::{AudioMutePolicy, AudioSettings},
    native_config::NativeConfigError,
    native_sip::NativeSipError,
};
use ivy_contracts::IvyJson;
use std::path::PathBuf;

fn invalid() -> NativeSipError {
    NativeConfigError::InvalidRequest.into()
}
fn string(value: &IvyJson) -> Result<String, NativeSipError> {
    if value.is_null() {
        Ok(String::new())
    } else {
        value.as_str().map_err(|_| invalid())
    }
}
fn optional_string(value: &IvyJson) -> Result<Option<String>, NativeSipError> {
    if value.is_null() {
        Ok(None)
    } else {
        value.as_str().map(Some).map_err(|_| invalid())
    }
}
fn integer(raw: &str) -> Result<i32, NativeSipError> {
    raw.parse().map_err(|_| invalid())
}
fn boolean(value: &IvyJson) -> Result<bool, NativeSipError> {
    value.as_bool().ok_or_else(invalid)
}

pub(crate) struct NativeScreeningSettings {
    pub path: PathBuf,
    pub hash: String,
    timeout: i32,
    repeats: i32,
}
impl NativeScreeningSettings {
    pub fn parse(value: &IvyJson, raw: &str) -> Result<Option<Self>, NativeSipError> {
        if value.is_null() {
            return Ok(None);
        }
        let fields = value.as_object().ok_or_else(invalid)?;
        let mut settings = Self {
            path: PathBuf::new(),
            hash: String::new(),
            timeout: 30,
            repeats: 1,
        };
        for ((key, value), raw) in fields.iter().zip(crate::native_rpc::root_values(raw)) {
            match key.to_utf8().map_err(|_| invalid())?.as_str() {
                "wavPath" => settings.path = string(value)?.into(),
                "sha256" => settings.hash = string(value)?,
                "timeoutSeconds" => settings.timeout = integer(raw)?,
                "repeatCount" => settings.repeats = integer(raw)?,
                _ => return Err(invalid()),
            }
        }
        Ok(Some(settings))
    }
    pub fn bounds(&self) -> Result<(u16, u8), NativeSipError> {
        if !(1..=120).contains(&self.timeout) || !(0..=3).contains(&self.repeats) {
            return Err(invalid());
        }
        Ok((self.timeout as u16, self.repeats as u8))
    }
}

pub(crate) struct NativeAudioSettings {
    capture: String,
    render: String,
    source: String,
    capture_ms: i32,
    render_ms: i32,
    queue_ms: i32,
    mute: Option<(Option<String>, bool, i32)>,
    prebuffer_ms: i32,
    low_latency: bool,
    loopback: Option<String>,
}
impl NativeAudioSettings {
    pub fn parse(value: &IvyJson, raw: &str) -> Result<Option<Self>, NativeSipError> {
        if value.is_null() {
            return Ok(None);
        }
        let fields = value.as_object().ok_or_else(invalid)?;
        let mut settings = Self {
            capture: String::new(),
            render: String::new(),
            source: String::new(),
            capture_ms: 0,
            render_ms: 0,
            queue_ms: 0,
            mute: None,
            prebuffer_ms: 20,
            low_latency: true,
            loopback: None,
        };
        for ((key, value), raw) in fields.iter().zip(crate::native_rpc::root_values(raw)) {
            match key.to_utf8().map_err(|_| invalid())?.as_str() {
                "captureEndpointId" => settings.capture = string(value)?,
                "renderEndpointId" => settings.render = string(value)?,
                "sourceMode" => settings.source = string(value)?,
                "captureBufferMs" => settings.capture_ms = integer(raw)?,
                "renderLatencyMs" => settings.render_ms = integer(raw)?,
                "queueMs" => settings.queue_ms = integer(raw)?,
                "playbackPrebufferMs" => settings.prebuffer_ms = integer(raw)?,
                "enableWasapiLowLatency" => settings.low_latency = boolean(value)?,
                "loopbackRenderEndpointId" => settings.loopback = optional_string(value)?,
                "mutePolicy" => {
                    if value.is_null() {
                        continue;
                    }
                    let mut mute = (None, false, 60);
                    for ((key, value), raw) in value
                        .as_object()
                        .ok_or_else(invalid)?
                        .iter()
                        .zip(crate::native_rpc::root_values(raw))
                    {
                        match key.to_utf8().map_err(|_| invalid())?.as_str() {
                            "mutedRenderEndpointId" => mute.0 = optional_string(value)?,
                            "unmuteReceive" => mute.1 = boolean(value)?,
                            "intervalSeconds" => mute.2 = integer(raw)?,
                            _ => return Err(invalid()),
                        }
                    }
                    settings.mute = Some(mute);
                }
                _ => return Err(invalid()),
            }
        }
        Ok(Some(settings))
    }
    pub fn validated(self) -> Result<AudioSettings, NativeSipError> {
        let bounded = |value: i32| u16::try_from(value).map_err(|_| invalid());
        let mute = if let Some((muted_render_endpoint, unmute_receive, interval)) = self.mute {
            Some(AudioMutePolicy {
                muted_render_endpoint,
                unmute_receive,
                interval_seconds: bounded(interval)?,
            })
        } else {
            None
        };
        let settings = AudioSettings {
            capture_endpoint: self.capture,
            render_endpoint: self.render,
            source_mode: self.source,
            capture_buffer_ms: bounded(self.capture_ms)?,
            render_latency_ms: bounded(self.render_ms)?,
            queue_ms: bounded(self.queue_ms)?,
            mute,
            playback_prebuffer_ms: bounded(self.prebuffer_ms)?,
            enable_wasapi_low_latency: self.low_latency,
            loopback_render_endpoint: self.loopback,
        };
        settings.validate().map_err(|_| invalid())?;
        Ok(settings)
    }
}
