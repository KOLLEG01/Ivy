//! Durable operation, publication and native-call receipts over JSON.
use super::{Document, Operations};
use crate::journal_store::{
    mutation_ports::{LocalMutationStore, WriteFailure, typed_write_cause},
    persistent_reads::json_values::{self as v, Value},
};
use ivy_contracts::{
    IvyError, JsString, Outcome,
    hash::{hash_json as hash, hash_object},
    limits::MANAGEMENT_FRAME_BYTES,
};

const OPERATION: &str = "task-board/operation";
type Result<T, E> = std::result::Result<T, WriteFailure<E>>;
pub(super) fn text(value: impl Into<JsString>) -> Value {
    Value::String(value.into())
}
fn obj(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    v::object(fields.into_iter().map(|(key, value)| (key.into(), value)))
}
fn f<'a>(value: &'a Value, key: &str) -> &'a Value {
    v::field(value, key)
}
fn at(value: &Value, index: usize) -> std::result::Result<&Value, IvyError> {
    v::at(value, index)
}
fn eq(value: &Value, key: &str, expected: &str) -> bool {
    f(value, key).is_str(expected)
}
fn require(ok: bool, code: &str, message: &str) -> std::result::Result<(), IvyError> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn fields(value: Value, changes: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut fields = value.into_object().unwrap_or_default();
    fields.extend(changes.into_iter().map(|(key, value)| (key.into(), value)));
    v::object(fields)
}
fn elements(value: Value) -> std::result::Result<Vec<Value>, IvyError> {
    value
        .into_array()
        .ok_or_else(|| v::invalid("Expected an array."))
}
fn take_field(value: &mut Value, key: &str) -> Value {
    // Keep the first key's slot for fields()'s original order and last-value rule.
    value
        .get_mut(key)
        .map(|value| std::mem::replace(value, Value::Null))
        .unwrap_or(Value::Null)
}
fn take_at(value: Value, index: usize) -> std::result::Result<Value, IvyError> {
    Ok(elements(value)?
        .into_iter()
        .nth(index)
        .unwrap_or(Value::Null))
}
fn finished(value: &Value) -> bool {
    eq(value, "phase", "failed") || eq(value, "phase", "succeeded")
}
fn same(left: &Value, right: &Value) -> std::result::Result<bool, IvyError> {
    ivy_contracts::json::canonical_equal(left, right, MANAGEMENT_FRAME_BYTES)
}
pub(crate) fn identity_hash(caller: &Value, id: &Value) -> std::result::Result<String, IvyError> {
    Ok(hash_object([
        (&"callerPrincipalId".into(), caller),
        (&"operationId".into(), id),
    ])?[7..]
        .to_owned())
}
pub(crate) fn mutation(scope: &Value, step: Value) -> std::result::Result<Value, IvyError> {
    Ok(text(format!(
        "task-board:{}",
        &hash_object([(&"scope".into(), scope), (&"step".into(), &step)])?[7..]
    )))
}
fn recoverable<E>(error: &WriteFailure<E>) -> bool {
    typed_write_cause(error).is_some_and(|error| {
        matches!(
            error.code.as_str(),
            "revision_conflict" | "mutation_conflict"
        )
    })
}

pub(crate) async fn find_graph<E, S: LocalMutationStore<E>, C>(
    operations: &Operations<S, C>,
    caller: &Value,
    id: &Value,
) -> Result<Option<Document>, E> {
    let name = text(format!(
        "TaskBoard operation {}",
        identity_hash(caller, id)?
    ));
    let found = operations.store.named_graph(OPERATION, &name, None).await?;
    require(
        found.is_some() || !operations.store.retired_operation_graph(caller, id)?,
        "operation_expired",
        "The completed TaskBoard operation was evicted; its identity cannot be reused.",
    )?;
    if let Some(found) = &found {
        let value = &found.value;
        require(
            f(&value, "callerPrincipalId").eq(caller) && f(&value, "operationId").eq(id),
            "task_board_identity_conflict",
            "Operation identity does not match its original caller.",
        )?;
    }
    Ok(found)
}

pub(crate) async fn begin_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    request: &Value,
    actor: &Value,
) -> Result<Document, E> {
    begin_graph_with(
        operations,
        request,
        actor,
        || std::future::ready(Ok::<_, WriteFailure<E>>(Value::Null)),
        &Value::Null,
    )
    .await
}

pub(crate) async fn begin_graph_with<E, S: LocalMutationStore<E>, C: Fn() -> String, F>(
    operations: &Operations<S, C>,
    request: &Value,
    actor: &Value,
    handoff: impl FnOnce() -> F,
    publication_task: &Value,
) -> Result<Document, E>
where
    F: std::future::Future<Output = Result<Value, E>>,
{
    let caller = f(actor, "principalId");
    let id = f(request, "operationId");
    let existing = find_graph(operations, &caller, &id).await?;
    let request_hash = text(hash(request)?);
    if let Some(existing) = existing {
        require(
            f(&existing.value, "requestHash").eq(&request_hash),
            "mutation_conflict",
            "The operation identity already names different TaskBoard arguments.",
        )?;
        return Ok(existing);
    }
    let binding = handoff().await?;
    let now = text((operations.now)());
    let mut value = obj([
        ("schemaVersion", Value::Number(1.0)),
        ("operationId", id.clone()),
        ("callerPrincipalId", caller.clone()),
        ("request", request.clone()),
        ("requestHash", request_hash.clone()),
        ("actor", actor.clone()),
        ("createdAt", now.clone()),
        ("updatedAt", now),
        ("phase", text("accepted")),
        ("writes", Value::Array(Vec::new())),
        ("outcome", Value::Null),
        ("error", Value::Null),
    ]);
    if v::present(&(binding)) {
        value = fields(
            value,
            [
                ("handoffRun", f(&binding, "run").clone()),
                ("handoffWork", f(&binding, "work").clone()),
            ],
        );
    }
    if v::present(&(publication_task)) {
        value = fields(value, [("publicationTask", publication_task.clone())]);
    }
    let name = text(format!(
        "TaskBoard operation {}",
        identity_hash(&caller, &id)?
    ));
    let destination = obj([(
        "create",
        obj([
            ("parentId", operations.store.root_object_id_graph()),
            ("name", name),
        ]),
    )]);
    let result = operations
        .store
        .write_graph(
            OPERATION,
            &value,
            &mutation(&text(identity_hash(&caller, &id)?), text("accept"))?,
            &destination,
        )
        .await;
    // The returned read future is outside the source write's recovery catch.
    match result {
        Ok(pin) => {
            operations
                .store
                .read_graph(OPERATION, &pin, true, false, false)
                .await
        }
        Err(error) if recoverable(&error) => {
            let winner = find_graph(operations, &caller, &id).await?;
            require(
                match &winner {
                    Some(winner) => f(&winner.value, "requestHash").eq(&request_hash),
                    None => false,
                },
                "mutation_conflict",
                "The winning Operation has different original arguments.",
            )?;
            Ok(winner.unwrap())
        }
        Err(error) => Err(error),
    }
}

pub(crate) async fn amend_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
    mut transition: impl FnMut(&mut Value) -> Result<Option<Vec<(&'static str, Value)>>, E>,
) -> Result<Document, E> {
    for _ in 0..12 {
        let mut current = operations
            .store
            .read_graph(OPERATION, id, true, false, false)
            .await?;
        let Some(mut changes) = transition(&mut current.value)? else {
            return Ok(current);
        };
        changes.push(("updatedAt", text((operations.now)())));
        let updated = fields(current.value, changes);
        let revision = current.pin.revision;
        let mut step = JsString::from("amend:");
        step.0.extend(format!("{revision:.0}").encode_utf16());
        step.0
            .extend(format!(":{}", hash(&updated)?).encode_utf16());
        let step = text(step);
        let result = operations
            .store
            .write_graph(
                OPERATION,
                &updated,
                &mutation(id, step)?,
                &obj([
                    ("objectId", id.clone()),
                    ("expectedRevision", Value::Number(revision)),
                ]),
            )
            .await;
        match result {
            Ok(pin) => {
                return operations
                    .store
                    .read_graph(OPERATION, &pin, true, false, false)
                    .await;
            }
            Err(error)
                if typed_write_cause(&error)
                    .is_some_and(|error| error.code == "revision_conflict") => {}
            Err(error) => return Err(error),
        }
    }
    Err(IvyError::new(
        "task_board_contention",
        "The operation changed repeatedly. Reconcile its saved stages before retrying.",
    )
    .with_outcome(Outcome::Unknown)
    .into())
}

/// The preparation callback belongs to the Store's actual upgrade/snapshot
/// boundary. A decoded compatibility caller already supplies a prepared value.
pub(crate) async fn stage_graph_with<
    E,
    S: LocalMutationStore<E>,
    C: Fn() -> String,
    F: std::future::Future<Output = Result<Value, E>>,
>(
    operations: &Operations<S, C>,
    id: &Value,
    index: f64,
    key: &str,
    destination: &Value,
    prepare: impl FnOnce() -> F,
) -> Result<Value, E> {
    stage_prepared_graph_with(operations, id, index, key, destination, || async {
        operations
            .store
            .prepare_payload_graph(key, prepare().await?, destination)
            .await
    })
    .await
}

pub(crate) async fn stage_prepared_graph_with<
    E,
    S: LocalMutationStore<E>,
    C: Fn() -> String,
    F: std::future::Future<Output = Result<crate::operations::PreparedPayload, E>>,
>(
    operations: &Operations<S, C>,
    id: &Value,
    index: f64,
    key: &str,
    destination: &Value,
    prepare: impl FnOnce() -> F,
) -> Result<Value, E> {
    require(
        index.is_finite() && index.fract() == 0.0 && (0.0..16.0).contains(&index),
        "task_board_stage_limit",
        "An operation supports at most 16 ordered prepared writes.",
    )?;
    let index = index as usize;
    let current = operations
        .store
        .read_graph(OPERATION, id, true, false, false)
        .await?;
    let writes = f(&current.value, "writes");
    let agrees = |write: &Value| -> Result<(), E> {
        require(
            f(write, "contractKey").eq(&text(key)) && same(f(write, "destination"), destination)?,
            "task_board_stage_conflict",
            "This operation stage already names another contract or destination.",
        )?;
        Ok(())
    };
    let existing = at(&writes, index)?;
    if v::present(&(existing)) {
        agrees(&existing)?;
        return Ok(take_at(v::take(current.value, "writes"), index)?);
    }
    let current_value = &current.value;
    require(
        !finished(&current_value) && v::length(&writes)?.eq(&Value::Number(index as f64)),
        "task_board_stage_conflict",
        "New writes must append to the unfinished operation in order.",
    )?;
    let prepared = prepare().await?;
    let proposed = prepared.value;
    let hash = hash(&proposed)?;
    let name = text(format!("Prepared {index} {}", &hash[7..]));
    let payload = match operations
        .store
        .write_graph(
            &prepared.key,
            &proposed,
            &mutation(id, text(format!("payload:{index}:{hash}")))?,
            &obj([(
                "create",
                obj([("parentId", id.clone()), ("name", name.clone())]),
            )]),
        )
        .await
    {
        Ok(pin) => pin,
        Err(error) if recoverable(&error) => {
            let found = operations
                .store
                .named_graph(&prepared.key, &name, Some(id))
                .await?;
            require(
                match &found {
                    Some(found) => found.content_hash == hash,
                    None => false,
                },
                "task_board_evidence_mismatch",
                "Prepared payload identity has different saved bytes.",
            )?;
            found.unwrap().pin.to_json()
        }
        Err(error) => return Err(error),
    };
    let mut entry = obj([
        (
            "mutationId",
            operations
                .store
                .prepared_mutation_id_graph(&mutation(id, text(format!("apply:{index}")))?)
                .await?,
        ),
        ("contractKey", text(key)),
        (
            "contractVersion",
            match prepared.version {
                Some(version) => Value::String(version),
                None => operations.store.contract_version_graph(key)?,
            },
        ),
        (
            "payload",
            obj([
                ("object", payload),
                ("label", name),
                ("mediaType", text("application/json")),
                ("contentHash", text(hash)),
            ]),
        ),
        ("destination", destination.clone()),
        ("outcome", Value::Null),
    ]);
    if prepared.key != key {
        require(
            prepared.key == crate::operations::PREPARED_PAYLOAD,
            "task_board_contract_mismatch",
            "A prepared publication needs its registered snapshot contract.",
        )?;
        entry.insert("payloadContract", text(prepared.key));
    }
    let saved = amend_graph(operations, id, |operation| {
        let writes = f(operation, "writes");
        let winner = at(&writes, index)?;
        if v::present(&(winner)) {
            agrees(&winner)?;
            return Ok(None);
        }
        require(
            !finished(operation) && v::length(&writes)?.eq(&Value::Number(index as f64)),
            "task_board_stage_conflict",
            "The operation cannot accept this new write.",
        )?;
        let mut writes = elements(take_field(operation, "writes"))?;
        writes.push(entry.clone());
        Ok(Some(vec![
            ("phase", text("preparing")),
            ("writes", Value::Array(writes)),
            ("error", Value::Null),
        ]))
    })
    .await?;
    Ok(take_at(v::take(saved.value, "writes"), index)?)
}

pub(crate) async fn apply_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
    index: usize,
) -> Result<Value, E> {
    let current = operations
        .store
        .read_graph(OPERATION, id, true, false, false)
        .await?;
    let current_value = &current.value;
    let entry = at(f(&current_value, "writes"), index)?;
    require(
        v::present(&(entry)),
        "task_board_stage_conflict",
        "No write has been prepared at this stage.",
    )?;
    let outcome = f(&entry, "outcome");
    if v::present(&(outcome)) {
        return Ok(v::take(
            take_at(v::take(current.value, "writes"), index)?,
            "outcome",
        ));
    }
    require(
        !finished(&current_value),
        "task_board_stage_conflict",
        "A completed/failed operation cannot issue another prepared write.",
    )?;
    let key = v::string(&(f(&entry, "contractKey")))?.to_utf8()?;
    let reference = f(&entry, "payload");
    let payload_key = entry.get("payloadContract").map(v::string).transpose()?;
    let payload_key = payload_key.as_ref().map(JsString::to_utf8).transpose()?;
    require(
        payload_key
            .as_deref()
            .is_none_or(|key| key == crate::operations::PREPARED_PAYLOAD),
        "task_board_contract_mismatch",
        "The prepared payload contract is not supported.",
    )?;
    let payload = operations
        .store
        .read_graph(
            payload_key.as_deref().unwrap_or(&key),
            f(&reference, "object"),
            false,
            false,
            true,
        )
        .await?;
    require(
        payload
            .parent_id
            .as_ref()
            .is_some_and(|parent| id.as_js_string() == Some(parent))
            && f(&reference, "contentHash").is_str(&payload.content_hash)
            && f(&reference, "mediaType").eq(&text("application/json")),
        "task_board_evidence_mismatch",
        "The staged payload must be owned by this operation and match its immutable content hash.",
    )?;
    let value = &payload.value;
    let (value, references) = if payload_key.is_some() {
        require(
            value.get("schemaVersion") == Some(&Value::Number(1.0))
                && value.get("contractKey") == entry.get("contractKey")
                && value.get("contractVersion") == entry.get("contractVersion"),
            "task_board_evidence_mismatch",
            "The snapshot must retain its original publication contract and version.",
        )?;
        (
            value.required("content")?,
            Some(value.required("references")?),
        )
    } else {
        (value, None)
    };
    let result = operations
        .store
        .write_prepared_graph(
            &key,
            value,
            f(&entry, "mutationId"),
            f(&entry, "destination"),
            f(&entry, "contractVersion"),
            references,
        )
        .await?;
    amend_graph(operations, id, |operation| {
        let writes = f(operation, "writes");
        let latest = at(&writes, index)?;
        require(
            v::present(&(latest))
                && same(
                    &fields(latest.clone(), [("outcome", Value::Null)]),
                    &fields(entry.clone(), [("outcome", Value::Null)]),
                )?,
            "task_board_stage_conflict",
            "The applied stage no longer has its original saved intent.",
        )?;
        let outcome = f(&latest, "outcome");
        if v::present(&(outcome)) {
            require(
                same(&outcome, &result)?,
                "task_board_evidence_mismatch",
                "The retained write receipt differs from Hive replay.",
            )?;
            return Ok(None);
        }
        let mut writes = elements(take_field(operation, "writes"))?;
        let latest = std::mem::replace(&mut writes[index], Value::Null);
        writes[index] = fields(latest, [("outcome", result.clone())]);
        Ok(Some(vec![
            ("phase", text("publishing")),
            ("writes", Value::Array(writes)),
            ("error", Value::Null),
        ]))
    })
    .await?;
    Ok(result)
}

pub(crate) async fn finish_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
    outcome: &Value,
) -> Result<Value, E> {
    let done = amend_graph(operations, id, |operation| {
        require(
            f(outcome, "operationId").eq(f(operation, "operationId")),
            "task_board_identity_conflict",
            "An outcome must retain its original operation ID.",
        )?;
        if eq(operation, "phase", "succeeded") {
            require(
                same(f(operation, "outcome"), outcome)?,
                "mutation_conflict",
                "The completed action has another original outcome.",
            )?;
            return Ok(None);
        }
        let all_done = if eq(operation, "phase", "failed") {
            false
        } else {
            let mut done = true;
            for write in v::items(f(operation, "writes"))? {
                if matches!(f(&write, "outcome"), Value::Null) {
                    done = false;
                    break;
                }
            }
            done
        };
        require(
            all_done,
            "task_board_publication_pending",
            "All prepared writes must have durable receipts before completing the action.",
        )?;
        Ok(Some(vec![
            ("phase", text("succeeded")),
            ("outcome", outcome.clone()),
            ("error", Value::Null),
        ]))
    })
    .await?;
    let value = &done.value;
    operations
        .store
        .complete_local_workflow_graph(id, f(&value, "updatedAt"))?;
    Ok(v::take(done.value, "outcome"))
}

pub(crate) async fn failure_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
    error: &IvyError,
    needs_attention: bool,
) -> Result<Document, E> {
    let failed = amend_graph(operations, id, |operation| {
        if finished(operation) {
            return Ok(None);
        }
        Ok(Some(vec![
            (
                "phase",
                text(if needs_attention {
                    "needs_attention"
                } else {
                    "failed"
                }),
            ),
            ("outcome", Value::Null),
            (
                "error",
                obj([
                    ("code", text(error.code_js())),
                    ("message", text(error.message_js())),
                    ("execution", (&error.outcome_json()).clone()),
                ]),
            ),
        ]))
    })
    .await?;
    let value = &failed.value;
    if eq(&value, "phase", "failed") {
        let at = f(&value, "updatedAt");
        operations.store.complete_local_workflow_graph(id, &at)?;
        if eq(f(&value, "actor"), "source", "scheduler") {
            let request = f(&value, "request");
            if (eq(&request, "action", "start") || eq(&request, "action", "continue"))
                && v::has(&request, "intent")
            {
                let intent = f(f(&request, "intent"), "objectId");
                if operations.store.local_key_graph(&intent)?.as_ref()
                    == Some(&JsString::from("task-board/native-plan"))
                {
                    operations
                        .store
                        .complete_local_workflow_graph(&intent, &at)?;
                }
            }
        }
    }
    Ok(failed)
}

fn find_call<'a>(
    calls: &'a Value,
    key: &Value,
) -> std::result::Result<Option<&'a Value>, IvyError> {
    Ok(v::items(calls)?.iter().find(|call| f(call, "key") == key))
}
fn take_call(calls: Value, key: &Value) -> std::result::Result<Value, IvyError> {
    Ok(elements(calls)?
        .into_iter()
        .find(|call| f(call, "key") == key)
        .unwrap_or(Value::Null))
}
pub(crate) async fn prepare_native_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
    proposed: &Value,
) -> Result<Value, E> {
    let saved = amend_graph(operations, id, |operation| {
        let calls = f(operation, "nativeCalls");
        let existing = if v::nullish(&calls) {
            None
        } else {
            find_call(&calls, f(proposed, "key"))?
        };
        if let Some(existing) = existing.filter(|existing| v::present(existing)) {
            require(
                same(
                    &fields(existing.clone(), [("observation", Value::Null)]),
                    proposed,
                )?,
                "task_board_native_intent_mismatch",
                "The native lifecycle stage already has another retained request.",
            )?;
            return Ok(None);
        }
        require(
            !finished(operation),
            "task_board_operation_failed",
            "A finished action cannot prepare native work.",
        )?;
        let mut calls = if v::nullish(&calls) {
            Vec::new()
        } else {
            elements(take_field(operation, "nativeCalls"))?
        };
        calls.push(proposed.clone());
        Ok(Some(vec![("nativeCalls", Value::Array(calls))]))
    })
    .await?;
    Ok(take_call(
        v::take(saved.value, "nativeCalls"),
        f(proposed, "key"),
    )?)
}
pub(crate) async fn observe_native_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
    key: &Value,
    observation: &Value,
) -> Result<Value, E> {
    let saved = amend_graph(operations, id, |operation| {
        let saved_calls = f(operation, "nativeCalls");
        let calls = if v::nullish(&saved_calls) {
            &[][..]
        } else {
            v::items(saved_calls)?
        };
        let mut index = None;
        for (n, call) in calls.iter().enumerate() {
            if f(call, "key").eq(key) {
                index = Some(n);
                break;
            }
        }
        require(
            match index {
                Some(index) => f(&calls[index], "operationId").eq(f(observation, "operationId")),
                None => false,
            },
            "task_board_native_intent_mismatch",
            "The observation must identify the retained native lifecycle call.",
        )?;
        let index = index.unwrap();
        if same(f(&calls[index], "observation"), observation)? {
            return Ok(None);
        }
        let mut calls = elements(take_field(operation, "nativeCalls"))?;
        let call = std::mem::replace(&mut calls[index], Value::Null);
        calls[index] = fields(call, [("observation", observation.clone())]);
        Ok(Some(vec![("nativeCalls", Value::Array(calls))]))
    })
    .await?;
    Ok(take_call(v::take(saved.value, "nativeCalls"), key)?)
}
pub(crate) async fn archive_identity_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
) -> Result<Value, E> {
    let current = operations
        .store
        .read_graph(OPERATION, id, true, false, false)
        .await?;
    let existing = f(&current.value, "archiveMutationId");
    if v::present(&(existing)) {
        return Ok(existing.clone());
    }
    let identity = operations
        .store
        .prepared_mutation_id_graph(&mutation(id, text("archive-membership"))?)
        .await?;
    let saved = amend_graph(operations, id, |operation| {
        Ok(if v::present(&(f(operation, "archiveMutationId"))) {
            None
        } else {
            Some(vec![("archiveMutationId", identity.clone())])
        })
    })
    .await?;
    Ok(v::take(saved.value, "archiveMutationId"))
}
