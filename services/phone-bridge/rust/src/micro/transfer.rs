use super::{Report, configuration, device, report_descriptor, string_descriptor};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlTransfer {
    Reply {
        status: i32,
        payload: Vec<u8>,
    },
    /// Process this report before acknowledging the SET_REPORT transfer. This is
    /// protocol input, not permission to emit a microphone action.
    HidOutput(Report),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferError;
impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Inconsistent USB setup transfer.")
    }
}
impl std::error::Error for TransferError {}

/// Decode endpoint-zero setup exactly as the original MicroUsbSession. The
/// connection owner validates device/endpoint identity and the 4096-byte URB bound.
pub fn control(
    direction: u32,
    capacity: u32,
    setup: &[u8; 8],
    payload: &[u8],
) -> Result<ControlTransfer, TransferError> {
    let [kind, request] = [setup[0], setup[1]];
    let value = u16::from_le_bytes([setup[2], setup[3]]);
    let index = u16::from_le_bytes([setup[4], setup[5]]);
    let length = u16::from_le_bytes([setup[6], setup[7]]);
    if u32::from(kind >> 7) != direction
        || u32::from(length) > capacity
        || direction == 0 && payload.len() != usize::from(length)
    {
        return Err(TransferError);
    }
    let descriptor = if kind == 0x80 && request == 6 {
        match value >> 8 {
            1 => Some(device().to_vec()),
            2 => Some(configuration().to_vec()),
            3 => string_descriptor(i32::from(value & 255)),
            _ => None,
        }
    } else if kind == 0x81 && request == 6 && index == 0 && value == 0x2200 {
        Some(report_descriptor().to_vec())
    } else if kind == 0x81 && request == 6 && index == 0 && value == 0x2100 {
        Some(configuration()[18..27].to_vec())
    } else if matches!(kind, 0x80 | 0x81 | 0x82)
        && request == 0
        && value == 0
        && length == 2
        && matches!(index, 0 | 1 | 0x81)
    {
        Some(vec![0, 0])
    } else if kind == 0x80 && request == 8 && value == 0 && index == 0 && length == 1 {
        Some(vec![1])
    } else if kind == 0x81 && request == 10 && value == 0 && index == 0 && length == 1 {
        Some(vec![0])
    } else if kind == 0xa1 && index == 0 && length == 1 && matches!(request, 2 | 3) {
        Some(vec![u8::from(request == 3)])
    } else if kind == 0x21
        && request == 9
        && index == 0
        && value == 0x0206
        && length == 64
        && payload.len() == 64
    {
        return Ok(ControlTransfer::HidOutput(payload.try_into().unwrap()));
    } else if direction == 0
        && length == 0
        && (kind == 0 && index == 0 && matches!(request, 5 | 9)
            || kind == 0x21 && index == 0 && matches!(request, 10 | 11)
            || kind == 1 && request == 11 && value == 0 && index == 0
            || kind == 2 && request == 1 && value == 0 && matches!(index, 1 | 0x81))
    {
        Some(Vec::new())
    } else {
        None
    };
    Ok(match descriptor {
        Some(mut bytes) => {
            bytes.truncate(usize::from(length));
            ControlTransfer::Reply {
                status: 0,
                payload: bytes,
            }
        }
        None => ControlTransfer::Reply {
            status: -32,
            payload: Vec::new(),
        },
    })
}
