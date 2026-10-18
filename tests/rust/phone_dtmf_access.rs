use ivy_phone_bridge::dtmf_access::{FailureLimiter, LimitError};
use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn access_limiter_matches_original_native_policy_and_clock_boundaries() {
    // Rows: record/check, UTC ticks, maximum failures, window ticks.
    let mut scenarios: Vec<Vec<[i64; 4]>> = vec![
        vec![
            [0, 100, 2, 10],
            [1, 100, 2, 10],
            [1, 100, 2, 10],
            [0, 109, 2, 10],
            [0, 110, 2, 10],
        ],
        // Above-limit failures remain when policy changes; checks never record.
        vec![
            [1, 100, 1, 10],
            [1, 101, 1, 10],
            [1, 102, 1, 10],
            [0, 103, 3, 10],
            [0, 110, 2, 10],
            [0, 112, 1, 10],
            [0, 112, 1, 10],
        ],
        // A backwards clock does not sort the FIFO, or clamp retry to window.
        vec![
            [1, 100, 1, 20],
            [1, 90, 1, 20],
            [0, 80, 1, 20],
            [0, 110, 2, 20],
            [0, 120, 1, 20],
        ],
        // Invalid policies must not prune or append, including far-future calls.
        vec![
            [1, 100, 1, 10],
            [0, 1000, 0, 10],
            [1, 1000, -1, 10],
            [1, 101, 1, 0],
            [0, 101, 1, -1],
            [0, 101, 1, 10],
            [0, 102, 0, 0],
            [0, 102, 1, 10],
        ],
        // Shrinking the window permanently prunes; extending cannot resurrect.
        vec![
            [1, 100, 1, 50],
            [1, 110, 1, 50],
            [0, 120, 1, 10],
            [0, 120, 1, 100],
        ],
        vec![
            [0, 0, 1, 1],
            [1, 0, 1, 1],
            [0, 1, 1, 1],
            [1, 1, 1, 1],
            [0, 1, 1, 1],
            [0, 2, 1, 1],
        ],
        vec![
            [1, 3_155_378_975_999_999_999, 1, 1],
            [0, 3_155_378_975_999_999_999, 1, 1],
            [0, 3_155_378_975_999_999_999, 2, 1],
        ],
        vec![
            [0, -1, 0, 0],
            [0, i64::MAX, 1, 1],
            [0, 100, 1, i64::MAX],
            [0, 100, 1, i64::MIN],
            [0, 100, i32::MAX as i64, 1],
        ],
    ];
    let mut seed = 0x7193_u32;
    let mut random = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 8) as i64
    };
    let mut now = 10_000;
    scenarios.push(
        (0..500)
            .map(|_| {
                now += random() % 17 - 7;
                [random() % 2, now, random() % 8, random() % 35 - 1]
            })
            .collect(),
    );
    let input = format!(
        "[{}]",
        scenarios
            .iter()
            .map(|scenario| format!(
                "[{}]",
                scenario
                    .iter()
                    .map(|a| format!("[{},{},{},{}]", a[0], a[1], a[2], a[3]))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut expected = Vec::new();
    for scenario in &scenarios {
        let mut limiter = FailureLimiter::default();
        for &[record, now, maximum, window] in scenario {
            let result = if record == 1 {
                limiter
                    .record_failure(now, maximum as i32, window)
                    .map(|()| "ok".to_owned())
            } else {
                limiter.check(now, maximum as i32, window).map(|d| {
                    format!(
                        "{}:{}",
                        if d.allowed { "allow" } else { "deny" },
                        d.retry_after_ticks
                    )
                })
            };
            expected.push(result.unwrap_or_else(|error| {
                format!(
                    "error:{}",
                    match error {
                        LimitError::MaximumFailures => "maximum",
                        LimitError::Window => "window",
                        LimitError::TimestampOutOfRange => "timestamp",
                    }
                )
            }));
        }
    }
    assert_eq!(
        &expected[..5],
        &["allow:0", "ok", "ok", "deny:1", "allow:0"]
    );
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-dtmf-access-oracle.ps1");
    let mut child = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(fixture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("PowerShell 7 is required for the original C# oracle");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual = String::from_utf8(output.stdout).unwrap();
    assert_eq!(actual.lines().collect::<Vec<_>>(), expected);
}
