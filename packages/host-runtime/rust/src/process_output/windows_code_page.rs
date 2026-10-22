//! Windows' no-provider console encoding; the caller owns BOM and read policy.
use super::Utf8Decoder;
use std::io;

#[repr(C)]
struct CodePageInfo {
    maximum_character_bytes: u32,
    default_character: [u8; 2],
    lead_byte_ranges: [u8; 12],
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleOutputCP() -> u32;
    fn GetCPInfo(code_page: u32, info: *mut CodePageInfo) -> i32;
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        bytes: *const u8,
        byte_count: i32,
        units: *mut u16,
        unit_count: i32,
    ) -> i32;
}

/// Return GetConsoleOutputCP exactly, including zero. The caller owns the
/// interpretation of a failed observation; no fallback or console change occurs.
pub fn console_output_code_page() -> u32 {
    unsafe { GetConsoleOutputCP() }
}

/// Incremental UTF-16 output using an explicit installed Windows code page.
/// UTF-8 reuses the existing decoder and preserves its initial BOM. Other pages
/// use native replacement behavior, with the no-provider OSEncoding DBCS state.
/// No encoding provider, BOM override, read buffer or character limit is supplied.
pub struct WindowsCodePageDecoder {
    code_page: u32,
    utf8: Option<Utf8Decoder>,
    lead_byte_ranges: [u8; 12],
    pending_lead: Option<u8>,
}

impl WindowsCodePageDecoder {
    pub fn new(code_page: u32) -> io::Result<Self> {
        let mut info: CodePageInfo = unsafe { std::mem::zeroed() };
        if unsafe { GetCPInfo(code_page, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // Use OSEncoding.GetDecoder's code pages, rather than guessing from
        // MaxCharSize (which also covers stateful encodings). Pseudo pages,
        // including zero/CP_ACP, retain its per-call native conversion policy.
        // https://github.com/dotnet/runtime/blob/v10.0.0/src/libraries/Common/src/System/Text/OSEncoding.Windows.cs
        let dbcs = matches!(code_page,
            932 | 936 | 949 | 950 | 1361 | 10001 | 10002 | 10003 | 10008 |
            20000 | 20001 | 20002 | 20003 | 20004 | 20005 | 20261 | 20932 | 20936 | 51949);
        Ok(Self {
            code_page,
            utf8: (code_page == 65001).then(Utf8Decoder::preserving_bom),
            lead_byte_ranges: if dbcs { info.lead_byte_ranges } else { [0; 12] },
            pending_lead: None,
        })
    }

    pub fn code_page(&self) -> u32 { self.code_page }

    pub fn decode(&mut self, bytes: &[u8], final_chunk: bool) -> io::Result<Vec<u16>> {
        if let Some(decoder) = &mut self.utf8 {
            return Ok(decoder.decode(bytes, final_chunk));
        }
        if bytes.is_empty() && (self.pending_lead.is_none() || !final_chunk) {
            return Ok(Vec::new());
        }
        let retain_last = !final_chunk && self.ends_in_lead(bytes);
        let count = bytes.len() - usize::from(retain_last);
        let input = &bytes[..count];
        let mut result = Vec::new();
        if let Some(lead) = self.pending_lead {
            let mut pair = [lead, 0];
            let consumed = usize::from(!input.is_empty());
            if consumed != 0 { pair[1] = input[0]; }
            // A carried pair is converted separately, just as DecoderDBCS does.
            result.extend(convert(self.code_page, &pair[..1 + consumed])?);
            result.extend(convert(self.code_page, &input[consumed..])?);
        } else {
            result = convert(self.code_page, input)?;
        }
        // Commit the carry only after successful native conversion.
        self.pending_lead = if retain_last { bytes.last().copied() } else { None };
        Ok(result)
    }

    fn ends_in_lead(&self, bytes: &[u8]) -> bool {
        let mut index = usize::from(self.pending_lead.is_some());
        while index < bytes.len() {
            if self.is_lead(bytes[index]) {
                index += 1;
                if index == bytes.len() { return true; }
            }
            index += 1;
        }
        false
    }

    fn is_lead(&self, byte: u8) -> bool {
        self.lead_byte_ranges.chunks_exact(2)
            .take_while(|range| range[0] != 0)
            .any(|range| (range[0]..=range[1]).contains(&byte))
    }
}

fn convert(code_page: u32, bytes: &[u8]) -> io::Result<Vec<u16>> {
    if bytes.is_empty() { return Ok(Vec::new()); }
    let count = i32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Windows conversion input is too large."))?;
    // Flags zero retain OS replacement behavior, including an incomplete final
    // DBCS lead. Explicit lengths retain NULs; the size query counts UTF-16 units.
    let needed = unsafe { MultiByteToWideChar(code_page, 0, bytes.as_ptr(), count, std::ptr::null_mut(), 0) };
    if needed == 0 { return Err(io::Error::last_os_error()); }
    let mut result = vec![0; needed as usize];
    let written = unsafe { MultiByteToWideChar(code_page, 0, bytes.as_ptr(), count, result.as_mut_ptr(), needed) };
    if written == 0 { return Err(io::Error::last_os_error()); }
    result.truncate(written as usize);
    Ok(result)
}
