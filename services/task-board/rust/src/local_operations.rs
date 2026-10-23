//! Durable operation, publication and native-call receipts over JSON.
use super::Operations;
use crate::journal_store::{
    mutation_ports::{LocalMutationStore, WriteFailure, typed_write_cause},
    persistent_reads::json_values::{self as v, Value},
};
use ivy_contracts::{IvyError, JsString, Outcome, hash::digest, limits::MANAGEMENT_FRAME_BYTES};

const OPERATION: &str = "task-board/operation";
type Result<T, E> = std::result::Result<T, WriteFailure<E>>;
pub(super) fn text(value: impl Into<JsString>) -> Value {
    Value::String(value.into())
}
fn obj(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    v::object(fields.into_iter().map(|(key, value)| (key.into(), value)))
}
fn f(value: &Value, key: &str) -> std::result::Result<Value, IvyError> {
    v::field(value, key)
}
fn at(value: &Value, index: usize) -> std::result::Result<Value, IvyError> {
    v::at(value, index)
}
fn eq(value: &Value, key: &str, expected: &str) -> std::result::Result<bool, IvyError> {
    Ok(f(value, key)?.eq(&text(expected)))
}
fn require(ok: bool, code: &str, message: &str) -> std::result::Result<(), IvyError> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn fields(value: &Value, changes: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut fields = v::fields(value);
    fields.extend(changes.into_iter().map(|(key, value)| (key.into(), value)));
    v::object(fields)
}
fn array(values: impl IntoIterator<Item = Value>) -> Value {
    v::array(values)
}
fn elements(value: &Value) -> std::result::Result<Vec<Value>, IvyError> {
    v::items(value)
}
fn finished(value: &Value) -> std::result::Result<bool, IvyError> {
    Ok(eq(value, "phase", "failed")? || eq(value, "phase", "succeeded")?)
}
fn same(left: &Value, right: &Value) -> std::result::Result<bool, IvyError> {
    Ok(
        ivy_contracts::json::canonical(&(left), MANAGEMENT_FRAME_BYTES)?
            == ivy_contracts::json::canonical(&(right), MANAGEMENT_FRAME_BYTES)?,
    )
}
fn hash(value: &Value) -> std::result::Result<String, IvyError> {
    Ok(digest(
        ivy_contracts::json::canonical(&(value), MANAGEMENT_FRAME_BYTES)?.as_bytes(),
    ))
}
pub(crate) fn identity_hash(caller: &Value, id: &Value) -> std::result::Result<String, IvyError> {
    Ok(hash(&obj([
        ("callerPrincipalId", caller.clone()),
        ("operationId", id.clone()),
    ]))?[7..]
        .to_owned())
}
pub(crate) fn mutation(scope: &Value, step: Value) -> std::result::Result<Value, IvyError> {
    Ok(text(format!(
        "task-board:{}",
        &hash(&obj([("scope", scope.clone()), ("step", step)]))?[7..]
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
) -> Result<Option<Value>, E> {
    let name = text(format!(
        "TaskBoard operation {}",
        identity_hash(caller, id)?
    ));
    let found = operations.store.named_graph(OPERATION, &name, None).await?;
    require(
        found.as_ref().is_some_and(v::present)
            || !operations.store.retired_operation_graph(caller, id)?,
        "operation_expired",
        "The completed TaskBoard operation was evicted; its identity cannot be reused.",
    )?;
    if let Some(found) = &found {
        let value = f(found, "value")?;
        require(
            f(&value, "callerPrincipalId")?.eq(caller) && f(&value, "operationId")?.eq(id),
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
) -> Result<Value, E> {
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
) -> Result<Value, E>
where
    F: std::future::Future<Output = Result<Value, E>>,
{
    let caller = f(actor, "principalId")?;
    let id = f(request, "operationId")?;
    let existing = find_graph(operations, &caller, &id).await?;
    let request_hash = text(hash(request)?);
    if let Some(existing) = existing {
        require(
            f(&f(&existing, "value")?, "requestHash")?.eq(&request_hash),
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
        ("writes", array([])),
        ("outcome", Value::Null),
        ("error", Value::Null),
    ]);
    if v::present(&(binding)) {
        value = fields(
            &value,
            [
                ("handoffRun", f(&binding, "run")?),
                ("handoffWork", f(&binding, "work")?),
            ],
        );
    }
    if v::present(&(publication_task)) {
        value = fields(&value, [("publicationTask", publication_task.clone())]);
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
                    Some(winner) => f(&f(winner, "value")?, "requestHash")?.eq(&request_hash),
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
    mut transition: impl FnMut(&Value) -> Result<Option<Value>, E>,
) -> Result<Value, E> {
    for _ in 0..12 {
        let current = operations
            .store
            .read_graph(OPERATION, id, true, false, false)
            .await?;
        let Some(next) = transition(&f(&current, "value")?)? else {
            return Ok(current);
        };
        let updated = fields(&next, [("updatedAt", text((operations.now)()))]);
        let revision = f(&f(&current, "pin")?, "revision")?;
        let mut step = JsString::from("amend:");
        step.0
            .extend(JsString::from(format!("{:.0}", v::number(&revision)?)).0);
        step.0
            .extend(format!(":{}", hash(&updated)?).encode_utf16());
        let step = text(step);
        let result = operations
            .store
            .write_graph(
                OPERATION,
                &updated,
                &mutation(id, step)?,
                &obj([("objectId", id.clone()), ("expectedRevision", revision)]),
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
    let writes = f(&f(&current, "value")?, "writes")?;
    let agrees = |write: &Value| -> Result<(), E> {
        require(
            f(write, "contractKey")?.eq(&text(key))
                && same(&f(write, "destination")?, destination)?,
            "task_board_stage_conflict",
            "This operation stage already names another contract or destination.",
        )?;
        Ok(())
    };
    let existing = at(&writes, index)?;
    if v::present(&(existing)) {
        agrees(&existing)?;
        return Ok(existing);
    }
    let current_value = f(&current, "value")?;
    require(
        !finished(&current_value)? && v::length(&writes)?.eq(&Value::Number(index as f64)),
        "task_board_stage_conflict",
        "New writes must append to the unfinished operation in order.",
    )?;
    let proposed = prepare().await?;
    let hash = hash(&proposed)?;
    let name = text(format!("Prepared {index} {}", &hash[7..]));
    let payload = match operations
        .store
        .write_graph(
            key,
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
            let found = operations.store.named_graph(key, &name, Some(id)).await?;
            require(
                match &found {
                    Some(found) => {
                        f(&f(found, "revision")?, "contentHash")?.eq(&text(hash.clone()))
                    }
                    None => false,
                },
                "task_board_evidence_mismatch",
                "Prepared payload identity has different saved bytes.",
            )?;
            f(&found.unwrap(), "pin")?
        }
        Err(error) => return Err(error),
    };
    let entry = obj([
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
            operations.store.contract_version_graph(key)?,
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
    let saved = amend_graph(operations, id, |operation| {
        let writes = f(operation, "writes")?;
        let winner = at(&writes, index)?;
        if v::present(&(winner)) {
            agrees(&winner)?;
            return Ok(None);
        }
        require(
            !finished(operation)? && v::length(&writes)?.eq(&Value::Number(index as f64)),
            "task_board_stage_conflict",
            "The operation cannot accept this new write.",
        )?;
        let mut writes = elements(&writes)?;
        writes.push(entry.clone());
        Ok(Some(fields(
            operation,
            [
                ("phase", text("preparing")),
                ("writes", array(writes)),
                ("error", Value::Null),
            ],
        )))
    })
    .await?;
    Ok(at(&f(&f(&saved, "value")?, "writes")?, index)?)
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
    let current_value = f(&current, "value")?;
    let entry = at(&f(&current_value, "writes")?, index)?;
    require(
        v::present(&(entry)),
        "task_board_stage_conflict",
        "No write has been prepared at this stage.",
    )?;
    let outcome = f(&entry, "outcome")?;
    if v::present(&(outcome)) {
        return Ok(outcome);
    }
    require(
        !finished(&current_value)?,
        "task_board_stage_conflict",
        "A completed/failed operation cannot issue another prepared write.",
    )?;
    let key = v::string(&(f(&entry, "contractKey")?))?.to_utf8()?;
    let reference = f(&entry, "payload")?;
    let payload = operations
        .store
        .read_graph(&key, &f(&reference, "object")?, false, false, true)
        .await?;
    require(
        f(&f(&payload, "metadata")?, "parentId")?.eq(id)
            && f(&f(&payload, "revision")?, "contentHash")?.eq(&f(&reference, "contentHash")?)
            && f(&reference, "mediaType")?.eq(&text("application/json")),
        "task_board_evidence_mismatch",
        "The staged payload must be owned by this operation and match its immutable content hash.",
    )?;
    let result = operations
        .store
        .write_prepared_graph(
            &key,
            &f(&payload, "value")?,
            &f(&entry, "mutationId")?,
            &f(&entry, "destination")?,
            &f(&entry, "contractVersion")?,
        )
        .await?;
    amend_graph(operations, id, |operation| {
        let writes = f(operation, "writes")?;
        let latest = at(&writes, index)?;
        require(
            v::present(&(latest))
                && same(
                    &fields(&latest, [("outcome", Value::Null)]),
                    &fields(&entry, [("outcome", Value::Null)]),
                )?,
            "task_board_stage_conflict",
            "The applied stage no longer has its original saved intent.",
        )?;
        let outcome = f(&latest, "outcome")?;
        if v::present(&(outcome)) {
            require(
                same(&outcome, &result)?,
                "task_board_evidence_mismatch",
                "The retained write receipt differs from Hive replay.",
            )?;
            return Ok(None);
        }
        let mut writes = elements(&writes)?;
        writes[index] = fields(&latest, [("outcome", result.clone())]);
        Ok(Some(fields(
            operation,
            [
                ("phase", text("publishing")),
                ("writes", array(writes)),
                ("error", Value::Null),
            ],
        )))
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
            f(outcome, "operationId")?.eq(&f(operation, "operationId")?),
            "task_board_identity_conflict",
            "An outcome must retain its original operation ID.",
        )?;
        if eq(operation, "phase", "succeeded")? {
            require(
                same(&f(operation, "outcome")?, outcome)?,
                "mutation_conflict",
                "The completed action has another original outcome.",
            )?;
            return Ok(None);
        }
        let all_done = if eq(operation, "phase", "failed")? {
            false
        } else {
            let mut done = true;
            for write in v::items(&f(operation, "writes")?)? {
                if matches!(f(&write, "outcome")?, Value::Null) {
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
        Ok(Some(fields(
            operation,
            [
                ("phase", text("succeeded")),
                ("outcome", outcome.clone()),
                ("error", Value::Null),
            ],
        )))
    })
    .await?;
    let value = f(&done, "value")?;
    operations
        .store
        .complete_local_workflow_graph(id, &f(&value, "updatedAt")?)?;
    Ok(f(&value, "outcome")?)
}

pub(crate) async fn failure_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
    error: &IvyError,
    needs_attention: bool,
) -> Result<Value, E> {
    let failed = amend_graph(operations, id, |operation| {
        if finished(operation)? {
            return Ok(None);
        }
        Ok(Some(fields(
            operation,
            [
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
            ],
        )))
    })
    .await?;
    let value = f(&failed, "value")?;
    if eq(&value, "phase", "failed")? {
        let at = f(&value, "updatedAt")?;
        operations.store.complete_local_workflow_graph(id, &at)?;
        if eq(&f(&value, "actor")?, "source", "scheduler")? {
            let request = f(&value, "request")?;
            if (eq(&request, "action", "start")? || eq(&request, "action", "continue")?)
                && v::has(&request, "intent")?
            {
                let intent = f(&f(&request, "intent")?, "objectId")?;
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

fn find_call(
    calls: &Value,
    mut key: impl FnMut() -> std::result::Result<Value, IvyError>,
) -> std::result::Result<Value, IvyError> {
    for call in v::items(calls)? {
        if f(&call, "key")?.eq(&key()?) {
            return Ok(call);
        }
    }
    Ok(Value::Null)
}
pub(crate) async fn prepare_native_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
    proposed: &Value,
) -> Result<Value, E> {
    let saved = amend_graph(operations, id, |operation| {
        let calls = f(operation, "nativeCalls")?;
        let existing = if v::nullish(&calls) {
            Value::Null
        } else {
            find_call(&calls, || f(proposed, "key"))?
        };
        if v::present(&(existing)) {
            require(
                same(&fields(&existing, [("observation", Value::Null)]), proposed)?,
                "task_board_native_intent_mismatch",
                "The native lifecycle stage already has another retained request.",
            )?;
            return Ok(None);
        }
        require(
            !finished(operation)?,
            "task_board_operation_failed",
            "A finished action cannot prepare native work.",
        )?;
        let mut calls = if v::nullish(&calls) {
            Vec::new()
        } else {
            elements(&calls)?
        };
        calls.push(proposed.clone());
        Ok(Some(fields(operation, [("nativeCalls", array(calls))])))
    })
    .await?;
    Ok(find_call(&f(&f(&saved, "value")?, "nativeCalls")?, || {
        f(proposed, "key")
    })?)
}
pub(crate) async fn observe_native_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
    key: &Value,
    observation: &Value,
) -> Result<Value, E> {
    let saved = amend_graph(operations, id, |operation| {
        let saved_calls = f(operation, "nativeCalls")?;
        let mut calls = if v::nullish(&saved_calls) {
            Vec::new()
        } else {
            elements(&saved_calls)?
        };
        let mut index = None;
        for (n, call) in calls.iter().enumerate() {
            if f(call, "key")?.eq(key) {
                index = Some(n);
                break;
            }
        }
        require(
            match index {
                Some(index) => f(&calls[index], "operationId")?.eq(&f(observation, "operationId")?),
                None => false,
            },
            "task_board_native_intent_mismatch",
            "The observation must identify the retained native lifecycle call.",
        )?;
        let index = index.unwrap();
        if same(&f(&calls[index], "observation")?, observation)? {
            return Ok(None);
        }
        calls[index] = fields(&calls[index], [("observation", observation.clone())]);
        Ok(Some(fields(operation, [("nativeCalls", array(calls))])))
    })
    .await?;
    Ok(find_call(&f(&f(&saved, "value")?, "nativeCalls")?, || {
        Ok(key.clone())
    })?)
}
pub(crate) async fn archive_identity_graph<E, S: LocalMutationStore<E>, C: Fn() -> String>(
    operations: &Operations<S, C>,
    id: &Value,
) -> Result<Value, E> {
    let current = operations
        .store
        .read_graph(OPERATION, id, true, false, false)
        .await?;
    let existing = f(&f(&current, "value")?, "archiveMutationId")?;
    if v::present(&(existing)) {
        return Ok(existing);
    }
    let identity = operations
        .store
        .prepared_mutation_id_graph(&mutation(id, text("archive-membership"))?)
        .await?;
    let saved = amend_graph(operations, id, |operation| {
        Ok(if v::present(&(f(operation, "archiveMutationId")?)) {
            None
        } else {
            Some(fields(operation, [("archiveMutationId", identity.clone())]))
        })
    })
    .await?;
    Ok(f(&f(&saved, "value")?, "archiveMutationId")?)
}
