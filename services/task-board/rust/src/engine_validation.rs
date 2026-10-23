//! Existing-action admission before retained publication. Current record,
//! execution, conversation and recovery checks remain explicit owner bindings.
use crate::{
    allocation::{AllocationStore, reserves_workspace, shared_project_key, shared_project_owner},
    checks::date,
    coordination::{GraphError, Parent, validate_parent},
    engine_actions::{ActionPolicies, ActionTaskRead},
    journal_store::Contracts,
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_workspaces::{LocalWorkspaces, workspace_key},
    native_lifecycle::LifecycleStore,
    operations::Store,
    wake::{WakeStore, comments_pending, queued_input, validate_dependencies},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::RpcClient;
use std::collections::HashSet;

#[allow(async_fn_in_trait)]
pub trait ActionValidationStore: AllocationStore + LifecycleStore + WakeStore {
    async fn reserve_available_workspace(
        &self,
        host: &JsString,
        path: &JsString,
        task: &JsString,
        operation: &JsString,
    ) -> IvyResult<Option<JsString>>;
}
impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    Q: LifecyclePages,
> ActionValidationStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    async fn reserve_available_workspace(
        &self,
        host: &JsString,
        path: &JsString,
        task: &JsString,
        operation: &JsString,
    ) -> IvyResult<Option<JsString>> {
        LocalWorkspaces {
            connection: self.journal.journal.connection,
            store: self.journal,
            normalize: workspace_key,
        }
        .reserve_available(host, path, task, operation)
        .await
    }
}
#[allow(async_fn_in_trait)]
pub trait ActionValidationChecks {
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()>;
    async fn fields(&self, fields: &Json, id: &JsString) -> IvyResult<()>;
    async fn execution_options(&self, fields: &Json, task: Option<&Json>) -> IvyResult<()>;
    async fn execution(
        &self,
        task: &Json,
        intent: &Json,
        id: &JsString,
        at: &JsString,
        target: &Json,
        workspace: &Json,
    ) -> IvyResult<Json>;
    async fn deferral(&self, request: &Json, id: &JsString, at: &JsString) -> IvyResult<()>;
    async fn content(&self, content: &Json) -> IvyResult<()>;
    async fn artifact(&self, artifact: &Json) -> IvyResult<()>;
}
pub struct ActionValidation<'a, S, P> {
    pub store: &'a S,
    pub checks: &'a P,
    pub task_read: &'a ActionTaskRead<'a>,
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn arr(v: &Json) -> IvyResult<&[Json]> {
    v.as_array().ok_or_else(IvyError::internal)
}
fn eq(v: &Json, k: &str, value: &str) -> bool {
    v.get(k) == Some(&value.into())
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(a: Option<&Json>, b: Option<&Json>) -> IvyResult<bool> {
    Ok(match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?
        }
        _ => false,
    })
}
fn one_of(v: &Json, k: &str, values: &[&str]) -> bool {
    values.iter().any(|value| eq(v, k, value))
}
fn terminal(v: &Json) -> bool {
    one_of(v, "workflowState", &["done", "cancelled"])
}
fn started(v: &Json) -> bool {
    f(v, "attemptCount").as_f64().is_some_and(|n| n > 0.0)
        || !f(v, "claim").is_null()
        || !f(v, "lastRun").is_null()
        || !f(v, "primaryResourceRef").is_null()
}
fn fields(request: &Json, task: &Json) -> Json {
    let mut fields = f(request, "fields").clone();
    if f(&fields, "userContact").is_null() {
        fields.insert("userContact", f(f(task, "fields"), "userContact").clone());
    }
    if f(&fields, "allowParallel").is_null() {
        fields.insert(
            "allowParallel",
            if f(f(task, "fields"), "allowParallel").is_null() {
                false.into()
            } else {
                f(f(task, "fields"), "allowParallel").clone()
            },
        );
    }
    fields
}
/// The original cross-record consistency checks. The owner still supplies its
/// current aggregate policy and canonical record validator; this is no default.
pub async fn check_aggregate<S: Store>(store: &S, task: &Json, id: &JsString) -> IvyResult<()> {
    let claim = f(task, "claim");
    if !claim.is_null() {
        let request = f(claim, "request");
        require(
            f(request, "taskId").as_js_string() == Some(id)
                && f(claim, "operationId") == f(request, "operationId")
                && hash_json(request)? == s(claim, "requestHash")?.to_utf8()?
                && f(claim, "attempt") == f(task, "attemptCount")
                && !f(claim, "run").is_null()
                && same(claim.get("run"), task.get("lastRun"))?,
            "task_board_claim_mismatch",
            "The published Task claim must retain its exact request, target, attempt and current Run.",
        )?;
    }
    if !f(task, "lastRun").is_null() {
        let run = store
            .read("task-board/run", f(task, "lastRun"), true)
            .await?
            .value;
        let request = f(&run, "originalRequest");
        require(
            f(&run, "taskId").as_js_string() == Some(id)
                && f(request, "taskId").as_js_string() == Some(id)
                && f(&run, "operationId") == f(request, "operationId")
                && s(&run, "requestHash")?.to_utf8()? == hash_json(request)?
                && f(&run, "attempt").as_f64().ok_or_else(IvyError::internal)?
                    <= f(task, "attemptCount")
                        .as_f64()
                        .ok_or_else(IvyError::internal)?,
            "task_board_run_mismatch",
            "The retained Run must belong to this task and its original execution intent.",
        )?;
        if !claim.is_null() {
            require(
                f(&run, "attempt") == f(claim, "attempt")
                    && f(&run, "operationId") == f(claim, "operationId")
                    && f(&run, "callerPrincipalId") == f(claim, "callerPrincipalId")
                    && same(run.get("owner"), claim.get("owner"))?
                    && same(run.get("target"), claim.get("target"))?,
                "task_board_claim_mismatch",
                "The current Run must have the original claimant identity and owner.",
            )?;
        }
    }
    if !f(task, "latestResult").is_null() {
        let result = store
            .read("task-board/result", f(task, "latestResult"), true)
            .await?
            .value;
        require(
            f(&result, "taskId").as_js_string() == Some(id)
                && (!eq(task, "workflowState", "review") || eq(&result, "kind", "final")),
            "task_board_result_mismatch",
            "Review tasks must retain their own final Result.",
        )?;
    }
    if !f(task, "acceptedReview").is_null() {
        let review = store
            .read("task-board/review", f(task, "acceptedReview"), true)
            .await?
            .value;
        require(
            f(&review, "taskId").as_js_string() == Some(id)
                && (!eq(task, "workflowState", "done")
                    || same(review.get("result"), task.get("latestResult"))?),
            "task_board_review_mismatch",
            "Task acceptance must retain its own exact Result.",
        )?;
    }
    if !f(task, "lastHistory").is_null() {
        require(
            f(
                &store
                    .read("task-board/history", f(task, "lastHistory"), true)
                    .await?
                    .value,
                "taskId",
            )
            .as_js_string()
                == Some(id),
            "task_board_history_mismatch",
            "The latest history must belong to this task.",
        )?;
    }
    Ok(())
}
impl<S: ActionValidationStore, P: ActionValidationChecks> ActionValidation<'_, S, P> {
    async fn parent(&self, id: &JsString, own: &JsString) -> IvyResult<()> {
        validate_parent(id.clone(), Some(own.clone()), |id| async move {
            let d = self
                .store
                .lifecycle_read("task-board/task", &Json::String(id), true, false)
                .await?;
            Ok::<_, IvyError>(Parent {
                archived: f(&d.metadata, "effectivelyArchived").as_bool() == Some(true),
                agent_controlled: eq(f(&d.document.value, "fields"), "control", "agent"),
                terminal: terminal(&d.document.value),
                parent: f(&d.document.value, "parentTaskId")
                    .as_js_string()
                    .filter(|s| !s.0.is_empty())
                    .cloned(),
            })
        })
        .await
        .map_err(|e| match e {
            GraphError::Read(e) => e,
            e => {
                let (code, message) = e.policy_error().unwrap();
                IvyError::new(code, message)
            }
        })
    }
    fn no_claim(&self, task: &Json) -> IvyResult<()> {
        require(
            f(task, "claim").is_null(),
            "task_board_claim_active",
            "Resolve the existing execution claim before this action.",
        )
    }
    fn no_owner(&self, owner: Option<&JsString>, prefix: &str) -> IvyResult<()> {
        if let Some(owner) = owner {
            let mut message = JsString::from(prefix);
            message.0.extend(&owner.0);
            message.0.push(46);
            Err(IvyError::new_js(
                "task_board_workspace_reserved".into(),
                message,
            ))
        } else {
            Ok(())
        }
    }
    pub async fn validate(
        &self,
        request: &Json,
        task: &Json,
        actor: &Json,
        at: &JsString,
        operation: &JsString,
    ) -> IvyResult<()> {
        let comment_request = one_of(request, "action", &["start", "continue"])
            && f(request, "commentIds")
                .as_array()
                .is_some_and(|ids| !ids.is_empty());
        let execution_change = f(task, "executionChangeRequested").as_bool() == Some(true)
            && one_of(request, "action", &["start", "continue", "defer"]);
        require(
            !terminal(task)
                || comment_request
                || execution_change
                || one_of(
                    request,
                    "action",
                    &["edit", "retargetQuestion", "comment", "uploadAttachment"],
                )
                || eq(request, "action", "transition")
                    && (eq(request, "workflowState", "backlog")
                        || eq(task, "workflowState", "done")
                            && eq(request, "workflowState", "todo")),
            "task_board_invalid_transition",
            "This task is terminal for the requested action.",
        )?;
        if eq(actor, "source", "scheduler") {
            require(
                one_of(
                    request,
                    "action",
                    &["start", "continue", "defer", "transition"],
                ) && (eq(f(task, "fields"), "control", "agent")
                    || comment_request
                    || execution_change
                    || eq(request, "action", "defer") && comments_pending(task)),
                "task_board_user_controlled",
                "Automatic allocation requires agent control or continuation of an explicitly triggered turn.",
            )?;
        }
        let id = s(request, "taskId")?;
        match s(request, "action")?.to_utf8()?.as_str() {
            "retargetQuestion" => {
                let questions = ivy_sdk::open_task_questions(task, None)?;
                let question = questions
                    .iter()
                    .find(|question| {
                        f(question, "commentId") == f(request, "questionId")
                            && f(question, "nativeInput").is_null()
                    })
                    .ok_or_else(|| {
                        IvyError::new(
                            "task_board_comment_mismatch",
                            "Only an unresolved ordinary question can change its recipient.",
                        )
                    })?;
                require(
                    eq(actor, "source", "user")
                        || eq(actor, "source", "worker")
                            && eq(question, "authorKind", "agent")
                            && f(f(question, "author"), "principalId") == f(actor, "principalId"),
                    "task_board_forbidden",
                    "Only the user or the original agent can correct a question recipient.",
                )?;
                if eq(f(request, "recipient"), "kind", "task") {
                    require(
                        f(f(request, "recipient"), "taskId") != f(request, "taskId"),
                        "invalid_arguments",
                        "A question must address a different task.",
                    )?;
                    self.store
                        .read(
                            "task-board/task",
                            f(f(request, "recipient"), "taskId"),
                            true,
                        )
                        .await?;
                }
            }
            "edit" => {
                let fields = fields(request, task);
                if !f(request, "parentTaskId").is_null() {
                    self.parent(s(request, "parentTaskId")?, id).await?;
                }
                require(
                    !started(task)
                        || same(
                            f(task, "fields").get("executionRequirement"),
                            fields.get("executionRequirement"),
                        )? && same(
                            f(task, "fields").get("workspaceRequirement"),
                            fields.get("workspaceRequirement"),
                        )?,
                    "task_board_reassign_required",
                    "Host and workspace are fixed once the task has run; use the reassign workflow to move it.",
                )?;
                if !f(task, "claim").is_null() {
                    for key in [
                        "title",
                        "description",
                        "acceptanceCriteria",
                        "control",
                        "requiredCapabilities",
                        "executionRequirement",
                        "workspaceRequirement",
                        "allowParallel",
                    ] {
                        require(
                            same(f(task, "fields").get(key), fields.get(key))?,
                            "task_board_claim_active",
                            "An active claim prevents changes to the running assignment.",
                        )?;
                    }
                }
                self.checks.fields(&fields, id).await?;
                date(Some(f(request, "wakeAt")))?;
                if let Some(ids) = request.get("dependencies").filter(|value| !value.is_null()) {
                    validate_dependencies(self.store, Some(id), arr(ids)?).await?;
                }
                if fields.get("control") != f(task, "fields").get("control")
                    || !same(
                        fields.get("executionRequirement"),
                        f(task, "fields").get("executionRequirement"),
                    )?
                    || !same(
                        Some(f(&fields, "nativeOptions")),
                        Some(f(f(task, "fields"), "nativeOptions")),
                    )?
                {
                    self.checks.execution_options(&fields, Some(task)).await?;
                }
            }
            "transition" => {
                self.no_claim(task)?;
                self.checks.fields(f(task, "fields"), id).await?;
                let legal = match s(task, "workflowState")?.to_utf8()?.as_str() {
                    "backlog" => &["todo", "cancelled"][..],
                    "todo" => &["backlog", "in_progress", "waiting", "cancelled"],
                    "in_progress" => &["backlog", "waiting", "done", "cancelled"],
                    "waiting" => &["backlog", "todo", "in_progress", "done", "cancelled"],
                    "review" => &["backlog", "done", "todo", "cancelled"],
                    "done" => &["backlog", "todo"],
                    "cancelled" => &["backlog"],
                    _ => return Err(IvyError::internal()),
                };
                require(
                    one_of(request, "workflowState", legal)
                        || eq(request, "workflowState", "todo")
                            && eq(task, "workflowState", "todo")
                            && (!f(task, "completionIssue").is_null()
                                || !f(task, "runtimeWait").is_null()),
                    "task_board_invalid_transition",
                    "Requested workflow destination is not legal from the current state.",
                )?;
                require(
                    eq(f(task, "fields"), "control", "user")
                        || !eq(request, "workflowState", "in_progress"),
                    "task_board_user_controlled",
                    "Starting agent execution is owned by TaskBoard.",
                )?;
            }
            "start" | "continue" => {
                self.no_claim(task)?;
                require(
                    one_of(task, "workflowState", &["todo", "waiting", "review"])
                        || comment_request
                        || execution_change,
                    "task_board_invalid_transition",
                    "Automatic execution starts from Todo or a resumable wait.",
                )?;
                if eq(request, "action", "continue") {
                    require(
                        !f(task, "primaryResourceRef").is_null(),
                        "task_board_primary_required",
                        "Continuation requires the saved primary native context.",
                    )?;
                }
                require(
                    (eq(f(task, "fields"), "control", "agent")
                        || comment_request
                        || execution_change)
                        && eq(actor, "source", "scheduler"),
                    "task_board_user_controlled",
                    "Native Task allocation is automatic for agent-controlled work.",
                )?;
                if comment_request {
                    require(
                        comments_pending(task)
                            && arr(f(request, "commentIds"))?.iter().all(|id| {
                                arr(f(task, "commentDeliveries")).is_ok_and(|items| {
                                    items.iter().any(|delivery| {
                                        f(delivery, "commentId") == id
                                            && eq(delivery, "state", "queued")
                                    })
                                }) && arr(f(task, "comments")).is_ok_and(|items| {
                                    items.iter().any(|comment| {
                                        f(comment, "commentId") == id && queued_input(comment)
                                    })
                                })
                            }),
                        "task_board_invalid_transition",
                        "A comment-triggered turn requires queued external comments.",
                    )?;
                }
                let plan = self
                    .checks
                    .execution(
                        task,
                        f(request, "intent"),
                        id,
                        at,
                        f(request, "target"),
                        f(request, "workspace"),
                    )
                    .await?;
                let input = f(f(&plan, "turnStart"), "input");
                require(
                    input.as_array().is_some_and(|v| !v.is_empty()),
                    "task_board_input_required",
                    "A native execution plan must contain its explicit first-turn input.",
                )?;
                if eq(request, "action", "continue") && !eq(actor, "source", "scheduler") {
                    require(
                        arr(input)?
                            .iter()
                            .filter(|item| {
                                item.as_object().is_some()
                                    && eq(item, "type", "text")
                                    && f(item, "text") == f(request, "feedback")
                            })
                            .count()
                            == 1,
                        "task_board_feedback_missing",
                        "The continuation plan must include the exact user feedback exactly once as native text input.",
                    )?;
                }
                if reserves_workspace(f(f(task, "fields"), "workspaceRequirement")) {
                    let owner = self
                        .store
                        .reserve_available_workspace(
                            s(f(request, "target"), "hostId")?,
                            s(f(request, "workspace"), "intendedPath")?,
                            id,
                            operation,
                        )
                        .await?;
                    self.no_owner(owner.as_ref(), "Workspace is already reserved by Task ")?;
                } else {
                    let owner = shared_project_owner(
                        self.store,
                        task,
                        id,
                        f(request, "target"),
                        f(request, "workspace"),
                    )
                    .await?;
                    self.no_owner(owner.as_ref(), "Project is already in use by Task ")?;
                    if let Some(key) = shared_project_key(
                        f(f(task, "fields"), "workspaceRequirement"),
                        f(request, "workspace"),
                    ) {
                        if f(f(task, "fields"), "allowParallel").as_bool() != Some(true) {
                            let owner = self
                                .store
                                .reserve_available_workspace(
                                    s(f(request, "target"), "hostId")?,
                                    &key,
                                    id,
                                    operation,
                                )
                                .await?;
                            self.no_owner(owner.as_ref(), "Project is already reserved by Task ")?;
                        }
                    }
                }
            }
            "defer" => {
                require(
                    f(task, "claim").is_null() || eq(actor, "source", "worker"),
                    "task_board_claim_active",
                    "Only the active worker can change its wake-up schedule.",
                )?;
                require(
                    f(request, "runtimeWait").is_null()
                        || one_of(actor, "source", &["scheduler", "recovery"]),
                    "task_board_forbidden",
                    "Runtime availability is owned by the execution system.",
                )?;
                self.checks.deferral(request, id, at).await?;
            }
            "reassign" => {
                let mut fields = f(task, "fields").clone();
                fields.insert(
                    "executionRequirement",
                    f(request, "executionRequirement").clone(),
                );
                fields.insert(
                    "workspaceRequirement",
                    f(request, "workspaceRequirement").clone(),
                );
                self.checks.fields(&fields, id).await?;
                self.checks.execution_options(&fields, None).await?;
                require(
                    same(request.get("previousRun"), task.get("lastRun"))?,
                    "task_board_prior_run_mismatch",
                    "Reassignment must acknowledge the exact saved prior attempt.",
                )?;
                if !f(task, "claim").is_null()
                    || eq(f(task, "runtimeWait"), "reason", "external_outcome")
                {
                    require(
                        !f(request, "previousRun").is_null()
                            && f(request, "acknowledgeUnresolved").as_bool() == Some(true),
                        "task_board_unresolved_ack_required",
                        "The unresolved prior attempt requires its exact Run pin and explicit acknowledgement.",
                    )?;
                }
            }
            "cancel" => {
                if !f(task, "claim").is_null() {
                    require(
                        !f(f(task, "claim"), "run").is_null(),
                        "task_board_publication_pending",
                        "Recover the existing claim's Run before cancelling.",
                    )?;
                    let run = self
                        .store
                        .read("task-board/run", f(f(task, "claim"), "run"), true)
                        .await?
                        .value;
                    require(
                        f(&run, "taskId").as_js_string() == Some(id)
                            && f(&run, "cancellation").is_null()
                            && !one_of(&run, "phase", &["completed", "failed", "cancelled"]),
                        "task_board_run_mismatch",
                        "Reconcile the existing Run or cancellation before another cancellation intent.",
                    )?;
                } else {
                    require(
                        !eq(f(task, "runtimeWait"), "reason", "external_outcome"),
                        "task_board_outcome_unresolved",
                        "An unresolved external outcome cannot be declared cancelled.",
                    )?;
                }
            }
            "review" => {
                self.no_claim(task)?;
                require(
                    eq(task, "workflowState", "review")
                        && same(task.get("latestResult"), request.get("result"))?,
                    "task_board_result_mismatch",
                    "Accept only the task's exact saved final Result.",
                )?;
                let result = self
                    .store
                    .read("task-board/result", f(request, "result"), true)
                    .await?
                    .value;
                require(
                    f(&result, "taskId").as_js_string() == Some(id) && eq(&result, "kind", "final"),
                    "task_board_result_mismatch",
                    "The acceptance Result must be final and belong to this task.",
                )?;
            }
            "saveResult" => {
                if !f(request, "run").is_null() {
                    let run = self
                        .store
                        .read("task-board/run", f(request, "run"), true)
                        .await?
                        .value;
                    require(
                        f(&run, "taskId").as_js_string() == Some(id)
                            && (f(task, "claim").is_null()
                                || same(f(task, "claim").get("run"), request.get("run"))?),
                        "task_board_run_mismatch",
                        "Material must identify this task's exact current claimed Run.",
                    )?;
                } else {
                    require(
                        f(task, "claim").is_null(),
                        "task_board_run_required",
                        "Material for an active attempt must include its exact Run pin.",
                    )?;
                }
                self.checks.content(f(request, "content")).await?;
                if eq(request, "kind", "final") {
                    self.no_claim(task)?;
                    require(
                        eq(f(task, "fields"), "control", "user") && eq(actor, "source", "user"),
                        "task_board_native_evidence_required",
                        "Only user-controlled manual work can finish without owner-observed native evidence.",
                    )?;
                }
            }
            "comment" => {
                require(
                    s(request, "body")?.0.len() <= 65536,
                    "invalid_arguments",
                    "A comment must not exceed 65536 characters.",
                )?;
                require(
                    !eq(request, "purpose", "question")
                        || eq(actor, "source", "native")
                        || !f(request, "recipient").is_null(),
                    "invalid_arguments",
                    "A question must identify its recipient: user or a specific task.",
                )?;
                require(
                    f(request, "recipient").is_null() || eq(request, "purpose", "question"),
                    "invalid_arguments",
                    "Only questions declare a recipient.",
                )?;
                require(
                    f(request, "contactUser").as_bool() != Some(true)
                        || !eq(f(request, "recipient"), "kind", "task"),
                    "invalid_arguments",
                    "Task questions cannot contact the user.",
                )?;
                if eq(f(request, "recipient"), "kind", "task") {
                    require(
                        f(f(request, "recipient"), "taskId") != f(request, "taskId")
                            && f(request, "sourceTaskId").is_null()
                            && f(request, "nativeInput").is_null(),
                        "invalid_arguments",
                        "A task question belongs to its sender and names a different receiving task.",
                    )?;
                    self.store
                        .read(
                            "task-board/task",
                            f(f(request, "recipient"), "taskId"),
                            true,
                        )
                        .await?;
                }
                require(
                    f(request, "questionSource").is_null()
                        || eq(actor, "source", "native")
                            && f(request, "sourceTaskId")
                                == f(f(request, "questionSource"), "taskId"),
                    "task_board_forbidden",
                    "Only TaskBoard delivers a canonical task question.",
                )?;
                require(
                    f(request, "handoff").is_null() || f(request, "sourceTaskId").is_null(),
                    "task_board_handoff_source",
                    "Omit sourceTaskId on your own handoff; it is only for a directed message to another task.",
                )?;
                require(
                    f(request, "handoff").is_null()
                        || eq(actor, "source", "worker")
                            && eq(request, "purpose", "handoff")
                            && f(request, "sourceTaskId").is_null()
                            && !f(f(task, "claim"), "run").is_null(),
                    "task_board_forbidden",
                    "Only the active worker can declare its own completion intent.",
                )?;
                require(
                    !eq(actor, "source", "worker")
                        || !eq(request, "purpose", "handoff")
                        || !f(request, "handoff").is_null(),
                    "task_board_handoff_required",
                    "A worker handoff must explicitly declare review readiness or waiting.",
                )?;
                if !f(request, "handoff").is_null() {
                    date(Some(f(f(request, "handoff"), "wakeAt")))?;
                    if let Some(ids) = f(request, "handoff")
                        .get("dependencies")
                        .filter(|value| !value.is_null())
                    {
                        validate_dependencies(self.store, Some(id), arr(ids)?).await?;
                    }
                }
                for id in f(request, "resolvesQuestions").as_array().unwrap_or(&[]) {
                    require(
                        arr(f(task, "comments"))?.iter().any(|comment| {
                            f(comment, "commentId") == id
                                && eq(comment, "purpose", "question")
                                && f(comment, "nativeInput").is_null()
                                && f(comment, "sourceTaskId").is_null()
                                && (!eq(f(comment, "recipient"), "kind", "task")
                                    || eq(actor, "source", "user")
                                    || f(request, "sourceTaskId")
                                        == f(f(comment, "recipient"), "taskId")
                                    || f(request, "sourceTaskId").is_null())
                        }),
                        "task_board_comment_mismatch",
                        "Only existing ordinary ticket questions can be explicitly resolved.",
                    )?;
                }
                if !f(request, "sourceTaskId").is_null() {
                    require(
                        one_of(actor, "source", &["worker", "native"])
                            && f(request, "sourceTaskId") != f(request, "taskId")
                            && f(request, "moveToTodo").as_bool() != Some(true),
                        "task_board_forbidden",
                        "Directed agent comments must identify another source task and cannot reopen Done.",
                    )?;
                    let source = (self.task_read)(f(request, "sourceTaskId").clone(), true)
                        .await?
                        .document
                        .value;
                    let routed = !f(request, "questionSource").is_null()
                        && arr(f(&source, "comments"))?.iter().any(|comment| {
                            f(comment, "commentId") == f(f(request, "questionSource"), "commentId")
                                && eq(comment, "purpose", "question")
                                && eq(f(comment, "recipient"), "kind", "task")
                                && f(f(comment, "recipient"), "taskId") == f(request, "taskId")
                        });
                    require(
                        f(request, "questionSource").is_null() || routed,
                        "task_board_forbidden",
                        "The canonical question is not addressed to this task.",
                    )?;
                    if eq(actor, "source", "native") {
                        self.checks
                            .aggregate(&source, s(request, "sourceTaskId")?)
                            .await?;
                    }
                    require(
                        eq(f(&source, "fields"), "control", "agent")
                            && if eq(actor, "source", "native") {
                                routed
                                    || f(&source, "parentTaskId") == f(request, "taskId")
                                        && eq(&source, "workflowState", "review")
                                        && !f(&source, "latestResult").is_null()
                                        && f(&source, "claim").is_null()
                                        && f(&source, "publication").is_null()
                            } else {
                                !f(&source, "claim").is_null()
                            },
                        "task_board_forbidden",
                        "Directed messages require an active source task or its verified delegated result.",
                    )?;
                }
                require(
                    f(request, "nativeInput").is_null() || eq(actor, "source", "native"),
                    "task_board_forbidden",
                    "Native input observations are recorded only by TaskBoard.",
                )?;
                require(
                    f(request, "contactUser").as_bool() != Some(true)
                        || eq(request, "purpose", "question"),
                    "invalid_arguments",
                    "Only important questions can request user contact.",
                )?;
                for attachment in arr(f(request, "attachments"))? {
                    self.checks
                        .artifact(&Json::object([
                            ("object", f(attachment, "object").clone()),
                            ("label", f(attachment, "filename").clone()),
                            ("mediaType", "application/octet-stream".into()),
                            ("contentHash", f(attachment, "contentHash").clone()),
                            ("byteLength", f(attachment, "byteLength").clone()),
                        ]))
                        .await?;
                }
                let comments = arr(f(task, "comments"))?;
                require(
                    !comments
                        .iter()
                        .any(|c| f(c, "commentId") == f(request, "commentId"))
                        && (f(request, "replyTo").is_null()
                            || comments
                                .iter()
                                .any(|c| f(c, "commentId") == f(request, "replyTo"))),
                    "task_board_comment_mismatch",
                    "Comment identity or reply target is invalid.",
                )?;
                let mut requests = HashSet::new();
                let mut responses = HashSet::new();
                for c in comments {
                    for r in arr(f(c, "requests"))? {
                        requests.insert(s(r, "requestId")?.clone());
                    }
                    for r in arr(f(c, "responses"))? {
                        responses.insert(s(r, "requestId")?.clone());
                    }
                }
                for item in arr(f(request, "requests"))? {
                    require(
                        requests.insert(s(item, "requestId")?.clone()),
                        "mutation_conflict",
                        "Request identity is already used.",
                    )?;
                }
                for item in arr(f(request, "responses"))? {
                    let id = s(item, "requestId")?;
                    require(
                        requests.contains(id),
                        "task_board_response_request_missing",
                        "A typed response must reference an existing typed request.",
                    )?;
                    require(
                        responses.insert(id.clone()),
                        "task_board_request_already_answered",
                        "This typed request already has a response.",
                    )?;
                }
            }
            "uploadAttachment" => {
                let text = s(request, "bytesBase64")?;
                let bytes = text.to_utf8().ok().and_then(|s| STANDARD.decode(s).ok());
                require(
                    bytes.as_ref().is_some_and(|bytes| {
                        bytes.len() <= 8 * 1024 * 1024
                            && JsString::from(STANDARD.encode(bytes)) == *text
                    }),
                    "invalid_arguments",
                    "Attachment must be canonical base64 up to 8 MiB.",
                )?;
                require(
                    !arr(f(task, "attachments"))?
                        .iter()
                        .any(|a| f(a, "attachmentId") == f(request, "attachmentId")),
                    "mutation_conflict",
                    "Attachment identity is already used.",
                )?;
            }
            _ => {}
        }
        Ok(())
    }
}
impl<S: ActionValidationStore, P: ActionValidationChecks> ActionPolicies
    for ActionValidation<'_, S, P>
{
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()> {
        self.checks.aggregate(task, id).await
    }
    async fn validate_action(
        &self,
        request: &Json,
        task: &Json,
        actor: &Json,
        at: &JsString,
        operation: &JsString,
    ) -> IvyResult<()> {
        self.validate(request, task, actor, at, operation).await
    }
}
