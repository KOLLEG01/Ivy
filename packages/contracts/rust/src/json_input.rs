use super::JsString;
use std::borrow::Cow;

/// Both public parse boundaries use the same grammar and iterative value stack.
/// UTF-8 callers keep their borrowed input; JS callers retain raw UTF-16 units.
pub(super) enum Input<'a> {
    Utf8(&'a str),
    Utf16(&'a JsString),
}
impl Input<'_> {
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Utf8(s) => s.len(),
            Self::Utf16(s) => s.0.len(),
        }
    }
    pub(super) fn token(&self, at: usize) -> Option<u8> {
        match self {
            Self::Utf8(s) => s.as_bytes().get(at).copied(),
            // Grammar tokens are ASCII. Non-ASCII units are read losslessly by
            // string_unit or by the diagnostic renderer, never narrowed here.
            Self::Utf16(s) => s.0.get(at).map(|c| if *c < 128 { *c as u8 } else { 128 }),
        }
    }
    pub(super) fn string_unit(&self, at: &mut usize, out: &mut Vec<u16>) {
        match self {
            Self::Utf8(s) => {
                let c = s[*at..].chars().next().unwrap();
                *at += c.len_utf8();
                let mut units = [0; 2];
                out.extend_from_slice(c.encode_utf16(&mut units));
            }
            Self::Utf16(s) => {
                out.push(s.0[*at]);
                *at += 1;
            }
        }
    }
    pub(super) fn number(&self, start: usize, end: usize) -> Cow<'_, str> {
        match self {
            Self::Utf8(s) => Cow::Borrowed(&s[start..end]),
            // The number scanner has admitted only ASCII before this boundary.
            Self::Utf16(s) => {
                Cow::Owned(s.0[start..end].iter().map(|c| *c as u8 as char).collect())
            }
        }
    }
}
