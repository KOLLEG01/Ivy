//! Independent RTCP wire encoding from RFC 3550 sections 6.4, 6.5 and 6.6.
//! The call serializes its report owner separately from PCM route permission.
use super::MediaError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtcpReportBlock {
    pub source: u32,
    pub fraction_lost: u8,
    loss_bits: u32,
    pub highest_sequence: u32,
    pub jitter: u32,
    pub last_sender_report: u32,
    pub delay_since_sender_report: u32,
}
impl RtcpReportBlock {
    pub fn new(
        source: u32,
        fraction_lost: u8,
        packets_lost: i32,
        highest_sequence: u32,
        jitter: u32,
        last_sender_report: u32,
        delay_since_sender_report: u32,
    ) -> Self {
        Self {
            source,
            fraction_lost,
            loss_bits: packets_lost as u32 & 0x00ff_ffff,
            highest_sequence,
            jitter,
            last_sender_report,
            delay_since_sender_report,
        }
    }
    /// Preserve the original Phone diagnostics' unsigned 24-bit projection.
    /// A signed interpretation is a separate observable behavior decision.
    pub fn packets_lost(&self) -> u32 {
        self.loss_bits
    }
    fn write(self, output: &mut Vec<u8>) {
        word(output, self.source);
        output.push(self.fraction_lost);
        output.extend_from_slice(&self.loss_bits.to_be_bytes()[1..]);
        for value in [
            self.highest_sequence,
            self.jitter,
            self.last_sender_report,
            self.delay_since_sender_report,
        ] {
            word(output, value);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtcpSenderInfo {
    pub ntp_timestamp: u64,
    pub rtp_timestamp: u32,
    pub packet_count: u32,
    pub octet_count: u32,
}

/// One report origin and its original call's CNAME. UTF-8 is truncated at a
/// character boundary to 255 bytes, matching the original outbound writer.
pub struct RtcpReportBuilder {
    source: u32,
    cname: String,
}
impl RtcpReportBuilder {
    pub fn new(source: u32, cname: &str) -> Result<Self, MediaError> {
        if cname.is_empty() {
            return Err(MediaError::InvalidPacket);
        }
        Ok(Self {
            source,
            cname: bounded_text(cname).into(),
        })
    }
    pub fn source(&self) -> u32 {
        self.source
    }
    pub fn cname(&self) -> &str {
        &self.cname
    }
    pub fn sender(
        &self,
        sender: RtcpSenderInfo,
        blocks: &[RtcpReportBlock],
    ) -> Result<Vec<u8>, MediaError> {
        let mut bytes = Self::sender_packet(self.source, sender, blocks)?;
        bytes.extend_from_slice(&self.sdes_packet());
        Ok(bytes)
    }
    pub fn receiver(&self, blocks: &[RtcpReportBlock]) -> Result<Vec<u8>, MediaError> {
        let mut bytes = Self::receiver_packet(self.source, blocks)?;
        bytes.extend_from_slice(&self.sdes_packet());
        Ok(bytes)
    }
    pub fn sender_packet(
        source: u32,
        sender: RtcpSenderInfo,
        blocks: &[RtcpReportBlock],
    ) -> Result<Vec<u8>, MediaError> {
        if blocks.len() > 31 {
            return Err(MediaError::InvalidPacket);
        }
        let mut bytes = Vec::with_capacity(28 + blocks.len() * 24);
        header(&mut bytes, 200, blocks.len() as u8, 28 + blocks.len() * 24);
        word(&mut bytes, source);
        bytes.extend_from_slice(&sender.ntp_timestamp.to_be_bytes());
        for value in [
            sender.rtp_timestamp,
            sender.packet_count,
            sender.octet_count,
        ] {
            word(&mut bytes, value);
        }
        for block in blocks {
            block.write(&mut bytes);
        }
        Ok(bytes)
    }
    pub fn receiver_packet(source: u32, blocks: &[RtcpReportBlock]) -> Result<Vec<u8>, MediaError> {
        if blocks.len() > 31 {
            return Err(MediaError::InvalidPacket);
        }
        let mut bytes = Vec::with_capacity(8 + blocks.len() * 24);
        header(&mut bytes, 201, blocks.len() as u8, 8 + blocks.len() * 24);
        word(&mut bytes, source);
        for block in blocks {
            block.write(&mut bytes);
        }
        Ok(bytes)
    }
    pub fn sdes_packet(&self) -> Vec<u8> {
        let text = self.cname.as_bytes();
        let size = (11 + text.len()).next_multiple_of(4);
        let mut bytes = Vec::with_capacity(size);
        header(&mut bytes, 202, 1, size);
        word(&mut bytes, self.source);
        bytes.extend_from_slice(&[1, text.len() as u8]);
        bytes.extend_from_slice(text);
        bytes.resize(size, 0); // END plus chunk alignment, not RTCP P padding.
        bytes
    }
    pub fn bye_packet(&self, reason: &str) -> Vec<u8> {
        let reason = bounded_text(reason).as_bytes();
        let size = if reason.is_empty() {
            8
        } else {
            (9 + reason.len()).next_multiple_of(4)
        };
        let mut bytes = Vec::with_capacity(size);
        header(&mut bytes, 203, 1, size);
        word(&mut bytes, self.source);
        if !reason.is_empty() {
            bytes.push(reason.len() as u8);
            bytes.extend_from_slice(reason);
            bytes.resize(size, 0);
        }
        bytes
    }
}
fn bounded_text(text: &str) -> &str {
    let mut length = text.len().min(255);
    while !text.is_char_boundary(length) {
        length -= 1;
    }
    &text[..length]
}
fn word(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}
fn header(output: &mut Vec<u8>, kind: u8, count: u8, size: usize) {
    output.extend_from_slice(&[0x80 | count, kind]);
    output.extend_from_slice(&((size / 4 - 1) as u16).to_be_bytes());
}

/// Count only actual admitted RTP sends. Report creation keeps the original
/// cumulative SR/RR choice: once RTP was sent, reports remain sender reports.
/// The report clock and transport completion belong to the media session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtcpTransmitCounters {
    pub packets: u32,
    pub octets: u32,
    pub last_sequence: u16,
    pub last_timestamp: u32,
}
impl RtcpTransmitCounters {
    pub fn record(
        &mut self,
        sequence: u16,
        timestamp: u32,
        payload_octets: usize,
    ) -> Result<(), MediaError> {
        if payload_octets > super::RtpReceiveQueue::MAXIMUM_PAYLOAD {
            return Err(MediaError::InvalidPacket);
        }
        self.packets = self.packets.wrapping_add(1);
        self.octets = self.octets.wrapping_add(payload_octets as u32);
        self.last_sequence = sequence;
        self.last_timestamp = timestamp;
        Ok(())
    }
    pub fn sender_info(&self, ntp_timestamp: u64) -> RtcpSenderInfo {
        RtcpSenderInfo {
            ntp_timestamp,
            rtp_timestamp: self.last_timestamp,
            packet_count: self.packets,
            octet_count: self.octets,
        }
    }
}
