use crate::raster::{ColorMode, gray_pixel};
use std::io::{Cursor, Write};

pub const MAX_PIXELS: u64 = 8_000_000;
pub const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub enum PngError {
    Decode(png::DecodingError),
    Encode(png::EncodingError),
    Dimensions,
    Format,
    TooLarge,
}
impl std::fmt::Display for PngError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(error) => write!(f, "PNG decode failed: {error}"),
            Self::Encode(error) => write!(f, "PNG encode failed: {error}"),
            Self::Dimensions => f.write_str("PNG dimensions exceed the render limit."),
            Self::Format => f.write_str("Unsupported screenshot PNG format."),
            Self::TooLarge => f.write_str("PNG exceeds 8 MiB; reduce dimensions."),
        }
    }
}
impl std::error::Error for PngError {}

/// Chromium emits 8-bit RGB/RGBA PNGs. Expansion also accepts palette and
/// grayscale inputs used by the compatibility corpus. Memory is bounded before
/// allocation; the output is always noninterlaced Gray8 for non-color modes.
pub fn convert(bytes: Vec<u8>, mode: ColorMode) -> Result<Vec<u8>, PngError> {
    if mode == ColorMode::Color {
        return bounded(bytes);
    }
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: (MAX_PIXELS * 8) as usize,
    });
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(PngError::Decode)?;
    let width = reader.info().width;
    let height = reader.info().height;
    let count = u64::from(width) * u64::from(height);
    if count == 0 || count > MAX_PIXELS {
        return Err(PngError::Dimensions);
    }
    // Browser screenshots are noninterlaced. Reject Adam7 rather than allocating
    // an entire RGBA frame just to recover rows in display order.
    if reader.info().interlaced {
        return Err(PngError::Format);
    }
    let (color, depth) = reader.output_color_type();
    if depth != png::BitDepth::Eight {
        return Err(PngError::Format);
    }
    let samples = color.samples();
    let mut result = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut result, width, height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(PngError::Encode)?;
        let mut stream = writer
            .stream_writer_with_size((width as usize).min(8192))
            .map_err(PngError::Encode)?;
        let mut gray = vec![0; width as usize];
        while let Some(row) = reader.next_row().map_err(PngError::Decode)? {
            for (target, p) in gray.iter_mut().zip(row.data().chunks_exact(samples)) {
                let pixel = match color {
                    png::ColorType::Rgb => [p[0], p[1], p[2], 255],
                    png::ColorType::Rgba => [p[0], p[1], p[2], p[3]],
                    png::ColorType::Grayscale => [p[0], p[0], p[0], 255],
                    png::ColorType::GrayscaleAlpha => [p[0], p[0], p[0], p[1]],
                    _ => return Err(PngError::Format),
                };
                *target = gray_pixel(pixel, mode);
            }
            stream.write_all(&gray).map_err(|_| PngError::TooLarge)?;
        }
        stream.finish().map_err(PngError::Encode)?;
        writer.finish().map_err(PngError::Encode)?;
    }
    bounded(result)
}

fn bounded(bytes: Vec<u8>) -> Result<Vec<u8>, PngError> {
    if bytes.len() > MAX_PNG_BYTES {
        Err(PngError::TooLarge)
    } else {
        Ok(bytes)
    }
}
