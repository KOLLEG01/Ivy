use super::{BUS_ID, SERIAL};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventoryError {
    Bound,
    Unrecognized,
    Port,
    Location,
    Conflict,
}
impl std::fmt::Display for InventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Bound => "Bounded USBip inventory required.",
            Self::Unrecognized => "Unrecognized USBip inventory.",
            Self::Port => "Invalid/duplicate USBip port.",
            Self::Location => "Exact USBip location required.",
            Self::Conflict => "Conflicting Micro attachment at the selected endpoint.",
        })
    }
}
impl std::error::Error for InventoryError {}

// Pinned client labels, matching MicroPortInventory. A VID/PID, hostname prefix,
// occupied driver port or an incomplete observation never establishes ownership.
pub fn own_port(output: Option<&str>, tcp_port: i32) -> Result<Option<u8>, InventoryError> {
    let output = output
        .filter(|s| s.encode_utf16().count() <= 65536)
        .ok_or(InventoryError::Bound)?;
    if output.chars().all(char::is_whitespace) {
        return Ok(None);
    }
    let mut blocks = Vec::new();
    let mut offset = 0;
    for line in output.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        if !content.contains('\r') {
            if let Some(rest) = content.strip_prefix("Port ") {
                let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
                if (1..=3).contains(&digits) && rest.as_bytes().get(digits) == Some(&b':') {
                    let port = rest[..digits].parse::<u16>().unwrap();
                    blocks.push((offset, port));
                }
            }
        }
        offset += line.len();
    }
    if blocks.is_empty() {
        return Err(InventoryError::Unrecognized);
    }
    let expected = format!("usbip://127.0.0.1:{tcp_port}/{BUS_ID}");
    let serial = format!("-> serial: {SERIAL}");
    let mut ports = HashSet::new();
    let mut selected = None;
    for (index, &(start, port)) in blocks.iter().enumerate() {
        if !(1..=255).contains(&port) || !ports.insert(port) {
            return Err(InventoryError::Port);
        }
        let end = blocks.get(index + 1).map_or(output.len(), |b| b.0);
        let mut locations = Vec::new();
        let mut remote = false;
        let mut own_serial = false;
        for line in output[start..end].split('\n') {
            let line = line.trim_start_matches(char::is_whitespace);
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.contains('\r') {
                continue;
            }
            if let Some(location) = line.strip_prefix("-> ") {
                if location
                    .strip_prefix("usbip://")
                    .is_some_and(|s| !s.is_empty())
                {
                    locations.push(location);
                }
            }
            remote |= line == "-> remote bus/dev: 001/007";
            own_serial |= line == serial;
        }
        if locations.len() != 1 {
            return Err(InventoryError::Location);
        }
        if locations[0] != expected {
            continue;
        }
        if selected.is_some() || !remote || !own_serial {
            return Err(InventoryError::Conflict);
        }
        selected = Some(port as u8);
    }
    Ok(selected)
}
