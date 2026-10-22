use super::JsString;

impl JsString {
    /// Trim Unicode whitespace using Rust's character classification.
    pub fn trim(&self) -> Self {
        let whitespace = |unit: &u16| char::from_u32(u32::from(*unit)).is_some_and(char::is_whitespace);
        let start = self.0.iter().position(|unit| !whitespace(unit)).unwrap_or(self.0.len());
        let end = self.0.iter().rposition(|unit| !whitespace(unit)).map_or(start, |index| index + 1);
        Self(self.0[start..end].to_vec())
    }
    /// Public object and asset names use NFC after Unicode validation.
    pub fn normalize_nfc(&self) -> Self {
        match self.to_utf8() {
            Ok(value) => icu_normalizer::ComposingNormalizer::new_nfc().normalize(&value).into_owned().into(),
            Err(_) => self.clone(),
        }
    }
}
