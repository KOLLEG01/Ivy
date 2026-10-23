//! Bind concrete domain checks to creation and existing-action admission.
//! Current aggregate, creation, conversation and recovery rules stay mandatory.
use crate::{
    checks::{CheckStore, TaskChecks},
    engine_publication::CreationChecks,
    engine_validation::ActionValidationChecks,
};
use ivy_contracts::{IvyJson as Json, IvyResult, JsString};

/// Current contract/conversation invariants are additional to cross-record and
/// native checks. No legacy policy, empty implementation or default is supplied.
#[allow(async_fn_in_trait)]
pub trait CheckPolicy {
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()>;
    async fn fields(&self, fields: &Json, id: Option<&JsString>) -> IvyResult<()>;
    async fn creation(
        &self,
        request: &Json,
        fields: &Json,
        actor: &Json,
        at: &JsString,
    ) -> IvyResult<()>;
}
pub struct EngineChecks<'a, S, P> {
    pub core: TaskChecks<'a, S>,
    pub policy: &'a P,
}
impl<S: CheckStore, P: CheckPolicy> CreationChecks for EngineChecks<'_, S, P> {
    async fn fields(&self, fields: &Json) -> IvyResult<()> {
        self.policy.fields(fields, None).await?;
        self.core.fields(fields, None).await
    }
    async fn initial_state(
        &self,
        request: &Json,
        fields: &Json,
        actor: &Json,
        at: &JsString,
    ) -> IvyResult<Json> {
        self.policy.creation(request, fields, actor, at).await?;
        self.core.initial_state(request, fields, actor, at).await
    }
    async fn execution_options(&self, fields: &Json, allow_unavailable: bool) -> IvyResult<()> {
        self.core
            .execution_options(fields, None, allow_unavailable)
            .await
    }
}
impl<S: CheckStore, P: CheckPolicy> ActionValidationChecks for EngineChecks<'_, S, P> {
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()> {
        self.policy.aggregate(task, id).await?;
        self.core.aggregate(task, id).await
    }
    async fn fields(&self, fields: &Json, id: &JsString) -> IvyResult<()> {
        self.policy.fields(fields, Some(id)).await?;
        self.core.fields(fields, Some(id)).await
    }
    async fn execution_options(&self, fields: &Json, task: Option<&Json>) -> IvyResult<()> {
        self.core.execution_options(fields, task, false).await
    }
    async fn execution(
        &self,
        task: &Json,
        intent: &Json,
        id: &JsString,
        at: &JsString,
        target: &Json,
        workspace: &Json,
    ) -> IvyResult<Json> {
        self.core
            .execution(task, intent, id, at, Some(target), Some(workspace))
            .await
    }
    async fn deferral(&self, request: &Json, id: &JsString, at: &JsString) -> IvyResult<()> {
        self.core.deferral(request, Some(id), at).await
    }
    async fn content(&self, content: &Json) -> IvyResult<()> {
        self.core.current_content(content).await
    }
    async fn artifact(&self, artifact: &Json) -> IvyResult<()> {
        self.core.artifact(artifact).await
    }
}
