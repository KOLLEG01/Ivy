use super::*;
use ivy_contracts::hash::hash_json;

impl<S: NativePublicationStore, E: NativePublicationEngine, V: NativePublicationEvidence>
    NativePublisher<'_, S, E, V>
{
    pub(super) async fn publish_adopted_turn<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
    ) -> IvyResult<Json> {
        let op = &operation.value;
        let request = f(op, "request");
        let change = f(request, "change");
        let task_id = s(request, "taskId")?;
        let at = f(op, "createdAt");
        // The exact captured revision is replayed by the ordinary write journal.
        // A fresh observation must never acquire a newer ticket implicitly.
        let task = self
            .store
            .lifecycle_read(
                "task-board/task",
                &Json::object([
                    ("objectId", Json::String(task_id.clone())),
                    ("revision", f(request, "expectedRevision").clone()),
                ]),
                false,
                false,
            )
            .await?;
        let previous = self
            .store
            .read("task-board/run", f(request, "run"), true)
            .await?;
        let value = &task.document.value;
        let primary = f(value, "primaryResourceRef");
        require(
            !primary.is_null()
                && same(primary, f(&previous.value, "primaryResourceRef"))?
                && f(&previous.value, "taskId") == f(request, "taskId")
                && finished_run(&previous.value),
            "task_board_primary_mismatch",
            "A direct turn must continue this ticket's saved primary and finished attempt.",
        )?;
        let plan = self.evidence.adoption(
            value,
            &previous.value,
            f(change, "metadata"),
            f(change, "turns"),
        )?;
        let turn = crate::native_adoption::candidate(
            value,
            &previous.value,
            f(change, "metadata"),
            f(change, "turns"),
        )?
        .ok_or_else(|| {
            IvyError::new(
                "task_board_turn_mismatch",
                "The observation must identify a new native turn.",
            )
        })?;
        if array(f(op, "writes")).is_empty() {
            require(
                f(&task.metadata, "currentRevision") == f(&task.document.pin, "revision")
                    && f(&task.metadata, "effectivelyArchived").as_bool() != Some(true)
                    && eq(f(value, "fields"), "control", "agent")
                    && f(value, "claim").is_null()
                    && f(value, "publication").is_null()
                    && same(f(value, "lastRun"), f(request, "run"))?,
                "revision_conflict",
                "The ticket changed before its direct turn could be adopted.",
            )?;
            self.engine.aggregate(value, task_id).await?;
        }
        let original_request = Json::object([
            ("action", "continue".into()),
            ("operationId", f(request, "operationId").clone()),
            ("taskId", Json::String(task_id.clone())),
            (
                "expectedRevision",
                f(&task.document.pin, "revision").clone(),
            ),
            (
                "feedback",
                "Track the turn started directly in the saved native thread.".into(),
            ),
            ("target", f(&previous.value, "target").clone()),
            ("workspace", f(&previous.value, "workspace").clone()),
            (
                "intent",
                f(f(&previous.value, "originalRequest"), "intent").clone(),
            ),
            ("commentIds", Json::Array(vec![])),
        ]);
        let request_hash = Json::from(hash_json(&original_request)?);
        let actor = f(op, "actor");
        let attempt = Json::Number(n(value, "attemptCount") + 1.0);
        let reserved = steps
            .write(
                "task-board/task",
                &patch(
                    value,
                    [("publication", steps.pin()?), ("updatedAt", at.clone())],
                ),
                &Json::object([
                    ("objectId", Json::String(task_id.clone())),
                    (
                        "expectedRevision",
                        f(&task.document.pin, "revision").clone(),
                    ),
                ]),
            )
            .await?;
        let run = steps
            .create(
                "task-board/run",
                &Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("taskId", Json::String(task_id.clone())),
                    ("attempt", attempt.clone()),
                    ("operationId", f(request, "operationId").clone()),
                    (
                        "callerPrincipalId",
                        Json::String(self.engine.principal_id().clone()),
                    ),
                    ("originalRequest", original_request.clone()),
                    ("requestHash", request_hash.clone()),
                    ("target", f(&original_request, "target").clone()),
                    ("workspace", f(&original_request, "workspace").clone()),
                    ("plan", plan),
                    ("owner", actor.clone()),
                    (
                        "executionContext",
                        Json::object([
                            ("workflowState", f(value, "workflowState").clone()),
                            ("latestResult", f(value, "latestResult").clone()),
                            ("acceptedReview", f(value, "acceptedReview").clone()),
                        ]),
                    ),
                    ("phase", "running".into()),
                    ("primaryResourceRef", primary.clone()),
                    ("turnId", f(turn, "id").clone()),
                    ("nativeEpoch", f(f(change, "metadata"), "epoch").clone()),
                    (
                        "calls",
                        Json::object([
                            ("thread", Json::Null),
                            ("turn", Json::Null),
                            ("interrupt", Json::Null),
                        ]),
                    ),
                    ("notificationCursor", Json::Number(0.0)),
                    ("result", Json::Null),
                    ("cancellation", Json::Null),
                    (
                        "externalOutcome",
                        Json::object([
                            ("state", "active".into()),
                            ("observedAt", f(f(change, "turns"), "observedAt").clone()),
                            ("evidence", Json::Null),
                            ("code", Json::Null),
                        ]),
                    ),
                    ("createdAt", at.clone()),
                    ("updatedAt", at.clone()),
                    ("finishedAt", Json::Null),
                ]),
                &"Observed native Run".into(),
            )
            .await?;
        let history = steps
            .create(
                "task-board/history",
                &Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("taskId", Json::String(task_id.clone())),
                    ("operationId", f(request, "operationId").clone()),
                    ("actor", actor.clone()),
                    ("createdAt", at.clone()),
                    ("kind", "continued".into()),
                    (
                        "detail",
                        "Tracking a turn started directly in the existing agent conversation."
                            .into(),
                    ),
                    ("previousTask", task.document.pin.clone()),
                    ("previousHistory", f(value, "lastHistory").clone()),
                    ("priorRun", f(request, "run").clone()),
                    ("fromTarget", Json::Null),
                    ("toTarget", f(&original_request, "target").clone()),
                    ("result", Json::Null),
                    ("review", Json::Null),
                ]),
                &"Native history".into(),
            )
            .await?;
        let mut next = patch(
            value,
            [
                ("workflowState", "in_progress".into()),
                ("publication", Json::Null),
                ("runtimeWait", Json::Null),
                ("attemptCount", attempt.clone()),
                ("lastRun", run.clone()),
                ("lastHistory", history.clone()),
                ("updatedAt", at.clone()),
                (
                    "claim",
                    Json::object([
                        ("operationId", f(request, "operationId").clone()),
                        (
                            "callerPrincipalId",
                            Json::String(self.engine.principal_id().clone()),
                        ),
                        ("attempt", attempt),
                        ("owner", actor.clone()),
                        ("request", original_request.clone()),
                        ("requestHash", request_hash),
                        ("target", f(&original_request, "target").clone()),
                        ("workspace", f(&original_request, "workspace").clone()),
                        ("primaryAtStart", primary.clone()),
                        ("claimedAt", at.clone()),
                        ("phase", "running".into()),
                        ("run", run.clone()),
                    ]),
                ),
            ],
        );
        next.remove("completionIssue");
        let published = steps
            .write(
                "task-board/task",
                &next,
                &Json::object([
                    ("objectId", Json::String(task_id.clone())),
                    ("expectedRevision", f(&reserved, "revision").clone()),
                ]),
            )
            .await?;
        let mut result = outcome(f(request, "operationId"));
        result.insert("task", published);
        result.insert("run", run);
        result.insert("history", history);
        Ok(result)
    }
}
