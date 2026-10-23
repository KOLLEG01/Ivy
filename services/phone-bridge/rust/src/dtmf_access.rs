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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeProgress {
    Unsupported,
    Ignored,
    Progress,
    Accepted,
    Rejected,
    AlreadyCompleted,
}

/// The call serializes this gate. Neither Debug nor its result exposes the
/// protected code/attempt. Both accepted forms always use equal-sized buffers.
pub struct AccessCodeGate {
    standard: Vec<u8>,
    no_upgrade: Vec<u8>,
    attempt: Vec<u8>,
    length: usize,
    started: bool,
    completed: bool,
    accepted: bool,
    disable_upgrade: bool,
}
impl AccessCodeGate {
    pub fn new(code: &str) -> Option<Self> {
        let code = code.to_ascii_uppercase();
        let bytes = code.as_bytes();
        if bytes.len() < 3
            || bytes[0] != b'*'
            || *bytes.last()? != b'#'
            || !bytes[1..bytes.len() - 1]
                .iter()
                .all(|b| b.is_ascii_digit() || matches!(b, b'A'..=b'D'))
        {
            return None;
        }
        let mut standard = bytes.to_vec();
        standard.push(0);
        let mut no_upgrade = vec![b'*'];
        no_upgrade.extend_from_slice(bytes);
        Some(Self {
            attempt: vec![0; standard.len()],
            standard,
            no_upgrade,
            length: 0,
            started: false,
            completed: false,
            accepted: false,
            disable_upgrade: false,
        })
    }
    pub fn is_accepted(&self) -> bool {
        self.accepted
    }
    pub fn disable_codec_upgrade(&self) -> bool {
        self.accepted && self.disable_upgrade
    }
    pub fn process(&mut self, digit: char) -> CodeProgress {
        use subtle::ConstantTimeEq;
        let digit = digit.to_ascii_uppercase();
        if !matches!(digit, '0'..='9' | '*' | '#' | 'A'..='D') {
            return CodeProgress::Unsupported;
        }
        if self.completed {
            return CodeProgress::AlreadyCompleted;
        }
        if !self.started {
            if digit != '*' {
                return CodeProgress::Ignored;
            }
            self.started = true;
        } else if digit == '#' {
            self.append(digit);
            self.completed = true;
            // Evaluate both comparisons even when the public lengths differ.
            let standard = self.attempt.ct_eq(&self.standard);
            let no_upgrade = self.attempt.ct_eq(&self.no_upgrade);
            self.disable_upgrade = bool::from(no_upgrade) & (self.length == self.no_upgrade.len());
            self.accepted = (bool::from(standard) & (self.length == self.standard.len() - 1))
                | self.disable_upgrade;
            return if self.accepted {
                CodeProgress::Accepted
            } else {
                CodeProgress::Rejected
            };
        }
        self.append(digit);
        CodeProgress::Progress
    }
    fn append(&mut self, digit: char) {
        if self.length < self.attempt.len() {
            self.attempt[self.length] = digit as u8;
        }
        self.length = (self.length + 1).min(self.attempt.len() + 1);
    }
}
