//! Phone native capacity policy, separate from durable journal reservations.
//!
//! Both peers use the schema's ControlMethod set. Cleanup receipt reservations
//! include effects only; heartbeat/status do not consume effect receipts.

pub const FRAME_BYTES: usize = 65_536;
pub const ORDINARY_SLOTS: usize = 32;
pub const CONTROL_SLOTS: usize = 48;
pub const MAX_RECEIPTS: usize = 4096;
pub const LATE_READ_SLOTS: usize = 64;
pub const LATE_EFFECT_SLOTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    Oversized,
    Empty,
    InvalidUtf8,
    InvalidRequest,
    Truncated,
    Closed,
}

/// Incremental inherited-stdio framing. A failed stream is terminal, so a later
/// valid frame cannot revive the old native epoch. The callback parses/validates
/// the complete JSON envelope before dispatch; no partial frame reaches it.
pub struct FrameReader {
    frame: Box<[u8]>,
    length: usize,
    closed: bool,
}

impl Default for FrameReader {
    fn default() -> Self {
        Self {
            frame: vec![0; FRAME_BYTES].into_boxed_slice(),
            length: 0,
            closed: false,
        }
    }
}

impl FrameReader {
    pub fn feed(
        &mut self,
        input: &[u8],
        mut accept: impl FnMut(&str) -> Result<(), FrameError>,
    ) -> Result<(), FrameError> {
        if self.closed {
            return Err(FrameError::Closed);
        }
        for byte in input {
            if *byte == b'\n' {
                let result = if self.length == 0 {
                    Err(FrameError::Empty)
                } else {
                    std::str::from_utf8(&self.frame[..self.length])
                        .map_err(|_| FrameError::InvalidUtf8)
                        .and_then(&mut accept)
                };
                self.frame[..self.length].fill(0);
                self.length = 0;
                if let Err(error) = result {
                    self.closed = true;
                    return Err(error);
                }
            } else {
                if self.length == FRAME_BYTES {
                    self.close();
                    return Err(FrameError::Oversized);
                }
                self.frame[self.length] = *byte;
                self.length += 1;
            }
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), FrameError> {
        if self.closed {
            return Err(FrameError::Closed);
        }
        let partial = self.length != 0;
        self.close();
        if partial {
            Err(FrameError::Truncated)
        } else {
            Ok(())
        }
    }

    fn close(&mut self) {
        self.frame[..self.length].fill(0);
        self.length = 0;
        self.closed = true;
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn is_control(method: &str) -> bool {
    matches!(
        method,
        "heartbeat"
            | "status"
            | "call.hangup"
            | "call.desktop.stopVoice"
            | "call.release"
            | "shutdown"
            | "call.realtime.stop"
    )
}

pub fn is_cleanup(method: &str) -> bool {
    matches!(
        method,
        "call.hangup"
            | "call.release"
            | "call.desktop.stopVoice"
            | "call.realtime.stop"
            | "shutdown"
    )
}

pub fn is_read_only(method: &str) -> bool {
    matches!(
        method,
        "audio.probe"
            | "codec.test"
            | "inventory"
            | "heartbeat"
            | "status"
            | "desktop.observe"
            | "desktop.capture"
            | "desktop.captureOwner"
            | "desktop.process"
            | "desktop.controls"
    )
}

pub fn has_pipe_capacity(method: &str, active: usize) -> bool {
    active
        < if is_control(method) {
            CONTROL_SLOTS
        } else {
            ORDINARY_SLOTS
        }
}

/// Preserve NativeRpc's per-call cleanup reservation, independently of pipe slots.
pub fn has_receipt_capacity(
    method: &str,
    retained: usize,
    capacity: usize,
    cleanup_slots: usize,
) -> bool {
    if !(1..=MAX_RECEIPTS).contains(&capacity) {
        return false;
    }
    let required = cleanup_slots.min(128);
    let reserve = required
        .saturating_sub(if is_cleanup(method) { 4 } else { 0 })
        .min(capacity - 1);
    retained < capacity - reserve
}
