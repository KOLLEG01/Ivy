//! Current ticket wake policy. Watched headers describe material work changes;
//! native claims, publication and receipt churn are deliberately excluded.
use futures_util::future::try_join_all;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
use ivy_sdk::native_evidence::native_instant;

#[allow(async_fn_in_trait)]
pub trait WakeStore<E = IvyError> {
    /// Read an unhydrated primary Task, including archived records. The envelope
    /// contains its value and metadata.effectivelyArchived, just like Store.read.
    async fn watched_task(&self, id: &JsString) -> Result<Json, E>;
    /// Only conclusive absence is a deleted dependency. Transport, scope and
    /// storage failures must remain errors, with their original outcomes.
    fn deleted(&self, error: &E) -> bool;
}
pub trait WakeFailure {
    fn deleted(&self) -> bool;
}
impl WakeFailure for IvyError {
    fn deleted(&self) -> bool {
        matches!(self.code.as_str(), "not_found" | "object_deleted")
    }
}
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn is(value: &Json, key: &str, text: &str) -> bool {
    value.get(key) == Some(&text.into())
}
fn present(value: &Json) -> bool {
    !value.is_null()
}
fn dependencies(task: &Json) -> IvyResult<&[Json]> {
    f(task, "dependencies")
        .as_array()
        .ok_or_else(IvyError::internal)
}

pub fn peer_comment(comment: &Json) -> bool {
    is(comment, "authorKind", "agent")
        && f(comment, "sourceTaskId")
            .as_js_string()
            .is_some_and(|id| !id.0.is_empty())
}
pub fn queued_input(comment: &Json) -> bool {
    is(comment, "authorKind", "user")
        || peer_comment(comment)
        || is(comment, "authorKind", "system")
            && is(comment, "body", "Ticket edited")
            && !present(f(comment, "nativeInput"))
}
pub fn comments_pending(task: &Json) -> bool {
    f(task, "commentDeliveries")
        .as_array()
        .unwrap_or(&[])
        .iter()
        .any(|delivery| {
            is(delivery, "state", "queued")
                && f(task, "comments")
                    .as_array()
                    .unwrap_or(&[])
                    .iter()
                    .any(|comment| {
                        f(comment, "commentId") == f(delivery, "commentId") && queued_input(comment)
                    })
        })
}
pub fn missing_wake_trigger(task: &Json, has_question: bool) -> bool {
    is(f(task, "fields"), "control", "agent")
        && is(task, "workflowState", "waiting")
        && !present(f(task, "claim"))
        && !present(f(task, "publication"))
        && !present(f(task, "runtimeWait"))
        && !present(f(task, "completionIssue"))
        && !present(f(task, "wakeAt"))
        && f(task, "dependencies")
            .as_array()
            .is_some_and(<[Json]>::is_empty)
        && !has_question
}
pub async fn dependency_snapshot<E: From<IvyError>, S: WakeStore<E>>(
    store: &S,
    task: &Json,
) -> Result<Json, E> {
    let mut entries = Vec::new();
    // Bound concurrent header reads without constructing a foreign graph or
    // loading any peer's conversation or saved Run.
    for batch in dependencies(task)?.chunks(8) {
        entries.extend(
            try_join_all(batch.iter().map(|id| async {
                let id = id.as_js_string().ok_or_else(IvyError::internal)?;
                let hash = match store.watched_task(id).await {
                    Ok(document) => {
                        let other = document.required("value")?;
                        hash_json(&Json::object([
                            ("work", other.required("workRevision")?.clone()),
                            ("review", other.required("acceptedReview")?.clone()),
                            (
                                "archived",
                                document
                                    .required("metadata")?
                                    .required("effectivelyArchived")?
                                    .clone(),
                            ),
                            (
                                "terminal",
                                if ["done", "cancelled"]
                                    .iter()
                                    .any(|state| is(other, "workflowState", state))
                                {
                                    other.required("workflowState")?.clone()
                                } else {
                                    Json::Null
                                },
                            ),
                        ]))?
                    }
                    Err(error) if store.deleted(&error) => hash_json(&Json::from("deleted"))?,
                    Err(error) => return Err(error),
                };
                Ok((id.clone(), Json::from(hash)))
            }))
            .await?,
        );
    }
    Ok(Json::object(entries))
}
pub fn dependencies_changed(task: &Json, current: &Json) -> IvyResult<bool> {
    Ok(dependencies(task)?.iter().any(|id| {
        id.as_js_string()
            .is_some_and(|id| current.get_key(id) != f(task, "observedDependencies").get_key(id))
    }))
}
pub async fn validate_dependencies<E: From<IvyError>, S: WakeStore<E>>(
    store: &S,
    own: Option<&JsString>,
    ids: &[Json],
) -> Result<(), E> {
    if own.is_some_and(|own| ids.iter().any(|id| id.as_js_string() == Some(own))) {
        return Err(IvyError::new("invalid_arguments", "A task cannot watch itself.").into());
    }
    for id in ids {
        store
            .watched_task(id.as_js_string().ok_or_else(IvyError::internal)?)
            .await?;
    }
    Ok(())
}
fn due(value: &Json, at: &JsString) -> bool {
    value.as_js_string().is_some_and(|value| value <= at)
}
pub fn next_execution_mode(task: &Json, at: &JsString) -> bool {
    if present(f(task, "claim")) || present(f(task, "publication")) {
        return false;
    }
    if comments_pending(task) {
        return true;
    }
    let issue = f(task, "completionIssue");
    let retry = !present(issue) || due(f(issue, "retryAt"), at);
    if f(task, "executionChangeRequested").as_bool() == Some(true) {
        return retry;
    }
    is(f(task, "fields"), "control", "agent")
        && !["backlog", "done", "cancelled"]
            .iter()
            .any(|state| is(task, "workflowState", state))
        && retry
}
pub async fn should_wake<E: From<IvyError>, S: WakeStore<E>>(
    store: &S,
    task: &Json,
    at: &JsString,
) -> Result<bool, E> {
    if present(f(task, "claim"))
        || present(f(task, "publication"))
        || f(f(task, "runtimeWait"), "retryAt")
            .as_js_string()
            .is_some_and(|retry| retry > at)
    {
        return Ok(false);
    }
    if comments_pending(task) {
        return Ok(true);
    }
    let issue = f(task, "completionIssue");
    if f(task, "executionChangeRequested").as_bool() == Some(true) {
        return Ok(!present(issue) || due(f(issue, "retryAt"), at));
    }
    if !is(f(task, "fields"), "control", "agent")
        || ["backlog", "done", "cancelled"]
            .iter()
            .any(|state| is(task, "workflowState", state))
    {
        return Ok(false);
    }
    if present(issue) {
        return Ok(due(f(issue, "retryAt"), at));
    }
    if missing_wake_trigger(task, !ivy_sdk::open_task_questions(task, None)?.is_empty()) {
        let updated = f(task, "updatedAt")
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        return Ok(native_instant(at, None)? >= native_instant(updated, None)? + 30_000);
    }
    if is(task, "workflowState", "todo")
        || due(f(f(task, "runtimeWait"), "retryAt"), at)
        || due(f(task, "wakeAt"), at)
    {
        return Ok(true);
    }
    dependencies_changed(task, &dependency_snapshot(store, task).await?).map_err(E::from)
}
pub fn wake_condition(task: &Json, questions: &[Json]) -> Json {
    let condition = |state: &str, detail: &str| {
        Json::object([("state", state.into()), ("detail", detail.into())])
    };
    if is(task, "workflowState", "todo") {
        return if is(f(task, "fields"), "control", "user") {
            condition("needs_user", "Ready for manual work.")
        } else {
            condition("queued", "Queued for the agent to continue.")
        };
    }
    if questions
        .iter()
        .any(|q| !is(f(q, "recipient"), "kind", "task"))
    {
        return condition(
            "needs_user",
            "The agent has a question for you. A normal comment wakes it.",
        );
    }
    if let Some(at) = f(task, "wakeAt").as_js_string() {
        let mut detail = JsString::from("Next wake-up: ");
        detail.0.extend(&at.0);
        detail.0.push(46);
        return Json::object([
            ("state", "deferred".into()),
            ("detail", Json::String(detail)),
        ]);
    }
    if f(task, "dependencies")
        .as_array()
        .is_some_and(|ids| !ids.is_empty())
        || !questions.is_empty()
    {
        return condition(
            "blocked_dependency",
            if questions.is_empty() {
                "Watching related tickets for changes."
            } else {
                "Waiting for another ticket to answer."
            },
        );
    }
    if is(f(task, "fields"), "control", "user") {
        return condition("needs_user", "Waiting for you to continue this task.");
    }
    if missing_wake_trigger(task, false) {
        return condition(
            "failed",
            "Waiting without a question, watched ticket or appointment. The agent retries automatically after 30 seconds.",
        );
    }
    condition("idle", "Waiting for a comment or ticket change.")
}
