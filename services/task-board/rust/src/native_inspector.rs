//! Read-only inspection of an already retained native execution. Search
//! checkpoints confer neither dispatch authority nor completion authority.
use crate::{
    operations::{Document, mutation},
    read_evidence::{ReadEvidence, ReadEvidenceStore, ReadIdentity},
    turn_output::{page_cursor, turns_params},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, agent_schema::validate_agent,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    clock::parse_date,
    native_evidence::{NativeOwnerIdentity, validate_native_status},
    tools::{InterfaceRequirement, ToolCallOptions, ToolClients},
};
use std::collections::HashSet;

#[allow(async_fn_in_trait)]
pub trait InspectionStore: ReadEvidenceStore {
    async fn verify_owner(&self) -> IvyResult<()>;
    async fn named_document(
        &self,
        key: &str,
        name: &str,
        parent: &JsString,
    ) -> IvyResult<Option<Document>>;
    fn tools(&self) -> &ToolClients;
    fn principal_id(&self) -> JsString;
    fn now(&self) -> JsString;
}
pub struct NativeInspector<'a, S> {
    pub store: &'a S,
    pub catalogs: &'a CheckedNativeContracts,
}
#[derive(Clone)]
pub struct InspectionContext {
    pub run: Json,
    pub native_version: JsString,
    pub catalog_source_hash: JsString,
    pub service_node_id: JsString,
    pub epoch: JsString,
    pub thread_id: JsString,
    pub turn_id: JsString,
}
impl InspectionContext {
    pub fn value(&self) -> Json {
        Json::object([
            ("run", self.run.clone()),
            ("nativeVersion", Json::String(self.native_version.clone())),
            (
                "catalogSourceHash",
                Json::String(self.catalog_source_hash.clone()),
            ),
            ("serviceNodeId", Json::String(self.service_node_id.clone())),
            ("epoch", Json::String(self.epoch.clone())),
            ("threadId", Json::String(self.thread_id.clone())),
            ("turnId", Json::String(self.turn_id.clone())),
        ])
    }
}
pub enum Inspection {
    Found { metadata: Json, turns: Json },
    Pending { search: Json },
    Absent,
}
#[derive(Clone, Copy)]
pub enum PageSize {
    One,
    Hundred,
}
impl PageSize {
    fn number(self) -> f64 {
        match self {
            Self::One => 1.0,
            Self::Hundred => 100.0,
        }
    }
}
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn text(v: &Json, k: &str) -> IvyResult<JsString> {
    v.required(k)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn equal(v: &Json, k: &str, s: &str) -> bool {
    v.get(k).and_then(Json::as_js_string) == Some(&s.into())
}
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => *n != 0.0 && !n.is_nan(),
        Json::String(s) => !s.0.is_empty(),
        _ => true,
    }
}
fn array(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(a: Option<&Json>, b: Option<&Json>) -> IvyResult<bool> {
    match (a, b) {
        (Some(a), Some(b)) => {
            Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
        }
        _ => Ok(a.is_none() && b.is_none()),
    }
}
fn read_time(v: &Json) -> f64 {
    v.get("observedAt")
        .and_then(Json::as_js_string)
        .map(parse_date)
        .unwrap_or(f64::NAN)
}

impl<S: InspectionStore> NativeInspector<'_, S> {
    async fn management(&self, node: &JsString, name: &str, args: Json) -> IvyResult<Json> {
        self.store
            .tools()
            .service_tools(
                node.clone(),
                &[InterfaceRequirement {
                    namespace: "agent".into(),
                    interface_version: "1.0.0".into(),
                }],
            )?
            .read(
                &format!("agent.{name}").into(),
                args,
                ToolCallOptions::default(),
            )
            .await
    }
    fn owner(&self, run: &Json) -> IvyResult<NativeOwnerIdentity> {
        let plan = run.required("plan")?;
        let version = text(plan, "nativeVersion")?;
        let source = self.catalogs.checked_native_contract(
            &version,
            Some(&Json::object([(
                "sourceHash",
                plan.required("catalogSourceHash")?.clone(),
            )])),
        )?;
        let target = run.required("target")?;
        Ok(NativeOwnerIdentity {
            service_node_id: text(target, "serviceNodeId")?,
            host_id: Some(text(target, "hostId")?),
            native_version: version,
            native_executable_hash: text(source.catalog(), "nativeExecutableHash")?,
            catalog_hash: source.catalog_hash().into(),
        })
    }
    pub async fn context(&self, run_pin: &Json) -> IvyResult<InspectionContext> {
        self.store.verify_owner().await?;
        let run = self
            .store
            .read_document("task-board/run", run_pin, true)
            .await?;
        let value = &run.value;
        let primary = field(value, "primaryResourceRef");
        let target = value.required("target")?;
        require(
            equal(primary, "namespace", "codex")
                && equal(primary, "kind", "thread")
                && primary.get("serviceNodeId") == target.get("serviceNodeId")
                && truthy(field(value, "turnId"))
                && truthy(field(value, "nativeEpoch")),
            "task_board_turn_mismatch",
            "Inspection requires an already saved original native primary and turn.",
        )?;
        let status = self
            .management(
                &text(target, "serviceNodeId")?,
                "status",
                Json::object(Vec::<(&str, Json)>::new()),
            )
            .await?;
        validate_native_status(
            &status,
            &self.owner(value)?,
            Some("task_board_native_unavailable"),
        )?;
        let plan = value.required("plan")?;
        Ok(InspectionContext {
            run: run_pin.clone(),
            native_version: text(plan, "nativeVersion")?,
            catalog_source_hash: text(plan, "catalogSourceHash")?,
            service_node_id: text(&status, "serviceNodeId")?,
            epoch: text(&status, "epoch")?,
            thread_id: text(primary, "nativeId")?,
            turn_id: text(value, "turnId")?,
        })
    }
    pub async fn owner_stopped(&self, run: &Json) -> IvyResult<bool> {
        if !truthy(field(run, "nativeEpoch")) {
            return Ok(false);
        }
        let status = self
            .management(
                &text(run.required("target")?, "serviceNodeId")?,
                "status",
                Json::object(Vec::<(&str, Json)>::new()),
            )
            .await?;
        validate_native_status(
            &status,
            &self.owner(run)?,
            Some("task_board_native_unavailable"),
        )?;
        let connection = field(&status, "connection");
        Ok(equal(&status, "state", "ready")
            && truthy(field(&status, "epoch"))
            && status.get("epoch") != run.get("nativeEpoch")
            && equal(connection, "mode", "owned-stdio")
            && truthy(field(connection, "ownsServer"))
            && array(field(&status, "processStops"))
                .iter()
                .any(|v| v.get("epoch") == run.get("nativeEpoch")))
    }
    pub async fn read(
        &self,
        context: &InspectionContext,
        method: &JsString,
        params: Json,
        allow_error: bool,
    ) -> IvyResult<Json> {
        self.store.verify_owner().await?;
        let observed = self
            .management(
                &context.service_node_id,
                "read",
                Json::object([
                    (
                        "nativeVersion",
                        Json::String(context.native_version.clone()),
                    ),
                    ("method", Json::String(method.clone())),
                    ("params", params.clone()),
                ]),
            )
            .await?;
        validate_agent("ReadObservation", &observed)?;
        ReadEvidence {
            store: self.store,
            catalogs: self.catalogs,
        }
        .verify(
            &observed,
            &ReadIdentity {
                observation_id: text(&observed, "observationId")?,
                caller_principal_id: self.store.principal_id(),
                service_node_id: context.service_node_id.clone(),
                native_version: context.native_version.clone(),
                epoch: context.epoch.clone(),
                method: method.clone(),
                params,
            },
            Some(&context.catalog_source_hash),
        )?;
        require(
            allow_error || field(&observed, "reply").get("result").is_some(),
            "task_board_native_read_failed",
            "Native inspection returned its original error; no successful page is available.",
        )?;
        require(
            read_time(&observed).is_finite(),
            "task_board_evidence_mismatch",
            "Native inspection requires a valid observation time.",
        )?;
        Ok(observed)
    }
    fn matches(&self, observation: &Json, turn: &JsString) -> IvyResult<(bool, Option<JsString>)> {
        let reply = field(observation, "reply");
        require(
            reply.get("result").is_some(),
            "task_board_native_read_failed",
            "Native turn inspection requires a successful reply.",
        )?;
        let page = field(reply, "result");
        let data = array(field(page, "data"));
        require(
            data.len() <= 100,
            "task_board_native_page_limit",
            "The native turn page exceeds the requested item bound.",
        )?;
        let count = data
            .iter()
            .filter(|v| v.get("id").and_then(Json::as_js_string) == Some(turn))
            .count();
        require(
            count <= 1,
            "task_board_turn_mismatch",
            "The known native turn may appear only once in its page.",
        )?;
        Ok((count == 1, page_cursor(page.get("nextCursor"))?))
    }
    async fn checkpoint(
        &self,
        context: &InspectionContext,
        saved: Option<&Document>,
        run_pin: &Json,
        name: &str,
        visited: &[String],
        cursor: Option<&JsString>,
        created_at: &JsString,
        finished: bool,
    ) -> IvyResult<Json> {
        self.store.verify_owner().await?;
        let mut value = context.value();
        value.remove("catalogSourceHash");
        value.insert("schemaVersion", Json::Number(1.0));
        value.insert(
            "run",
            saved
                .map(|d| field(&d.value, "run").clone())
                .unwrap_or_else(|| run_pin.clone()),
        );
        value.insert(
            "phase",
            if finished { "finished" } else { "searching" }.into(),
        );
        value.insert(
            "nextCursor",
            if finished {
                Json::Null
            } else {
                cursor.cloned().map(Json::String).unwrap_or(Json::Null)
            },
        );
        value.insert("pageCount", Json::Number(visited.len() as f64));
        value.insert(
            "visitedCursorHashes",
            Json::Array(visited.iter().cloned().map(Json::from).collect()),
        );
        value.insert("createdAt", Json::String(created_at.clone()));
        value.insert("updatedAt", Json::String(self.store.now()));
        let (id, destination) = if let Some(saved) = saved {
            (
                mutation(
                    &name.into(),
                    &format!(
                        "advance:{}",
                        hash_json(&Json::object([
                            ("pin", saved.pin.clone()),
                            ("value", value.clone())
                        ]))?
                    ),
                )?,
                Json::object([
                    ("objectId", saved.pin.required("objectId")?.clone()),
                    ("expectedRevision", saved.pin.required("revision")?.clone()),
                ]),
            )
        } else {
            (
                mutation(&name.into(), "create")?,
                Json::object([(
                    "create",
                    Json::object([
                        ("parentId", run_pin.required("objectId")?.clone()),
                        ("name", name.into()),
                    ]),
                )]),
            )
        };
        self.store
            .write_document("task-board/native-turn-search", &value, &id, &destination)
            .await
    }
    pub async fn find(
        &self,
        run_pin: &Json,
        budget: f64,
        page_size: PageSize,
    ) -> IvyResult<Inspection> {
        require(
            budget.is_finite() && budget.fract() == 0.0 && (1.0..=64.0).contains(&budget),
            "task_board_inspection_budget",
            "One inspection pass allows one to 64 search pages.",
        )?;
        let context = self.context(run_pin).await?;
        let mut identity = context.value();
        identity.insert("run", run_pin.required("objectId")?.clone());
        if matches!(page_size, PageSize::One) {
            identity.insert("pageSize", Json::Number(1.0));
        }
        let name = format!("Native turn search {}", &hash_json(&identity)?[7..]);
        let saved = self
            .store
            .named_document(
                "task-board/native-turn-search",
                &name,
                &text(run_pin, "objectId")?,
            )
            .await?;
        if let Some(saved) = &saved {
            let old = &saved.value;
            let old_run = old.required("run")?;
            let original = self
                .store
                .read_document("task-board/run", old_run, true)
                .await?;
            let current = self
                .store
                .read_document("task-board/run", run_pin, true)
                .await?;
            require(
                old_run.get("objectId") == run_pin.get("objectId")
                    && field(old_run, "revision").as_f64() <= field(run_pin, "revision").as_f64()
                    && same(
                        original.value.get("originalRequest"),
                        current.value.get("originalRequest"),
                    )?
                    && [
                        "threadId",
                        "turnId",
                        "nativeVersion",
                        "serviceNodeId",
                        "epoch",
                    ]
                    .iter()
                    .all(|k| old.get(k) == context.value().get(k))
                    && field(old, "pageCount").as_f64()
                        == Some(array(field(old, "visitedCursorHashes")).len() as f64)
                    && field(&original.value, "primaryResourceRef").get("nativeId")
                        == Some(&Json::String(context.thread_id.clone()))
                    && original.value.get("turnId") == Some(&Json::String(context.turn_id.clone())),
                "task_board_evidence_mismatch",
                "Retained search must preserve the exact known execution and bounded cursor history.",
            )?;
        }
        let continuing = saved
            .as_ref()
            .filter(|d| equal(&d.value, "phase", "searching"));
        let mut visited = Vec::<String>::new();
        let mut seen = HashSet::new();
        if let Some(old) = continuing {
            for v in array(field(&old.value, "visitedCursorHashes")) {
                let v = v.as_str()?;
                if seen.insert(v.clone()) {
                    visited.push(v);
                }
            }
        }
        let mut cursor = continuing
            .and_then(|d| field(&d.value, "nextCursor").as_js_string())
            .cloned();
        let created_at = if let Some(old) = continuing {
            text(&old.value, "createdAt")?
        } else {
            self.store.now()
        };
        for _ in 0..budget as usize {
            require(
                visited.len() < 1024,
                "task_board_native_search_limit",
                "The known native turn search exceeded 1024 pages.",
            )?;
            let identity = hash_json(&cursor.clone().map(Json::String).unwrap_or(Json::Null))?;
            require(
                !seen.contains(&identity),
                "task_board_native_cursor_loop",
                "The native turn search repeated an opaque cursor.",
            )?;
            let mut params = turns_params(&context.thread_id, cursor.as_ref());
            params.insert("limit", Json::Number(page_size.number()));
            let turns = self
                .read(&context, &"thread/turns/list".into(), params.clone(), false)
                .await?;
            require(
                field(&turns, "reply").get("result").is_some()
                    && array(field(field(field(&turns, "reply"), "result"), "data")).len() as f64
                        <= page_size.number(),
                "task_board_native_page_limit",
                "The native turn page exceeds its exact requested bound.",
            )?;
            let (found, next) = self.matches(&turns, &context.turn_id)?;
            seen.insert(identity.clone());
            visited.push(identity);
            if found {
                let metadata = self
                    .read(
                        &context,
                        &"thread/read".into(),
                        Json::object([
                            ("threadId", Json::String(context.thread_id.clone())),
                            ("includeTurns", Json::Bool(false)),
                        ]),
                        false,
                    )
                    .await?;
                let fresh = self
                    .read(&context, &"thread/turns/list".into(), params, false)
                    .await?;
                require(
                    field(&fresh, "reply").get("result").is_some()
                        && array(field(field(field(&fresh, "reply"), "result"), "data")).len()
                            as f64
                            <= page_size.number(),
                    "task_board_native_page_limit",
                    "The fresh native turn page exceeds its exact requested bound.",
                )?;
                require(
                    self.matches(&fresh, &context.turn_id)?.0,
                    "task_board_native_state_changed",
                    "The known turn moved during inspection; inspect again before publication.",
                )?;
                require(
                    field(&metadata, "reply").get("result").is_some(),
                    "task_board_native_read_failed",
                    "Native metadata requires its successful reply.",
                )?;
                let thread = field(field(field(&metadata, "reply"), "result"), "thread");
                require(
                    thread.get("id").and_then(Json::as_js_string) == Some(&context.thread_id)
                        && field(thread, "ephemeral").as_bool() == Some(false)
                        && read_time(&metadata) <= read_time(&fresh),
                    "task_board_evidence_mismatch",
                    "Fresh metadata must retain the exact durable primary and precede its selected turn observation.",
                )?;
                if saved.is_some() {
                    self.checkpoint(
                        &context,
                        saved.as_ref(),
                        run_pin,
                        &name,
                        &visited,
                        cursor.as_ref(),
                        &created_at,
                        true,
                    )
                    .await?;
                }
                return Ok(Inspection::Found {
                    metadata,
                    turns: fresh,
                });
            }
            cursor = next;
            if cursor.is_none() {
                if saved.is_some() {
                    self.checkpoint(
                        &context,
                        saved.as_ref(),
                        run_pin,
                        &name,
                        &visited,
                        None,
                        &created_at,
                        true,
                    )
                    .await?;
                }
                return Ok(Inspection::Absent);
            }
            require(
                !seen.contains(&hash_json(&Json::String(cursor.clone().unwrap()))?),
                "task_board_native_cursor_loop",
                "The native turn search repeated an opaque cursor.",
            )?;
        }
        Ok(Inspection::Pending {
            search: self
                .checkpoint(
                    &context,
                    saved.as_ref(),
                    run_pin,
                    &name,
                    &visited,
                    cursor.as_ref(),
                    &created_at,
                    false,
                )
                .await?,
        })
    }
}
