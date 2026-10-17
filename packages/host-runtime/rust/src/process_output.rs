//! Bounded diagnostic capture, including UTF-8 decoding and secrets split across reads.
//! Raw protocol pipes bypass this module entirely.

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

pub struct DiagnosticCapture {
    decoder: Utf8Decoder,
    secrets: Vec<Vec<u16>>,
    retained_units: usize,
    tail: Vec<u16>,
    output: Vec<u8>,
    maximum_bytes: usize,
    truncated: bool,
}

impl DiagnosticCapture {
    /// Public process admission checks limits before constructing a capture.
    pub fn new(secrets: Vec<Vec<u16>>, maximum_bytes: usize) -> Self {
        let secrets: Vec<_> = secrets.into_iter().filter(|s| !s.is_empty()).collect();
        let retained_units = secrets
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            .saturating_sub(1);
        Self {
            decoder: Utf8Decoder::default(),
            secrets,
            retained_units,
            tail: Vec::new(),
            output: Vec::new(),
            maximum_bytes,
            truncated: false,
        }
    }

    /// Returns only redacted text suitable for a diagnostic sink. A final call
    /// flushes decoder and tail once the process/pipe ownership boundary is known.
    pub fn push(&mut self, bytes: &[u8], final_chunk: bool) -> String {
        self.tail.extend(self.decoder.decode(bytes, final_chunk));
        let mut boundary = if final_chunk {
            self.tail.len()
        } else {
            self.tail.len().saturating_sub(self.retained_units)
        };
        // Moving the boundary for one secret can expose an earlier crossing
        // match of another. Revisit every secret until none crosses it.
        loop {
            let previous_boundary = boundary;
            for secret in &self.secrets {
                if secret.len() > self.tail.len() {
                    continue;
                }
                for at in 0..self.tail.len() - secret.len() + 1 {
                    if at >= boundary {
                        break;
                    }
                    if self.tail[at..].starts_with(secret) && at + secret.len() > boundary {
                        boundary = at;
                    }
                }
            }
            if boundary == previous_boundary {
                break;
            }
        }
        let mut safe: Vec<u16> = self.tail.drain(..boundary).collect();
        let replacement: Vec<u16> = "[redacted]".encode_utf16().collect();
        for secret in &self.secrets {
            let mut redacted = Vec::new();
            let mut at = 0;
            while at < safe.len() {
                if safe[at..].starts_with(secret) {
                    redacted.extend_from_slice(&replacement);
                    at += secret.len();
                } else {
                    redacted.push(safe[at]);
                    at += 1;
                }
            }
            safe = redacted;
        }
        let text = String::from_utf16_lossy(&safe);
        self.output.extend_from_slice(text.as_bytes());
        if self.output.len() > self.maximum_bytes {
            self.output.drain(..self.output.len() - self.maximum_bytes);
            self.truncated = true;
        }
        text
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}
