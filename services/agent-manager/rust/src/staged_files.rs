//! Message attachments retain the original cross-platform filename policy.
use ivy_contracts::JsString;

pub const MAXIMUM_BYTES: usize = 8 * 1024 * 1024;
pub const RETENTION_MS: f64 = 30.0 * 24.0 * 60.0 * 60.0 * 1000.0;

/// One path segment, keeping reserved names within the published UTF-16 cap.
pub fn staged_name(name: &JsString) -> JsString {
    let last = name
        .0
        .rsplit(|unit| matches!(*unit, 47 | 92))
        .next()
        .unwrap_or(&[]);
    let safe: Vec<_> = last
        .iter()
        .map(|unit| {
            if matches!(*unit, 0..=31 | 34 | 42 | 58 | 60 | 62 | 63 | 124) {
                95
            } else {
                *unit
            }
        })
        .collect();
    // The source class is the union of '.' and ECMAScript whitespace. Retain
    // the shared trim boundary, including BOM and excluding NEL.
    let edge = |unit: &u16| *unit == 46 || JsString(vec![*unit]).trim().0.is_empty();
    let start = safe
        .iter()
        .position(|unit| !edge(unit))
        .unwrap_or(safe.len());
    let end = safe
        .iter()
        .rposition(|unit| !edge(unit))
        .map_or(start, |index| index + 1);
    let mut units = safe[start..end]
        .iter()
        .take(128)
        .copied()
        .collect::<Vec<_>>();
    if units.is_empty() {
        return "file".into();
    }
    let first: Vec<_> = units
        .iter()
        .copied()
        .take_while(|unit| *unit != 46)
        .map(|unit| {
            if (65..=90).contains(&unit) {
                unit + 32
            } else {
                unit
            }
        })
        .collect();
    let reserved = ["con", "prn", "aux", "nul"]
        .iter()
        .any(|name| first.iter().copied().eq(name.encode_utf16()))
        || first.len() == 4
            && (first.starts_with(&[99, 111, 109]) || first.starts_with(&[108, 112, 116]))
            && (48..=57).contains(&first[3]);
    if reserved {
        units.insert(0, 95);
        units.truncate(128);
    }
    JsString(units)
}
