//! Immutable native request parameters and the identity used for saved evidence.
//! These helpers neither dispatch native work nor authorize a replacement run.
use crate::{
    native_plan::catalog,
    operations::{Store, mutation},
    read_evidence::ReadEvidenceStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, hash::hash_json,
};
use ivy_sdk::{
    native_parameters::{NATIVE_PARAMETERS_BYTES, read_native_parameters, save_native_parameters},
    native_plan::validate_native_invocation,
};

#[derive(Clone, Debug)]
pub struct EvidenceIdentity {
    pub operation_id: JsString,
    pub caller_principal_id: JsString,
    pub service_node_id: JsString,
    pub native_version: JsString,
    pub method: JsString,
    pub params: Json,
}
impl EvidenceIdentity {
    pub fn to_json(&self) -> Json {
        Json::object([
            ("operationId", Json::String(self.operation_id.clone())),
            (
                "callerPrincipalId",
                Json::String(self.caller_principal_id.clone()),
            ),
            ("serviceNodeId", Json::String(self.service_node_id.clone())),
            ("nativeVersion", Json::String(self.native_version.clone())),
            ("method", Json::String(self.method.clone())),
            ("params", self.params.clone()),
        ])
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
}
impl NativeSlot {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Turn => "turn",
            Self::Steer => "steer",
            Self::Interrupt => "interrupt",
        }
    }
}
/// Current prompt policy. An omitted execution-change flag uses the normal prompt.
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
        let initial = Json::object([("kind", "initial".into())]);
        let origin = origin.unwrap_or(&initial);
        self.store.validate("NativeCallOrigin", origin)?;
        let plan = field(run, "plan");
        let primary = field(run, "primaryResourceRef");
        let mut definition = "turnStart";
        let (method, params) = if field(origin, "kind").is_str("steer") {
            require(
                slot == NativeSlot::Steer
                    && primary.as_object().is_some()
                    && present_text(field(run, "turnId"))
                    && present_text(field(run, "nativeEpoch"))
                    && field(run, "cancellation").is_null(),
                "task_board_native_order",
                "Steering requires the exact active native turn.",
            )?;
            let task =
                Store::read(self.store, "task-board/task", run.required("taskId")?, true).await?;
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
                slot == NativeSlot::Thread
                    && field(primary, "namespace").is_str("codex")
                    && field(primary, "kind").is_str("thread")
                    && primary.get("serviceNodeId") == field(run, "target").get("serviceNodeId"),
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
                primary.is_null()
                    || (field(primary, "namespace").is_str("codex")
                        && field(primary, "kind").is_str("thread")
                        && primary.get("serviceNodeId")
                            == field(run, "target").get("serviceNodeId")),
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
        let request = Json::object([
            ("nativeVersion", field(plan, "nativeVersion").clone()),
            (
                "catalogSourceHash",
                field(plan, "catalogSourceHash").clone(),
            ),
            ("method", method.into()),
            ("params", params),
        ]);
        let contract = catalog(self.catalogs, plan)?;
        validate_native_invocation(&contract, &request, NATIVE_PARAMETERS_BYTES)?;
        let expected = if slot == NativeSlot::Steer {
            hash_json(contract.definition(&method.into())?)?.into()
        } else {
            field(plan, "definitions").required(definition)?.clone()
        };
        Ok(Json::object([
            ("request", request),
            ("expectedDefinitionHash", expected),
        ]))
    }

    async fn initial_task(&self, run: &Json) -> IvyResult<crate::operations::Document> {
        Store::read(
            self.store,
            "task-board/task",
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
        validate_native_invocation(&contract, request, NATIVE_PARAMETERS_BYTES)?;
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

    pub fn evidence_identity(
        &self,
        run: &Json,
        call: &Json,
        principal_id: &JsString,
    ) -> IvyResult<EvidenceIdentity> {
        let request = call.required("request")?;
        let operation_id = string(call, "operationId")?.clone();
        let service_node_id = string(run.required("target")?, "serviceNodeId")?.clone();
        let native_version = string(request, "nativeVersion")?.clone();
        let method = string(request, "method")?.clone();
        let resolved = self.resolve_native_request(request)?;
        Ok(EvidenceIdentity {
            operation_id,
            caller_principal_id: principal_id.clone(),
            service_node_id,
            native_version,
            method,
            params: resolved.required("params")?.clone(),
        })
    }
}
