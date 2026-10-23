//! The Micro command's text and Windows console readers with BOM overrides.
use ivy_host_runtime::process_output::Utf8Decoder;
#[cfg(windows)]
use ivy_host_runtime::process_output::WindowsCodePageDecoder;
use std::{collections::VecDeque, io};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum MicroOutputError {
    Cancelled,
    Io(io::Error),
    Limit,
}
impl std::fmt::Display for MicroOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Micro command output cancelled."),
            Self::Limit => f.write_str("Bounded USBip command output exceeded."),
            Self::Io(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for MicroOutputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Io(error) = self {
            Some(error)
        } else {
            None
        }
    }
}
impl From<io::Error> for MicroOutputError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

enum Decoder {
    Utf8(Utf8Decoder),
    #[cfg(windows)]
    Windows(WindowsCodePageDecoder),
    Utf16 {
        big: bool,
        byte: Option<u8>,
        high: Option<u16>,
    },
    Utf32 {
        big: bool,
        bytes: Vec<u8>,
    },
}
impl Default for Decoder {
    fn default() -> Self {
        Self::Utf8(Utf8Decoder::preserving_bom())
    }
}
impl Decoder {
    fn decode(&mut self, bytes: &[u8], end: bool) -> io::Result<Vec<u16>> {
        Ok(match self {
            Self::Utf8(value) => value.decode(bytes, end),
            #[cfg(windows)]
            Self::Windows(value) => {
                // The original .NET10 console decoder has empty DBCS lead
                // ranges (GetLeadByteRanges checks its zeroed destination).
                // Preserve its per-read conversion; only UTF8 carries bytes.
                value.decode(bytes, end || value.code_page() != 65001)?
            }
            Self::Utf16 { big, byte, high } => {
                let mut out = Vec::new();
                for &next in bytes {
                    let Some(first) = byte.take() else {
                        *byte = Some(next);
                        continue;
                    };
                    let value = if *big {
                        u16::from_be_bytes([first, next])
                    } else {
                        u16::from_le_bytes([first, next])
                    };
                    if let Some(previous) = high.take() {
                        if (0xdc00..=0xdfff).contains(&value) {
                            out.extend([previous, value]);
                            continue;
                        } else {
                            out.push(0xfffd)
                        }
                    }
                    if (0xd800..=0xdbff).contains(&value) {
                        *high = Some(value)
                    } else if (0xdc00..=0xdfff).contains(&value) {
                        out.push(0xfffd)
                    } else {
                        out.push(value)
                    }
                }
                if end {
                    if high.take().is_some() {
                        out.push(0xfffd)
                    }
                    if byte.take().is_some() {
                        out.push(0xfffd)
                    }
                }
                out
            }
            Self::Utf32 {
                big,
                bytes: pending,
            } => {
                let mut out = Vec::new();
                for &next in bytes {
                    pending.push(next);
                    if pending.len() == 4 {
                        let raw = pending[..].try_into().unwrap();
                        let value = if *big {
                            u32::from_be_bytes(raw)
                        } else {
                            u32::from_le_bytes(raw)
                        };
                        let value = char::from_u32(value).unwrap_or('\u{fffd}');
                        out.extend_from_slice(value.encode_utf16(&mut [0; 2]));
                        pending.clear();
                    }
                }
                if end && !pending.is_empty() {
                    out.push(0xfffd);
                    pending.clear();
                }
                out
            }
        })
    }
}
struct Reader {
    decoder: Decoder,
    preamble: Option<Vec<u8>>,
    detect: bool,
    decoded: VecDeque<u16>,
    blocked: bool,
    end: bool,
    buffer_size: usize,
}
impl Default for Reader {
    fn default() -> Self {
        Self {
            decoder: Decoder::default(),
            preamble: Some(Vec::new()),
            detect: true,
            decoded: VecDeque::new(),
            blocked: false,
            end: false,
            buffer_size: 1024,
        }
    }
}
impl Reader {
    fn console_utf8() -> Self {
        Self {
            preamble: None,
            buffer_size: 4096,
            ..Self::default()
        }
    }
    #[cfg(windows)]
    fn console_code_page(code_page: u32) -> io::Result<Self> {
        Ok(Self {
            decoder: Decoder::Windows(WindowsCodePageDecoder::new(code_page)?),
            ..Self::console_utf8()
        })
    }
    fn decode(&mut self, mut bytes: Vec<u8>, end: bool) -> io::Result<()> {
        if let Some(mut prefix) = self.preamble.take() {
            prefix.append(&mut bytes);
            bytes = prefix;
            if !end && bytes.len() < 3 && [0xef, 0xbb, 0xbf].starts_with(&bytes) {
                self.preamble = Some(bytes);
                return Ok(());
            }
            if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
                self.detect = false;
                bytes.drain(..3);
            }
        }
        if self.detect && bytes.len() >= 2 {
            self.detect = false;
            let (decoder, skip) = if bytes.starts_with(&[0xfe, 0xff]) {
                (
                    Some(Decoder::Utf16 {
                        big: true,
                        byte: None,
                        high: None,
                    }),
                    2,
                )
            } else if bytes.starts_with(&[0xff, 0xfe]) {
                if bytes.starts_with(&[0xff, 0xfe, 0, 0]) {
                    (
                        Some(Decoder::Utf32 {
                            big: false,
                            bytes: Vec::new(),
                        }),
                        4,
                    )
                } else {
                    (
                        Some(Decoder::Utf16 {
                            big: false,
                            byte: None,
                            high: None,
                        }),
                        2,
                    )
                }
            } else if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
                (Some(Decoder::default()), 3)
            } else if bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
                (
                    Some(Decoder::Utf32 {
                        big: true,
                        bytes: Vec::new(),
                    }),
                    4,
                )
            } else {
                self.detect = bytes.len() == 2;
                (None, 0)
            };
            if let Some(decoder) = decoder {
                self.decoder = decoder;
                bytes.drain(..skip);
            }
        }
        self.decoded.extend(self.decoder.decode(&bytes, end)?);
        Ok(())
    }
    async fn block(
        &mut self,
        reader: &mut (impl AsyncRead + Unpin),
        stop: &CancellationToken,
    ) -> Result<(), MicroOutputError> {
        while self.decoded.is_empty() && !self.end {
            let prefix = self.preamble.as_ref().map_or(0, Vec::len);
            let mut bytes = [0; 4096];
            let count = tokio::select! {biased;_ = stop.cancelled()=>return Err(MicroOutputError::Cancelled),result=reader.read(&mut bytes[..self.buffer_size-prefix])=>result?};
            self.end = count == 0;
            if count > 0 {
                self.blocked = count + prefix < self.buffer_size;
            }
            self.decode(bytes[..count].to_vec(), self.end)?;
            // A decoder flush may still return text. The original reader makes
            // another underlying read before the next empty character result.
            if self.end && !self.decoded.is_empty() {
                self.end = false;
            }
        }
        Ok(())
    }
    async fn chars(
        &mut self,
        reader: &mut (impl AsyncRead + Unpin),
        stop: &CancellationToken,
    ) -> Result<Vec<u16>, MicroOutputError> {
        if stop.is_cancelled() {
            return Err(MicroOutputError::Cancelled);
        }
        self.block(reader, stop).await?;
        let mut out = Vec::with_capacity(2048);
        while !self.decoded.is_empty() {
            let count = (2048 - out.len()).min(self.decoded.len());
            out.extend(self.decoded.drain(..count));
            if out.len() == 2048 || self.blocked {
                break;
            }
            self.block(reader, stop).await?;
        }
        if !out.is_empty() {
            self.end = false;
        }
        Ok(out)
    }
}
pub async fn read_micro_command_output(
    reader: &mut (impl AsyncRead + Unpin),
    stop: &CancellationToken,
) -> Result<String, MicroOutputError> {
    read_output(reader, stop, Reader::default()).await
}
/// The UTF-8 instance of the Windows Process console reader. The installed
/// client must still select its actual console code page before choosing this.
pub async fn read_micro_utf8_console_output(
    reader: &mut (impl AsyncRead + Unpin),
    stop: &CancellationToken,
) -> Result<String, MicroOutputError> {
    read_output(reader, stop, Reader::console_utf8()).await
}
/// Use the code page observed by the process owner at launch. Zero remains the
/// native CP_ACP alias; this never substitutes UTF-8 or changes console state.
#[cfg(windows)]
pub async fn read_micro_console_output(
    reader: &mut (impl AsyncRead + Unpin),
    stop: &CancellationToken,
    code_page: u32,
) -> Result<String, MicroOutputError> {
    read_output(reader, stop, Reader::console_code_page(code_page)?).await
}
async fn read_output(
    reader: &mut (impl AsyncRead + Unpin),
    stop: &CancellationToken,
    mut reader_state: Reader,
) -> Result<String, MicroOutputError> {
    let mut output = Vec::new();
    loop {
        let chars = reader_state.chars(reader, stop).await?;
        if chars.is_empty() {
            return Ok(String::from_utf16(&output).expect("Decoder emits valid UTF-16"));
        }
        if output.len() + chars.len() > 65536 {
            return Err(MicroOutputError::Limit);
        }
        output.extend(chars);
    }
}

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_micro_command_output.rs"]
mod tests;
