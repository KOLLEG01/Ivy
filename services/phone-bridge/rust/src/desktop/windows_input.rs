//! Windows keyboard ABI only; policy and original-owner authority stay in actions.
use super::{ActionResult, DesktopActionError, KeyStroke, KeyboardInput};

#[repr(C)]
#[derive(Clone, Copy)]
struct Input {
    kind: u32,
    _padding: u32,
    key: u16,
    scan: u16,
    flags: u32,
    time: u32,
    _padding2: u32,
    extra: usize,
    _union_tail: u64,
}
type SendInputFn = unsafe extern "system" fn(u32, *const Input, i32) -> u32;
type KeyStateFn = unsafe extern "system" fn(i32) -> i16;
#[link(name = "user32")]
unsafe extern "system" {
    fn SendInput(count: u32, inputs: *const Input, size: i32) -> u32;
    fn GetAsyncKeyState(key: i32) -> i16;
}
pub struct WindowsKeyboardInput {
    send: SendInputFn,
    key_state: KeyStateFn,
}
impl WindowsKeyboardInput {
    pub fn new() -> Self {
        Self {
            send: SendInput,
            key_state: GetAsyncKeyState,
        }
    }
}
impl Default for WindowsKeyboardInput {
    fn default() -> Self {
        Self::new()
    }
}
impl KeyboardInput for WindowsKeyboardInput {
    fn is_idle(&mut self) -> ActionResult<bool> {
        for key in 1..255 {
            if unsafe { (self.key_state)(key) } & i16::MIN != 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn send(&mut self, keys: &[KeyStroke]) -> ActionResult<i32> {
        let inputs: Vec<Input> = keys
            .iter()
            .map(|key| Input {
                kind: 1,
                _padding: 0,
                key: key.key,
                scan: 0,
                flags: if key.up { 2 } else { 0 } | if key.key == 0x5b { 1 } else { 0 },
                time: 0,
                _padding2: 0,
                extra: 0,
                _union_tail: 0,
            })
            .collect();
        let count = u32::try_from(inputs.len()).map_err(|_| DesktopActionError::Unavailable)?;
        let sent =
            unsafe { (self.send)(count, inputs.as_ptr(), std::mem::size_of::<Input>() as i32) };
        i32::try_from(sent).map_err(|_| DesktopActionError::Unavailable)
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_desktop_actions.rs"]
mod tests;
