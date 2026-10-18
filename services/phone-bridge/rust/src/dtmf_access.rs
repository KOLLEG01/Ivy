//! Access failures shared across calls by the transport owner, which serializes
//! access. UTC timestamps and windows use 100 ns ticks, matching the native
//! DateTimeOffset/TimeSpan contract (UTC epoch: 0001-01-01).
use std::collections::VecDeque;

const MAX_UTC_TICKS: i64 = 3_155_378_975_999_999_999;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitError {
    MaximumFailures,
    Window,
    TimestampOutOfRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Decision {
    pub allowed: bool,
    pub retry_after_ticks: i64,
}

#[derive(Default)]
pub struct FailureLimiter {
    failures: VecDeque<i64>,
}

impl FailureLimiter {
    pub fn check(
        &mut self,
        now_utc_ticks: i64,
        maximum_failures: i32,
        window_ticks: i64,
    ) -> Result<Decision, LimitError> {
        self.remove_expired(now_utc_ticks, maximum_failures, window_ticks)?;
        if self.failures.len() < maximum_failures as usize {
            return Ok(Decision {
                allowed: true,
                retry_after_ticks: 0,
            });
        }
        let expiry = self.failures[0]
            .checked_add(window_ticks)
            .ok_or(LimitError::TimestampOutOfRange)?;
        utc_ticks(expiry)?;
        Ok(Decision {
            allowed: false,
            retry_after_ticks: (expiry - now_utc_ticks).max(0),
        })
    }

    pub fn record_failure(
        &mut self,
        now_utc_ticks: i64,
        maximum_failures: i32,
        window_ticks: i64,
    ) -> Result<(), LimitError> {
        self.remove_expired(now_utc_ticks, maximum_failures, window_ticks)?;
        // The maximum controls admission, not storage capacity. Failed attempts
        // above the current maximum still count if a later call changes policy.
        self.failures.push_back(now_utc_ticks);
        Ok(())
    }

    fn remove_expired(
        &mut self,
        now_utc_ticks: i64,
        maximum_failures: i32,
        window_ticks: i64,
    ) -> Result<(), LimitError> {
        utc_ticks(now_utc_ticks)?;
        if maximum_failures < 1 {
            return Err(LimitError::MaximumFailures);
        }
        if window_ticks <= 0 {
            return Err(LimitError::Window);
        }
        let cutoff = now_utc_ticks - window_ticks;
        utc_ticks(cutoff)?;
        // Keep insertion order even when the wall clock moves backwards.
        while self
            .failures
            .front()
            .is_some_and(|failure| *failure <= cutoff)
        {
            self.failures.pop_front();
        }
        Ok(())
    }
}

fn utc_ticks(value: i64) -> Result<(), LimitError> {
    if (0..=MAX_UTC_TICKS).contains(&value) {
        Ok(())
    } else {
        Err(LimitError::TimestampOutOfRange)
    }
}
