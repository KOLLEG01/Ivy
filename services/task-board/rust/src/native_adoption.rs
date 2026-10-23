//! Observe direct work in a saved durable thread without starting native work.
//! Adoption uses the ordinary retained native-update publication program.
use crate::{
    native_driver::NativeDriverConnection,
    native_inspector::management_read,
    native_lifecycle::LifecycleStore,
    native_state::finished_run,
    operations::mutation,
    read_evidence::{ReadEvidence, ReadEvidenceStore, ReadIdentity},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, agent_schema::validate_agent,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    clock::parse_date,
    native_evidence::NativeOwnerIdentity,
    native_owner::NativeOwner,
    rpc::{RequestOptions, RpcClient},
};
use std::cell::RefCell;

fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn eq(v: &Json, k: &str, text: &str) -> bool {
    f(v, k) == &Json::from(text)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn missing_thread(message: &JsString) -> bool {
    let Ok(message) = message.to_utf8() else {
        return false;
    };
    let lower = message.to_ascii_lowercase();
    lower.starts_with("unknown thread:")
        || ["thread ", "session "].iter().any(|start| {
            lower.starts_with(start)
                && (lower.contains(" not found") || lower.contains(" is archived"))
        })
}
pub fn turns_params(thread: &Json) -> Json {
    Json::object([
        ("threadId", thread.clone()),
        ("cursor", Json::Null),
        ("limit", Json::Number(1.0)),
        ("sortDirection", "desc".into()),
        ("itemsView", "notLoaded".into()),
    ])
}
/// Owners may return more metadata than requested. Only a complete bounded
/// page with a unique newest owner timestamp can identify a direct turn.
pub fn latest_turn(observation: &Json) -> IvyResult<Option<&Json>> {
    let page = f(observation, "reply").get("result").ok_or_else(|| {
        IvyError::new(
            "task_board_native_read_failed",
            "Direct turn inspection requires a successful native read.",
        )
    })?;
    let rows = f(page, "data").as_array().ok_or_else(IvyError::internal)?;
    require(
        rows.len() <= 100,
        "task_board_native_page_limit",
        "Direct turn discovery requires a bounded page of turn metadata.",
    )?;
    if rows.len() <= 1 {
        return Ok(rows.first());
    }
    require(
        page.get("nextCursor") == Some(&Json::Null)
            && rows
                .iter()
                .all(|v| f(v, "startedAt").as_f64().is_some_and(f64::is_finite)),
        "task_board_native_page_limit",
        "A multi-turn discovery page must be complete and ordered by owner timestamps.",
    )?;
    let newest = rows
        .iter()
        .filter_map(|v| f(v, "startedAt").as_f64())
        .fold(f64::NEG_INFINITY, f64::max);
    let mut selected = rows
        .iter()
        .filter(|v| f(v, "startedAt").as_f64() == Some(newest));
    let first = selected.next();
    Ok(if selected.next().is_none() {
        first
    } else {
        None
    })
}
pub fn candidate<'a>(
    task: &Json,
    previous: &Json,
    metadata: &Json,
    turns: &'a Json,
) -> IvyResult<Option<&'a Json>> {
    require(
        f(metadata, "reply").get("result").is_some(),
        "task_board_native_read_failed",
        "Direct turn inspection requires successful native reads.",
    )?;
    let thread = f(f(f(metadata, "reply"), "result"), "thread");
    require(
        f(thread, "id") == f(f(task, "primaryResourceRef"), "nativeId")
            && f(thread, "ephemeral").as_bool() == Some(false),
        "task_board_evidence_mismatch",
        "Direct turn inspection must identify the saved durable thread.",
    )?;
    if let Some(path) = f(thread, "path")
        .as_js_string()
        .and_then(|v| v.to_utf8().ok())
    {
        if path.replace('\\', "/").contains("/archived_sessions/") {
            return Ok(None);
        }
    }
    let Some(turn) = latest_turn(turns)? else {
        return Ok(None);
    };
    if !f(turn, "id")
        .as_js_string()
        .is_some_and(|v| !v.0.is_empty())
        || f(turn, "id") == f(previous, "turnId")
    {
        return Ok(None);
    }
    if eq(turn, "status", "inProgress") {
        return Ok(eq(f(thread, "status"), "type", "active").then_some(turn));
    }
    let completed = f(turn, "completedAt").as_f64().map(|v| v * 1000.0);
    let created = s(previous, "createdAt").map(parse_date)?;
    Ok((["completed", "failed", "interrupted"]
        .iter()
        .any(|v| eq(turn, "status", v))
        && completed.is_some_and(|v| v.is_finite() && v >= created))
    .then_some(turn))
}

/// Verify the pair against one exact registered catalog and return its current
/// continuation definitions. Neither observation confers dispatch authority.
pub fn verify_observations<S: ReadEvidenceStore>(
    store: &S,
    catalogs: &CheckedNativeContracts,
    principal: &JsString,
    task: &Json,
    previous: &Json,
    metadata: &Json,
    turns: &Json,
) -> IvyResult<Json> {
    let source = catalogs.checked_native_contract(
        s(metadata, "nativeVersion")?,
        Some(&Json::object([(
            "catalogHash",
            f(metadata, "catalogHash").clone(),
        )])),
    )?;
    let primary = f(task, "primaryResourceRef");
    for (observation, method, params) in [
        (
            metadata,
            "thread/read",
            Json::object([
                ("threadId", f(primary, "nativeId").clone()),
                ("includeTurns", false.into()),
            ]),
        ),
        (
            turns,
            "thread/turns/list",
            turns_params(f(primary, "nativeId")),
        ),
    ] {
        ReadEvidence { store, catalogs }.verify(
            observation,
            &ReadIdentity {
                observation_id: s(observation, "observationId")?.clone(),
                caller_principal_id: principal.clone(),
                service_node_id: s(primary, "serviceNodeId")?.clone(),
                native_version: s(metadata, "nativeVersion")?.clone(),
                epoch: s(metadata, "epoch")?.clone(),
                method: method.into(),
                params,
            },
            Some(s(source.catalog(), "sourceHash")?),
        )?;
    }
    require(
        candidate(task, previous, metadata, turns)?.is_some(),
        "task_board_turn_mismatch",
        "The observation must identify a new native turn.",
    )?;
    let mut plan = f(previous, "plan").clone();
    plan.insert("nativeVersion", f(metadata, "nativeVersion").clone());
    plan.insert(
        "catalogSourceHash",
        f(source.catalog(), "sourceHash").clone(),
    );
    plan.insert(
        "definitions",
        Json::object([
            (
                "threadStart",
                source.definition_hash(&"thread/start".into())?.into(),
            ),
            (
                "threadResume",
                source.definition_hash(&"thread/resume".into())?.into(),
            ),
            (
                "turnStart",
                source.definition_hash(&"turn/start".into())?.into(),
            ),
            (
                "turnInterrupt",
                source.definition_hash(&"turn/interrupt".into())?.into(),
            ),
        ]),
    );
    Ok(plan)
}

struct CheckedPrimary {
    task: JsString,
    primary: Json,
    run: Json,
    after: f64,
}
#[allow(async_fn_in_trait)]
pub trait DirectTurnAdoption {
    async fn adopt(&self, task: &JsString) -> IvyResult<Option<JsString>>;
    fn notification(&self, value: &Json) -> bool;
}
pub struct NativeAdoption<'a, S, D, C> {
    pub store: &'a S,
    pub driver: &'a D,
    pub client: &'a dyn RpcClient,
    pub now_millis: C,
    checked: RefCell<Vec<CheckedPrimary>>,
}
impl<'a, S, D, C> NativeAdoption<'a, S, D, C> {
    pub fn new(store: &'a S, driver: &'a D, client: &'a dyn RpcClient, now_millis: C) -> Self {
        Self {
            store,
            driver,
            client,
            now_millis,
            checked: RefCell::default(),
        }
    }
}
impl<S: LifecycleStore, D: NativeDriverConnection, C: Fn() -> f64> NativeAdoption<'_, S, D, C> {
    async fn observe(&self, task: &crate::operations::Document) -> IvyResult<Option<Json>> {
        let value = &task.value;
        let primary = f(value, "primaryResourceRef");
        if !f(value, "claim").is_null()
            || !f(value, "publication").is_null()
            || f(value, "lastRun").is_null()
            || !eq(primary, "namespace", "codex")
            || !eq(primary, "kind", "thread")
        {
            return Ok(None);
        }
        let previous = self
            .store
            .read("task-board/run", f(value, "lastRun"), true)
            .await?;
        if !finished_run(&previous.value)
            || canonical(primary, MANAGEMENT_FRAME_BYTES)?
                != canonical(
                    f(&previous.value, "primaryResourceRef"),
                    MANAGEMENT_FRAME_BYTES,
                )?
        {
            return Ok(None);
        }
        let node = self
            .client
            .request(
                "serviceNodes.get",
                Json::object([("serviceNodeId", f(primary, "serviceNodeId").clone())]),
                RequestOptions::default(),
            )
            .await?;
        if !["connected", "synced", "ready"]
            .iter()
            .all(|k| f(&node, k).as_bool() == Some(true))
        {
            return Ok(None);
        }
        let status = management_read(
            self.driver.tools(),
            s(primary, "serviceNodeId")?,
            "status",
            Json::object([] as [(&str, Json); 0]),
        )
        .await?;
        validate_agent("Status", &status)?;
        if !eq(&status, "state", "ready")
            || !f(&status, "epoch")
                .as_js_string()
                .is_some_and(|v| !v.0.is_empty())
        {
            return Ok(None);
        }
        require(
            eq(&node, "serviceName", "agent-manager")
                && f(&status, "serviceNodeId") == f(primary, "serviceNodeId")
                && f(&status, "hostId") == f(f(&previous.value, "target"), "hostId")
                && f(&status, "nativeVersion") == f(&node, "nativeVersion"),
            "task_board_native_unavailable",
            "Direct turns must belong to the saved native owner.",
        )?;
        let owner = NativeOwner::new(
            self.driver.tools().clone(),
            self.driver.principal_id().clone(),
            NativeOwnerIdentity {
                service_node_id: s(&status, "serviceNodeId")?.clone(),
                host_id: Some(s(&status, "hostId")?.clone()),
                native_version: s(&status, "nativeVersion")?.clone(),
                native_executable_hash: s(&status, "nativeExecutableHash")?.clone(),
                catalog_hash: s(&status, "catalogHash")?.clone(),
            },
            RequestOptions::default(),
        );
        let turns = owner
            .read(
                &"thread/turns/list".into(),
                turns_params(f(primary, "nativeId")),
                Some(s(&status, "epoch")?),
                true,
            )
            .await?;
        if f(&turns, "reply").get("result").is_none() {
            let message = s(f(f(&turns, "reply"), "error"), "message")?;
            if missing_thread(message) {
                return Ok(None);
            }
            return Err(IvyError::new_js(
                "task_board_native_read_failed".into(),
                message.clone(),
            ));
        }
        let Some(latest) = latest_turn(&turns)? else {
            return Ok(None);
        };
        if f(latest, "id").is_null() || f(latest, "id") == f(&previous.value, "turnId") {
            return Ok(None);
        }
        let metadata = owner
            .read(
                &"thread/read".into(),
                Json::object([
                    ("threadId", f(primary, "nativeId").clone()),
                    ("includeTurns", false.into()),
                ]),
                Some(s(&status, "epoch")?),
                false,
            )
            .await?;
        let Some(turn) = candidate(value, &previous.value, &metadata, &turns)? else {
            return Ok(None);
        };
        let task_id = s(&task.pin, "objectId")?;
        let hash = hash_json(&Json::object([
            ("primary", primary.clone()),
            ("turnId", f(turn, "id").clone()),
            ("task", task.pin.clone()),
            ("observation", f(&turns, "observationId").clone()),
        ]))?;
        let operation = mutation(task_id, &format!("adopt:{hash}"))?;
        self.driver
            .native_update(&Json::object([
                ("action", "nativeUpdate".into()),
                ("operationId", operation.into()),
                ("taskId", Json::String(task_id.clone())),
                ("expectedRevision", f(&task.pin, "revision").clone()),
                ("run", previous.pin),
                (
                    "change",
                    Json::object([
                        ("kind", "adoptTurn".into()),
                        ("metadata", metadata),
                        ("turns", turns),
                    ]),
                ),
            ]))
            .await
            .map(Some)
    }
}
impl<S: LifecycleStore, D: NativeDriverConnection, C: Fn() -> f64> DirectTurnAdoption
    for NativeAdoption<'_, S, D, C>
{
    async fn adopt(&self, id: &JsString) -> IvyResult<Option<JsString>> {
        let task = self
            .store
            .lifecycle_read("task-board/task", &Json::String(id.clone()), true, true)
            .await?;
        let value = &task.document.value;
        let primary = f(value, "primaryResourceRef");
        if f(&task.metadata, "effectivelyArchived").as_bool() == Some(true)
            || !eq(f(value, "fields"), "control", "agent")
            || primary.is_null()
        {
            self.checked.borrow_mut().retain(|v| &v.task != id);
            return Ok(None);
        }
        let now = (self.now_millis)();
        let run = f(f(value, "lastRun"), "objectId");
        let force = eq(value, "workflowState", "todo")
            || f(value, "commentDeliveries")
                .as_array()
                .unwrap_or(&[])
                .iter()
                .any(|v| eq(v, "state", "queued"));
        if !f(value, "claim").is_null() {
            return Ok(None);
        }
        if self
            .checked
            .borrow()
            .iter()
            .any(|v| &v.task == id && &v.primary == primary && &v.run == run && v.after > now)
            && !force
        {
            return Ok(None);
        }
        self.driver.verify_owner().await?;
        let adopted = self.observe(&task.document).await?;
        let adopted_run = adopted
            .as_ref()
            .and_then(|v| f(v, "run").get("objectId"))
            .and_then(Json::as_js_string)
            .cloned();
        let mut checked = self.checked.borrow_mut();
        checked.retain(|v| &v.task != id);
        if checked.len() >= 1024 {
            checked.remove(0);
        }
        checked.push(CheckedPrimary {
            task: id.clone(),
            primary: primary.clone(),
            run: adopted_run
                .clone()
                .map(Json::String)
                .unwrap_or_else(|| run.clone()),
            after: now + 30_000.0,
        });
        Ok(adopted_run)
    }
    fn notification(&self, value: &Json) -> bool {
        let params = f(value, "params");
        let event = f(params, "payload");
        let body = f(event, "params");
        if !["turn/started", "turn/completed", "thread/status/changed"]
            .iter()
            .any(|v| eq(event, "method", v))
        {
            return false;
        }
        let mut relevant = false;
        for entry in self.checked.borrow_mut().iter_mut() {
            if f(&entry.primary, "serviceNodeId") == f(params, "serviceNodeId")
                && f(&entry.primary, "nativeId") == f(body, "threadId")
            {
                entry.after = f64::NEG_INFINITY;
                relevant = true;
            }
        }
        relevant
    }
}
