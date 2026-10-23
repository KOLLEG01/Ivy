use ivy_phone_bridge::dtmf_access::{FailureLimiter, LimitError};

fn check(
    limiter: &mut FailureLimiter,
    now: i64,
    maximum: i32,
    window: i64,
    allowed: bool,
    retry: i64,
) {
    let result = limiter.check(now, maximum, window).unwrap();
    assert_eq!((result.allowed, result.retry_after_ticks), (allowed, retry));
}
#[test]
fn access_failure_expiry_threshold_changes_and_backwards_clocks_keep_call_admission_bounded() {
    let mut limiter = FailureLimiter::default();
    check(&mut limiter, 100, 2, 10, true, 0);
    limiter.record_failure(100, 2, 10).unwrap();
    limiter.record_failure(100, 2, 10).unwrap();
    check(&mut limiter, 109, 2, 10, false, 1);
    check(&mut limiter, 110, 2, 10, true, 0);
    for now in [100, 101, 102] {
        limiter.record_failure(now, 1, 10).unwrap();
    }
    check(&mut limiter, 103, 3, 10, false, 7);
    check(&mut limiter, 110, 2, 10, false, 1);
    check(&mut limiter, 112, 1, 10, true, 0);
    check(&mut limiter, 112, 1, 10, true, 0);
    let mut backwards = FailureLimiter::default();
    backwards.record_failure(100, 1, 20).unwrap();
    backwards.record_failure(90, 1, 20).unwrap();
    check(&mut backwards, 80, 1, 20, false, 40);
    check(&mut backwards, 110, 2, 20, false, 10);
    check(&mut backwards, 120, 1, 20, true, 0);
}
#[test]
fn invalid_access_policy_and_timestamp_never_prune_or_record_failures() {
    let mut limiter = FailureLimiter::default();
    limiter.record_failure(100, 1, 10).unwrap();
    for (maximum, window, error) in [
        (0, 10, LimitError::MaximumFailures),
        (-1, 10, LimitError::MaximumFailures),
        (1, 0, LimitError::Window),
        (1, -1, LimitError::Window),
    ] {
        assert_eq!(limiter.record_failure(1000, maximum, window), Err(error));
    }
    check(&mut limiter, 101, 1, 10, false, 9);
    for now in [-1, i64::MAX] {
        assert_eq!(
            limiter.check(now, 1, 1),
            Err(LimitError::TimestampOutOfRange)
        );
    }
    assert_eq!(
        limiter.check(100, 1, i64::MAX),
        Err(LimitError::TimestampOutOfRange)
    );
    assert_eq!(limiter.check(100, 1, i64::MIN), Err(LimitError::Window));
    check(&mut limiter, 102, 1, 10, false, 8);
    let mut shrinking = FailureLimiter::default();
    for now in [100, 110] {
        shrinking.record_failure(now, 1, 50).unwrap();
    }
    check(&mut shrinking, 120, 1, 10, true, 0);
    check(&mut shrinking, 120, 1, 100, true, 0);
}
#[test]
fn access_timestamp_edges_refuse_negative_cutoffs_and_expiry_overflow() {
    let mut limiter = FailureLimiter::default();
    assert_eq!(limiter.check(0, 1, 1), Err(LimitError::TimestampOutOfRange));
    limiter.record_failure(1, 1, 1).unwrap();
    check(&mut limiter, 1, 1, 1, false, 1);
    check(&mut limiter, 2, 1, 1, true, 0);
    let last = 3_155_378_975_999_999_999;
    limiter.record_failure(last, 1, 1).unwrap();
    assert_eq!(
        limiter.check(last, 1, 1),
        Err(LimitError::TimestampOutOfRange)
    );
    check(&mut limiter, last, 2, 1, true, 0);
}
