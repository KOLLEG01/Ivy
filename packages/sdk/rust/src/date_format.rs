use crate::{IvyError, IvyResult, JsString};

/// Equivalent to `new Date(utc_millis).toISOString()` for a JavaScript number.
/// Applies TimeClip before formatting the full ECMAScript UTC calendar range.
/// Invalid dates return `invalid_arguments` with the native "Invalid time value" message.
pub fn date_to_iso(utc_millis: f64) -> IvyResult<JsString> {
    if !utc_millis.is_finite() || utc_millis.abs() > 8_640_000_000_000_000.0 {
        return Err(IvyError::new("invalid_arguments", "Invalid time value"));
    }
    let millis = utc_millis.trunc() as i64;
    let days = millis.div_euclid(86_400_000);
    let time = millis.rem_euclid(86_400_000);

    // Invert the Gregorian civil-day calculation using 400-year eras. Euclidean
    // division keeps dates before the epoch (including year zero) in the same calendar.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = march_month + if march_month < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let year = if (0..=9_999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{year:+07}")
    };
    Ok(format!(
        "{year}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3_600_000,
        (time / 60_000) % 60,
        (time / 1_000) % 60,
        time % 1_000
    )
    .into())
}
