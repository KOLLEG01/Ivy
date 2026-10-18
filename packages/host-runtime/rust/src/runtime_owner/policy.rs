//! Restart and readiness policy for the single RuntimeOwner loop.
//!
//! Times here are elapsed milliseconds from the owner's monotonic clock. The
//! journal adapter converts the remaining restart delay to a wall-clock date.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitAction {
    Exited,
    Restart,
    Blocked,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub restart_count: u64,
    pub next_restart: Option<u64>,
    pub restart_allowed: bool,
    pub blocked: bool,
    pub manifest_ready: bool,
    started_at: u64,
    unready_since: Option<u64>,
}

impl Policy {
    /// The caller must already hold the existing per-instance OS lock. A PID
    /// difference alone is never authority to take ownership of a process.
    pub fn new(same_manager_unknown: bool) -> Self {
        Self {
            restart_count: 0,
            next_restart: None,
            restart_allowed: true,
            blocked: same_manager_unknown,
            manifest_ready: false,
            started_at: 0,
            unready_since: None,
        }
    }

    pub fn select_running_target(&mut self) {
        // A new target must not erase an uncertain process outcome.
        let blocked = self.blocked;
        *self = Self::new(blocked);
    }

    pub fn launched(&mut self, now: u64) {
        self.started_at = now;
        self.unready_since = Some(now);
        self.next_restart = None;
        self.manifest_ready = false;
    }

    pub fn can_launch(&self, now: u64) -> bool {
        !self.blocked
            && self.restart_allowed
            && self.next_restart.is_none_or(|deadline| now >= deadline)
    }

    pub fn backoff(&mut self, now: u64, minimum_ms: u64, maximum_ms: u64) {
        self.manifest_ready = false;
        self.restart_count = self.restart_count.saturating_add(1);
        let exponent = (self.restart_count - 1).min(16) as u32;
        let delay = minimum_ms.saturating_mul(1_u64 << exponent).min(maximum_ms);
        self.next_restart = Some(now.saturating_add(delay));
    }

    /// Called only after the owned-process API has established completion.
    pub fn exited(
        &mut self,
        restart_policy: &str,
        exit_code: Option<i32>,
        error_code: Option<&str>,
    ) -> ExitAction {
        if error_code == Some("outcome_unknown") {
            self.blocked = true;
            return ExitAction::Blocked;
        }
        let failed = exit_code != Some(0) || error_code.is_some();
        if restart_policy == "never" || (restart_policy == "on-failure" && !failed) {
            self.restart_allowed = false;
            ExitAction::Exited
        } else {
            ExitAction::Restart
        }
    }

    pub fn ready(&mut self, now: u64) {
        self.manifest_ready = true;
        self.unready_since = None;
        if now.saturating_sub(self.started_at) >= 30_000 {
            self.restart_count = 0;
        }
    }

    /// Loss of a read-only health dependency does not fence process ownership.
    /// Repeated failures retain the first failure time, including after readiness.
    pub fn unready(&mut self, now: u64) {
        self.unready_since.get_or_insert(now);
    }

    pub fn readiness_expired(&self, now: u64, timeout_ms: u64) -> bool {
        self.unready_since
            .is_some_and(|since| now.saturating_sub(since) > timeout_ms)
    }
}

/// The provider and process layers must use the same bounded diagnostic-code
/// grammar. Arbitrary stderr text is never a persisted owner diagnostic.
pub fn valid_process_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// These health dispatchers are read-only helpers, not long-lived service Jobs.
pub fn direct_health_probe(executable: &str, args: &[String]) -> bool {
    if executable != "node" || args.len() != 1 {
        return false;
    }
    let path = &args[0];
    if path == "dist/packages/host-runtime/src/health.js" {
        return true;
    }
    let Some(component) = path
        .strip_prefix("dist/services/")
        .and_then(|p| p.strip_suffix("/src/health.js"))
    else {
        return false;
    };
    let bytes = component.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}
