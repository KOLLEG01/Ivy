//! Bounded native JSONL framing. JSON and catalog validation belong to the RPC owner.

const FRAGMENT_BYTES: usize = 65_536;

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError<E> {
    TooLarge,
    InvalidUtf8,
    Consumer(E),
}

/// One receive stream. Complete frames are delivered in order, including empty
/// lines; the JSON validator must reject an empty protocol frame.
pub struct NativeLines {
    maximum: usize,
    fragments: Vec<Vec<u8>>,
    buffered: usize,
    ended: bool,
}

impl NativeLines {
    pub fn new(maximum: usize) -> Self {
        Self {
            maximum,
            fragments: Vec::new(),
            buffered: 0,
            ended: false,
        }
    }

    pub fn buffered_bytes(&self) -> usize {
        self.buffered
    }

    /// Each partial byte is copied into fixed-size fragments once. On newline,
    /// concatenate once, then decode strictly; fragmented UTF-8 is not decoded
    /// prematurely. The budget excludes only the LF, as in native stdio.
    pub fn receive<E>(
        &mut self,
        chunk: &[u8],
        mut consume: impl FnMut(&str) -> Result<(), E>,
    ) -> Result<(), FrameError<E>> {
        if self.ended {
            return Ok(());
        }
        let mut offset = 0;
        while offset < chunk.len() {
            let newline = chunk[offset..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|n| offset + n);
            let end = newline.unwrap_or(chunk.len());
            let length = end - offset;
            if length > self.maximum.saturating_sub(self.buffered) {
                self.close();
                return Err(FrameError::TooLarge);
            }
            while offset < end {
                if self.buffered % FRAGMENT_BYTES == 0 {
                    self.fragments.push(Vec::with_capacity(
                        FRAGMENT_BYTES.min(self.maximum - self.buffered),
                    ));
                }
                let tail = self.fragments.last_mut().expect("fragment was allocated");
                let count = (FRAGMENT_BYTES - tail.len()).min(end - offset);
                tail.extend_from_slice(&chunk[offset..offset + count]);
                offset += count;
                self.buffered += count;
            }
            if newline.is_none() {
                break;
            }
            let mut line = Vec::with_capacity(self.buffered);
            for fragment in self.fragments.drain(..) {
                line.extend_from_slice(&fragment);
            }
            self.buffered = 0;
            offset = end + 1;
            let text = match std::str::from_utf8(&line) {
                Ok(text) => text,
                Err(_) => {
                    self.close();
                    return Err(FrameError::InvalidUtf8);
                }
            };
            // TextDecoder('utf-8', {fatal:true}) consumes a leading BOM on each
            // separately decoded frame. Keep CR/other JSON whitespace intact.
            let text = text.strip_prefix('\u{feff}').unwrap_or(text);
            if let Err(error) = consume(text) {
                self.close();
                return Err(FrameError::Consumer(error));
            }
        }
        Ok(())
    }

    /// Return the original native end-of-stream code before releasing buffers.
    pub fn finish(&mut self) -> &'static str {
        let code = if self.buffered == 0 {
            "native_connection_closed"
        } else {
            "native_truncated_frame"
        };
        self.close();
        code
    }

    pub fn close(&mut self) {
        self.ended = true;
        self.fragments.clear();
        self.buffered = 0;
    }
}
