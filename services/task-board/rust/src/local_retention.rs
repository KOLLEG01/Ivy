//! Collection of eligible local workflow graphs. A remote read can veto cleanup;
//! a failed read never establishes that an external owner or delivery is absent.
use crate::local_journal::{LocalJournal, count_column, text};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, json::parse, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
use ivy_store::{
    receipts::has_unresolved_outcome,
    rusqlite::{OptionalExtension, params},
    sqlite::error,
};
use std::collections::HashSet;

pub struct HistoryLimits {
    pub count: u64,
    pub bytes: u64,
}
impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            count: 10_000,
            bytes: 256 * 1024 * 1024,
        }
    }
}

pub struct LocalRetention<'a, R> {
    pub journal: LocalJournal<'a>,
    pub client: &'a R,
    pub limits: HistoryLimits,
}

fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn equal(value: &Json, key: &str, expected: &str) -> bool {
    value.get(key).and_then(Json::as_js_string) == Some(&expected.into())
}
fn string(value: &Json, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn references(value: &Json, found: &mut HashSet<JsString>) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Json::String(value) => {
                found.insert(value.clone());
            }
            Json::Array(values) => pending.extend(values),
            Json::Object(values) => pending.extend(values.iter().map(|(_, v)| v)),
            _ => {}
        }
    }
}

impl<R: RpcClient> LocalRetention<'_, R> {
    /// `completed_at` has been serialized by the owner's UTC clock. No retention
    /// age is imposed: eligible closed graphs can be removed as soon as needed.
    pub fn complete_workflow(
        &self,
        root: &JsString,
        completed_at: &JsString,
        dependency: Option<&JsString>,
    ) -> IvyResult<()> {
        if let Some(dependency) = dependency {
            self.journal
                .connection
                .execute(
                    "INSERT OR IGNORE INTO cleanup_dependencies VALUES(?,?)",
                    params![text(root), text(dependency)],
                )
                .map_err(error)?;
        }
        self.journal.connection.execute("INSERT INTO cleanup_roots VALUES(?,?) ON CONFLICT(root_id) DO UPDATE SET delete_after=excluded.delete_after",params![text(root),text(completed_at)]).map_err(error)?;
        Ok(())
    }

    fn full(&self, reserved_bytes: u64, reserved_rows: u64) -> IvyResult<bool> {
        let (rows, bytes): (u64, u64) = self
            .journal
            .connection
            .query_row(
                "SELECT COALESCE(SUM(rows),0),COALESCE(SUM(bytes),0) FROM storage_usage",
                [],
                |row| Ok((count_column(row, 0)?, count_column(row, 1)?)),
            )
            .map_err(error)?;
        Ok(rows.saturating_add(reserved_rows) > self.limits.count
            || bytes.saturating_add(reserved_bytes) > self.limits.bytes)
    }
    fn active_gate(&self, root: &JsString) -> IvyResult<bool> {
        Ok(self
            .journal
            .connection
            .query_row(
                "SELECT 1 FROM active_gates WHERE operation_id=? LIMIT 1",
                [text(root)],
                |_| Ok(()),
            )
            .optional()
            .map_err(error)?
            .is_some())
    }
    async fn remote(&self, id: &JsString) -> IvyResult<Option<Json>> {
        match self
            .client
            .request(
                "objects.read",
                Json::object([("objectId", Json::String(id.clone()))]),
                RequestOptions::default(),
            )
            .await
        {
            Ok(value) => Ok(Some(value)),
            Err(failure) if failure.code == "not_found" => Ok(None),
            Err(failure) => Err(failure),
        }
    }
    async fn task_active(&self, id: &JsString, revision: &Json) -> IvyResult<bool> {
        let Some(read) = self.remote(id).await? else {
            return Ok(false);
        };
        let object = field(&read, "object");
        let content = field(&read, "content");
        let value = field(content, "value");
        Ok(equal(object, "contractKey", "task-board/task")
            && field(object, "effectivelyArchived").as_bool() != Some(true)
            && field(object, "currentRevision") == revision
            && equal(content, "encoding", "json")
            && (equal(value, "workflowState", "todo") || equal(value, "workflowState", "waiting")))
    }
    async fn retained_root(&self, root: &JsString) -> IvyResult<bool> {
        if self.active_gate(root)? {
            return Ok(true);
        }
        let pending=self.journal.connection.query_row("SELECT 1 FROM cleanup_dependencies c JOIN documents d ON d.id=c.operation_id JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE c.root_id=? AND json_extract(r.value_json,'$.phase') NOT IN ('succeeded','failed') LIMIT 1",[text(root)],|_|Ok(())).optional().map_err(error)?.is_some();
        if pending {
            return Ok(true);
        }
        let Some(key) = self
            .journal
            .connection
            .query_row(
                "SELECT key FROM documents WHERE id=?",
                [text(root)],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(error)?
        else {
            return Ok(false);
        };
        if key == "task-board/scheduler-attempt" {
            let attempt = self
                .journal
                .read(&key, &Json::String(root.clone()), false)?
                .value;
            let task = attempt.required("task")?;
            return self
                .task_active(&string(task, "objectId")?, task.required("revision")?)
                .await;
        }
        if key != "task-board/operation" {
            return Ok(false);
        }
        let operation = self
            .journal
            .read(&key, &Json::String(root.clone()), false)?
            .value;
        if !equal(&operation, "phase", "succeeded") && !equal(&operation, "phase", "failed") {
            return Ok(true);
        }
        if equal(field(&operation, "error"), "execution", "unknown") {
            return Ok(true);
        }
        let request = field(&operation, "request");
        if equal(field(&operation, "actor"), "source", "scheduler")
            && request.get("taskId").is_some()
            && request.get("expectedRevision").is_some()
            && self
                .task_active(
                    &string(request, "taskId")?,
                    request.required("expectedRevision")?,
                )
                .await?
        {
            return Ok(true);
        }
        for write in operation
            .required("writes")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let outcome = field(write, "outcome");
            if !equal(write, "contractKey", "task-board/delivery") || outcome == &Json::Null {
                continue;
            }
            if let Some(delivery) = self.remote(&string(outcome, "objectId")?).await? {
                if !equal(
                    field(&delivery, "object"),
                    "contractKey",
                    "task-board/delivery",
                ) || !equal(field(&delivery, "content"), "encoding", "json")
                    || !equal(
                        field(field(&delivery, "content"), "value"),
                        "state",
                        "confirmed",
                    )
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub async fn collect(
        &self,
        reserved_bytes: u64,
        reserved_rows: u64,
        protected_id: Option<&JsString>,
        incoming: Option<&Json>,
    ) -> IvyResult<()> {
        if !self.full(reserved_bytes, reserved_rows)? {
            return Ok(());
        }
        let mut incoming_ids = HashSet::new();
        if let Some(id) = protected_id {
            incoming_ids.insert(id.clone());
        }
        if let Some(incoming) = incoming {
            references(incoming, &mut incoming_ids);
        }
        let due = self
            .journal
            .connection
            .prepare("SELECT root_id FROM cleanup_roots ORDER BY delete_after,rowid")
            .map_err(error)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        for root in &due {
            if !self.full(reserved_bytes, reserved_rows)? {
                break;
            }
            let root = JsString::from(root.clone());
            let before: Option<(u64, String, String)> = self
                .journal
                .connection
                .query_row(
                    "SELECT current_revision,key,name FROM documents WHERE id=?",
                    [text(&root)],
                    |row| Ok((count_column(row, 0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(error)?;
            if self.retained_root(&root).await? {
                continue;
            }
            let ids=self.journal.connection.prepare("WITH RECURSIVE subtree(id) AS (SELECT ? UNION ALL SELECT documents.id FROM documents JOIN subtree ON documents.parent_id=subtree.id) SELECT id FROM subtree").map_err(error)?.query_map([text(&root)],|row|row.get::<_,String>(0)).map_err(error)?.collect::<Result<Vec<_>,_>>().map_err(error)?;
            // A veto inside the transaction deliberately rolls back rather than
            // committing even a nominally read-only attempt.
            self.journal
                .connection
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(error)?;
            let result = (|| -> IvyResult<bool> {
                let after: Option<u64> = self
                    .journal
                    .connection
                    .query_row(
                        "SELECT current_revision FROM documents WHERE id=?",
                        [text(&root)],
                        |row| count_column(row, 0),
                    )
                    .optional()
                    .map_err(error)?;
                if before.as_ref().map(|row| row.0) != after
                    || ids
                        .iter()
                        .any(|id| incoming_ids.contains(&JsString::from(id.clone())))
                    || self.active_gate(&root)?
                {
                    return Ok(false);
                }
                let selected: HashSet<_> = ids.iter().cloned().collect();
                let rows=self.journal.connection.prepare("SELECT d.id,d.key,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision").map_err(error)?.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?))).map_err(error)?.collect::<Result<Vec<_>,_>>().map_err(error)?;
                for (id, key, encoded) in rows {
                    let value = parse(&encoded, MANAGEMENT_FRAME_BYTES)?;
                    if selected.contains(&id) {
                        if has_unresolved_outcome(&value)?
                            || key == "task-board/native-call"
                                && (!equal(&value, "state", "observed")
                                    || !equal(&value, "ownerPhase", "succeeded")
                                        && !equal(&value, "ownerPhase", "failed"))
                        {
                            return Ok(false);
                        }
                    } else {
                        let mut found = HashSet::new();
                        references(&value, &mut found);
                        if ids
                            .iter()
                            .any(|id| found.contains(&JsString::from(id.clone())))
                        {
                            return Ok(false);
                        }
                    }
                }
                if let Some((_, key, name)) = &before {
                    if key == "task-board/operation" {
                        let operation = self
                            .journal
                            .read(key, &Json::String(root.clone()), false)?
                            .value;
                        self.journal
                            .connection
                            .execute(
                                "INSERT OR IGNORE INTO retired_operations VALUES(?,?,?,?)",
                                params![
                                    text(&string(&operation, "callerPrincipalId")?),
                                    text(&string(&operation, "operationId")?),
                                    text(&string(&operation, "requestHash")?),
                                    name
                                ],
                            )
                            .map_err(error)?;
                    }
                }
                for id in &ids {
                    self.journal
                        .connection
                        .execute("DELETE FROM active_gates WHERE gate_id=?", [id])
                        .map_err(error)?;
                    self.journal
                        .connection
                        .execute("DELETE FROM operation_gates WHERE operation_id=?", [id])
                        .map_err(error)?;
                    self.journal
                        .connection
                        .execute(
                            "DELETE FROM cleanup_dependencies WHERE root_id=? OR operation_id=?",
                            params![id, id],
                        )
                        .map_err(error)?;
                    self.journal
                        .connection
                        .execute(
                            "DELETE FROM blobs WHERE id=? OR parent_id=?",
                            params![id, id],
                        )
                        .map_err(error)?;
                    self.journal
                        .connection
                        .execute("DELETE FROM revisions WHERE id=?", [id])
                        .map_err(error)?;
                    self.journal
                        .connection
                        .execute("DELETE FROM documents WHERE id=?", [id])
                        .map_err(error)?;
                }
                self.journal
                    .connection
                    .execute("DELETE FROM cleanup_roots WHERE root_id=?", [text(&root)])
                    .map_err(error)?;
                Ok(true)
            })();
            match result {
                Ok(true) => {
                    if let Err(failure) = self
                        .journal
                        .connection
                        .execute_batch("COMMIT")
                        .map_err(error)
                    {
                        self.journal
                            .connection
                            .execute_batch("ROLLBACK")
                            .map_err(error)?;
                        return Err(failure);
                    }
                }
                Ok(false) => self
                    .journal
                    .connection
                    .execute_batch("ROLLBACK")
                    .map_err(error)?,
                Err(failure) => {
                    self.journal
                        .connection
                        .execute_batch("ROLLBACK")
                        .map_err(error)?;
                    return Err(failure);
                }
            }
        }
        if !due.is_empty() {
            self.journal
                .connection
                .execute_batch("PRAGMA incremental_vacuum(256); PRAGMA wal_checkpoint(PASSIVE)")
                .map_err(error)?;
        }
        Ok(())
    }
}
