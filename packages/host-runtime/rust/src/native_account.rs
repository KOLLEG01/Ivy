//! Executing-account inputs. No profile creation, environment mutation or shell.
use ivy_contracts::JsString;
use std::{collections::BTreeMap, io, path::Path};

use crate::node_path::node_utf8 as node_string;

pub fn environment() -> BTreeMap<JsString, JsString> {
    let mut values: BTreeMap<_, _> = std::env::vars_os()
        .map(|(key, value)| (node_string(&key), node_string(&value)))
        .collect();
    // process.env lookups on Windows ignore key case. Explicit caller-provided
    // JSON environments retain their own property semantics in layout helpers.
    #[cfg(windows)]
    for key in ["HOME", "USERPROFILE", "CODEX_HOME"] {
        if let Some(value) = std::env::var_os(key) {
            values.insert(key.into(), node_string(&value));
        }
    }
    values
}

/// Node os.homedir's native environment/profile lookup, before path validation.
pub fn os_homedir() -> io::Result<JsString> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Some(value) = std::env::var_os(key) {
        #[cfg(windows)]
        if char::decode_utf16(crate::node_path::from_native(Path::new(&value)).0)
            .map(|c| c.map_or(3, char::len_utf8))
            .sum::<usize>()
            < 3
        {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        return Ok(node_string(&value));
    }
    profile_home()
}

#[cfg(windows)]
fn profile_home() -> io::Result<JsString> {
    use std::ptr::null_mut;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE},
        Security::TOKEN_READ,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    #[link(name = "userenv")]
    unsafe extern "system" {
        fn GetUserProfileDirectoryW(token: HANDLE, path: *mut u16, size: *mut u32) -> i32;
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetUserNameW(name: *mut u16, size: *mut u32) -> i32;
    }
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_READ, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let home = (|| {
            let mut size = 0;
            GetUserProfileDirectoryW(token, null_mut(), &mut size);
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
                return Err(error);
            }
            let mut home = vec![0; size as usize];
            if GetUserProfileDirectoryW(token, home.as_mut_ptr(), &mut size) == 0 {
                return Err(io::Error::last_os_error());
            }
            home.truncate(home.iter().position(|c| *c == 0).unwrap_or(home.len()));
            Ok(JsString(home))
        })();
        CloseHandle(token);
        let home = home?;
        // libuv's passwd fallback also obtains the account name; preserve its
        // failure boundary without exposing the name to host configuration.
        let mut name = [0; 257];
        let mut size = name.len() as u32;
        if GetUserNameW(name.as_mut_ptr(), &mut size) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(node_string(crate::node_path::to_native(&home).as_os_str()))
    }
}

#[cfg(unix)]
fn profile_home() -> io::Result<JsString> {
    use std::{ffi::CStr, mem::MaybeUninit, ptr::null_mut};
    let mut size = 2000;
    loop {
        let mut buffer = vec![0; size];
        let mut passwd = MaybeUninit::<libc::passwd>::uninit();
        let mut result = null_mut();
        let error = loop {
            let error = unsafe {
                libc::getpwuid_r(
                    libc::geteuid(),
                    passwd.as_mut_ptr(),
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &mut result,
                )
            };
            if error != libc::EINTR {
                break error;
            }
        };
        if error == libc::ERANGE {
            size = size
                .checked_mul(2)
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
            continue;
        }
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        if result.is_null() {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        let passwd = unsafe { passwd.assume_init() };
        if passwd.pw_dir.is_null() {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        return Ok(JsString::from(
            String::from_utf8_lossy(unsafe { CStr::from_ptr(passwd.pw_dir) }.to_bytes()).as_ref(),
        ));
    }
}
