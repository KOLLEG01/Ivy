//! Network-side rejected-binding lifetime. The worker retains diagnostic rows;
//! the configured date/runtime owner supplies original timestamp semantics.
use crate::retention::Runtime;
use ivy_contracts::{IvyJson, IvyResult, hash::hash_json};
use std::{collections::HashMap, sync::Arc};
pub struct ToolBindingDiagnostics {
    active: HashMap<String, IvyJson>,
    record: Box<dyn Fn(IvyJson) -> IvyResult<()> + Send + Sync>,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    runtime: Arc<dyn Runtime + Send + Sync>,
}
impl ToolBindingDiagnostics {
    pub fn new(
        record: Box<dyn Fn(IvyJson) -> IvyResult<()> + Send + Sync>,
        now: Arc<dyn Fn() -> f64 + Send + Sync>,
        runtime: Arc<dyn Runtime + Send + Sync>,
    ) -> Self {
        Self {
            active: HashMap::new(),
            record,
            now,
            runtime,
        }
    }
    pub fn restore(&mut self, values: &[IvyJson]) -> IvyResult<()> {
        for value in values {
            if value.get("source") == Some(&"hive.tool-bindings".into())
                && value.get("code") == Some(&"tool_definition_changed".into())
                && value.get("status") != Some(&"resolved".into())
            {
                self.active
                    .insert(hash_json(value.required("resource")?)?, value.clone());
            }
        }
        Ok(())
    }
    pub fn rejected(&mut self, target: &IvyJson) -> IvyResult<()> {
        let resource = target.clone();
        let key = hash_json(&resource)?;
        let prior = self.active.get(&key);
        let at = self.runtime.iso_date((self.now)())?;
        if let Some(prior) = prior {
            let observed = (self.now)();
            let milliseconds = prior
                .get("lastObservedAt")
                .and_then(IvyJson::as_js_string)
                .map(|v| self.runtime.parse_date(v))
                .unwrap_or(f64::NAN);
            if observed - milliseconds < 30_000.0 {
                return Ok(());
            }
        }
        let value=IvyJson::object([("code","tool_definition_changed".into()),("source","hive.tool-bindings".into()),("resource",resource),("severity","warning".into()),("status","current".into()),("firstObservedAt",prior.and_then(|v|v.get("firstObservedAt")).filter(|v|!v.is_null()).cloned().unwrap_or_else(||IvyJson::String(at.clone()))),("lastObservedAt",IvyJson::String(at)),("message","A caller is using an outdated Tool definition. Rediscover the selected provider before resuming this operation.".into())]);
        self.active.insert(key, value.clone());
        (self.record)(value)
    }
    pub fn completed(&mut self, target: &IvyJson) -> IvyResult<()> {
        let key = hash_json(target)?;
        let Some(mut prior) = self.active.remove(&key) else {
            return Ok(());
        };
        prior.insert("status", "resolved".into());
        prior.insert(
            "lastObservedAt",
            IvyJson::String(self.runtime.iso_date((self.now)())?),
        );
        prior.insert(
            "message",
            "The caller completed a call using the current Tool definition.".into(),
        );
        (self.record)(prior)
    }
}
