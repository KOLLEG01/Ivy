//! Retained publication for existing Tasks. Aggregate/action admission is bound
//! to the owner's accepted policy; no frozen validation fallback is supplied.
use crate::{
    deliveries::initial_delivery,
    engine_publication::{ExistingTaskPublication, history},
    journal_store::Contracts,
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    native_lifecycle::{
        LifecycleEngine, LifecycleOperations, LifecyclePublication, LifecycleStore, NativeLifecycle,
    },
    native_plan::read_plan,
    operations::Document,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::canonical,
    limits::{JSON_OBJECT_CONTENT_BYTES, MANAGEMENT_FRAME_BYTES},
};
use ivy_sdk::rpc::RpcClient;

#[allow(async_fn_in_trait)]
pub trait ActionStore: LifecycleStore {
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
pub struct TaskActions<'a, S, P, L> {
    pub store: &'a S,
    pub settings: &'a Json,
    pub catalogs: &'a CheckedNativeContracts,
    pub policies: &'a P,
    pub lifecycle: &'a L,
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
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
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
        || (eq(actor, "source", "native") && truthy(f(request, "sourceTaskId")))
        || (truthy(f(request, "nativeInput")) && !truthy(f(request, "replyTo")))
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
    for key in ["purpose", "nativeInput", "sourceTaskId"] {
        if truthy(f(request, key)) {
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
pub fn result_comment_fits(task: &Json, comment: &Json, publication: &Json) -> IvyResult<bool> {
    if a(f(task, "comments"))?.len() >= 512 {
        return Ok(false);
    }
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
    match canonical(&projected, JSON_OBJECT_CONTENT_BYTES - 1024) {
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
        let base = self
            .store
            .lifecycle_read(
                "task-board/task",
                &Json::object([
                    ("objectId", Json::String(id.clone())),
                    ("revision", f(request, "expectedRevision").clone()),
                ]),
                true,
                false,
            )
            .await?;
        let previous = &base.document.value;
        if first {
            require(
                f(&base.metadata, "currentRevision") == f(request, "expectedRevision")
                    && !truthy(f(previous, "publication")),
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
            self.store
                .read(
                    "task-board/task",
                    f(f(&a(op.required("writes")?)?[0], "payload"), "object"),
                    false,
                )
                .await?
                .value
        };
        let mut kind = "edited";
        let mut detail = JsString::default();
        if eq(request, "action", "start") || eq(request, "action", "continue") {
            if !(eq(request, "action", "continue") && truthy(f(request, "coordinationOnly"))) {
                next.remove("blocker");
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
            next.insert("waiting", Json::Null);
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
            if !truthy(f(previous, "claim"))
                && (request.get("parentTaskId").is_none()
                    || !same(previous.get("fields"), next.get("fields"))?)
            {
                increment(&mut next, "workRevision")?;
            }
        }
        if eq(request, "action", "reassign") && first {
            reassign(&mut next, request)?;
        }
        if eq(request, "action", "comment") && first {
            let who = author(request, actor);
            let mut c = comment(request, previous, actor, at, who);
            c.insert(
                "delivery",
                if truthy(f(request, "contactUser"))
                    && eq(request, "purpose", "question")
                    && !eq(f(previous, "fields"), "userContact", "ticket")
                {
                    steps.pin()?
                } else {
                    Json::Null
                },
            );
            let queue = (who == "user"
                || (who == "agent"
                    && truthy(f(request, "sourceTaskId"))
                    && eq(request, "agentDelivery", "queue")))
                && eq(f(previous, "fields"), "control", "agent")
                && !eq(request, "agentDelivery", "none")
                && (!eq(previous, "workflowState", "done") || truthy(f(request, "moveToTodo")))
                && !eq(previous, "workflowState", "cancelled");
            let deliveries = if queue {
                append(
                    f(previous, "commentDeliveries"),
                    queued(&c, at, f(request, "operationId")),
                )?
            } else {
                f(previous, "commentDeliveries").clone()
            };
            require(
                a(f(previous, "comments"))?.len() < 512 && a(&deliveries)?.len() <= 512,
                "content_too_large",
                "The Task has reached its comment capacity.",
            )?;
            let mut projected = next.clone();
            projected.insert("comments", append(f(previous, "comments"), c)?);
            projected.insert("commentDeliveries", deliveries);
            projected.insert("lastHistory", steps.pin()?);
            canonical(&projected, JSON_OBJECT_CONTENT_BYTES - 1024)?;
        }
        if eq(request, "action", "uploadAttachment") && first {
            let data = bytes(request)?;
            let mut projected = next.clone();
            projected.insert(
                "attachments",
                append(
                    f(previous, "attachments"),
                    attachment(request, steps.pin()?, &data, actor, at),
                )?,
            );
            projected.insert("lastHistory", steps.pin()?);
            canonical(&projected, JSON_OBJECT_CONTENT_BYTES - 1024)?;
        }
        if eq(request, "action", "defer") && truthy(f(request, "blocker")) {
            next.insert("blocker", f(request, "blocker").clone());
        }
        let reserved = steps
            .write(
                "task-board/task",
                &next,
                &Json::object([
                    ("objectId", Json::String(id.clone())),
                    ("expectedRevision", f(request, "expectedRevision").clone()),
                ]),
            )
            .await?;
        match s(request, "action")?.to_utf8()?.as_str() {
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
                let parent_only = request.get("parentTaskId").is_some()
                    && same(previous.get("fields"), next.get("fields"))?;
                if !parent_only
                    && eq(f(&next, "fields"), "control", "agent")
                    && !eq(previous, "workflowState", "cancelled")
                    && !truthy(f(previous, "claim"))
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
                if !parent_only
                    && eq(f(&next, "fields"), "control", "agent")
                    && ["review", "done"]
                        .iter()
                        .any(|p| eq(previous, "workflowState", p))
                {
                    next.insert("workflowState", "todo".into());
                    next.insert("acceptedReview", Json::Null);
                }
                detail = "Updated editable task fields.".into();
            }
            "transition" => {
                next.insert("workflowState", f(request, "workflowState").clone());
                next.insert(
                    "waiting",
                    if eq(request, "workflowState", "waiting") {
                        Json::object([
                            ("reason", "user".into()),
                            (
                                "detail",
                                if f(request, "detail").is_null() {
                                    "User-controlled work is waiting.".into()
                                } else {
                                    f(request, "detail").clone()
                                },
                            ),
                            ("since", Json::String(at.clone())),
                        ])
                    } else {
                        Json::Null
                    },
                );
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
                next.insert("workflowState", "waiting".into());
                if truthy(f(request, "blocker")) {
                    next.insert("blocker", f(request, "blocker").clone());
                }
                next.insert(
                    "waiting",
                    Json::object([
                        ("reason", f(request, "reason").clone()),
                        ("detail", f(request, "detail").clone()),
                        ("since", Json::String(at.clone())),
                    ]),
                );
                let mut fields = f(&next, "fields").clone();
                fields.insert("nextReviewAt", f(request, "nextReviewAt").clone());
                next.insert("fields", fields);
                kind = "deferred";
                detail = s(request, "detail")?.clone();
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
                if !truthy(f(previous, "claim")) {
                    next.insert("workflowState", "cancelled".into());
                    next.insert("waiting", Json::Null);
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
                    next.insert("waiting", Json::Null);
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
                    let fits = result_comment_fits(&next, &c, &steps.pin()?)?;
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
                if truthy(f(request, "contactUser"))
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
                let peer = eq(&c, "authorKind", "agent") && truthy(f(&c, "sourceTaskId"));
                if (eq(&c, "authorKind", "user") || (peer && eq(request, "agentDelivery", "queue")))
                    && (!eq(previous, "workflowState", "done") || truthy(f(request, "moveToTodo")))
                    && !eq(previous, "workflowState", "cancelled")
                {
                    if !eq(request, "agentDelivery", "none") {
                        increment(&mut next, "workRevision")?;
                    }
                    if eq(f(previous, "fields"), "control", "agent")
                        && !eq(request, "agentDelivery", "none")
                    {
                        next.insert(
                            "commentDeliveries",
                            append(
                                f(previous, "commentDeliveries"),
                                queued(&c, at, f(request, "operationId")),
                            )?,
                        );
                    }
                    if eq(f(previous, "fields"), "control", "agent")
                        && !eq(request, "agentDelivery", "none")
                        && eq(&c, "authorKind", "user")
                        && !truthy(f(previous, "claim"))
                        && ["waiting", "review", "done"]
                            .iter()
                            .any(|p| eq(previous, "workflowState", p))
                    {
                        next.insert("workflowState", "todo".into());
                        next.insert("waiting", Json::Null);
                        next.insert("acceptedReview", Json::Null);
                    }
                }
                if eq(previous, "workflowState", "done") && truthy(f(request, "moveToTodo")) {
                    next.insert("workflowState", "todo".into());
                    next.insert("waiting", Json::Null);
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
            || (eq(request, "action", "cancel") && !truthy(f(previous, "claim")))
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
    next.insert("waiting", Json::Null);
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
