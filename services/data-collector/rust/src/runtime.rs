//! Local runtime control. Database transitions are synchronous; remote reads and
//! cancellation happen after the store command completes.
use crate::{cache::ExecutionCache, model, schema, store::CollectorStore};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::canonical};

pub enum Read {
    Local(IvyJson),
    Remote {
        method: &'static str,
        params: IvyJson,
    },
}
pub struct Updated {
    pub value: IvyJson,
    /// Only a newly committed disable requests cancellation. Receipt replay must
    /// not cancel a later execution that happens to use the same task ID.
    pub disabled: Option<String>,
}
pub struct Context<'a> {
    pub caller: &'a JsString,
    pub operation_id: Option<&'a JsString>,
}
pub struct Runtime<'a> {
    pub store: &'a CollectorStore,
    pub settings: &'a IvyJson,
    pub root_id: &'a str,
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn nullable(value: Option<&IvyJson>) -> IvyJson {
    value.cloned().unwrap_or(IvyJson::Null)
}
fn select(value: &IvyJson, keys: &[&str]) -> IvyJson {
    IvyJson::object(
        keys.iter()
            .filter_map(|key| value.get(key).map(|v| (*key, v.clone()))),
    )
}
impl Runtime<'_> {
    pub fn read(&self, args: &IvyJson) -> IvyResult<Read> {
        let input = schema::parse_schema("runtimeReadSchema", args)?;
        let cache = ExecutionCache { store: self.store };
        let maximum = model::field(self.settings, "retention")?;
        let view = model::text(&input, "view")?;
        if view == "tasks" {
            let mut items = Vec::new();
            for row in self.store.list(false)? {
                let mut visible = cache.visible(&row, maximum)?;
                visible.insert(
                    "task",
                    select(model::field(&row, "task")?, &["id", "name", "enabled"]),
                );
                items.push(visible);
            }
            return Ok(Read::Local(IvyJson::object([
                ("rootObjectId", self.root_id.into()),
                (
                    "limits",
                    select(
                        self.settings,
                        &[
                            "retention",
                            "concurrency",
                            "maximumTasks",
                            "maximumMemoryMb",
                            "maximumTimeoutSeconds",
                        ],
                    ),
                ),
                ("items", IvyJson::Array(items)),
            ])));
        }
        if view == "operation" {
            let id = model::sqlite_key(model::field(&input, "operationId")?)?;
            let run = self.store.run(&id)?;
            return Ok(Read::Local(IvyJson::object([
                (
                    "operation",
                    self.store.operation(&id)?.unwrap_or(IvyJson::Null),
                ),
                (
                    "run",
                    run.as_ref()
                        .map(|r| {
                            select(
                                r,
                                &["id", "taskId", "status", "startedAt", "finishedAt", "error"],
                            )
                        })
                        .unwrap_or(IvyJson::Null),
                ),
            ])));
        }
        let row = cache.task(&model::text(&input, "id")?)?;
        if view == "task" {
            return Ok(Read::Local(cache.visible(&row, maximum)?));
        }
        let Some(result) = row
            .get("resultObjectId")
            .filter(|v| !v.is_null() && v.as_js_string().is_some_and(|s| !s.0.is_empty()))
        else {
            return Ok(Read::Local(IvyJson::Null));
        };
        let mut params = IvyJson::object([("objectId", result.clone())]);
        if view == "history" {
            params.insert("limit", IvyJson::Number(50.0));
            if let Some(cursor) = input
                .get("cursor")
                .filter(|v| v.as_js_string().is_some_and(|s| !s.0.is_empty()))
            {
                params.insert("cursor", cursor.clone());
            }
            Ok(Read::Remote {
                method: "objects.history",
                params,
            })
        } else {
            if let Some(revision) = input.get("revision") {
                params.insert("revision", revision.clone());
            }
            Ok(Read::Remote {
                method: "objects.read",
                params,
            })
        }
    }

    /// Run on the store's blocking owner. Authentication deletion is synchronous
    /// and executes inside the original mutation, before saving the tombstone.
    pub fn update(
        &self,
        args: &IvyJson,
        context: Option<Context<'_>>,
        now_ms: i64,
        now_iso: &str,
        active: impl Fn(&str) -> bool,
        mut clear_authentication: impl FnMut(&str) -> IvyResult<()>,
    ) -> IvyResult<Updated> {
        let input = schema::parse_schema("runtimeUpdateSchema", args)?;
        let operation = model::field(&input, "operationId")?;
        require(
            context
                .as_ref()
                .and_then(|c| c.operation_id)
                .is_none_or(|id| id.0.is_empty() || operation.as_js_string() == Some(id)),
            "operation_conflict",
            "Routed and inner operation IDs must match.",
        )?;
        let caller = context
            .map(|c| IvyJson::String(c.caller.clone()))
            .unwrap_or("local".into());
        let key = canonical(
            &IvyJson::object([("caller", caller), ("input", input.clone())]),
            ivy_contracts::limits::MANAGEMENT_FRAME_BYTES,
        )?;
        let operation_key = model::sqlite_key(operation)?;
        let action = model::text(&input, "action")?;
        let mut disabled = None;
        let cache = ExecutionCache { store: self.store };
        let maximum = model::field(self.settings, "retention")?;
        let value = self.store.mutate(&operation_key, &key, now_ms, |_| {
            if action == "save" {
                let task = model::field(&input, "task")?;
                let id = model::text(task, "id")?;
                let saved = self.store.get(&id, true)?;
                if let Some(row) = &saved { cache.require_stopped(row)?; }
                let previous = saved.as_ref().filter(|row| !model::truth(row, "deleted"));
                require(previous.map(|r| model::number(r, "revision")).transpose()?.unwrap_or(0.0) == model::number(&input, "expectedRevision")?, "revision_conflict", "Task changed; reload before saving.")?;
                require(!self.store.busy(&id)?, "task_busy", "Wait for the current run before changing a task.")?;
                require(previous.is_some() || (self.store.list(false)?.len() as f64) < model::number(self.settings, "maximumTasks")?, "limit_exceeded", "Maximum task count reached.")?;
                let mut row = IvyJson::object([
                    ("task", task.clone()),
                    ("revision", IvyJson::Number(saved.as_ref().map(|r| model::number(r, "revision")).transpose()?.unwrap_or(0.0) + 1.0)),
                    ("resultObjectId", nullable(saved.as_ref().and_then(|r| r.get("resultObjectId")))),
                    ("nextAt", IvyJson::Number(now_ms as f64 + model::number(task, "intervalSeconds")? * 1000.0)),
                    ("state", nullable(previous.and_then(|r| r.get("state")))),
                    ("deleted", false.into()),
                    ("lastRunId", nullable(previous.and_then(|r| r.get("lastRunId")))),
                    ("retentionWarning", IvyJson::Null),
                ]);
                for key in ["definition", "resultRootId"] { if let Some(value) = saved.as_ref().and_then(|r| r.get(key)).filter(|v| !v.is_null()) { row.insert(key, value.clone()); } }
                self.store.save(&row)?;
                return cache.visible(&row, maximum);
            }
            let id = model::text(&input, "id")?;
            let mut row = cache.task(&id)?;
            if action != "disable" { cache.require_stopped(&row)?; }
            if action == "run" {
                require(!self.store.executing(&id)?, "task_busy", "Task already has an active or queued run.")?;
                cache.enqueue_json(row, operation.clone(), nullable(input.get("input")), now_ms as f64)?;
                return Ok(IvyJson::object([("runId", operation.clone()), ("status", "queued".into())]));
            }
            require(model::number(&row, "revision")? == model::number(&input, "expectedRevision")?, "revision_conflict", "Task changed; reload before updating.")?;
            let mut task = model::field(&row, "task")?.clone();
            if action == "delete" {
                require(!self.store.busy(&id)?, "task_busy", "Disable and wait for the task before deleting.")?;
                clear_authentication(&id)?;
                row.insert("deleted", true.into());
                task.insert("enabled", false.into()); task.insert("script", "".into());
                task.insert("config", model::object()); task.insert("dependencies", model::object());
                task.insert("secretNames", IvyJson::Array(Vec::new())); task.insert("inputSecretName", IvyJson::Null);
                row.insert("state", IvyJson::Null); row.insert("lastRunId", IvyJson::Null);
            } else {
                if action == "disable" {
                    if let Some(confirmed) = input.get("confirmedStoppedRunId") {
                        let run = self.store.run(&model::sqlite_key(confirmed)?)?;
                        require(run.as_ref().is_some_and(|r| row.get("lastRunId") == r.get("id") && r.get("taskId") == input.get("id") && r.get("error") == Some(&"outcome_unknown".into())), "operation_conflict", "Confirm the current run whose worker termination is unconfirmed.")?;
                        require(!self.store.busy(&id)? && !active(&id), "task_busy", "Wait for the active run before confirming worker termination.")?;
                        let mut run = run.unwrap();
                        run.insert("error", "Operator confirmed worker termination; execution outcome remains unknown.".into());
                        self.store.save_run(&run)?;
                    }
                }
                task.insert("enabled", (action == "enable").into());
                row.insert("nextAt", IvyJson::Number(now_ms as f64 + model::number(&task, "intervalSeconds")? * 1000.0));
            }
            row.insert("task", task);
            row.insert("revision", IvyJson::Number(model::number(&row, "revision")? + 1.0));
            self.store.save(&row)?;
            if action == "delete" { return Ok(IvyJson::object([("deleted", true.into()), ("resultObjectId", nullable(row.get("resultObjectId")))])); }
            if action == "disable" {
                disabled = Some(id.clone());
                for mut run in self.store.runs("queued")? { if model::text(&run, "taskId")? == id {
                    run.insert("status", "cancelled".into()); run.insert("finishedAt", now_iso.into());
                    run.insert("error", "Task disabled before execution.".into()); self.store.save_run(&run)?;
                } }
            }
            cache.visible(&row, maximum)
        })?;
        Ok(Updated { value, disabled })
    }
}
