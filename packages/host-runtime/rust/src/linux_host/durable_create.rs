//! One creation sequence for native handles and deterministic boundary tests.
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

pub(super) trait Handle {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn sync_all(&self) -> io::Result<()>;
    fn close(self) -> io::Result<()>;
}
pub(super) trait Files {
    type Handle: Handle;
    fn create(&self, path: &Path, mode: u32) -> io::Result<Self::Handle>;
    fn directory(&self, path: &Path) -> io::Result<Self::Handle>;
}
pub(super) fn create(
    files: &impl Files,
    path: &Path,
    bytes: &[u8],
    mode: u32,
    parent: &Path,
) -> io::Result<()> {
    let mut file = files.create(path, mode)?;
    let written = file.write_all(bytes).and_then(|_| file.sync_all());
    // Match finally: close always runs and its error replaces an earlier error.
    file.close()?;
    written?;
    let directory = files.directory(parent)?;
    let synced = directory.sync_all();
    directory.close()?;
    synced
}

pub(super) struct NativeFiles;
impl Files for NativeFiles {
    type Handle = fs::File;
    fn create(&self, path: &Path, mode: u32) -> io::Result<fs::File> {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        options.open(path)
    }
    fn directory(&self, path: &Path) -> io::Result<fs::File> {
        fs::File::open(path)
    }
}
impl Handle for fs::File {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        Write::write_all(self, bytes)
    }
    fn sync_all(&self) -> io::Result<()> {
        fs::File::sync_all(self)
    }
    fn close(self) -> io::Result<()> {
        // Consume once. In particular an interrupted Unix close is never retried
        // against a descriptor which the kernel may already have reused.
        #[cfg(unix)]
        {
            use std::os::fd::IntoRawFd;
            if unsafe { libc::close(self.into_raw_fd()) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::IntoRawHandle;
            if unsafe { windows_sys::Win32::Foundation::CloseHandle(self.into_raw_handle()) } != 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }
}
