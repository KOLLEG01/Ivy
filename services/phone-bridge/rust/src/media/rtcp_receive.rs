//! Bounded report decoding. Nothing is published until the complete datagram
//! validates; unsupported types advance by their declared, checked length.
//! Legacy TWCC chunk semantics retain SIPSorcery 10.0.15 behavior (Aaron
//! Clauson); its full notice is in assets/licenses/SIPSorcery-10.0.15.md.
use super::{MediaError, RtcpReportBlock, RtcpSenderInfo};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtcpReportBlocks<'a>(&'a [u8]);
impl RtcpReportBlocks<'_> {
    pub fn len(self) -> usize {
        self.0.len() / 24
    }
    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(self) -> impl ExactSizeIterator<Item = RtcpReportBlock> {
        self.0.chunks_exact(24).map(|b| {
            RtcpReportBlock::new(
                read32(b, 0),
                b[4],
                (read32(b, 4) & 0x00ff_ffff) as i32,
                read32(b, 8),
                read32(b, 12),
                read32(b, 16),
                read32(b, 20),
            )
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtcpSenderReport<'a> {
    pub source: u32,
    pub info: RtcpSenderInfo,
    pub blocks: RtcpReportBlocks<'a>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtcpReceiverReport<'a> {
    pub source: u32,
    pub blocks: RtcpReportBlocks<'a>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtcpCompound<'a> {
    pub sender: Option<RtcpSenderReport<'a>>,
    pub receiver: Option<RtcpReceiverReport<'a>>,
    /// Structurally validated packets. The call owns their legacy field
    /// interpretation and session effects, separately from frame validation.
    pub sdes_packet: Option<&'a [u8]>,
    pub bye_packet: Option<&'a [u8]>,
    /// Preserve the two original feedback categories and their routing fields.
    /// No feedback control action or value reinterpretation is performed here.
    pub feedback_packet: Option<&'a [u8]>,
    pub twcc_packet: Option<&'a [u8]>,
    pub packet_count: usize,
}
impl<'a> RtcpCompound<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MediaError> {
        if bytes.is_empty() {
            return Err(MediaError::InvalidPacket);
        }
        let mut result = Self::default();
        let mut offset = 0usize;
        while offset < bytes.len() {
            let remaining = &bytes[offset..];
            if remaining.len() < 4 || remaining[0] >> 6 != 2 {
                return Err(MediaError::InvalidPacket);
            }
            let length = (usize::from(u16::from_be_bytes([remaining[2], remaining[3]])) + 1)
                .checked_mul(4)
                .ok_or(MediaError::InvalidPacket)?;
            let packet = remaining.get(..length).ok_or(MediaError::InvalidPacket)?;
            let count = usize::from(packet[0] & 31);
            let padded = packet[0] & 32 != 0;
            let end = if padded {
                let padding = usize::from(packet[length - 1]);
                if length != remaining.len() || padding == 0 || padding > length - 4 {
                    return Err(MediaError::InvalidPacket);
                }
                length - padding
            } else {
                length
            };
            let body = &packet[..end];
            match packet[1] {
                200 | 201 => {
                    let fixed = if packet[1] == 200 { 28 } else { 8 };
                    let needed = count
                        .checked_mul(24)
                        .and_then(|n| n.checked_add(fixed))
                        .ok_or(MediaError::InvalidPacket)?;
                    // Complete report blocks and word-aligned profile extensions.
                    if body.len() < needed || body.len() % 4 != 0 {
                        return Err(MediaError::InvalidPacket);
                    }
                    let blocks = RtcpReportBlocks(&body[fixed..needed]);
                    if packet[1] == 200 {
                        result.sender = Some(RtcpSenderReport {
                            source: read32(body, 4),
                            info: RtcpSenderInfo {
                                ntp_timestamp: u64::from_be_bytes(body[8..16].try_into().unwrap()),
                                rtp_timestamp: read32(body, 16),
                                packet_count: read32(body, 20),
                                octet_count: read32(body, 24),
                            },
                            blocks,
                        });
                    } else {
                        result.receiver = Some(RtcpReceiverReport {
                            source: read32(body, 4),
                            blocks,
                        });
                    }
                }
                202 => {
                    sdes(body, count)?;
                    result.sdes_packet = Some(packet);
                }
                203 => {
                    bye(body, count)?;
                    result.bye_packet = Some(packet);
                }
                205 | 206 => {
                    feedback(body, count)?;
                    if packet[1] == 205 && count == 15 {
                        result.twcc_packet = Some(packet);
                    } else {
                        result.feedback_packet = Some(packet);
                    }
                }
                // Other packet types have no SR/RR diagnostic fields. Their
                // framing still validates, including tail padding and progress.
                _ => {}
            }
            result.packet_count += 1;
            offset = offset
                .checked_add(length)
                .ok_or(MediaError::InvalidPacket)?;
        }
        Ok(result)
    }
    /// The original application prefers the last SR, even with zero blocks,
    /// otherwise the last RR. Parsing does not require either type; application
    /// report-event admission and counting belong to the call separately.
    pub fn reception_reports(self) -> Option<RtcpReportBlocks<'a>> {
        self.sender
            .map(|r| r.blocks)
            .or_else(|| self.receiver.map(|r| r.blocks))
    }
}
fn feedback(bytes: &[u8], format: usize) -> Result<(), MediaError> {
    if bytes.len() < 12 {
        return Err(MediaError::InvalidPacket);
    }
    if bytes[1] == 205 {
        if format == 15 {
            return twcc(bytes);
        }
        if format != 5 && bytes.len() < 16 {
            return Err(MediaError::InvalidPacket);
        }
    } else if format == 15 {
        let id = bytes.get(12..16).ok_or(MediaError::InvalidPacket)?;
        if id.eq_ignore_ascii_case(b"REMB") {
            let count = usize::from(*bytes.get(16).ok_or(MediaError::InvalidPacket)?);
            bytes
                .get(..20 + count * 4)
                .ok_or(MediaError::InvalidPacket)?;
        }
    }
    Ok(())
}
// Validate the original TWCC chunk meanings without publishing new feedback
// values. Bounded chunks and complete deltas are required before any effects.
fn twcc(bytes: &[u8]) -> Result<(), MediaError> {
    if bytes.len() < 20 {
        return Err(MediaError::InvalidPacket);
    }
    let mut remaining = usize::from(u16::from_be_bytes([bytes[14], bytes[15]]));
    let mut offset = 20;
    let mut deltas = 0usize;
    while remaining > 0 {
        let value = bytes
            .get(offset..offset + 2)
            .ok_or(MediaError::InvalidPacket)?;
        let chunk = u16::from_be_bytes([value[0], value[1]]);
        offset += 2;
        match chunk >> 14 {
            0 => {
                let count = usize::from(chunk & 4095).min(remaining);
                deltas += count
                    * match (chunk >> 12) & 3 {
                        0 => 0,
                        3 => 2,
                        _ => 1,
                    };
                remaining -= count;
            }
            2 => {
                let count = remaining.min(7);
                for i in 0..count {
                    deltas += match (chunk >> (12 - i * 2)) & 3 {
                        1 => 1,
                        2 => 2,
                        _ => 0,
                    };
                }
                remaining -= count;
            }
            3 => {
                let count = remaining.min(14);
                for i in 0..count {
                    deltas += usize::from((chunk >> (13 - i)) & 1);
                }
                remaining -= count;
            }
            _ => (),
        }
    }
    bytes
        .get(offset..offset + deltas)
        .ok_or(MediaError::InvalidPacket)?;
    Ok(())
}
fn read32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn sdes(bytes: &[u8], count: usize) -> Result<(), MediaError> {
    let mut offset = 4usize;
    for _ in 0..count {
        offset = offset
            .checked_add(4)
            .filter(|&end| end <= bytes.len())
            .ok_or(MediaError::InvalidPacket)?;
        loop {
            let kind = *bytes.get(offset).ok_or(MediaError::InvalidPacket)?;
            offset += 1;
            if kind == 0 {
                let end = offset
                    .checked_add(3)
                    .map(|n| n & !3)
                    .ok_or(MediaError::InvalidPacket)?;
                if bytes
                    .get(offset..end)
                    .is_none_or(|b| b.iter().any(|&v| v != 0))
                {
                    return Err(MediaError::InvalidPacket);
                }
                offset = end;
                break;
            }
            let length = usize::from(*bytes.get(offset).ok_or(MediaError::InvalidPacket)?);
            offset += 1;
            let end = offset
                .checked_add(length)
                .ok_or(MediaError::InvalidPacket)?;
            let value = bytes.get(offset..end).ok_or(MediaError::InvalidPacket)?;
            if kind == 8 && (value.is_empty() || usize::from(value[0]) >= value.len()) {
                return Err(MediaError::InvalidPacket);
            }
            offset = end;
        }
    }
    if offset != bytes.len() {
        return Err(MediaError::InvalidPacket);
    }
    Ok(())
}
fn bye(bytes: &[u8], count: usize) -> Result<(), MediaError> {
    let start = count
        .checked_mul(4)
        .and_then(|n| n.checked_add(4))
        .ok_or(MediaError::InvalidPacket)?;
    bytes.get(4..start).ok_or(MediaError::InvalidPacket)?;
    if start != bytes.len() {
        let length = usize::from(bytes[start]);
        let end = (start + 1)
            .checked_add(length)
            .ok_or(MediaError::InvalidPacket)?;
        bytes.get(start + 1..end).ok_or(MediaError::InvalidPacket)?;
        if end.checked_add(3).map(|n| n & !3) != Some(bytes.len())
            || bytes[end..].iter().any(|&v| v != 0)
        {
            return Err(MediaError::InvalidPacket);
        }
    }
    Ok(())
}
