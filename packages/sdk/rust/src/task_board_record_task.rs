//! Historical task scheduling becomes current record fields and ordinary context.
use super::{configuration, findings, value};
use ivy_contracts::{IvyJson as Json, IvyResult, JsString, json::stringify};

fn object(value: &Json) -> IvyResult<Json> {
    value
        .as_object()
        .map(|_| value.clone())
        .ok_or_else(value::invalid)
}
fn list(value: Option<&Json>) -> IvyResult<&[Json]> {
    match value {
        None | Some(Json::Null) => Ok(&[]),
        Some(Json::Array(values)) => Ok(values),
        _ => Err(value::invalid()),
    }
}
fn text(value: Option<&Json>) -> IvyResult<&JsString> {
    value
        .and_then(Json::as_js_string)
        .ok_or_else(value::invalid)
}
fn is_text(value: Option<&Json>, expected: &str) -> bool {
    value.and_then(Json::as_js_string) == Some(&JsString::from(expected))
}
fn ids(values: impl IntoIterator<Item = Json>) -> IvyResult<Vec<Json>> {
    let mut unique = Vec::new();
    for value in values {
        text(Some(&value))?;
        if !unique.contains(&value) {
            unique.push(value);
        }
    }
    Ok(unique)
}

struct Schedule {
    wake_at: Json,
    dependencies: Vec<Json>,
}
fn schedule(
    steps: Option<&Json>,
    next_review: Option<&Json>,
    waiting: Option<&Json>,
) -> IvyResult<Schedule> {
    let mut appointments = Vec::new();
    let mut dependencies = Vec::new();
    for step in list(steps)? {
        for condition in list(step.get("requires"))? {
            if is_text(condition.get("kind"), "time") {
                if let Some(at) = condition.get("at").filter(|at| !at.is_null()) {
                    let at = text(Some(at))?;
                    if !at.0.is_empty() {
                        appointments.push(at.clone());
                    }
                }
            }
            if let Some(task_id) = condition.get("taskId").filter(|id| !id.is_null()) {
                if !text(Some(task_id))?.0.is_empty() {
                    dependencies.push(task_id.clone());
                }
            }
        }
    }
    if let Some(task_id) = waiting
        .and_then(|v| v.get("blocker"))
        .and_then(|v| v.get("taskId"))
    {
        dependencies.push(task_id.clone());
    }
    let wake_at = appointments
        .into_iter()
        .min()
        .map(Json::String)
        .or_else(|| next_review.cloned())
        .unwrap_or(Json::Null);
    Ok(Schedule {
        wake_at,
        dependencies: ids(dependencies)?,
    })
}

/// A single comment's handoff can be normalized without interpreting its body.
pub(super) fn comment(comment: &Json) -> IvyResult<Json> {
    let mut current = object(comment)?;
    let Some(handoff) = comment.get("handoff").filter(|value| !value.is_null()) else {
        return Ok(current);
    };
    if handoff.as_object().is_none() {
        return Err(value::invalid());
    }
    if !["nextSteps", "nextReviewAt", "waiting"]
        .iter()
        .any(|key| handoff.get(key).is_some())
    {
        return Ok(current);
    }
    let imported = schedule(
        handoff.get("nextSteps"),
        handoff.get("nextReviewAt"),
        handoff.get("waiting"),
    )?;
    let mut normalized = Json::object([
        (
            "wakeAt",
            handoff.get("wakeAt").cloned().unwrap_or(imported.wake_at),
        ),
        (
            "dependencies",
            handoff
                .get("dependencies")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(Json::Array(imported.dependencies)),
        ),
    ]);
    if let Some(state) = handoff.get("workflowState") {
        normalized.insert("workflowState", state.clone());
    }
    current.insert("handoff", normalized);
    Ok(current)
}

fn scheduling_context(task: &Json, comments: &mut Vec<Json>) -> IvyResult<()> {
    if task
        .get("commentStore")
        .is_some_and(|store| !store.is_null())
    {
        return Ok(());
    }
    let steps = list(task.get("nextSteps"))?;
    if steps.is_empty() {
        return Ok(());
    }
    let mut context = JsString::from(
        "Previous scheduling notes (context only; decide the work from the ticket):\n\n",
    );
    for (index, step) in steps.iter().enumerate() {
        if index > 0 {
            context.0.extend("\n\n".encode_utf16());
        }
        context.0.extend_from_slice(&text(step.get("scope"))?.0);
        let requires = list(step.get("requires"))?;
        if !requires.is_empty() {
            context.0.push(10);
            context
                .0
                .extend(stringify(&Json::Array(requires.to_vec()), usize::MAX)?.encode_utf16());
        }
    }
    let updated_at = text(task.get("updatedAt"))?;
    let mut sequence = comments.iter().try_fold(0.0_f64, |max, comment| {
        let sequence = comment
            .get("sequence")
            .and_then(Json::as_f64)
            .ok_or_else(value::invalid)?;
        Ok::<_, ivy_contracts::IvyError>(max.max(sequence))
    })?;
    for (offset, fragment) in findings::fragments(&context) {
        let id = value::concat([
            "schedule-context-".into(),
            updated_at.clone(),
            "-".into(),
            offset.to_string().into(),
        ]);
        if comments
            .iter()
            .any(|comment| comment.get("commentId").and_then(Json::as_js_string) == Some(&id))
        {
            continue;
        }
        sequence += 1.0;
        comments.push(Json::object([
            ("commentId", Json::String(id.clone())),
            ("sequence", Json::Number(sequence)),
            (
                "author",
                Json::object([
                    ("principalId", "task-board-record-upgrade".into()),
                    ("serviceNodeId", "task-board".into()),
                    ("generation", Json::Number(1.0)),
                    ("source", "recovery".into()),
                ]),
            ),
            ("authorKind", "system".into()),
            ("purpose", "update".into()),
            ("body", Json::String(fragment)),
            ("requests", Json::Array(Vec::new())),
            ("responses", Json::Array(Vec::new())),
            ("attachments", Json::Array(Vec::new())),
            ("run", Json::Null),
            ("turnId", Json::Null),
            ("replyTo", Json::Null),
            ("delivery", Json::Null),
            ("createdAt", Json::String(updated_at.clone())),
            ("operationId", Json::String(id)),
        ]));
    }
    Ok(())
}

pub(super) fn json(task: &Json) -> IvyResult<Json> {
    let prior = findings::preserve_task(value::borrow(task))?;
    let mut current = object(&prior)?;
    for key in [
        "nextSteps",
        "consumedSteps",
        "deliveries",
        "waiting",
        "blocker",
        "questionWait",
        "scheduledCheck",
        "consumedBlockers",
        "continuation",
    ] {
        current.remove(key);
    }
    let prior_fields = prior.get("fields").ok_or_else(value::invalid)?;
    let mut fields = object(prior_fields)?;
    fields.remove("dependencies");
    fields.remove("nextReviewAt");
    let requirement = prior_fields.get("executionRequirement");
    if let Some(requirement) = configuration::requirement_json(requirement) {
        fields.insert("executionRequirement", requirement.into_owned());
    }
    if is_text(
        requirement.and_then(|requirement| requirement.get("kind")),
        "capability",
    ) {
        let capabilities = list(prior_fields.get("requiredCapabilities"))?
            .iter()
            .cloned()
            .chain(std::iter::once(
                requirement
                    .and_then(|v| v.get("capabilityKey"))
                    .cloned()
                    .ok_or_else(value::invalid)?,
            ));
        fields.insert("requiredCapabilities", Json::Array(ids(capabilities)?));
    }
    let imported = schedule(
        prior.get("nextSteps"),
        prior_fields.get("nextReviewAt"),
        prior.get("waiting"),
    )?;
    let dependencies = match prior.get("dependencies").filter(|value| !value.is_null()) {
        Some(dependencies) => {
            list(Some(dependencies))?;
            dependencies.clone()
        }
        None => {
            let mut dependencies = imported.dependencies;
            dependencies.extend_from_slice(list(prior_fields.get("dependencies"))?);
            if let Some(task_id) = prior.get("blocker").and_then(|v| v.get("taskId")) {
                dependencies.push(task_id.clone());
            }
            Json::Array(ids(dependencies)?)
        }
    };
    let mut comments = list(prior.get("comments"))?
        .iter()
        .map(comment)
        .collect::<IvyResult<Vec<_>>>()?;
    scheduling_context(&prior, &mut comments)?;
    current.insert("fields", fields);
    current.insert(
        "wakeAt",
        prior.get("wakeAt").cloned().unwrap_or(imported.wake_at),
    );
    current.insert("dependencies", dependencies);
    current.insert(
        "runtimeWait",
        prior
            .get("runtimeWait")
            .filter(|wait| !wait.is_null() && !is_text(wait.get("reason"), "workflow"))
            .cloned()
            .unwrap_or(Json::Null),
    );
    current.insert("comments", Json::Array(comments));
    let has_claim = prior.get("claim").is_some_and(|claim| !claim.is_null());
    let obsolete = prior
        .get("completionIssue")
        .and_then(|issue| issue.get("code"))
        .and_then(Json::as_js_string)
        .is_some_and(|code| {
            code.0.starts_with(&JsString::from("task_board_handoff_").0)
                || code
                    .0
                    .starts_with(&JsString::from("task_board_recovery_").0)
        });
    if obsolete {
        current.remove("completionIssue");
        if !has_claim && is_text(current.get("workflowState"), "waiting") {
            current.insert("workflowState", "todo".into());
        }
    } else if let Some(issue) = prior
        .get("completionIssue")
        .filter(|issue| !issue.is_null())
    {
        let mut issue = object(issue)?;
        issue.remove("attempts");
        current.insert("completionIssue", issue);
    }
    if !has_claim
        && is_text(current.get("workflowState"), "in_progress")
        && is_text(prior_fields.get("control"), "agent")
    {
        current.insert("workflowState", "todo".into());
    }
    Ok(current)
}

pub(super) fn run(run: &Json) -> IvyResult<Json> {
    let mut current = object(run)?;
    if let Some(context) = run
        .get("executionContext")
        .filter(|context| !context.is_null())
    {
        if context.as_object().is_none() {
            return Err(value::invalid());
        }
        let mut normalized = Json::Object(Vec::new());
        for key in ["workflowState", "latestResult", "acceptedReview"] {
            if let Some(value) = context.get(key) {
                normalized.insert(key, value.clone());
            }
        }
        current.insert("executionContext", normalized);
    }
    Ok(current)
}
