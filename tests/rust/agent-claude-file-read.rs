use super::*;
use base64::Engine;
use std::{cell::Cell, io::Cursor, rc::Rc};

struct Counted<R> {
    input: R,
    bytes: Rc<Cell<usize>>,
    interrupt: bool,
}
impl<R: Read> Read for Counted<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if std::mem::take(&mut self.interrupt) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let length = bytes.len().min(1);
        let count = self.input.read(&mut bytes[..length])?;
        self.bytes.set(self.bytes.get() + count);
        Ok(count)
    }
}

#[test]
fn oversized_metadata_is_refused_before_reading() {
    struct MustNotRead;
    impl Read for MustNotRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("Oversized file was read before admission");
        }
    }
    let error = encode_base64(MustNotRead, u64::MAX, 3).unwrap_err();
    assert_eq!(error.name, "native_capacity");
}

#[test]
fn short_and_interrupted_reads_preserve_padding_and_bound_final_capacity() {
    for length in [0usize, 1, 2, 3, 4, 5, 6, 7, 8, 9, 3_073] {
        for metadata in [0, length] {
            let bytes: Vec<u8> = (0..length).map(|i| i as u8).collect();
            let count = Rc::new(Cell::new(0));
            let input = Counted {
                input: Cursor::new(&bytes),
                bytes: count.clone(),
                interrupt: true,
            };
            let maximum_bytes = length.div_ceil(3) * 3;
            let encoded = encode_base64(input, metadata as u64, maximum_bytes).unwrap();
            assert_eq!(encoded, JsString::from(STANDARD.encode(bytes)));
            assert!(encoded.0.capacity() <= maximum_bytes / 3 * 4);
            assert_eq!(count.get(), length);
        }
    }
}

#[test]
fn unknown_or_growing_inputs_stop_after_the_budget_and_one_probe() {
    for maximum_bytes in [0, 3, 9, 3_075] {
        for metadata in [0, maximum_bytes] {
            let count = Rc::new(Cell::new(0));
            let input = Counted {
                input: io::repeat(0),
                bytes: count.clone(),
                interrupt: true,
            };
            let error = encode_base64(input, metadata as u64, maximum_bytes).unwrap_err();
            assert_eq!(error.name, "native_capacity");
            assert_eq!(count.get(), maximum_bytes + 1);
        }
    }
}

#[test]
fn input_error_never_returns_a_partial_success() {
    struct Failed;
    impl Read for Failed {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "original file error",
            ))
        }
    }
    let error = encode_base64(Cursor::new([1, 2]).chain(Failed), 0, 3).unwrap_err();
    assert_eq!(error.name, "io_error");
    assert_eq!(error.message, JsString::from("original file error"));
}
