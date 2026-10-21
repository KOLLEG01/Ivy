// Framing adapted from codexdeck d8d8e737, emulator/framing.js.
// MIT attribution: licenses/CodexMicro-MIT.txt.
pub const REPORT_SIZE: usize = 64;
pub const PAYLOAD_SIZE: usize = 61;
pub const MAXIMUM_MESSAGE_BYTES: usize = 16384;
pub type Report = [u8; REPORT_SIZE];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportError {
    MessageBound,
    InvalidReport,
}
impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MessageBound => "Bounded Micro message required.",
            Self::InvalidReport => "Invalid Micro RPC report.",
        })
    }
}
impl std::error::Error for ReportError {}

pub fn frame(message: &[u8]) -> Result<Vec<Report>, ReportError> {
    if !(1..=MAXIMUM_MESSAGE_BYTES).contains(&message.len()) {
        return Err(ReportError::MessageBound);
    }
    Ok(message
        .chunks(PAYLOAD_SIZE)
        .map(|chunk| {
            let mut report = [0; REPORT_SIZE];
            report[..3].copy_from_slice(&[6, 2, chunk.len() as u8]);
            report[3..3 + chunk.len()].copy_from_slice(chunk);
            report
        })
        .collect())
}

/// Keep the report ID on the USB/IP wire; ignore padding after the declared bytes.
pub fn payload(report: &[u8]) -> Result<&[u8], ReportError> {
    if report.len() != REPORT_SIZE
        || report[0] != 6
        || report[1] != 2
        || usize::from(report[2]) > PAYLOAD_SIZE
    {
        return Err(ReportError::InvalidReport);
    }
    Ok(&report[3..3 + usize::from(report[2])])
}

/// The firmware notification has no request ID and never queues a future action.
pub fn microphone(pressed: bool) -> Report {
    let message = if pressed {
        b"{\"m\":\"v.oai.hid\",\"p\":{\"k\":\"ACT10\",\"act\":1}}\n"
    } else {
        b"{\"m\":\"v.oai.hid\",\"p\":{\"k\":\"ACT10\",\"act\":0}}\n"
    };
    frame(message).expect("fixed microphone event fits one report")[0]
}
