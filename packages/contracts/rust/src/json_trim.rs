use super::JsString;

impl JsString {
    /// Trim Unicode whitespace using Rust's character classification.
    pub fn trim(&self) -> Self {
        let whitespace =
            |unit: &u16| char::from_u32(u32::from(*unit)).is_some_and(char::is_whitespace);
        let start = self
            .0
            .iter()
            .position(|unit| !whitespace(unit))
            .unwrap_or(self.0.len());
        let end = self
            .0
            .iter()
            .rposition(|unit| !whitespace(unit))
            .map_or(start, |index| index + 1);
        Self(self.0[start..end].to_vec())
    }
    /// ECMAScript WhiteSpace and LineTerminator, preserving UTF-16 code units.
    pub fn trim_ecmascript(&self) -> Self {
        Self(Self::trim_ecmascript_units(&self.0).to_vec())
    }
    /// Borrow the trimmed code units when the caller needs no owned string.
    pub fn trim_ecmascript_units(units: &[u16]) -> &[u16] {
        let whitespace = |unit: &u16| {
            matches!(unit,
            0x9..=0xd | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a |
            0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
        };
        let start = units
            .iter()
            .position(|unit| !whitespace(unit))
            .unwrap_or(units.len());
        let end = units
            .iter()
            .rposition(|unit| !whitespace(unit))
            .map_or(start, |index| index + 1);
        &units[start..end]
    }
    /// Public object and asset names use NFC after Unicode validation.
    pub fn normalize_nfc(&self) -> Self {
        match self.to_utf8() {
            Ok(value) => icu_normalizer::ComposingNormalizer::new_nfc()
                .normalize(&value)
                .into_owned()
                .into(),
            Err(_) => self.clone(),
        }
    }
}
