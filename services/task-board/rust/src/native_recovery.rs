//! Saved recovery lineage and failed empty contexts. These reads neither dispatch
//! native work nor replace an uncertain original execution.
use crate::{
    native_evidence::NativeEvidence,
    native_intent::NativeIntent,
    operations::{Document, Store},
    read_evidence::ReadEvidenceStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::collections::HashSet;

fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(value) => *value,
        Json::Number(value) => *value != 0.0 && !value.is_nan(),
        Json::String(value) => !value.0.is_empty(),
        _ => true,
    }
}

pub async fn context_recovery_history<S: Store>(
    store: &S,
    task: &Json,
    task_id: &JsString,
) -> IvyResult<Option<Json>> {
    let mut pin = field(task, "lastHistory").clone();
    let mut visited = HashSet::new();
    while truthy(&pin) && truthy(field(task, "lastRun")) {
        if !visited.insert(string(&pin, "objectId")?.clone()) {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "The recovery History lineage contains a cycle.",
            ));
        }
        let history = store.read("task-board/history", &pin, true).await?;
        if history.value.get("taskId").and_then(Json::as_js_string) != Some(task_id) {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "Recovery History must belong to this ticket.",
            ));
        }
        if field(&history.value, "priorRun").get("objectId")
            != field(task, "lastRun").get("objectId")
            || field(&history.value, "kind") == &Json::from("reassigned")
        {
            break;
        }
        if field(&history.value, "kind") == &Json::from("recovered")
            && field(&history.value, "actor").get("source") == Some(&Json::from("scheduler"))
        {
            return Ok(Some(history.value));
        }
        pin = field(&history.value, "previousHistory").clone();
    }
    Ok(None)
}

pub async fn fresh_context_recovery<S: Store>(store: &S, task: &Json) -> IvyResult<Option<Json>> {
    if truthy(field(task, "primaryResourceRef")) || !truthy(field(task, "lastRun")) {
        return Ok(None);
    }
    let run = store
        .read("task-board/run", field(task, "lastRun"), true)
        .await?;
    if field(&run.value, "phase") != &Json::from("failed")
        || truthy(field(&run.value, "turnId"))
        || truthy(field(&run.value, "cancellation"))
    {
        return Ok(None);
    }
    Ok(
        if context_recovery_history(store, task, string(&run.value, "taskId")?)
            .await?
            .is_some()
        {
            Some(run.value)
        } else {
            None
        },
    )
}

pub fn missing_native_thread(reply: &Json, thread_id: &JsString) -> IvyResult<bool> {
    let Some(error) = reply.get("error") else {
        return Ok(false);
    };
    let message = string(error, "message")?;
    if !thread_id.0.is_empty()
        && !message
            .0
            .windows(thread_id.0.len())
            .any(|part| part == thread_id.0)
    {
        return Ok(false);
    }
    // The source's non-Unicode /i expression contains only ASCII keywords. Its
    // wildcard cannot cross any JavaScript line terminator; the two ordered
    // alternatives together accept either keyword order on the same line.
    let lower = message
        .0
        .iter()
        .map(|unit| {
            if (65..=90).contains(unit) {
                unit + 32
            } else {
                *unit
            }
        })
        .collect::<Vec<_>>();
    let contains = |line: &[u16], word: &str| {
        line.windows(word.len())
            .any(|part| part.iter().copied().eq(word.encode_utf16()))
    };
    Ok(lower
        .split(|unit| matches!(unit, 10 | 13 | 0x2028 | 0x2029))
        .any(|line| {
            contains(line, "no rollout found")
                || ((contains(line, "thread") || contains(line, "rollout"))
                    && (contains(line, "not found") || contains(line, "does not exist")))
        }))
}

/// Read-only proof selection. The owner must supply its accepted current
/// question policy; there is no legacy or permissive default. A positive result
/// alone is neither restoration nor replacement authority.
pub struct ContextRecovery<'a, S, Q> {
    pub store: &'a S,
    pub catalogs: &'a CheckedNativeContracts,
    pub principal_id: JsString,
    pub unanswered_question: Q,
}

struct UnstartedAttempt {
    saved: Document,
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(value: Option<&Json>, expected: &Json) -> IvyResult<bool> {
    match value {
        Some(value) => Ok(canonical(value, MANAGEMENT_FRAME_BYTES)?
            == canonical(expected, MANAGEMENT_FRAME_BYTES)?),
        None => Ok(false),
    }
}
fn primary(task: &Json) -> Option<&Json> {
    let value = field(task, "primaryResourceRef");
    (truthy(value)
        && value.get("namespace") == Some(&Json::from("codex"))
        && value.get("kind") == Some(&Json::from("thread"))
        && truthy(field(task, "lastRun")))
    .then_some(value)
}
fn missing_proof(result: IvyResult<bool>) -> IvyResult<bool> {
    match result {
        Err(error) if error.code == "not_found" => Ok(false),
        other => other,
    }
}
fn archived_message(reply: &Json, thread_id: &JsString) -> IvyResult<bool> {
    let Some(error) = reply.get("error") else {
        return Ok(false);
    };
    let message = string(error, "message")?;
    if !thread_id.0.is_empty()
        && !message
            .0
            .windows(thread_id.0.len())
            .any(|part| part == thread_id.0)
    {
        return Ok(false);
    }
    let lower: Vec<_> = message
        .0
        .iter()
        .map(|unit| {
            if (65..=90).contains(unit) {
                unit + 32
            } else {
                *unit
            }
        })
        .collect();
    let phrase: Vec<_> = "is archived".encode_utf16().collect();
    let word = |unit: u16| {
        (65..=90).contains(&unit)
            || (97..=122).contains(&unit)
            || (48..=57).contains(&unit)
            || unit == 95
    };
    Ok(lower
        .windows(phrase.len())
        .enumerate()
        .any(|(index, part)| {
            part == phrase
                && (index == 0 || !word(lower[index - 1]))
                && (index + phrase.len() == lower.len() || !word(lower[index + phrase.len()]))
        }))
}

impl<S: Store + ReadEvidenceStore, Q: Fn(&Json, &JsString) -> IvyResult<bool>>
    ContextRecovery<'_, S, Q>
{
    async fn observed(
        &self,
        proof: &UnstartedAttempt,
        call_pin: &Json,
        primary: &Json,
    ) -> IvyResult<Option<Json>> {
        // Terminal closure may retain a prepared pin older than the observed
        // call. Read its current identity, then verify that exact returned pin.
        let call = Store::read(
            self.store,
            "task-board/native-call",
            &Json::String(string(call_pin, "objectId")?.clone()),
            true,
        )
        .await?;
        if !truthy(field(&call.value, "evidence"))
            || field(&call.value, "ownerPhase") != &Json::from("failed")
            || field(&call.value, "state") != &Json::from("observed")
        {
            return Ok(None);
        }
        let intent = NativeIntent {
            store: self.store,
            catalogs: self.catalogs,
        };
        let identity =
            intent.evidence_identity(&proof.saved.value, &call.value, &self.principal_id)?;
        let observation = NativeEvidence {
            store: self.store,
            catalogs: self.catalogs,
        }
        .read(&call.pin, call.value.required("evidence")?, &identity)
        .await?;
        require(
            observation.get("phase") == call.value.get("ownerPhase")
                && observation.get("code") == call.value.get("code")
                && observation.get("epoch") == call.value.get("epoch"),
            "task_board_evidence_mismatch",
            "Recovery must use the verified original native receipt.",
        )?;
        let request = intent.resolve_native_request(call.value.required("request")?)?;
        require(
            request.required("params")?.get("threadId") == primary.get("nativeId"),
            "task_board_primary_mismatch",
            "Recovery proof must identify the saved native context.",
        )?;
        Ok(Some(observation))
    }

    async fn unstarted_attempt(
        &self,
        task_id: &JsString,
        primary: &Json,
        run: &Document,
    ) -> IvyResult<Option<UnstartedAttempt>> {
        let value = &run.value;
        if value.get("taskId").and_then(Json::as_js_string) != Some(task_id)
            || !same(value.get("primaryResourceRef"), primary)?
            || field(value, "phase") != &Json::from("failed")
            || field(value, "externalOutcome").get("state") != Some(&Json::from("failed"))
            || truthy(field(value, "turnId"))
            || truthy(field(value, "cancellation"))
            || !truthy(field(value, "result"))
        {
            return Ok(None);
        }
        let result = Store::read(
            self.store,
            "task-board/result",
            value.required("result")?,
            true,
        )
        .await?;
        let result_run = field(&result.value, "run");
        require(
            result.value.get("taskId").and_then(Json::as_js_string) == Some(task_id)
                && result.value.get("kind") == Some(&Json::from("final"))
                && result_run.get("objectId") == run.pin.get("objectId")
                && result_run
                    .required("revision")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)?
                    < run
                        .pin
                        .required("revision")?
                        .as_f64()
                        .ok_or_else(IvyError::internal)?,
            "task_board_evidence_mismatch",
            "Recovery requires the exact saved attempt before its terminal publication.",
        )?;
        let saved = Store::read(self.store, "task-board/run", result_run, true).await?;
        let calls = field(&saved.value, "calls");
        if truthy(field(&saved.value, "turnId"))
            || truthy(field(calls, "steer"))
            || truthy(field(calls, "interrupt"))
        {
            return Ok(None);
        }
        let proof = UnstartedAttempt { saved };
        let turn = field(field(&proof.saved.value, "calls"), "turn");
        if truthy(turn) {
            let Some(turn) = self.observed(&proof, turn, primary).await? else {
                return Ok(None);
            };
            if turn.get("method") != Some(&Json::from("turn/start"))
                || turn.get("phase") != Some(&Json::from("failed"))
                || turn.get("code") != Some(&Json::from("request_prevented"))
                || turn.get("requestId") != Some(&Json::Null)
                || turn.get("epoch") != Some(&Json::Null)
                || turn.get("reply") != Some(&Json::Null)
            {
                return Ok(None);
            }
        }
        Ok(Some(proof))
    }

    pub async fn unstarted_archived_context(&self, task: &Document) -> IvyResult<bool> {
        let Some(primary) = primary(&task.value) else {
            return Ok(false);
        };
        if (self.unanswered_question)(
            &task.value,
            string(task.value.required("lastRun")?, "objectId")?,
        )? {
            return Ok(false);
        }
        missing_proof(
            async {
                let run = Store::read(
                    self.store,
                    "task-board/run",
                    task.value.required("lastRun")?,
                    true,
                )
                .await?;
                let Some(proof) = self
                    .unstarted_attempt(string(&task.pin, "objectId")?, primary, &run)
                    .await?
                else {
                    return Ok(false);
                };
                let thread = field(field(&proof.saved.value, "calls"), "thread");
                if !truthy(thread) {
                    return Ok(false);
                }
                let Some(observed) = self.observed(&proof, thread, primary).await? else {
                    return Ok(false);
                };
                if observed.get("method") != Some(&Json::from("thread/resume")) {
                    return Ok(false);
                }
                archived_message(field(&observed, "reply"), string(primary, "nativeId")?)
            }
            .await,
        )
    }

    pub async fn unstarted_missing_context(&self, task: &Document) -> IvyResult<bool> {
        let Some(primary) = primary(&task.value) else {
            return Ok(false);
        };
        if (self.unanswered_question)(
            &task.value,
            string(task.value.required("lastRun")?, "objectId")?,
        )? {
            return Ok(false);
        }
        missing_proof(
            async {
                let mut visited = HashSet::new();
                let mut pin = field(&task.value, "lastRun").clone();
                let mut first = true;
                while truthy(&pin) {
                    require(
                        visited.insert(string(&pin, "objectId")?.clone()),
                        "task_board_evidence_mismatch",
                        "The native context Run lineage contains a cycle.",
                    )?;
                    let run = Store::read(self.store, "task-board/run", &pin, true).await?;
                    if !same(run.value.get("primaryResourceRef"), primary)? {
                        return Ok(!first);
                    }
                    if first
                        && context_recovery_history(
                            self.store,
                            &task.value,
                            string(&task.pin, "objectId")?,
                        )
                        .await?
                        .is_some()
                    {
                        return Ok(false);
                    }
                    let Some(proof) = self
                        .unstarted_attempt(string(&task.pin, "objectId")?, primary, &run)
                        .await?
                    else {
                        return Ok(false);
                    };
                    if first {
                        let thread = field(field(&proof.saved.value, "calls"), "thread");
                        if !truthy(thread) {
                            return Ok(false);
                        }
                        let Some(thread) = self.observed(&proof, thread, primary).await? else {
                            return Ok(false);
                        };
                        if thread.get("method") != Some(&Json::from("thread/resume"))
                            || !missing_native_thread(
                                field(&thread, "reply"),
                                string(primary, "nativeId")?,
                            )?
                        {
                            return Ok(false);
                        }
                        first = false;
                    }
                    let initial = Store::read(
                        self.store,
                        "task-board/task",
                        &Json::object([
                            ("objectId", task.pin.required("objectId")?.clone()),
                            (
                                "revision",
                                run.value
                                    .required("originalRequest")?
                                    .required("expectedRevision")?
                                    .clone(),
                            ),
                        ]),
                        true,
                    )
                    .await?;
                    if !same(initial.value.get("primaryResourceRef"), primary)? {
                        return Ok(true);
                    }
                    pin = field(&initial.value, "lastRun").clone();
                }
                Ok(!first)
            }
            .await,
        )
    }
}
