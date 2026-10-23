use ivy_sdk::{
    JsString,
    clock::{date_to_iso, parse_date},
};
use std::process::Command;

#[test]
fn timestamp_formats_resolve_to_the_same_instant() {
    for (value, expected) in [
        ("1970-01-01T00:00:00Z", 0_i64),
        ("1970-01-01T01:30:00+01:30", 0),
        ("1969-12-31T16:00:00-08:00", 0),
        ("1969-12-31T23:59:59.999999Z", -1),
        ("2000-01-01T00:00:00.123456789Z", 946_684_800_123),
        ("2000-01-01T02:30:00+02:30", 946_684_800_000),
        ("Sat, 01 Jan 2000 00:00:00 +0000", 946_684_800_000),
        ("Fri, 31 Dec 1999 19:00:00 -0500", 946_684_800_000),
        ("2000-01-01", 946_684_800_000),
        ("2000-02-29", 951_782_400_000),
    ] {
        assert_eq!(parse_date(&value.into()), expected as f64, "{value}");
    }
}

#[test]
fn canonical_extended_timestamps_preserve_the_public_range() {
    for (value, expected) in [
        ("-271821-04-20T00:00:00.000Z", -8_640_000_000_000_000_i64),
        ("-000001-01-01T00:00:00.000Z", -62_198_755_200_000),
        ("0000-01-01T00:00:00.000Z", -62_167_219_200_000),
        ("9999-12-31T23:59:59.999Z", 253_402_300_799_999),
        ("+010000-01-01T00:00:00.000Z", 253_402_300_800_000),
        ("+275760-09-13T00:00:00.000Z", 8_640_000_000_000_000),
    ] {
        let at = parse_date(&value.into());
        assert_eq!(at, expected as f64, "{value}");
        assert_eq!(date_to_iso(at).unwrap(), JsString::from(value));
    }
    for value in ["-271821-04-19T23:59:59.999Z", "+275760-09-13T00:00:00.001Z"] {
        assert!(parse_date(&value.into()).is_nan(), "{value}");
    }
}

#[test]
fn malformed_dates_and_runtime_only_grammar_are_rejected() {
    for value in [
        "",
        "0",
        "1",
        "2026",
        "March 1, 2026 3:30 pm",
        "2026-01-01T00:00:00",
        "2026-02-29T00:00:00Z",
        "2026-02-30T00:00:00Z",
        "2026-01-01T25:00:00Z",
        "2026-01-01T00:00:00+24:00",
        "2026-01-01T00:00:00Z (ignored comment)",
        "2026-01-01T00:00:00Z\0ignored suffix",
        "not a timestamp",
    ] {
        assert!(parse_date(&value.into()).is_nan(), "{value}");
    }
    assert!(parse_date(&JsString(vec![0xd800])).is_nan());
}

#[test]
fn timestamp_parsing_is_independent_of_process_timezone() {
    for zone in ["UTC", "Europe/Berlin", "America/New_York", "unknown"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "timestamp_zone_child", "--nocapture"])
            .env("IVY_TIMESTAMP_ZONE_CASE", "1")
            .env("TZ", zone)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "TZ={zone}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn timestamp_zone_child() {
    if std::env::var_os("IVY_TIMESTAMP_ZONE_CASE").is_none() {
        return;
    }
    assert_eq!(parse_date(&"1970-01-01".into()), 0.0);
    assert_eq!(parse_date(&"1970-01-01T01:00:00+01:00".into()), 0.0);
    assert!(parse_date(&"2026-10-25T02:30:00".into()).is_nan());
}
