//! Atomic local execution transitions, independent of Hive connectivity.
use crate::{
    model::{self, RecordTask, Run},
    policy,
    store::CollectorStore,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult};

pub struct ExecutionCache<'a> {
    pub store: &'a CollectorStore,
}
impl ExecutionCache<'_> {
    pub fn task(&self, id: &str) -> IvyResult<RecordTask> {
        self.store
            .get(id, false)?
            .ok_or_else(|| IvyError::new("not_found", "Collector task not found."))
    }
    pub fn unconfirmed(&self, row: &RecordTask) -> IvyResult<bool> {
        let Some(id) = model::optional_sqlite_key(row, "lastRunId") else {
            return Ok(false);
        };
        Ok(self.store.run(&id)?.is_some_and(|r| {
            model::optional_text(&r, "error").as_deref() == Some("outcome_unknown")
        }))
    }
    pub fn require_stopped(&self, row: &RecordTask) -> IvyResult<()> {
        require(
            !self.unconfirmed(row)?,
            "outcome_unknown",
            "Task termination is unconfirmed. Stop and verify its workers on the host before disabling with confirmedStoppedRunId.",
        )
    }
    /// Admission occurs inside mutate/transaction, before acknowledging a run.
    /// Publishing rows deliberately do not prevent another execution.
    pub fn enqueue(
        &self,
        row: RecordTask,
        id: &str,
        input: IvyJson,
        now_ms: f64,
    ) -> IvyResult<()> {
        self.enqueue_json(row, id.into(), input, now_ms)
    }
    pub fn enqueue_json(&self, mut row: RecordTask, id: IvyJson, input: IvyJson, now_ms: f64) -> IvyResult<()> {
        self.require_stopped(&row)?;
        require(
            !self.store.used_run_id(&model::sqlite_key(&id)?)?,
            "operation_conflict",
            "Run ID was already used for another collection.",
        )?;
        row.insert("lastRunId", id.clone());
        row.insert(
            "nextAt",
            IvyJson::Number(
                now_ms + model::number(model::field(&row, "task")?, "intervalSeconds")? * 1000.0,
            ),
        );
        self.store.save(&row)?;
        let mut run = IvyJson::object([
            ("id", id),
            (
                "taskId",
                model::field(model::field(&row, "task")?, "id")?.clone(),
            ),
            ("task", model::field(&row, "task")?.clone()),
            ("status", "queued".into()),
            ("input", input),
            ("output", IvyJson::Null),
            ("startedAt", IvyJson::Null),
            ("finishedAt", IvyJson::Null),
            ("eventIndex", IvyJson::Number(0.0)),
            ("error", IvyJson::Null),
        ]);
        if let Some(definition) = row.get("definition") {
            run.insert("definition", definition.clone());
        }
        self.store.save_run(&run)
    }
    pub fn admit(
        &self,
        task_id: &str,
        run_id: &str,
        request: &str,
        input: IvyJson,
        now_ms: i64,
    ) -> IvyResult<IvyJson> {
        self.store.mutate(run_id, request, now_ms, |_| {
            let row = self.task(task_id)?;
            require(
                !self.store.executing(task_id)?,
                "task_busy",
                "Task already has an active or queued run.",
            )?;
            self.enqueue(row, run_id, input, now_ms as f64)?;
            Ok(IvyJson::object([
                ("runId", run_id.into()),
                ("status", "queued".into()),
            ]))
        })
    }
    /// Caller supplies a fresh UUID for each due task. Missed intervals do not
    /// produce catch-up bursts; enqueue advances from the current time.
    pub fn schedule(&self, now_ms: f64, mut fresh_id: impl FnMut() -> String) -> IvyResult<usize> {
        let mut queued = 0;
        for row in self.store.list(false)? {
            let task = model::field(&row, "task")?;
            let id = model::text(task, "id")?;
            if model::truth(task, "enabled")
                && model::number(task, "intervalSeconds")? > 0.0
                && model::number(&row, "nextAt")? <= now_ms
                && !self.unconfirmed(&row)?
                && !self.store.executing(&id)?
            {
                self.store
                    .transaction(|_| self.enqueue(row, &fresh_id(), IvyJson::Null, now_ms))?;
                queued += 1;
            }
        }
        Ok(queued)
    }
    pub fn mark_running(&self, run_id: &str, now_iso: &str) -> IvyResult<Run> {
        let mut run = self
            .store
            .run(run_id)?
            .ok_or_else(|| IvyError::new("not_found", "Collector run not found."))?;
        require(
            model::text(&run, "status")? == "queued",
            "task_busy",
            "Collector run is no longer queued.",
        )?;
        run.insert("status", "running".into());
        run.insert("startedAt", now_iso.into());
        self.store.save_run(&run)?;
        Ok(run)
    }
    pub fn register_job(&self, run: &mut Run, job_name: &str) -> IvyResult<()> {
        run.insert("workerJobName", job_name.into());
        self.store.save_run(run)
    }
    pub fn commit_output(
        &self,
        run: &mut Run,
        output: IvyJson,
        now_iso: &str,
        now_ms: f64,
        maximum_retention: &IvyJson,
    ) -> IvyResult<()> {
        run.insert("output", output);
        run.insert("finishedAt", now_iso.into());
        policy::validate_publication(run, maximum_retention)?;
        run.insert("status", "publishing".into());
        self.commit_execution(run, now_ms)
    }
    pub fn commit_execution(&self, run: &mut Run, now_ms: f64) -> IvyResult<()> {
        if model::truth(run, "executionCommitted") {
            return Ok(());
        }
        let mut row = self.task(&model::text(run, "taskId")?)?;
        policy::commit_execution(&mut row, run, now_ms)?;
        self.store.transaction(|s| {
            s.save(&row)?;
            s.save_run(run)
        })
    }
    pub fn fail(&self, run: &mut Run, code: &str, cancelled: bool, now_iso: &str) -> IvyResult<()> {
        run.insert(
            "status",
            if cancelled { "cancelled" } else { "failed" }.into(),
        );
        run.insert("finishedAt", now_iso.into());
        run.insert(
            "error",
            if code == "outcome_unknown" {
                "outcome_unknown"
            } else if cancelled {
                "Task was cancelled."
            } else {
                code
            }
            .into(),
        );
        self.store.save_run(run)?;
        if code == "outcome_unknown"
            && model::optional_text(run, "workerJobName").is_none_or(|s| s.is_empty())
        {
            if let Some(mut row) = self.store.get(&model::text(run, "taskId")?, false)? {
                let mut task = model::field(&row, "task")?.clone();
                task.insert("enabled", false.into());
                row.insert("task", task);
                row.insert(
                    "revision",
                    IvyJson::Number(model::number(&row, "revision")? + 1.0),
                );
                self.store.save(&row)?;
            }
        }
        Ok(())
    }
    /// Called only after the shared process owner proves this exact job absent.
    /// Re-read the row so a stale observation cannot release a newer run fence.
    pub fn verified_stopped(
        &self,
        task_id: &str,
        run_id: &str,
        job_name: &str,
        now_ms: f64,
    ) -> IvyResult<bool> {
        let Some(mut current) = self.store.get(task_id, false)? else {
            return Ok(false);
        };
        if model::optional_sqlite_key(&current, "lastRunId").as_deref() != Some(run_id)
            || !self.unconfirmed(&current)?
        {
            return Ok(false);
        }
        let Some(mut run) = self.store.run(run_id)? else {
            return Ok(false);
        };
        if model::optional_text(&run, "workerJobName").as_deref() != Some(job_name) {
            return Ok(false);
        }
        self.store.transaction(|s| {
            run.insert(
                "error",
                "Worker termination verified; execution outcome remains unknown.".into(),
            );
            s.save_run(&run)?;
            current.insert(
                "nextAt",
                IvyJson::Number(
                    now_ms
                        + model::number(model::field(&current, "task")?, "intervalSeconds")?
                            * 1000.0,
                ),
            );
            s.save(&current)
        })?;
        Ok(true)
    }
    pub fn visible(&self, row: &RecordTask, maximum_retention: &IvyJson) -> IvyResult<IvyJson> {
        let IvyJson::Object(pairs) = row else {
            return Err(IvyError::new(
                "invalid_arguments",
                "Invalid Collector task.",
            ));
        };
        let mut visible = IvyJson::object(
            pairs
                .iter()
                .filter(|(key, _)| {
                    !["state", "deleted", "lastRunId"]
                        .iter()
                        .any(|s| key == &(*s).into())
                })
                .cloned(),
        );
        let run = if let Some(id) = model::optional_sqlite_key(row, "lastRunId") {
            self.store.run(&id)?
        } else {
            None
        };
        visible.insert(
            "lastRunId",
            row.get("lastRunId").cloned().unwrap_or(IvyJson::Null),
        );
        visible.insert(
            "status",
            run.as_ref()
                .and_then(|r| r.get("status"))
                .cloned()
                .unwrap_or("idle".into()),
        );
        visible.insert(
            "error",
            run.as_ref()
                .and_then(|r| r.get("error"))
                .cloned()
                .unwrap_or(IvyJson::Null),
        );
        visible.insert(
            "lastRunAt",
            run.as_ref()
                .and_then(|r| r.get("startedAt"))
                .cloned()
                .unwrap_or(IvyJson::Null),
        );
        visible.insert(
            "effectiveRetention",
            policy::effective_retention(
                model::field(model::field(row, "task")?, "retention")?,
                maximum_retention,
            )?,
        );
        Ok(visible)
    }
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
