//! The persisted wire-text limit and message offsets use Unicode code points.
//! Rust's decoder finds boundaries while the original encoded units stay intact.
pub const OUTPUT_CHUNK_CODE_POINTS: usize = 3_500;

#[derive(Debug, PartialEq, Eq)]
pub struct OutputChunk<'a> {
    pub offset: usize,
    pub text: &'a [u16],
}

pub fn output_chunks(text: &[u16]) -> impl Iterator<Item = OutputChunk<'_>> {
    let mut characters = char::decode_utf16(text.iter().copied());
    let mut unit_offset = 0;
    let mut point_offset = 0;
    std::iter::from_fn(move || {
        let length: usize = characters
            .by_ref()
            .take(OUTPUT_CHUNK_CODE_POINTS)
            .map(|value| value.map_or(1, char::len_utf16))
            .sum();
        if length == 0 {
            return None;
        }
        let chunk = OutputChunk {
            offset: point_offset,
            text: &text[unit_offset..unit_offset + length],
        };
        unit_offset += length;
        point_offset += OUTPUT_CHUNK_CODE_POINTS;
        Some(chunk)
    })
}
