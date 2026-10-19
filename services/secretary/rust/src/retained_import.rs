//! Bounded one-time import. No inbox/native delivery worker is initialized here.
use crate::{
    assignment_import::{
        SubscriptionAssignment, import_event_gap, import_event_subscription,
        import_schedule_progress,
    },
    assignment_scheduler::AssignmentScheduler,
    assignment_schema::assignment_name,
    engine::{SecretaryEngine, random_uuid},
    execution_import::import_execution_delivery,
    follow_up::SecretaryFollowUp,
    follow_up_import::{import_context_link, import_pending_context},
    import_rpc,
    import_state::{self, RUN},
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Document, Journal},
    store::{Record, Store},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::parse,
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestOptions, RpcClient},
};
use ivy_store::{
    rusqlite::{Connection, OptionalExtension, params},
    sqlite,
};
use std::{cell::RefCell, sync::Arc};

struct ImportClock(i64);
impl Clock for ImportClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
pub struct ImportContext<'a> {
    pub journal: &'a Journal,
    pub source: &'a Connection,
    pub settings: &'a IvyJson,
    pub service_node_id: &'a JsString,
    pub client: Arc<dyn RpcClient>,
    pub retained: &'a CheckedNativeContracts,
    pub cancellation: RequestOptions,
    pub page_size: usize,
}
pub struct ImportProgress {
    pub stage: String,
    pub complete: bool,
    pub advanced: bool,
}
fn failure(message: &str) -> IvyError {
    IvyError::new("secretary_import_incomplete", message)
}
fn pin(document: &Document) -> IvyJson {
    IvyJson::object([
        ("objectId", document.pin.object_id.clone().into()),
        ("revision", IvyJson::Number(document.pin.revision as f64)),
    ])
}
fn map_prefix(state: &IvyJson) -> IvyResult<String> {
    Ok(format!(
        "secretary/import/map/{}/",
        &hash_json(state.required("operationId")?)?[7..]
    ))
}
fn source_local(source: &Connection, id: &str) -> IvyResult<IvyJson> {
    let row: Option<(String, String, i64, String)> = source
        .query_row(
            "SELECT key,name,current_revision,value_json FROM technical_documents WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite::error)?;
    let (key, name, revision, json) = row.ok_or_else(|| {
        failure("The original local import source is not present in the sealed backup.")
    })?;
    Ok(IvyJson::object([
        ("key", key.into()),
        ("name", name.into()),
        (
            "pin",
            IvyJson::object([
                ("objectId", id.into()),
                ("revision", IvyJson::Number(revision as f64)),
            ]),
        ),
        ("rawHash", digest(json.as_bytes()).into()),
    ]))
}
fn mapped(ctx: &ImportContext<'_>, state: &IvyJson, id: &str, target: IvyJson) -> IvyResult<()> {
    let key = format!("{}{}", map_prefix(state)?, id);
    let source = source_local(ctx.source, id)?;
    import_state::save(
        ctx.journal,
        &key,
        &IvyJson::object([("source", source), ("target", target)]),
    )
}
fn local_page(
    journal: &Journal,
    key: Option<&str>,
    cursor: Option<&str>,
    limit: usize,
) -> IvyResult<(Vec<Document>, bool)> {
    let ids=journal.db.prepare("SELECT id FROM technical_documents WHERE (? IS NULL OR key=?) AND (? IS NULL OR id>?) ORDER BY id LIMIT ?").map_err(sqlite::error)?.query_map(params![key,key,cursor,cursor,limit as i64+1],|r|r.get::<_,String>(0)).map_err(sqlite::error)?.collect::<Result<Vec<_>,_>>().map_err(sqlite::error)?;
    let more = ids.len() > limit;
    let mut rows = vec![];
    for id in ids.into_iter().take(limit) {
        let key: String = journal
            .db
            .query_row(
                "SELECT key FROM technical_documents WHERE id=?",
                [&id],
                |r| r.get(0),
            )
            .map_err(sqlite::error)?;
        rows.push(journal.read(&key, &id, None, true)?);
    }
    Ok((rows, more))
}
fn advance(state: &mut IvyJson, stage: &str, cursor: Option<IvyJson>) {
    state.insert("stage", stage.into());
    state.insert("cursor", cursor.unwrap_or(IvyJson::Null));
}
fn cursor(state: &IvyJson) -> IvyResult<Option<String>> {
    state
        .get("cursor")
        .filter(|v| !v.is_null())
        .map(IvyJson::as_str)
        .transpose()
}
async fn query(
    ctx: &ImportContext<'_>,
    client: &dyn RpcClient,
    key: Record,
    cursor: Option<&str>,
) -> IvyResult<IvyJson> {
    let mut request = IvyJson::object([
        ("contractKey", key.key().into()),
        ("includeArchived", (key == Record::Assignment).into()),
        ("limit", IvyJson::Number(ctx.page_size as f64)),
        (
            "where",
            IvyJson::object([
                ("op", "eq".into()),
                ("field", "object.parentId".into()),
                (
                    "value",
                    ctx.settings
                        .required("identity")?
                        .required("scope")?
                        .required("rootObjectId")?
                        .clone(),
                ),
            ]),
        ),
        (
            "orderBy",
            IvyJson::Array(vec![IvyJson::object([
                ("field", "object.id".into()),
                ("direction", "asc".into()),
            ])]),
        ),
        (
            "select",
            IvyJson::Array(
                [
                    "object.parentId",
                    "object.effectivelyArchived",
                    "object.archivedAt",
                    "object.name",
                    "data:/assignmentId",
                    "data:/executionId",
                ]
                .into_iter()
                .map(IvyJson::from)
                .collect(),
            ),
        ),
    ]);
    if let Some(cursor) = cursor {
        request.insert("cursor", cursor.into());
    }
    let page = client
        .request("objects.query", request, ctx.cancellation.clone())
        .await?;
    if page
        .required("items")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .len()
        > ctx.page_size
    {
        return Err(failure("The import query exceeded its bounded page."));
    }
    if let Some(next) = page.get("nextCursor").filter(|v| !v.is_null()) {
        if next.as_str()?.as_str() == cursor.unwrap_or("") {
            return Err(failure("The import query cursor did not advance."));
        }
    }
    Ok(page)
}
async fn page(
    ctx: &ImportContext<'_>,
    state: &mut IvyJson,
    client: Arc<dyn RpcClient>,
) -> IvyResult<bool> {
    let journal = ctx.journal;
    let clock = ImportClock(
        state
            .required("at")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    );
    let root = ctx
        .settings
        .required("identity")?
        .required("scope")?
        .required("rootObjectId")?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)?;
    let store = Store {
        journal,
        client: client.clone(),
        root,
    };
    let evidence = InboxEvidence {
        store: &store,
        settings: ctx.settings,
        service_node_id: ctx.service_node_id,
        cancellation: ctx.cancellation.clone(),
    };
    evidence.verify_owner().await?;
    let tools = Arc::new(ivy_sdk::tools::ToolClients::new(client.clone()));
    let scheduler = AssignmentScheduler {
        evidence: &evidence,
        tools: tools.clone(),
        clock: &clock,
        fallback_execution: None,
    };
    let stage = state.required("stage")?.as_str()?;
    let after = cursor(state)?;
    match stage.as_str() {
        "follow-up" => {
            let issues = RefCell::new(RecoveryScan::default());
            let engine = SecretaryEngine::new(
                &evidence,
                &clock,
                &clock,
                random_uuid,
                tools.as_ref(),
                &issues,
            );
            let worker = SecretaryFollowUp::new(&engine, tools.clone(), ctx.retained);
            let (rows, more) = local_page(
                journal,
                Some(crate::follow_up::FOLLOW_UP_KEY),
                after.as_deref(),
                ctx.page_size,
            )?;
            for row in &rows {
                evidence.verify_owner().await?;
                let link =
                    import_context_link(journal, &row.name)?.ok_or_else(IvyError::internal)?;
                mapped(
                    ctx,
                    state,
                    &row.pin.object_id,
                    IvyJson::object([
                        ("kind", "context-link".into()),
                        ("pin", pin(&link.after)),
                        (
                            "contextObjectId",
                            link.after.value.required("contextObjectId")?.clone(),
                        ),
                    ]),
                )?;
                import_pending_context(&worker, &row.name).await?;
                if let Some(context) = link.context {
                    let current = journal.read(
                        crate::follow_up::CONTEXT_KEY,
                        &context.pin.object_id,
                        None,
                        true,
                    )?;
                    mapped(
                        ctx,
                        state,
                        &context.pin.object_id,
                        IvyJson::object([
                            ("kind", "follow-up-context".into()),
                            ("pin", pin(&current)),
                            ("hash", hash_json(&current.value)?.into()),
                        ]),
                    )?;
                }
            }
            if more {
                advance(
                    state,
                    "follow-up",
                    Some(rows.last().unwrap().pin.object_id.clone().into()),
                );
            } else {
                advance(state, "acknowledgements", None);
            }
        }
        "acknowledgements" => {
            let imported =
                journal.import_capture_acknowledgements(after.as_deref(), ctx.page_size)?;
            // The original checkpoint source remains readable in the sealed backup and
            // in unchanged current local revisions; acknowledgement pages are idempotent.
            if let Some(next) = imported.next_cursor {
                advance(state, "acknowledgements", Some(next.into()));
            } else {
                advance(state, "assignments", None);
            }
        }
        "assignments" | "executions" => {
            let assignments = stage == "assignments";
            let record = if assignments {
                Record::Assignment
            } else {
                Record::Execution
            };
            let result = query(ctx, client.as_ref(), record, after.as_deref()).await?;
            for row in result
                .required("items")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                evidence.verify_owner().await?;
                let values = row.required("values")?;
                if values.get("object.parentId") != Some(&IvyJson::String(store.root.clone())) {
                    return Err(failure("The import query returned a foreign root record."));
                }
                if assignments {
                    if values.get("object.effectivelyArchived") == Some(&true.into()) {
                        if let Some(imported) = import_event_subscription(
                            &scheduler,
                            SubscriptionAssignment::Archived(row),
                            &mut RecoveryScan::default(),
                        )
                        .await?
                        {
                            if !imported.completed {
                                return Ok(false);
                            }
                            mapped(
                                ctx,
                                state,
                                &imported.source.pin.object_id,
                                IvyJson::object([
                                    ("kind", "archived-subscription".into()),
                                    ("receipt", imported.after.unwrap_or(IvyJson::Null)),
                                ]),
                            )?;
                        }
                        continue;
                    }
                    let selected = store
                        .read(Record::Assignment, row.required("objectId")?)
                        .await?;
                    let id = selected
                        .required("value")?
                        .required("assignmentId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?;
                    if values.get("data:/assignmentId") != Some(&IvyJson::String(id.clone()))
                        || values.get("object.name") != Some(&assignment_name(id)?.into())
                    {
                        return Err(failure(
                            "The original assignment identity changed during import.",
                        ));
                    }
                    if let Some(imported) =
                        import_schedule_progress(&scheduler, &selected, clock.utc_millis()).await?
                    {
                        mapped(
                            ctx,
                            state,
                            &imported.source.pin.object_id,
                            IvyJson::object([
                                ("kind", "schedule-progress".into()),
                                ("pin", imported.after.required("pin")?.clone()),
                                ("hash", hash_json(imported.after.required("value")?)?.into()),
                            ]),
                        )?;
                    }
                    if let Some(imported) = import_event_gap(
                        &scheduler,
                        &selected,
                        &RefCell::new(RecoveryScan::default()),
                    )
                    .await?
                    {
                        mapped(
                            ctx,
                            state,
                            &imported.before.pin.object_id,
                            IvyJson::object([
                                ("kind", "event-gap".into()),
                                ("pin", pin(&imported.after)),
                                ("hash", hash_json(&imported.after.value)?.into()),
                            ]),
                        )?;
                    } else if let Some(saved) = journal.named(
                        "secretary/event-gap",
                        &String::from_utf16_lossy(&id.0),
                        true,
                    )? {
                        mapped(
                            ctx,
                            state,
                            &saved.pin.object_id,
                            IvyJson::object([
                                ("kind", "current-event-gap".into()),
                                ("pin", pin(&saved)),
                                ("hash", hash_json(&saved.value)?.into()),
                            ]),
                        )?;
                    }
                    if let Some(source) = journal.named(
                        "secretary/event-subscription",
                        &String::from_utf16_lossy(&id.0),
                        true,
                    )? {
                        let configuration=store.named(Record::Configuration,&"configuration".into()).await?.ok_or_else(||failure("The retained subscription needs its original current configuration."))?;
                        if let Some(imported) = import_event_subscription(
                            &scheduler,
                            SubscriptionAssignment::Active {
                                assignment: &selected,
                                configuration: configuration.required("value")?,
                            },
                            &mut RecoveryScan::default(),
                        )
                        .await?
                        {
                            if !imported.completed {
                                return Ok(false);
                            }
                            mapped(
                                ctx,
                                state,
                                &source.pin.object_id,
                                IvyJson::object([
                                    ("kind", "event-subscription".into()),
                                    ("receipt", imported.after.unwrap_or(IvyJson::Null)),
                                ]),
                            )?;
                        }
                    }
                } else {
                    let selected = store
                        .read(Record::Execution, row.required("objectId")?)
                        .await?;
                    let id = selected
                        .required("value")?
                        .required("executionId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?;
                    let value = selected.required("value")?;
                    let assignment = value
                        .required("assignmentSnapshot")?
                        .required("assignmentId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?;
                    let trigger = value
                        .required("trigger")?
                        .required("key")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?;
                    let expected = hash_json(&IvyJson::Array(vec![
                        IvyJson::String(assignment.clone()),
                        IvyJson::String(trigger.clone()),
                    ]))?;
                    if values.get("data:/executionId") != Some(&IvyJson::String(id.clone()))
                        || id != &JsString::from(expected)
                        || values.get("object.name")
                            != Some(
                                &crate::assignment_schema::execution_name(assignment, trigger)?
                                    .into(),
                            )
                    {
                        return Err(failure(
                            "The original execution identity changed during import.",
                        ));
                    }
                    let imported = import_execution_delivery(&evidence, &selected).await?;
                    for mapping in imported.mappings {
                        mapped(
                            ctx,
                            state,
                            &mapping.source.pin.object_id,
                            IvyJson::object([
                                ("kind", mapping.target_field.into()),
                                ("pin", imported.after.required("pin")?.clone()),
                                ("hash", hash_json(imported.after.required("value")?)?.into()),
                            ]),
                        )?;
                    }
                }
            }
            let next = result.get("nextCursor").filter(|v| !v.is_null()).cloned();
            if let Some(next) = next {
                advance(state, &stage, Some(next));
            } else {
                advance(
                    state,
                    if assignments { "executions" } else { "writes" },
                    None,
                );
            }
        }
        "writes" => {
            let pattern = format!("{}%", import_rpc::prefix(state)?);
            let rows=journal.db.prepare("SELECT key,value FROM journal_state WHERE key LIKE ? AND (? IS NULL OR key>?) ORDER BY key LIMIT ?").map_err(sqlite::error)?.query_map(params![pattern,after,after,ctx.page_size as i64+1],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(sqlite::error)?.collect::<Result<Vec<_>,_>>().map_err(sqlite::error)?;
            let more = rows.len() > ctx.page_size;
            for (_, json) in rows.iter().take(ctx.page_size) {
                let receipt = parse(json, 16 * 1024 * 1024)?;
                if receipt.get("response").is_none_or(IvyJson::is_null) {
                    evidence.verify_owner().await?;
                    client
                        .request(
                            "objects.write",
                            receipt.required("params")?.clone(),
                            ctx.cancellation.clone(),
                        )
                        .await?;
                }
            }
            if more {
                advance(
                    state,
                    "writes",
                    Some(rows[ctx.page_size - 1].0.clone().into()),
                );
            } else {
                advance(state, "verify-local", None);
            }
        }
        "verify-local" => {
            let (rows, more) = local_page(journal, None, after.as_deref(), ctx.page_size)?;
            for row in &rows {
                let key: String = journal
                    .db
                    .query_row(
                        "SELECT key FROM technical_documents WHERE id=?",
                        [&row.pin.object_id],
                        |r| r.get(0),
                    )
                    .map_err(sqlite::error)?;
                let needs_map = matches!(
                    key.as_str(),
                    "secretary/follow-up"
                        | "secretary/schedule-cursor"
                        | "secretary/event-subscription"
                        | "secretary/event-gap"
                        | "secretary/assignment-handoff"
                        | "secretary/assignment-voice"
                );
                if needs_map
                    && import_state::read(
                        journal,
                        &format!("{}{}", map_prefix(state)?, row.pin.object_id),
                    )?
                    .is_none()
                {
                    return Err(failure(
                        "A retained legacy record has no verified target mapping; its original history remains intact.",
                    ));
                }
                if key == crate::follow_up::CONTEXT_KEY
                    && row.value.get("threadId") == Some(&"".into())
                    && row.value.get("start").is_none_or(IvyJson::is_null)
                {
                    return Err(failure(
                        "A retained pending context has no original admitted start mapping.",
                    ));
                }
            }
            if more {
                advance(
                    state,
                    "verify-local",
                    Some(rows.last().unwrap().pin.object_id.clone().into()),
                );
            } else {
                evidence.verify_owner().await?;
                let transaction = journal.db.unchecked_transaction().map_err(sqlite::error)?;
                advance(state, "done", None);
                import_state::save(journal, RUN, state)?;
                import_state::publish(journal, state)?;
                transaction.commit().map_err(sqlite::error)?;
                return Ok(true);
            }
        }
        "done" => {}
        _ => return Err(failure("The retained import stage is invalid.")),
    }
    import_state::save(journal, RUN, state)?;
    Ok(true)
}
/// One bounded page; the caller retains the exclusive journal and immutable
/// backup connection. Admission is serviced on this same local owner while an
/// RPC future waits, preserving SQLite's established non-Send architecture.
pub async fn step(ctx: &ImportContext<'_>, state: &mut IvyJson) -> IvyResult<ImportProgress> {
    if !(1..=1000).contains(&ctx.page_size) {
        return Err(IvyError::new(
            "invalid_arguments",
            "Select an import page of 1 to 1000 records.",
        ));
    }
    let expected = import_state::read(ctx.journal, RUN)?.ok_or_else(IvyError::internal)?;
    if ivy_contracts::json::canonical(&expected, 16 * 1024 * 1024)?
        != ivy_contracts::json::canonical(state, 16 * 1024 * 1024)?
    {
        return Err(failure("The retained import cursor changed."));
    }
    let frozen_state = state.clone();
    let (client, mut admissions) = import_rpc::client(ctx.client.clone());
    let work = page(ctx, state, client);
    tokio::pin!(work);
    let advanced = loop {
        tokio::select! {result=&mut work=>break result?,request=admissions.recv()=>{let request=request.ok_or_else(IvyError::internal)?;import_rpc::admit(ctx.journal,&frozen_state,request);}}
    };
    // Drop the completed page's mutable borrow before observing its final state.
    drop(work);
    let saved = import_state::read(ctx.journal, RUN)?.ok_or_else(IvyError::internal)?;
    let stage = saved.required("stage")?.as_str()?;
    Ok(ImportProgress {
        complete: stage == "done",
        stage,
        advanced,
    })
}
