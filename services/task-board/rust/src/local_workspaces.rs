//! Reservations share the TaskBoard journal. Owner observations are fresh reads;
//! reclamation is fenced by both the observed revision and publication gate.
use crate::local_journal::text;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_store::{
    rusqlite::{Connection, OptionalExtension, params},
    sqlite::error,
};

pub struct WorkspaceDocument {
    pub pin: Json,
    pub value: Json,
    pub effectively_archived: bool,
}

/// Windows drive and UNC reservation keys use portable lexical normalization.
/// Other workspace identifiers retain their exact units.
pub fn workspace_key(path: &JsString) -> IvyResult<JsString> {
    let units = &path.0;
    let drive = units.len() >= 3
        && ((65..=90).contains(&units[0]) || (97..=122).contains(&units[0]))
        && units[1] == 58
        && [47, 92].contains(&units[2]);
    if drive || units.starts_with(&[92, 92]) {
        Ok(ivy_sdk::lowercase_utf16(&windows_key(path, drive).0))
    } else {
        Ok(path.clone())
    }
}

fn windows_key(path: &JsString, drive: bool) -> JsString {
    let separator = |unit: &u16| matches!(*unit, 47 | 92);
    let raw: Vec<_> = path.0[2..]
        .split(separator)
        .filter(|part| !part.is_empty())
        .collect();
    let roots = if !drive && raw.len() >= 2 { 2 } else { 0 };
    let mut parts: Vec<&[u16]> = Vec::new();
    for (index, part) in raw.into_iter().enumerate() {
        if index < roots {
            parts.push(part);
        } else if part == [46, 46] {
            if parts.len() > roots {
                parts.pop();
            }
        } else if part != [46] {
            parts.push(part);
        }
    }
    let mut key = if drive {
        vec![path.0[0], 58, 92]
    } else if roots == 2 {
        vec![92, 92]
    } else {
        vec![92]
    };
    for part in &parts {
        if key.last() != Some(&92) {
            key.push(92);
        }
        key.extend_from_slice(part);
    }
    if (parts.len() == roots || path.0.last().is_some_and(separator)) && key.last() != Some(&92) {
        key.push(92);
    }
    JsString(key)
}

/// The workflow store enforces its selected exact contract, root and immutable
/// revision before returning an owner observation.
#[allow(async_fn_in_trait)]
pub trait WorkspaceOwner {
    async fn read(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        include_archived: bool,
    ) -> IvyResult<WorkspaceDocument>;
    async fn stat(&self, object_id: &JsString) -> IvyResult<Json>;
}

pub struct LocalWorkspaces<'a, S, N> {
    pub connection: &'a Connection,
    pub store: &'a S,
    /// Required exact workspace-key policy. No OS-dependent filesystem lookup
    /// or ordinal/case-normalization fallback is provided by this journal.
    pub normalize: N,
}
struct Reservation {
    cwd: JsString,
    task: JsString,
    operation: JsString,
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn equal(value: &Json, key: &str, want: &str) -> bool {
    value.get(key).and_then(Json::as_js_string) == Some(&want.into())
}
fn optional_record<'a>(value: &'a Json, key: &str) -> IvyResult<Option<&'a Json>> {
    match value.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(value) if value.as_object().is_some() => Ok(Some(value)),
        _ => Err(IvyError::new(
            "internal_error",
            "Workspace owner contains an invalid record field.",
        )),
    }
}
impl<S: WorkspaceOwner, N: Fn(&JsString) -> IvyResult<JsString>> LocalWorkspaces<'_, S, N> {
    fn reservation(&self, host: &JsString, cwd: &JsString) -> IvyResult<Option<Reservation>> {
        let key = (self.normalize)(cwd)?;
        let rows=self.connection.prepare("SELECT canonical_cwd,task_id,operation_id FROM workspace_reservations WHERE host_id=?").map_err(error)?.query_map([text(host)],|row|Ok(Reservation{cwd:row.get::<_,String>(0)?.into(),task:row.get::<_,String>(1)?.into(),operation:row.get::<_,String>(2)?.into()})).map_err(error)?.collect::<Result<Vec<_>,_>>().map_err(error)?;
        for row in rows {
            if (self.normalize)(&row.cwd)? == key {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    pub fn reserve(
        &self,
        host: &JsString,
        cwd: &JsString,
        task: &JsString,
        operation: &JsString,
    ) -> IvyResult<Option<JsString>> {
        let key = (self.normalize)(cwd)?;
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(error)?;
        let result = (|| -> IvyResult<Option<JsString>> {
            let existing = self.reservation(host, &key)?;
            if let Some(row) = &existing {
                if &row.task != task {
                    return Ok(Some(row.task.clone()));
                }
            }
            self.connection.execute("DELETE FROM workspace_reservations WHERE task_id=? AND (host_id<>? OR canonical_cwd<>?)",params![text(task),text(host),text(&key)]).map_err(error)?;
            self.connection.execute("INSERT INTO workspace_reservations VALUES(?,?,?,?) ON CONFLICT(host_id,canonical_cwd) DO NOTHING",params![text(host),text(&key),text(task),text(existing.as_ref().map(|row|&row.operation).unwrap_or(operation))]).map_err(error)?;
            Ok(None)
        })();
        match result {
            Ok(Some(owner)) => {
                self.connection.execute_batch("ROLLBACK").map_err(error)?;
                Ok(Some(owner))
            }
            Ok(None) => match self.connection.execute_batch("COMMIT").map_err(error) {
                Ok(()) => Ok(None),
                Err(failure) => {
                    self.connection.execute_batch("ROLLBACK").map_err(error)?;
                    Err(failure)
                }
            },
            Err(failure) => {
                self.connection.execute_batch("ROLLBACK").map_err(error)?;
                Err(failure)
            }
        }
    }

    pub async fn owner(
        &self,
        host: &JsString,
        cwd: &JsString,
        task: &JsString,
    ) -> IvyResult<Option<JsString>> {
        let key = (self.normalize)(cwd)?;
        let Some(row) = self.reservation(host, &key)? else {
            return Ok(None);
        };
        if &row.task == task {
            return Ok(None);
        }
        let selected = match self
            .store
            .read(
                "task-board/task",
                &Json::String(row.task.clone()),
                true,
                true,
            )
            .await
        {
            Ok(task) => Some(task),
            Err(failure) if failure.code == "not_found" => None,
            Err(failure) => return Err(failure),
        };
        if let Some(selected) = selected.filter(|task| !task.effectively_archived) {
            let shared = key
                .0
                .starts_with(&"project:".encode_utf16().collect::<Vec<_>>())
                || key
                    .0
                    .starts_with(&"directory:".encode_utf16().collect::<Vec<_>>());
            if optional_record(&selected.value, "claim")?.is_some()
                || optional_record(&selected.value, "publication")?.is_some()
                || !shared
                    && !["done", "cancelled", "backlog"]
                        .iter()
                        .any(|phase| equal(&selected.value, "workflowState", phase))
            {
                return Ok(Some(row.task));
            }
            if let Some(last) = optional_record(&selected.value, "lastRun")? {
                let run = self.store.read("task-board/run", last, true, false).await?;
                if !["completed", "failed", "cancelled"]
                    .iter()
                    .any(|phase| equal(&run.value, "phase", phase))
                {
                    return Ok(Some(row.task));
                }
            }
            let current = self.store.stat(&row.task).await?;
            let gated=self.connection.query_row("SELECT 1 FROM active_gates g JOIN documents d ON d.id=g.gate_id WHERE d.key='task-board/coordination' AND d.name=?",[format!("TaskBoard publication gate {}",text(&row.task))],|_|Ok(())).optional().map_err(error)?.is_some();
            if field(&current, "currentRevision") != field(&selected.pin, "revision") || gated {
                return Ok(Some(row.task));
            }
        }
        self.connection.execute("DELETE FROM workspace_reservations WHERE host_id=? AND canonical_cwd=? AND task_id=? AND operation_id=?",params![text(host),text(&row.cwd),text(&row.task),text(&row.operation)]).map_err(error)?;
        Ok(None)
    }
    pub async fn reserve_available(
        &self,
        host: &JsString,
        cwd: &JsString,
        task: &JsString,
        operation: &JsString,
    ) -> IvyResult<Option<JsString>> {
        if let Some(owner) = self.owner(host, cwd, task).await? {
            return Ok(Some(owner));
        }
        self.reserve(host, cwd, task, operation)
    }
    pub fn release(&self, task: &JsString) -> IvyResult<()> {
        self.connection
            .execute(
                "DELETE FROM workspace_reservations WHERE task_id=?",
                [text(task)],
            )
            .map_err(error)?;
        Ok(())
    }
    pub fn release_operation(&self, operation: &JsString) -> IvyResult<()> {
        self.connection
            .execute(
                "DELETE FROM workspace_reservations WHERE operation_id=?",
                [text(operation)],
            )
            .map_err(error)?;
        Ok(())
    }
}
