//! Restore ticket state only after its original native Run has closed.
use super::*;
use crate::{
    deliveries::initial_delivery, engine_actions::result_comment_fits,
    engine_read::conversation::run_handoff,
};
use ivy_sdk::clock::{date_to_iso, parse_date};

pub(super) fn initial(task: &Json, run: &Json) -> Json {
    run.get("executionContext")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| {
            Json::object([
                ("workflowState", "todo".into()),
                ("latestResult", f(task, "latestResult").clone()),
                ("acceptedReview", f(task, "acceptedReview").clone()),
            ])
        })
}
pub(super) fn changing_execution(task: &Json, run: &Json) -> bool {
    f(task, "executionChangeRequested").as_bool() == Some(true) && f(run, "cancellation").is_null()
}
pub(super) fn intent(task: &Json, run: &Json, run_id: &JsString, active: bool) -> Json {
    if active && changing_execution(task, run) {
        return Json::Null;
    }
    if let Some(comment) = run_handoff(task, run_id).filter(|c| !f(c, "handoff").is_null()) {
        return f(&comment, "handoff").clone();
    }
    if eq(f(run, "owner"), "source", "native")
        && !one(&initial(task, run), "workflowState", &["done", "cancelled"])
    {
        Json::object([("workflowState", "review".into())])
    } else {
        Json::Null
    }
}
pub(super) fn after(at: &JsString, delay: f64) -> IvyResult<Json> {
    Ok(Json::String(
        date_to_iso(parse_date(at) + delay).map_err(|_| IvyError::internal())?,
    ))
}
fn limited(value: &Json, units: usize) -> Json {
    Json::String(JsString(text(value).0.into_iter().take(units).collect()))
}

impl<S: NativePublicationStore, E: NativePublicationEngine, V: NativePublicationEvidence>
    NativePublisher<'_, S, E, V>
{
    pub(super) async fn finish_active<P: LifecyclePublication>(
        &self,
        operation: &Document,
        task: &Json,
        run: &Json,
        next: &Json,
        mut final_task: Json,
        done: &Json,
        outcome: &Json,
        handoff: &Json,
        steps: &mut P,
    ) -> IvyResult<Json> {
        let request = f(&operation.value, "request");
        let run_pin = f(request, "run");
        let at = s(&operation.value, "createdAt")?;
        let initial = initial(task, run);
        let succeeded = eq(done, "state", "succeeded");
        let cancelled = eq(done, "state", "cancelled");
        let ready = succeeded && eq(handoff, "workflowState", "review");
        if !f(run, "cancellation").is_null() {
            final_task.remove("executionChangeRequested");
        }
        if ready {
            final_task.insert(
                "workRevision",
                Json::Number(n(&final_task, "workRevision") + 1.0),
            );
        }
        if succeeded && !handoff.is_null() {
            final_task.insert("workflowState", f(handoff, "workflowState").clone());
            final_task.remove("completionIssue");
        }
        for (key, value) in [
            ("claim", Json::Null),
            ("runtimeWait", Json::Null),
            ("primaryResourceRef", f(next, "primaryResourceRef").clone()),
            ("lastRun", f(outcome, "run").clone()),
        ] {
            final_task.insert(key, value);
        }
        let publication_error = self.store.failed_handoff(s(run_pin, "objectId")?)?;
        let restored_state = if eq(&initial, "workflowState", "in_progress") {
            "todo".into()
        } else {
            f(&initial, "workflowState").clone()
        };
        if cancelled {
            final_task.insert("workflowState", "cancelled".into());
            final_task.remove("executionChangeRequested");
        } else if succeeded {
            final_task.remove("completionIssue");
            final_task.insert(
                "workflowState",
                if handoff.is_null() {
                    restored_state
                } else {
                    f(handoff, "workflowState").clone()
                },
            );
            let replacing = !handoff.is_null() || eq(&initial, "workflowState", "todo");
            final_task.insert(
                "latestResult",
                if replacing {
                    f(outcome, "result").clone()
                } else {
                    f(&initial, "latestResult").clone()
                },
            );
            final_task.insert(
                "acceptedReview",
                if replacing {
                    Json::Null
                } else {
                    f(&initial, "acceptedReview").clone()
                },
            );
        } else {
            let retry = if !f(done, "retryAt").is_null() {
                f(done, "retryAt").clone()
            } else if f(next, "turnId").is_null() || changing_execution(task, run) {
                after(at, 30_000.0)?
            } else {
                Json::Null
            };
            final_task.insert("workflowState", restored_state);
            final_task.insert(
                "latestResult",
                if f(task, "latestResult").is_null() {
                    f(outcome, "result").clone()
                } else {
                    f(task, "latestResult").clone()
                },
            );
            final_task.insert(
                "completionIssue",
                Json::object([
                    (
                        "code",
                        if f(done, "code").is_null() {
                            "native_failed".into()
                        } else {
                            f(done, "code").clone()
                        },
                    ),
                    ("detail", limited(f(done, "summary"), 2048)),
                    ("run", run_pin.clone()),
                    ("operationId", f(request, "operationId").clone()),
                    ("retryAt", retry.clone()),
                ]),
            );
            if !retry.is_null() {
                if f(&final_task, "executionChangeRequested").as_bool() != Some(true)
                    && f(run, "cancellation").is_null()
                {
                    let origin = self
                        .store
                        .read(
                            "task-board/task",
                            &Json::object([
                                ("objectId", f(run, "taskId").clone()),
                                (
                                    "revision",
                                    f(f(run, "originalRequest"), "expectedRevision").clone(),
                                ),
                            ]),
                            true,
                        )
                        .await?;
                    if f(&origin.value, "executionChangeRequested").as_bool() == Some(true) {
                        final_task.insert("executionChangeRequested", true.into());
                    }
                }
                final_task.insert(
                    "commentDeliveries",
                    Json::Array(
                        array(f(&final_task, "commentDeliveries"))
                            .iter()
                            .map(|v| {
                                if array(f(f(run, "originalRequest"), "commentIds"))
                                    .contains(f(v, "commentId"))
                                {
                                    patch(
                                        v,
                                        [
                                            ("state", "queued".into()),
                                            ("updatedAt", Json::String(at.clone())),
                                            ("code", f(done, "code").clone()),
                                        ],
                                    )
                                } else {
                                    v.clone()
                                }
                            })
                            .collect(),
                    ),
                );
                let detail = crate::native_rate_limit::waiting_detail(
                    retry.as_js_string().ok_or_else(IvyError::internal)?,
                );
                final_task.insert(
                    "runtimeWait",
                    Json::object([
                        ("reason", "host".into()),
                        ("detail", Json::String(detail)),
                        ("since", Json::String(at.clone())),
                        ("retryAt", retry),
                    ]),
                );
            }
        }
        if let Some(error) = publication_error {
            final_task.insert(
                "completionIssue",
                patch(&error, [("run", run_pin.clone()), ("retryAt", Json::Null)]),
            );
            final_task.insert("runtimeWait", Json::Null);
        }
        if !cancelled {
            let handoff_comment = run_handoff(&final_task, s(run_pin, "objectId")?);
            let inputs = array(f(f(run, "originalRequest"), "commentIds"));
            let answered = !inputs.is_empty()
                && array(f(&final_task, "comments")).iter().any(|c| {
                    eq(c, "authorKind", "agent")
                        && f(f(c, "run"), "objectId") == f(run_pin, "objectId")
                        && !f(c, "replyTo").is_null()
                        && inputs.contains(f(c, "replyTo"))
                });
            let mut comment = handoff_comment.clone().unwrap_or_else(|| {
                Json::object([
                    ("commentId", f(request, "operationId").clone()),
                    (
                        "sequence",
                        Json::Number(array(f(&final_task, "comments")).len() as f64 + 1.0),
                    ),
                    ("author", f(&operation.value, "actor").clone()),
                    ("authorKind", "agent".into()),
                    ("purpose", "update".into()),
                    ("body", limited(f(done, "summary"), 65536)),
                    ("requests", Json::Array(vec![])),
                    ("responses", Json::Array(vec![])),
                    ("delivery", Json::Null),
                    ("attachments", Json::Array(vec![])),
                    ("run", run_pin.clone()),
                    ("turnId", f(next, "turnId").clone()),
                    ("replyTo", inputs.last().cloned().unwrap_or(Json::Null)),
                    ("createdAt", Json::String(at.clone())),
                    ("operationId", f(request, "operationId").clone()),
                ])
            });
            let fits = handoff_comment.is_some()
                || result_comment_fits(
                    &final_task,
                    &comment,
                    &steps.pin()?,
                    s(request, "taskId")?,
                )?;
            if fits
                && ready
                && f(&final_task, "parentTaskId").is_null()
                && !eq(f(&final_task, "fields"), "userContact", "ticket")
                && f(&comment, "delivery").is_null()
            {
                let delivery = initial_delivery(
                    self.engine.phone_target(),
                    &steps.pin()?,
                    s(request, "taskId")?,
                    s(&comment, "commentId")?,
                    s(f(&final_task, "fields"), "userContact")?,
                    at,
                );
                comment.insert(
                    "delivery",
                    steps
                        .create(
                            "task-board/delivery",
                            &delivery,
                            &"Handoff notification".into(),
                        )
                        .await?,
                );
            }
            if handoff_comment.is_some() {
                final_task.insert(
                    "comments",
                    Json::Array(
                        array(f(&final_task, "comments"))
                            .iter()
                            .map(|c| {
                                if f(c, "commentId") == f(&comment, "commentId") {
                                    comment.clone()
                                } else {
                                    c.clone()
                                }
                            })
                            .collect(),
                    ),
                );
            } else if !answered && fits {
                let mut comments = array(f(&final_task, "comments")).to_vec();
                comments.push(comment);
                final_task.insert("comments", Json::Array(comments));
                final_task.insert(
                    "agentCommentCount",
                    Json::Number(n(&final_task, "agentCommentCount") + 1.0),
                );
            }
        }
        Ok(final_task)
    }
}
