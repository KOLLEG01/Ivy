//! Mirror the native owner's exact request/answer identity into the ticket.
//! This does not answer native inputs or confer native execution authority.
use crate::native_inspector::{InspectionStore, management_read};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::hash_json,
    json::{object_entries, stringify, try_js_string},
    limits::MANAGEMENT_FRAME_BYTES,
};

#[allow(async_fn_in_trait)]
pub trait NativeCommentStore: InspectionStore {
    /// The engine supplies normal TaskBoard validation, ownership, durable
    /// operation identity and publication. Uncertain outcomes remain retained.
    async fn native_comment(&self, request: &Json) -> IvyResult<()>;
}
fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn text(v: &Json, key: &str) -> IvyResult<JsString> {
    v.required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
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
fn values(v: &Json) -> Vec<&Json> {
    match v {
        Json::Array(values) => values.iter().collect(),
        Json::Object(_) => object_entries(v).into_iter().map(|(_, v)| v).collect(),
        _ => vec![],
    }
}
fn join(values: &[&Json]) -> IvyResult<JsString> {
    let mut output = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.extend([10, 10]);
        }
        if **value != Json::Null {
            output.extend(try_js_string(value)?.0);
        }
    }
    Ok(JsString(output))
}
fn answer_text(reply: &Json) -> IvyResult<JsString> {
    let mut answers = Vec::new();
    for answer in values(field(reply, "answers")) {
        if answer == &Json::Null {
            return Err(IvyError::internal());
        }
        match field(answer, "answers") {
            Json::Null => {}
            Json::Array(parts) => answers.extend(parts.iter()),
            value => answers.push(value),
        }
    }
    let joined = join(&answers)?;
    if !joined.0.is_empty() {
        return Ok(joined);
    }
    Ok(reply
        .get("decision")
        .and_then(Json::as_js_string)
        .cloned()
        .unwrap_or_else(|| "Response recorded.".into()))
}
fn question_text(params: &Json) -> IvyResult<JsString> {
    let mut questions = Vec::new();
    if let Some(values) = params.get("questions").and_then(Json::as_array) {
        for question in values {
            if question == &Json::Null {
                return Err(IvyError::internal());
            }
            if let Some(value) = question.get("question").filter(|v| truthy(v)) {
                questions.push(value);
            }
        }
    }
    let joined = join(&questions)?;
    if !joined.0.is_empty() {
        return Ok(joined);
    }
    Ok(params
        .get("reason")
        .and_then(Json::as_js_string)
        .cloned()
        .unwrap_or_else(|| "The agent needs your approval to continue.".into()))
}
fn contains_comment(task: &Json, id: &str) -> bool {
    field(task, "comments").as_array().is_some_and(|comments| {
        comments
            .iter()
            .any(|comment| field(comment, "commentId").as_js_string() == Some(&id.into()))
    })
}
fn request(
    task: &Json,
    id: &str,
    purpose: &str,
    body: JsString,
    input: Json,
    reply_to: Json,
) -> IvyResult<Json> {
    let revision = task.required("pin")?.required("revision")?.clone();
    Ok(Json::object([
        ("action", "comment".into()),
        ("taskId", task.required("taskId")?.clone()),
        ("expectedRevision", revision.clone()),
        (
            "operationId",
            format!("{id}-{}", try_js_string(&revision)?.to_utf8()?).into(),
        ),
        ("commentId", id.into()),
        ("purpose", purpose.into()),
        ("body", Json::String(body)),
        ("nativeInput", input),
        ("requests", Json::Array(vec![])),
        ("responses", Json::Array(vec![])),
        ("attachments", Json::Array(vec![])),
        ("replyTo", reply_to),
        ("agentDelivery", "none".into()),
    ]))
}

pub async fn sync_native_comments<S: NativeCommentStore>(
    store: &S,
    run_id: &JsString,
) -> IvyResult<()> {
    let run = store
        .read_document("task-board/run", &Json::String(run_id.clone()), true)
        .await?;
    let primary = field(&run.value, "primaryResourceRef");
    if !truthy(primary) || !truthy(field(&run.value, "turnId")) {
        return Ok(());
    }
    let task_id = text(&run.value, "taskId")?;
    let current = store
        .read_document("task-board/task", &Json::String(task_id.clone()), true)
        .await?;
    if field(field(field(&current.value, "claim"), "run"), "objectId").as_js_string()
        != Some(run_id)
    {
        return Ok(());
    }
    let target = field(&run.value, "target");
    let page = management_read(
        store.tools(),
        &text(target, "serviceNodeId")?,
        "inputs",
        Json::object([
            ("includeExpired", true.into()),
            ("limit", Json::Number(128.0)),
        ]),
    )
    .await?;
    for input in page
        .required("items")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        let identity = input.required("identity")?;
        if field(identity, "serviceNodeId") != field(target, "serviceNodeId")
            || field(input, "threadId") != field(primary, "nativeId")
            || truthy(field(input, "turnId"))
                && field(input, "turnId") != field(&run.value, "turnId")
            || field(identity, "epoch") != field(&run.value, "nativeEpoch")
        {
            continue;
        }
        let hash = hash_json(identity)?;
        let question_id = format!("native-question-{}", &hash[7..]);
        let secret = stringify(input.required("params")?, MANAGEMENT_FRAME_BYTES)?
            .contains("\"isSecret\":true");
        let mut retained = input.clone();
        if secret {
            retained.insert("reply", Json::Null);
        }
        let reply = field(field(input, "reply"), "result");
        let answer = answer_text(reply)?;
        let body = question_text(input.required("params")?)?;
        let task = store
            .read_document("task-board/task", &Json::String(task_id.clone()), true)
            .await?;
        if !contains_comment(&task.value, &question_id) {
            let mut pending = retained.clone();
            for (key, value) in [
                ("state", "pending".into()),
                ("reply", Json::Null),
                ("answerOperationId", Json::Null),
                ("answerCallerPrincipalId", Json::Null),
                ("code", Json::Null),
            ] {
                pending.insert(key, value);
            }
            let mut request = request(
                &Json::object([
                    ("pin", task.pin.clone()),
                    ("taskId", Json::String(task_id.clone())),
                ]),
                &question_id,
                "question",
                body,
                pending,
                Json::Null,
            )?;
            request.insert(
                "contactUser",
                (field(input, "state").as_js_string() == Some(&"pending".into())).into(),
            );
            store.native_comment(&request).await?;
        }
        if !["answered", "expired"]
            .iter()
            .any(|state| field(input, "state").as_js_string() == Some(&(*state).into()))
        {
            continue;
        }
        let task = store
            .read_document("task-board/task", &Json::String(task_id.clone()), true)
            .await?;
        let response_id = format!("native-response-{}", &hash[7..]);
        if contains_comment(&task.value, &response_id) {
            continue;
        }
        let body = if field(input, "state").as_js_string() == Some(&"expired".into()) {
            "This request has expired.".into()
        } else if secret {
            "A confidential response was provided.".into()
        } else {
            JsString(answer.0.into_iter().take(16_000).collect())
        };
        let request = request(
            &Json::object([("pin", task.pin), ("taskId", Json::String(task_id.clone()))]),
            &response_id,
            "update",
            body,
            retained,
            question_id.into(),
        )?;
        store.native_comment(&request).await?;
    }
    Ok(())
}
