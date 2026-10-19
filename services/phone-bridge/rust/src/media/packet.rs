//! Independent RTP framing from RFC 3550 sections 5.1 and 5.3.1.
//! Parsing borrows the datagram; admission owns any subsequent payload copy.
use super::MediaError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtpHeader {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub source: u32,
}
impl RtpHeader {
    pub const BYTES: usize = 12;
    /// Outbound audio has no CSRC, extension or padding. The payload is already
    /// encoded by the media owner at its independently negotiated RTP clock.
    pub fn write(&self, payload: &[u8], output: &mut [u8]) -> Result<usize, MediaError> {
        let size = Self::BYTES
            .checked_add(payload.len())
            .ok_or(MediaError::InvalidPacket)?;
        if self.payload_type > 127 || size > 65535 || output.len() < size {
            return Err(MediaError::InvalidPacket);
        }
        output[0] = 0x80;
        output[1] = self.payload_type | if self.marker { 0x80 } else { 0 };
        output[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        output[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        output[8..12].copy_from_slice(&self.source.to_be_bytes());
        output[12..size].copy_from_slice(payload);
        Ok(size)
    }
}

#[derive(Clone, Copy)]
pub struct RtpExtension<'a> {
    pub profile: u16,
    pub data: &'a [u8],
}
#[derive(Clone, Copy)]
pub struct RtpPacket<'a> {
    pub header: RtpHeader,
    /// Network-order u32 values. At most fifteen CSRCs are present.
    pub contributing_sources: &'a [u8],
    pub extension: Option<RtpExtension<'a>>,
    pub padding: u8,
    pub payload: &'a [u8],
}
impl<'a> RtpPacket<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MediaError> {
        if bytes.len() < RtpHeader::BYTES || bytes.len() > 65535 || bytes[0] >> 6 != 2 {
            return Err(MediaError::InvalidPacket);
        }
        let mut offset = RtpHeader::BYTES + usize::from(bytes[0] & 15) * 4;
        let contributing_sources = bytes.get(12..offset).ok_or(MediaError::InvalidPacket)?;
        let extension = if bytes[0] & 0x10 != 0 {
            let header = bytes
                .get(offset..offset + 4)
                .ok_or(MediaError::InvalidPacket)?;
            let profile = u16::from_be_bytes([header[0], header[1]]);
            let size = usize::from(u16::from_be_bytes([header[2], header[3]])) * 4;
            offset += 4;
            let data = bytes
                .get(offset..offset + size)
                .ok_or(MediaError::InvalidPacket)?;
            offset += size;
            Some(RtpExtension { profile, data })
        } else {
            None
        };
        let padding = if bytes[0] & 0x20 != 0 {
            let size = *bytes.last().ok_or(MediaError::InvalidPacket)?;
            if size == 0 || usize::from(size) > bytes.len() - offset {
                return Err(MediaError::InvalidPacket);
            }
            size
        } else {
            0
        };
        Ok(Self {
            header: RtpHeader {
                marker: bytes[1] & 0x80 != 0,
                payload_type: bytes[1] & 127,
                sequence: u16::from_be_bytes([bytes[2], bytes[3]]),
                timestamp: u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
                source: u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
            },
            contributing_sources,
            extension,
            padding,
            payload: &bytes[offset..bytes.len() - usize::from(padding)],
        })
    }
}
