use super::*;
use records::{load, now, save};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};

pub trait MaintenanceTick {
    fn tick(&self) -> Pin<Box<dyn Future<Output = ()> + '_>>;
    fn diagnostics(&self) -> Vec<Json>;
}
pub struct Maintenance<'a, S> {
    pub retention: Retention<'a, S>,
    pub context: MaintenanceContext,
    loaded: Cell<bool>,
    artifacts: RefCell<Option<ArtifactJob>>,
    comments: RefCell<Option<CommentJob>>,
    error: RefCell<Option<(String, String)>>,
    retry_after: Cell<Option<Instant>>,
}
impl<'a, S: MaintenanceStore> Maintenance<'a, S> {
    pub fn new(store: &'a S, context: MaintenanceContext) -> Self {
        Self {
            retention: Retention::new(store),
            context,
            loaded: Cell::new(false),
            artifacts: RefCell::new(None),
            comments: RefCell::new(None),
            error: RefCell::new(None),
            retry_after: Cell::new(None),
        }
    }
}
impl MaintenanceContext {
    pub(super) fn manifest(
        &self,
        schema: u8,
        scope: &str,
        manifest: &ArchiveManifest,
    ) -> IvyResult<()> {
        require(
            schema == 1
                && !scope.is_empty()
                && hash(&manifest.content_hash)
                && JsString::from(manifest.host_id.as_str()) == self.host_id
                && absolute(&manifest.path),
            "task_board_scope_mismatch",
            "The cleanup must retain this host's verified export and scope.",
        )
    }
    pub fn validate_plan(&self, plan: &ArtifactPlan) -> IvyResult<()> {
        self.manifest(plan.schema_version, &plan.scope, &plan.archive_manifest)?;
        require(
            JsString::from(plan.host_id.as_str()) == self.host_id
                && JsString::from(plan.service_node_id.as_str()) == self.service_node_id,
            "task_board_scope_mismatch",
            "The export plan must name this owning host and service.",
        )?;
        let tasks: HashSet<_> = plan.task_ids.iter().collect();
        let pins: HashSet<_> = plan.files.iter().map(|v| &v.archive.object).collect();
        require(
            tasks.len() == plan.task_ids.len()
                && pins.len() == plan.files.len()
                && plan.files.iter().all(|v| {
                    tasks.contains(&v.owner_task_id)
                        && absolute(&v.archive.local_path)
                        && hash(&v.archive.content_hash)
                        && v.archive.object.revision > 0
                        && v.archive.object.revision <= 9_007_199_254_740_991
                })
                && plan
                    .reference_pages
                    .iter()
                    .all(|v| tasks.contains(&v.owner_task_id)),
            "invalid_arguments",
            "The verified plan must contain unique exact file pins owned by its selected Tasks.",
        )
    }
}
impl<'a, S: MaintenanceStore> Maintenance<'a, S> {
    pub async fn admit_artifacts(&self, plan: ArtifactPlan) -> IvyResult<ArtifactJob> {
        self.context.validate_plan(&plan)?;
        let path = self.context.data_root.join("artifact-cleanup.json");
        let previous: Option<ArtifactJob> = load(path.clone()).await?;
        if let Some(job) = &previous {
            self.context
                .manifest(job.schema_version, &job.scope, &job.archive_manifest)?;
            if let Some(original) =
                load::<ArtifactPlan>(self.context.data_root.join("artifact-cleanup-plan.json"))
                    .await?
            {
                require(
                    original == plan,
                    "mutation_conflict",
                    "Resume the original verified export plan.",
                )?;
            }
        }
        let mut job = previous.unwrap_or_else(|| ArtifactJob {
            schema_version: 1,
            scope: plan.scope.clone(),
            archive_manifest: plan.archive_manifest.clone(),
            task_ids: vec![],
            reference_pages: vec![],
            remaining: vec![],
            deleted_files: 0,
            deleted_bytes: 0,
            pruned_revisions: 0,
            prune_attempts: 0,
            task_cursor: 0,
            file_cursor: 0,
            run_history_cursor: None,
            updated_at: now(),
            completed_at: None,
            last_error: None,
            retire_pending: vec![],
        });
        require(
            job.scope == plan.scope && job.archive_manifest == plan.archive_manifest,
            "mutation_conflict",
            "A different verified export already owns this cleanup.",
        )?;
        let retired: HashSet<_> = job.task_ids.iter().cloned().collect();
        job.retire_pending = plan
            .task_ids
            .iter()
            .filter(|id| !retired.contains(*id))
            .cloned()
            .collect();
        if !job.retire_pending.is_empty() {
            job.completed_at = None;
        }
        // Persist both documents before the first Task CAS. After a lost reply,
        // an already-retired Task still contributes its verified deletion queue.
        save(
            &self.context.data_root.join("artifact-cleanup-plan.json"),
            &plan,
        )
        .await?;
        save(&path, &job).await?;
        let attempts = job.retire_pending.len();
        for _ in 0..attempts {
            self.retire_one(&mut job, &plan).await?;
            save(&path, &job).await?;
        }
        Ok(job)
    }
    async fn retire_one(&self, job: &mut ArtifactJob, plan: &ArtifactPlan) -> IvyResult<()> {
        let Some(id) = job.retire_pending.first().cloned() else {
            return Ok(());
        };
        let archives: Vec<_> = plan.files.iter().map(|file| file.archive.clone()).collect();
        match self
            .retention
            .retire_attachments(&id, &archives, &job.scope)
            .await
        {
            Ok(_) => {
                job.retire_pending.remove(0);
                if !job.task_ids.contains(&id) {
                    job.task_ids.push(id.clone());
                    job.remaining.extend(
                        plan.files
                            .iter()
                            .filter(|v| v.owner_task_id == id)
                            .map(|v| v.archive.clone()),
                    );
                    job.reference_pages.extend(
                        plan.reference_pages
                            .iter()
                            .filter(|v| v.owner_task_id == id)
                            .map(|v| v.object.clone()),
                    );
                }
            }
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "task_board_publication_busy" | "revision_conflict"
                ) =>
            {
                let id = job.retire_pending.remove(0);
                job.retire_pending.push(id);
            }
            Err(error) => return Err(error),
        }
        job.updated_at = now();
        Ok(())
    }
    pub async fn pass(&self) -> IvyResult<()> {
        if !self.loaded.get() {
            let mut artifacts: Option<ArtifactJob> =
                load(self.context.data_root.join("artifact-cleanup.json")).await?;
            if let Some(job) = artifacts.as_mut() {
                if !job.remaining.is_empty()
                    || !job.reference_pages.is_empty()
                    || !job.retire_pending.is_empty()
                {
                    job.completed_at = None;
                }
            }
            let comments = load(self.context.data_root.join("comment-cleanup.json")).await?;
            self.artifacts.replace(artifacts);
            self.comments.replace(comments);
            self.loaded.set(true);
        }
        let mut artifact = self.artifacts.borrow_mut().take();
        let mut comment = self.comments.borrow_mut().take();
        let active = artifact.as_ref().is_some_and(|v| v.completed_at.is_none())
            || comment.as_ref().is_some_and(|v| v.completed_at.is_none());
        let result = async {
            if !active {
                return Ok(());
            }
            crate::inspection_journal::verify_owner(
                self.retention.store.client().as_ref(),
                &self.context.principal_id,
                &self.context.service_node_id,
            )
            .await?;
            let mut error = None;
            if let Some(job) = artifact.as_mut().filter(|v| v.completed_at.is_none()) {
                let work = self.artifact_pass(job).await;
                job.last_error = work.as_ref().err().map(|e| e.code.clone());
                save(&self.context.data_root.join("artifact-cleanup.json"), job).await?;
                if let Err(failure) = work {
                    error = Some(failure);
                }
            }
            if let Some(job) = comment.as_mut().filter(|v| v.completed_at.is_none()) {
                let work = self.comment_pass(job).await;
                job.last_error = work.as_ref().err().map(|e| e.code.clone());
                save(&self.context.data_root.join("comment-cleanup.json"), job).await?;
                if let Err(failure) = work {
                    error.get_or_insert(failure);
                }
            }
            error.map_or(Ok(()), Err)
        }
        .await;
        self.artifacts.replace(artifact);
        self.comments.replace(comment);
        if result.is_err() {
            // A remote effect may have succeeded before saving its progress.
            // Reconcile from the durable queue on the next attempt.
            self.loaded.set(false);
        }
        result
    }
    async fn scoped_task(&self, id: &str) -> IvyResult<Json> {
        let metadata = self
            .retention
            .request("objects.stat", Json::object([("objectId", id.into())]))
            .await?;
        require(
            metadata.get("contractKey") == Some(&"task-board/task".into())
                && metadata.get("parentId")
                    == Some(
                        &self
                            .retention
                            .store
                            .root()
                            .map(Json::String)
                            .unwrap_or(Json::Null),
                    )
                && metadata.get("ownerObjectId").is_some_and(Json::is_null),
            "task_board_scope_mismatch",
            "Cleanup cannot affect another TaskBoard workspace.",
        )?;
        Ok(metadata)
    }
    async fn prune(&self, id: &str, revision: Json, job: &mut ArtifactJob) -> IvyResult<u64> {
        let mutation = self
            .retention
            .store
            .mutation(&format!(
                "{}:prune:{id}:{}:{}",
                job.scope,
                revision.as_f64().ok_or_else(IvyError::internal)?,
                job.prune_attempts
            ))
            .await?;
        match self
            .retention
            .request(
                "objects.pruneRevisions",
                Json::object([
                    ("objectId", id.into()),
                    ("expectedRevision", revision),
                    ("maximumCount", Json::Number(1.0)),
                    ("maximumAgeDays", Json::Number(1.0)),
                    ("maximumBytes", Json::Number(1.0)),
                    ("mutationId", Json::String(mutation)),
                ]),
            )
            .await
        {
            Ok(value) => {
                let count = value
                    .required("deleted")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)? as u64;
                job.prune_attempts += 1;
                job.pruned_revisions += count;
                Ok(count)
            }
            Err(error) if error.code == "revision_conflict" => Ok(0),
            Err(error) => Err(error),
        }
    }
    async fn remove(
        &self,
        object: &ObjectPin,
        key: &str,
        job: &ArtifactJob,
        file: Option<&ArchivedAttachment>,
        verified: &mut HashSet<String>,
    ) -> IvyResult<bool> {
        let metadata = match self
            .retention
            .request(
                "objects.stat",
                Json::object([("objectId", object.object_id.clone().into())]),
            )
            .await
        {
            Ok(value) => value,
            Err(error) if error.code == "not_found" => return Ok(true),
            Err(error) => return Err(error),
        };
        let result: IvyResult<bool> = async {
            let owner = metadata.required("ownerObjectId")?.as_str()?;
            require(
                metadata.get("contractKey") == Some(&key.into()) && job.task_ids.contains(&owner),
                "task_board_scope_mismatch",
                "Cleanup cannot delete content outside the exported Tasks.",
            )?;
            if !verified.contains(&owner) {
                self.scoped_task(&owner).await?;
                verified.insert(owner);
            }
            if let Some(file) = file {
                if metadata.get("currentRevision") != Some(&Json::Number(object.revision as f64)) {
                    return Ok(false);
                }
                let history = self
                    .retention
                    .request(
                        "objects.history",
                        Json::object([
                            ("objectId", object.object_id.clone().into()),
                            ("limit", Json::Number(1.0)),
                        ]),
                    )
                    .await?;
                let revision = items(&history, "items")?
                    .first()
                    .ok_or_else(IvyError::internal)?;
                require(
                    revision.get("contentHash") == Some(&file.content_hash.clone().into())
                        && revision.get("byteLength")
                            == Some(&Json::Number(file.byte_length as f64)),
                    "task_board_evidence_mismatch",
                    "Cleanup cannot delete bytes that differ from the verified export.",
                )?;
            }
            let mutation = self
                .retention
                .store
                .mutation(&format!(
                    "{}:delete:{}:{}",
                    job.scope,
                    object.object_id,
                    metadata
                        .required("currentRevision")?
                        .as_f64()
                        .ok_or_else(IvyError::internal)?
                ))
                .await?;
            self.retention
                .request(
                    "objects.delete",
                    Json::object([
                        ("objectId", object.object_id.clone().into()),
                        (
                            "expectedRevision",
                            metadata.required("currentRevision")?.clone(),
                        ),
                        ("mutationId", Json::String(mutation)),
                    ]),
                )
                .await?;
            Ok(true)
        }
        .await;
        match result {
            Err(error)
                if matches!(error.code.as_str(), "target_conflict" | "revision_conflict") =>
            {
                Ok(false)
            }
            result => result,
        }
    }
    async fn artifact_pass(&self, job: &mut ArtifactJob) -> IvyResult<()> {
        self.context
            .manifest(job.schema_version, &job.scope, &job.archive_manifest)?;
        if !job.retire_pending.is_empty() {
            let plan: ArtifactPlan =
                load(self.context.data_root.join("artifact-cleanup-plan.json"))
                    .await?
                    .ok_or_else(|| {
                        IvyError::new(
                            "task_board_evidence_mismatch",
                            "The interrupted retirement requires its original verified plan.",
                        )
                    })?;
            self.context.validate_plan(&plan)?;
            require(
                plan.scope == job.scope
                    && plan.archive_manifest.content_hash == job.archive_manifest.content_hash
                    && job
                        .retire_pending
                        .iter()
                        .all(|id| plan.task_ids.contains(id)),
                "mutation_conflict",
                "The interrupted retirement must retain its original export plan.",
            )?;
            self.retire_one(job, &plan).await?;
        }
        let mut verified = HashSet::new();
        if !job.task_ids.is_empty() {
            let id = job.task_ids[job.task_cursor % job.task_ids.len()].clone();
            job.task_cursor = (job.task_cursor % job.task_ids.len()) + 1;
            let metadata = self.scoped_task(&id).await?;
            verified.insert(id.clone());
            if !self.retention.store.publication_busy(&id)?
                && !self.retention.store.source_in_use(&id)?
            {
                self.prune(&id, metadata.required("currentRevision")?.clone(), job)
                    .await?;
            }
            let mut query = Json::object([
                ("contractKey", "task-board/run".into()),
                ("includeArchived", true.into()),
                ("includeContent", true.into()),
                ("limit", Json::Number(32.0)),
            ]);
            if let Some(cursor) = &job.run_history_cursor {
                query.insert("cursor", cursor.clone().into());
            }
            let runs = self.retention.request("objects.query", query).await?;
            for run in items(&runs, "items")? {
                let Some(document) = run.get("document") else {
                    continue;
                };
                let object = document.required("object")?;
                let content = document.required("content")?;
                if content.get("encoding") != Some(&"json".into()) {
                    continue;
                }
                let value = content.required("value")?;
                if object.get("parentId")
                    == Some(
                        &self
                            .retention
                            .store
                            .root()
                            .map(Json::String)
                            .unwrap_or(Json::Null),
                    )
                    && object.get("ownerObjectId").is_some_and(Json::is_null)
                    && value
                        .get("taskId")
                        .and_then(Json::as_js_string)
                        .and_then(|v| v.to_utf8().ok())
                        .is_some_and(|id| job.task_ids.contains(&id))
                    && value
                        .get("phase")
                        .and_then(Json::as_js_string)
                        .and_then(|v| v.to_utf8().ok())
                        .is_some_and(|p| ["completed", "failed", "cancelled"].contains(&p.as_str()))
                {
                    self.prune(
                        &run.required("objectId")?.as_str()?,
                        run.required("revision")?.clone(),
                        job,
                    )
                    .await?;
                }
            }
            job.run_history_cursor = runs
                .get("nextCursor")
                .filter(|v| !v.is_null())
                .map(Json::as_str)
                .transpose()?;
        }
        for _ in 0..job.reference_pages.len().min(2) {
            let page = job.reference_pages[0].clone();
            let removed = self
                .remove(&page, "task-board/reference-page", job, None, &mut verified)
                .await?;
            job.reference_pages.remove(0);
            if !removed {
                job.reference_pages.push(page);
            }
        }
        for _ in 0..job.remaining.len().min(24) {
            let index = job.file_cursor % job.remaining.len();
            let file = job.remaining[index].clone();
            if self
                .remove(
                    &file.object,
                    "task-board/attachment",
                    job,
                    Some(&file),
                    &mut verified,
                )
                .await?
            {
                job.remaining.remove(index);
                job.deleted_files += 1;
                job.deleted_bytes += file.byte_length;
                job.file_cursor = index;
            } else {
                job.file_cursor = index + 1;
            }
        }
        job.updated_at = now();
        if job.remaining.is_empty()
            && job.reference_pages.is_empty()
            && job.retire_pending.is_empty()
        {
            job.completed_at = Some(job.updated_at.clone());
        }
        Ok(())
    }
    async fn comment_pass(&self, job: &mut CommentJob) -> IvyResult<()> {
        self.context
            .manifest(job.schema_version, &job.scope, &job.archive_manifest)?;
        if let Some(selected) = job.remaining.first() {
            match self
                .retention
                .archive_comments(&selected.task_id, &selected.comments, &job.scope)
                .await
            {
                Ok((rewritten, removed)) => {
                    job.remaining.remove(0);
                    job.completed_tasks += 1;
                    job.rewritten_comments += rewritten;
                    job.removed_characters += removed;
                }
                Err(error)
                    if matches!(
                        error.code.as_str(),
                        "revision_conflict" | "task_board_publication_busy"
                    ) =>
                {
                    let selected = job.remaining.remove(0);
                    job.remaining.push(selected);
                }
                Err(error) => return Err(error),
            }
        }
        job.updated_at = now();
        if job.remaining.is_empty() {
            job.completed_at = Some(job.updated_at.clone());
        }
        Ok(())
    }
}
impl<S: MaintenanceStore> MaintenanceTick for Maintenance<'_, S> {
    fn tick(&self) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        Box::pin(async {
            if self.retry_after.get().is_some_and(|at| Instant::now() < at) {
                return;
            }
            match self.pass().await {
                Ok(()) => {
                    self.error.replace(None);
                    self.retry_after.set(None);
                }
                Err(error) => {
                    self.retry_after
                        .set(Some(Instant::now() + Duration::from_secs(30)));
                    let mut current = self.error.borrow_mut();
                    if current.as_ref().is_none_or(|(code, _)| code != &error.code) {
                        *current = Some((error.code, now()));
                    }
                }
            }
        })
    }
    fn diagnostics(&self) -> Vec<Json> {
        self.error.borrow().as_ref().map(|(code,first)|vec![Json::object([("code",code.clone().into()),("resource",Json::object([("serviceNodeId",Json::String(self.context.service_node_id.clone()))])),("source",Json::String(self.context.service_node_id.clone())),("severity","warning".into()),("status","current".into()),("firstObservedAt",first.clone().into()),("lastObservedAt",now().into()),("message","TaskBoard export cleanup is pending; its durable job and remaining data are retained.".into())])]).unwrap_or_default()
    }
}
