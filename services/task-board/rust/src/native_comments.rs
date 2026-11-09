//! Mirror the native owner's exact request/answer identity into the ticket.
//! This does not answer native inputs or confer native execution authority.
use crate::{
    evidence_bytes::{BlobStore, ChunkKey},
    native_inspector::{InspectionStore, NativeInspector, management_read},
    native_plan::{read_target_matches, routed_target},
    operations::Document,
    read_evidence::ReadEvidenceStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, agent_schema::validate_agent,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::stringify,
    limits::MANAGEMENT_FRAME_BYTES, native_backend::BackendIdentity,
};
use ivy_sdk::tools::ToolClients;
use std::future::Future;

#[allow(async_fn_in_trait)]
pub trait NativeCommentStore: InspectionStore {
    /// The engine supplies normal TaskBoard validation, ownership, durable
    /// operation identity and publication. Uncertain outcomes remain retained.
    async fn native_comment(&self, request: &Json) -> IvyResult<()>;
}
/// Keep mirroring reads on the admitted inspection journal and publication on
/// the current engine's retained native-comment admission path. The callback
/// must supply that path; it is not a direct store-write or legacy policy default.
pub struct NativeCommentJournal<'a, S, W> {
    pub inspection: &'a S,
    pub native_comment: W,
}
impl<S: BlobStore<Pin = Json, Error = IvyError>, W> BlobStore for NativeCommentJournal<'_, S, W> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, key: ChunkKey, name: &str, parent: &JsString, bytes: &[u8]) -> IvyResult<Json> {
        self.inspection.save(key, name, parent, bytes)
    }
    fn read(&self, key: ChunkKey, pin: &Json, parent: &JsString) -> IvyResult<Vec<u8>> {
        self.inspection.read(key, pin, parent)
    }
}
impl<S: ReadEvidenceStore, W> ReadEvidenceStore for NativeCommentJournal<'_, S, W> {
    fn validate(&self, definition: &str, value: &Json) -> IvyResult<()> {
        self.inspection.validate(definition, value)
    }
    async fn read_document(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        self.inspection.read_document(key, address, primary).await
    }
    async fn read_task_routing(&self, address: &Json) -> IvyResult<Json> {
        self.inspection.read_task_routing(address).await
    }
    async fn write_document(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json> {
        self.inspection
            .write_document(key, value, mutation, destination)
            .await
    }
}
impl<S: InspectionStore, W> InspectionStore for NativeCommentJournal<'_, S, W> {
    async fn task_header(&self, address: &Json) -> IvyResult<Document> {
        self.inspection.task_header(address).await
    }
    async fn task_has_comment(
        &self,
        task: &Json,
        task_id: &JsString,
        id: &JsString,
    ) -> IvyResult<bool> {
        self.inspection.task_has_comment(task, task_id, id).await
    }
    async fn task_native_questions(
        &self,
        task: &Json,
        task_id: &JsString,
        run_id: &JsString,
        comment_id: Option<&JsString>,
    ) -> IvyResult<Vec<Json>> {
        self.inspection
            .task_native_questions(task, task_id, run_id, comment_id)
            .await
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        self.inspection.verify_owner().await
    }
    async fn named_document(
        &self,
        key: &str,
        name: &str,
        parent: &JsString,
    ) -> IvyResult<Option<Document>> {
        self.inspection.named_document(key, name, parent).await
    }
    fn tools(&self) -> &ToolClients {
        self.inspection.tools()
    }
    fn principal_id(&self) -> JsString {
        self.inspection.principal_id()
    }
    fn now(&self) -> JsString {
        self.inspection.now()
    }
}
impl<S: InspectionStore, W: Fn(Json) -> F, F: Future<Output = IvyResult<Json>>> NativeCommentStore
    for NativeCommentJournal<'_, S, W>
{
    async fn native_comment(&self, request: &Json) -> IvyResult<()> {
        (self.native_comment)(request.clone()).await.map(|_| ())
    }
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
fn present(v: &Json) -> bool {
    !v.is_null()
}
fn values(v: &Json) -> Vec<&Json> {
    match v {
        Json::Array(values) => values.iter().collect(),
        Json::Object(fields) => fields.iter().map(|(_, value)| value).collect(),
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
            output.extend(&value.as_js_string().ok_or_else(IvyError::internal)?.0);
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
            if let Some(value) = question
                .get("question")
                .filter(|v| v.as_js_string().is_some_and(|s| !s.0.is_empty()))
            {
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
#[cfg(test)]
mod text_tests {
    use super::*;
    #[test]
    fn native_input_text_preserves_question_and_answer_strings() {
        let params = Json::object([(
            "questions",
            Json::Array(vec![
                Json::object([("question", "First?".into())]),
                Json::object([("question", "Second?".into())]),
            ]),
        )]);
        assert_eq!(
            question_text(&params).unwrap(),
            JsString::from("First?\n\nSecond?")
        );
        let answers = Json::object([(
            "answers",
            Json::object([
                (
                    "first",
                    Json::object([("answers", Json::Array(vec!["A".into(), "B".into()]))]),
                ),
                (
                    "second",
                    Json::object([("answers", Json::Array(vec!["C".into()]))]),
                ),
            ]),
        )]);
        assert_eq!(
            answer_text(&answers).unwrap(),
            JsString::from("A\n\nB\n\nC")
        );
        assert!(
            answer_text(&Json::object([(
                "answers",
                Json::Array(vec![Json::object([(
                    "answers",
                    Json::Array(vec![Json::Number(1.0)])
                )]),])
            )]))
            .is_err(),
            "Protocol answer text cannot coerce arbitrary JSON"
        );
    }
}
pub(crate) fn question_for_run(comment: &Json, run_id: &JsString) -> bool {
    field(comment, "purpose").is_str("question")
        && field(field(comment, "author"), "source").is_str("native")
        && field(field(comment, "run"), "objectId").as_js_string() == Some(run_id)
}
async fn recorded_question<S: NativeCommentStore>(
    store: &S,
    task: &Json,
    task_id: &JsString,
    id: &str,
    run_id: &JsString,
    input: &Json,
) -> IvyResult<bool> {
    Ok(store
        .task_native_questions(task, task_id, run_id, Some(&id.into()))
        .await?
        .iter()
        .any(|comment| {
            field(comment, "commentId").is_str(id)
                && question_for_run(comment, run_id)
                && [
                    "identity",
                    "method",
                    "params",
                    "observedAt",
                    "threadId",
                    "turnId",
                ]
                .iter()
                .all(|key| field(field(comment, "nativeInput"), key) == field(input, key))
        }))
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
            format!("{id}-{}", revision.as_f64().ok_or_else(IvyError::internal)?).into(),
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
    catalogs: &CheckedNativeContracts,
    run_id: &JsString,
) -> IvyResult<()> {
    let run = store
        .read_document("task-board/run", &Json::String(run_id.clone()), true)
        .await?;
    let primary = field(&run.value, "primaryResourceRef");
    if !present(primary) || !present(field(&run.value, "turnId")) {
        return Ok(());
    }
    let task_id = text(&run.value, "taskId")?;
    let current = store.task_header(&Json::String(task_id.clone())).await?;
    if field(field(field(&current.value, "claim"), "run"), "objectId").as_js_string()
        != Some(run_id)
    {
        return Ok(());
    }
    let original = run.value.required("target")?;
    let target = routed_target(&current.value, original)?;
    let select_initial = target == *original
        && field(field(&current.value, "commentStore"), "kind").is_str("tree")
        && field(&current.value, "comments")
            .as_array()
            .is_some_and(|v| v.is_empty())
        && field(&current.value, "commentDeliveries")
            .as_array()
            .is_some_and(|v| v.is_empty());
    let questions = if select_initial {
        vec![]
    } else {
        store
            .task_native_questions(&current.value, &task_id, run_id, None)
            .await?
    };
    let recorded: Vec<JsString> = questions
        .iter()
        .filter_map(|comment| field(comment, "commentId").as_js_string().cloned())
        .collect();
    let backend = BackendIdentity::optional(target.get("backend"))?;
    let mut retired = Vec::new();
    for comment in questions.iter().filter(|_| target != *original) {
        if !question_for_run(comment, run_id) {
            continue;
        }
        let input = field(comment, "nativeInput");
        let identity = field(input, "identity");
        let Some(service) = field(identity, "serviceNodeId").as_js_string() else {
            continue;
        };
        let previous_backend = BackendIdentity::optional(identity.get("backend"))?;
        if Some(service) == field(&target, "serviceNodeId").as_js_string()
            && previous_backend == backend
            || !read_target_matches(
                &current.value,
                original,
                service,
                previous_backend.as_ref(),
                true,
            )?
            || field(input, "threadId") != field(primary, "nativeId")
            || present(field(input, "turnId"))
                && field(input, "turnId") != field(&run.value, "turnId")
        {
            continue;
        }
        let hash = hash_json(identity)?;
        if field(comment, "commentId").is_str(&format!("native-question-{}", &hash[7..]))
            && !store
                .task_has_comment(
                    &current.value,
                    &task_id,
                    &format!("native-response-{}", &hash[7..]).into(),
                )
                .await?
            && !retired.contains(identity)
        {
            retired.push(identity.clone());
        }
    }
    drop(questions);
    let initial = select_initial.then_some(current.value);
    let inspector = NativeInspector { store, catalogs };
    let context = if target != *original {
        Some(inspector.context(&run.pin.to_json()).await?)
    } else {
        None
    };
    let epoch = context
        .as_ref()
        .map(|context| Json::String(context.epoch.clone()))
        .unwrap_or_else(|| field(&run.value, "nativeEpoch").clone());
    let page = management_read(
        store.tools(),
        &text(&target, "serviceNodeId")?,
        "inputs",
        BackendIdentity::select_arguments(
            backend.as_ref(),
            Json::object([
                ("includeExpired", true.into()),
                ("limit", Json::Number(128.0)),
            ]),
        )?,
    )
    .await?;
    validate_agent("PendingInputPage", &page)?;
    if !BackendIdentity::matches(&page, backend.as_ref()) {
        return Err(IvyError::new(
            "task_board_target_mismatch",
            "Native input pages must retain the selected backend and account.",
        ));
    }
    if let Some(context) = context.as_ref() {
        if page.get("epoch") != Some(&epoch)
            || inspector.context(&run.pin.to_json()).await? != *context
        {
            return Err(IvyError::new(
                "task_board_native_epoch_changed",
                "The selected owner changed while reading native inputs.",
            ));
        }
    }
    for input in page
        .required("items")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        let identity = input.required("identity")?;
        if field(identity, "serviceNodeId") != field(&target, "serviceNodeId")
            || !BackendIdentity::matches(identity, backend.as_ref())
            || field(input, "threadId") != field(primary, "nativeId")
            || present(field(input, "turnId"))
                && field(input, "turnId") != field(&run.value, "turnId")
        {
            continue;
        }
        let hash = hash_json(identity)?;
        let question_id = format!("native-question-{}", &hash[7..]);
        let historical = field(identity, "epoch") != &epoch;
        // A previous connection cannot introduce a new question. Only finish
        // mirroring a question already recorded by this Run's native owner.
        let historical_task = if historical {
            if !["answered", "expired"]
                .iter()
                .any(|state| field(input, "state").is_str(state))
            {
                continue;
            }
            let recorded = if let Some(initial) = initial.as_ref() {
                !store
                    .task_native_questions(
                        initial,
                        &task_id,
                        run_id,
                        Some(&question_id.clone().into()),
                    )
                    .await?
                    .is_empty()
            } else {
                recorded.contains(&JsString::from(question_id.as_str()))
            };
            if !recorded {
                continue;
            }
            let task = input_task(store, &task_id, run_id, original, &target).await?;
            if !recorded_question(store, &task.value, &task_id, &question_id, run_id, input).await?
            {
                continue;
            }
            Some(task)
        } else {
            None
        };
        let secret = stringify(input.required("params")?, MANAGEMENT_FRAME_BYTES)?
            .contains("\"isSecret\":true");
        let mut retained = input.clone();
        if secret {
            retained.insert("reply", Json::Null);
        }
        let reply = field(field(input, "reply"), "result");
        let answer = answer_text(reply)?;
        let body = question_text(input.required("params")?)?;
        let task = match historical_task {
            Some(task) => task,
            None => input_task(store, &task_id, run_id, original, &target).await?,
        };
        if !store
            .task_has_comment(&task.value, &task_id, &question_id.clone().into())
            .await?
        {
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
                    ("pin", task.pin.to_json()),
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
        drop(task);
        if !["answered", "expired"]
            .iter()
            .any(|state| field(input, "state").as_js_string() == Some(&(*state).into()))
        {
            continue;
        }
        let task = input_task(store, &task_id, run_id, original, &target).await?;
        if historical
            && !recorded_question(store, &task.value, &task_id, &question_id, run_id, input).await?
        {
            continue;
        }
        let response_id = format!("native-response-{}", &hash[7..]);
        if store
            .task_has_comment(&task.value, &task_id, &response_id.clone().into())
            .await?
        {
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
            &Json::object([
                ("pin", task.pin.to_json()),
                ("taskId", Json::String(task_id.clone())),
            ]),
            &response_id,
            "update",
            body,
            retained,
            question_id.into(),
        )?;
        store.native_comment(&request).await?;
    }
    drop(initial);
    // Historical requests remain with their original connection. Read only
    // unresolved, recorded identities from explicitly published former owners;
    // never reinterpret them as inputs of the selected destination.
    for identity in retired {
        let hash = hash_json(&identity)?;
        let question_id = format!("native-question-{}", &hash[7..]);
        let response_id = format!("native-response-{}", &hash[7..]);
        let task = input_task(store, &task_id, run_id, original, &target).await?;
        if store
            .task_has_comment(&task.value, &task_id, &response_id.clone().into())
            .await?
        {
            continue;
        }
        drop(task);
        let previous_backend = BackendIdentity::optional(identity.get("backend"))?;
        let page = management_read(
            store.tools(),
            &text(&identity, "serviceNodeId")?,
            "inputs",
            BackendIdentity::select_arguments(
                previous_backend.as_ref(),
                Json::object([
                    ("identity", identity.clone()),
                    ("includeExpired", true.into()),
                ]),
            )?,
        )
        .await;
        // Losing an old owner cannot prove expiration or transfer authority.
        let Ok(page) = page else {
            continue;
        };
        validate_agent("PendingInputPage", &page)?;
        if !BackendIdentity::matches(&page, previous_backend.as_ref()) {
            return Err(IvyError::new(
                "task_board_target_mismatch",
                "Historical input pages must retain their original backend and account.",
            ));
        }
        for input in page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            if input.get("identity") != Some(&identity)
                || !["answered", "expired"]
                    .iter()
                    .any(|state| field(input, "state").is_str(state))
            {
                continue;
            }
            let task = input_task(store, &task_id, run_id, original, &target).await?;
            if !recorded_question(store, &task.value, &task_id, &question_id, run_id, input).await?
                || store
                    .task_has_comment(&task.value, &task_id, &response_id.clone().into())
                    .await?
            {
                continue;
            }
            let secret = stringify(input.required("params")?, MANAGEMENT_FRAME_BYTES)?
                .contains("\"isSecret\":true");
            let mut retained = input.clone();
            if secret {
                retained.insert("reply", Json::Null);
            }
            let body = if field(input, "state").is_str("expired") {
                "This request has expired.".into()
            } else if secret {
                "A confidential response was provided.".into()
            } else {
                JsString(
                    answer_text(field(field(input, "reply"), "result"))?
                        .0
                        .into_iter()
                        .take(16_000)
                        .collect(),
                )
            };
            let request = request(
                &Json::object([
                    ("pin", task.pin.to_json()),
                    ("taskId", Json::String(task_id.clone())),
                ]),
                &response_id,
                "update",
                body,
                retained,
                question_id.clone().into(),
            )?;
            store.native_comment(&request).await?;
        }
    }
    Ok(())
}

async fn input_task<S: NativeCommentStore>(
    store: &S,
    task_id: &JsString,
    run_id: &JsString,
    original: &Json,
    target: &Json,
) -> IvyResult<Document> {
    let task = store.task_header(&Json::String(task_id.clone())).await?;
    if field(field(field(&task.value, "claim"), "run"), "objectId").as_js_string() != Some(run_id)
        || routed_target(&task.value, original)? != *target
    {
        return Err(IvyError::new(
            "task_board_identity_conflict",
            "Native input comments require the same current Run and selected route.",
        ));
    }
    Ok(task)
}
