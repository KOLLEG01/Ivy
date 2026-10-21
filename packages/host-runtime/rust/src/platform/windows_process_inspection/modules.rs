use super::*;
use std::{ffi::OsString, os::windows::ffi::OsStringExt, thread, time::Duration};

fn enumerate(process: BorrowedHandle<'_>, modules: &mut [Handle]) -> io::Result<u32> {
    let mut attempts = 0;
    loop {
        let mut needed = 0;
        if unsafe {
            K32EnumProcessModulesEx(
                process.as_raw_handle(),
                if modules.is_empty() {
                    null_mut()
                } else {
                    modules.as_mut_ptr()
                },
                (std::mem::size_of_val(modules)) as u32,
                &mut needed,
                3,
            )
        } != 0
        {
            return Ok(needed);
        }
        let error = io::Error::last_os_error();
        if attempts > 50 {
            return Err(error);
        }
        attempts += 1;
        thread::sleep(Duration::from_millis(1));
    }
}

fn module_error(error: io::Error) -> io::Result<Option<PathBuf>> {
    if matches!(error.raw_os_error(), Some(6 | 299)) {
        Ok(None)
    } else {
        Err(error)
    }
}

pub(super) fn main_module(pid: u32) -> io::Result<Option<PathBuf>> {
    if pid == 0 || pid == 4 {
        return Err(io::Error::from_raw_os_error(0x80004005u32 as i32));
    }
    let process = process_handle(pid, 0x400 | 0x10, true)?.unwrap();
    let mut needed = 0;
    if unsafe { K32EnumProcessModulesEx(process.as_raw_handle(), null_mut(), 0, &mut needed, 3) }
        == 0
    {
        let mut source = 0;
        let mut target = 0;
        if unsafe { IsWow64Process(GetCurrentProcess(), &mut source) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { IsWow64Process(process.as_raw_handle(), &mut target) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if source != 0 && target == 0 {
            return Err(io::Error::from_raw_os_error(299));
        }
        needed = enumerate(process.as_handle(), &mut [])?;
    }
    let mut handles = vec![null_mut(); needed as usize / size_of::<Handle>()];
    loop {
        let previous = needed;
        needed = enumerate(process.as_handle(), &mut handles)?;
        if previous == needed {
            break;
        }
        if needed > previous && needed as usize / size_of::<Handle>() > handles.len() {
            handles.resize(needed as usize / size_of::<Handle>(), null_mut());
        }
    }
    let Some(&module) = handles.first() else {
        return Ok(None);
    };
    let mut info = ModuleInfo::default();
    if unsafe {
        K32GetModuleInformation(
            process.as_raw_handle(),
            module,
            &mut info,
            size_of::<ModuleInfo>() as u32,
        )
    } == 0
    {
        return module_error(io::Error::last_os_error());
    }
    // The original shared array pool rounds its 260-character request to 512.
    let mut text = vec![0u16; 512];
    loop {
        let size = unsafe {
            K32GetModuleBaseNameW(
                process.as_raw_handle(),
                module,
                text.as_mut_ptr(),
                text.len() as u32,
            )
        };
        if size == 0 {
            return module_error(io::Error::last_os_error());
        }
        if size as usize != text.len() {
            break;
        }
        text.resize(text.len() * 2, 0);
    }
    loop {
        let size = unsafe {
            K32GetModuleFileNameExW(
                process.as_raw_handle(),
                module,
                text.as_mut_ptr(),
                text.len() as u32,
            )
        };
        if size == 0 {
            return module_error(io::Error::last_os_error());
        }
        if size as usize == text.len() {
            text.resize(text.len() * 2, 0);
            continue;
        }
        text.truncate(size as usize);
        let text = text.strip_prefix(&[92, 92, 63, 92]).unwrap_or(&text);
        return Ok(Some(PathBuf::from(OsString::from_wide(text))));
    }
}
