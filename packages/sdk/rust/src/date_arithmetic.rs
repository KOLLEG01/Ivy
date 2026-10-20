// Date arithmetic adapted from V8's date.cc, shipped in Node v24.18.0.
// Copyright 2012 the V8 project authors. See V8-LICENSE for redistribution terms.

/// JavaScript Date.UTC for seven already converted numeric arguments.
/// Month is zero-based; callers supply the JS defaults (month/time 0, day 1)
/// for omitted arguments. Invalid inputs return NaN, independently of time zone.
pub fn date_utc(
    year: f64,
    month: f64,
    day: f64,
    hour: f64,
    minute: f64,
    second: f64,
    millisecond: f64,
) -> f64 {
    if ![year, month, day, hour, minute, second, millisecond]
        .into_iter()
        .all(f64::is_finite)
    {
        return f64::NAN;
    }
    let integer_year = year.trunc();
    let year = if (0.0..=99.0).contains(&integer_year) {
        1900.0 + integer_year
    } else {
        year
    };
    // Retain the pinned runtime's MakeDay bounds before month normalization,
    // including inputs whose enormous components would otherwise cancel out.
    if !(-1_000_000.0..=1_000_000.0).contains(&year)
        || !(-10_000_000.0..=10_000_000.0).contains(&month)
    {
        return f64::NAN;
    }
    let month = month as i64;
    let year = year as i64 + month.div_euclid(12);
    let month = month.rem_euclid(12) as usize;
    let shifted = year + 399_999;
    let base = 1970 + 399_999;
    let mut days = 365 * shifted + shifted / 4 - shifted / 100 + shifted / 400
        - (365 * base + base / 4 - base / 100 + base / 400);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    days += [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334][month];
    days += i64::from(leap && month >= 2);
    let time = hour.trunc() * 3_600_000.0
        + minute.trunc() * 60_000.0
        + second.trunc() * 1_000.0
        + millisecond.trunc();
    let result = time + ((days - 1) as f64 + day.trunc()) * 86_400_000.0;
    if result.abs() <= 8_640_000_000_000_000.0 {
        result.trunc() + 0.0
    } else {
        f64::NAN
    }
}
