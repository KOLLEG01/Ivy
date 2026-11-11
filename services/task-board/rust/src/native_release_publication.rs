//! Publish only the compact release intent/observation on an already closed Run.
use super::*;

impl<S: NativePublicationStore, E: NativePublicationEngine, V: NativePublicationEvidence>
    NativePublisher<'_, S, E, V>
{
    pub(super) async fn publish_context_release<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
    ) -> IvyResult<Json> {
        let op = &operation.value;
        let request = f(op, "request");
        let change = f(request, "change");
        let run_pin = f(request, "run");
        let task_id = s(request, "taskId")?;
        // Retained write destinations select the exact pre-publication snapshot
        // after a lost response, just as the ordinary native publisher does.
        let reservation = array(f(op, "writes")).first().map(|v| f(v, "destination"));
        let task_address = Json::object([
            ("objectId", Json::String(task_id.clone())),
            (
                "revision",
                reservation
                    .and_then(|v| v.get("expectedRevision"))
                    .unwrap_or(f(request, "expectedRevision"))
                    .clone(),
            ),
        ]);
        let task = self.store.task_header(&task_address, false, true).await?;
        let expected_archived = reservation
            .and_then(|v| v.get("expectedArchived"))
            .unwrap_or(f(&task.metadata, "effectivelyArchived"))
            .clone();
        require(
            expected_archived.as_bool().is_some(),
            "task_board_contract_mismatch",
            "Release publication requires exact archive metadata.",
        )?;
        let original = self
            .store
            .lifecycle_read("task-board/run", run_pin, false, false)
            .await?;
        let run = &original.document.value;
        let task_value = &task.document.value;
        if array(f(op, "writes")).is_empty() {
            require(
                f(&task.metadata, "currentRevision") == f(request, "expectedRevision")
                    && f(&original.metadata, "currentRevision") == f(run_pin, "revision")
                    && f(task_value, "publication").is_null(),
                "revision_conflict",
                "Task or Run changed before release publication.",
            )?;
            self.engine.aggregate(task_value, task_id).await?;
        }
        require(
            eq(run, "phase", "completed")
                && !f(run, "result").is_null()
                && f(run, "taskId") == f(request, "taskId")
                && crate::native_release::pending(run),
            "task_board_native_order",
            "Only a declared completed context has a pending release.",
        )?;
        let mut release = f(run, "contextRelease").clone();
        if eq(change, "kind", "attachRelease") {
            require(
                !self
                    .store
                    .task_operations_pending(task_id, Some(&operation.pin.object_id))?,
                "task_board_context_release_pending",
                "Release attachment waits for accepted ticket operations.",
            )?;
            require(
                expected_archived.as_bool() == Some(false)
                    && crate::native_release::eligible(task_value, run, run_pin)
                    && f(&release, "call").is_null()
                    && self
                        .store
                        .queued_comment_ids(task_value, task_id)
                        .await?
                        .is_empty(),
                "task_board_context_needed",
                "New work retains this context before release attachment.",
            )?;
            let call = self
                .store
                .read("task-board/native-call", f(change, "call"), false)
                .await?;
            prepared_owner_matches(task_value, run, &call.value)?;
            let intended = self
                .evidence
                .intended(task_value, run, &Json::object([("kind", "release".into())]))
                .await?;
            require(
                eq(&call.value, "state", "prepared")
                    && eq(f(&call.value, "origin"), "kind", "release")
                    && same(
                        &self.evidence.resolve(f(&call.value, "request"))?,
                        f(&intended, "request"),
                    )?
                    && f(&call.value, "expectedDefinitionHash")
                        == f(&intended, "expectedDefinitionHash"),
                "task_board_native_intent_mismatch",
                "Release must retain the exact managed request and native definition.",
            )?;
            release.insert("call", f(change, "call").clone());
        } else if eq(change, "kind", "observeRelease") {
            let prior = f(&release, "call");
            require(
                !prior.is_null()
                    && f(prior, "objectId") == f(f(change, "call"), "objectId")
                    && n(f(change, "call"), "revision") > n(prior, "revision"),
                "task_board_native_intent_mismatch",
                "Observe only a newer revision of the original release.",
            )?;
            let previous = self
                .store
                .read("task-board/native-call", prior, false)
                .await?;
            let (call, observed) = self.observed(run, f(change, "call")).await?;
            require(
                !one(&previous.value, "ownerPhase", &["succeeded", "failed"])
                    && [
                        "operationId",
                        "request",
                        "owner",
                        "expectedDefinitionHash",
                        "origin",
                        "preparedAt",
                    ]
                    .iter()
                    .all(|key| f(&previous.value, key) == f(&call.value, key)),
                "task_board_native_intent_mismatch",
                "Release observations cannot replace their original identity or terminal receipt.",
            )?;
            release.insert("call", f(change, "call").clone());
            release.insert(
                "state",
                if eq(&observed, "phase", "succeeded") {
                    "succeeded"
                } else if eq(&observed, "phase", "failed") {
                    "retained"
                } else if eq(&observed, "phase", "outcome_unknown") {
                    "outcome_unknown"
                } else {
                    "observing"
                }
                .into(),
            );
            release.insert("code", f(&observed, "code").clone());
        } else {
            require(
                eq(&release, "state", "pending"),
                "task_board_native_order",
                "An owner-observed release must be reconciled, not replaced by local retention.",
            )?;
            release.insert("state", "retained".into());
            release.insert("code", f(change, "code").clone());
        }
        let compact = eq(f(task_value, "commentStore"), "kind", "tree");
        let mut final_task = task_value.clone();
        final_task.insert("publication", steps.pin()?);
        let reserved = self
            .write_task(
                steps,
                &final_task,
                &Json::object([
                    ("objectId", Json::String(task_id.clone())),
                    ("expectedRevision", f(request, "expectedRevision").clone()),
                    ("expectedArchived", expected_archived.clone()),
                ]),
                compact,
                false,
            )
            .await?;
        let mut next = run.clone();
        next.insert("contextRelease", release.clone());
        next.insert("updatedAt", f(op, "createdAt").clone());
        let saved = steps
            .write(
                "task-board/run",
                &next,
                &Json::object([
                    ("objectId", f(run_pin, "objectId").clone()),
                    ("expectedRevision", f(run_pin, "revision").clone()),
                ]),
            )
            .await?;
        if f(f(task_value, "lastRun"), "objectId") == f(run_pin, "objectId") {
            final_task.insert("lastRun", saved.clone());
        }
        if crate::native_release::owns_fence(task_value, run_pin) {
            if eq(&release, "state", "pending") {
                final_task.insert("contextRelease", saved.clone());
            } else {
                final_task.remove("contextRelease");
            }
        }
        final_task.insert("publication", Json::Null);
        let mut out = outcome(f(request, "operationId"));
        out.insert("run", saved);
        out.insert(
            "task",
            self.write_task(
                steps,
                &final_task,
                &Json::object([
                    ("objectId", Json::String(task_id.clone())),
                    ("expectedRevision", f(&reserved, "revision").clone()),
                    ("expectedArchived", expected_archived),
                ]),
                compact,
                false,
            )
            .await?,
        );
        if !crate::native_release::pending(&next) {
            let at = self.engine.now();
            self.store.complete_after_publication(
                s(run_pin, "objectId")?,
                &at,
                &operation.pin.object_id,
            )?;
            if !f(&release, "call").is_null() {
                self.store.complete_after_publication(
                    s(f(&release, "call"), "objectId")?,
                    &at,
                    &operation.pin.object_id,
                )?;
            }
        }
        Ok(out)
    }
}
