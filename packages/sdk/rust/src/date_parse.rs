use crate::{JsString, native_evidence::native_instant};
use chrono::{DateTime, NaiveDate};

/// Read a timestamp with an explicit offset, an RFC 2822 date, or a calendar date
/// at midnight UTC. Invalid input returns NaN for the existing numeric callers.
/// Parsing never consults the process's local time zone.
pub fn parse_date(value: &JsString) -> f64 {
    let Ok(text) = value.to_utf8() else {
        return f64::NAN;
    };
    DateTime::parse_from_rfc3339(&text)
        .map(|date| date.timestamp_millis())
        .or_else(|_| DateTime::parse_from_rfc2822(&text).map(|date| date.timestamp_millis()))
        .or_else(|_| {
            NaiveDate::parse_from_str(&text, "%Y-%m-%d").map(|date| {
                date.and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp_millis()
            })
        })
        .ok()
        // Canonical protocol timestamps include years beyond chrono's range.
        .or_else(|| native_instant(value, None).ok())
        .map_or(f64::NAN, |millis| millis as f64)
}
