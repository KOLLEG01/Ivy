//! Keep legacy Run sources alive without changing accepted workflow records.
use crate::{
    journal_store::reference_writes::run_source_task,
    local_journal::require,
    maintenance::{MaintenanceStore, items, pin, text},
};
use ivy_contracts::{IvyJson as Json, IvyResult, hash::hash_json};
use ivy_sdk::rpc::RequestOptions;

const KEY: &str = "task-board/revision-protection";
const PREFIX: &str = "Run source ";

fn ledger_name(run: &Json) -> IvyResult<String> {
    Ok(format!("{PREFIX}{}", hash_json(run)?))
}
fn same(left: &Json, right: &Json) -> IvyResult<bool> {
    Ok(hash_json(left)? == hash_json(right)?)
}

struct Protection<'a, S>(&'a S);
impl<S: MaintenanceStore> Protection<'_, S> {
    async fn request(&self, method: &str, value: Json) -> IvyResult<Json> {
        self.0
            .client()
            .request(method, value, RequestOptions::default())
            .await
    }
    fn scope(&self) -> Json {
        match self.0.root() {
            Some(root) => Json::object([
                ("field", "object.parentId".into()),
                ("op", "eq".into()),
                ("value", Json::String(root)),
            ]),
            None => Json::object([("field", "object.parentId".into()), ("op", "isNull".into())]),
        }
    }
    async fn page(&self, key: &str, predicate: Json, cursor: &Json) -> IvyResult<Json> {
        let mut query = Json::object([
            ("contractKey", key.into()),
            ("includeArchived", true.into()),
            (
                "where",
                Json::object([
                    ("op", "and".into()),
                    ("args", Json::Array(vec![self.scope(), predicate])),
                ]),
            ),
            (
                "orderBy",
                Json::Array(vec![Json::object([
                    ("field", "object.id".into()),
                    ("direction", "asc".into()),
                ])]),
            ),
            ("limit", Json::Number(100.0)),
        ]);
        if !cursor.is_null() {
            query.insert("cursor", cursor.clone());
        }
        self.request("objects.query", query).await
    }
    async fn read(&self, key: &str, address: &Json) -> IvyResult<Json> {
        let params = if address.as_js_string().is_some() {
            Json::object([("objectId", address.clone())])
        } else {
            address.clone()
        };
        let record = self.request("objects.read", params.clone()).await?;
        let object = record.required("object")?;
        let revision = record.required("revision")?;
        require(
            object.get("contractKey") == Some(&key.into())
                && object.get("id") == params.get("objectId")
                && revision.get("objectId") == params.get("objectId")
                && params
                    .get("revision")
                    .is_none_or(|pin| revision.get("revision") == Some(pin))
                && object.get("parentId")
                    == Some(&self.0.root().map(Json::String).unwrap_or(Json::Null))
                && object.get("ownerObjectId").is_some_and(Json::is_null)
                && revision.get("mediaType") == Some(&"application/json".into())
                && record.required("content")?.get("encoding") == Some(&"json".into()),
            "task_board_scope_mismatch",
            "Run source protection requires exact records in this workspace.",
        )?;
        let value = Self::value(&record)?;
        self.0
            .contracts()
            .validate_record(key, text(revision, "contractVersion")?, value)?;
        if key == "task-board/run" {
            require(
                value.get("taskId") == value.required("originalRequest")?.get("taskId"),
                "task_board_evidence_mismatch",
                "A Run must retain its original Task identity.",
            )?;
        }
        Ok(record)
    }
    fn value(record: &Json) -> IvyResult<&Json> {
        record.required("content")?.required("value")
    }
    fn self_protected(run: &Json, source: &Json) -> IvyResult<bool> {
        run.required("revision")?
            .required("references")?
            .get("source-task")
            .map(|saved| same(saved, source))
            .transpose()
            .map(|saved| saved.unwrap_or(false))
    }
    async fn named(&self, name: &str) -> IvyResult<Option<Json>> {
        let page = self
            .page(
                KEY,
                Json::object([
                    ("field", "object.name".into()),
                    ("op", "eq".into()),
                    ("value", name.into()),
                ]),
                &Json::Null,
            )
            .await?;
        let found = items(&page, "items")?;
        require(
            found.len() <= 1 && page.required("nextCursor")?.is_null(),
            "task_board_identity_conflict",
            "A Run source ledger must have one identity.",
        )?;
        match found.first() {
            Some(row) => self.read(KEY, row.required("objectId")?).await.map(Some),
            None => Ok(None),
        }
    }
    fn source(ledger: &Json) -> IvyResult<Option<&Json>> {
        let sources = items(Self::value(ledger)?, "sources")?;
        require(
            sources.len() <= 1,
            "task_board_identity_conflict",
            "A named Run source ledger must retain one original attempt.",
        )?;
        let Some(source) = sources.first() else {
            require(
                ledger.required("revision")?.required("references")? == &Json::Object(vec![]),
                "task_board_evidence_mismatch",
                "An empty Run source ledger cannot hide references.",
            )?;
            return Ok(None);
        };
        require(
            text(ledger.required("object")?, "name")?.to_utf8()?
                == ledger_name(source.required("run")?.required("objectId")?)?
                && same(
                    ledger.required("revision")?.required("references")?,
                    &Json::object([
                        ("run", source.required("run")?.clone()),
                        ("source", source.required("source")?.clone()),
                    ]),
                )?,
            "task_board_evidence_mismatch",
            "A Run source ledger must retain its exact original pins.",
        )?;
        Ok(Some(source))
    }
    async fn save(
        &self,
        name: &str,
        previous: Option<&Json>,
        source: Option<Json>,
    ) -> IvyResult<()> {
        let references = match &source {
            Some(pair) => Json::object([
                ("run", pair.required("run")?.clone()),
                ("source", pair.required("source")?.clone()),
            ]),
            None => Json::Object(vec![]),
        };
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("sources", Json::Array(source.into_iter().collect())),
        ]);
        let version = self.0.contracts().version(KEY)?;
        self.0
            .contracts()
            .validate_record(KEY, &version.clone().into(), &value)?;
        let identity = Json::object([
            (
                "root",
                self.0.root().map(Json::String).unwrap_or(Json::Null),
            ),
            ("name", name.into()),
            ("value", value.clone()),
            (
                "previous",
                previous.map(pin).transpose()?.unwrap_or(Json::Null),
            ),
        ]);
        let mutation = self
            .0
            .mutation(&format!("run-source:{}", hash_json(&identity)?))
            .await?;
        let mut params = Json::object([
            ("contractVersion", version.into()),
            ("mutationId", Json::String(mutation)),
            (
                "content",
                Json::object([("encoding", "json".into()), ("value", value)]),
            ),
            ("references", references),
        ]);
        if let Some(previous) = previous {
            params.insert(
                "objectId",
                previous.required("object")?.required("id")?.clone(),
            );
            params.insert(
                "expectedRevision",
                previous.required("revision")?.required("revision")?.clone(),
            );
            params.insert(
                "expectedArchived",
                previous
                    .required("object")?
                    .required("effectivelyArchived")?
                    .clone(),
            );
        } else {
            params.insert(
                "create",
                Json::object([
                    ("contractKey", KEY.into()),
                    ("name", name.into()),
                    (
                        "parentId",
                        self.0.root().map(Json::String).unwrap_or(Json::Null),
                    ),
                    ("ownerObjectId", Json::Null),
                ]),
            );
        }
        self.request("objects.write", params).await?;
        Ok(())
    }
    async fn retain(&self, run: &Json, source: &Json) -> IvyResult<bool> {
        let name = ledger_name(run.required("object")?.required("id")?)?;
        let previous = self.named(&name).await?;
        if let Some(saved) = previous.as_ref().map(Self::source).transpose()?.flatten() {
            require(
                same(saved.required("source")?, source)?,
                "task_board_evidence_mismatch",
                "An accepted attempt cannot change its original source Task.",
            )?;
            return Ok(true);
        }
        // The reference write also fences pruning between this exact read and
        // publication. A lost reply is reconciled by the same ledger name.
        match self.read("task-board/task", source).await {
            Ok(_) => (),
            Err(error) if matches!(error.code.as_str(), "revision_pruned" | "not_found") => {
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
        self.save(
            &name,
            previous.as_ref(),
            Some(Json::object([
                ("run", pin(run)?),
                ("source", source.clone()),
            ])),
        )
        .await?;
        Ok(true)
    }
    async fn release(&self, ledger: &Json) -> IvyResult<()> {
        let Some(saved) = Self::source(ledger)? else {
            return Ok(());
        };
        let run = self
            .read(
                "task-board/run",
                saved.required("run")?.required("objectId")?,
            )
            .await?;
        let source = run_source_task(Self::value(&run)?)?;
        if let Some(source) = source {
            require(
                same(saved.required("source")?, &source)?,
                "task_board_evidence_mismatch",
                "An accepted attempt cannot change its original source Task.",
            )?;
            if !Self::self_protected(&run, &source)? {
                return Ok(());
            }
        }
        self.save(
            &text(ledger.required("object")?, "name")?.to_utf8()?,
            Some(ledger),
            None,
        )
        .await
    }
}

/// A bounded page and one original document at a time. Future writers pin their
/// own source; only legacy unfinished Runs need these small current-only ledgers.
/// Missing historical bytes remain a diagnostic, never replay authority.
pub async fn protect_run_sources<S: MaintenanceStore>(store: &S) -> IvyResult<usize> {
    // Local-only Runs remain under the journal's graph retention. Hive ledgers
    // protect published primary Runs, not locally staged workflow documents.
    if store.contracts().local("task-board/run")? {
        return Ok(0);
    }
    let protection = Protection(store);
    let mut cursor = Json::Null;
    let mut missing = 0;
    loop {
        let page = protection
            .page(
                "task-board/run",
                Json::object([
                    ("op", "not".into()),
                    (
                        "arg",
                        Json::object([
                            ("field", "data:/phase".into()),
                            ("op", "in".into()),
                            (
                                "value",
                                Json::Array(
                                    ["completed", "failed", "cancelled"]
                                        .map(Json::from)
                                        .to_vec(),
                                ),
                            ),
                        ]),
                    ),
                ]),
                &cursor,
            )
            .await?;
        for row in items(&page, "items")? {
            let run = protection
                .read("task-board/run", row.required("objectId")?)
                .await?;
            if let Some(source) = run_source_task(Protection::<S>::value(&run)?)?
                && !Protection::<S>::self_protected(&run, &source)?
                && !protection.retain(&run, &source).await?
            {
                missing += 1;
            }
        }
        cursor = page.required("nextCursor")?.clone();
        if cursor.is_null() {
            break;
        }
    }
    cursor = Json::Null;
    loop {
        let page = protection
            .page(
                KEY,
                Json::object([
                    ("field", "object.name".into()),
                    ("op", "contains".into()),
                    ("value", PREFIX.into()),
                ]),
                &cursor,
            )
            .await?;
        for row in items(&page, "items")? {
            let ledger = protection.read(KEY, row.required("objectId")?).await?;
            if text(ledger.required("object")?, "name")?
                .to_utf8()?
                .starts_with(PREFIX)
            {
                protection.release(&ledger).await?;
            }
        }
        cursor = page.required("nextCursor")?.clone();
        if cursor.is_null() {
            break;
        }
    }
    Ok(missing)
}
