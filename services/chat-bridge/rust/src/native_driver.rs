//! Durable original native calls. Current Main/queue claims remain live caller
//! guards; retained evidence and readiness alone never authorize dispatch.
use crate::{
    admission::ChatAdmission,
    native_evidence::{ChatNativeEvidence, verify_chat_native_request},
    native_plan::{ChatNativeValidation, resolve_chat_plan, validate_chat_native_draft},
    operations::contention,
    store::{ChatStore, Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, agent_schema::validate_agent,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, js_path::normalize_win32,
    json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    connection_owner::connection_owner,
    native_evidence::{NativeOwnerIdentity, validate_native_status},
    native_operation::{
        NativeAbsentHandler, NativeOperationCall, NativeOperationJournal, NativeReconciliation,
        SubmissionFuture, reconcile_native_operation,
    },
    native_owner::{GuardFuture, NativeDispatchGuard, NativeOwner},
    rpc::RequestOptions,
    tools::ToolClients,
};
use std::sync::Arc;

pub struct ChatNativeDriver<'a> {
    pub store: &'a ChatStore,
    tools: Arc<ToolClients>,
    admission: &'a ChatAdmission,
    catalogs: &'a CheckedNativeContracts,
    service_node_id: JsString,
    instructions: &'a JsString,
    validation: ChatNativeValidation<'a>,
}

#[derive(Clone, Default)]
pub struct NativeOrigin {
    pub predecessor: Option<Pin>,
    pub prepared_epoch: Option<JsString>,
}

impl<'a> ChatNativeDriver<'a> {
    /// Reuse the provider connection's ToolClients, including its binding and
    /// selected-catalog caches. The service identity is distinct from the
    /// configured native project's owner.
    pub fn new(
        store: &'a ChatStore,
        tools: Arc<ToolClients>,
        admission: &'a ChatAdmission,
        catalogs: &'a CheckedNativeContracts,
        service_node_id: JsString,
        instructions: &'a JsString,
    ) -> IvyResult<Self> {
        let value = Self {
            store,
            tools,
            admission,
            catalogs,
            service_node_id,
            instructions,
            validation: ChatNativeValidation::new(catalogs, instructions),
        };
        value.evidence()?;
        Ok(value)
    }

    pub fn evidence(&self) -> IvyResult<ChatNativeEvidence<'_>> {
        ChatNativeEvidence::new(
            self.store,
            self.admission.definition(),
            self.catalogs,
            &self.validation,
            self.instructions,
        )
    }

    pub async fn verify_owner(&self) -> IvyResult<()> {
        let node = connection_owner(self.store.client.as_ref(), &self.service_node_id).await?;
        if node.get("principalId") != self.admission.definition().get("principalId")
            || node.get("serviceName") != Some(&Json::from("chat-bridge"))
            || !node.get("connected").is_some_and(truthy)
            || !node.get("synced").is_some_and(truthy)
        {
            return Err(IvyError::new(
                "chat_principal_mismatch",
                "Chat recovery requires its configured principal and a live synchronized service connection.",
            ));
        }
        Ok(())
    }

    pub fn native_owner(&self) -> IvyResult<NativeOwner> {
        let definition = self.admission.definition();
        let plan = definition.required("nativePlan")?;
        let source = self.catalogs.checked_native_contract(
            text(plan, "nativeVersion")?,
            Some(&Json::object([(
                "sourceHash",
                plan.required("catalogSourceHash")?.clone(),
            )])),
        )?;
        let target = NativeOwnerIdentity {
            service_node_id: text(definition.required("project")?, "serviceNodeId")?.clone(),
            host_id: None,
            native_version: text(source.catalog(), "version")?.clone(),
            native_executable_hash: text(source.catalog(), "nativeExecutableHash")?.clone(),
            catalog_hash: source.catalog_hash().into(),
        };
        Ok(NativeOwner::new(
            self.tools.clone(),
            text(definition, "principalId")?.clone(),
            target,
            RequestOptions::default(),
        ))
    }

    pub async fn management(&self, name: &str, args: Json) -> IvyResult<Json> {
        self.native_owner()?.management(name, args, None).await
    }

    pub async fn ready(&self) -> IvyResult<Json> {
        self.verify_owner().await?;
        let value = self
            .management("status", Json::object([] as [(&str, Json); 0]))
            .await?;
        validate_native_status(
            &value,
            self.native_owner()?.target(),
            Some("chat_native_unavailable"),
        )?;
        Ok(value)
    }

    pub async fn verify_project(&self) -> IvyResult<()> {
        self.ready().await?;
        let definition = self.admission.definition();
        let plan = resolve_chat_plan(
            self.store,
            self.catalogs,
            definition.required("nativePlan")?,
            self.instructions,
        )?;
        let source = self.catalogs.checked_native_contract(
            text(&plan, "nativeVersion")?,
            Some(&Json::object([(
                "sourceHash",
                plan.required("catalogSourceHash")?.clone(),
            )])),
        )?;
        require(
            source.catalog().get("sourceHash") == plan.get("catalogSourceHash"),
            "chat_native_mismatch",
            "The configured native catalog source must match.",
        )?;
        let owner = self.native_owner()?;
        for (method, key) in METHODS {
            let method = JsString::from(method);
            let binding = owner.binding(&method, None).await?;
            require(
                binding.service_node_id()?
                    == *text(definition.required("project")?, "serviceNodeId")?
                    && binding.value().get("definitionHash")
                        == plan.required("definitions")?.get(key)
                    && binding.value().get("definitionHash")
                        == Some(&hash_json(source.definition(&method)?)?.into()),
                "chat_native_definition_changed",
                "Main needs the exact configured native method definitions.",
            )?;
        }
        let value = self.management("projects", Json::Object(vec![])).await?;
        validate_agent("ProjectsResult", &value)?;
        let project_id = definition.required("project")?.required("nativeId")?;
        let projects: Vec<_> = value
            .required("projects")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .filter(|p| p.get("nativeId") == Some(project_id))
            .collect();
        require(
            projects.len() == 1,
            "chat_project_mismatch",
            "The exact configured project must exist on its native owner.",
        )?;
        let start = plan.required("threadStart")?;
        require(
            start.get("cwd").and_then(Json::as_js_string).is_some()
                && start.get("ephemeral") != Some(&Json::Bool(true)),
            "chat_project_mismatch",
            "Main needs an explicit project cwd and a persistent native thread.",
        )?;
        let cwd = native_path(text(start, "cwd")?)?;
        let mut belongs = false;
        for root in projects[0]
            .required("paths")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let path = native_path(root.as_js_string().ok_or_else(IvyError::internal)?)?;
            let mut prefix = path.0.clone();
            if prefix.last() != Some(&47) {
                prefix.push(47);
            }
            if cwd == path || cwd.0.starts_with(&prefix) {
                belongs = true;
                break;
            }
        }
        require(
            belongs,
            "chat_project_mismatch",
            "Main cwd must belong to the exact configured native project.",
        )?;
        for key in ["threadResume", "turnStart"] {
            let path = plan.required(key)?.get("cwd").filter(|v| !v.is_null());
            let matches = match path {
                None => true,
                Some(Json::String(path)) => native_path(path)? == cwd,
                _ => false,
            };
            require(
                matches,
                "chat_project_mismatch",
                "The native plan cannot switch Main to another cwd.",
            )?;
        }
        Ok(())
    }

    pub async fn get(&self, parent_id: &str, reference: Reference<'_>) -> IvyResult<Document> {
        let attached = self
            .store
            .read_in("chat-bridge/native-call", reference, Some(parent_id))
            .await
            .map_err(|e| e.into_ivy())?;
        let original = immutable(&attached.value);
        let current = if matches!(reference, Reference::Current(_)) {
            attached
        } else {
            let current = self
                .store
                .read_in(
                    "chat-bridge/native-call",
                    Reference::Current(&attached.pin.object_id),
                    Some(parent_id),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            current
        };
        require(
            current.metadata.get("name")
                == Some(&call_name(text(&current.value, "operationId")?)?.into())
                && same(&original, &immutable(&current.value))?,
            "chat_native_mismatch",
            "The retained native call cannot change its original request, preparation or identity.",
        )?;
        let evidence = self.evidence()?;
        if let Some(pin) = current.value.get("evidence").filter(|v| !v.is_null()) {
            let original = evidence
                .operation(parent_id, &current.value, &object_pin(pin)?)
                .await?;
            require(
                original.get("phase") == current.value.get("state")
                    && original.get("epoch") == current.value.get("epoch")
                    && original.get("code") == current.value.get("code")
                    && original.get("updatedAt") == current.value.get("updatedAt"),
                "chat_native_mismatch",
                "The native call must agree with its retained original owner receipt.",
            )?;
        } else {
            evidence.request(parent_id, &current.value).await?;
            require(
                current.value.get("state") == Some(&"prepared".into()),
                "chat_native_mismatch",
                "An observed native call needs its original owner evidence.",
            )?;
        }
        Ok(current)
    }

    async fn matches(
        &self,
        parent_id: &str,
        operation_id: &JsString,
        origin: &NativeOrigin,
        request: &Json,
        call: Document,
    ) -> IvyResult<Document> {
        let predecessor = origin
            .predecessor
            .as_ref()
            .map(Pin::json)
            .unwrap_or(Json::Null);
        let epoch = origin
            .prepared_epoch
            .clone()
            .map(Json::String)
            .unwrap_or(Json::Null);
        require(
            call.value.get("operationId") == Some(&Json::String(operation_id.clone()))
                && same(call.value.required("predecessor")?, &predecessor)?
                && call.value.get("preparedEpoch") == Some(&epoch)
                && same(
                    &self.evidence()?.request(parent_id, &call.value).await?,
                    request,
                )?,
            "chat_native_mismatch",
            "The same native identity must recover its original request and origin.",
        )?;
        Ok(call)
    }

    pub async fn prepare(
        &self,
        parent_id: &str,
        operation_id: &JsString,
        original: &Json,
        origin: NativeOrigin,
        response_projection: bool,
    ) -> IvyResult<Document> {
        self.verify_owner().await?;
        validate_chat_native_draft(self.catalogs, original)?;
        let mut request = original.clone();
        require(
            request.get("method") != Some(&"turn/steer".into()),
            "chat_native_mismatch",
            "ChatBridge prepares only its retained plan methods.",
        )?;
        let name = call_name(operation_id)?;
        let existing = self
            .store
            .named_in("chat-bridge/native-call", &name, Some(parent_id))
            .await
            .map_err(|e| e.into_ivy())?;
        let resume = request.get("method") == Some(&"thread/resume".into());
        if let Some(existing) = existing {
            let saved = self
                .get(parent_id, Reference::Pinned(&existing.pin))
                .await?;
            if response_projection && resume {
                let retained = self.evidence()?.request(parent_id, &saved.value).await?;
                if let Some(exclude) = retained.required("params")?.get("excludeTurns") {
                    insert_params(&mut request, "excludeTurns", exclude.clone())?;
                }
            }
            return self
                .matches(parent_id, operation_id, &origin, &request, saved)
                .await;
        }
        if response_projection && resume {
            insert_params(&mut request, "excludeTurns", Json::Bool(true))?;
        }
        let at = self.store.now_iso()?;
        let mut value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("operationId", Json::String(operation_id.clone())),
            (
                "serviceNodeId",
                self.admission
                    .definition()
                    .required("project")?
                    .required("serviceNodeId")?
                    .clone(),
            ),
            (
                "callerPrincipalId",
                self.admission.definition().required("principalId")?.clone(),
            ),
            (
                "request",
                Pin {
                    object_id: parent_id.into(),
                    revision: 1,
                }
                .json(),
            ),
            ("nativeVersion", request.required("nativeVersion")?.clone()),
            (
                "catalogSourceHash",
                request.required("catalogSourceHash")?.clone(),
            ),
            ("method", request.required("method")?.clone()),
            (
                "predecessor",
                origin
                    .predecessor
                    .as_ref()
                    .map(Pin::json)
                    .unwrap_or(Json::Null),
            ),
            (
                "preparedEpoch",
                origin
                    .prepared_epoch
                    .clone()
                    .map(Json::String)
                    .unwrap_or(Json::Null),
            ),
            (
                "requestHash",
                hash_json(&Json::object([
                    ("method", request.required("method")?.clone()),
                    ("params", request.required("params")?.clone()),
                ]))?
                .into(),
            ),
            (
                "expectedDefinitionHash",
                self.admission
                    .definition()
                    .required("nativePlan")?
                    .required("definitions")?
                    .required(method_key(text(&request, "method")?)?)?
                    .clone(),
            ),
            ("createdAt", at.clone().into()),
            ("updatedAt", at.into()),
            ("state", "prepared".into()),
            ("epoch", Json::Null),
            ("evidence", Json::Null),
            ("code", Json::Null),
        ]);
        verify_chat_native_request(
            self.store,
            self.admission.definition(),
            &value,
            &request,
            self.catalogs,
            &self.validation,
            self.instructions,
        )
        .await?;
        let evidence = self.evidence()?;
        value.insert(
            "request",
            evidence.save_request(parent_id, &request).await?.json(),
        );
        evidence.request(parent_id, &value).await?;
        match self
            .store
            .write(
                "chat-bridge/native-call",
                &value,
                &mutation(&operation_id.to_utf8()?, "prepare")?,
                Destination::Create {
                    parent_id: Some(parent_id),
                    name: &name,
                },
            )
            .await
        {
            Ok(pin) => self.get(parent_id, Reference::Pinned(&pin)).await,
            Err(error) => {
                let error = error.into_ivy();
                if !contention(&error) {
                    return Err(error);
                }
                let winner = self
                    .store
                    .named_in("chat-bridge/native-call", &name, Some(parent_id))
                    .await
                    .map_err(|e| e.into_ivy())?
                    .ok_or_else(|| {
                        IvyError::new(
                            "chat_native_mismatch",
                            "The competing original native call must remain available.",
                        )
                    })?;
                self.matches(
                    parent_id,
                    operation_id,
                    &origin,
                    &request,
                    self.get(parent_id, Reference::Pinned(&winner.pin)).await?,
                )
                .await
            }
        }
    }

    pub async fn prevent(
        &self,
        parent_id: &str,
        pin: &Pin,
        guard: &dyn NativeDispatchGuard,
    ) -> IvyResult<Document> {
        self.verify_owner().await?;
        let call = self.get(parent_id, Reference::Pinned(pin)).await?;
        if !terminal(&call.value) {
            let request = self.evidence()?.request(parent_id, &call.value).await?;
            guard.check().await?;
            self.verify_owner().await?;
            // The original owner journals the admission/prevention race. Typed
            // Ivy failures are reconciled below without dispatch permission.
            let _ = self
                .management(
                    "prevent",
                    Json::object([
                        ("operationId", call.value.required("operationId")?.clone()),
                        ("nativeVersion", request.required("nativeVersion")?.clone()),
                        ("method", request.required("method")?.clone()),
                        ("params", request.required("params")?.clone()),
                        (
                            "expectedDefinitionHash",
                            call.value.required("expectedDefinitionHash")?.clone(),
                        ),
                    ]),
                )
                .await;
        }
        self.advance(parent_id, &call.pin, &PreventionGuard).await
    }

    async fn retain(
        &self,
        parent_id: &str,
        value: &Json,
        pin: &Pin,
        observed: Json,
    ) -> IvyResult<Document> {
        let evidence = self
            .evidence()?
            .save_operation(parent_id, value, &observed)
            .await?;
        let mut next = value.clone();
        for (target, source) in [
            ("state", "phase"),
            ("epoch", "epoch"),
            ("code", "code"),
            ("updatedAt", "updatedAt"),
        ] {
            next.insert(target, observed.required(source)?.clone());
        }
        next.insert("evidence", evidence.json());
        let write = self
            .store
            .write(
                "chat-bridge/native-call",
                &next,
                &mutation(
                    &pin.object_id,
                    &format!("observe:{}", hash_json(&observed)?),
                )?,
                Destination::Update {
                    object_id: &pin.object_id,
                    expected_revision: pin.revision,
                },
            )
            .await;
        match write {
            Ok(saved) => self.get(parent_id, Reference::Pinned(&saved)).await,
            Err(error) => {
                let error = error.into_ivy();
                if !contention(&error) {
                    return Err(error);
                }
                self.get(parent_id, Reference::Current(&pin.object_id))
                    .await
            }
        }
    }

    pub async fn advance(
        &self,
        parent_id: &str,
        pin: &Pin,
        guard: &dyn NativeDispatchGuard,
    ) -> IvyResult<Document> {
        self.verify_owner().await?;
        let call = self.get(parent_id, Reference::Pinned(pin)).await?;
        if terminal(&call.value) {
            return Ok(call);
        }
        let owner = self.native_owner()?;
        let request = self.evidence()?.request(parent_id, &call.value).await?;
        let original = NativeOperationCall {
            operation_id: text(&call.value, "operationId")?.clone(),
            method: text(&call.value, "method")?.clone(),
            params: request.required("params")?.clone(),
            definition_hash: text(&call.value, "expectedDefinitionHash")?.clone(),
        };
        let previous = match call.value.get("evidence").filter(|v| !v.is_null()) {
            Some(pin) => Some(
                self.evidence()?
                    .operation(parent_id, &call.value, &object_pin(pin)?)
                    .await?,
            ),
            None => None,
        };
        let value = call.value.clone();
        let call_pin = call.pin.clone();
        let absent = Absent {
            driver: self,
            owner: &owner,
            value: &value,
            guard,
        };
        let result = reconcile_native_operation(
            &owner,
            &original,
            NativeOperationJournal {
                was_observed: value.get("state") != Some(&"prepared".into()),
                current: call,
                previous,
                retain: |observed| self.retain(parent_id, &value, &call_pin, observed),
            },
            Some(&absent),
            Some("chat_native_absence_conflict"),
        )
        .await?;
        match result {
            NativeReconciliation::Observed { value, .. } => Ok(value),
            NativeReconciliation::Absent { .. } => Err(IvyError::new(
                "chat_native_outcome_unavailable",
                "The original native effect has no observed receipt; keep its identity for recovery.",
            )),
        }
    }
}

struct DispatchGuard<'a, 'b> {
    driver: &'a ChatNativeDriver<'b>,
    guard: &'a dyn NativeDispatchGuard,
}
impl NativeDispatchGuard for DispatchGuard<'_, '_> {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async move {
            self.guard.check().await?;
            self.driver.verify_owner().await
        })
    }
}
struct Absent<'a, 'b> {
    driver: &'a ChatNativeDriver<'b>,
    owner: &'a NativeOwner,
    value: &'a Json,
    guard: &'a dyn NativeDispatchGuard,
}
impl NativeAbsentHandler for Absent<'_, '_> {
    fn on_absent<'a>(
        &'a self,
        call: NativeOperationCall,
        absence: &'a Json,
    ) -> SubmissionFuture<'a> {
        Box::pin(async move {
            let status = self.driver.ready().await?;
            require(
                absence.get("epoch") == status.get("epoch")
                    && (self.value.get("method") == Some(&"thread/resume".into())
                        || self.value.get("preparedEpoch") == Some(&Json::Null)
                        || self.value.get("preparedEpoch") == status.get("epoch")),
                "chat_native_epoch_changed",
                "Native dispatch must use its confirmed prepared owner epoch.",
            )?;
            self.owner
                .dispatch(
                    &call,
                    &DispatchGuard {
                        driver: self.driver,
                        guard: self.guard,
                    },
                )
                .await
        })
    }
}
struct PreventionGuard;
impl NativeDispatchGuard for PreventionGuard {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async {
            Err(IvyError::new(
                "chat_native_prevention_unknown",
                "Original prevention has no observed outcome; keep the cancellation intent.",
            )
            .with_outcome(Outcome::Unknown))
        })
    }
}
const METHODS: [(&str, &str); 4] = [
    ("thread/start", "threadStart"),
    ("thread/resume", "threadResume"),
    ("turn/start", "turnStart"),
    ("turn/interrupt", "turnInterrupt"),
];
fn method_key(method: &JsString) -> IvyResult<&'static str> {
    METHODS
        .iter()
        .find(|(name, _)| method == &JsString::from(*name))
        .map(|(_, key)| *key)
        .ok_or_else(IvyError::internal)
}
fn call_name(id: &JsString) -> IvyResult<String> {
    Ok(format!("Chat native call {}", id.to_utf8()?))
}
fn terminal(value: &Json) -> bool {
    ["succeeded", "failed", "outcome_unknown"]
        .iter()
        .any(|state| value.get("state") == Some(&Json::from(*state)))
}
fn immutable(value: &Json) -> Json {
    let mut result = value.clone();
    for key in ["state", "epoch", "evidence", "code", "updatedAt"] {
        result.remove(key);
    }
    result
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn object_pin(value: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: value.required("objectId")?.as_str()?,
        revision: value
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    })
}
fn insert_params(value: &mut Json, key: &str, field: Json) -> IvyResult<()> {
    let mut params = value.required("params")?.clone();
    params.insert(key, field);
    value.insert("params", params);
    Ok(())
}

/// Pure lexical owner-path comparison. Never resolve through this host's cwd or
/// filesystem, or strip an extended Win32 prefix.
pub fn native_path(value: &JsString) -> IvyResult<JsString> {
    let units = &value.0;
    let windows = units.starts_with(&[92, 92])
        || (units.len() >= 3
            && ((65..=90).contains(&units[0]) || (97..=122).contains(&units[0]))
            && units[1] == 58
            && [47, 92].contains(&units[2]));
    require(
        windows || units.first() == Some(&47),
        "chat_project_mismatch",
        "Main requires an explicit absolute native working directory.",
    )?;
    let mut normalized = if windows {
        normalize_win32(value)
    } else {
        let mut parts: Vec<&[u16]> = Vec::new();
        for part in units.split(|unit| *unit == 47) {
            if part.is_empty() || part == [46] {
                continue;
            }
            if part == [46, 46] {
                parts.pop();
            } else {
                parts.push(part);
            }
        }
        let mut result = vec![47];
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                result.push(47);
            }
            result.extend_from_slice(part);
        }
        JsString(result)
    };
    while normalized.0.last().is_some_and(|u| [47, 92].contains(u)) {
        normalized.0.pop();
    }
    if normalized.0.is_empty() {
        normalized = JsString::from("/");
    }
    if windows {
        normalized = normalized.to_lowercase();
        for unit in &mut normalized.0 {
            if *unit == 92 {
                *unit = 47;
            }
        }
    }
    Ok(normalized)
}

fn text<'a>(value: &'a Json, name: &str) -> IvyResult<&'a JsString> {
    value
        .required(name)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}

fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(value) => *value,
        Json::Number(value) => *value != 0.0 && !value.is_nan(),
        Json::String(value) => !value.0.is_empty(),
        _ => true,
    }
}
