//! Immutable native request parameters and the identity used for saved evidence.
//! These helpers neither dispatch native work nor authorize a replacement run.
use crate::{
    native_plan::{catalog, matches_primary, owner, routed_owner, routed_selection},
    operations::{Store, mutation},
    read_evidence::ReadEvidenceStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, native_backend::BackendIdentity,
};
use ivy_sdk::{
    native_evidence::NativeOwnerIdentity,
    native_parameters::{NATIVE_PARAMETERS_BYTES, read_native_parameters, save_native_parameters},
    native_plan::validate_native_invocation,
};

#[derive(Clone, Debug)]
pub struct EvidenceIdentity {
    pub backend: Option<BackendIdentity>,
    pub operation_id: JsString,
    pub caller_principal_id: JsString,
    pub service_node_id: JsString,
    pub native_version: JsString,
    pub method: JsString,
    pub params: Json,
}
impl EvidenceIdentity {
    pub fn to_json(&self) -> Json {
        let mut value = Json::object([
            ("operationId", Json::String(self.operation_id.clone())),
            (
                "callerPrincipalId",
                Json::String(self.caller_principal_id.clone()),
            ),
            ("serviceNodeId", Json::String(self.service_node_id.clone())),
            ("nativeVersion", Json::String(self.native_version.clone())),
            ("method", Json::String(self.method.clone())),
            ("params", self.params.clone()),
        ]);
        if let Some(backend) = &self.backend {
            value.insert("backend", backend.json());
        }
        value
    }
}

pub struct NativeIntent<'a, S> {
    pub store: &'a S,
    pub catalogs: &'a CheckedNativeContracts,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeSlot {
    Thread,
    Turn,
    Steer,
    Interrupt,
    Release,
}
impl NativeSlot {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Turn => "turn",
            Self::Steer => "steer",
            Self::Interrupt => "interrupt",
            Self::Release => "release",
        }
    }
}
/// Current prompt policy consumes Task headers and their question summary.
/// An omitted execution-change flag uses the normal prompt.
pub trait TaskPrompts {
    fn task_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        execution_change_requested: Option<&Json>,
    ) -> IvyResult<JsString>;
    fn task_update_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        execution_change_requested: bool,
    ) -> IvyResult<JsString>;
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn present_text(value: &Json) -> bool {
    value.as_js_string().is_some_and(|text| !text.0.is_empty())
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn validate_request(
    contract: &ivy_contracts::native_contract::NativeContract,
    request: &Json,
) -> IvyResult<()> {
    if !field(request, "method").is_str("agent.release") {
        return validate_native_invocation(contract, request, NATIVE_PARAMETERS_BYTES);
    }
    let mut input = request.required("params")?.clone();
    input.insert("operationId", "release-validation".into());
    input.insert("nativeVersion", request.required("nativeVersion")?.clone());
    input.insert(
        "expectedDefinitionHash",
        contract
            .definition_hash(&"thread/unsubscribe".into())?
            .into(),
    );
    if let Some(backend) = request.get("backend") {
        input.insert("backend", backend.clone());
    }
    ivy_contracts::agent_schema::validate_agent("ThreadReleaseInput", &input)?;
    ivy_contracts::json::canonical_size(&input, NATIVE_PARAMETERS_BYTES)?;
    Ok(())
}
fn parameters(value: &Json) -> Json {
    match value {
        Json::Object(_) => value.clone(),
        Json::Array(values) => Json::Object(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (index.to_string().into(), value.clone()))
                .collect(),
        ),
        Json::String(value) => Json::Object(
            value
                .0
                .iter()
                .enumerate()
                .map(|(index, unit)| {
                    (
                        index.to_string().into(),
                        Json::String(JsString(vec![*unit])),
                    )
                })
                .collect(),
        ),
        _ => Json::Object(vec![]),
    }
}
fn task_input(text: JsString) -> Json {
    Json::object([
        ("type", "text".into()),
        ("text", Json::String(text)),
        ("text_elements", Json::Array(vec![])),
    ])
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}

impl<S: Store + ReadEvidenceStore> NativeIntent<'_, S> {
    /// Materialize parameters from the Run's already resolved retained plan.
    /// This neither attaches/publishes a call nor dispatches native work.
    pub async fn intended_call<P: TaskPrompts>(
        &self,
        run: &Json,
        slot: NativeSlot,
        origin: Option<&Json>,
        prompts: &P,
    ) -> IvyResult<Json> {
        self.intended_call_for_task(run, slot, origin, prompts, None)
            .await
    }
    /// Reuse the caller's fenced Task snapshot instead of materializing it again.
    pub(crate) async fn intended_call_for_task<P: TaskPrompts>(
        &self,
        run: &Json,
        slot: NativeSlot,
        origin: Option<&Json>,
        prompts: &P,
        current_task: Option<&Json>,
    ) -> IvyResult<Json> {
        let initial = Json::object([("kind", "initial".into())]);
        let origin = origin.unwrap_or(&initial);
        self.store.validate("NativeCallOrigin", origin)?;
        let plan = field(run, "plan");
        let primary = field(run, "primaryResourceRef");
        let mut definition = "turnStart";
        let (method, params) = if slot == NativeSlot::Release {
            require(
                field(origin, "kind").is_str("release")
                    && field(run, "phase").is_str("completed")
                    && !field(run, "result").is_null(),
                "task_board_native_order",
                "Release requires a completed Run and its durable result.",
            )?;
            (
                "agent.release",
                Json::object([
                    ("threadId", primary.required("nativeId")?.clone()),
                    ("expectedEpoch", run.required("nativeEpoch")?.clone()),
                    (
                        "expectedOperationId",
                        run.required("contextRelease")?
                            .required("expectedOperationId")?
                            .clone(),
                    ),
                ]),
            )
        } else if field(origin, "kind").is_str("steer") {
            require(
                slot == NativeSlot::Steer
                    && primary.as_object().is_some()
                    && present_text(field(run, "turnId"))
                    && present_text(field(run, "nativeEpoch"))
                    && field(run, "cancellation").is_null(),
                "task_board_native_order",
                "Steering requires the exact active native turn.",
            )?;
            let task = self
                .store
                .read_task_header(run.required("taskId")?, true)
                .await?;
            let text = prompts.task_prompt(
                &task.value,
                string(run.required("owner")?, "serviceNodeId")?,
                origin.get("executionChangeRequested"),
            )?;
            (
                "turn/steer",
                Json::object([
                    ("threadId", primary.required("nativeId")?.clone()),
                    ("expectedTurnId", run.required("turnId")?.clone()),
                    ("input", Json::Array(vec![task_input(text)])),
                ]),
            )
        } else if field(origin, "kind").is_str("reattach") {
            require(
                slot == NativeSlot::Thread && matches_primary(run.required("target")?, primary)?,
                "task_board_primary_mismatch",
                "Reattachment requires the saved primary thread on the selected native owner.",
            )?;
            definition = "threadResume";
            let mut params = parameters(field(plan, "threadResume"));
            params.insert("threadId", primary.required("nativeId")?.clone());
            params.insert("excludeTurns", true.into());
            ("thread/resume", params)
        } else if slot == NativeSlot::Thread {
            let task = self.initial_task(run).await?;
            let primary = field(&task.value, "primaryResourceRef");
            require(
                primary.is_null() || matches_primary(run.required("target")?, primary)?,
                "task_board_primary_mismatch",
                "The original primary must belong to the selected native owner.",
            )?;
            if primary.as_object().is_some() {
                definition = "threadResume";
                let mut params = parameters(field(plan, "threadResume"));
                params.insert("threadId", primary.required("nativeId")?.clone());
                params.insert("excludeTurns", true.into());
                ("thread/resume", params)
            } else {
                definition = "threadStart";
                ("thread/start", field(plan, "threadStart").clone())
            }
        } else if slot == NativeSlot::Turn {
            require(
                primary.as_object().is_some() && present_text(field(run, "nativeEpoch")),
                "task_board_primary_required",
                "Turn preparation requires the saved primary and native epoch.",
            )?;
            let task = self.initial_task(run).await?;
            let text = prompts.task_prompt(
                &task.value,
                string(run.required("owner")?, "serviceNodeId")?,
                None,
            )?;
            let mut input = if field(field(run, "owner"), "source").is_str("scheduler") {
                vec![]
            } else {
                field(field(plan, "turnStart"), "input")
                    .as_array()
                    .unwrap_or(&[])
                    .to_vec()
            };
            input.push(task_input(text));
            let mut params = parameters(field(plan, "turnStart"));
            params.insert("threadId", primary.required("nativeId")?.clone());
            params.insert("input", Json::Array(input));
            ("turn/start", params)
        } else {
            require(
                primary.as_object().is_some()
                    && present_text(field(run, "turnId"))
                    && field(run, "cancellation").as_object().is_some(),
                "task_board_cancellation_required",
                "Interruption requires the exact known turn and saved cancellation intent.",
            )?;
            definition = "turnInterrupt";
            (
                "turn/interrupt",
                Json::object([
                    ("threadId", primary.required("nativeId")?.clone()),
                    ("turnId", run.required("turnId")?.clone()),
                ]),
            )
        };
        let selection = match current_task {
            Some(task) => routed_selection(task, run)?,
            None => plan,
        };
        let mut request = Json::object([
            ("nativeVersion", field(selection, "nativeVersion").clone()),
            (
                "catalogSourceHash",
                field(selection, "catalogSourceHash").clone(),
            ),
            ("method", method.into()),
            ("params", params),
        ]);
        let contract = catalog(self.catalogs, selection)?;
        let selected = match current_task {
            Some(task) => routed_owner(self.catalogs, task, run)?,
            None => owner(self.catalogs, run.required("target")?, plan)?,
        };
        let contract = contract.with_backend(selected.backend.clone());
        if let Some(backend) = contract.backend() {
            request.insert("backend", backend.json());
        }
        validate_request(&contract, &request)?;
        let expected = if slot == NativeSlot::Release
            || slot == NativeSlot::Steer
            || selection.get("nativeVersion") != plan.get("nativeVersion")
            || selection.get("catalogSourceHash") != plan.get("catalogSourceHash")
            || selected.backend
                != ivy_contracts::native_backend::BackendIdentity::optional(plan.get("backend"))?
        {
            contract
                .definition_hash(crate::native_release::definition(&method.into()))?
                .into()
        } else {
            field(plan, "definitions").required(definition)?.clone()
        };
        let mut value = Json::object([("request", request), ("expectedDefinitionHash", expected)]);
        if selected.backend.is_some() || slot == NativeSlot::Release {
            value.insert("owner", selected.to_json());
        }
        Ok(value)
    }

    async fn initial_task(&self, run: &Json) -> IvyResult<crate::operations::Document> {
        self.store
            .read_task_header(
                &Json::object([
                    ("objectId", run.required("taskId")?.clone()),
                    (
                        "revision",
                        run.required("originalRequest")?
                            .required("expectedRevision")?
                            .clone(),
                    ),
                ]),
                true,
            )
            .await
    }

    pub fn retain_native_request(
        &self,
        request: &Json,
        operation_id: &JsString,
    ) -> IvyResult<Json> {
        let contract = catalog(self.catalogs, request)?;
        validate_request(&contract, request)?;
        if field(request, "method").is_str("agent.release") {
            let mut retained = request.clone();
            retained.insert(
                "params",
                Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("nativeVersion", request.required("nativeVersion")?.clone()),
                    (
                        "nativeExecutableHash",
                        contract.catalog().required("nativeExecutableHash")?.clone(),
                    ),
                    ("catalogHash", contract.catalog_hash().into()),
                    ("method", "agent.release".into()),
                    ("reservedFields", Json::Array(vec![])),
                    ("params", request.required("params")?.clone()),
                ]),
            );
            ivy_contracts::agent_schema::validate_agent(
                "ParametersRef",
                retained.required("params")?,
            )?;
            return Ok(retained);
        }
        let parent = self.store.root_object_id();
        // Preserve the original hash/error boundary even though inline parameter
        // references do not publish an Object or consume this mutation identity.
        mutation(operation_id, "native-parameters")?;
        let params = save_native_parameters(
            &contract,
            string(request, "method")?,
            request.required("params")?,
            &[],
            parent.as_ref(),
        )?;
        let mut retained = request.clone();
        retained.insert("params", params);
        Ok(retained)
    }

    pub fn resolve_native_request(&self, request: &Json) -> IvyResult<Json> {
        self.store.validate("NativeRequest", request)?;
        let contract = catalog(self.catalogs, request)?;
        if field(request, "method").is_str("agent.release") {
            let params = request.required("params")?;
            require(
                field(params, "method").is_str("agent.release")
                    && params.get("nativeVersion") == request.get("nativeVersion")
                    && params.get("nativeExecutableHash")
                        == contract.catalog().get("nativeExecutableHash")
                    && field(params, "catalogHash").as_js_string()
                        == Some(&contract.catalog_hash().into())
                    && params
                        .get("reservedFields")
                        .and_then(Json::as_array)
                        .is_some_and(|fields| fields.is_empty()),
                "task_board_native_intent_mismatch",
                "Managed release parameters must retain their exact checked owner.",
            )?;
            let mut resolved = request.clone();
            resolved.insert("params", params.required("params")?.clone());
            validate_request(&contract, &resolved)?;
            return Ok(resolved);
        }
        let params = read_native_parameters(
            &contract,
            string(request, "method")?,
            request.required("params")?,
            &[],
            self.store.root_object_id().as_ref(),
        )?;
        let mut resolved = request.clone();
        resolved.insert("params", params);
        validate_native_invocation(&contract, &resolved, NATIVE_PARAMETERS_BYTES)?;
        Ok(resolved)
    }

    /// Calls created before explicit backend selection retain the immutable
    /// original request target. New scoped calls carry their own owner snapshot.
    pub fn call_owner(&self, run: &Json, call: &Json) -> IvyResult<NativeOwnerIdentity> {
        crate::native_plan::call_owner(self.catalogs, run, call)
    }

    pub fn evidence_identity(
        &self,
        run: &Json,
        call: &Json,
        principal_id: &JsString,
    ) -> IvyResult<EvidenceIdentity> {
        let request = call.required("request")?;
        let operation_id = string(call, "operationId")?.clone();
        if call.get("owner").is_none() {
            let target = run
                .get("originalRequest")
                .and_then(|value| value.get("target"))
                .unwrap_or(run.required("target")?);
            string(target, "serviceNodeId")?;
        }
        let native_version = string(request, "nativeVersion")?.clone();
        let method = string(request, "method")?.clone();
        let resolved = self.resolve_native_request(request)?;
        let owner = self.call_owner(run, call)?;
        Ok(EvidenceIdentity {
            backend: owner.backend,
            operation_id,
            caller_principal_id: principal_id.clone(),
            service_node_id: owner.service_node_id,
            native_version,
            method,
            params: resolved.required("params")?.clone(),
        })
    }
}
