// Date arithmetic adapted from V8's date.cc, shipped in Node v24.18.0.
// Copyright 2012 the V8 project authors. See V8-LICENSE for redistribution terms.

const LIMIT: f64 = 8_640_000_000_000_000.0;
const DAY: i64 = 86_400_000;

fn add_local_seconds(utc_millis: f64, seconds: f64, zone: &tz::TimeZone) -> Option<f64> {
    // new Date(number) applies TimeClip before getSeconds/setSeconds.
    if !utc_millis.is_finite() || utc_millis.abs() > LIMIT || !seconds.is_finite() {
        return None;
    }
    let utc = utc_millis.trunc() as i64;
    let offset = i64::from(
        zone.find_local_time_type(utc.div_euclid(1000))
            .ok()?
            .ut_offset(),
    );
    let local = utc + offset * 1000;
    let time = local.rem_euclid(DAY);
    // Preserve MakeTime's conversion and operation order, including fractional
    // increments, overflow, and historical offsets containing seconds.
    let second = ((time / 1000) % 60) as f64 + seconds;
    let time = (time / 3_600_000) as f64 * 3_600_000.0
        + ((time / 60_000) % 60) as f64 * 60_000.0
        + second.trunc() * 1000.0
        + (time % 1000) as f64;
    let wall = local.div_euclid(DAY) as f64 * DAY as f64 + time;
    // V8 rejects this range before converting local wall time back to UTC.
    if !wall.is_finite() || wall.abs() > LIMIT + 30.0 * DAY as f64 {
        return None;
    }
    let offset = crate::date_parse::zone_offset(wall as i64, zone)?;
    let result = wall - offset as f64;
    (result.abs() <= LIMIT).then(|| result.trunc() + 0.0)
}

/// JavaScript `date.setSeconds(date.getSeconds() + seconds)` on a numeric date.
/// Uses the same TZ/system-zone policy as `parse_date`, including gap/fold rules.
/// The increment is already numeric; invalid dates or results return NaN.
pub fn date_add_local_seconds(utc_millis: f64, seconds: f64) -> f64 {
    crate::date_parse::with_runtime_zone(|zone| add_local_seconds(utc_millis, seconds, zone))
        .unwrap_or(f64::NAN)
}

/// Explicit IANA-zone variant, without changing process state. Unknown zones
/// return NaN. Milliseconds are retained; this is not elapsed UTC arithmetic.
pub fn date_add_local_seconds_in_zone(utc_millis: f64, seconds: f64, zone: &str) -> f64 {
    crate::date_parse::named_zone(zone)
        .and_then(|zone| add_local_seconds(utc_millis, seconds, &zone))
        .unwrap_or(f64::NAN)
}
