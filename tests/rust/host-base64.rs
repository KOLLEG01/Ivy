use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{JsString, hash::digest};
use ivy_host_runtime::base64_content::{self, Base64Content, CHUNK_CHARS, DecodeError};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

thread_local! { static ALLOCATIONS: Cell<(bool, usize, usize)> = const { Cell::new((false, 0, 0)) }; }
struct Counted;
fn record(size: usize) {
    let _ = ALLOCATIONS.try_with(|state| {
        let (active, largest, total) = state.get();
        if active {
            state.set((true, largest.max(size), total + size));
        }
    });
}
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            record(size);
        }
        next
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;
fn measured<T>(operation: impl FnOnce() -> T) -> (T, usize, usize) {
    ALLOCATIONS.with(|state| state.set((true, 0, 0)));
    let value = operation();
    let (_, largest, total) = ALLOCATIONS.with(|state| state.replace((false, 0, 0)));
    (value, largest, total)
}

#[test]
fn canonical_content_keeps_exact_bytes_padding_and_digest_across_chunk_boundaries() {
    for length in [0, 1, 2, 3, 4, 257, 12287, 12288, 12289, 32769] {
        let bytes = (0..length)
            .map(|n: usize| (n.wrapping_mul(37) & 255) as u8)
            .collect::<Vec<_>>();
        let encoded = JsString::from(STANDARD.encode(&bytes));
        let content = Base64Content::inspect(&encoded, length).unwrap();
        assert_eq!(content.encoded(), &encoded);
        assert_eq!(content.byte_length(), length);
        assert_eq!(content.content_hash(), digest(&bytes));
        assert_eq!(base64_content::decode(&encoded, length).unwrap(), bytes);
    }
    for bad in [
        "A", "====", "Zg", "Zh==", "Zm9=", "Zg===", "Zg==\n", "AA-_", "Zg==AAAA",
    ] {
        assert_eq!(
            base64_content::decode(&bad.into(), 100),
            Err(DecodeError::Invalid),
            "{bad:?}"
        );
    }
    for bad in [JsString(vec![0xd800]), JsString(vec![0x141, 65, 65, 65])] {
        assert_eq!(base64_content::decode(&bad, 100), Err(DecodeError::Invalid));
    }
    let padded_middle = JsString::from(STANDARD.encode(vec![0; CHUNK_CHARS / 4 * 3 - 1]) + "AAAA");
    assert_eq!(
        base64_content::decode(&padded_middle, usize::MAX),
        Err(DecodeError::Invalid)
    );
}

#[test]
fn maximum_upload_validation_and_fingerprint_keep_only_bounded_working_bytes() {
    const MAXIMUM: usize = 8 * 1024 * 1024;
    let bytes = vec![0xfb; MAXIMUM];
    let encoded = JsString::from(STANDARD.encode(&bytes));
    let ((length, largest_chunk), largest_allocation, total_allocation) = measured(|| {
        let mut largest = 0;
        let length = base64_content::visit(&encoded, MAXIMUM, |chunk| {
            largest = largest.max(chunk.len())
        })
        .unwrap();
        (length, largest)
    });
    assert_eq!(length, MAXIMUM);
    assert!(largest_chunk <= CHUNK_CHARS / 4 * 3);
    assert_eq!((largest_allocation, total_allocation), (0, 0));
    let (content, largest, total) = measured(|| Base64Content::inspect(&encoded, MAXIMUM).unwrap());
    assert_eq!(content.content_hash(), digest(&bytes));
    assert!(
        largest < 4096 && total < 4096,
        "largest={largest}, total={total}"
    );
    let (decoded, largest, total) = measured(|| base64_content::decode(&encoded, MAXIMUM).unwrap());
    assert_eq!(decoded, bytes);
    assert_eq!(largest, MAXIMUM);
    assert_eq!(
        total, MAXIMUM,
        "Only the required output buffer is allocated."
    );
}

#[test]
fn oversized_content_has_no_output_allocation_and_keeps_malformed_input_error_order() {
    let encoded = JsString::from(STANDARD.encode(vec![0; 65537]));
    let (result, largest, total) = measured(|| base64_content::decode(&encoded, 65536));
    assert_eq!(result, Err(DecodeError::TooLarge));
    assert_eq!((largest, total), (0, 0));
    let mut malformed = encoded.clone();
    malformed.0[CHUNK_CHARS + 1] = b'!' as u16;
    assert_eq!(
        base64_content::decode(&malformed, 65536),
        Err(DecodeError::Invalid)
    );
    let mut visits = 0;
    assert_eq!(
        base64_content::visit(&encoded, 65536, |_| visits += 1),
        Err(DecodeError::TooLarge)
    );
    assert_eq!(visits, 0);
}
