//! Current bounded Task read projections. Persistence and execution-condition
//! precedence remain explicit owner bindings; no legacy full-reader default.
use super::conversation::cross_agent_comment as cross_agent;
use crate::coordination::next_steps::{
    NextStepPolicies, NextStepStore, assess_steps, next_step_time, step_condition,
};
use futures_util::future::{join_all, poll_fn};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    future::Future,
    task::Poll,
};

/// Immutable prefix reads use the supplied tree, task identity and half-open
/// range verbatim. A missing indexed summary explicitly selects the retained
/// full-record fallback; it never means that this ticket has no questions.
#[allow(async_fn_in_trait)]
pub trait CommentReadStore: NextStepStore {
    fn comment_summary(
        &self,
        task: &Json,
        tree: &Json,
        task_id: &JsString,
    ) -> IvyResult<Option<Json>>;
    async fn comment_range(
        &self,
        tree: &Json,
        task_id: &JsString,
        start: usize,
        end: usize,
    ) -> IvyResult<Vec<Json>>;
}
pub trait CommentReadPolicies: NextStepPolicies {
    fn now(&self) -> JsString;
    fn execution_condition(&self, task: &Json, steps: Option<&Json>) -> IvyResult<Json>;
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn array(value: &Json) -> &[Json] {
    value.as_array().unwrap_or(&[])
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn number(value: &Json) -> IvyResult<usize> {
    value
        .as_f64()
        .map(|v| v as usize)
        .ok_or_else(IvyError::internal)
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
fn optional_number(value: &Json, key: &str, default: usize) -> IvyResult<usize> {
    value
        .get(key)
        .filter(|v| !v.is_null())
        .map(number)
        .unwrap_or(Ok(default))
}
pub fn comment_summary<P: NextStepPolicies>(
    policies: &P,
    task: &Json,
    task_id: &JsString,
) -> IvyResult<Json> {
    let open: HashSet<_> = policies.open_questions(task)?.into_iter().collect();
    let referenced: HashSet<_> = array(field(task, "nextSteps"))
        .iter()
        .flat_map(|step| array(field(step, "requires")))
        .filter(|c| is(c, "kind", "question") || is(c, "kind", "user"))
        .filter_map(|c| field(c, "questionId").as_js_string())
        .collect();
    let comments = array(field(task, "comments"));
    let mut questions = Vec::new();
    for comment in comments {
        if !is(comment, "purpose", "question") || truthy(field(comment, "sourceTaskId")) {
            continue;
        }
        let id = string(comment, "commentId")?;
        if !open.contains(id) && !referenced.contains(id) {
            continue;
        }
        let mut question = Json::object([
            ("commentId", Json::String(id.clone())),
            ("sequence", comment.required("sequence")?.clone()),
            ("open", open.contains(id).into()),
        ]);
        if truthy(field(comment, "recipient")) {
            question.insert("recipient", field(comment, "recipient").clone());
        }
        questions.push(question);
    }
    Ok(Json::object([
        (
            "directCount",
            Json::Number(comments.iter().filter(|c| !cross_agent(c, task_id)).count() as f64),
        ),
        ("questions", Json::Array(questions)),
    ]))
}

/// Poll every initiated read before propagating the first completed error, as
/// Promise.all does; a stalled peer read cannot hide an available failure.
async fn batch<T>(
    reads: impl IntoIterator<Item = impl Future<Output = IvyResult<T>>>,
) -> IvyResult<Vec<T>> {
    let first_error = RefCell::new(None);
    let mut reads = Box::pin(join_all(reads.into_iter().map(|read| {
        let first_error = &first_error;
        async move {
            let result = read.await;
            if let Err(error) = &result {
                if first_error.borrow().is_none() {
                    *first_error.borrow_mut() = Some(error.clone());
                }
            }
            result
        }
    })));
    poll_fn(|cx| {
        let result = reads.as_mut().poll(cx);
        if let Some(error) = first_error.borrow_mut().take() {
            Poll::Ready(Err(error))
        } else {
            result.map(|values| values.into_iter().collect())
        }
    })
    .await
}
async fn range<S: CommentReadStore>(
    store: &S,
    indexed: Option<&Json>,
    task: &Json,
    task_id: &JsString,
    start: usize,
    end: usize,
) -> IvyResult<Vec<Json>> {
    if let Some(tree) = indexed {
        return store.comment_range(tree, task_id, start, end).await;
    }
    let comments = array(field(task, "comments"));
    let receipts = array(field(task, "commentDeliveries"));
    Ok(
        comments[start.min(comments.len())..end.min(comments.len()).max(start.min(comments.len()))]
            .iter()
            .map(|comment| {
                let receipt = receipts
                    .iter()
                    .find(|r| field(r, "commentId") == field(comment, "commentId"));
                Json::object([
                    ("comment", comment.clone()),
                    ("receipt", receipt.cloned().unwrap_or(Json::Null)),
                ])
            })
            .collect(),
    )
}

pub async fn read_task<S: CommentReadStore, P: CommentReadPolicies>(
    store: &S,
    policies: &P,
    header: &Json,
    query: &Json,
) -> IvyResult<Json> {
    let pin = header.required("pin")?;
    let task_id = string(pin, "objectId")?;
    let original = header.required("value")?;
    let tree = field(original, "commentStore").clone();
    let indexed = if is(&tree, "kind", "tree") {
        store
            .comment_summary(original, &tree, task_id)?
            .filter(truthy)
    } else {
        None
    };
    let task = if indexed.is_none() {
        store.read("task-board/task", pin, false).await?
    } else {
        original.clone()
    };
    let summary = match &indexed {
        Some(summary) => summary.clone(),
        None => comment_summary(policies, &task, task_id)?,
    };
    let total = if indexed.is_some() {
        number(tree.required("length")?)?
    } else {
        array(field(&task, "comments")).len()
    };
    let steps = if truthy(field(&task, "claim")) || truthy(field(&task, "publication")) {
        None
    } else {
        Some(step_condition(
            &assess_steps(
                store,
                policies,
                &task,
                &policies.now(),
                &HashMap::new(),
                Some(array(field(&summary, "questions"))),
            )
            .await?,
        ))
    };
    let condition = policies.execution_condition(&task, steps.as_ref())?;
    let archived = header
        .required("metadata")?
        .required("effectivelyArchived")?
        .clone();
    if is(query, "view", "card") {
        let view = Json::object([
            ("object", pin.clone()),
            ("task", task),
            ("executionCondition", condition),
            ("archived", archived),
        ]);
        return task_card(policies, &view, Some(&summary), Some(total));
    }

    let comments_query = field(query, "comments");
    let limit = optional_number(comments_query, "limit", 50)?;
    let before = optional_number(comments_query, "before", total + 1)?.min(total + 1);
    let ascending = comments_query.get("offset").is_some();
    let questions = array(field(&summary, "questions"));
    let candidates = if truthy(field(comments_query, "questionsOnly")) {
        Some(
            questions
                .iter()
                .filter(|q| truthy(field(q, "open")))
                .map(|q| number(q.required("sequence")?))
                .collect::<IvyResult<Vec<_>>>()?
                .into_iter()
                .filter(|sequence| *sequence < before)
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let end = before.saturating_sub(1);
    let offset = if ascending {
        optional_number(comments_query, "offset", 0)?
            .min(candidates.as_ref().map_or(total, Vec::len))
    } else {
        candidates
            .as_ref()
            .map_or(end, Vec::len)
            .saturating_sub(limit)
    };
    let tree = indexed.as_ref().map(|_| &tree);
    let mut selected =
        if let Some(sequences) = &candidates {
            let mut entries = Vec::new();
            for part in sequences[offset..(offset + limit).min(sequences.len())].chunks(8) {
                entries.extend(
                    batch(part.iter().map(|sequence| {
                        range(store, tree, &task, task_id, sequence - 1, *sequence)
                    }))
                    .await?
                    .into_iter()
                    .flatten(),
                );
            }
            entries
        } else {
            range(
                store,
                tree,
                &task,
                task_id,
                offset,
                (offset + limit).min(if ascending { total } else { end }),
            )
            .await?
        };
    let pending: Vec<_> = questions
        .iter()
        .filter(|q| truthy(field(q, "open")) && !is(field(q, "recipient"), "kind", "task"))
        .collect();
    let mut user_questions = Vec::new();
    for part in pending[pending.len().saturating_sub(100)..].chunks(8) {
        user_questions.extend(
            batch(part.iter().map(|question| {
                let selected = &selected;
                let task = &task;
                async move {
                    if let Some(entry) = selected.iter().find(|entry| {
                        field(field(entry, "comment"), "commentId") == field(question, "commentId")
                    }) {
                        return Ok(field(entry, "comment").clone());
                    }
                    let sequence = number(question.required("sequence")?)?;
                    let entries = range(store, tree, task, task_id, sequence - 1, sequence).await?;
                    entries
                        .first()
                        .and_then(|entry| entry.get("comment"))
                        .cloned()
                        .ok_or_else(IvyError::internal)
                }
            }))
            .await?,
        );
    }
    if !ascending {
        selected.reverse();
    }
    let next_offset =
        if ascending && offset + selected.len() < candidates.as_ref().map_or(total, Vec::len) {
            Json::Number((offset + selected.len()) as f64)
        } else {
            Json::Null
        };
    let next_before = if offset > 0 && !selected.is_empty() {
        selected
            .iter()
            .map(|entry| number(field(field(entry, "comment"), "sequence")))
            .collect::<IvyResult<Vec<_>>>()?
            .into_iter()
            .min()
            .map(|v| Json::Number(v as f64))
            .unwrap_or(Json::Null)
    } else {
        Json::Null
    };
    let mut content = task;
    content.remove("commentStore");
    content.insert(
        "comments",
        Json::Array(
            selected
                .iter()
                .map(|entry| field(entry, "comment").clone())
                .collect(),
        ),
    );
    content.insert(
        "commentDeliveries",
        Json::Array(
            selected
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("receipt")
                        .filter(|receipt| truthy(receipt))
                        .cloned()
                })
                .collect(),
        ),
    );
    let mut result = Json::object([
        ("object", pin.clone()),
        ("task", content),
        ("executionCondition", condition),
        ("archived", archived),
    ]);
    result.insert("openQuestions", Json::Array(user_questions));
    result.insert(
        "commentPage",
        Json::object([
            (
                "total",
                Json::Number(if candidates.is_some() {
                    questions
                        .iter()
                        .filter(|q| truthy(field(q, "open")))
                        .count()
                } else {
                    total
                } as f64),
            ),
            ("offset", Json::Number(offset as f64)),
            ("nextOffset", next_offset),
            ("nextBefore", next_before),
            ("order", if ascending { "asc" } else { "desc" }.into()),
        ]),
    );
    Ok(result)
}

pub fn task_card<P: NextStepPolicies>(
    policies: &P,
    view: &Json,
    summary: Option<&Json>,
    total: Option<usize>,
) -> IvyResult<Json> {
    let task = view.required("task")?;
    let comments = array(field(task, "comments"));
    let mut fields = Json::object([] as [(&str, Json); 0]);
    for name in [
        "title",
        "category",
        "control",
        "priority",
        "executionRequirement",
        "dueAt",
    ] {
        if let Some(value) = task.required("fields")?.get(name) {
            fields.insert(name, value.clone());
        }
    }
    let direct = if let Some(summary) = summary {
        summary.required("directCount")?.clone()
    } else {
        let task_id = string(view.required("object")?, "objectId")?;
        Json::Number(comments.iter().filter(|c| !cross_agent(c, task_id)).count() as f64)
    };
    let open_count = if let Some(summary) = summary {
        array(field(summary, "questions"))
            .iter()
            .filter(|q| truthy(field(q, "open")) && !is(field(q, "recipient"), "kind", "task"))
            .count()
    } else {
        let open = policies.open_questions(task)?;
        comments
            .iter()
            .filter(|c| {
                field(c, "commentId")
                    .as_js_string()
                    .is_some_and(|id| open.contains(id))
                    && !is(field(c, "recipient"), "kind", "task")
            })
            .count()
    };
    let mut card_task = Json::object([] as [(&str, Json); 0]);
    for name in [
        "taskKey",
        "workflowState",
        "runtimeWait",
        "publication",
        "latestResult",
    ] {
        if let Some(value) = task.get(name) {
            card_task.insert(name, value.clone());
        }
    }
    card_task.insert("fields", fields);
    card_task.insert(
        "nextStepAt",
        next_step_time(task).map(Json::String).unwrap_or(Json::Null),
    );
    let mut claim = Json::Null;
    if truthy(field(task, "claim")) {
        claim = Json::object([] as [(&str, Json); 0]);
        for key in ["phase", "target"] {
            if let Some(value) = field(task, "claim").get(key) {
                claim.insert(key, value.clone());
            }
        }
    }
    card_task.insert("claim", claim);
    card_task.insert("openQuestionCount", Json::Number(open_count as f64));
    Ok(Json::object([
        ("object", view.required("object")?.clone()),
        (
            "executionCondition",
            view.required("executionCondition")?.clone(),
        ),
        (
            "commentCount",
            Json::Number(total.unwrap_or(comments.len()) as f64),
        ),
        ("directCommentCount", direct),
        ("task", card_task),
    ]))
}

pub fn task_view_page<P: NextStepPolicies>(
    policies: &P,
    view: &Json,
    query: &Json,
) -> IvyResult<Json> {
    let task = view.required("task")?;
    let comments = array(field(task, "comments"));
    let q = field(query, "comments");
    let all: Vec<_> = if truthy(field(q, "questionsOnly")) {
        let open = policies.open_questions(task)?;
        comments
            .iter()
            .filter(|c| {
                field(c, "commentId")
                    .as_js_string()
                    .is_some_and(|id| open.contains(id))
            })
            .collect()
    } else {
        comments.iter().collect()
    };
    let limit = optional_number(q, "limit", 50)?;
    let offset = optional_number(q, "offset", all.len().saturating_sub(limit))?;
    let selected =
        &all[offset.min(all.len())..(offset + limit).min(all.len()).max(offset.min(all.len()))];
    let open = policies.open_questions(task)?;
    let mut content = task.clone();
    content.insert(
        "comments",
        Json::Array(selected.iter().map(|c| (*c).clone()).collect()),
    );
    content.insert(
        "commentDeliveries",
        Json::Array(
            array(field(task, "commentDeliveries"))
                .iter()
                .filter(|r| {
                    selected
                        .iter()
                        .any(|c| field(c, "commentId") == field(r, "commentId"))
                })
                .cloned()
                .collect(),
        ),
    );
    let mut result = view.clone();
    result.insert("task", content);
    result.insert(
        "openQuestions",
        Json::Array(
            comments
                .iter()
                .filter(|c| {
                    field(c, "commentId")
                        .as_js_string()
                        .is_some_and(|id| open.contains(id))
                        && !is(field(c, "recipient"), "kind", "task")
                })
                .cloned()
                .collect(),
        ),
    );
    result.insert(
        "commentPage",
        Json::object([
            ("total", Json::Number(all.len() as f64)),
            ("offset", Json::Number(offset as f64)),
            (
                "nextOffset",
                if offset + selected.len() < all.len() {
                    Json::Number((offset + selected.len()) as f64)
                } else {
                    Json::Null
                },
            ),
        ]),
    );
    Ok(result)
}
