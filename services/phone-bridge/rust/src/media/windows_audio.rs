//! The Windows route's mono queues and authority transitions. Its native owner
//! serializes every method with capture/render callbacks under the same lock.
use super::{AudioQueue, AudioSettings, CallAudioError, SAMPLE_RATE};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

pub(crate) mod render;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Suspended,
    Open,
    Failed,
    Closed,
}

pub(crate) struct BufferSnapshot {
    pub state: &'static str,
    pub generation: i64,
    pub capture_dropped: u64,
    pub render_dropped: u64,
    pub capture_queued: usize,
    pub render_queued: usize,
}

/// Device construction never grants this core permission to transfer samples.
/// Closing here only retires PCM; the native owner must separately join devices.
pub(crate) struct WindowsAudioBuffers {
    state: State,
    generation: i64,
    permitted: Arc<dyn Fn() -> bool + Send + Sync>,
    captured: AudioQueue,
    rendered: AudioQueue,
}

impl WindowsAudioBuffers {
    pub fn new(
        settings: &AudioSettings,
        permitted: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Self, CallAudioError> {
        settings.validate()?;
        let capacity = SAMPLE_RATE * usize::from(settings.queue_ms) / 1000;
        Ok(Self {
            state: State::Suspended,
            generation: 0,
            permitted,
            captured: AudioQueue::new(capacity, 0, false, false)?,
            rendered: AudioQueue::new(
                capacity,
                SAMPLE_RATE * usize::from(settings.playback_prebuffer_ms) / 1000,
                false,
                false,
            )?,
        })
    }
    pub fn snapshot(&self) -> BufferSnapshot {
        BufferSnapshot {
            state: match self.state {
                State::Suspended => "suspended",
                State::Open => "open",
                State::Failed => "failed",
                State::Closed => "closed",
            },
            generation: self.generation,
            capture_dropped: self.captured.dropped(),
            render_dropped: self.rendered.dropped(),
            capture_queued: self.captured.count(),
            render_queued: self.rendered.count(),
        }
    }
    fn allowed(&self) -> bool {
        catch_unwind(AssertUnwindSafe(|| (self.permitted)())).unwrap_or(false)
    }
    fn clear(&mut self) {
        self.captured.clear();
        self.rendered.clear();
    }
    pub fn is_open(&mut self) -> bool {
        if self.state != State::Open {
            return false;
        }
        if self.allowed() {
            return true;
        }
        self.state = State::Suspended;
        self.clear();
        false
    }
    pub fn resume(&mut self) -> Result<(), CallAudioError> {
        if matches!(self.state, State::Closed | State::Failed) {
            return Err(CallAudioError::RouteFailed);
        }
        if !self.allowed() {
            return Err(CallAudioError::OwnerChanged);
        }
        if self.state == State::Open {
            return Ok(());
        }
        self.clear();
        self.generation = self.generation.wrapping_add(1);
        self.state = State::Open;
        Ok(())
    }
    pub fn suspend(&mut self) {
        if self.state == State::Open {
            self.state = State::Suspended;
        }
        self.clear();
    }
    pub fn fail(&mut self) {
        if self.state != State::Closed {
            self.state = State::Failed;
        }
        self.clear();
    }
    pub fn close(&mut self) {
        self.state = State::Closed;
        self.clear();
    }
    /// The native packet lease supplies a valid bounded byte span. Silent data
    /// is counted without decoding its bytes; other capture flags are carriers.
    pub fn captured(&mut self, bytes: &[u8], flags: u32) {
        if !self.is_open() {
            return;
        }
        if !bytes.len().is_multiple_of(8) {
            self.fail();
            return;
        }
        let mut mono = [0.0; 512];
        for chunk in bytes.chunks(mono.len() * 8) {
            let count = chunk.len() / 8;
            for (index, frame) in chunk.chunks_exact(8).enumerate() {
                let value = if flags & 2 != 0 {
                    0.0
                } else {
                    let left = f32::from_le_bytes(frame[..4].try_into().unwrap());
                    let right = f32::from_le_bytes(frame[4..].try_into().unwrap());
                    (left + right) * 0.5
                };
                if !value.is_finite() {
                    self.fail();
                    return;
                }
                mono[index] = value.clamp(-1.0, 1.0);
            }
            self.captured.write(&mono[..count]);
        }
    }
    pub fn read_captured(&mut self, output: &mut [f32]) -> usize {
        if !self.is_open() {
            output.fill(0.0);
            return 0;
        }
        self.captured.read(output)
    }
    pub fn write_received(&mut self, input: &[f32]) {
        if !self.is_open() {
            return;
        }
        if input
            .iter()
            .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
        {
            self.fail();
            return;
        }
        self.rendered.write(input);
    }
    pub fn render(&mut self, output: &mut [f32]) -> usize {
        if self.is_open() {
            self.rendered.read(output);
        } else {
            output.fill(0.0);
        }
        output.len()
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_windows_audio.rs"
    ));
}
