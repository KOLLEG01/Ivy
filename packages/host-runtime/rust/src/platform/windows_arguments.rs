//! Win32 command lines use CRT quoting, never shell escaping.

/// Quote one UTF-16 argument without losing unpaired Windows filename code units.
pub fn quote_argument(value: &[u16]) -> Vec<u16> {
    let mut result = vec![b'"' as u16];
    let mut slashes = 0;
    for &character in value {
        if character == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(b'\\' as u16,
            if character == b'"' as u16 { slashes * 2 + 1 } else { slashes }));
        result.push(character);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    result.push(b'"' as u16);
    result
}

pub fn valid_job_name(name: &str) -> bool {
    name.strip_prefix("Global\\Ivy.").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.len() <= 128
            && suffix.bytes().all(|ch| ch.is_ascii_alphanumeric() || b"._-".contains(&ch))
    })
}
