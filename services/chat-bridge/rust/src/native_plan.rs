//! Chat-specific native preferences. Retained-plan loading and native schema
//! validation use the shared SDK; this module owns the domain overlay only.
use crate::{
    main_context::{main_thread_options, receiver, spread, truthy},
    native_evidence::{EvidenceFuture, NativeEvidenceValidation},
    store::ChatStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, chat_schema::validate_chat,
    checked_native_contract::CheckedNativeContracts, hash::hash_json,
    limits::NATIVE_REQUEST_FRAME_BYTES, native_contract::NativeContract,
};
use ivy_sdk::{
    native_evidence::{NativePayloadLimits, validate_native_payload},
    native_plan::{NativePlanKind, read_native_plan, validate_native_invocation},
};

fn selected(
    catalogs: &CheckedNativeContracts,
    value: &Json,
) -> IvyResult<std::sync::Arc<NativeContract>> {
    if value.is_null() {
        return Err(IvyError::internal());
    }
    let missing = JsString::from("");
    catalogs.checked_native_contract(
        value
            .get("nativeVersion")
            .and_then(Json::as_js_string)
            .unwrap_or(&missing),
        Some(&Json::object([(
            "sourceHash",
            value
                .get("catalogSourceHash")
                .cloned()
                .unwrap_or(Json::Null),
        )])),
    )
}
pub fn validate_chat_native_draft(
    catalogs: &CheckedNativeContracts,
    value: &Json,
) -> IvyResult<()> {
    validate_chat("NativeRequest", value)?;
    validate_native_invocation(
        selected(catalogs, value)?.as_ref(),
        value,
        NATIVE_REQUEST_FRAME_BYTES,
    )
}
/// Retain four successful original plan reads per authenticated Store, then
/// apply Main's context to a fresh clone. Cached data grants no live authority.
pub fn resolve_chat_plan(
    store: &ChatStore,
    catalogs: &CheckedNativeContracts,
    plan: &Json,
    instructions: &JsString,
) -> IvyResult<Json> {
    let key = hash_json(&Json::object([
        ("plan", plan.clone()),
        (
            "parentId",
            store
                .root_object_id
                .as_deref()
                .map(Json::from)
                .unwrap_or(Json::Null),
        ),
    ]))?;
    let mut resolved = {
        let mut cache = store
            .native_plans
            .lock()
            .map_err(|_| IvyError::internal())?;
        if let Some((_, value)) = cache.iter().find(|(saved, _)| saved == &key) {
            value.clone()
        } else {
            if cache.len() >= 4 {
                cache.pop_front();
            }
            // Shared plan reads are synchronous and do not perform RPC. Keep
            // insertion/eviction serialized without holding a guard over await.
            let parent = store.root_object_id.as_deref().map(JsString::from);
            let value = read_native_plan(
                selected(catalogs, plan)?.as_ref(),
                plan,
                NativePlanKind::Chat,
                parent.as_ref(),
            )?;
            cache.push_back((key, value.clone()));
            value
        }
    };
    for key in ["threadStart", "threadResume"] {
        let value = main_thread_options(resolved.required(key)?, instructions)?;
        resolved.insert(key, value);
    }
    Ok(resolved)
}
/// Production implementation of every native plan/compiler boundary used by
/// Chat evidence. Current owner, project and workflow guards remain mandatory.
pub struct ChatNativeValidation<'a> {
    catalogs: &'a CheckedNativeContracts,
    instructions: &'a JsString,
}
impl<'a> ChatNativeValidation<'a> {
    pub fn new(catalogs: &'a CheckedNativeContracts, instructions: &'a JsString) -> Self {
        Self {
            catalogs,
            instructions,
        }
    }
}
impl NativeEvidenceValidation for ChatNativeValidation<'_> {
    fn validate_draft(&self, value: &Json) -> IvyResult<()> {
        validate_chat_native_draft(self.catalogs, value)
    }
    fn resolve_plan<'a>(
        &'a self,
        store: &'a ChatStore,
        plan: &'a Json,
    ) -> EvidenceFuture<'a, Json> {
        Box::pin(async move { resolve_chat_plan(store, self.catalogs, plan, self.instructions) })
    }
    fn validate_input(
        &self,
        contract: &NativeContract,
        method: &JsString,
        params: &Json,
        maximum_bytes: usize,
    ) -> IvyResult<()> {
        contract.validate_input(method, params, maximum_bytes)
    }
    fn validate_payload(
        &self,
        contract: &NativeContract,
        observation: &Json,
        request_bytes: usize,
        response_bytes: usize,
    ) -> IvyResult<()> {
        validate_native_payload(
            contract,
            observation,
            NativePayloadLimits {
                request_bytes,
                response_bytes,
            },
        )
    }
}

/// Native collaboration settings take precedence over top-level model/effort.
/// A falsey preference leaves its corresponding retained collaboration setting.
pub fn chat_turn_options(template: &Json, options: Option<&Json>) -> IvyResult<Json> {
    receiver(template)?;
    let mut result = spread(template);
    if let Some(options) = options {
        for (key, value) in spread(options).as_object().unwrap() {
            result.insert(key.clone(), value.clone());
        }
    }
    if let Some(mode) = template
        .get("collaborationMode")
        .filter(|v| v.as_object().is_some())
    {
        if let Some(settings) = mode.get("settings").filter(|v| v.as_object().is_some()) {
            let mut settings = settings.clone();
            for (source, target) in [("model", "model"), ("effort", "reasoning_effort")] {
                if let Some(value) = options.and_then(|v| v.get(source)).filter(|v| truthy(v)) {
                    settings.insert(target, value.clone());
                }
            }
            let mut mode = mode.clone();
            mode.insert("settings", settings);
            result.insert("collaborationMode", mode);
        }
    }
    Ok(result)
}
