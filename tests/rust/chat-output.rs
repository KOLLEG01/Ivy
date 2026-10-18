#[path = "../../services/chat-bridge/rust/src/whatsapp/output_chunks.rs"]
mod output_chunks;
use output_chunks::output_chunks;

#[test]
fn chunk_offsets_match_js_with_astral_and_unpaired_surrogates() {
    let mut text = vec![u16::from(b'a'); 3499];
    text.extend([0xd83d, 0xde00, 0xd800, u16::from(b'b'), 0xdc00]);
    let chunks: Vec<_> = output_chunks(&text).collect();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].offset, 0);
    assert_eq!(chunks[0].text.len(), 3501);
    assert_eq!(chunks[1].offset, 3500);
    assert_eq!(chunks[1].text, &[0xd800, u16::from(b'b'), 0xdc00]);
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
