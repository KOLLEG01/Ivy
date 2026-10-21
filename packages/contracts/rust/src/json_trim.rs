use super::JsString;

pub(super) fn space(unit: u16) -> bool {
    matches!(unit, 0x9..=0xd | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

impl JsString {
    /// ECMAScript trim whitespace at both ends without decoding or replacing units.
    pub fn trim(&self) -> Self {
        let start = self
            .0
            .iter()
            .position(|unit| !space(*unit))
            .unwrap_or(self.0.len());
        let end = self
            .0
            .iter()
            .rposition(|unit| !space(*unit))
            .map_or(start, |index| index + 1);
        Self(self.0[start..end].to_vec())
    }
}
