// USB/IP/HID layout adapted from codexdeck d8d8e737, usbip-device.ts.
// MIT attribution: licenses/CodexMicro-MIT.txt.
pub const BUS_ID: &str = "1-7";
pub const SERIAL: &str = "IvyPhoneMicro";
pub const DEVICE_ID: u32 = 0x0001_0007;

pub fn report_descriptor() -> [u8; 27] {
    [
        0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x06, 0x09, 0x01, 0x15, 0x00, 0x26, 0xff,
        0x00, 0x75, 0x08, 0x95, 0x3f, 0x81, 0x02, 0x09, 0x01, 0x91, 0x02, 0xc0,
    ]
}

pub fn device() -> [u8; 18] {
    [
        0x12, 0x01, 0x00, 0x02, 0, 0, 0, 0x40, 0x3a, 0x30, 0x60, 0x83, 0, 1, 1, 2, 3, 1,
    ]
}

pub fn configuration() -> [u8; 41] {
    [
        9, 2, 41, 0, 1, 1, 0, 0x80, 0x32, 9, 4, 0, 0, 2, 3, 0, 0, 0, 9, 0x21, 0x11, 1, 0, 1, 0x22,
        27, 0, 7, 5, 0x81, 3, 64, 0, 1, 7, 5, 0x01, 3, 64, 0, 1,
    ]
}

pub fn string_descriptor(index: i32) -> Option<Vec<u8>> {
    if index == 0 {
        return Some(vec![4, 3, 9, 4]);
    }
    let text = match index {
        1 => "Work Louder",
        2 => "Codex Micro",
        3 => SERIAL,
        _ => return None,
    };
    let mut result = vec![(2 + text.encode_utf16().count() * 2) as u8, 3];
    result.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    Some(result)
}

pub fn export() -> [u8; 312] {
    let mut result = [0; 312];
    let path = format!("/sys/devices/ivy-phone/{BUS_ID}");
    result[..path.len()].copy_from_slice(path.as_bytes());
    result[256..256 + BUS_ID.len()].copy_from_slice(BUS_ID.as_bytes());
    result[288..292].copy_from_slice(&1u32.to_be_bytes());
    result[292..296].copy_from_slice(&7u32.to_be_bytes());
    result[296..300].copy_from_slice(&3u32.to_be_bytes());
    result[300..302].copy_from_slice(&0x303au16.to_be_bytes());
    result[302..304].copy_from_slice(&0x8360u16.to_be_bytes());
    result[304..306].copy_from_slice(&0x0100u16.to_be_bytes());
    result[309..312].fill(1);
    result
}
