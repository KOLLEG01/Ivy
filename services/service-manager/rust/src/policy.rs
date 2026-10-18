//! Pure policy shared by manager reconciliation and its boundary tests.

/// Age is signed so a clock in the future cannot accidentally appear fresh.
pub fn executor_fresh(now_ms: i64, observed_ms: i64) -> bool {
    let age = i128::from(now_ms) - i128::from(observed_ms);
    (-1_000..15_000).contains(&age)
}
