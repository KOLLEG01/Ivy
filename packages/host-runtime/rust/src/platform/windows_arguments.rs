//! Win32 command lines use CRT quoting, never shell escaping.

/// Give applications an ordinary module filename when it names the same file.
/// Keep extended spelling for long names and names that DOS normalization changes.
#[cfg(windows)]
pub fn application_path(value: &std::ffi::OsStr) -> std::ffi::OsString {
    use std::{
        fs,
        os::windows::ffi::{OsStrExt, OsStringExt},
    };
    let units: Vec<_> = value.encode_wide().collect();
    let ordinary = if units.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
        [vec![92, 92], units[8..].to_vec()].concat()
    } else if units.starts_with(&[92, 92, 63, 92])
        && units
            .get(4)
            .is_some_and(|unit| (65..=90).contains(unit) || (97..=122).contains(unit))
        && units.get(5) == Some(&58)
        && units.get(6) == Some(&92)
    {
        units[4..].to_vec()
    } else {
        return value.to_owned();
    };
    if ordinary.len() >= 260 {
        return value.to_owned();
    }
    let ordinary = std::ffi::OsString::from_wide(&ordinary);
    match (fs::canonicalize(value), fs::canonicalize(&ordinary)) {
        (Ok(selected), Ok(candidate)) if selected == candidate => ordinary,
        _ => value.to_owned(),
    }
}

/// Quote one UTF-16 argument without losing unpaired Windows filename code units.
pub fn quote_argument(value: &[u16]) -> Vec<u16> {
    if !value.is_empty() && !value.iter().any(|unit| matches!(*unit, 9 | 32 | 34)) {
        return value.to_vec();
    }
    let mut result = vec![b'"' as u16];
    let mut slashes = 0;
    for &character in value {
        if character == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            b'\\' as u16,
            if character == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        result.push(character);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    result.push(b'"' as u16);
    result
}

pub fn valid_job_name(name: &str) -> bool {
    name.strip_prefix("Global\\Ivy.").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.len() <= 128
            && suffix
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"._-".contains(&ch))
    })
}
