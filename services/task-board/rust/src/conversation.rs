//! Current conversation projections and the Shared question-resolver binding.
//! Validation, clock and condition-store dependencies remain explicit.
use super::comment_reads::CommentReadPolicies;
use crate::coordination::next_steps::NextStepPolicies;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};

pub type QuestionResolver<'a> = dyn Fn(&Json, Option<&JsString>) -> IvyResult<Vec<Json>> + 'a;
pub type ReadCondition<'a> = dyn Fn(&Json, Option<&Json>) -> IvyResult<Json> + 'a;

/// Explicit question, validation, clock and condition-store dependencies.
/// `current` binds the SDK resolver while retaining the other owner bindings.
pub struct ConversationPolicies<'a> {
    pub questions: &'a QuestionResolver<'a>,
    pub validate_next_step: &'a dyn Fn(&Json) -> IvyResult<()>,
    pub now: &'a dyn Fn() -> JsString,
    pub condition: &'a ReadCondition<'a>,
}
impl<'a> ConversationPolicies<'a> {
    /// Bind the current SDK resolver without changing its full-value or failure
    /// semantics. The remaining runtime dependencies have no implicit defaults.
    pub fn current(
        validate_next_step: &'a dyn Fn(&Json) -> IvyResult<()>,
        now: &'a dyn Fn() -> JsString,
        condition: &'a ReadCondition<'a>,
    ) -> Self {
        Self {
            questions: &ivy_sdk::open_task_questions,
            validate_next_step,
            now,
            condition,
        }
    }
}
impl NextStepPolicies for ConversationPolicies<'_> {
    fn open_questions(&self, task: &Json) -> IvyResult<Vec<JsString>> {
        open_question_ids(self.questions, task)
    }
    fn validate_next_step(&self, step: &Json) -> IvyResult<()> {
        (self.validate_next_step)(step)
    }
}
impl CommentReadPolicies for ConversationPolicies<'_> {
    fn now(&self) -> JsString {
        (self.now)()
    }
    fn execution_condition(&self, task: &Json, steps: Option<&Json>) -> IvyResult<Json> {
        (self.condition)(task, steps)
    }
}

fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn items(value: &Json) -> &[Json] {
    value.as_array().unwrap_or(&[])
}
fn is(value: &Json, key: &str, expected: &str) -> bool {
    value.get(key) == Some(&expected.into())
}
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
}

/// A ticket's own worker stays direct; addressed task questions and mirrored
/// question sources are cross-agent even when they carry the same task ID.
pub fn cross_agent_comment(comment: &Json, task_id: &JsString) -> bool {
    is(field(comment, "recipient"), "kind", "task")
        || truthy(field(comment, "questionSource"))
        || is(comment, "authorKind", "agent")
            && truthy(field(comment, "sourceTaskId"))
            && field(comment, "sourceTaskId").as_js_string() != Some(task_id)
}

pub fn unread_comments(task: &Json, task_id: &JsString, state: Option<&Json>) -> IvyResult<Json> {
    let agents: Vec<_> = items(field(task, "comments"))
        .iter()
        .filter(|c| is(c, "authorKind", "agent"))
        .collect();
    let count = |key| -> IvyResult<usize> {
        state
            .and_then(|s| s.get(key))
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_f64()
                    .map(|n| n as usize)
                    .ok_or_else(IvyError::internal)
            })
            .unwrap_or(Ok(0))
    };
    let prefix = count("readAgentCommentCount")?.min(agents.len());
    let direct = agents
        .iter()
        .filter(|c| !cross_agent_comment(c, task_id))
        .count();
    let read_direct = count("readDirectAgentCommentCount")?.max(
        agents[..prefix]
            .iter()
            .filter(|c| !cross_agent_comment(c, task_id))
            .count(),
    );
    let unread_direct = direct.saturating_sub(read_direct);
    Ok(Json::object([
        (
            "unreadAgentComments",
            Json::Number(
                (unread_direct
                    + agents[prefix..]
                        .iter()
                        .filter(|c| cross_agent_comment(c, task_id))
                        .count()) as f64,
            ),
        ),
        (
            "unreadDirectAgentComments",
            Json::Number(unread_direct as f64),
        ),
    ]))
}

pub fn run_handoff(task: &Json, run_id: &JsString) -> Option<Json> {
    items(field(task, "comments"))
        .iter()
        .rev()
        .find(|c| {
            is(c, "purpose", "handoff")
                && field(field(c, "run"), "objectId").as_js_string() == Some(run_id)
        })
        .cloned()
}

pub fn open_question_ids(resolver: &QuestionResolver<'_>, task: &Json) -> IvyResult<Vec<JsString>> {
    resolver(task, None)?
        .iter()
        .map(|c| {
            c.required("commentId")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)
        })
        .collect()
}

pub fn unanswered_question(
    resolver: &QuestionResolver<'_>,
    task: &Json,
    run_id: &JsString,
) -> IvyResult<bool> {
    Ok(!resolver(task, Some(run_id))?.is_empty())
}

pub fn queued_user_input(task: &Json) -> bool {
    items(field(task, "commentDeliveries")).iter().any(|d| {
        is(d, "state", "queued")
            && items(field(task, "comments")).iter().any(|c| {
                field(c, "commentId") == field(d, "commentId") && is(c, "authorKind", "user")
            })
    })
}

pub fn queued_task_edits(task: &Json) -> Vec<JsString> {
    items(field(task, "commentDeliveries"))
        .iter()
        .filter(|d| {
            is(d, "state", "queued")
                && items(field(task, "comments")).iter().any(|c| {
                    field(c, "commentId") == field(d, "commentId")
                        && is(c, "authorKind", "system")
                        && is(c, "body", "Ticket edited")
                        && !truthy(field(c, "nativeInput"))
                })
        })
        .filter_map(|d| field(d, "commentId").as_js_string().cloned())
        .collect()
}
