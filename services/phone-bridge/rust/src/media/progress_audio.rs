//! Local call-progress PCM. Normal capture keeps its separate call permit.
use super::announcement::{
    AnnouncementCleanup, AnnouncementPlayback, Pcm16Wave, WavError, load_announcement,
};
use super::call_audio::CallAudioContext;
use super::{
    AudioSendHealth, CallAudioError, CallAudioPort, CallAudioRoute, PcmAudioPort, PcmAudioState,
    PcmSendPort,
};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

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
    samples: Pcm16Wave<'static>,
    state: Mutex<Waiting>,
}
impl CallWaitingAudio {
    pub fn new(normal: CallAudioPort) -> Result<Self, WavError> {
        static SAMPLES: OnceLock<Result<Pcm16Wave<'static>, WavError>> = OnceLock::new();
        let samples = SAMPLES
            .get_or_init(|| {
                Pcm16Wave::parse(include_bytes!("../../../assets/audio/waiting-marimba.wav"))
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
                let cycle = self.samples.samples + 96000;
                for sample in output.iter_mut() {
                    let position = (waiting.sample % cycle as u64) as usize;
                    waiting.sample += 1;
                    *sample = self.samples.sample(position);
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
    samples: Option<AnnouncementPlayback>,
    offset: usize,
    remaining: usize,
    pause: usize,
    pause_remaining: usize,
    generation: u64,
    waiting: bool,
}
pub struct ScreeningAudio {
    normal: CallAudioPort,
    state: Mutex<Screening>,
    cleanup: AnnouncementCleanup,
    waiting_policy: OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
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
        let remaining = usize::from(repeats) + 1;
        let samples = AnnouncementPlayback::new(load_announcement(path, hash)?, remaining)?;
        let pause =
            (usize::from(timeout) * 48000).saturating_sub(samples.samples * remaining) / remaining;
        Ok(Self {
            normal,
            waiting_policy: OnceLock::new(),
            cleanup: samples.cleanup(),
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
    pub(crate) fn follow_waiting(&self, policy: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = self.waiting_policy.set(Box::new(policy));
    }
    fn waiting(&self, state: &Screening) -> bool {
        self.waiting_policy
            .get()
            .map_or(state.waiting, |policy| policy())
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
        state.samples.take();
        state.generation += 1;
    }
    pub(crate) async fn close(&self) {
        self.bridge();
        self.cleanup.wait().await;
    }
}
impl PcmAudioPort for ScreeningAudio {
    fn state(&self) -> PcmAudioState {
        let screen = self.state.lock().unwrap_or_else(|p| p.into_inner());
        self.normal.with_call(|context, _| {
            state(
                context,
                screen.generation,
                screen.samples.is_some() && self.waiting(&screen),
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
                screen.samples.is_some() && self.waiting(&screen),
            );
            if !admitted.open || admitted.generation != generation {
                return 0;
            }
            if screen.samples.is_none() {
                capture(context, route, output);
            } else if self.waiting(&screen) {
                let mut written = 0;
                while written < output.len() && screen.remaining > 0 {
                    if screen.pause_remaining > 0 {
                        let count = screen.pause_remaining.min(output.len() - written);
                        screen.pause_remaining -= count;
                        written += count;
                        continue;
                    }
                    let samples = screen.samples.as_ref().unwrap();
                    let count = (samples.samples - screen.offset).min(output.len() - written);
                    let count = samples.read(&mut output[written..written + count]);
                    if count == 0 {
                        break;
                    }
                    screen.offset += count;
                    written += count;
                    if screen.offset == screen.samples.as_ref().unwrap().samples {
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
                screen.samples.is_some() && self.waiting(&screen),
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
                screen.samples.is_some() && self.waiting(&screen),
            ) != admitted
            {
                return false;
            }
            guarded(context, route, action)
        })
    }
    fn send_health(&self) -> AudioSendHealth {
        let screen = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if screen
            .samples
            .as_ref()
            .is_some_and(AnnouncementPlayback::failed)
        {
            AudioSendHealth::Failed
        } else {
            self.normal.send_health()
        }
    }
}
