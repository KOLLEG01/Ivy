//! Pixel conversion shared by the PNG codec and its differential fixtures.
//! Deliberately no color management, dithering, or integer approximation.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorMode {
    Color,
    Grayscale,
    Monochrome,
}

/// Composite straight-alpha sRGB bytes onto white in the same f64 operation order
/// as the existing browser renderer, then apply JavaScript Math.round semantics.
pub fn gray_pixel(rgba: [u8; 4], mode: ColorMode) -> u8 {
    let alpha = f64::from(rgba[3]) / 255.0;
    let luminance =
        ((0.2126 * f64::from(rgba[0]) + 0.7152 * f64::from(rgba[1]) + 0.0722 * f64::from(rgba[2]))
            * alpha
            + 255.0 * (1.0 - alpha)
            + 0.5)
            .floor() as u8;
    match mode {
        ColorMode::Monochrome => {
            if luminance >= 128 {
                255
            } else {
                0
            }
        }
        _ => luminance,
    }
}
