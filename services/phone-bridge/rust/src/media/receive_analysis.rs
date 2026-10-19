//! Private DTMF analysis sink; decoded PCM never reaches a device or route.
use super::{AudioFormat, CodecError, EvsLibrary, MediaCodec, PcmAudioPort, PcmAudioState};
use crate::dtmf_detector::{Diagnostics, InBandDetector};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct AnalysisState { detector: InBandDetector, digits: Vec<char>, failed: bool }
#[derive(Default)]
struct AnalysisPort(Mutex<AnalysisState>);
impl PcmAudioPort for AnalysisPort {
    fn state(&self) -> PcmAudioState { PcmAudioState { open: true, generation: 0 } }
    fn read_captured(&self, output: &mut [f32], _: u64) -> usize { output.fill(0.0); 0 }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        if generation != 0 { return false; }
        let mut pcm: Vec<i16> = input.iter().map(|v| (v * 32767.0).clamp(-32768.0, 32767.0) as i16).collect();
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let result = state.detector.process(&pcm, 48000);
        pcm.fill(0);
        match result {
            Ok(detections) => state.digits.extend(detections.into_iter().map(|v| v.digit)),
            Err(_) => state.failed = true,
        }
        true
    }
}
#[derive(Default)]
pub(super) struct ReceiveAnalysis { port: Arc<AnalysisPort>, decoder: Option<MediaCodec> }
impl ReceiveAnalysis {
    pub fn receive(&mut self, format: &AudioFormat, payload: &[u8], evs: Option<Arc<EvsLibrary>>) -> Result<Vec<char>, CodecError> {
        if self.decoder.as_ref().is_none_or(|decoder| decoder.format() != format) {
            self.decoder = Some(MediaCodec::new(format.clone(), self.port.clone(), evs)?);
        }
        self.decoder.as_mut().unwrap().decode(payload, None)?;
        let mut state = self.port.0.lock().unwrap_or_else(|p| p.into_inner());
        if state.failed { return Err(CodecError::Failed); }
        Ok(std::mem::take(&mut state.digits))
    }
    pub fn diagnostics(&self) -> Diagnostics {
        self.port.0.lock().unwrap_or_else(|p| p.into_inner()).detector.diagnostics()
    }
}
