#[path = "../../services/chat-bridge/rust/src/whatsapp/output_chunks.rs"]
mod output_chunks;
use output_chunks::output_chunks;

#[test]
fn chunk_offsets_preserve_wire_text_and_astral_characters() {
    let mut text: Vec<_> = "a".repeat(3499).encode_utf16().collect();
    text.extend("😀世界".encode_utf16());
    text.extend([0xd800, 0xdc00, 0xd800, 0xdc00, 0xdfff]);
    let chunks: Vec<_> = output_chunks(&text).collect();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].offset, 0);
    assert_eq!(
        char::decode_utf16(chunks[0].text.iter().copied()).count(),
        3500
    );
    assert_eq!(chunks[1].offset, 3500);
    assert_eq!(
        chunks
            .iter()
            .flat_map(|chunk| chunk.text.iter().copied())
            .collect::<Vec<_>>(),
        text
    );
}

#[test]
fn empty_output_has_no_ids_and_exact_multiple_has_no_empty_tail() {
    assert_eq!(output_chunks(&[]).count(), 0);
    let text = vec![u16::from(b'a'); 7000];
    let chunks: Vec<_> = output_chunks(&text).collect();
    assert_eq!(
        chunks.iter().map(|chunk| chunk.offset).collect::<Vec<_>>(),
        [0, 3500]
    );
    assert!(chunks.iter().all(|chunk| chunk.text.len() == 3500));
}
