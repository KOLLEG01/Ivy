//! Local call-progress PCM. Normal capture keeps its separate call permit.
use super::call_audio::CallAudioContext;
use super::{
    AudioSendHealth, CallAudioError, CallAudioPort, CallAudioRoute, PcmAudioPort, PcmAudioState,
    PcmSendPort,
};
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::Read,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavError {
    Invalid,
    Truncated,
    Layout,
    Duration,
    Path,
    Hash,
    Io,
}
impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WavError {}
/// Original bounded PCM16 RIFF parser, channel mix and linear48k conversion.
/// The declared RIFF size is intentionally not a second admission authority.
pub fn parse_announcement(bytes: &[u8]) -> Result<Vec<f32>, WavError> {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::Invalid);
    }
    let mut rate = 0;
    let mut channels = 0;
    let mut data = &[][..];
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let start = offset + 8;
        let mut length = u32::from_le_bytes(bytes[offset + 4..start].try_into().unwrap()) as usize;
        if length == u32::MAX as usize && id == b"data" {
            length = bytes.len() - start;
        }
        if length > bytes.len() - start {
            return Err(WavError::Truncated);
        }
        if id == b"fmt " {
            if length < 16
                || u16::from_le_bytes(bytes[start..start + 2].try_into().unwrap()) != 1
                || u16::from_le_bytes(bytes[start + 14..start + 16].try_into().unwrap()) != 16
            {
                return Err(WavError::Layout);
            }
            channels = u16::from_le_bytes(bytes[start + 2..start + 4].try_into().unwrap()) as usize;
            rate = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap()) as usize;
        } else if id == b"data" {
            data = &bytes[start..start + length];
        }
        offset = start + length + (length & 1);
    }
    if !(1..=8).contains(&channels)
        || !(8000..=192000).contains(&rate)
        || data.is_empty()
        || data.len() % (2 * channels) != 0
    {
        return Err(WavError::Layout);
    }
    let frames = data.len() / (2 * channels);
    let samples = (frames as u64) * 48000 / (rate as u64);
    if samples == 0 || samples > 48000 * 120 {
        return Err(WavError::Duration);
    }
    let mut output = vec![0.0; samples as usize];
    for (i, sample) in output.iter_mut().enumerate() {
        let position = i as f64 * rate as f64 / 48000.0;
        let first = (position as usize).min(frames - 1);
        let second = (first + 1).min(frames - 1);
        let mut sum = 0.0;
        for channel in 0..channels {
            let a = (first * channels + channel) * 2;
            let b = (second * channels + channel) * 2;
            let a = f64::from(i16::from_le_bytes(data[a..a + 2].try_into().unwrap()));
            let b = f64::from(i16::from_le_bytes(data[b..b + 2].try_into().unwrap()));
            sum += a + (b - a) * (position - first as f64);
        }
        *sample = (sum / channels as f64 / 32768.0) as f32;
    }
    Ok(output)
}
struct Bytes(Vec<u8>);
impl Drop for Bytes {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}
pub fn load_announcement(path: &Path, hash: &str) -> Result<Vec<f32>, WavError> {
    if !path.is_absolute() {
        return Err(WavError::Path);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let mut file = options.open(path).map_err(|_| WavError::Io)?;
    let length = file.metadata().map_err(|_| WavError::Io)?.len();
    if !(44..=10_485_760).contains(&length) {
        return Err(WavError::Layout);
    }
    let mut bytes = Bytes(vec![0; length as usize]);
    file.read_exact(&mut bytes.0).map_err(|_| WavError::Io)?;
    if ivy_contracts::hash::digest(&bytes.0) != hash {
        return Err(WavError::Hash);
    }
    parse_announcement(&bytes.0)
}
fn state(context: CallAudioContext, generation: u64, local: bool) -> PcmAudioState {
    PcmAudioState {
        open: context.live && (local || context.pcm.open),
        generation: context
            .pcm
            .generation
            .checked_add(generation)
            .expect("progress generation exhausted"),
    }
}
fn guarded(
    context: CallAudioContext,
    route: Option<&Arc<dyn CallAudioRoute>>,
    action: &mut dyn FnMut(),
) -> bool {
    // Even local PCM keeps the original call and underlying route snapshot,
    // without requiring the independently suspended capture permit to open.
    if !context.live {
        return false;
    }
    if let Some(route) = route {
        route.with_send_state(context.route_state.unwrap(), action)
    } else {
        action();
        true
    }
}
fn capture(context: CallAudioContext, route: Option<&Arc<dyn CallAudioRoute>>, output: &mut [f32]) {
    output.fill(0.0);
    if !context.pcm.open {
        return;
    }
    let route = route.unwrap();
    let original = context.route_state.unwrap();
    route.read_captured(output, original.generation);
    if route.failed() || route.state() != original {
        output.fill(0.0);
    }
}
struct Waiting {
    state: u8,
    generation: u64,
    sample: u64,
    feedback: VecDeque<bool>,
    pending: usize,
    current: Option<bool>,
    position: usize,
}
pub struct CallWaitingAudio {
    normal: CallAudioPort,
    samples: Arc<Vec<f32>>,
    state: Mutex<Waiting>,
}
impl CallWaitingAudio {
    pub fn new(normal: CallAudioPort) -> Result<Self, WavError> {
        static SAMPLES: OnceLock<Result<Arc<Vec<f32>>, WavError>> = OnceLock::new();
        let samples = SAMPLES
            .get_or_init(|| {
                parse_announcement(include_bytes!(
                    "../../../native/phone-runtime/audio/waiting-marimba.wav"
                ))
                .map(Arc::new)
            })
            .clone()?;
        Ok(Self {
            normal,
            samples,
            state: Mutex::new(Waiting {
                state: 0,
                generation: 0,
                sample: 0,
                feedback: VecDeque::new(),
                pending: 0,
                current: None,
                position: 0,
            }),
        })
    }
    pub fn begin(&self) -> Result<(), CallAudioError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.state != 0 {
            return Err(CallAudioError::NotReady);
        }
        state.state = 1;
        state.generation += 1;
        Ok(())
    }
    pub fn end(&self) -> Result<(), CallAudioError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.state != 1 {
            return Err(CallAudioError::NotReady);
        }
        state.state = 2;
        state.generation += 1;
        Ok(())
    }
    pub fn acknowledge(&self, success: bool) -> Result<(), CallAudioError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.feedback.len() >= 32 {
            return Err(CallAudioError::NotReady);
        }
        if state.pending == 0 {
            state.generation += 1;
        }
        state.feedback.push_back(success);
        state.pending += 1;
        Ok(())
    }
}
impl PcmAudioPort for CallWaitingAudio {
    fn state(&self) -> PcmAudioState {
        let waiting = self.state.lock().unwrap_or_else(|p| p.into_inner());
        self.normal.with_call(|context, _| {
            state(
                context,
                waiting.generation,
                waiting.state == 1 || waiting.pending > 0,
            )
        })
    }
    fn read_captured(&self, output: &mut [f32], generation: u64) -> usize {
        output.fill(0.0);
        let mut waiting = self.state.lock().unwrap_or_else(|p| p.into_inner());
        self.normal.with_call(|context, route| {
            let admitted = state(
                context,
                waiting.generation,
                waiting.state == 1 || waiting.pending > 0,
            );
            if !admitted.open || admitted.generation != generation {
                return 0;
            }
            if waiting.state == 1 {
                let cycle = self.samples.len() + 96000;
                for sample in output.iter_mut() {
                    let position = (waiting.sample % cycle as u64) as usize;
                    waiting.sample += 1;
                    *sample = self.samples.get(position).copied().unwrap_or(0.0);
                }
            } else {
                capture(context, route, output);
                for sample in output.iter_mut() {
                    if waiting.pending == 0 {
                        break;
                    }
                    if waiting.current.is_none() {
                        waiting.current = waiting.feedback.pop_front();
                        waiting.position = 0;
                    }
                    let position = waiting.position;
                    waiting.position += 1;
                    let success = waiting.current.unwrap();
                    let duration = if success { 7680 } else { 20160 };
                    let sounding = if success {
                        position < 5760
                    } else {
                        position < 5760 || (9600..15360).contains(&position)
                    };
                    if sounding {
                        let within = position % 9600;
                        let envelope =
                            (within.min(5760usize.saturating_sub(within)) as f64 / 480.0).min(1.0);
                        *sample = (*sample
                            + (0.1
                                * envelope
                                * (std::f64::consts::TAU
                                    * if success { 880.0 } else { 330.0 }
                                    * position as f64
                                    / 48000.0)
                                    .sin()) as f32)
                            .clamp(-1.0, 1.0);
                    }
                    if waiting.position >= duration {
                        waiting.current = None;
                        waiting.pending -= 1;
                    }
                }
            }
            if self.normal.revoked() {
                output.fill(0.0);
                0
            } else {
                output.len()
            }
        })
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        let waiting = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if waiting.state == 1 {
            return false;
        }
        self.normal.with_call(|context, route| {
            let pcm = state(context, waiting.generation, waiting.pending > 0);
            if !pcm.open || pcm.generation != generation || !context.pcm.open {
                return false;
            }
            route
                .unwrap()
                .write_received(input, context.route_state.unwrap().generation)
        })
    }
}
impl PcmSendPort for CallWaitingAudio {
    fn with_send_state(&self, admitted: PcmAudioState, action: &mut dyn FnMut()) -> bool {
        let waiting = self.state.lock().unwrap_or_else(|p| p.into_inner());
        self.normal.with_call(|context, route| {
            if state(
                context,
                waiting.generation,
                waiting.state == 1 || waiting.pending > 0,
            ) != admitted
            {
                return false;
            }
            guarded(context, route, action)
        })
    }
    fn send_health(&self) -> AudioSendHealth {
        self.normal.send_health()
    }
}
struct Screening {
    samples: Option<Vec<f32>>,
    offset: usize,
    remaining: usize,
    pause: usize,
    pause_remaining: usize,
    generation: u64,
    waiting: bool,
}
impl Drop for Screening {
    fn drop(&mut self) {
        if let Some(samples) = &mut self.samples {
            samples.fill(0.0);
        }
    }
}
pub struct ScreeningAudio {
    normal: CallAudioPort,
    state: Mutex<Screening>,
}
impl ScreeningAudio {
    pub fn new(
        normal: CallAudioPort,
        path: &Path,
        hash: &str,
        timeout: u16,
        repeats: u8,
        waiting: bool,
    ) -> Result<Self, WavError> {
        if !(1..=120).contains(&timeout) || repeats > 3 {
            return Err(WavError::Layout);
        }
        let samples = load_announcement(path, hash)?;
        let remaining = usize::from(repeats) + 1;
        let pause =
            (usize::from(timeout) * 48000).saturating_sub(samples.len() * remaining) / remaining;
        Ok(Self {
            normal,
            state: Mutex::new(Screening {
                samples: Some(samples),
                offset: 0,
                remaining,
                pause,
                pause_remaining: 0,
                generation: 1,
                waiting,
            }),
        })
    }
    pub fn set_waiting(&self, waiting: bool) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.waiting != waiting {
            state.waiting = waiting;
            state.generation += 1;
        }
    }
    pub fn bridge(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(mut samples) = state.samples.take() {
            samples.fill(0.0);
        }
        state.generation += 1;
    }
}
impl PcmAudioPort for ScreeningAudio {
    fn state(&self) -> PcmAudioState {
        let screen = self.state.lock().unwrap_or_else(|p| p.into_inner());
        self.normal.with_call(|context, _| {
            state(
                context,
                screen.generation,
                screen.samples.is_some() && screen.waiting,
            )
        })
    }
    fn read_captured(&self, output: &mut [f32], generation: u64) -> usize {
        output.fill(0.0);
        let mut screen = self.state.lock().unwrap_or_else(|p| p.into_inner());
        self.normal.with_call(|context, route| {
            let admitted = state(
                context,
                screen.generation,
                screen.samples.is_some() && screen.waiting,
            );
            if !admitted.open || admitted.generation != generation {
                return 0;
            }
            if screen.samples.is_none() {
                capture(context, route, output);
            } else if screen.waiting {
                for sample in output.iter_mut() {
                    if screen.remaining == 0 {
                        break;
                    }
                    if screen.pause_remaining > 0 {
                        screen.pause_remaining -= 1;
                        continue;
                    }
                    *sample = screen.samples.as_ref().unwrap()[screen.offset];
                    screen.offset += 1;
                    if screen.offset == screen.samples.as_ref().unwrap().len() {
                        screen.offset = 0;
                        screen.remaining -= 1;
                        screen.pause_remaining = screen.pause;
                    }
                }
            }
            if self.normal.revoked() {
                output.fill(0.0);
                0
            } else {
                output.len()
            }
        })
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        let screen = self.state.lock().unwrap_or_else(|p| p.into_inner());
        self.normal.with_call(|context, route| {
            if state(
                context,
                screen.generation,
                screen.samples.is_some() && screen.waiting,
            )
            .generation
                != generation
                || !context.live
                || !context.pcm.open
            {
                return false;
            }
            route
                .unwrap()
                .write_received(input, context.route_state.unwrap().generation)
        })
    }
}
impl PcmSendPort for ScreeningAudio {
    fn with_send_state(&self, admitted: PcmAudioState, action: &mut dyn FnMut()) -> bool {
        let screen = self.state.lock().unwrap_or_else(|p| p.into_inner());
        self.normal.with_call(|context, route| {
            if state(
                context,
                screen.generation,
                screen.samples.is_some() && screen.waiting,
            ) != admitted
            {
                return false;
            }
            guarded(context, route, action)
        })
    }
    fn send_health(&self) -> AudioSendHealth {
        self.normal.send_health()
    }
}
