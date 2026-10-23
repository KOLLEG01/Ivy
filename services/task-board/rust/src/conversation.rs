//! Current conversation projections and the Shared question-resolver binding.
//! Validation, clock and condition-store dependencies remain explicit.
use super::{comment_reads::CommentReadPolicies, read_policy::QuestionPolicies};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
pub type QuestionResolver<'a, E = IvyError> =
    dyn Fn(&Json, Option<&JsString>) -> Result<Vec<Json>, E> + 'a;
pub type ReadCondition<'a, E = IvyError> = dyn Fn(&Json, Option<&Json>) -> Result<Json, E> + 'a;
pub struct ConversationPolicies<'a, E = IvyError> {
    pub questions: &'a QuestionResolver<'a, E>,
    pub condition: &'a ReadCondition<'a, E>,
}
fn current_questions<E: From<IvyError>>(
    task: &Json,
    run: Option<&JsString>,
) -> Result<Vec<Json>, E> {
    ivy_sdk::open_task_questions(task, run).map_err(E::from)
}
impl<'a, E: From<IvyError>> ConversationPolicies<'a, E> {
    pub fn with_current_questions(condition: &'a ReadCondition<'a, E>) -> Self {
        Self {
            questions: &current_questions::<E>,
            condition,
        }
    }
}
impl<E: From<IvyError>> QuestionPolicies<E> for ConversationPolicies<'_, E> {
    fn open_questions(&self, task: &Json) -> Result<Vec<JsString>, E> {
        open_question_ids(self.questions, task)
    }
}
impl<E: From<IvyError>> CommentReadPolicies<E> for ConversationPolicies<'_, E> {
    fn execution_condition(&self, task: &Json, wake: Option<&Json>) -> Result<Json, E> {
        (self.condition)(task, wake)
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
/// A ticket's own worker stays direct; addressed task questions and mirrored
/// question sources are cross-agent even when they carry the same task ID.
pub fn cross_agent_comment(comment: &Json, task_id: &JsString) -> bool {
    cross_agent_comment_raw(comment, Some(&Json::String(task_id.clone())))
}

/// Compare admitted string task identities without coercion.
pub fn cross_agent_comment_raw(comment: &Json, task_id: Option<&Json>) -> bool {
    is(field(comment, "recipient"), "kind", "task")
        || !field(comment, "questionSource").is_null()
        || is(comment, "authorKind", "agent")
            && field(comment, "sourceTaskId")
                .as_js_string()
                .is_some_and(|id| !id.0.is_empty())
            && !same_owner(comment.get("sourceTaskId"), task_id)
}
fn same_owner(a: Option<&Json>, b: Option<&Json>) -> bool {
    a.and_then(Json::as_js_string) == b.and_then(Json::as_js_string)
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

pub fn open_question_ids<E: From<IvyError>>(
    resolver: &QuestionResolver<'_, E>,
    task: &Json,
) -> Result<Vec<JsString>, E> {
    Ok(resolver(task, None)?
        .iter()
        .map(|c| {
            c.required("commentId")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)
        })
        .collect::<IvyResult<_>>()?)
}

pub fn unanswered_question<E>(
    resolver: &QuestionResolver<'_, E>,
    task: &Json,
    run_id: &JsString,
) -> Result<bool, E> {
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
                        && field(c, "nativeInput").is_null()
                })
        })
        .filter_map(|d| field(d, "commentId").as_js_string().cloned())
        .collect()
}
