//! WhatsApp's 3,500-code-point presentation limit. Offsets feed the existing
//! message identity and therefore count JS string iterator entries, not bytes
//! or UTF-16 code units. Lone surrogates remain individual entries.
pub const OUTPUT_CHUNK_CODE_POINTS: usize = 3_500;

#[derive(Debug, PartialEq, Eq)]
pub struct OutputChunk<'a> {
    pub offset: usize,
    pub text: &'a [u16],
}

pub fn output_chunks(text: &[u16]) -> impl Iterator<Item = OutputChunk<'_>> {
    let mut unit_offset = 0;
    let mut point_offset = 0;
    std::iter::from_fn(move || {
        if unit_offset == text.len() {
            return None;
        }
        let start = unit_offset;
        let offset = point_offset;
        for _ in 0..OUTPUT_CHUNK_CODE_POINTS {
            if unit_offset == text.len() {
                break;
            }
            let paired = (0xd800..=0xdbff).contains(&text[unit_offset])
                && text
                    .get(unit_offset + 1)
                    .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit));
            unit_offset += if paired { 2 } else { 1 };
            point_offset += 1;
        }
        Some(OutputChunk {
            offset,
            text: &text[start..unit_offset],
        })
    })
}
