//! Lossless stored-record upgrades. Read admission, hydration and writes belong to the owner.
use crate::{date_format::date_to_iso, date_parse::parse_date};
use ivy_contracts::{
    IvyJson as Json,
    error::{JavaScriptException, JavaScriptExceptionKind},
    task_board_questions::open_task_questions_raw,
};
use std::borrow::Cow;

#[path = "task_board_record_findings.rs"]
mod findings;
#[path = "task_board_record_value.rs"]
mod value;
use value::*;

/// Missing remains missing; only the former capability requirement is replaced.
pub fn upgrade_requirement(requirement: Option<&Json>) -> Option<Cow<'_, Json>> {
    requirement.map(|value| {
        if is_text(value.get("kind"), "capability") {
            Cow::Owned(Json::object([("kind", "automatic".into())]))
        } else {
            Cow::Borrowed(value)
        }
    })
}

pub fn upgrade_configuration_record(configuration: &Json) -> Result<Cow<'_, Json>> {
    let defaults = field(Some(configuration), "defaults")?;
    let requirement = field(defaults, "executionRequirement")?;
    if !is_text(optional(requirement, "kind")?, "capability") {
        return Ok(Cow::Borrowed(configuration));
    }
    let mut upgraded_defaults = spread(defaults);
    if let Some(requirement) = upgrade_requirement(requirement) {
        upgraded_defaults.insert("executionRequirement", requirement.into_owned());
    }
    let mut upgraded = spread(Some(configuration));
    upgraded.insert("defaults", upgraded_defaults);
    Ok(Cow::Owned(upgraded))
}

fn imported_conditions(task: &Json, blocker: Option<&Json>) -> Result<Vec<Json>> {
    let dependencies = field(field(Some(task), "fields")?, "dependencies")?;
    let mut conditions = if nullish(dependencies) {
        Vec::new()
    } else {
        dependencies
            .and_then(Json::as_array)
            .ok_or_else(|| type_error("(task.fields.dependencies ?? []).map is not a function"))?
            .iter()
            .map(|task_id| {
                Json::object([
                    ("kind", "task".into()),
                    ("taskId", task_id.clone()),
                    ("until", "done".into()),
                ])
            })
            .collect()
    };
    if !truthy(blocker) {
        return Ok(conditions);
    }
    let until = field(blocker, "until")?;
    let mut condition = Json::object([(
        "kind",
        if is_text(until, "delivery") {
            "delivery"
        } else {
            "task"
        }
        .into(),
    )]);
    let task_id = field(blocker, "taskId")?;
    put(&mut condition, "taskId", task_id);
    if is_text(until, "delivery") {
        let key = field(blocker, "key")?;
        let consumed = field(Some(task), "consumedBlockers")?;
        let mut fingerprint = None;
        if !nullish(consumed) {
            let consumed = consumed
                .and_then(Json::as_array)
                .ok_or_else(|| type_error("task.consumedBlockers?.find is not a function"))?;
            for value in consumed {
                let previous = field(Some(value), "blocker")?;
                if strict_equal(field(previous, "taskId")?, task_id)
                    && strict_equal(field(previous, "key")?, key)
                {
                    fingerprint = field(Some(value), "fingerprint")?;
                    break;
                }
            }
        }
        put(&mut condition, "key", key);
        if truthy(fingerprint) {
            put(&mut condition, "after", fingerprint);
        }
    } else {
        put(&mut condition, "until", until);
    }
    conditions.push(condition);
    Ok(conditions)
}

fn own_question_conditions(task: &Json) -> Result<Vec<Json>> {
    open_task_questions_raw(task, None)?
        .iter()
        .map(|question| {
            let mut condition = Json::object([("kind", "user".into())]);
            put(
                &mut condition,
                "questionId",
                field(Some(question), "commentId")?,
            );
            Ok(condition)
        })
        .collect()
}

fn imported_handoff(
    previous: &Json,
    task: &Json,
    identity: ivy_contracts::JsString,
) -> Result<Json> {
    if field(Some(previous), "nextSteps")?
        .and_then(Json::as_array)
        .is_some()
    {
        return Ok(previous.clone());
    }
    let waiting = field(Some(previous), "waiting")?;
    let conditions = imported_conditions(task, optional(waiting, "blocker")?)?;
    let questions = own_question_conditions(task)?;
    let appointment = field(Some(previous), "nextReviewAt")?;
    let state = field(Some(previous), "workflowState")?;
    let mut steps = Vec::new();
    if truthy(appointment) {
        let scope = field(Some(previous), "checkScope")?;
        let scope = if nullish(scope) {
            optional(waiting, "detail")?
        } else {
            scope
        };
        let mut requires = conditions;
        requires.push(Json::object([
            ("kind", "time".into()),
            ("at", appointment.unwrap().clone()),
        ]));
        steps.push(Json::object([
            (
                "id",
                Json::String(concat([identity, "-appointment".into()])),
            ),
            (
                "scope",
                if nullish(scope) {
                    "Continue the authorized task at its appointment.".into()
                } else {
                    scope.unwrap().clone()
                },
            ),
            ("requires", Json::Array(requires)),
        ]));
    } else if !is_text(state, "review") && !truthy(field(Some(task), "questionWait")?) {
        let scope = field(Some(previous), "nextStep")?;
        let scope = if nullish(scope) {
            optional(waiting, "detail")?
        } else {
            scope
        };
        let mut requires = conditions;
        if is_text(optional(waiting, "reason")?, "user") {
            requires.extend(questions);
        }
        steps.push(Json::object([
            ("id", Json::String(identity)),
            (
                "scope",
                if nullish(scope) {
                    "Continue the authorized task described in this ticket.".into()
                } else {
                    scope.unwrap().clone()
                },
            ),
            ("requires", Json::Array(requires)),
        ]));
    }
    let mut upgraded = Json::object::<&str>([]);
    put(&mut upgraded, "workflowState", state);
    upgraded.insert("nextSteps", Json::Array(steps));
    let deliveries = field(Some(previous), "deliveries")?;
    if truthy(deliveries) {
        put(&mut upgraded, "deliveries", deliveries);
    }
    Ok(upgraded)
}

fn capabilities(fields: &Json, requirement: &Json) -> Result<Json> {
    let values = field(Some(fields), "requiredCapabilities")?;
    let mut values: Vec<Option<Json>> = if nullish(values) {
        Vec::new()
    } else {
        iterable(
            values.unwrap(),
            "(fields.requiredCapabilities ?? []) is not iterable",
        )?
        .into_iter()
        .map(Some)
        .collect()
    };
    values.push(field(Some(requirement), "capabilityKey")?.cloned());
    let mut unique: Vec<Option<Json>> = Vec::new();
    for value in values {
        if !unique.iter().any(|prior| strict_equal(prior.as_ref(), value.as_ref())
            || matches!((prior.as_ref(), value.as_ref()), (Some(Json::Number(a)), Some(Json::Number(b))) if a.is_nan() && b.is_nan())) {
            unique.push(value);
        }
    }
    Ok(Json::Array(
        unique
            .into_iter()
            .map(|v| v.unwrap_or(Json::Null))
            .collect(),
    ))
}

/// Findings are retained before the current-record short circuit, as in the stored source.
pub fn upgrade_task_record(value: &Json) -> Result<Json> {
    let prior = findings::preserve_task(value)?;
    if prior.get("nextSteps").and_then(Json::as_array).is_some() {
        return Ok(prior);
    }
    let prior_fields = field(Some(&prior), "fields")?;
    let requirement = field(prior_fields, "executionRequirement")?;
    let mut fields = spread(prior_fields);
    fields.remove("dependencies");
    fields.remove("nextReviewAt");
    let waiting = field(Some(&prior), "waiting")?;
    let blocker = field(Some(&prior), "blocker")?;
    let question_wait = field(Some(&prior), "questionWait")?;
    let scheduled = field(Some(&prior), "scheduledCheck")?;
    let mut base = prior.clone();
    for key in [
        "waiting",
        "blocker",
        "questionWait",
        "scheduledCheck",
        "consumedBlockers",
    ] {
        base.remove(key);
    }
    let questions = open_task_questions_raw(&prior, None)?;
    let mut next_steps = Vec::new();
    let conditions = imported_conditions(&prior, blocker)?;
    let appointment = field(prior_fields, "nextReviewAt")?;
    if truthy(appointment) {
        let scope = optional(scheduled, "scope")?;
        let scope = if nullish(scope) {
            optional(waiting, "detail")?
        } else {
            scope
        };
        let mut requires = conditions.clone();
        requires.push(Json::object([
            ("kind", "time".into()),
            ("at", appointment.unwrap().clone()),
        ]));
        next_steps.push(Json::object([
            ("id", "imported-appointment".into()),
            (
                "scope",
                if nullish(scope) {
                    "Continue the authorized task at its appointment.".into()
                } else {
                    scope.unwrap().clone()
                },
            ),
            ("requires", Json::Array(requires)),
        ]));
    }
    if !truthy(question_wait)
        && !truthy(scheduled)
        && !["backlog", "review", "done", "cancelled"]
            .iter()
            .any(|state| is_text(prior.get("workflowState"), state))
    {
        let scope = optional(waiting, "detail")?;
        let mut requires = conditions;
        if is_text(optional(waiting, "reason")?, "user") {
            for question in &questions {
                let mut condition = Json::object([("kind", "user".into())]);
                put(
                    &mut condition,
                    "questionId",
                    field(Some(question), "commentId")?,
                );
                requires.push(condition);
            }
        }
        next_steps.push(Json::object([
            ("id", "imported-work".into()),
            ("scope", if nullish(scope) { "Continue the authorized task described in this ticket; identify and preserve any answer-dependent prerequisites.".into() } else { scope.unwrap().clone() }),
            ("requires", Json::Array(requires)),
        ]));
    }
    let technical = if truthy(waiting)
        && [
            "host",
            "capability",
            "workspace",
            "external_outcome",
            "publication",
            "workflow",
        ]
        .iter()
        .any(|reason| is_text(waiting.and_then(|v| v.get("reason")), reason))
    {
        let reason = field(waiting, "reason")?;
        let mut technical = Json::object::<&str>([]);
        put(&mut technical, "reason", reason);
        put(&mut technical, "detail", field(waiting, "detail")?);
        let since = field(waiting, "since")?;
        put(&mut technical, "since", since);
        let retry = if ["host", "capability", "workspace"]
            .iter()
            .any(|r| is_text(reason, r))
        {
            let millis = parse_date(&string(since)?) + 30_000.0;
            Json::String(date_to_iso(millis).map_err(|_| JavaScriptException {
                kind: JavaScriptExceptionKind::RangeError,
                message: "Invalid time value".into(),
            })?)
        } else {
            Json::Null
        };
        technical.insert("retryAt", retry);
        technical
    } else if truthy(question_wait) {
        let mut technical = Json::object([
            ("reason", "workflow".into()),
            ("detail", "The former cross-ticket user wait needs an explicit task delivery or a genuine own question.".into()),
        ]);
        put(&mut technical, "since", field(Some(&prior), "updatedAt")?);
        technical.insert("retryAt", Json::Null);
        technical
    } else {
        Json::Null
    };
    if let Some(upgraded) = upgrade_requirement(requirement) {
        fields.insert("executionRequirement", upgraded.into_owned());
    }
    if is_text(optional(requirement, "kind")?, "capability") {
        fields.insert(
            "requiredCapabilities",
            capabilities(&fields, requirement.unwrap())?,
        );
    }
    base.insert("fields", fields);
    base.insert("nextSteps", Json::Array(next_steps));
    base.insert("runtimeWait", technical);
    base.insert("consumedSteps", Json::Array(vec![]));
    let mut comments = Vec::new();
    for comment in prior.get("comments").unwrap().as_array().unwrap() {
        let handoff = field(Some(comment), "handoff")?;
        if truthy(handoff) {
            let identity = concat([
                "imported-".into(),
                string(field(Some(comment), "sequence")?)?,
            ]);
            let mut changed = spread(Some(comment));
            changed.insert(
                "handoff",
                imported_handoff(handoff.unwrap(), &prior, identity)?,
            );
            comments.push(changed);
        } else {
            comments.push(comment.clone());
        }
    }
    base.insert("comments", Json::Array(comments));
    base.insert("openQuestionCount", Json::Number(questions.len() as f64));
    Ok(base)
}

pub fn upgrade_result_content(content: &Json) -> Result<Json> {
    result_content(Some(content))
}
fn result_content(content: Option<&Json>) -> Result<Json> {
    let mut current = object_rest(content, "content", "checks")?;
    let checks = field(content, "checks")?;
    if !has_length(checks) {
        return Ok(current);
    }
    let summary = concat([
        string(field(Some(&current), "summary")?)?,
        "\n\n".into(),
        findings::text(checks.unwrap()),
    ]);
    let artifacts = field(Some(&current), "artifacts")?;
    let mut artifacts = match artifacts {
        None => return Err(type_error("current.artifacts is not iterable")),
        Some(value) => iterable(value, "current.artifacts is not iterable")?,
    };
    artifacts.extend(findings::evidence(checks.unwrap())?);
    current.insert("summary", Json::String(summary));
    current.insert("artifacts", Json::Array(findings::dedup(artifacts)));
    Ok(current)
}

pub fn upgrade_result_record(result: &Json) -> Result<Json> {
    let mut current = spread(Some(result));
    current.insert("content", result_content(field(Some(result), "content")?)?);
    Ok(current)
}
