//! Durable activation lanes. The host composes the existing preparation,
//! compatibility and OS owners through one mandatory runtime boundary.
use crate::{IvyError, IvyJson, IvyResult, Outcome, config, journal::HostJournal};
use ivy_contracts::{JsString, json};
use ivy_sdk::clock::{Clock, SystemClock, parse_date};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio_util::sync::CancellationToken;

mod coordinator;
mod lane;
mod runtime;
pub use coordinator::{HostExecutor, PackageUpdateRuntime};
pub use runtime::{
    NativeActivationRuntime, NativeOfflineHiveInspector, OfflineHiveInspector, OfflineInspection,
};

pub type ExecutorFuture<'a, T> = Pin<Box<dyn Future<Output = IvyResult<T>> + Send + 'a>>;

/// These are the original effect boundaries, not alternate journal/target policy.
/// A production composition must supply every boundary; there is no permissive
/// default for compatibility, preparation or process ownership.
pub trait ActivationRuntime: Send + Sync {
    /// Read the captured source plan, check service storage, then call the
    /// existing preparer with the lane's immutable accepted HostConfig.
    fn prepare<'a>(
        &'a self,
        context: &'a OperationContext,
        snapshot: &'a IvyJson,
        component: &'a str,
    ) -> ExecutorFuture<'a, IvyJson>;
    /// Run all preflight checks: runtime/candidate/native/storage/commands,
    /// journal format, disk reserve, and actual current/historical Hive data.
    /// Rollback supplies the exact saved prior settings. Called again after drain.
    fn compatibility<'a>(
        &'a self,
        context: &'a OperationContext,
        candidate: &'a str,
        settings: Option<IvyJson>,
    ) -> ExecutorFuture<'a, ()>;
    /// Reconcile the selected Docker/systemd owner, or observe the independent
    /// Windows RuntimeOwner. Only allow_start permits an explicit new launch.
    fn reconcile<'a>(
        &'a self,
        context: &'a OperationContext,
        target: Option<IvyJson>,
        allow_start: bool,
    ) -> ExecutorFuture<'a, ()>;
    /// Release passive Docker observers when this executor closes. Do not stop
    /// managed services or change durable target/restart policy during disposal.
    fn dispose(&self) -> ExecutorFuture<'_, ()>;
}

#[derive(Clone)]
pub struct ExecutorOptions {
    pub own_instance_id: Option<String>,
    pub account_home: JsString,
    pub build_id: Option<JsString>,
}

/// Each lane owns a distinct journal handle and a fixed accepted configuration.
/// It never switches the base journal or borrows desired settings for rollback.
pub struct OperationContext {
    pub journal: HostJournal,
    pub config: IvyJson,
    pub instance_id: String,
    pub options: ExecutorOptions,
    pub cancel: CancellationToken,
    pub clock: Arc<dyn Clock>,
}
impl OperationContext {
    pub async fn open(
        base: &HostJournal,
        entry: &IvyJson,
        options: ExecutorOptions,
        cancel: CancellationToken,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        let source = config::path_value(entry.required("configurationPath")?)?;
        let config = base.accepted_configuration(source).await?;
        let journal = HostJournal::open_with_clock(config.clone(), clock.clone()).await?;
        Ok(Self {
            journal,
            config,
            instance_id: text(entry.required("record")?, "instanceId")?,
            options,
            cancel,
            clock,
        })
    }
    pub fn is_own_instance(&self) -> bool {
        self.options.own_instance_id.as_deref() == Some(self.instance_id.as_str())
    }
}

/// The accepted lane keeps running when its caller drops the response future.
/// Explicit cancellation preserves its last durable phase for the next executor.
pub async fn execute_deployment(
    base: HostJournal,
    entry: IvyJson,
    runtime: Arc<dyn ActivationRuntime>,
    options: ExecutorOptions,
    cancel: CancellationToken,
) -> IvyResult<()> {
    execute_with_clock(base, entry, runtime, options, cancel, Arc::new(SystemClock)).await
}
pub async fn execute_with_clock(
    base: HostJournal,
    entry: IvyJson,
    runtime: Arc<dyn ActivationRuntime>,
    options: ExecutorOptions,
    cancel: CancellationToken,
    clock: Arc<dyn Clock>,
) -> IvyResult<()> {
    tokio::spawn(async move {
        let context = OperationContext::open(&base, &entry, options, cancel, clock).await?;
        let result = lane::execute(&context, runtime.as_ref(), entry).await;
        let closed = context.journal.close().await;
        result.and(closed)
    })
    .await
    .map_err(|_| IvyError::internal())?
}

pub fn readiness_failure_start(previous: f64, at: f64) -> f64 {
    if previous != 0.0 && !previous.is_nan() {
        previous
    } else {
        at
    }
}
pub fn readiness_expired(start: f64, timeout_ms: f64, at: f64) -> bool {
    start > 0.0 && at - start > timeout_ms
}
pub fn replacement_needs_settled_observation(
    current: Option<&str>,
    installed: Option<&str>,
) -> bool {
    current.is_some() && current != installed
}
pub fn stopped_target_is_conclusive(observed: Option<&IvyJson>, target: Option<&IvyJson>) -> bool {
    let Some(observed) = observed else {
        return false;
    };
    match target {
        None => {
            observed.get("state") == Some(&"idle".into())
                && ["targetRevision", "candidateId", "buildId"]
                    .iter()
                    .all(|key| observed.get(key) == Some(&IvyJson::Null))
        }
        Some(target) => {
            field(target, "desired") == &"stopped".into()
                && field(observed, "state") == &"stopped".into()
                && observed.get("targetRevision") == target.get("revision")
        }
    }
}
pub fn running_hive_can_be_inspected(observed: Option<&IvyJson>, target: Option<&IvyJson>) -> bool {
    observed.zip(target).is_some_and(|(observed, target)| {
        field(target, "desired") == &"running".into()
            && field(observed, "state") == &"ready".into()
            && observed.get("targetRevision") == target.get("revision")
            && observed.get("candidateId") == target.get("candidateId")
    })
}
pub fn fresh_observation(observed: &IvyJson, at: f64) -> bool {
    let Some(date) = observed.get("observedAt").and_then(IvyJson::as_js_string) else {
        return false;
    };
    let time = parse_date(date);
    at - time < 15_000.0 && time <= at + 1000.0
}

#[derive(Clone, Copy, Debug)]
pub enum ObservationGoal {
    Stopped,
    Drained,
    Activation,
    ArtifactVerified,
    Desired(bool),
}
impl ObservationGoal {
    fn matches(self, value: &IvyJson) -> bool {
        let state = field(value, "state");
        match self {
            Self::Stopped => state == &"stopped".into(),
            Self::Drained => ["idle", "stopped"]
                .iter()
                .any(|s| state == &IvyJson::from(*s)),
            Self::Activation => ["starting", "ready", "stopped", "failed", "exited"]
                .iter()
                .any(|s| state == &IvyJson::from(*s)),
            Self::ArtifactVerified => {
                !(state == &"starting".into()
                    && field(value, "code") == &"artifact_verification".into())
            }
            Self::Desired(running) => {
                state == &IvyJson::from(if running { "ready" } else { "stopped" })
            }
        }
    }
}
pub async fn wait_for(
    context: &OperationContext,
    runtime: &dyn ActivationRuntime,
    target: Option<IvyJson>,
    goal: ObservationGoal,
    timeout_ms: f64,
    allow_start: bool,
) -> IvyResult<IvyJson> {
    let end = context.clock.utc_millis() as f64 + timeout_ms;
    while !context.cancel.is_cancelled() {
        runtime
            .reconcile(context, target.clone(), allow_start)
            .await?;
        let observations = context.journal.observations().await?;
        let value = observations.get(&context.instance_id);
        let at = context.clock.utc_millis() as f64;
        let absent_revision = IvyJson::Null;
        let revision = target
            .as_ref()
            .map(|t| field(t, "revision"))
            .unwrap_or(&absent_revision);
        if let Some(value) = value {
            let fresh = fresh_observation(value, at);
            if (fresh || stopped_target_is_conclusive(Some(value), target.as_ref()))
                && field(value, "targetRevision") == revision
                && goal.matches(value)
            {
                return Ok(value.clone());
            }
            if fresh
                && target.as_ref().is_some_and(|t| {
                    field(value, "targetRevision") == field(t, "revision")
                        && field(t, "desired") == &"running".into()
                })
                && ["failed", "exited", "unknown"]
                    .iter()
                    .any(|s| field(value, "state") == &IvyJson::from(*s))
            {
                let code = selected(value, "code")
                    .cloned()
                    .unwrap_or_else(|| "process_failed".into());
                let message = concat(&[
                    "Runtime owner reported ".into(),
                    json::try_js_string(field(value, "state"))?,
                    ": ".into(),
                    json::try_js_string(&code)?,
                    ". ".into(),
                    json::try_js_string(field(value, "message"))?,
                ]);
                let mut error = IvyError::new_js(json::try_js_string(&code)?, message);
                error.outcome = Outcome::Unknown.into();
                return Err(error);
            }
        }
        if at >= end {
            return Err(unknown(
                "observation_deadline",
                "The current runtime owner did not establish the required state before its deadline.",
            ));
        }
        tokio::select! { _=context.cancel.cancelled()=>break, _=tokio::time::sleep(std::time::Duration::from_millis(200))=>{} }
    }
    Err(IvyError::new("cancelled", "Cancelled"))
}

pub(crate) fn empty() -> IvyJson {
    IvyJson::Object(vec![])
}
pub(crate) fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    static NULL: IvyJson = IvyJson::Null;
    value.get(key).unwrap_or(&NULL)
}
pub(crate) fn selected<'a>(value: &'a IvyJson, key: &str) -> Option<&'a IvyJson> {
    value.get(key).filter(|v| !v.is_null())
}
pub(crate) fn text(value: &IvyJson, key: &str) -> IvyResult<String> {
    value.required(key)?.as_str()
}
pub(crate) fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
pub(crate) fn unknown(code: &str, message: &str) -> IvyError {
    let mut e = IvyError::new(code, message);
    e.outcome = Outcome::Unknown.into();
    e
}
pub(crate) fn terminal(phase: &str) -> bool {
    ["succeeded", "rolled_back", "failed", "needs_attention"].contains(&phase)
}
pub(crate) fn stamp(clock: &dyn Clock) -> IvyResult<IvyJson> {
    Ok(chrono::DateTime::from_timestamp_millis(clock.utc_millis())
        .ok_or_else(IvyError::internal)?
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .into())
}
pub(crate) fn concat(parts: &[JsString]) -> JsString {
    JsString(parts.iter().flat_map(|p| p.0.iter().copied()).collect())
}
