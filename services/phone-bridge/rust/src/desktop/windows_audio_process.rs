//! Read-only process pins survive every ancestry and capture recheck. They do
//! not establish ownership of a previously observed stream or grant audio.
use super::{
    ActionResult, AudioProcessOwnership, AudioProcessRecord, AudioProcessSnapshot,
    DesktopActionError, DesktopCaptureCorrelation, DesktopOwner, WindowsCaptureEndpoint,
    WindowsDesktopProcessOwner,
};
use crate::media::{AudioDesktopIdentity, image_equal};
use ivy_contracts::json::JsString;
use ivy_host_runtime::platform::{windows_process, windows_process_inspection::ProcessObservation};
use std::{cell::RefCell, collections::HashMap, io, path::PathBuf};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, WaitForSingleObject},
};

const FILETIME_TICKS: u64 = 504_911_232_000_000_000;
const MAX_DATE_TICKS: u64 = 3_155_378_975_999_999_999;
const HANDLE_LIMIT: usize = 4096;
trait ProcessMetadata {
    fn module(&self) -> io::Result<Option<PathBuf>>;
    fn session(&mut self) -> io::Result<u32>;
    fn created(&mut self) -> io::Result<u64>;
    fn exited(&mut self) -> io::Result<bool>;
}
impl ProcessMetadata for ProcessObservation {
    fn module(&self) -> io::Result<Option<PathBuf>> {
        self.main_module_file_name()
    }
    fn session(&mut self) -> io::Result<u32> {
        self.session_id()
    }
    fn created(&mut self) -> io::Result<u64> {
        self.created_filetime()
    }
    fn exited(&mut self) -> io::Result<bool> {
        self.has_exited()
    }
}
#[derive(Clone, Copy)]
struct Native {
    open: unsafe extern "system" fn(u32, i32, u32) -> HANDLE,
    wait: unsafe extern "system" fn(HANDLE, u32) -> u32,
    close: unsafe extern "system" fn(HANDLE) -> i32,
    parent: fn(u32, usize) -> io::Result<Option<u32>>,
    process: fn(u32) -> io::Result<Option<Box<dyn ProcessMetadata>>>,
}
impl Default for Native {
    fn default() -> Self {
        Self {
            open: OpenProcess,
            wait: WaitForSingleObject,
            close: CloseHandle,
            parent: windows_process::process_parent_id,
            process: |pid| {
                ProcessObservation::by_id(pid)
                    .map(|value| value.map(|v| Box::new(v) as Box<dyn ProcessMetadata>))
            },
        }
    }
}
pub struct WindowsAudioProcessSnapshot {
    native: Native,
    handles: HashMap<i32, HANDLE>,
    order: Vec<i32>,
    closed: bool,
}
impl Default for WindowsAudioProcessSnapshot {
    fn default() -> Self {
        Self {
            native: Native::default(),
            handles: HashMap::new(),
            order: vec![],
            closed: false,
        }
    }
}
impl WindowsAudioProcessSnapshot {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn observe(root: &AudioDesktopIdentity, pid: i32) -> ActionResult<AudioProcessOwnership> {
        let owner = RefCell::new(WindowsDesktopProcessOwner::new(
            root.pid(),
            root.creation_ticks(),
            root.image(),
        )?);
        AudioProcessOwnership::read(
            root,
            pid,
            &|| owner.borrow_mut().is_current(),
            || Ok(Some(Self::new())),
            &image_equal,
        )
    }
    pub fn capture_owner(
        root: &AudioDesktopIdentity,
        endpoint: &JsString,
    ) -> ActionResult<DesktopCaptureCorrelation> {
        let owner = RefCell::new(WindowsDesktopProcessOwner::new(
            root.pid(),
            root.creation_ticks(),
            root.image(),
        )?);
        DesktopCaptureCorrelation::read(
            root,
            endpoint,
            &|| owner.borrow_mut().is_current(),
            |id| WindowsCaptureEndpoint::observe(id).map(Some),
            || Ok(Some(Self::new())),
            &image_equal,
        )
    }
}
impl AudioProcessSnapshot for WindowsAudioProcessSnapshot {
    fn read(&mut self, pid: i32) -> ActionResult<Option<AudioProcessRecord>> {
        if self.closed {
            return Err(DesktopActionError::Unavailable);
        }
        if pid <= 0 {
            return Err(DesktopActionError::Argument(
                "Positive process ID required.",
            ));
        }
        let handle = if let Some(handle) = self.handles.get(&pid) {
            *handle
        } else {
            if self.handles.len() >= HANDLE_LIMIT {
                return Err(DesktopActionError::Unavailable);
            }
            let handle = unsafe { (self.native.open)(0x00101000, 0, pid as u32) };
            if handle.is_null() {
                return Ok(None);
            }
            self.handles.insert(pid, handle);
            self.order.push(pid);
            handle
        };
        if unsafe { (self.native.wait)(handle, 0) } != WAIT_TIMEOUT {
            return Ok(None);
        }
        let observe = || -> io::Result<Option<AudioProcessRecord>> {
            // A fresh original process-object lifetime for each Read. Its lazy
            // metadata remains ordered; only the separate pin survives reads.
            let Some(mut process) = (self.native.process)(pid as u32)? else {
                return Ok(None);
            };
            let parent = (self.native.parent)(pid as u32, 65536)?
                .ok_or_else(|| io::Error::other("Process missing from inventory."))?;
            let parent_pid = i32::try_from(parent)
                .map_err(|_| io::Error::other("Parent process ID exceeds Int32."))?;
            let Some(image_path) = process.module()? else {
                return Ok(None);
            };
            let session_id = process.session()? as i32;
            let ticks = process
                .created()?
                .checked_add(FILETIME_TICKS)
                .filter(|v| *v <= MAX_DATE_TICKS)
                .ok_or_else(|| io::Error::other("Process creation time unavailable."))?;
            let record = AudioProcessRecord {
                pid,
                parent_pid,
                session_id,
                start_ticks: ticks as i64,
                image_path: Some(image_path),
            };
            Ok(
                if process.exited()? || unsafe { (self.native.wait)(handle, 0) } != WAIT_TIMEOUT {
                    None
                } else {
                    Some(record)
                },
            )
        };
        Ok(observe().unwrap_or(None))
    }
    fn close(&mut self) -> ActionResult<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let mut failed = false;
        for pid in self.order.drain(..) {
            if unsafe { (self.native.close)(self.handles.remove(&pid).unwrap()) } == 0 {
                failed = true;
            }
        }
        if failed {
            Err(DesktopActionError::Unavailable)
        } else {
            Ok(())
        }
    }
}
impl Drop for WindowsAudioProcessSnapshot {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_audio_process_native.rs"]
pub(crate) mod tests;
