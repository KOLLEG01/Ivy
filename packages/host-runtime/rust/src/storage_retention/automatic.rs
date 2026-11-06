//! The executor's single storage job. Probes are cheap; admitted collection drains.
use super::{RetentionBindings, collect_host_storage};
use crate::{IvyError, IvyJson, IvyResult, journal::HostJournal, storage_space::{self, RetentionSchedule}};
use std::sync::Arc;
use tokio::task::JoinHandle;

pub struct AutomaticRetention {
    schedule: Option<RetentionSchedule>,
    next_probe: i64,
    last_clock: i64,
    observed_attempt: Option<IvyJson>,
    active: Option<JoinHandle<IvyResult<IvyJson>>>,
}
impl Default for AutomaticRetention {
    fn default() -> Self {
        Self { schedule: None, next_probe: 0, last_clock: i64::MIN, observed_attempt: None, active: None }
    }
}
impl AutomaticRetention {
    /// Called by the existing coordinator. Never waits for a collection in flight.
    pub async fn tick(&mut self, journal: &HostJournal, bindings: Arc<RetentionBindings>) -> IvyResult<bool> {
        let now = bindings.clock.utc_millis();
        if now < self.last_clock { self.next_probe = now; self.schedule = None; }
        self.last_clock = now;
        let mut completed = false;
        if self.active.as_ref().is_some_and(JoinHandle::is_finished) {
            let result = self.active.take().unwrap().await.map_err(|_| IvyError::internal()).and_then(|v| v);
            if result.is_err() { self.schedule.get_or_insert_with(|| RetentionSchedule::new(now as f64)).failed(now as f64); }
            result?;
            completed = true;
        }
        if self.active.is_some() || now < self.next_probe { return Ok(completed); }
        self.next_probe = now.saturating_add(60_000);
        let previous = journal.storage_retention_status().await?;
        let attempt = previous.as_ref().and_then(|v| v.get("startedAt")).cloned();
        if self.schedule.is_none() || attempt != self.observed_attempt {
            self.schedule = Some(RetentionSchedule::resume(now as f64, previous.as_ref()));
            self.observed_attempt = attempt;
            completed |= previous.as_ref().is_some_and(|v| v.get("state") != Some(&"running".into()));
        }
        let config = journal.config().await?;
        let space = match storage_space::host_storage_space(config.clone()).await {
            Ok(space) => space,
            Err(error) => { self.schedule.as_mut().unwrap().failed(now as f64); return Err(error); }
        };
        let schedule = self.schedule.as_mut().unwrap();
        if let Some(trigger) = schedule.trigger(now as f64, space.get("lowSpace") == Some(&true.into())) {
            schedule.started(now as f64);
            self.active = Some(tokio::spawn(async move {
                collect_host_storage(config, None, Some(trigger.into()), bindings).await
            }));
        }
        Ok(completed)
    }
    /// Closing the executor drains deletion and publication before its host lock.
    pub async fn close(&mut self) {
        if let Some(active) = self.active.take() { let _ = active.await; }
    }
}
