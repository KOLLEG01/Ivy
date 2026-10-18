#[path = "../../services/dashboards/rust/src/png.rs"]
mod conversion;
#[path = "../../services/dashboards/rust/src/raster.rs"]
mod raster;
use raster::ColorMode;
use std::io::Cursor;

fn rgba_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(pixels).unwrap();
    writer.finish().unwrap();
    bytes
}

#[test]
fn png_codec_preserves_color_bytes_and_encodes_deterministic_gray8() {
    let input = rgba_png(
        4,
        2,
        &[
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 128, 255, 255,
            255, 128, 127, 127, 127, 255, 128, 128, 128, 255,
        ],
    );
    assert_eq!(
        conversion::convert(input.clone(), ColorMode::Color).unwrap(),
        input
    );
    for (mode, expected) in [
        (ColorMode::Grayscale, [54, 182, 18, 255, 127, 255, 127, 128]),
        (ColorMode::Monochrome, [0, 255, 0, 255, 0, 255, 0, 255]),
    ] {
        let bytes = conversion::convert(input.clone(), mode).unwrap();
        assert_eq!(conversion::convert(input.clone(), mode).unwrap(), bytes);
        let mut reader = png::Decoder::new(Cursor::new(&bytes)).read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (4, 2));
        assert_eq!(reader.info().bit_depth, png::BitDepth::Eight);
        assert_eq!(reader.info().color_type, png::ColorType::Grayscale);
        assert!(!reader.info().interlaced);
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut decoded).unwrap();
        assert_eq!(decoded, expected);
    }
}

#[test]
fn oversized_and_invalid_pngs_fail_before_unbounded_pixel_allocation() {
    assert!(matches!(
        conversion::convert(vec![0; conversion::MAX_PNG_BYTES + 1], ColorMode::Color),
        Err(conversion::PngError::TooLarge)
    ));
    assert!(matches!(
        conversion::convert(vec![0; 8], ColorMode::Grayscale),
        Err(conversion::PngError::Decode(_))
    ));
    // A CRC-valid oversized IHDR on a tiny stream: reject its dimensions before
    // trying to expand or allocate the advertised pixels.
    let mut header = rgba_png(1, 1, &[0, 0, 0, 255]);
    header[16..20].copy_from_slice(&4096u32.to_be_bytes());
    header[20..24].copy_from_slice(&4096u32.to_be_bytes());
    let mut crc = !0u32;
    for byte in &header[12..29] {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320u32 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    header[29..33].copy_from_slice(&(!crc).to_be_bytes());
    assert!(matches!(
        conversion::convert(header, ColorMode::Grayscale),
        Err(conversion::PngError::Dimensions)
    ));
}
