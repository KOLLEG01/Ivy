//! Retained publication for existing Tasks. Aggregate/action admission is bound
//! to the owner's accepted policy; no frozen validation fallback is supplied.
use crate::{
    deliveries::initial_delivery,
    engine_publication::{ExistingTaskPublication, history},
    journal_store::Contracts,
    journal_store::persistent_reads::task_comments::validate_task_content,
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    native_lifecycle::{
        LifecycleDocument, LifecycleEngine, LifecycleOperations, LifecyclePublication,
        LifecycleStore, NativeLifecycle,
    },
    native_plan::read_plan,
    operations::Document,
    wake::{WakeStore, dependency_snapshot, peer_comment},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::RpcClient;

#[allow(async_fn_in_trait)]
pub trait ActionStore: LifecycleStore + WakeStore {
    async fn write_attachment(
        &self,
        task: &JsString,
        operation: &JsString,
        attachment: &JsString,
        bytes: &[u8],
    ) -> IvyResult<Json>;
}
impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    Q: LifecyclePages,
> ActionStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    async fn write_attachment(
        &self,
        task: &JsString,
        operation: &JsString,
        attachment: &JsString,
        bytes: &[u8],
    ) -> IvyResult<Json> {
        self.journal
            .write_attachment(task, operation, attachment, bytes)
            .await
    }
}
#[allow(async_fn_in_trait)]
pub trait ActionPolicies {
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()>;
    async fn validate_action(
        &self,
        request: &Json,
        task: &Json,
        actor: &Json,
        at: &JsString,
        operation: &JsString,
    ) -> IvyResult<()>;
}
#[allow(async_fn_in_trait)]
pub trait ActionRestore {
    async fn restore_for_execution(&self, operation: &Document, task: &Document) -> IvyResult<()>;
}
impl<S: LifecycleStore, E: LifecycleEngine, O: LifecycleOperations> ActionRestore
    for NativeLifecycle<'_, S, E, O>
{
    async fn restore_for_execution(&self, operation: &Document, task: &Document) -> IvyResult<()> {
        NativeLifecycle::restore_for_execution(self, operation, task).await
    }
}
pub type ActionTaskRead<'a> = dyn Fn(Json, bool) -> futures_util::future::LocalBoxFuture<'a, IvyResult<LifecycleDocument>>
    + 'a;
pub struct TaskActions<'a, S, P, L> {
    pub store: &'a S,
    pub settings: &'a Json,
    pub catalogs: &'a CheckedNativeContracts,
    pub policies: &'a P,
    pub lifecycle: &'a L,
    pub task_read: &'a ActionTaskRead<'a>,
}
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    v.required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn a(v: &Json) -> IvyResult<&[Json]> {
    v.as_array().ok_or_else(IvyError::internal)
}
fn eq(v: &Json, key: &str, want: &str) -> bool {
    v.get(key) == Some(&Json::from(want))
}
fn present(v: &Json) -> bool {
    !v.is_null()
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(left: Option<&Json>, right: Option<&Json>) -> IvyResult<bool> {
    Ok(match (left, right) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?
        }
        _ => false,
    })
}
pub(crate) fn same_assignment(left: &Json, right: &Json) -> IvyResult<bool> {
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
        if !same(left.get(key), right.get(key))? {
            return Ok(false);
        }
    }
    Ok(true)
}
/// Persisted completion identity excludes contact, priority and due-date edits.
pub fn handoff_work(task: &Json) -> IvyResult<Json> {
    require(
        present(f(task, "claim")),
        "task_board_handoff_changed",
        "The completion intent requires its original active claim.",
    )?;
    let mut fields = f(task, "fields").clone();
    for key in ["priority", "dueAt", "userContact"] {
        fields.remove(key);
    }
    Ok(Json::object([
        (
            "claimOperationId",
            f(f(task, "claim"), "operationId").clone(),
        ),
        ("workRevision", f(task, "workRevision").clone()),
        (
            "contentHash",
            hash_json(&Json::object([
                ("fields", fields),
                ("parentTaskId", f(task, "parentTaskId").clone()),
            ]))?
            .into(),
        ),
        ("primaryResourceRef", f(task, "primaryResourceRef").clone()),
    ]))
}
fn increment(v: &mut Json, key: &str) -> IvyResult<()> {
    let n = v.required(key)?.as_f64().ok_or_else(IvyError::internal)?;
    v.insert(key, Json::Number(n + 1.0));
    Ok(())
}
fn append(v: &Json, item: Json) -> IvyResult<Json> {
    let mut values = a(v)?.to_vec();
    values.push(item);
    Ok(Json::Array(values))
}
fn author(request: &Json, actor: &Json) -> &'static str {
    if eq(actor, "source", "user") {
        "user"
    } else if eq(actor, "source", "worker")
        || (eq(actor, "source", "native") && present(f(request, "sourceTaskId")))
        || (present(f(request, "nativeInput")) && !present(f(request, "replyTo")))
    {
        "agent"
    } else {
        "system"
    }
}
fn queued(comment: &Json, at: &JsString, operation: &Json) -> Json {
    Json::object([
        ("commentId", f(comment, "commentId").clone()),
        ("state", "queued".into()),
        ("operationId", operation.clone()),
        ("updatedAt", Json::String(at.clone())),
        ("code", Json::Null),
    ])
}
fn comment(request: &Json, previous: &Json, actor: &Json, at: &JsString, kind: &str) -> Json {
    let mut c = Json::object([
        ("commentId", f(request, "commentId").clone()),
        (
            "sequence",
            Json::Number(f(previous, "comments").as_array().map_or(0, |v| v.len()) as f64 + 1.0),
        ),
        ("author", actor.clone()),
        ("authorKind", kind.into()),
        ("body", f(request, "body").clone()),
        ("requests", f(request, "requests").clone()),
        ("responses", f(request, "responses").clone()),
        ("delivery", Json::Null),
        ("attachments", f(request, "attachments").clone()),
        ("run", f(previous, "lastRun").clone()),
        ("turnId", Json::Null),
        ("replyTo", f(request, "replyTo").clone()),
        ("createdAt", Json::String(at.clone())),
        ("operationId", f(request, "operationId").clone()),
    ]);
    for key in [
        "purpose",
        "recipient",
        "questionSource",
        "nativeInput",
        "sourceTaskId",
        "handoff",
        "resolvesQuestions",
    ] {
        if present(f(request, key)) {
            c.insert(key, f(request, key).clone());
        }
    }
    c
}
fn system_comment(
    id: &Json,
    body: JsString,
    previous: &Json,
    actor: &Json,
    at: &JsString,
    run: &Json,
) -> Json {
    Json::object([
        ("commentId", id.clone()),
        (
            "sequence",
            Json::Number(f(previous, "comments").as_array().map_or(0, |v| v.len()) as f64 + 1.0),
        ),
        ("author", actor.clone()),
        ("authorKind", "system".into()),
        ("body", Json::String(body)),
        ("requests", Json::Array(vec![])),
        ("responses", Json::Array(vec![])),
        ("delivery", Json::Null),
        ("attachments", Json::Array(vec![])),
        ("run", run.clone()),
        ("turnId", Json::Null),
        ("replyTo", Json::Null),
        ("createdAt", Json::String(at.clone())),
        ("operationId", id.clone()),
    ])
}
pub fn result_comment_fits(
    task: &Json,
    comment: &Json,
    publication: &Json,
    task_id: &JsString,
) -> IvyResult<bool> {
    let mut projected = task.clone();
    projected.insert("publication", Json::Null);
    projected.insert("lastHistory", publication.clone());
    let mut c = comment.clone();
    c.insert(
        "delivery",
        if eq(f(task, "fields"), "userContact", "ticket") {
            Json::Null
        } else {
            publication.clone()
        },
    );
    projected.insert("comments", append(f(task, "comments"), c)?);
    increment(&mut projected, "agentCommentCount")?;
    match validate_task_content(&projected, task_id) {
        Ok(_) => Ok(true),
        Err(e) if e.code == "content_too_large" => Ok(false),
        Err(e) => Err(e),
    }
}
fn bytes(request: &Json) -> IvyResult<Vec<u8>> {
    // The mandatory action admission already checked canonical base64 and its
    // size before reservation. Recovery reuses that same admitted request.
    STANDARD
        .decode(s(request, "bytesBase64")?.to_utf8()?)
        .map_err(|_| IvyError::internal())
}
fn attachment(request: &Json, object: Json, bytes: &[u8], actor: &Json, at: &JsString) -> Json {
    Json::object([
        ("attachmentId", f(request, "attachmentId").clone()),
        ("object", object),
        ("filename", f(request, "filename").clone()),
        ("mediaType", f(request, "mediaType").clone()),
        ("byteLength", Json::Number(bytes.len() as f64)),
        ("contentHash", digest(bytes).into()),
        ("uploader", actor.clone()),
        ("createdAt", Json::String(at.clone())),
    ])
}
impl<S: ActionStore, P: ActionPolicies, L: ActionRestore> TaskActions<'_, S, P, L> {
    async fn schedule_fields(&self, next: &mut Json, request: &Json) -> IvyResult<()> {
        if let Some(wake) = request.get("wakeAt") {
            next.insert("wakeAt", wake.clone());
        }
        if let Some(ids) = request.get("dependencies").filter(|value| !value.is_null()) {
            next.insert("dependencies", ids.clone());
            next.insert(
                "observedDependencies",
                dependency_snapshot(self.store, next).await?,
            );
        }
        Ok(())
    }
    async fn delivery<Q: LifecyclePublication>(
        &self,
        steps: &mut Q,
        request: &Json,
        c: &Json,
        previous: &Json,
        at: &JsString,
    ) -> IvyResult<Json> {
        let publication = steps.pin()?;
        let value = initial_delivery(
            f(self.settings, "phoneTarget"),
            &publication,
            s(request, "taskId")?,
            s(c, "commentId")?,
            s(f(previous, "fields"), "userContact")?,
            at,
        );
        steps
            .create("task-board/delivery", &value, &"Comment delivery".into())
            .await
    }
    async fn program<Q: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut Q,
        mut outcome: Json,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json> {
        let op = &operation.value;
        let request = op.required("request")?;
        let actor = op.required("actor")?;
        let at = s(op, "createdAt")?;
        let id = s(request, "taskId")?;
        let first = a(op.required("writes")?)?.is_empty();
        let writes = a(op.required("writes")?)?;
        let admission = eq(request, "action", "start")
            || eq(request, "action", "continue")
            || eq(actor, "source", "scheduler") && eq(request, "action", "defer");
        let append_only =
            eq(request, "action", "comment") || eq(request, "action", "uploadAttachment");
        let address = if first && (present(f(op, "handoffWork")) || append_only || admission) {
            Json::String(id.clone())
        } else {
            Json::object([
                ("objectId", Json::String(id.clone())),
                (
                    "revision",
                    writes
                        .first()
                        .and_then(|write| f(write, "destination").get("expectedRevision"))
                        .unwrap_or(f(request, "expectedRevision"))
                        .clone(),
                ),
            ])
        };
        let mut base = (self.task_read)(address, true).await?;
        if first && present(f(op, "handoffRun")) {
            let expected = if present(f(op, "handoffWork")) {
                f(op, "handoffWork").clone()
            } else {
                handoff_work(&base.document.value)?
            };
            let current = if present(f(op, "handoffWork")) {
                base
            } else {
                (self.task_read)(Json::String(id.clone()), true).await?
            };
            let mut observed = handoff_work(&current.document.value)?;
            if observed.get("contentHash") != expected.get("contentHash")
                && f(&current.document.value, "executionChangeRequested").as_bool() == Some(true)
            {
                let original = (self.task_read)(
                    Json::object([
                        ("objectId", Json::String(id.clone())),
                        ("revision", f(request, "expectedRevision").clone()),
                    ]),
                    true,
                )
                .await?;
                let mut adjusted = current.document.value.clone();
                let mut fields = f(&adjusted, "fields").clone();
                if let Some(options) = f(&original.document.value, "fields")
                    .get("nativeOptions")
                    .filter(|value| value.as_object().is_some())
                {
                    fields.insert("nativeOptions", options.clone());
                } else {
                    fields.remove("nativeOptions");
                }
                adjusted.insert("fields", fields);
                observed.insert(
                    "contentHash",
                    f(&handoff_work(&adjusted)?, "contentHash").clone(),
                );
            }
            observed.insert("workRevision", Json::Number(0.0));
            let mut expected = expected;
            expected.insert("workRevision", Json::Number(0.0));
            require(
                f(f(f(&current.document.value, "claim"), "run"), "objectId")
                    == f(f(op, "handoffRun"), "objectId")
                    && same(Some(&observed), Some(&expected))?,
                "task_board_handoff_changed",
                "The original handoff no longer belongs to the same unchanged work and execution claim.",
            )?;
            base = current;
        }
        if first
            && admission
            && base.document.pin.get("revision") != request.get("expectedRevision")
        {
            let original = (self.task_read)(
                Json::object([
                    ("objectId", Json::String(id.clone())),
                    ("revision", f(request, "expectedRevision").clone()),
                ]),
                true,
            )
            .await?
            .document
            .value;
            let value = &base.document.value;
            require(
                !present(f(value, "claim"))
                    && value.get("workflowState") == original.get("workflowState")
                    && same_assignment(f(value, "fields"), f(&original, "fields"))?
                    && same(
                        Some(f(f(value, "fields"), "nativeOptions")),
                        Some(f(f(&original, "fields"), "nativeOptions")),
                    )?
                    && same(
                        value.get("primaryResourceRef"),
                        original.get("primaryResourceRef"),
                    )?
                    && same(value.get("dependencies"), original.get("dependencies"))?
                    && f(value, "wakeAt") == f(&original, "wakeAt"),
                "revision_conflict",
                "The assignment or wake-up schedule changed during allocation.",
            )?;
        }
        let previous = &base.document.value;
        if first {
            require(
                f(&base.metadata, "currentRevision") == f(&base.document.pin, "revision")
                    && !present(f(previous, "publication")),
                "revision_conflict",
                "The task changed or has an unfinished publication. Reload its current revision.",
            )?;
            self.policies.aggregate(previous, id).await?;
            self.policies
                .validate_action(request, previous, actor, at, s(&operation.pin, "objectId")?)
                .await?;
            if eq(request, "action", "continue") {
                self.lifecycle
                    .restore_for_execution(operation, &base.document)
                    .await?;
            }
        }
        let mut next = if first {
            let mut v = previous.clone();
            v.insert("updatedAt", Json::String(at.clone()));
            v.insert("publication", steps.pin()?);
            v
        } else {
            (self.task_read)(
                f(f(&a(op.required("writes")?)?[0], "payload"), "object").clone(),
                false,
            )
            .await?
            .document
            .value
        };
        let mut kind = "edited";
        let mut detail = JsString::default();
        if eq(request, "action", "start") || eq(request, "action", "continue") {
            if first {
                next.insert(
                    "observedDependencies",
                    dependency_snapshot(self.store, previous).await?,
                );
                if f(&next, "wakeAt")
                    .as_js_string()
                    .is_some_and(|wake| wake <= at)
                {
                    next.insert("wakeAt", Json::Null);
                }
                next.remove("completionIssue");
                next.remove("executionChangeRequested");
            }
            let attempt = previous
                .required("attemptCount")?
                .as_f64()
                .ok_or_else(IvyError::internal)?
                + 1.0;
            let claim = Json::object([
                ("operationId", f(request, "operationId").clone()),
                ("callerPrincipalId", f(op, "callerPrincipalId").clone()),
                ("attempt", Json::Number(attempt)),
                ("owner", actor.clone()),
                ("request", request.clone()),
                ("requestHash", f(op, "requestHash").clone()),
                ("target", f(request, "target").clone()),
                ("workspace", f(request, "workspace").clone()),
                ("primaryAtStart", f(previous, "primaryResourceRef").clone()),
                ("claimedAt", Json::String(at.clone())),
                ("phase", "claimed".into()),
                ("run", Json::Null),
            ]);
            next.insert("workflowState", "in_progress".into());
            next.insert("runtimeWait", Json::Null);
            next.insert("claim", claim);
            next.insert("attemptCount", Json::Number(attempt));
        }
        if eq(request, "action", "edit") && first {
            if request.get("parentTaskId") == Some(&Json::Null) {
                next.remove("parentTaskId");
            } else if let Some(parent) = request.get("parentTaskId") {
                next.insert("parentTaskId", parent.clone());
            }
            let mut fields = f(request, "fields").clone();
            fields.insert(
                "userContact",
                if f(&fields, "userContact").is_null() {
                    f(f(previous, "fields"), "userContact").clone()
                } else {
                    f(&fields, "userContact").clone()
                },
            );
            fields.insert(
                "allowParallel",
                if !f(&fields, "allowParallel").is_null() {
                    f(&fields, "allowParallel").clone()
                } else if !f(f(previous, "fields"), "allowParallel").is_null() {
                    f(f(previous, "fields"), "allowParallel").clone()
                } else {
                    false.into()
                },
            );
            if !f(&fields, "category").is_null() {
                require(
                    normalized.is_some(),
                    "task_board_category_projection_pending",
                    "A categorized Task needs a current canonical category before publication.",
                )?;
                fields.insert("category", Json::String(normalized.unwrap().clone()));
            }
            next.insert("fields", fields);
            self.schedule_fields(&mut next, request).await?;
            let mut old = f(previous, "fields").clone();
            let mut new = f(&next, "fields").clone();
            old.insert("nativeOptions", Json::Null);
            new.insert("nativeOptions", Json::Null);
            if !same(previous.get("dependencies"), next.get("dependencies"))?
                || f(previous, "wakeAt") != f(&next, "wakeAt")
                || !present(f(previous, "claim"))
                    && (!same(Some(&old), Some(&new))?
                        || f(previous, "parentTaskId") != f(&next, "parentTaskId"))
            {
                increment(&mut next, "workRevision")?;
            }
            if present(f(previous, "claim"))
                && !same(
                    Some(f(f(previous, "fields"), "nativeOptions")),
                    Some(f(f(&next, "fields"), "nativeOptions")),
                )?
            {
                next.insert("executionChangeRequested", true.into());
            }
        }
        if eq(request, "action", "reassign") && first {
            reassign(&mut next, request)?;
        }
        if eq(request, "action", "comment") && first && present(f(request, "handoff")) {
            self.schedule_fields(&mut next, f(request, "handoff"))
                .await?;
        }
        if first && eq(request, "action", "comment") {
            let mut projected = next.clone();
            let mut c = comment(request, previous, actor, at, author(request, actor));
            if f(request, "contactUser").as_bool() == Some(true)
                && eq(request, "purpose", "question")
                && !eq(f(previous, "fields"), "userContact", "ticket")
            {
                c.insert("delivery", steps.pin()?);
            }
            if (eq(&c, "authorKind", "user") || peer_comment(&c))
                && !eq(request, "agentDelivery", "none")
            {
                projected.insert(
                    "commentDeliveries",
                    append(
                        f(previous, "commentDeliveries"),
                        queued(&c, at, f(request, "operationId")),
                    )?,
                );
            }
            projected.insert("comments", append(f(previous, "comments"), c)?);
            projected.insert("lastHistory", steps.pin()?);
            validate_task_content(&projected, id)?;
        }
        if first && eq(request, "action", "uploadAttachment") {
            let mut projected = next.clone();
            let data = bytes(request)?;
            projected.insert(
                "attachments",
                append(
                    f(previous, "attachments"),
                    attachment(request, steps.pin()?, &data, actor, at),
                )?,
            );
            projected.insert("lastHistory", steps.pin()?);
            validate_task_content(&projected, id)?;
        }
        let reserved = steps
            .write(
                "task-board/task",
                &next,
                &Json::object([
                    ("objectId", Json::String(id.clone())),
                    (
                        "expectedRevision",
                        f(&base.document.pin, "revision").clone(),
                    ),
                ]),
            )
            .await?;
        match s(request, "action")?.to_utf8()?.as_str() {
            "retargetQuestion" => {
                let comments = a(f(previous, "comments"))?
                    .iter()
                    .map(|comment| {
                        let mut comment = comment.clone();
                        if f(&comment, "commentId") == f(request, "questionId") {
                            comment.insert("recipient", f(request, "recipient").clone());
                        }
                        comment
                    })
                    .collect();
                next.insert("comments", Json::Array(comments));
                detail = JsString::from("Question ");
                detail.0.extend(&s(request, "questionId")?.0);
                detail.0.extend(" addressed to ".encode_utf16());
                if eq(f(request, "recipient"), "kind", "user") {
                    detail.0.extend("the user.".encode_utf16());
                } else {
                    detail.0.extend("task ".encode_utf16());
                    detail.0.extend(&s(f(request, "recipient"), "taskId")?.0);
                    detail.0.push(46);
                }
            }
            "edit" => {
                let mut fields = f(request, "fields").clone();
                fields.insert(
                    "userContact",
                    if f(&fields, "userContact").is_null() {
                        f(f(previous, "fields"), "userContact").clone()
                    } else {
                        f(&fields, "userContact").clone()
                    },
                );
                fields.insert(
                    "allowParallel",
                    if !f(&fields, "allowParallel").is_null() {
                        f(&fields, "allowParallel").clone()
                    } else if !f(f(previous, "fields"), "allowParallel").is_null() {
                        f(f(previous, "fields"), "allowParallel").clone()
                    } else {
                        false.into()
                    },
                );
                if first {
                    fields.insert("category", f(f(&next, "fields"), "category").clone());
                    next.insert("fields", fields);
                }
                let assignment_changed =
                    !same_assignment(f(previous, "fields"), f(&next, "fields"))?;
                let schedule_changed =
                    !same(previous.get("dependencies"), next.get("dependencies"))?
                        || f(previous, "wakeAt") != f(&next, "wakeAt");
                if assignment_changed
                    && eq(f(&next, "fields"), "control", "agent")
                    && !eq(previous, "workflowState", "cancelled")
                    && !present(f(previous, "claim"))
                {
                    let c = system_comment(
                        f(request, "operationId"),
                        "Ticket edited".into(),
                        previous,
                        actor,
                        at,
                        f(previous, "lastRun"),
                    );
                    next.insert("comments", append(f(previous, "comments"), c.clone())?);
                    next.insert(
                        "commentDeliveries",
                        append(
                            f(previous, "commentDeliveries"),
                            queued(&c, at, f(request, "operationId")),
                        )?,
                    );
                }
                if (assignment_changed || schedule_changed)
                    && eq(f(&next, "fields"), "control", "agent")
                    && ["review", "done"]
                        .iter()
                        .any(|state| eq(previous, "workflowState", state))
                {
                    next.insert("workflowState", "todo".into());
                    next.insert("acceptedReview", Json::Null);
                }
                detail = "Updated editable task fields.".into();
            }
            "transition" => {
                next.insert("workflowState", f(request, "workflowState").clone());
                if ["backlog", "done", "cancelled"]
                    .iter()
                    .any(|state| eq(request, "workflowState", state))
                {
                    next.remove("executionChangeRequested");
                }
                if eq(request, "workflowState", "todo") && present(f(previous, "completionIssue")) {
                    next.remove("completionIssue");
                }
                next.insert("runtimeWait", Json::Null);
                kind = if eq(request, "workflowState", "todo") {
                    "ready"
                } else if eq(request, "workflowState", "done") {
                    "reviewed"
                } else if eq(request, "workflowState", "cancelled") {
                    "cancelled"
                } else {
                    "edited"
                };
                detail = if f(request, "detail").is_null() {
                    let mut v = JsString::from("Moved to ");
                    v.0.extend_from_slice(&s(request, "workflowState")?.0);
                    v.0.push(46);
                    v
                } else {
                    s(request, "detail")?.clone()
                };
            }
            "defer" => {
                self.schedule_fields(&mut next, request).await?;
                next.insert(
                    "runtimeWait",
                    request
                        .get("runtimeWait")
                        .unwrap_or(f(previous, "runtimeWait"))
                        .clone(),
                );
                if !present(f(previous, "claim"))
                    && !present(f(request, "runtimeWait"))
                    && !["review", "done", "cancelled", "backlog"]
                        .iter()
                        .any(|state| eq(previous, "workflowState", state))
                {
                    next.insert("workflowState", "waiting".into());
                }
                if present(f(previous, "claim"))
                    && eq(actor, "source", "worker")
                    && !present(f(request, "runtimeWait"))
                {
                    let mut c = system_comment(
                        f(request, "operationId"),
                        "Waiting for new input, a watched ticket change or the next appointment."
                            .into(),
                        &next,
                        actor,
                        at,
                        f(f(previous, "claim"), "run"),
                    );
                    c.insert("authorKind", "agent".into());
                    c.insert("purpose", "handoff".into());
                    c.insert(
                        "handoff",
                        Json::object([("workflowState", "waiting".into())]),
                    );
                    next.insert("comments", append(f(&next, "comments"), c)?);
                    increment(&mut next, "agentCommentCount")?;
                }
                kind = "deferred";
                detail = f(f(request, "runtimeWait"), "detail")
                    .as_js_string()
                    .cloned()
                    .unwrap_or_else(|| "Updated the wake-up schedule.".into());
            }
            "reassign" => {
                reassign(&mut next, request)?;
                kind = "reassigned";
                detail = s(request, "reason")?.clone();
            }
            "start" | "continue" => {
                let claim = next.required("claim")?.clone();
                let plan = read_plan(self.store, self.catalogs, f(request, "intent")).await?;
                let run = Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("taskId", Json::String(id.clone())),
                    ("attempt", f(&claim, "attempt").clone()),
                    ("operationId", f(request, "operationId").clone()),
                    ("callerPrincipalId", f(op, "callerPrincipalId").clone()),
                    ("originalRequest", request.clone()),
                    (
                        "executionContext",
                        Json::object([
                            ("workflowState", f(previous, "workflowState").clone()),
                            ("latestResult", f(previous, "latestResult").clone()),
                            ("acceptedReview", f(previous, "acceptedReview").clone()),
                        ]),
                    ),
                    ("requestHash", hash_json(request)?.into()),
                    ("target", f(&claim, "target").clone()),
                    ("workspace", f(&claim, "workspace").clone()),
                    ("plan", plan),
                    ("owner", actor.clone()),
                    ("phase", "starting".into()),
                    (
                        "primaryResourceRef",
                        f(previous, "primaryResourceRef").clone(),
                    ),
                    ("turnId", Json::Null),
                    ("nativeEpoch", Json::Null),
                    (
                        "calls",
                        Json::object([
                            ("thread", Json::Null),
                            ("turn", Json::Null),
                            ("interrupt", Json::Null),
                        ]),
                    ),
                    ("notificationCursor", Json::Number(0.0)),
                    ("result", Json::Null),
                    ("cancellation", Json::Null),
                    (
                        "externalOutcome",
                        Json::object([
                            ("state", "not_started".into()),
                            ("observedAt", Json::Null),
                            ("evidence", Json::Null),
                            ("code", Json::Null),
                        ]),
                    ),
                    ("createdAt", Json::String(at.clone())),
                    ("updatedAt", Json::String(at.clone())),
                    ("finishedAt", Json::Null),
                ]);
                let pin = steps.create("task-board/run", &run, &"Run".into()).await?;
                outcome.insert("run", pin.clone());
                next.insert("lastRun", pin.clone());
                let mut claim = claim;
                claim.insert("phase", "run_created".into());
                claim.insert("run", pin);
                next.insert("claim", claim);
                kind = if eq(request, "action", "start") {
                    "started"
                } else {
                    "continued"
                };
                detail = if eq(request, "action", "continue") {
                    s(request, "feedback")?.clone()
                } else {
                    "Claimed one native execution attempt.".into()
                };
            }
            "cancel" => {
                detail = s(request, "reason")?.clone();
                next.remove("executionChangeRequested");
                if !present(f(previous, "claim")) {
                    next.insert("workflowState", "cancelled".into());
                    next.insert("runtimeWait", Json::Null);
                    kind = "cancelled";
                } else {
                    let run = self
                        .store
                        .read("task-board/run", f(f(previous, "claim"), "run"), true)
                        .await?;
                    let mut value = run.value;
                    value.insert("phase", "cancel_requested".into());
                    value.insert(
                        "cancellation",
                        Json::object([
                            ("operationId", f(request, "operationId").clone()),
                            ("reason", f(request, "reason").clone()),
                            ("requestedAt", Json::String(at.clone())),
                            ("actor", actor.clone()),
                        ]),
                    );
                    value.insert("updatedAt", Json::String(at.clone()));
                    let pin = steps
                        .write(
                            "task-board/run",
                            &value,
                            &Json::object([
                                ("objectId", f(&run.pin, "objectId").clone()),
                                ("expectedRevision", f(&run.pin, "revision").clone()),
                            ]),
                        )
                        .await?;
                    outcome.insert("run", pin.clone());
                    let mut claim = f(previous, "claim").clone();
                    claim.insert("phase", "cancel_requested".into());
                    claim.insert("run", pin.clone());
                    next.insert("claim", claim);
                    next.insert("lastRun", pin);
                    kind = "cancel_requested";
                }
            }
            "review" => {
                let pin = steps
                    .create(
                        "task-board/review",
                        &Json::object([
                            ("schemaVersion", Json::Number(1.0)),
                            ("taskId", Json::String(id.clone())),
                            ("result", f(request, "result").clone()),
                            ("acceptance", f(request, "acceptance").clone()),
                            ("actor", actor.clone()),
                            ("createdAt", Json::String(at.clone())),
                            ("operationId", f(request, "operationId").clone()),
                        ]),
                        &"Review".into(),
                    )
                    .await?;
                outcome.insert("review", pin.clone());
                next.insert("workflowState", "done".into());
                next.insert("acceptedReview", pin);
                kind = "reviewed";
                detail = s(request, "acceptance")?.clone();
            }
            "saveResult" => {
                let pin = steps
                    .create(
                        "task-board/result",
                        &Json::object([
                            ("schemaVersion", Json::Number(1.0)),
                            ("taskId", Json::String(id.clone())),
                            ("run", f(request, "run").clone()),
                            ("kind", f(request, "kind").clone()),
                            ("content", f(request, "content").clone()),
                            ("actor", actor.clone()),
                            ("createdAt", Json::String(at.clone())),
                            ("operationId", f(request, "operationId").clone()),
                        ]),
                        &"Result".into(),
                    )
                    .await?;
                outcome.insert("result", pin.clone());
                if eq(request, "kind", "final") {
                    next.insert("workflowState", "review".into());
                    next.insert("runtimeWait", Json::Null);
                    next.insert("latestResult", pin);
                    next.insert("acceptedReview", Json::Null);
                    let mut body = JsString::from("Final result ready for review.\n\n");
                    body.0
                        .extend_from_slice(&s(f(request, "content"), "summary")?.0);
                    body.0.truncate(65536);
                    let mut c = system_comment(
                        f(request, "operationId"),
                        body,
                        previous,
                        actor,
                        at,
                        f(request, "run"),
                    );
                    let fits = result_comment_fits(&next, &c, &steps.pin()?, id)?;
                    if fits && !eq(f(previous, "fields"), "userContact", "ticket") {
                        let delivery = self.delivery(steps, request, &c, previous, at).await?;
                        c.insert("delivery", delivery);
                    }
                    if fits {
                        next.insert("comments", append(f(previous, "comments"), c)?);
                        next.insert(
                            "agentCommentCount",
                            Json::Number(
                                f(previous, "agentCommentCount")
                                    .as_f64()
                                    .ok_or_else(IvyError::internal)?
                                    + 1.0,
                            ),
                        );
                    }
                } else if !eq(previous, "workflowState", "review") {
                    next.insert("latestResult", pin);
                }
                kind = "result_saved";
                let mut text = s(request, "kind")?.clone();
                text.0.extend(" result saved.".encode_utf16());
                detail = text;
            }
            "comment" => {
                let mut c = comment(request, previous, actor, at, author(request, actor));
                if (f(request, "contactUser").as_bool() == Some(true))
                    && eq(request, "purpose", "question")
                    && !eq(f(previous, "fields"), "userContact", "ticket")
                {
                    let delivery = self.delivery(steps, request, &c, previous, at).await?;
                    c.insert("delivery", delivery);
                }
                next.insert("comments", append(f(previous, "comments"), c.clone())?);
                kind = "commented";
                detail = "Added Task comment.".into();
                if eq(&c, "authorKind", "agent") {
                    next.insert(
                        "agentCommentCount",
                        Json::Number(
                            f(previous, "agentCommentCount")
                                .as_f64()
                                .ok_or_else(IvyError::internal)?
                                + 1.0,
                        ),
                    );
                }
                if eq(&c, "authorKind", "user") || peer_comment(&c) {
                    if !eq(request, "agentDelivery", "none") {
                        let mut wait = f(&next, "runtimeWait").clone();
                        if present(&wait)
                            && ["host", "capability", "workspace"]
                                .iter()
                                .any(|reason| eq(&wait, "reason", reason))
                        {
                            wait.insert("retryAt", Json::Null);
                            next.insert("runtimeWait", wait);
                        }
                        next.insert(
                            "commentDeliveries",
                            append(
                                f(previous, "commentDeliveries"),
                                queued(&c, at, f(request, "operationId")),
                            )?,
                        );
                    }
                }
                if eq(previous, "workflowState", "done")
                    && (f(request, "moveToTodo").as_bool() == Some(true))
                {
                    next.insert("workflowState", "todo".into());
                    next.insert("runtimeWait", Json::Null);
                    next.insert("acceptedReview", Json::Null);
                }
            }
            "uploadAttachment" => {
                let data = bytes(request)?;
                let object = self
                    .store
                    .write_attachment(
                        id,
                        s(request, "operationId")?,
                        s(request, "attachmentId")?,
                        &data,
                    )
                    .await?;
                let item = attachment(request, object, &data, actor, at);
                next.insert(
                    "attachments",
                    append(f(previous, "attachments"), item.clone())?,
                );
                outcome.insert("attachment", item);
                detail = "Uploaded Task attachment.".into();
            }
            _ => {}
        }
        let mut record = history(operation, id, Some(&base.document), kind, &detail);
        record.insert("result", f(&outcome, "result").clone());
        record.insert("review", f(&outcome, "review").clone());
        if eq(request, "action", "reassign") {
            record.insert("fromTarget", f(f(previous, "claim"), "target").clone());
            record.insert("toTarget", Json::Null);
            record.insert("priorRun", f(request, "previousRun").clone());
        }
        let history = steps
            .create("task-board/history", &record, &"History".into())
            .await?;
        outcome.insert("history", history.clone());
        let mut final_task = next.clone();
        final_task.insert("lastHistory", history);
        final_task.insert("publication", Json::Null);
        outcome.insert(
            "task",
            steps
                .write(
                    "task-board/task",
                    &final_task,
                    &Json::object([
                        ("objectId", Json::String(id.clone())),
                        ("expectedRevision", f(&reserved, "revision").clone()),
                    ]),
                )
                .await?,
        );
        let workspace_changed = eq(request, "action", "edit")
            && (!same(
                f(previous, "fields").get("workspaceRequirement"),
                f(&next, "fields").get("workspaceRequirement"),
            )? || f(previous, "fields").get("allowParallel")
                != f(&next, "fields").get("allowParallel"));
        if eq(request, "action", "reassign")
            || eq(request, "action", "review")
            || (eq(request, "action", "edit") && workspace_changed)
            || (eq(request, "action", "cancel") && !present(f(previous, "claim")))
            || (eq(request, "action", "transition")
                && ["done", "cancelled", "backlog"]
                    .iter()
                    .any(|p| eq(request, "workflowState", p)))
        {
            self.store.release_workspace(id)?;
        }
        Ok(outcome)
    }
}
fn reassign(next: &mut Json, request: &Json) -> IvyResult<()> {
    let mut fields = next.required("fields")?.clone();
    fields.insert(
        "executionRequirement",
        f(request, "executionRequirement").clone(),
    );
    fields.insert(
        "workspaceRequirement",
        f(request, "workspaceRequirement").clone(),
    );
    next.insert("fields", fields);
    next.insert("workflowState", "todo".into());
    next.insert("runtimeWait", Json::Null);
    next.insert("claim", Json::Null);
    next.insert("primaryResourceRef", Json::Null);
    Ok(())
}
impl<S: ActionStore, P: ActionPolicies, L: ActionRestore> ExistingTaskPublication
    for TaskActions<'_, S, P, L>
{
    async fn publish<Q: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut Q,
        outcome: Json,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json> {
        self.program(operation, steps, outcome, normalized).await
    }
}
