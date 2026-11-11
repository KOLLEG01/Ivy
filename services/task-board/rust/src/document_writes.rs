//! Journal write policy over the same authoritative values, reads and SQL owner.
//! Comment/identity bindings retain their accepted producer lifetimes and drains.
#![cfg_attr(not(test), allow(dead_code))]
use super::{
    document_reads::{DocumentReads, TreeReads, sql_argument},
    json_values::{self as v, Value},
    record_upgrades::upgrade_stored_owned_with,
    store::ObjectReads,
};
use crate::journal_store::{
    Contracts,
    comment_writes::{Result, add, array, at, f, hash, iterated, number, obj, require, text},
    mutation_ports::{LocalMutationSource, WriteFailure, typed_write_cause},
    reference_writes::{task_attachment_pins, task_references},
    write_completion::complete_write,
};
use crate::operations::Document;
use ivy_contracts::{IvyError, IvyJson as Json, JsString, limits::MANAGEMENT_FRAME_BYTES};
use std::future::Future;

#[cfg(test)]
#[path = "../../../../tests/fixtures/task-board-archive-writes.rs"]
mod archive_tests;

#[cfg(test)]
#[path = "../../../../tests/fixtures/task-board-run-source-writes.rs"]
mod run_source_tests;

pub(crate) struct DocumentWrites<'a, S, H, W, I, N, G> {
    pub(crate) source: &'a S,
    pub(crate) hydrate: &'a H,
    pub(crate) comments: &'a W,
    pub(crate) identity: &'a I,
    pub(crate) now: &'a N,
    pub(crate) new_id: &'a G,
}

fn root<R, C, P, F, M>(reads: &DocumentReads<'_, R, C, P, F, M>) -> Value {
    reads
        .local
        .journal
        .root_object_id
        .clone()
        .map(text)
        .unwrap_or(Value::Null)
}
fn code<E>(error: &WriteFailure<E>, codes: &[&str]) -> bool {
    typed_write_cause(error).is_some_and(|error| codes.iter().any(|code| error.code == *code))
}
fn gte(left: &Value, right: &Value) -> std::result::Result<bool, IvyError> {
    match (left, right) {
        (Value::String(a), Value::String(b)) => Ok(a >= b),
        (Value::Number(a), Value::Number(b)) => Ok(a >= b),
        _ => Err(v::invalid("Expected two timestamps or two numbers.")),
    }
}

pub(crate) async fn named_graph<R, C, Parse, SqlFailure, M, S, E>(
    reads: &DocumentReads<'_, R, C, Parse, SqlFailure, M>,
    source: &S,
    key: &str,
    name: &Value,
    parent: Option<&Value>,
) -> Result<Option<Document>, E>
where
    R: ObjectReads<WriteFailure<E>>,
    C: Contracts,
    M: TreeReads<WriteFailure<E>>,
    S: LocalMutationSource<E>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> WriteFailure<E>,
{
    let parent = parent.cloned().unwrap_or_else(|| root(reads));
    let local = reads.local;
    if local.contracts.local(key)?
        || (!matches!(parent, Value::Null)
            && local
                .key::<WriteFailure<E>>(sql_argument(&parent)?.as_ref())?
                .as_deref()
                == Some("task-board/operation"))
    {
        let key_arg: Json = key.into();
        let name_arg = sql_argument(name)?;
        let parent_arg = sql_argument(&parent)?;
        let row = if matches!(parent, Value::Null) {
            local.row::<WriteFailure<E>>(
                "SELECT id FROM documents WHERE key=? AND name=? AND parent_id IS NULL",
                &[Some(&key_arg), name_arg.as_ref()],
            )?
        } else {
            local.row::<WriteFailure<E>>(
                "SELECT id FROM documents WHERE key=? AND name=? AND parent_id=?",
                &[Some(&key_arg), name_arg.as_ref(), parent_arg.as_ref()],
            )?
        };
        return row
            .map(|row| {
                local
                    .record::<WriteFailure<E>>(
                        key,
                        &row[0].string()?.into(),
                        parent.eq(&root(reads)),
                    )
                    .and_then(|value| Ok(value.into_document()?))
            })
            .transpose();
    }
    let page = source
        .request_owned(
            "objects.query",
            Some(obj([
                ("contractKey", text(key)),
                (
                    "where",
                    obj([
                        ("op", text("and")),
                        (
                            "args",
                            array([
                                if matches!(parent, Value::Null) {
                                    obj([
                                        ("op", text("isNull")),
                                        ("field", text("object.parentId")),
                                    ])
                                } else {
                                    obj([
                                        ("op", text("eq")),
                                        ("field", text("object.parentId")),
                                        ("value", parent.clone()),
                                    ])
                                },
                                obj([
                                    ("op", text("eq")),
                                    ("field", text("object.name")),
                                    ("value", name.clone()),
                                ]),
                            ]),
                        ),
                    ]),
                ),
                ("limit", Value::Number(2.0)),
            ])),
        )
        .await?;
    require(
        number(&v::length(f(&page, "items"))?)? <= 1.0 && !v::present(&(f(&page, "nextCursor"))),
        "task_board_identity_conflict",
        "A durable workflow identity must select exactly one record.",
    )?;
    let first = at(f(&page, "items"), 0)?;
    if v::present(&(first)) {
        reads
            .read_graph(
                key,
                f(&first, "objectId"),
                parent.eq(&root(reads)),
                false,
                false,
            )
            .await
            .map(Some)
    } else {
        Ok(None)
    }
}

async fn recovery_reference<R, C, Parse, SqlFailure, M, E>(
    reads: &DocumentReads<'_, R, C, Parse, SqlFailure, M>,
    pin: &Value,
) -> Result<Option<Value>, E>
where
    R: ObjectReads<WriteFailure<E>>,
    C: Contracts,
    M: TreeReads<WriteFailure<E>>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> WriteFailure<E>,
{
    let retained = async {
        match reads.read_graph("task-board/run", pin, true, false, false).await {
            Ok(document) => Ok(document),
            Err(error) if code(&error, &["not_found", "revision_pruned"]) => {
                let current = reads.read_graph("task-board/run", f(pin, "objectId"), true, false, false).await?;
                let agrees = if gte(&Json::Number(current.pin.revision), f(pin, "revision"))? {
                    let phase = f(&current.value, "phase");
                    if ["completed", "failed"].iter().any(|item| phase.eq(&text(*item))) {
                        let state = f(f(&current.value, "externalOutcome"), "state");
                        ["succeeded", "failed"].iter().any(|item| state.eq(&text(*item)))
                    } else { false }
                } else { false };
                require(agrees,
                    "task_board_recovery_evidence_missing", "A pruned recovery revision needs the same original Run with a confirmed terminal owner outcome.")?;
                Ok(current)
            }
            Err(error) => Err(error),
        }
    }.await;
    match retained {
        Ok(document) => Ok(Some(document.pin.to_json())),
        Err(error)
            if code(
                &error,
                &[
                    "not_found",
                    "revision_pruned",
                    "task_board_recovery_evidence_missing",
                ],
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

impl<S, H, W, I, N, G> DocumentWrites<'_, S, H, W, I, N, G> {
    pub(crate) async fn write_graph<R, C, Parse, SqlFailure, M, E, HFut, WFut, IFut>(
        &self,
        reads: &DocumentReads<'_, R, C, Parse, SqlFailure, M>,
        key: &str,
        value: &Value,
        mutation: &Value,
        destination: &Value,
        retained: bool,
        selected_version: &Value,
        selected_references: Option<&Value>,
    ) -> Result<Value, E>
    where
        R: ObjectReads<WriteFailure<E>>,
        C: Contracts,
        M: TreeReads<WriteFailure<E>>,
        S: LocalMutationSource<E>,
        Parse: Fn(&str) -> Result<Json, E>,
        SqlFailure: Fn(ivy_store::rusqlite::Error) -> WriteFailure<E>,
        H: Fn(Value, Value) -> HFut,
        HFut: Future<Output = Result<Value, E>>,
        W: Fn(Value, Value, Value, Value) -> WFut,
        WFut: Future<Output = Result<Value, E>>,
        I: Fn(Value) -> IFut,
        IFut: Future<Output = Result<Value, E>>,
        N: Fn() -> String,
        G: Fn() -> JsString,
    {
        // Parameter-default evaluation precedes even the first `in` expression.
        let version = if matches!(selected_version, Value::Null) {
            text(reads.local.contracts.version(key)?)
        } else {
            selected_version.clone()
        };
        let mut value = value.clone();
        if key == "task-board/task"
            && selected_references.is_none()
            && !v::has(destination, "create")
            && v::has(&value, "commentStore")
        {
            value = (self.hydrate)(value, f(destination, "objectId").clone()).await?;
        }
        if !retained {
            value = upgrade_stored_owned_with(key, value, |params| {
                self.source.request_owned("objects.read", params)
            })
            .await?;
        }
        let local = reads.local;
        if local.contracts.local(key)?
            || (v::has(destination, "create")
                && !matches!(f(f(destination, "create"), "parentId"), Value::Null)
                && local
                    .key::<WriteFailure<E>>(
                        sql_argument(f(f(destination, "create"), "parentId"))?.as_ref(),
                    )?
                    .as_deref()
                    == Some("task-board/operation"))
        {
            let encoded: Json =
                ivy_contracts::json::canonical(&(value), MANAGEMENT_FRAME_BYTES)?.into();
            let now: Json = (self.now)().into();
            let exec = |sql: &str| {
                local
                    .journal
                    .connection
                    .execute_batch(sql)
                    .map_err(&local.sqlite_failure)
            };
            if v::has(destination, "create") {
                if let Some(existing) = named_graph(
                    reads,
                    self.source,
                    key,
                    f(f(destination, "create"), "name"),
                    destination.get("create").and_then(|v| v.get("parentId")),
                )
                .await?
                {
                    require(
                        hash(&existing.value)? == hash(&value)?,
                        "revision_conflict",
                        "The local TaskBoard identity differs.",
                    )?;
                    return Ok(existing.pin.to_json());
                }
                let id = text((self.new_id)());
                exec("BEGIN IMMEDIATE")?;
                let written = (|| -> Result<(), E> {
                    local.run::<WriteFailure<E>>(
                        "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                        &[
                            sql_argument(&id)?.as_ref(),
                            Some(&key.into()),
                            sql_argument(f(f(destination, "create"), "name"))?.as_ref(),
                            sql_argument(f(f(destination, "create"), "parentId"))?.as_ref(),
                            Some(&Json::Number(1.0)),
                            Some(&now),
                            Some(&now),
                        ],
                    )?;
                    local.run::<WriteFailure<E>>(
                        "INSERT INTO revisions VALUES(?,?,?,?)",
                        &[
                            sql_argument(&id)?.as_ref(),
                            Some(&Json::Number(1.0)),
                            Some(&encoded),
                            Some(&now),
                        ],
                    )?;
                    if key == "task-board/coordination"
                        && v::present(&(f(&value, "activeOperation")))
                    {
                        local.run::<WriteFailure<E>>(
                            "INSERT INTO active_gates VALUES(?,?)",
                            &[
                                sql_argument(&id)?.as_ref(),
                                sql_argument(f(f(&value, "activeOperation"), "objectId"))?.as_ref(),
                            ],
                        )?;
                    }
                    exec("COMMIT")?;
                    Ok(())
                })();
                if let Err(error) = written {
                    exec("ROLLBACK")?;
                    if let Some(winner) = named_graph(
                        reads,
                        self.source,
                        key,
                        f(f(destination, "create"), "name"),
                        destination.get("create").and_then(|v| v.get("parentId")),
                    )
                    .await?
                    {
                        require(
                            hash(&winner.value)? == hash(&value)?,
                            "revision_conflict",
                            "The local TaskBoard identity already exists.",
                        )?;
                        return Ok(winner.pin.to_json());
                    }
                    return Err(error);
                }
                return Ok(obj([("objectId", id), ("revision", Value::Number(1.0))]));
            }
            let next = add(
                f(destination, "expectedRevision").clone(),
                Value::Number(1.0),
            )?;
            exec("BEGIN IMMEDIATE")?;
            let written = (|| -> Result<(), E> {
                let changed = local.run::<WriteFailure<E>>("UPDATE documents SET current_revision=?,updated_at=? WHERE id=? AND key=? AND current_revision=?", &[
                    sql_argument(&next)?.as_ref(), Some(&now), sql_argument(f(destination, "objectId"))?.as_ref(), Some(&key.into()), sql_argument(f(destination, "expectedRevision"))?.as_ref(),
                ])?;
                require(
                    changed == 1,
                    "revision_conflict",
                    "The local TaskBoard row changed.",
                )?;
                local.run::<WriteFailure<E>>(
                    "INSERT INTO revisions VALUES(?,?,?,?)",
                    &[
                        sql_argument(f(destination, "objectId"))?.as_ref(),
                        sql_argument(&next)?.as_ref(),
                        Some(&encoded),
                        Some(&now),
                    ],
                )?;
                if key == "task-board/coordination" {
                    let active = f(&value, "activeOperation");
                    if v::present(&(active)) {
                        local.run::<WriteFailure<E>>("INSERT INTO active_gates VALUES(?,?) ON CONFLICT(gate_id) DO UPDATE SET operation_id=excluded.operation_id", &[sql_argument(f(destination, "objectId"))?.as_ref(), sql_argument(f(&active, "objectId"))?.as_ref()])?;
                    } else {
                        local.run::<WriteFailure<E>>(
                            "DELETE FROM active_gates WHERE gate_id=?",
                            &[sql_argument(f(destination, "objectId"))?.as_ref()],
                        )?;
                    }
                }
                if [
                    "task-board/scan-cursor",
                    "task-board/coordination",
                    "task-board/native-signals",
                ]
                .contains(&key)
                {
                    local.run::<WriteFailure<E>>(
                        "DELETE FROM revisions WHERE id=? AND revision<?",
                        &[
                            sql_argument(f(destination, "objectId"))?.as_ref(),
                            sql_argument(&next)?.as_ref(),
                        ],
                    )?;
                }
                exec("COMMIT")?;
                Ok(())
            })();
            if let Err(error) = written {
                exec("ROLLBACK")?;
                return Err(error);
            }
            return Ok(obj([
                ("objectId", f(destination, "objectId").clone()),
                ("revision", next),
            ]));
        }
        let content = obj([
            ("encoding", text("json")),
            (
                "value",
                if selected_references.is_none()
                    && key == "task-board/task"
                    && !v::has(destination, "create")
                {
                    (self.comments)(
                        value.clone(),
                        f(destination, "objectId").clone(),
                        mutation.clone(),
                        version.clone(),
                    )
                    .await?
                } else {
                    value.clone()
                },
            ),
        ]);
        let mut references = Vec::new();
        if let Some(selected) = selected_references {
            references = v::fields(selected).to_vec();
        } else if key == "task-board/task" {
            let stored = f(&content, "value");
            let tree = f(&stored, "commentStore");
            if !v::nullish(&tree)
                && f(&tree, "kind").eq(&text("tree"))
                && v::present(&(f(&tree, "root")))
            {
                references.push(("conversation".into(), f(f(&tree, "root"), "object").clone()));
            }
            let result = f(&value, "latestResult");
            if v::present(&(result)) {
                references.push(("result".into(), result.clone()));
            }
            let run = f(&value, "lastRun");
            if v::present(&(run)) {
                references.push(("run".into(), run.clone()));
            }
            let release = f(&value, "contextRelease");
            if v::present(release) {
                references.push(("context-release".into(), release.clone()));
            }
            if v::present(&(f(&value, "completionIssue"))) {
                if let Some(pin) =
                    recovery_reference(reads, f(f(&value, "completionIssue"), "run")).await?
                {
                    if v::present(&(pin)) {
                        references.push(("recovery-run".into(), pin));
                    }
                }
            }
            let attachments = task_attachment_pins::<E>(&value)?;
            if v::has(destination, "create") {
                let pins = iterated(&attachments, "attachments")?;
                require(
                    pins.len() + references.len() <= 256,
                    "limit_exceeded",
                    "An initial Task exceeds its exact reference capacity.",
                )?;
                references.extend(
                    pins.iter()
                        .cloned()
                        .enumerate()
                        .map(|(index, pin)| (format!("attachment-{index}").into(), pin)),
                );
            } else {
                references = v::fields(
                    &task_references(
                        self.source,
                        self.identity,
                        f(destination, "objectId"),
                        &v::object(references),
                        &attachments,
                        crate::journal_store::reference_writes::ReferencePages::Retained,
                    )
                    .await?,
                )
                .to_vec();
            }
        } else if key == "task-board/run" {
            if let Some(pin) = crate::journal_store::reference_writes::run_source_for_write(
                &root(reads),
                &value,
                destination,
                |params| self.source.request_owned("objects.read", Some(params)),
                |error| {
                    typed_write_cause(error).is_some_and(|error| {
                        error.code == "revision_pruned"
                            && error.outcome_json() == text("not_executed")
                    })
                },
            )
            .await?
            {
                references.push(("source-task".into(), pin));
            }
        } else if key == "task-board/delivery" {
            let request = f(&value, "chatRequest");
            if v::present(&(request)) {
                references.push(("source".into(), f(&request, "source").clone()));
            }
        } else if key == "task-board/result" {
            let mut artifacts = iterated(
                f(f(&value, "content"), "artifacts"),
                "value.content.artifacts",
            )?
            .to_vec();
            let checks = f(f(&value, "content"), "checks");
            let checks = if v::nullish(&checks) {
                &[][..]
            } else {
                v::items(checks)?
            };
            for finding in checks {
                if let Some(evidence) = finding.get("evidence") {
                    artifacts.extend(v::items(evidence)?.iter().cloned());
                }
            }
            let mut remote = Vec::new();
            for artifact in artifacts {
                if local
                    .key::<WriteFailure<E>>(
                        sql_argument(f(f(&artifact, "object"), "objectId"))?.as_ref(),
                    )?
                    .is_none()
                {
                    remote.push(artifact);
                }
            }
            for (index, artifact) in remote.into_iter().take(256).enumerate() {
                references.push((
                    format!("artifact-{index}").into(),
                    f(&artifact, "object").clone(),
                ));
            }
        }
        let references = v::object(references);
        let mutation = if retained {
            mutation.clone()
        } else {
            (self.identity)(mutation.clone()).await?
        };
        let mut params = vec![
            ("mutationId".into(), mutation),
            ("contractVersion".into(), version),
            ("content".into(), content),
            ("references".into(), references),
        ];
        if v::has(destination, "create") {
            let mut create = v::fields(f(destination, "create")).to_vec();
            create.extend([
                ("ownerObjectId".into(), Value::Null),
                ("contractKey".into(), text(key)),
            ]);
            params.push(("create".into(), v::object(create)));
        } else {
            params.extend([
                ("objectId".into(), f(destination, "objectId").clone()),
                (
                    "expectedRevision".into(),
                    f(destination, "expectedRevision").clone(),
                ),
            ]);
            if key == "task-board/task" {
                params.push((
                    "expectedArchived".into(),
                    destination
                        .get("expectedArchived")
                        .cloned()
                        .unwrap_or(Value::Bool(false)),
                ));
            }
        }
        // Only this await is inside the original typed create-conflict catch.
        let result = match self
            .source
            .request_owned("objects.write", Some(v::object(params)))
            .await
        {
            Ok(result) => result,
            Err(error) => {
                if !v::has(destination, "create") || !code(&error, &["revision_conflict"]) {
                    return Err(error);
                }
                let existing = named_graph(
                    reads,
                    self.source,
                    key,
                    f(f(destination, "create"), "name"),
                    destination.get("create").and_then(|v| v.get("parentId")),
                )
                .await?;
                let agrees = if let Some(existing) = &existing {
                    existing.pin.revision == 1.0 && hash(&existing.value)? == hash(&value)?
                } else {
                    false
                };
                require(
                    agrees,
                    "revision_conflict",
                    "The durable TaskBoard identity already contains different content.",
                )?;
                return Ok(existing.unwrap().pin.to_json());
            }
        };
        let pin = complete_write(reads.cache, &result)?;
        Ok(pin)
    }
}
