//! Bounded diagnostic capture, including UTF-8 decoding and secrets split across reads.
//! Raw protocol pipes bypass this module entirely.

#[cfg(windows)]
mod windows_code_page;
#[cfg(windows)]
pub use windows_code_page::{console_output_code_page, WindowsCodePageDecoder};

pub struct Utf8Decoder {
    incomplete: Vec<u8>,
    at_start: bool,
}

impl Default for Utf8Decoder {
    fn default() -> Self {
        Self {
            incomplete: Vec::new(),
            at_start: true,
        }
    }
}

impl Utf8Decoder {
    /// Node StringDecoder retains the initial BOM; diagnostic TextDecoder does not.
    pub fn preserving_bom() -> Self { Self { incomplete: Vec::new(), at_start: false } }
    pub fn decode(&mut self, bytes: &[u8], final_chunk: bool) -> Vec<u16> {
        self.incomplete.extend_from_slice(bytes);
        let mut result = Vec::new();
        let mut consumed = 0;
        while consumed < self.incomplete.len() {
            let remaining = &self.incomplete[consumed..];
            match std::str::from_utf8(remaining) {
                Ok(text) => {
                    result.extend(text.encode_utf16());
                    consumed = self.incomplete.len();
                }
                Err(error) => {
                    // valid_up_to is a UTF-8 character boundary.
                    let valid = std::str::from_utf8(&remaining[..error.valid_up_to()]).unwrap();
                    result.extend(valid.encode_utf16());
                    consumed += error.valid_up_to();
                    match error.error_len() {
                        Some(length) => {
                            result.push(0xfffd);
                            consumed += length;
                        }
                        None if final_chunk => {
                            result.push(0xfffd);
                            consumed = self.incomplete.len();
                        }
                        None => break,
                    }
                }
            }
        }
        self.incomplete.drain(..consumed);
        // TextDecoder's default consumes one initial BOM, including when its
        // bytes span reads. Later U+FEFF characters remain ordinary text.
        if self.at_start && !result.is_empty() {
            self.at_start = false;
            if result[0] == 0xfeff {
                result.remove(0);
            }
        }
        result
    }
}

const REPLACEMENT: &[u16] = &[91, 114, 101, 100, 97, 99, 116, 101, 100, 93];
const EMISSION_BYTES: usize = 4096;

/// One literal replaceAll pass. A retained prefix is represented by its length;
/// neither an overlapping match nor replacement expansion accumulates input.
struct ReplacementStage {
    secret: Vec<u16>,
    failure: Vec<usize>,
    matched: usize,
    input: Option<u16>,
    emission: std::ops::Range<usize>,
    replacement: bool,
}

impl ReplacementStage {
    fn new(secret: Vec<u16>) -> Self {
        let mut failure = vec![0; secret.len()];
        for at in 1..secret.len() {
            let mut prefix = failure[at - 1];
            while prefix > 0 && secret[at] != secret[prefix] {
                prefix = failure[prefix - 1];
            }
            if secret[at] == secret[prefix] {
                prefix += 1;
            }
            failure[at] = prefix;
        }
        Self {
            secret,
            failure,
            matched: 0,
            input: None,
            emission: 0..0,
            replacement: false,
        }
    }

    fn feed(&mut self, unit: u16) {
        debug_assert!(self.input.is_none() && self.emission.is_empty());
        self.input = Some(unit);
    }

    fn finish(&mut self) {
        debug_assert!(self.input.is_none() && self.emission.is_empty());
        self.emission = 0..self.matched;
        self.replacement = false;
        self.matched = 0;
    }

    fn next(&mut self) -> Option<u16> {
        loop {
            if let Some(at) = self.emission.next() {
                return Some(if self.replacement {
                    REPLACEMENT[at]
                } else {
                    self.secret[at]
                });
            }
            let unit = self.input?;
            if self.matched > 0 && self.secret[self.matched] != unit {
                let fallback = self.failure[self.matched - 1];
                self.emission = 0..self.matched - fallback;
                self.replacement = false;
                self.matched = fallback;
                continue;
            }
            self.input = None;
            if self.secret[self.matched] == unit {
                self.matched += 1;
                if self.matched == self.secret.len() {
                    self.matched = 0;
                    self.emission = 0..REPLACEMENT.len();
                    self.replacement = true;
                    continue;
                }
                return None;
            }
            return Some(unit);
        }
    }
}

struct DiagnosticOutput {
    high_surrogate: Option<u16>,
    emission: String,
    tail: std::collections::VecDeque<u8>,
    maximum_bytes: usize,
    truncated: bool,
}

impl DiagnosticOutput {
    fn flush(&mut self, sink: &mut impl FnMut(&str)) {
        if self.emission.is_empty() {
            return;
        }
        for byte in self.emission.bytes() {
            if self.tail.len() == self.maximum_bytes {
                self.tail.pop_front();
                self.truncated = true;
            }
            if self.maximum_bytes > 0 {
                self.tail.push_back(byte);
            }
        }
        sink(&self.emission);
        self.emission.clear();
    }

    fn character(&mut self, value: char, sink: &mut impl FnMut(&str)) {
        if self.emission.len() + value.len_utf8() > EMISSION_BYTES {
            self.flush(sink);
        }
        self.emission.push(value);
    }

    fn unit(&mut self, unit: u16, sink: &mut impl FnMut(&str)) {
        if let Some(high) = self.high_surrogate.take() {
            if (0xdc00..=0xdfff).contains(&unit) {
                self.character(
                    char::from_u32(
                        0x10000 + ((u32::from(high) - 0xd800) << 10) + u32::from(unit) - 0xdc00,
                    )
                    .unwrap(),
                    sink,
                );
                return;
            }
            self.character(char::REPLACEMENT_CHARACTER, sink);
        }
        if (0xd800..=0xdbff).contains(&unit) {
            self.high_surrogate = Some(unit);
        } else {
            self.character(
                char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER),
                sink,
            );
        }
    }
}

pub struct DiagnosticCapture {
    decoder: Utf8Decoder,
    stages: Vec<ReplacementStage>,
    output: DiagnosticOutput,
}

impl DiagnosticCapture {
    /// Public process admission checks limits before constructing a capture.
    pub fn new(secrets: Vec<Vec<u16>>, maximum_bytes: usize) -> Self {
        Self {
            decoder: Utf8Decoder::default(),
            stages: secrets
                .into_iter()
                .filter(|s| !s.is_empty())
                .map(ReplacementStage::new)
                .collect(),
            output: DiagnosticOutput {
                high_surrogate: None,
                emission: String::with_capacity(EMISSION_BYTES),
                tail: std::collections::VecDeque::new(),
                maximum_bytes,
                truncated: false,
            },
        }
    }

    // Depth-first pumping uses a stage index, not recursion or a buffer between
    // stages. Replacement text reaches only downstream stages, in secret order.
    fn pump(&mut self, start: usize, sink: &mut impl FnMut(&str)) {
        let mut at = start;
        loop {
            if let Some(unit) = self.stages[at].next() {
                if at + 1 == self.stages.len() {
                    self.output.unit(unit, sink);
                } else {
                    at += 1;
                    self.stages[at].feed(unit);
                }
            } else if at == start {
                break;
            } else {
                at -= 1;
            }
        }
    }

    fn decoded(&mut self, units: Vec<u16>, sink: &mut impl FnMut(&str)) {
        for unit in units {
            if self.stages.is_empty() {
                self.output.unit(unit, sink);
            } else {
                self.stages[0].feed(unit);
                self.pump(0, sink);
            }
        }
    }

    /// Emits redacted UTF-8 in chunks of at most 4096 bytes with intact scalar
    /// boundaries. Active matcher prefixes total at most sum(secret.len()-1),
    /// plus up to three decoder bytes and one output high surrogate. Immutable
    /// secrets/failure tables are O(sum(secret.len())); each decode consumes at
    /// most 1027 bytes and produces at most 1027 UTF-16 units. No replacement
    /// expansion buffer is allocated between stages.
    pub fn push_to(&mut self, bytes: &[u8], final_chunk: bool, mut sink: impl FnMut(&str)) {
        for chunk in bytes.chunks(1024) {
            let units = self.decoder.decode(chunk, false);
            self.decoded(units, &mut sink);
        }
        if final_chunk {
            let units = self.decoder.decode(&[], true);
            self.decoded(units, &mut sink);
            for at in 0..self.stages.len() {
                self.stages[at].finish();
                self.pump(at, &mut sink);
            }
            if self.output.high_surrogate.take().is_some() {
                self.output
                    .character(char::REPLACEMENT_CHARACTER, &mut sink);
            }
        }
        self.output.flush(&mut sink);
    }

    // Test-only aggregation; production sinks never allocate the expanded stream.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn push(&mut self, bytes: &[u8], final_chunk: bool) -> String {
        let mut result = String::new();
        self.push_to(bytes, final_chunk, |text| result.push_str(text));
        result
    }

    #[cfg(test)]
    pub fn retained_units(&self) -> usize {
        self.stages.iter().map(|s| s.matched).sum::<usize>()
            + usize::from(self.output.high_surrogate.is_some())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.output.tail.iter().copied().collect::<Vec<_>>()).into_owned()
    }
    pub fn truncated(&self) -> bool {
        self.output.truncated
    }
}
