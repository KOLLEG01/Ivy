//! Current explicit collection. The owner supplies its real document reader,
//! request source and lossless parser/SQLite bindings. No catch is installed here.
use super::{
    local_reads::SqliteReads,
    operation_receipt::operation_receipt,
    store::{ObjectReads, RpcObjectReads},
    values::{field, has, insert, same, type_error},
};
use crate::{
    engine_read::read_failure::{ReadCause, ReadFailure},
    journal_store::Contracts,
    local_retention::HistoryLimits,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, JsString, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{OptionalRpcClient, RequestOptions};
use ivy_store::receipts::has_unresolved_outcome;
use std::collections::{HashMap, HashSet};
use tokio::sync::Mutex;

#[allow(async_fn_in_trait)]
pub trait HistoryQueries<E> {
    async fn query(&self, params: Json) -> std::result::Result<Json, E>;
}
impl<R: OptionalRpcClient + ?Sized, E: From<IvyError>> HistoryQueries<E> for RpcObjectReads<'_, R> {
    async fn query(&self, params: Json) -> std::result::Result<Json, E> {
        self.client
            .request_optional("objects.query", Some(params), RequestOptions::default())
            .await
            .map_err(E::from)
    }
}
#[allow(async_fn_in_trait)]
pub trait HistoryDocuments<E> {
    async fn history_document(
        &self,
        key: &str,
        address: Option<&Json>,
        primary: bool,
        include_archived: bool,
    ) -> std::result::Result<Json, E>;
}

/// Keep this state with the journal owner, including when read/store facades are
/// assembled for individual actions. Local writes never acquire this FIFO.
#[derive(Default)]
pub struct HistoryCollections {
    collection: Mutex<()>,
}

/// Local writes remain independent and are revalidated after every owner await.
pub struct LocalHistory<'a, D, R, C, Parse, Failure> {
    local: &'a SqliteReads<'a, C, Parse, Failure>,
    documents: &'a D,
    objects: &'a R,
    limits: HistoryLimits,
    collections: &'a HistoryCollections,
}
impl<'a, D, R, C, Parse, Failure> LocalHistory<'a, D, R, C, Parse, Failure> {
    pub fn new(
        local: &'a SqliteReads<'a, C, Parse, Failure>,
        documents: &'a D,
        objects: &'a R,
        collections: &'a HistoryCollections,
        limits: HistoryLimits,
    ) -> Self {
        Self {
            local,
            documents,
            objects,
            limits,
            collections,
        }
    }
}

#[derive(Default)]
struct Ids {
    ordered: Vec<JsString>,
    set: HashSet<JsString>,
}
impl Ids {
    fn add(&mut self, id: JsString) {
        if self.set.insert(id.clone()) {
            self.ordered.push(id);
        }
    }
}
fn references(value: &Json, found: &mut Ids) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Json::String(id) => found.add(id.clone()),
            Json::Array(values) => pending.extend(values.iter().rev()),
            Json::Object(fields) => pending.extend(fields.iter().rev().map(|(_, value)| value)),
            _ => {}
        }
    }
}
#[cfg(test)]
mod json_reference_tests {
    use super::*;
    #[test]
    fn deep_json_reference_walk_retains_all_values_and_deduplicates_without_recursion() {
        let mut value = Json::object([
            ("objectId", "pinned".into()),
            ("nested", Json::Array(vec!["other".into(), "pinned".into()])),
        ]);
        for _ in 0..30_000 {
            value = Json::Array(vec![value]);
        }
        let mut found = Ids::default();
        references(&value, &mut found);
        assert_eq!(
            found.ordered,
            vec![JsString::from("pinned"), JsString::from("other")]
        );
    }
}
struct Head {
    revision: f64,
    references: HashSet<JsString>,
    unresolved: bool,
}
#[derive(Default)]
struct Index {
    documents: HashMap<JsString, Head>,
    referrers: HashMap<JsString, HashSet<JsString>>,
    version: Option<(String, String)>,
}
impl Index {
    fn forget(&mut self, id: &JsString) {
        if let Some(head) = self.documents.remove(id) {
            for target in head.references {
                if let Some(sources) = self.referrers.get_mut(&target) {
                    sources.remove(id);
                    if sources.is_empty() {
                        self.referrers.remove(&target);
                    }
                }
            }
        }
    }
    fn protected(&self, ids: &[JsString]) -> bool {
        let selected: HashSet<_> = ids.iter().collect();
        ids.iter().any(|id| {
            self.documents.get(id).is_some_and(|head| head.unresolved)
                || self
                    .referrers
                    .get(id)
                    .is_some_and(|sources| sources.iter().any(|source| !selected.contains(source)))
        })
    }
}
type HistoryResult<T, E> = std::result::Result<T, ReadFailure<E>>;
fn ivy_code<E>(failure: &ReadFailure<E>, codes: &[&str]) -> bool {
    matches!(failure.cause(), ReadCause::Ivy(error) if codes.iter().any(|code| error.code == *code))
}
fn optional<'a, E>(value: Option<&'a Json>, key: &str) -> HistoryResult<Option<&'a Json>, E> {
    match value {
        None | Some(Json::Null) => Ok(None),
        _ => field(value, key),
    }
}
fn value<'a, E>(document: &'a Json) -> HistoryResult<&'a Json, E> {
    // Reading a later property produces the same undefined/null failure at its use.
    field::<ReadFailure<E>>(Some(document), "value")?
        .ok_or_else(|| type_error("Cannot read properties of undefined (reading 'phase')"))
}
fn iterable<E>(value: Option<&Json>, expression: &str) -> HistoryResult<Vec<Json>, E> {
    match value {
        Some(Json::Array(values)) => Ok(values.clone()),
        Some(Json::String(value)) => {
            let mut result = vec![];
            let mut i = 0;
            while i < value.0.len() {
                let length = if (0xd800..=0xdbff).contains(&value.0[i])
                    && value
                        .0
                        .get(i + 1)
                        .is_some_and(|v| (0xdc00..=0xdfff).contains(v))
                {
                    2
                } else {
                    1
                };
                result.push(Json::String(JsString(value.0[i..i + length].to_vec())));
                i += length;
            }
            Ok(result)
        }
        _ => Err(type_error(format!("{expression} is not iterable"))),
    }
}

impl<D, R, C: Contracts, Parse, Failure> LocalHistory<'_, D, R, C, Parse, Failure> {
    fn full<E>(&self, bytes: u64, rows: u64, headroom: bool) -> HistoryResult<bool, E>
    where
        Failure: Fn(ivy_store::rusqlite::Error) -> ReadFailure<E>,
    {
        let usage = self
            .local
            .row::<ReadFailure<E>>(
                "SELECT COALESCE(SUM(rows),0) AS rows,COALESCE(SUM(bytes),0) AS bytes FROM storage_usage",
                &[],
            )?
            .unwrap();
        let factor = if headroom { 0.9 } else { 1.0 };
        Ok(
            usage[0].number()? + rows as f64 > self.limits.count as f64 * factor
                || usage[1].number()? + bytes as f64 > self.limits.bytes as f64 * factor,
        )
    }
    fn version<E>(&self) -> HistoryResult<(String, String), E>
    where
        Failure: Fn(ivy_store::rusqlite::Error) -> ReadFailure<E>,
    {
        let data = self
            .local
            .row::<ReadFailure<E>>("PRAGMA data_version", &[])?
            .unwrap();
        let changes = self
            .local
            .row::<ReadFailure<E>>("SELECT total_changes() AS changes", &[])?
            .unwrap();
        Ok((data[0].string()?, changes[0].string()?))
    }
    async fn refresh<E>(&self, index: &mut Index, cooperative: bool) -> HistoryResult<(), E>
    where
        Parse: Fn(&str) -> HistoryResult<Json, E>,
        Failure: Fn(ivy_store::rusqlite::Error) -> ReadFailure<E>,
    {
        if index.version.as_ref() == Some(&self.version()?) {
            return Ok(());
        }
        let version = self.version()?;
        let heads = self
            .local
            .rows::<ReadFailure<E>>("SELECT id,key,current_revision FROM documents", &[])?;
        let mut live = HashSet::new();
        for (offset, head) in heads.iter().enumerate() {
            let id = JsString::from(head[0].string()?);
            let revision = head[2].number()?;
            live.insert(id.clone());
            if index
                .documents
                .get(&id)
                .is_none_or(|old| old.revision != revision)
            {
                let row = self.local.row::<ReadFailure<E>>(
                    "SELECT value_json FROM revisions WHERE id=? AND revision=?",
                    &[
                        Some(&Json::String(id.clone())),
                        Some(&Json::Number(revision)),
                    ],
                )?;
                if let Some(row) = row {
                    let value = (self.local.parse)(&row[0].string()?)?;
                    let mut found = Ids::default();
                    references(&value, &mut found);
                    index.forget(&id);
                    let key = head[1].string()?;
                    let unresolved = self.local.contracts.local(&key)?
                        && has_unresolved_outcome(&value)?
                        || key == "task-board/native-call"
                            && (field::<ReadFailure<E>>(Some(&value), "state")?
                                != Some(&Json::from("observed"))
                                || ![Some(&Json::from("succeeded")), Some(&Json::from("failed"))]
                                    .contains(&field::<ReadFailure<E>>(
                                        Some(&value),
                                        "ownerPhase",
                                    )?));
                    for target in &found.set {
                        index
                            .referrers
                            .entry(target.clone())
                            .or_default()
                            .insert(id.clone());
                    }
                    index.documents.insert(
                        id,
                        Head {
                            revision,
                            references: found.set,
                            unresolved,
                        },
                    );
                }
            }
            if cooperative && offset % 32 == 31 {
                tokio::task::yield_now().await;
            }
        }
        for id in index
            .documents
            .keys()
            .filter(|id| !live.contains(*id))
            .cloned()
            .collect::<Vec<_>>()
        {
            index.forget(&id);
        }
        index.version = Some(version);
        Ok(())
    }
    fn subtree<E>(&self, root: &Json) -> HistoryResult<Vec<JsString>, E>
    where
        Failure: Fn(ivy_store::rusqlite::Error) -> ReadFailure<E>,
    {
        Ok(self.local.rows::<ReadFailure<E>>("WITH RECURSIVE subtree(id) AS (SELECT ? UNION ALL SELECT documents.id FROM documents JOIN subtree ON documents.parent_id=subtree.id) SELECT id FROM subtree", &[Some(root)])?
            .into_iter().map(|row| row[0].string().map(JsString::from).map_err(ReadFailure::from)).collect::<Result<Vec<_>, _>>()?)
    }
    fn active_gate<E>(&self, root: &Json) -> HistoryResult<bool, E>
    where
        Failure: Fn(ivy_store::rusqlite::Error) -> ReadFailure<E>,
    {
        Ok(self
            .local
            .row::<ReadFailure<E>>(
                "SELECT 1 FROM active_gates WHERE operation_id=? LIMIT 1",
                &[Some(root)],
            )?
            .is_some())
    }
    async fn task_active<E>(
        &self,
        id: Option<&Json>,
        revision: Option<&Json>,
    ) -> HistoryResult<bool, E>
    where
        R: ObjectReads<ReadFailure<E>>,
    {
        let mut params = Json::Object(vec![]);
        insert(&mut params, "objectId", id);
        let read = match self.objects.read(Some(params)).await {
            Ok(read) => read,
            Err(error) if ivy_code(&error, &["not_found"]) => return Ok(false),
            Err(error) => return Err(error),
        };
        // content is evaluated before object, including on a foreign contract.
        let content = field::<ReadFailure<E>>(Some(&read), "content")?;
        let task = if field::<ReadFailure<E>>(content, "encoding")? == Some(&Json::from("json")) {
            field::<ReadFailure<E>>(content, "value")?
        } else {
            None
        };
        let object = field::<ReadFailure<E>>(Some(&read), "object")?;
        Ok(
            field::<ReadFailure<E>>(object, "contractKey")? == Some(&Json::from("task-board/task"))
                && field::<ReadFailure<E>>(object, "effectivelyArchived")?
                    != Some(&Json::Bool(true))
                && same(
                    field::<ReadFailure<E>>(object, "currentRevision")?,
                    revision,
                )
                && task.is_some_and(|value| !value.is_null())
                && [Some(&Json::from("todo")), Some(&Json::from("waiting"))].contains(&field::<
                    ReadFailure<E>,
                >(
                    task,
                    "workflowState",
                )?),
        )
    }
    async fn retained_root<E>(&self, root: &Json) -> HistoryResult<bool, E>
    where
        D: HistoryDocuments<ReadFailure<E>>,
        R: ObjectReads<ReadFailure<E>>,
        Parse: Fn(&str) -> HistoryResult<Json, E>,
        Failure: Fn(ivy_store::rusqlite::Error) -> ReadFailure<E>,
    {
        if self.active_gate(root)? {
            return Ok(true);
        }
        if self.local.row::<ReadFailure<E>>("SELECT 1 FROM cleanup_dependencies c JOIN documents d ON d.id=c.operation_id JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE c.root_id=? AND json_extract(r.value_json,'$.phase') NOT IN ('succeeded','failed') LIMIT 1", &[Some(root)])?.is_some() { return Ok(true); }
        let Some(row) = self
            .local
            .row::<ReadFailure<E>>("SELECT key FROM documents WHERE id=?", &[Some(root)])?
        else {
            return Ok(false);
        };
        let key = row[0].string()?;
        if key == "task-board/scheduler-attempt" {
            let attempt = self.local.document::<ReadFailure<E>>(&key, root, false)?;
            let task = field::<ReadFailure<E>>(Some(value::<E>(&attempt)?), "task")?;
            return self
                .task_active(
                    field::<ReadFailure<E>>(task, "objectId")?,
                    field::<ReadFailure<E>>(task, "revision")?,
                )
                .await;
        }
        if key != "task-board/operation" {
            return Ok(false);
        }
        let document = self.local.document::<ReadFailure<E>>(&key, root, false)?;
        let operation = value::<E>(&document)?;
        let phase = field::<ReadFailure<E>>(Some(operation), "phase")?;
        if ![Some(&Json::from("succeeded")), Some(&Json::from("failed"))].contains(&phase) {
            return Ok(true);
        }
        if optional::<E>(
            field::<ReadFailure<E>>(Some(operation), "error")?,
            "execution",
        )? == Some(&Json::from("unknown"))
        {
            return Ok(true);
        }
        let request = field::<ReadFailure<E>>(Some(operation), "request")?;
        let handoff = field::<ReadFailure<E>>(Some(operation), "handoffRun")?;
        if phase == Some(&Json::from("succeeded"))
            && handoff.is_some_and(|value| !value.is_null())
            && has::<ReadFailure<E>>(request, "taskId")?
        {
            let address = field::<ReadFailure<E>>(request, "taskId")?;
            let task = match self
                .documents
                .history_document("task-board/task", address, true, true)
                .await
            {
                Ok(task) => Some(task),
                Err(error) if ivy_code(&error, &["not_found"]) => None,
                Err(error) => return Err(error),
            };
            let task = optional::<E>(task.as_ref(), "value")?;
            let claim = optional::<E>(task, "claim")?;
            let run = optional::<E>(claim, "run")?;
            let handoff_id = field::<ReadFailure<E>>(handoff, "objectId")?;
            if same(optional::<E>(run, "objectId")?, handoff_id) {
                return Ok(true);
            }
            let issue = optional::<E>(task, "completionIssue")?;
            let issue_id = match issue {
                None | Some(Json::Null) => None,
                _ => field::<ReadFailure<E>>(field::<ReadFailure<E>>(issue, "run")?, "objectId")?,
            };
            if same(issue_id, handoff_id) {
                return Ok(true);
            }
        }
        let actor = field::<ReadFailure<E>>(Some(operation), "actor")?;
        if field::<ReadFailure<E>>(actor, "source")? == Some(&Json::from("scheduler"))
            && has::<ReadFailure<E>>(request, "taskId")?
            && has::<ReadFailure<E>>(request, "expectedRevision")?
            && self
                .task_active(
                    field::<ReadFailure<E>>(request, "taskId")?,
                    field::<ReadFailure<E>>(request, "expectedRevision")?,
                )
                .await?
        {
            return Ok(true);
        }
        for write in iterable::<E>(
            field::<ReadFailure<E>>(Some(operation), "writes")?,
            "operation.writes",
        )? {
            if field::<ReadFailure<E>>(Some(&write), "contractKey")?
                != Some(&Json::from("task-board/delivery"))
            {
                continue;
            }
            let outcome = field::<ReadFailure<E>>(Some(&write), "outcome")?;
            if !outcome.is_some_and(|value| !value.is_null()) {
                continue;
            }
            let mut params = Json::Object(vec![]);
            insert(
                &mut params,
                "objectId",
                field::<ReadFailure<E>>(outcome, "objectId")?,
            );
            let delivery = match self.objects.read(Some(params)).await {
                Ok(delivery) => delivery,
                Err(error) if ivy_code(&error, &["not_found"]) => continue,
                Err(error) => return Err(error),
            };
            if field::<ReadFailure<E>>(
                field::<ReadFailure<E>>(Some(&delivery), "object")?,
                "contractKey",
            )? != Some(&Json::from("task-board/delivery"))
            {
                return Ok(true);
            }
            let content = field::<ReadFailure<E>>(Some(&delivery), "content")?;
            if field::<ReadFailure<E>>(content, "encoding")? != Some(&Json::from("json"))
                || field::<ReadFailure<E>>(field::<ReadFailure<E>>(content, "value")?, "state")?
                    != Some(&Json::from("confirmed"))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    async fn recovery_run<E>(&self, pin: &Json) -> HistoryResult<Json, E>
    where
        D: HistoryDocuments<ReadFailure<E>>,
    {
        match self
            .documents
            .history_document("task-board/run", Some(pin), true, false)
            .await
        {
            Ok(run) => Ok(run),
            Err(error) if ivy_code(&error, &["not_found", "revision_pruned"]) => {
                let id = field::<ReadFailure<E>>(Some(pin), "objectId")?;
                let current = self
                    .documents
                    .history_document("task-board/run", id, true, false)
                    .await?;
                let current_revision = field::<ReadFailure<E>>(
                    field::<ReadFailure<E>>(Some(&current), "pin")?,
                    "revision",
                )?;
                let revision = field::<ReadFailure<E>>(Some(pin), "revision")?;
                let run = field::<ReadFailure<E>>(Some(&current), "value")?;
                // ObjectPin revisions have already passed the actual read contract.
                if !matches!((current_revision, revision), (Some(Json::Number(a)), Some(Json::Number(b))) if a >= b)
                    || ![Some(&Json::from("completed")), Some(&Json::from("failed"))]
                        .contains(&field::<ReadFailure<E>>(run, "phase")?)
                    || ![Some(&Json::from("succeeded")), Some(&Json::from("failed"))].contains(
                        &field::<ReadFailure<E>>(
                            field::<ReadFailure<E>>(run, "externalOutcome")?,
                            "state",
                        )?,
                    )
                {
                    return Err(IvyError::new("task_board_recovery_evidence_missing", "A pruned recovery revision needs the same original Run with a confirmed terminal owner outcome.").into());
                }
                Ok(current)
            }
            Err(error) => Err(error),
        }
    }
    pub async fn collect<E>(
        &self,
        reserved_bytes: u64,
        reserved_rows: u64,
        protected_id: Option<&JsString>,
        incoming: Option<&Json>,
    ) -> HistoryResult<(), E>
    where
        D: HistoryDocuments<ReadFailure<E>>,
        R: ObjectReads<ReadFailure<E>> + HistoryQueries<ReadFailure<E>>,
        Parse: Fn(&str) -> HistoryResult<Json, E>,
        Failure: Fn(ivy_store::rusqlite::Error) -> ReadFailure<E>,
    {
        if !self.full(reserved_bytes, reserved_rows, false)? {
            return Ok(());
        }
        let _collection = self.collections.collection.lock().await;
        if !self.full(reserved_bytes, reserved_rows, false)? {
            return Ok(());
        }
        let mut incoming_ids = Ids::default();
        if let Some(id) = protected_id.filter(|id| !id.0.is_empty()) {
            incoming_ids.add(id.clone());
        }
        if let Some(incoming) = incoming {
            references(incoming, &mut incoming_ids);
        }
        let root = self.local.journal.root_object_id.clone().map(Json::String);
        let parent = root
            .as_ref()
            .map(|root| {
                Json::object([
                    ("field", "object.parentId".into()),
                    ("op", "eq".into()),
                    ("value", root.clone()),
                ])
            })
            .unwrap_or_else(|| {
                Json::object([("field", "object.parentId".into()), ("op", "isNull".into())])
            });
        let mut cursor = None;
        loop {
            let mut params = Json::object([
                ("contractKey", "task-board/task".into()),
                ("includeArchived", true.into()),
                (
                    "where",
                    Json::object([
                        ("op", "and".into()),
                        (
                            "args",
                            Json::Array(vec![
                                Json::object([
                                    ("field", "data:/completionIssue/code".into()),
                                    ("op", "in".into()),
                                    (
                                        "value",
                                        Json::Array(
                                            [
                                                "task_board_handoff_missing",
                                                "task_board_recovery_context_unavailable",
                                                "task_board_recovery_evidence_missing",
                                                "task_board_no_progress",
                                            ]
                                            .into_iter()
                                            .map(Json::from)
                                            .collect(),
                                        ),
                                    ),
                                ]),
                                Json::object([
                                    ("field", "object.ownerObjectId".into()),
                                    ("op", "isNull".into()),
                                ]),
                                parent.clone(),
                            ]),
                        ),
                    ]),
                ),
                ("limit", Json::Number(100.0)),
            ]);
            insert(&mut params, "cursor", cursor.as_ref());
            let page = self.objects.query(params).await?;
            for item in iterable::<E>(field::<ReadFailure<E>>(Some(&page), "items")?, "page.items")?
            {
                let id = field::<ReadFailure<E>>(Some(&item), "objectId")?;
                let task = self
                    .documents
                    .history_document("task-board/task", id, true, false)
                    .await?;
                let pin = optional::<E>(
                    field::<ReadFailure<E>>(
                        field::<ReadFailure<E>>(Some(&task), "value")?,
                        "completionIssue",
                    )?,
                    "run",
                )?;
                let Some(pin) = pin.filter(|value| !value.is_null()) else {
                    continue;
                };
                let run = match self.recovery_run(pin).await {
                    Ok(run) => run,
                    Err(error) if ivy_code(&error, &["not_found"]) => continue,
                    Err(error) => return Err(error),
                };
                let mut pending = Ids::default();
                if let Some(value) = field::<ReadFailure<E>>(Some(&run), "value")? {
                    references(value, &mut pending);
                }
                let mut offset = 0;
                while offset < pending.ordered.len() {
                    let id = pending.ordered[offset].clone();
                    offset += 1;
                    if incoming_ids.set.contains(&id) {
                        continue;
                    }
                    incoming_ids.add(id.clone());
                    if let Some(row) = self.local.row::<ReadFailure<E>>("SELECT value_json FROM revisions WHERE id=? ORDER BY revision DESC LIMIT 1", &[Some(&Json::String(id))])? {
                        references(&(self.local.parse)(&row[0].string()?)?, &mut pending);
                    }
                    if pending.set.len() > 10000 {
                        return Err(IvyError::new(
                            "task_board_evidence_mismatch",
                            "Recovery evidence exceeds its bounded lineage.",
                        )
                        .into());
                    }
                }
            }
            cursor = field::<ReadFailure<E>>(Some(&page), "nextCursor")?
                .filter(|value| !value.is_null())
                .cloned();
            if cursor.is_none() {
                break;
            }
        }
        let mut index = Index::default();
        self.refresh(&mut index, true).await?;
        let due = self.local.rows::<ReadFailure<E>>(
            "SELECT root_id FROM cleanup_roots ORDER BY delete_after,rowid",
            &[],
        )?;
        for (offset, row) in due.iter().enumerate() {
            if !self.full(reserved_bytes, reserved_rows, true)? {
                break;
            }
            if offset % 32 == 31 {
                tokio::task::yield_now().await;
            }
            let root = Json::from(row[0].string()?);
            let before = self.local.row::<ReadFailure<E>>(
                "SELECT current_revision,key,name FROM documents WHERE id=?",
                &[Some(&root)],
            )?;
            let mut ids = self.subtree(&root)?;
            self.refresh(&mut index, false).await?;
            if index.protected(&ids) || self.retained_root(&root).await? {
                continue;
            }
            self.local
                .journal
                .connection
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(&self.local.sqlite_failure)?;
            let result: HistoryResult<bool, E> = async {
                self.refresh(&mut index, false).await?;
                ids = self.subtree(&root)?;
                let after = self.local.row::<ReadFailure<E>>(
                    "SELECT current_revision FROM documents WHERE id=?",
                    &[Some(&root)],
                )?;
                if before.as_ref().map(|row| row[0].number()).transpose()?
                    != after.as_ref().map(|row| row[0].number()).transpose()?
                    || ids.iter().any(|id| incoming_ids.set.contains(id))
                    || self.active_gate(&root)?
                    || index.protected(&ids)
                {
                    return Ok(false);
                }
                if before
                    .as_ref()
                    .map(|row| row[1].string())
                    .transpose()?
                    .as_deref()
                    == Some("task-board/operation")
                {
                    let operation = self.local.document::<ReadFailure<E>>(
                        "task-board/operation",
                        &root,
                        false,
                    )?;
                    let operation = value::<E>(&operation)?;
                    let caller = field::<ReadFailure<E>>(Some(operation), "callerPrincipalId")?;
                    let operation_id = field::<ReadFailure<E>>(Some(operation), "operationId")?;
                    self.local.execute::<ReadFailure<E>>(
                        "INSERT OR IGNORE INTO retired_operations VALUES(?,?,?,?)",
                        &[
                            caller,
                            operation_id,
                            field::<ReadFailure<E>>(Some(operation), "requestHash")?,
                            Some(&Json::from(before.as_ref().unwrap()[2].string()?)),
                        ],
                    )?;
                    let receipt = canonical(
                        &operation_receipt::<ReadFailure<E>>(operation)?,
                        MANAGEMENT_FRAME_BYTES,
                    )?;
                    self.local.execute::<ReadFailure<E>>(
                        "INSERT OR IGNORE INTO retired_operation_receipts VALUES(?,?,?)",
                        &[caller, operation_id, Some(&Json::from(receipt))],
                    )?;
                }
                for id in &ids {
                    let id = Json::String(id.clone());
                    for sql in [
                        "DELETE FROM active_gates WHERE gate_id=?",
                        "DELETE FROM operation_gates WHERE operation_id=?",
                    ] {
                        self.local.execute::<ReadFailure<E>>(sql, &[Some(&id)])?;
                    }
                    for sql in [
                        "DELETE FROM cleanup_dependencies WHERE root_id=? OR operation_id=?",
                        "DELETE FROM blobs WHERE id=? OR parent_id=?",
                    ] {
                        self.local
                            .execute::<ReadFailure<E>>(sql, &[Some(&id), Some(&id)])?;
                    }
                    for sql in [
                        "DELETE FROM revisions WHERE id=?",
                        "DELETE FROM documents WHERE id=?",
                    ] {
                        self.local.execute::<ReadFailure<E>>(sql, &[Some(&id)])?;
                    }
                }
                self.local.execute::<ReadFailure<E>>(
                    "DELETE FROM cleanup_roots WHERE root_id=?",
                    &[Some(&root)],
                )?;
                index.version = Some(self.version()?);
                self.local
                    .journal
                    .connection
                    .execute_batch("COMMIT")
                    .map_err(&self.local.sqlite_failure)?;
                for id in &ids {
                    index.forget(id);
                }
                Ok(true)
            }
            .await;
            match result {
                Ok(true) => {}
                result => {
                    self.local
                        .journal
                        .connection
                        .execute_batch("ROLLBACK")
                        .map_err(&self.local.sqlite_failure)?;
                    result?;
                }
            }
        }
        if !due.is_empty() {
            self.local
                .journal
                .connection
                .execute_batch("PRAGMA incremental_vacuum(256); PRAGMA wal_checkpoint(PASSIVE)")
                .map_err(&self.local.sqlite_failure)?;
        }
        Ok(())
    }
}
