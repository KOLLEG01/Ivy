//! Serialize explicit native-stop reconciliation with normal host admission.
use super::*;

impl HostJournal {
    /// Only a completed explicit disable and the original owner's stopped
    /// observation release nested native ownership. PID absence is insufficient.
    /// The callback runs inside host admission's transaction, before any new
    /// lifecycle action can be accepted or publish a replacement target.
    pub async fn with_stopped_owner<T: Send + 'static>(
        &self,
        instance_id: String,
        deployment_id: String,
        reconcile: impl FnOnce(IvyJson, IvyJson, IvyJson) -> IvyResult<T> + Send + 'static,
    ) -> IvyResult<T> {
        self.with(move |s| s.transaction(|s| {
            let entry = s.lookup(&deployment_id, false)?.ok_or_else(|| IvyError::new("not_found", "Original disable deployment is unavailable."))?;
            let record = entry.required("record")?;
            let request = entry.required("request")?;
            let previous = entry.required("activation")?.required("previousTarget")?;
            let target = s.target(&instance_id)?.ok_or_else(IvyError::internal)?;
            let observations = s.observations()?;
            let observed = observations.required(&instance_id)?;
            require(record.get("phase") == Some(&"succeeded".into())
                && request.get("action") == Some(&"disable".into())
                && record.get("instanceId") == Some(&instance_id.clone().into())
                && record.get("hostId") == s.config.get("hostId")
                && request.get("instanceId") == record.get("instanceId")
                && previous.get("desired") == Some(&"running".into())
                && target.get("desired") == Some(&"stopped".into())
                && target.get("candidateId") == previous.get("candidateId")
                && observed.get("targetRevision") == target.get("revision")
                && observed.get("state") == Some(&"stopped".into())
                && observed.get("code").is_none_or(IvyJson::is_null),
                "native_stop_unverified", "Native recovery requires the exact completed disable and stopped original owner.")?;
            let count: i64 = internal(s.db.query_row("SELECT count(*) FROM operations WHERE phase NOT IN ('succeeded','failed','rolled_back','needs_attention') AND json_extract(entry_json,'$.record.instanceId')=?", [&instance_id], |row| row.get(0)))?;
            require(count == 0, "native_stop_unverified", "Another lifecycle action still owns this instance.")?;
            let (reference, _) = s.policy(&target)?.ok_or_else(|| IvyError::new("native_stop_unverified", "Stopped target policy is unavailable."))?;
            require(Some(&reference) == entry.get("configurationPath"),
                "native_stop_unverified", "Stopped target belongs to another lifecycle action.")?;
            let observed_at = parse_date(observed.required("observedAt")?.as_js_string().ok_or_else(IvyError::internal)?);
            let requested_at = parse_date(record.required("createdAt")?.as_js_string().ok_or_else(IvyError::internal)?);
            require(observed_at.is_finite() && requested_at.is_finite() && observed_at >= requested_at,
                "native_stop_unverified", "Stopped evidence predates the original disable.")?;
            let (_, policy) = s.policy(previous)?.ok_or_else(|| IvyError::new("native_stop_unverified", "Original process policy is unavailable."))?;
            let instance = policy.required("instances")?.as_array().ok_or_else(IvyError::internal)?.iter()
                .find(|value| value.get("instanceId") == Some(&instance_id.clone().into())).ok_or_else(IvyError::internal)?;
            require(cfg!(windows) && instance.get("engine") == Some(&"process".into())
                && instance.get("process").and_then(|p| p.get("allowWindowsBreakaway")) != Some(&IvyJson::Bool(true)),
                "native_stop_unverified", "Recovery requires the original Windows owner without process breakaway.")?;
            reconcile(policy, previous.clone(), observed.clone())
        })).await
    }
}
