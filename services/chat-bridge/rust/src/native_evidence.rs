//! Chat-specific native request/receipt authority and immutable evidence scope.
//! The retained-plan and dynamic payload compiler are required shared boundaries.
use crate::{
    admission::ChatAdmission,
    evidence_bytes::{read_chat_evidence, save_chat_evidence},
    main_context::{main_input_origin, main_turn_context, spread, truthy},
    native_plan::chat_turn_options,
    operations::ChatOperations,
    store::{ChatStore, Pin, Reference},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    chat_schema::validate_chat,
    checked_native_contract::CheckedNativeContracts,
    hash::hash_json,
    json::canonical,
    limits::{MANAGEMENT_FRAME_BYTES, NATIVE_FRAME_BYTES, NATIVE_REQUEST_FRAME_BYTES},
    native_contract::NativeContract,
};
use ivy_sdk::{
    native_evidence::{
        NativeOperationIdentity, NativeOwnerIdentity, NativeReadIdentity,
        validate_native_operation, validate_native_read,
    },
    rpc::RequestOptions,
};
use std::{future::Future, pin::Pin as FuturePin, sync::Arc};

pub type EvidenceFuture<'a, T> = FuturePin<Box<dyn Future<Output = IvyResult<T>> + Send + 'a>>;
/// No default or permissive implementation: runtime composition must supply the
/// shared native-plan and dynamic argument/result validation at these exact gates.
pub trait NativeEvidenceValidation: Send + Sync {
    fn validate_draft(&self, value: &Json) -> IvyResult<()>;
    fn resolve_plan<'a>(&'a self, store: &'a ChatStore, plan: &'a Json)
    -> EvidenceFuture<'a, Json>;
    fn validate_input(
        &self,
        contract: &NativeContract,
        method: &JsString,
        params: &Json,
        maximum_bytes: usize,
    ) -> IvyResult<()>;
    fn validate_payload(
        &self,
        contract: &NativeContract,
        observation: &Json,
        request_bytes: usize,
        response_bytes: usize,
    ) -> IvyResult<()>;
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a Json, key: &str) -> IvyResult<&'a Json> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn pin(value: &Json) -> IvyResult<Pin> {
    let revision = field(value, "revision")?
        .as_f64()
        .ok_or_else(IvyError::internal)?;
    require(
        revision.is_finite()
            && revision.fract() == 0.0
            && revision >= 1.0
            && revision <= 9_007_199_254_740_991.0,
        "chat_evidence_mismatch",
        "The selected local ChatBridge revision is unavailable.",
    )?;
    Ok(Pin {
        object_id: field(value, "objectId")?.as_str()?,
        revision: revision as i64,
    })
}
fn catalog(
    catalogs: &CheckedNativeContracts,
    version: &Json,
    source_hash: Option<&Json>,
) -> IvyResult<Arc<NativeContract>> {
    let version = version.as_js_string().ok_or_else(|| {
        IvyError::new(
            "native_version_unsupported",
            "A supported native catalog version is required.",
        )
    })?;
    let identity = source_hash
        .filter(|v| truthy(v))
        .map(|hash| Json::object([("sourceHash", hash.clone())]));
    catalogs.checked_native_contract(version, identity.as_ref())
}
fn owner(expected: &Json, contract: &NativeContract) -> IvyResult<NativeOwnerIdentity> {
    Ok(NativeOwnerIdentity {
        service_node_id: string(expected, "serviceNodeId")?.clone(),
        host_id: None,
        native_version: string(expected, "nativeVersion")?.clone(),
        native_executable_hash: string(contract.catalog(), "nativeExecutableHash")?.clone(),
        catalog_hash: contract.catalog_hash().into(),
    })
}
fn method_key(method: &JsString) -> IvyResult<&'static str> {
    match method.to_utf8()?.as_str() {
        "thread/start" => Ok("threadStart"),
        "thread/resume" => Ok("threadResume"),
        "turn/start" => Ok("turnStart"),
        "turn/interrupt" => Ok("turnInterrupt"),
        _ => Err(IvyError::internal()),
    }
}

/// Current native owner and Main queue claims remain live caller checks.
pub async fn verify_chat_native_request(
    store: &ChatStore,
    definition: &Json,
    call: &Json,
    request: &Json,
    catalogs: &CheckedNativeContracts,
    validation: &dyn NativeEvidenceValidation,
    instructions: &JsString,
) -> IvyResult<()> {
    validate_chat("Definition", definition)?;
    validate_chat("NativeCall", call)?;
    validation.validate_draft(request)?;
    let plan = validation
        .resolve_plan(store, field(definition, "nativePlan")?)
        .await?;
    let source = catalog(
        catalogs,
        field(request, "nativeVersion")?,
        Some(field(request, "catalogSourceHash")?),
    )?;
    let method = string(request, "method")?;
    let definition_hash = hash_json(source.definition(method)?)?;
    require(
        request.get("nativeVersion") == plan.get("nativeVersion")
            && request.get("catalogSourceHash") == plan.get("catalogSourceHash")
            && request.get("catalogSourceHash") == source.catalog().get("sourceHash")
            && call.get("nativeVersion") == request.get("nativeVersion")
            && call.get("catalogSourceHash") == request.get("catalogSourceHash")
            && call.get("method") == request.get("method")
            && call.get("callerPrincipalId") == definition.get("principalId")
            && call.get("serviceNodeId") == field(definition, "project")?.get("serviceNodeId")
            && call.get("requestHash")
                == Some(
                    &hash_json(&Json::object([
                        ("method", field(request, "method")?.clone()),
                        ("params", field(request, "params")?.clone()),
                    ]))?
                    .into(),
                )
            && call.get("expectedDefinitionHash") == Some(&definition_hash.into())
            && call.get("expectedDefinitionHash")
                == field(&plan, "definitions")?.get(method_key(method)?),
        "chat_native_mismatch",
        "The native call must retain its configured caller, owner, catalog, method, definition and exact request.",
    )?;
    let params = field(request, "params")?;
    canonical(
        &Json::object([
            ("id", "00000000-0000-0000-0000-000000000000".into()),
            ("method", Json::String(method.clone())),
            ("params", params.clone()),
        ]),
        NATIVE_REQUEST_FRAME_BYTES,
    )?;
    validation.validate_input(&source, method, params, NATIVE_REQUEST_FRAME_BYTES)?;
    let mut template = spread(params);
    if ["thread/resume", "turn/start"]
        .iter()
        .any(|v| method == &JsString::from(*v))
    {
        template.remove("threadId");
    }
    if method == &JsString::from("turn/start") {
        template.remove("input");
        template.remove("clientUserMessageId");
    }
    let mut expected = if method == &JsString::from("turn/interrupt") {
        Json::Object(vec![])
    } else {
        field(&plan, method_key(method)?)?.clone()
    };
    if method == &JsString::from("thread/resume") {
        if let Some(exclude) = params.get("excludeTurns") {
            expected = spread(&expected);
            expected.insert("excludeTurns", exclude.clone());
        }
    }
    if method == &JsString::from("turn/start") {
        let request_id = field(field(call, "request")?, "objectId")?.as_str()?;
        let metadata = store
            .local_info(&request_id)
            .await
            .map_err(|e| e.into_ivy())?;
        let (key, parent_id, object_id) = if let Some((key, parent)) = metadata {
            (key, parent, request_id.clone())
        } else {
            let metadata = store
                .client
                .request(
                    "objects.stat",
                    Json::object([("objectId", request_id.clone().into())]),
                    RequestOptions::default(),
                )
                .await?;
            (
                field(
                    &metadata,
                    if metadata.get("key").is_some() {
                        "key"
                    } else {
                        "contractKey"
                    },
                )?
                .as_str()?,
                metadata
                    .get("parentId")
                    .filter(|v| !v.is_null())
                    .map(Json::as_str)
                    .transpose()?,
                metadata
                    .get("id")
                    .map(Json::as_str)
                    .transpose()?
                    .unwrap_or_else(|| request_id.clone()),
            )
        };
        require(
            key == "chat-bridge/input" || key == "chat-bridge/evidence",
            "chat_scope_mismatch",
            "Turn options require the original input.",
        )?;
        let parent = store
            .read(
                "chat-bridge/input",
                Reference::Current(if key == "chat-bridge/input" {
                    &object_id
                } else {
                    parent_id.as_deref().ok_or_else(IvyError::internal)?
                }),
            )
            .await
            .map_err(|e| e.into_ivy())?;
        let admission = ChatAdmission::new(definition)?;
        let operations = ChatOperations::new(store, &admission)?;
        let sender = string(field(&parent.value, "identity")?, "senderPrincipalId")?;
        let operation = operations
            .find(sender, string(&parent.value, "operationId")?)
            .await?;
        require(
            operation.as_ref().is_some_and(|op| {
                field(&op.value, "request")
                    .ok()
                    .and_then(|r| r.get("action"))
                    .is_some_and(|a| a == &"send".into() || a == &"notify".into())
            }),
            "chat_identity_conflict",
            "Main context requires the original input operation.",
        )?;
        let operation = operation.unwrap();
        expected = main_turn_context(
            &chat_turn_options(
                field(&plan, "turnStart")?,
                field(&parent.value, "payload")?.get("turnOptions"),
            )?,
            &main_input_origin(sender, field(&operation.value, "request")?)?,
            instructions,
        )?;
    }
    if method != &JsString::from("turn/interrupt") {
        require(
            same(&template, &expected)?,
            "chat_native_mismatch",
            "Native execution cannot substitute different settings for the retained plan.",
        )?;
    }
    require(
        !(params.get("permissions").is_some_and(|v| !v.is_null())
            && (params.get("sandbox").is_some_and(|v| !v.is_null())
                || params.get("sandboxPolicy").is_some_and(|v| !v.is_null()))),
        "chat_native_mismatch",
        "Native execution cannot combine distinct permission models.",
    )
}
pub async fn verify_chat_native_operation<'v>(
    store: &ChatStore,
    definition: &Json,
    call: &Json,
    request: &Json,
    value: &'v Json,
    catalogs: &CheckedNativeContracts,
    validation: &dyn NativeEvidenceValidation,
    instructions: &JsString,
) -> IvyResult<&'v Json> {
    verify_chat_native_request(
        store,
        definition,
        call,
        request,
        catalogs,
        validation,
        instructions,
    )
    .await?;
    let source = catalog(
        catalogs,
        field(request, "nativeVersion")?,
        Some(field(request, "catalogSourceHash")?),
    )?;
    validate_native_operation(
        value,
        &NativeOperationIdentity {
            owner: owner(call, &source)?,
            caller_principal_id: string(call, "callerPrincipalId")?.clone(),
            operation_id: string(call, "operationId")?.clone(),
            method: string(call, "method")?.clone(),
            params: field(request, "params")?.clone(),
        },
        Some("chat_native_mismatch"),
    )?;
    validation.validate_payload(
        &source,
        value,
        NATIVE_REQUEST_FRAME_BYTES,
        NATIVE_FRAME_BYTES,
    )?;
    Ok(value)
}
pub fn verify_chat_native_read(
    value: &Json,
    expected: &Json,
    catalog_source_hash: Option<&Json>,
    catalogs: &CheckedNativeContracts,
    validation: &dyn NativeEvidenceValidation,
) -> IvyResult<()> {
    let source = catalog(
        catalogs,
        field(expected, "nativeVersion")?,
        catalog_source_hash,
    )?;
    validate_native_read(
        value,
        &NativeReadIdentity {
            owner: owner(expected, &source)?,
            caller_principal_id: string(expected, "callerPrincipalId")?.clone(),
            epoch: string(expected, "epoch")?.clone(),
            method: string(expected, "method")?.clone(),
            params: field(expected, "params")?.clone(),
        },
        Some("chat_native_mismatch"),
    )?;
    validation.validate_payload(
        &source,
        value,
        NATIVE_REQUEST_FRAME_BYTES,
        NATIVE_FRAME_BYTES,
    )
}

pub struct ChatNativeEvidence<'a> {
    pub store: &'a ChatStore,
    definition: Json,
    catalogs: &'a CheckedNativeContracts,
    validation: &'a dyn NativeEvidenceValidation,
    instructions: &'a JsString,
}
impl<'a> ChatNativeEvidence<'a> {
    pub fn new(
        store: &'a ChatStore,
        definition: &Json,
        catalogs: &'a CheckedNativeContracts,
        validation: &'a dyn NativeEvidenceValidation,
        instructions: &'a JsString,
    ) -> IvyResult<Self> {
        let admission = ChatAdmission::new(definition)?;
        require(
            definition.get("rootObjectId")
                == Some(
                    &store
                        .root_object_id
                        .as_deref()
                        .map(Json::from)
                        .unwrap_or(Json::Null),
                ),
            "chat_scope_mismatch",
            "Native evidence must use the configured ChatBridge root.",
        )?;
        Ok(Self {
            store,
            definition: admission.definition().clone(),
            catalogs,
            validation,
            instructions,
        })
    }
    pub fn definition(&self) -> &Json {
        &self.definition
    }
    async fn scope(&self, parent_id: &str) -> IvyResult<()> {
        let local = self
            .store
            .local_info(parent_id)
            .await
            .map_err(|e| e.into_ivy())?;
        let key = if local.is_some_and(|(key, _)| key == "chat-bridge/operation") {
            "chat-bridge/operation"
        } else {
            "chat-bridge/input"
        };
        let parent = self
            .store
            .read(key, Reference::Current(parent_id))
            .await
            .map_err(|e| e.into_ivy())?;
        require(
            parent.value.get("workspaceId") == self.definition.get("workspaceId")
                && parent.value.get("definitionHash") == Some(&hash_json(&self.definition)?.into()),
            "chat_scope_mismatch",
            "Native evidence cannot attach to another workspace or configuration under the same Hive root.",
        )
    }
    pub async fn save_request(&self, parent_id: &str, request: &Json) -> IvyResult<Pin> {
        self.validation.validate_draft(request)?;
        let original = request.clone();
        self.scope(parent_id).await?;
        save_chat_evidence(
            self.store,
            parent_id,
            "native-request/canonical-json",
            &original,
            &|v| self.validation.validate_draft(v),
        )
        .await
    }
    async fn read_request(&self, parent_id: &str, call: &Json) -> IvyResult<Json> {
        self.scope(parent_id).await?;
        read_chat_evidence(
            self.store,
            parent_id,
            &pin(field(call, "request")?)?,
            "native-request/canonical-json",
            &|v| self.validation.validate_draft(v),
        )
        .await
    }
    pub async fn request(&self, parent_id: &str, call: &Json) -> IvyResult<Json> {
        let value = self.read_request(parent_id, call).await?;
        verify_chat_native_request(
            self.store,
            &self.definition,
            call,
            &value,
            self.catalogs,
            self.validation,
            self.instructions,
        )
        .await?;
        Ok(value)
    }
    pub async fn save_operation(
        &self,
        parent_id: &str,
        call: &Json,
        value: &Json,
    ) -> IvyResult<Pin> {
        let request = self.read_request(parent_id, call).await?;
        verify_chat_native_operation(
            self.store,
            &self.definition,
            call,
            &request,
            value,
            self.catalogs,
            self.validation,
            self.instructions,
        )
        .await?;
        save_chat_evidence(
            self.store,
            parent_id,
            "agent-operation/canonical-json",
            value,
            &|v| self.validation.validate_draft(v),
        )
        .await
    }
    pub async fn operation(
        &self,
        parent_id: &str,
        call: &Json,
        reference: &Pin,
    ) -> IvyResult<Json> {
        let request = self.read_request(parent_id, call).await?;
        let value = read_chat_evidence(
            self.store,
            parent_id,
            reference,
            "agent-operation/canonical-json",
            &|v| self.validation.validate_draft(v),
        )
        .await?;
        verify_chat_native_operation(
            self.store,
            &self.definition,
            call,
            &request,
            &value,
            self.catalogs,
            self.validation,
            self.instructions,
        )
        .await?;
        Ok(value)
    }
    fn read_owner(&self, expected: &Json) -> IvyResult<()> {
        require(
            expected.get("serviceNodeId")
                == field(&self.definition, "project")?.get("serviceNodeId")
                && expected.get("callerPrincipalId") == self.definition.get("principalId")
                && expected.get("nativeVersion")
                    == field(&self.definition, "nativePlan")?.get("nativeVersion"),
            "chat_native_mismatch",
            "The native read must use the configured ChatBridge owner.",
        )
    }
    pub async fn save_read(
        &self,
        parent_id: &str,
        expected: &Json,
        value: &Json,
    ) -> IvyResult<Pin> {
        self.read_owner(expected)?;
        verify_chat_native_read(
            value,
            expected,
            Some(field(
                field(&self.definition, "nativePlan")?,
                "catalogSourceHash",
            )?),
            self.catalogs,
            self.validation,
        )?;
        self.scope(parent_id).await?;
        save_chat_evidence(
            self.store,
            parent_id,
            "agent-read-observation/canonical-json",
            value,
            &|v| self.validation.validate_draft(v),
        )
        .await
    }
    pub async fn read(&self, parent_id: &str, expected: &Json, reference: &Pin) -> IvyResult<Json> {
        self.read_owner(expected)?;
        self.scope(parent_id).await?;
        let value = read_chat_evidence(
            self.store,
            parent_id,
            reference,
            "agent-read-observation/canonical-json",
            &|v| self.validation.validate_draft(v),
        )
        .await?;
        verify_chat_native_read(
            &value,
            expected,
            Some(field(
                field(&self.definition, "nativePlan")?,
                "catalogSourceHash",
            )?),
            self.catalogs,
            self.validation,
        )?;
        Ok(value)
    }
}
