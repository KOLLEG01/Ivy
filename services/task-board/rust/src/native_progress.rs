//! Observation only: silence never authorizes cancellation or repeated input.
use ivy_contracts::{IvyJson as Json, IvyResult, JsString};
use ivy_sdk::clock::date_to_iso;
use std::cell::RefCell;

struct ObservedRun {
    run: JsString,
    task: JsString,
    fingerprint: JsString,
    last_activity: f64,
}
pub struct NativeProgress {
    source: JsString,
    warning_ms: f64,
    runs: RefCell<Vec<ObservedRun>>,
}
impl NativeProgress {
    pub fn new(source: JsString, warning_ms: f64) -> Self {
        Self {
            source,
            warning_ms,
            runs: RefCell::default(),
        }
    }
    pub fn observe(
        &self,
        run: &JsString,
        task: &JsString,
        running: bool,
        fingerprint: JsString,
        now: f64,
    ) {
        if !running {
            self.forget(run);
            return;
        }
        let mut runs = self.runs.borrow_mut();
        if let Some(previous) = runs.iter_mut().find(|value| &value.run == run) {
            if previous.fingerprint != fingerprint {
                *previous = ObservedRun {
                    run: run.clone(),
                    task: task.clone(),
                    fingerprint,
                    last_activity: now,
                };
            }
            return;
        }
        if runs.len() >= 1024 {
            runs.remove(0);
        }
        runs.push(ObservedRun {
            run: run.clone(),
            task: task.clone(),
            fingerprint,
            last_activity: now,
        });
    }
    pub fn activity(&self, run: &JsString, now: f64) {
        if let Some(value) = self
            .runs
            .borrow_mut()
            .iter_mut()
            .find(|value| &value.run == run)
        {
            value.last_activity = now;
        }
    }
    pub fn forget(&self, run: &JsString) {
        self.runs.borrow_mut().retain(|value| &value.run != run);
    }
    pub fn diagnostics(&self, now: f64) -> IvyResult<Vec<Json>> {
        self.runs.borrow().iter().filter(|value| now - value.last_activity >= self.warning_ms).map(|value| {
            Ok(Json::object([
                ("code","task_board_native_silent".into()),
                ("resource",Json::object([("objectId",Json::String(value.task.clone()))])),
                ("source",Json::String(self.source.clone())),
                ("severity","warning".into()),("status","current".into()),
                ("firstObservedAt",Json::String(date_to_iso(value.last_activity + self.warning_ms)?)),
                ("lastObservedAt",Json::String(date_to_iso(now)?)),
                ("message",format!("The native owner still reports running, but no progress has been observed for {} minutes. Inspect the original native turn; its claim remains active.",((now-value.last_activity)/60_000.0).floor()).into()),
            ]))
        }).collect()
    }
}
