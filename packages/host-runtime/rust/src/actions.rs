//! Shared CLI/Manager mutation intake. Acceptance returns the durable journal
//! entry, including a definite terminal failure, without waiting for execution.
use crate::{IvyError, IvyJson, IvyResult, journal::{HostJournal, requires_bootstrap_replacement}, node_path::Context, source};
use ivy_contracts::JsString;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Deploy, Rollback, Restart, Enable, Disable }
impl Action {
    pub fn as_str(self) -> &'static str {
        match self { Self::Deploy => "deploy", Self::Rollback => "rollback", Self::Restart => "restart", Self::Enable => "enable", Self::Disable => "disable" }
    }
}
/// CLI callers pass their admitted option values. Manager callers use
/// `from_manager` for typed tool arguments and operation binding.
pub struct LocalAction {
    pub action: Action,
    pub instance_id: JsString,
    pub operation_id: JsString,
    pub candidate_id: Option<JsString>,
    pub source: Option<JsString>,
    pub target_build: Option<JsString>,
}
fn manager_string(args: &IvyJson, key: &str) -> IvyResult<JsString> {
    args.required(key)?.as_js_string().cloned().ok_or_else(|| IvyError::new("invalid_arguments", "Action identifiers must be strings."))
}
impl LocalAction {
    pub fn from_manager(action: Action, args: &IvyJson, context_operation_id: Option<&JsString>) -> IvyResult<Self> {
        if context_operation_id.is_some_and(|id| !id.0.is_empty() && args.get("operationId") != Some(&IvyJson::String(id.clone()))) {
            return Err(IvyError::new("mutation_conflict", "Tool and local action operation IDs must match."));
        }
        let option = |key| args.get(key).filter(|v| !v.is_null()).map(|value| {
            value.as_js_string().cloned().ok_or_else(|| IvyError::new("invalid_arguments", "Action options must be strings."))
        }).transpose().map(|value| value.filter(|value| !value.0.is_empty()));
        let result = Self { action, instance_id: manager_string(args, "instanceId")?, operation_id: manager_string(args, "operationId")?,
            candidate_id: option("candidateId")?, source: option("source")?, target_build: option("targetBuild")? };
        if action != Action::Deploy && (result.candidate_id.is_some() || result.source.is_some()) || action != Action::Rollback && result.target_build.is_some() {
            return Err(IvyError::new("invalid_arguments", "This option does not belong to the selected command."));
        }
        Ok(result)
    }
    /// Resolve source once, as the CLI did before checking bootstrap ownership.
    /// Empty optional CLI values are omitted rather than inventing arguments.
    pub fn request(self) -> IvyResult<IvyJson> {
        for (key, value) in [("instance", &self.instance_id), ("operation-id", &self.operation_id)] {
            if value.0.is_empty() { return Err(IvyError::new("invalid_arguments", format!("Missing --{key}."))); }
        }
        let mut request = IvyJson::object([("action", self.action.as_str().into()), ("instanceId", IvyJson::String(self.instance_id)), ("operationId", IvyJson::String(self.operation_id))]);
        for (key, value) in [("candidateId", self.candidate_id), ("targetBuild", self.target_build)] {
            if let Some(value) = value.filter(|v| !v.0.is_empty()) { request.insert(key, IvyJson::String(value)); }
        }
        if let Some(source) = self.source.filter(|v| !v.0.is_empty()) {
            let paths = Context::current().map_err(|_| IvyError::internal())?;
            request.insert("source", IvyJson::String(paths.resolve(&[source])));
        }
        Ok(request)
    }
}

/// Use one journal bound to the request's normalized HostConfig. A caller that
/// reloads configuration concurrently must open a separate request journal.
/// Dropping this waiter does not cancel an admitted capture or journal write.
pub async fn accept_local_action(journal: HostJournal, action: LocalAction, bootstrap_root: PathBuf) -> IvyResult<IvyJson> {
    let request = action.request()?;
    tokio::spawn(async move {
        let instance_id = request.required("instanceId")?.as_str()?;
        let operation_id = request.required("operationId")?.as_str()?;
        let instance = journal.instance(&instance_id).await?;
        if requires_bootstrap_replacement(&instance) && journal.installed(&instance_id).await?.is_some() {
            return Err(IvyError::new("restart_required", "Bootstrap-owned releases use prepare followed by install-bootstrap; normal deployment cannot observe their OS-owned drain."));
        }
        // Replay, including argument conflict/expiry, goes through the journal.
        // Never recapture an already accepted source operation.
        if journal.operation(&operation_id).await?.is_some() {
            return journal.accept(request, None).await;
        }
        let snapshot = if let Some(original) = request.get("source").and_then(IvyJson::as_js_string) {
            if request.get("candidateId").is_some() { return Err(IvyError::new("invalid_arguments", "Deploy requires exactly one candidate or source.")); }
            journal.instance(&instance_id).await?;
            Some(source::capture_source(original.clone(), journal.config().await?, bootstrap_root).await?)
        } else { None };
        journal.accept(request, snapshot).await
    }).await.map_err(|_| IvyError::internal())?
}
