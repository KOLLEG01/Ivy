//! Native Windows mount-point directory links.
use std::{fs, io, os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle}, path::Path};
use windows_sys::Win32::{Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT}, System::IO::DeviceIoControl};

/// `target` is already an absolute native path.
/// Never replace an existing destination. Remove only a newly created empty
/// link directory if setting its reparse data fails.
pub(crate) fn create(target: &Path, destination: &Path) -> io::Result<()> {
    let raw: Vec<u16> = target.as_os_str().encode_wide().collect();
    if raw.contains(&0) { return Err(io::ErrorKind::InvalidInput.into()); }
    let extended: Vec<_> = "\\\\?\\".encode_utf16().collect();
    let print = if raw.starts_with(&extended) { raw[4..].to_vec() }
        else if raw.starts_with(&[92, 92]) { "UNC\\".encode_utf16().chain(raw[2..].iter().copied()).collect() }
        else {
            if raw.len() < 3 || !matches!(raw[0], 65..=90 | 97..=122) || raw[1] != 58 || !matches!(raw[2], 47 | 92) { return Err(io::ErrorKind::InvalidInput.into()); }
            raw
        };
    let print: Vec<_> = print.into_iter().map(|c| if c == 47 { 92 } else { c }).collect();
    let substitute: Vec<_> = "\\??\\".encode_utf16().chain(print.iter().copied()).collect();
    // REPARSE_DATA_BUFFER mount-point header: eight-byte common header,
    // four u16 name fields, then NUL-terminated substitute and print names.
    let bytes = 16 + 2 * (substitute.len() + print.len() + 2);
    if bytes > 16_384 { return Err(io::ErrorKind::InvalidInput.into()); }
    let mut buffer = vec![0u8; bytes];
    buffer[..4].copy_from_slice(&0xa000_0003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
    for (offset, value) in [(4, bytes - 8), (8, 0), (10, substitute.len() * 2), (12, (substitute.len() + 1) * 2), (14, print.len() * 2)] {
        buffer[offset..offset + 2].copy_from_slice(&(value as u16).to_le_bytes());
    }
    for (index, unit) in substitute.into_iter().chain([0]).chain(print).chain([0]).enumerate() {
        buffer[16 + index * 2..18 + index * 2].copy_from_slice(&unit.to_le_bytes());
    }
    fs::create_dir(destination)?;
    let result = (|| {
        let file = fs::OpenOptions::new().access_mode(0x4000_0000) // GENERIC_WRITE
            .share_mode(0).custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(destination)?;
        let mut returned = 0;
        // SAFETY: the synchronous call borrows a live file and initialized
        // buffer; it has no output buffer or asynchronous OVERLAPPED state.
        if unsafe { DeviceIoControl(file.as_raw_handle(), 0x0009_00a4, buffer.as_ptr().cast(), buffer.len() as u32,
            std::ptr::null_mut(), 0, &mut returned, std::ptr::null_mut()) } == 0 {
            Err(io::Error::last_os_error())
        } else { Ok(()) }
    })();
    if result.is_err() { let _ = fs::remove_dir(destination); }
    result
}
