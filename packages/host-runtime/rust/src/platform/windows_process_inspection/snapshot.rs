use super::*;
use std::{
    mem::size_of,
    slice,
    sync::atomic::{AtomicU32, Ordering},
};

fn equal_name(left: &[u16], right: &[u16]) -> io::Result<bool> {
    // Empty filters select all process rows, as the original local snapshot does.
    if right.is_empty() {
        return Ok(true);
    }
    if left.is_empty() {
        return Ok(false);
    }
    match unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    } {
        0 => Err(io::Error::last_os_error()),
        2 => Ok(true),
        _ => Ok(false),
    }
}

pub(super) fn processes(name: Option<&str>, pid: Option<u32>) -> io::Result<Vec<(u32, u32)>> {
    let filter = name.map(|value| value.encode_utf16().collect::<Vec<_>>());
    static LAST_SIZE: AtomicU32 = AtomicU32::new(1024 * 1024);
    let mut data =
        vec![0usize; (LAST_SIZE.load(Ordering::Relaxed) as usize).div_ceil(size_of::<usize>())];
    let actual = loop {
        let mut needed = 0u32;
        let length = u32::try_from(data.len() * size_of::<usize>())
            .map_err(|_| io::Error::other("Process snapshot is too large."))?;
        let status =
            unsafe { NtQuerySystemInformation(5, data.as_mut_ptr().cast(), length, &mut needed) };
        if status == 0xc0000004u32 as i32 {
            let length = (needed as usize)
                .checked_add(10240)
                .ok_or_else(|| io::Error::other("Process snapshot is too large."))?;
            data.resize(length.div_ceil(size_of::<usize>()), 0);
            continue;
        }
        if status < 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
        LAST_SIZE.store(needed.saturating_add(10240), Ordering::Relaxed);
        break needed as usize;
    };
    let base = data.as_ptr() as usize;
    if actual > data.len() * size_of::<usize>() {
        return Err(io::Error::other("Invalid process snapshot length."));
    }
    let mut offset = 0usize;
    let mut rows = Vec::new();
    loop {
        if offset
            .checked_add(size_of::<ProcessHeader>())
            .is_none_or(|end| end > actual)
        {
            return Err(io::Error::other("Incomplete process snapshot."));
        }
        let row = unsafe { std::ptr::read_unaligned((base + offset) as *const ProcessHeader) };
        let process_id =
            u32::try_from(row.pid).map_err(|_| io::Error::other("Invalid process identifier."))?;
        if pid.is_none_or(|wanted| wanted == process_id) {
            let fallback;
            let mut short = if row.name.buffer.is_null() {
                fallback = match process_id {
                    0 => "Idle".to_owned(),
                    4 => "System".to_owned(),
                    _ => process_id.to_string(),
                }
                .encode_utf16()
                .collect::<Vec<_>>();
                fallback.as_slice()
            } else {
                let start = row.name.buffer as usize;
                let end = start.checked_add(row.name.length as usize);
                if row.name.length % 2 != 0
                    || start % 2 != 0
                    || start < base
                    || end.is_none_or(|end| end > base + actual)
                {
                    return Err(io::Error::other("Invalid process name in snapshot."));
                }
                unsafe { slice::from_raw_parts(row.name.buffer, row.name.length as usize / 2) }
            };
            if let Some(index) = short.iter().rposition(|unit| *unit == b'\\' as u16) {
                short = &short[index + 1..];
            }
            if short.len() >= 4 && equal_name(&short[short.len() - 4..], &[46, 101, 120, 101])? {
                short = &short[..short.len() - 4];
            }
            if filter.as_deref().is_none_or(|wanted| wanted.is_empty())
                || equal_name(short, filter.as_deref().unwrap())?
            {
                // The original dictionary retains the first insertion order and
                // replaces duplicate process IDs in place.
                if let Some(existing) = rows
                    .iter_mut()
                    .find(|row: &&mut (u32, u32)| row.0 == process_id)
                {
                    existing.1 = row.session;
                } else {
                    rows.push((process_id, row.session));
                }
            }
        }
        if row.next == 0 {
            break;
        }
        if (row.next as usize) < size_of::<ProcessHeader>() {
            return Err(io::Error::other("Invalid process snapshot offset."));
        }
        offset = offset
            .checked_add(row.next as usize)
            .ok_or_else(|| io::Error::other("Invalid process snapshot offset."))?;
    }
    Ok(rows)
}
