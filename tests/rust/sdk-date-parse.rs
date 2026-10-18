use ivy_contracts::json::parse;
use ivy_sdk::{
    JsString,
    clock::{parse_date, parse_date_in_zone},
};
use std::process::Command;

#[test]
fn date_parse_matches_node() {
    // Separate Node processes keep timezone mutation outside the Rust test process.
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for zone in [
        "UTC",
        "Europe/Berlin",
        "America/New_York",
        "Australia/Lord_Howe",
        "Pacific/Apia",
        "Asia/Kathmandu",
    ] {
        let output = Command::new("node")
            .arg("tests/fixtures/date-parse-oracle.mjs")
            .env("TZ", zone)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows = parse(&String::from_utf8(output.stdout).unwrap(), usize::MAX).unwrap();
        assert!(rows.as_array().unwrap().len() > 2000);
        for row in rows.as_array().unwrap() {
            let value = row.get("value").unwrap().as_js_string().unwrap();
            let actual = parse_date_in_zone(value, zone);
            match row.get("expected").unwrap().as_f64() {
                Some(expected) => assert_eq!(actual, expected, "zone={zone} input={value:?}"),
                None => assert!(
                    actual.is_nan(),
                    "zone={zone} input={value:?} actual={actual}"
                ),
            }
        }
    }
    // Default system-zone path, including the native runtime's environment policy.
    for value in [
        "2026-01-01T00:00:00",
        "2026-03-29T02:30:00",
        "2026-10-25T02:30:00",
        "2026-02-30T00:00:00Z",
    ] {
        let output = Command::new("node")
            .args(["-p", "JSON.stringify(Date.parse(process.argv[1]))", value])
            .output()
            .unwrap();
        let expected = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<f64>()
            .unwrap();
        assert_eq!(
            parse_date(&JsString::from(value)),
            expected,
            "system zone {value}"
        );
    }
    for zone in [
        "UTC",
        "Europe/Berlin",
        "America/New_York",
        "Australia/Lord_Howe",
        "Pacific/Apia",
        "Asia/Kathmandu",
        "",
        "unknown",
        "GMT+05:30",
        "GMT-8",
        "GMT+0530",
        "UTC+01:00",
        "europe/berlin",
        "EST5EDT",
        "CET",
        "US/Eastern",
        "ACT",
        "AET",
        "AGT",
        "ART",
        "AST",
        "BET",
        "BST",
        "CAT",
        "CNT",
        "CST",
        "CTT",
        "EAT",
        "ECT",
        "IET",
        "IST",
        "JST",
        "MIT",
        "NET",
        "NST",
        "PLT",
        "PNT",
        "PRT",
        "PST",
        "SST",
        "VST",
        "gmt+5:30",
        "GMT+٠٥:٣٠",
        "GMT+５:３０",
        "GMT+123456",
        "GMT+1234567",
        "GMT+000000",
        "GMT+5:30:15",
        "GMT+24",
        "GMT+23:60",
        "GMT+1:01:60",
    ] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime_zone_child", "--nocapture"])
            .env("IVY_DATE_ZONE_CASE", "1")
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
fn runtime_zone_child() {
    if std::env::var_os("IVY_DATE_ZONE_CASE").is_none() {
        return;
    }
    for value in [
        "2026-01-01T00:00:00",
        "2026-07-01T00:00:00",
        "2026-03-29T02:30:00",
        "2026-10-25T02:30:00",
    ] {
        let output = Command::new("node")
            .args(["-p", "JSON.stringify(Date.parse(process.argv[1]))", value])
            .output()
            .unwrap();
        assert!(output.status.success());
        let expected = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<f64>()
            .unwrap();
        assert_eq!(
            parse_date(&JsString::from(value)),
            expected,
            "input={value}"
        );
    }
}
