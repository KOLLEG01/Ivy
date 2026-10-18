use super::JsString;

/// ECMAScript StringToNumber, preserving negative zero and rejecting non-grammar
/// spellings accepted by Rust's float parser. Non-decimal integers round once.
pub fn number_from_string(value: &JsString) -> f64 {
    let units = &value.0;
    let start = units.iter().position(|u| !space(*u)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !space(*u))
        .map_or(start, |i| i + 1);
    let units = &units[start..end];
    if units.is_empty() {
        return 0.0;
    }
    if units.iter().any(|u| *u > 127) {
        return f64::NAN;
    }
    let text: String = units.iter().map(|u| *u as u8 as char).collect();
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'0' {
        let width = match bytes[1] {
            b'x' | b'X' => 4,
            b'o' | b'O' => 3,
            b'b' | b'B' => 1,
            _ => 0,
        };
        if width != 0 {
            return radix(&bytes[2..], width);
        }
    }
    let mut i = usize::from(matches!(bytes[0], b'+' | b'-'));
    if &text[i..] == "Infinity" {
        return if bytes[0] == b'-' {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let mut digits = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return f64::NAN;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            return f64::NAN;
        }
    }
    if i != bytes.len() {
        return f64::NAN;
    }
    text.parse().unwrap_or(f64::NAN)
}

fn space(u: u16) -> bool {
    matches!(u, 0x9..=0xd | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

fn radix(bytes: &[u8], width: u32) -> f64 {
    if bytes.is_empty() {
        return f64::NAN;
    }
    let mut count = 0usize;
    let mut leading = 0u64;
    let mut guard = false;
    let mut sticky = false;
    for byte in bytes {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return f64::NAN,
        };
        if u32::from(digit) >= (1 << width) {
            return f64::NAN;
        }
        for shift in (0..width).rev() {
            let bit = (digit >> shift) & 1;
            if count == 0 && bit == 0 {
                continue;
            }
            count = count.saturating_add(1);
            if count <= 53 {
                leading = (leading << 1) | u64::from(bit);
            } else if count == 54 {
                guard = bit != 0;
            } else {
                sticky |= bit != 0;
            }
        }
    }
    if count <= 53 {
        return leading as f64;
    }
    if guard && (sticky || leading & 1 != 0) {
        leading += 1;
    }
    if count > 1024 {
        return f64::INFINITY;
    }
    (leading as f64) * 2f64.powi((count - 53) as i32)
}
