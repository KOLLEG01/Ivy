use crate::{IvyError, IvyResult};
use std::{ffi::{CStr, c_void}, path::Path};

pub(crate) fn unavailable() -> IvyError {
    IvyError::new("locale_unavailable", "The selected locale runtime is unavailable.")
}

pub(crate) struct Library(*mut c_void);
// Loader handles are immutable, and unload happens only after the last runtime
// owner (including every collator) is dropped. No borrowed symbol escapes it.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Library {
    pub(crate) fn open(path: &Path) -> IvyResult<Self> {
        if !path.is_absolute() { return Err(unavailable()); }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32};
            // canonicalize supplies the verbatim path required by Win32 when
            // the selected installation directory exceeds MAX_PATH. It keeps
            // native UTF-16 path units, including unpaired surrogates.
            let path = std::fs::canonicalize(path).map_err(|_| unavailable())?;
            let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
            if wide.contains(&0) { return Err(unavailable()); }
            wide.push(0);
            let raw = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) };
            if raw.is_null() { Err(unavailable()) } else { Ok(Self(raw)) }
        }
        #[cfg(target_os = "linux")]
        {
            use std::{ffi::CString, os::unix::ffi::OsStrExt};
            let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| unavailable())?;
            let raw = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if raw.is_null() { Err(unavailable()) } else { Ok(Self(raw)) }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        { Err(unavailable()) }
    }

    pub(crate) fn symbol(&self, name: &CStr) -> IvyResult<*mut c_void> {
        #[cfg(windows)]
        let raw = unsafe { windows_sys::Win32::System::LibraryLoader::GetProcAddress(self.0, name.as_ptr().cast()) }.map(|p| p as *mut c_void).unwrap_or(std::ptr::null_mut());
        #[cfg(target_os = "linux")]
        let raw = unsafe { libc::dlsym(self.0, name.as_ptr()) };
        #[cfg(not(any(windows, target_os = "linux")))]
        let raw: *mut c_void = std::ptr::null_mut();
        if raw.is_null() { Err(unavailable()) } else { Ok(raw) }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe { windows_sys::Win32::Foundation::FreeLibrary(self.0); }
        #[cfg(target_os = "linux")]
        unsafe { libc::dlclose(self.0); }
    }
}
