//! Exact Desktop process ownership and normal foreground confirmation. Merely
//! observing a process/window never establishes Voice or audio authority.
use super::{ActionResult, DesktopActionError, DesktopOwner, confirm_activation};
use crate::media::{desktop_image, image_equal};
use ivy_host_runtime::platform::windows_process::{self, ProcessIdentity};
use std::{
    io,
    path::{Path, PathBuf},
};

const FILETIME_TICKS: u64 = 504_911_232_000_000_000;
type WindowCallback = unsafe extern "system" fn(isize, isize) -> i32;
#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(callback: WindowCallback, data: isize) -> i32;
    fn GetWindowThreadProcessId(window: isize, pid: *mut u32) -> u32;
    fn GetWindow(window: isize, command: u32) -> isize;
    fn IsWindowVisible(window: isize) -> i32;
    fn GetForegroundWindow() -> isize;
    fn IsIconic(window: isize) -> i32;
    fn ShowWindowAsync(window: isize, command: i32) -> i32;
    fn SetForegroundWindow(window: isize) -> i32;
    fn SendMessageTimeoutW(
        window: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
        flags: u32,
        milliseconds: u32,
        result: *mut usize,
    ) -> isize;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
}
#[derive(Clone, Copy)]
struct Windows {
    enumerate: unsafe extern "system" fn(WindowCallback, isize) -> i32,
    pid: unsafe extern "system" fn(isize, *mut u32) -> u32,
    owner: unsafe extern "system" fn(isize, u32) -> isize,
    visible: unsafe extern "system" fn(isize) -> i32,
}
impl Default for Windows {
    fn default() -> Self {
        Self {
            enumerate: EnumWindows,
            pid: GetWindowThreadProcessId,
            owner: GetWindow,
            visible: IsWindowVisible,
        }
    }
}
fn main_window_with(pid: u32, native: Windows) -> isize {
    struct Find {
        pid: u32,
        window: isize,
        native: Windows,
    }
    unsafe extern "system" fn visit(window: isize, data: isize) -> i32 {
        // EnumWindows calls synchronously and retains neither pointer.
        let find = unsafe { &mut *(data as *mut Find) };
        let mut pid = 0;
        unsafe { (find.native.pid)(window, &mut pid) };
        if pid == find.pid
            && unsafe { (find.native.owner)(window, 4) } == 0
            && unsafe { (find.native.visible)(window) } != 0
        {
            find.window = window;
            return 0;
        }
        1
    }
    let mut find = Find {
        pid,
        window: 0,
        native,
    };
    // Match Process.MainWindowHandle: first visible unowned top-level window,
    // including an intentional false return when its callback found a match.
    unsafe { (native.enumerate)(visit, &mut find as *mut Find as isize) };
    find.window
}
pub(super) fn session(pid: u32) -> io::Result<u32> {
    let mut value = 0;
    if unsafe { ProcessIdToSessionId(pid, &mut value) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}
pub(super) fn main_window(pid: u32) -> isize {
    main_window_with(pid, Windows::default())
}
#[derive(Clone, Copy)]
struct Native {
    process: fn(u32) -> io::Result<Option<ProcessIdentity>>,
    exited: fn(u32, u64) -> io::Result<bool>,
    session: fn(u32) -> io::Result<u32>,
    window: fn(u32) -> isize,
    foreground: fn() -> isize,
    iconic: fn(isize) -> bool,
    restore: fn(isize) -> bool,
    activate: fn(isize) -> bool,
    processed: fn(isize) -> bool,
}
impl Default for Native {
    fn default() -> Self {
        Self {
            process: windows_process::process_identity,
            exited: windows_process::process_exited,
            session,
            window: main_window,
            foreground: || unsafe { GetForegroundWindow() },
            iconic: |window| unsafe { IsIconic(window) != 0 },
            restore: |window| unsafe { ShowWindowAsync(window, 9) != 0 },
            activate: |window| unsafe { SetForegroundWindow(window) != 0 },
            processed: |window| {
                let mut result = 0;
                // WM_NULL waits for normal activation without changing UI state.
                unsafe { SendMessageTimeoutW(window, 0, 0, 0, 0x22, 200, &mut result) != 0 }
            },
        }
    }
}

/// The original PID, UTC creation ticks and full image path remain pinned.
/// Every operation reads fresh process/window state. This owner neither starts
/// an application nor attaches input queues, injects input or retries activation.
pub struct WindowsDesktopProcessOwner {
    pid: u32,
    ticks: i64,
    image: PathBuf,
    native: Native,
}
impl WindowsDesktopProcessOwner {
    pub fn new(pid: i32, start_ticks: i64, image: &Path) -> ActionResult<Self> {
        if pid <= 0 || start_ticks <= 0 {
            return Err(DesktopActionError::Argument(
                "Invalid Desktop process identity.",
            ));
        }
        let image = desktop_image(image)
            .map_err(|_| DesktopActionError::Argument("Invalid Desktop process identity."))?;
        Ok(Self {
            pid: pid as u32,
            ticks: start_ticks,
            image,
            native: Native::default(),
        })
    }
    fn current(&self) -> bool {
        let check = || -> io::Result<bool> {
            let Some(process) = (self.native.process)(self.pid)? else {
                return Ok(false);
            };
            if (self.native.exited)(self.pid, process.created_filetime)?
                || (self.native.session)(self.pid)? != (self.native.session)(std::process::id())?
                || process.created_filetime.checked_add(FILETIME_TICKS) != Some(self.ticks as u64)
                || (self.native.window)(self.pid) == 0
            {
                return Ok(false);
            }
            Ok(image_equal(
                &std::path::absolute(&process.image)?,
                &self.image,
            ))
        };
        check().unwrap_or(false)
    }
    fn focused(&self) -> bool {
        self.current() && (self.native.foreground)() == (self.native.window)(self.pid)
    }
}
impl DesktopOwner for WindowsDesktopProcessOwner {
    fn is_current(&mut self) -> ActionResult<bool> {
        Ok(self.current())
    }
    fn is_focused(&mut self) -> ActionResult<bool> {
        Ok(self.focused())
    }
    fn try_focus(&mut self) -> ActionResult<bool> {
        if !self.current() {
            return Ok(false);
        }
        let window = (self.native.window)(self.pid);
        confirm_activation(
            || Ok(self.current()),
            || Ok((self.native.foreground)() == window && self.focused()),
            || Ok(!(self.native.iconic)(window) || (self.native.restore)(window)),
            || Ok((self.native.activate)(window)),
            || Ok((self.native.processed)(window)),
        )
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_desktop_process.rs"]
mod tests;
