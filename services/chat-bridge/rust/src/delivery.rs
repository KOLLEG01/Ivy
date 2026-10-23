//! One queue-head delivery pass. The input retains its original resume/turn
//! calls; completion, publication and cancellation remain separate lanes.
use crate::{
    chat_main::ChatMain,
    collector::{CollectionHead, CollectionRuntime},
    input_media::admitted_native_input,
    main_context::{main_input_origin, main_turn_context},
    native_driver::{ChatNativeDriver, NativeOrigin, native_path},
    native_plan::{chat_turn_options, resolve_chat_plan},
    notice_input::admitted_notice_input,
    operations::contention,
    publisher::PublicationFuture,
    store::{ChatStore, Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, agent_schema::validate_agent, hash::hash_json,
    json::canonical, limits::MANAGEMENT_FRAME_BYTES, operation::derive_operation_id_js,
};
use ivy_sdk::{
    native_observations::NativeObservations,
    native_owner::{GuardFuture, NativeDispatchGuard},
};

fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn text<'a>(v: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    v.required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn pin(v: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: v.required("objectId")?.as_str()?,
        revision: v
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    })
}
fn present(v: &Json, key: &str) -> bool {
    v.get(key).is_some_and(|value| !value.is_null())
}
fn state(v: &Json, value: &str) -> bool {
    v.get("state") == Some(&Json::from(value))
}
fn slot_pin(input: &Json, slot: Slot) -> IvyResult<&Json> {
    input.required("nativeCalls")?.required(slot.name())
}
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Resume,
    Turn,
}
impl Slot {
    fn name(self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::Turn => "turn",
        }
    }
}

pub struct DeliveryHead {
    pub input: Document,
    pub binding: Document,
    pub request: Json,
}
pub struct ChatDelivery<'a> {
    pub main: &'a ChatMain<'a>,
}
impl<'a> ChatDelivery<'a> {
    pub fn new(main: &'a ChatMain<'a>) -> Self {
        Self { main }
    }
    pub fn store(&self) -> &ChatStore {
        self.main.store()
    }
    pub fn native(&self) -> &ChatNativeDriver<'_> {
        self.main.native
    }
    pub async fn head(&self, input_id: Option<&str>) -> IvyResult<Option<DeliveryHead>> {
        self.native().verify_owner().await?;
        let main = self.main.queue().main().await?;
        let ticket = main
            .value
            .required("queue")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .first();
        require(
            !present(&main.value, "pendingAction"),
            "chat_pending_action",
            "Finish the original Main action before native queue work.",
        )?;
        let Some(ticket) = ticket else {
            require(
                input_id.is_none_or(str::is_empty),
                "revision_conflict",
                "The selected queue head has already finished.",
            )?;
            return Ok(None);
        };
        require(
            input_id.is_none_or(|id| id.is_empty() || ticket.get("inputId") == Some(&id.into())),
            "revision_conflict",
            "Only the current queue head may deliver native input.",
        )?;
        let input = self
            .store()
            .read(
                "chat-bridge/input",
                Reference::Current(&ticket.required("inputId")?.as_str()?),
            )
            .await
            .map_err(|e| e.into_ivy())?;
        let caller = text(input.value.required("identity")?, "senderPrincipalId")?;
        let operation = self
            .main
            .operations
            .find(caller, text(&input.value, "operationId")?)
            .await?;
        require(
            operation.is_some(),
            "chat_identity_conflict",
            "Queue delivery requires its original retained action.",
        )?;
        let operation = operation.unwrap();
        let request = operation.value.required("request")?;
        let admission = self.main.admission();
        let (identity, request_hash, payload) = if request.get("action") == Some(&"notify".into()) {
            let admitted = admitted_notice_input(
                admission,
                caller,
                request,
                input.value.required("binding")?,
                self.main.notice_instructions(),
            )?;
            (admitted.identity, admitted.request_hash, admitted.payload)
        } else {
            require(
                request.get("action") == Some(&"send".into()),
                "chat_identity_conflict",
                "Only an admitted user message or service handoff may enter the Main queue.",
            )?;
            let admitted = admission.send(caller, request)?;
            (
                admitted.identity,
                admitted.request_hash,
                request.required("payload")?.clone(),
            )
        };
        require(
            input.value.get("workspaceId") == admission.definition().get("workspaceId")
                && input.value.get("definitionHash") == Some(&admission.definition_hash().into())
                && same(ticket.required("identity")?, &identity)?
                && same(input.value.required("identity")?, &identity)?
                && ticket.get("requestHash") == Some(&request_hash.clone().into())
                && input.value.get("requestHash") == Some(&request_hash.into())
                && same(input.value.required("payload")?, &payload)?
                && same(
                    ticket.required("binding")?,
                    input.value.required("binding")?,
                )?
                && ticket.get("sequence") == input.value.get("sequence")
                && ticket.get("admittedAt") == input.value.get("admittedAt"),
            "chat_identity_conflict",
            "Queue delivery must preserve its original admitted payload, caller, binding and sequence.",
        )?;
        let binding = self
            .store()
            .read(
                "chat-bridge/binding",
                Reference::Pinned(&pin(input.value.required("binding")?)?),
            )
            .await
            .map_err(|e| e.into_ivy())?;
        let definition = admission.definition();
        let primary = binding.value.required("primary")?;
        require(
            binding.value.get("workspaceId") == definition.get("workspaceId")
                && binding.value.get("definitionHash") == Some(&admission.definition_hash().into())
                && same(
                    binding.value.required("project")?,
                    definition.required("project")?,
                )?
                && same(
                    binding.value.required("nativePlan")?,
                    definition.required("nativePlan")?,
                )?
                && primary.get("serviceNodeId")
                    == definition.required("project")?.get("serviceNodeId")
                && primary.get("namespace") == Some(&"codex".into())
                && primary.get("kind") == Some(&"thread".into()),
            "chat_binding_mismatch",
            "The original input must keep its configured native project, plan and owning primary.",
        )?;
        Ok(Some(DeliveryHead {
            input,
            binding,
            request: request.clone(),
        }))
    }
    fn plan(&self, binding: &Json) -> IvyResult<Json> {
        resolve_chat_plan(
            self.store(),
            self.native().catalogs(),
            binding.required("nativePlan")?,
            self.native().instructions(),
        )
    }
    async fn inspect(
        &self,
        _input: &Document,
        binding: &Json,
        status: &Json,
        allow_unloaded: bool,
    ) -> IvyResult<bool> {
        let primary = binding.required("primary")?;
        let plan = binding.required("nativePlan")?;
        let expected = Json::object([
            ("serviceNodeId", primary.required("serviceNodeId")?.clone()),
            (
                "callerPrincipalId",
                self.main
                    .admission()
                    .definition()
                    .required("principalId")?
                    .clone(),
            ),
            ("nativeVersion", plan.required("nativeVersion")?.clone()),
            ("epoch", status.required("epoch")?.clone()),
            ("method", "thread/read".into()),
            (
                "params",
                Json::object([
                    ("threadId", primary.required("nativeId")?.clone()),
                    ("includeTurns", false.into()),
                ]),
            ),
        ]);
        let observed = self
            .native()
            .management(
                "read",
                Json::object([
                    ("nativeVersion", expected.required("nativeVersion")?.clone()),
                    ("method", expected.required("method")?.clone()),
                    ("params", expected.required("params")?.clone()),
                ]),
            )
            .await?;
        self.native()
            .evidence()?
            .verify_read(&observed, &expected)?;
        let reply = observed.required("reply")?;
        let unloaded = JsString::from(format!(
            "thread not loaded: {}",
            text(primary, "nativeId")?.to_utf8()?
        ));
        if allow_unloaded
            && reply.get("error").is_some_and(|e| {
                e.get("code") == Some(&Json::Number(-32600.0))
                    && e.get("message") == Some(&Json::String(unloaded))
            })
        {
            return Ok(false);
        }
        require(
            reply.get("result").is_some(),
            "chat_native_unavailable",
            "Native input needs an original successful primary observation.",
        )?;
        let source = self.native().catalogs().checked_native_contract(
            text(&expected, "nativeVersion")?,
            Some(&Json::object([(
                "sourceHash",
                plan.required("catalogSourceHash")?.clone(),
            )])),
        )?;
        let observed =
            NativeObservations::new(&source).native_thread_state(reply.required("result")?)?;
        require(
            observed.thread.get("id") == primary.get("nativeId")
                && observed.thread.get("ephemeral") == Some(&false.into())
                && observed
                    .thread
                    .get("cwd")
                    .is_some_and(|v| v.as_js_string().is_some())
                && native_path(text(&observed.thread, "cwd")?)?
                    == native_path(text(self.plan(binding)?.required("threadStart")?, "cwd")?)?,
            "chat_native_mismatch",
            "Input must target its original persistent native Main and cwd.",
        )?;
        require(
            observed.can_start_turn
                || (allow_unloaded
                    && ["idle", "systemError", "notLoaded"]
                        .iter()
                        .any(|s| observed.state == JsString::from(*s))),
            "chat_native_busy",
            "Current native activity or unavailable direct input blocks another Chat turn.",
        )?;
        Ok(observed.can_start_turn)
    }
    async fn attach(&self, input: &Document, slot: Slot, call: &Document) -> IvyResult<()> {
        let current = self.head(Some(&input.pin.object_id)).await?;
        require(
            current.is_some(),
            "revision_conflict",
            "The original queue head must remain current.",
        )?;
        let current = current.unwrap();
        require(
            current.input.pin == input.pin
                && !present(&current.input.value, "cancellation")
                && !present(&current.input.value, "result")
                && !present(&current.input.value, "turnId"),
            "revision_conflict",
            "Native delivery requires its unchanged unfinished input.",
        )?;
        let previous = slot_pin(&input.value, slot)?;
        require(
            (previous).is_null()
                || previous.get("objectId") == Some(&call.pin.object_id.clone().into())
                || (slot == Slot::Resume && same(call.value.required("predecessor")?, previous)?),
            "chat_native_mismatch",
            "Only a retained resume predecessor may replace an attached input call.",
        )?;
        if same(previous, &call.pin.json())? {
            return Ok(());
        }
        let failed = state(&call.value, "failed");
        let unknown = state(&call.value, "outcome_unknown");
        let error = if failed || unknown {
            Json::object([
                ("code", call.value.required("code")?.clone()),
                (
                    "outcome",
                    if failed && call.value.get("epoch") == Some(&Json::Null) {
                        "not_executed"
                    } else {
                        "unknown"
                    }
                    .into(),
                ),
                (
                    "detail",
                    format!(
                        "The original native input call {}.",
                        call.value.required("state")?.as_str()?
                    )
                    .into(),
                ),
            ])
        } else {
            Json::Null
        };
        let mut turn_id = Json::Null;
        if slot == Slot::Turn && state(&call.value, "succeeded") {
            require(
                present(&call.value, "evidence"),
                "chat_native_mismatch",
                "Native turn delivery needs its saved original receipt.",
            )?;
            let original = self
                .native()
                .evidence()?
                .operation(
                    &input.pin.object_id,
                    &call.value,
                    &pin(call.value.required("evidence")?)?,
                )
                .await?;
            let reply = original.get("reply");
            require(
                reply.is_some_and(|r| !(r).is_null() && r.get("result").is_some()),
                "chat_native_mismatch",
                "The original native turn needs its successful reply.",
            )?;
            let turn = reply.unwrap().required("result")?.required("turn")?;
            require(
                turn.get("id").is_some_and(|id| id.as_js_string().is_some()),
                "chat_native_mismatch",
                "The native reply must name its original turn.",
            )?;
            turn_id = turn.required("id")?.clone();
        }
        let mut calls = input.value.required("nativeCalls")?.clone();
        calls.insert(slot.name(), call.pin.json());
        let mut value = input.value.clone();
        value.insert("nativeCalls", calls);
        value.insert("turnId", turn_id.clone());
        value.insert("epoch", call.value.required("epoch")?.clone());
        value.insert(
            "state",
            if failed {
                "failed"
            } else if unknown {
                "outcome_unknown"
            } else if !(&turn_id).is_null() {
                "running"
            } else if slot == Slot::Turn {
                "delivering"
            } else {
                "preparing"
            }
            .into(),
        );
        value.insert("error", error);
        value.insert(
            "finishedAt",
            if failed {
                call.value.required("updatedAt")?.clone()
            } else {
                Json::Null
            },
        );
        value.insert("updatedAt", call.value.required("updatedAt")?.clone());
        self.store()
            .write(
                "chat-bridge/input",
                &value,
                &mutation(
                    &input.pin.object_id,
                    &format!(
                        "native:{}:{}:{}",
                        slot.name(),
                        input.pin.revision,
                        hash_json(&value)?
                    ),
                )?,
                Destination::Update {
                    object_id: &input.pin.object_id,
                    expected_revision: input.pin.revision,
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        Ok(())
    }
    async fn resume(&self, input: &Document, binding: &Json, status: &Json) -> IvyResult<()> {
        let previous = if !(slot_pin(&input.value, Slot::Resume)?).is_null() {
            Some(
                self.native()
                    .get(
                        &input.pin.object_id,
                        Reference::Pinned(&pin(slot_pin(&input.value, Slot::Resume)?)?),
                    )
                    .await?,
            )
        } else {
            None
        };
        let reattach = previous
            .as_ref()
            .is_some_and(|p| state(&p.value, "succeeded"));
        if previous.is_some() && !reattach {
            return self.advance(input, binding, Slot::Resume).await;
        }
        let loaded = self.inspect(input, binding, status, true).await?;
        if reattach
            && previous.as_ref().unwrap().value.get("epoch") == status.get("epoch")
            && loaded
        {
            return self.advance(input, binding, Slot::Resume).await;
        }
        let origin = if reattach {
            NativeOrigin {
                predecessor: Some(previous.unwrap().pin),
                prepared_epoch: Some(text(status, "epoch")?.clone()),
            }
        } else {
            NativeOrigin::default()
        };
        let origin_value = Json::object([
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
        ]);
        let plan = self.plan(binding)?;
        let mut params = plan.required("threadResume")?.clone();
        params.insert(
            "threadId",
            binding.required("primary")?.required("nativeId")?.clone(),
        );
        let request = Json::object([
            ("nativeVersion", plan.required("nativeVersion")?.clone()),
            (
                "catalogSourceHash",
                plan.required("catalogSourceHash")?.clone(),
            ),
            ("method", "thread/resume".into()),
            ("params", params),
        ]);
        let operation_id = derive_operation_id_js(
            text(&input.value, "operationId")?,
            &format!("chat:native:resume:{}", hash_json(&origin_value)?).into(),
        )?;
        let call = self
            .native()
            .prepare(&input.pin.object_id, &operation_id, &request, origin, true)
            .await?;
        self.attach(input, Slot::Resume, &call).await
    }
    async fn guard(
        &self,
        input: &Document,
        binding: &Json,
        slot: Slot,
        selected: &Pin,
    ) -> IvyResult<()> {
        let current = self.head(Some(&input.pin.object_id)).await?;
        let status = self.native().ready().await?;
        let valid = match &current {
            Some(c) => {
                c.input.pin == input.pin
                    && !present(&c.input.value, "cancellation")
                    && same(slot_pin(&c.input.value, slot)?, &selected.json())?
            }
            None => false,
        };
        require(
            valid,
            "revision_conflict",
            "The same queue head must still authorize its original attached native call.",
        )?;
        let current = current.unwrap();
        if slot == Slot::Turn {
            let resume = slot_pin(&current.input.value, Slot::Resume)?;
            if !(resume).is_null() {
                let attached = self
                    .native()
                    .get(&input.pin.object_id, Reference::Pinned(&pin(resume)?))
                    .await?;
                require(
                    state(&attached.value, "succeeded")
                        && attached.value.get("epoch") == status.get("epoch"),
                    "chat_reattach_required",
                    "Main must be attached in the current native epoch before the original turn can start.",
                )?;
            }
            require(
                self.inspect(&current.input, binding, &status, true).await?,
                "chat_reattach_required",
                "Main must accept direct input in the current native epoch.",
            )?;
        } else {
            self.inspect(&current.input, binding, &status, true).await?;
        }
        let final_head = self.head(Some(&input.pin.object_id)).await?;
        require(
            final_head.is_some_and(|h| h.input.pin == input.pin),
            "revision_conflict",
            "Input changed during the last native readiness observation.",
        )
    }
    async fn advance(&self, input: &Document, binding: &Json, slot: Slot) -> IvyResult<()> {
        let selected = slot_pin(&input.value, slot)?;
        require(
            !(selected).is_null(),
            "chat_native_mismatch",
            "A native input call must be attached before dispatch.",
        )?;
        let selected = pin(selected)?;
        let guard = DeliveryGuard {
            delivery: self,
            input,
            binding,
            slot,
            selected: &selected,
        };
        let call = self
            .native()
            .advance(&input.pin.object_id, &selected, &guard)
            .await?;
        self.attach(input, slot, &call).await
    }
    pub async fn step(&self) -> IvyResult<()> {
        self.main.recover_pending().await?;
        let Some(current) = self.head(None).await? else {
            return Ok(());
        };
        let input = &current.input;
        let binding = &current.binding.value;
        if current.request.get("action") == Some(&"notify".into()) {
            self.store()
                .remember_notice_input(&input.pin.object_id)
                .await
                .map_err(|e| e.into_ivy())?;
        }
        if ["cancellation", "turnId", "finishedAt", "result"]
            .iter()
            .any(|k| present(&input.value, k))
        {
            return Ok(());
        }
        let work: IvyResult<()> = async {
            if !(slot_pin(&input.value, Slot::Turn)?).is_null() {
                match self.advance(input, binding, Slot::Turn).await {
                    Err(e) if e.code == "chat_reattach_required" => {
                        self.resume(input, binding, &self.native().ready().await?)
                            .await?
                    }
                    Err(e) => return Err(e),
                    Ok(()) => {}
                }
                return Ok(());
            }
            let status = self.native().ready().await?;
            let attached = if !(slot_pin(&input.value, Slot::Resume)?).is_null() {
                Some(
                    self.native()
                        .get(
                            &input.pin.object_id,
                            Reference::Pinned(&pin(slot_pin(&input.value, Slot::Resume)?)?),
                        )
                        .await?,
                )
            } else {
                None
            };
            let needs_resume = match attached {
                Some(a) => {
                    !state(&a.value, "succeeded")
                        || a.value.get("epoch") != status.get("epoch")
                        || !same(&a.pin.json(), slot_pin(&input.value, Slot::Resume)?)?
                }
                None => !self.inspect(input, binding, &status, true).await?,
            };
            if needs_resume {
                self.resume(input, binding, &status).await?;
                return Ok(());
            }
            let capacity = self
                .native()
                .management("frameLimits", Json::object([] as [(&str, Json); 0]))
                .await?;
            validate_agent("FrameLimits", &capacity)?;
            require(
                [
                    "serviceNodeId",
                    "epoch",
                    "nativeVersion",
                    "nativeExecutableHash",
                    "catalogHash",
                ]
                .iter()
                .all(|k| capacity.get(k) == status.get(k)),
                "chat_native_unavailable",
                "Native input requires the current owner's verified frame capacity.",
            )?;
            let plan = self.plan(binding)?;
            let caller = text(input.value.required("identity")?, "senderPrincipalId")?;
            let origin = main_input_origin(caller, &current.request)?;
            let mut params = main_turn_context(
                &chat_turn_options(
                    plan.required("turnStart")?,
                    input.value.required("payload")?.get("turnOptions"),
                )?,
                &origin,
                self.native().instructions(),
            )?;
            params.insert(
                "threadId",
                binding.required("primary")?.required("nativeId")?.clone(),
            );
            let native_input = if current.request.get("action") == Some(&"notify".into()) {
                vec![Json::object([
                    ("type", "text".into()),
                    (
                        "text",
                        input.value.required("payload")?.required("text")?.clone(),
                    ),
                ])]
            } else {
                admitted_native_input(
                    self.main.admission(),
                    self.store().client.as_ref(),
                    caller,
                    &current.request,
                )
                .await?
                .input
            };
            params.insert("input", Json::Array(native_input));
            params.insert(
                "clientUserMessageId",
                mutation(&input.pin.object_id, "native-user-message")?.into(),
            );
            let request = Json::object([
                ("nativeVersion", plan.required("nativeVersion")?.clone()),
                (
                    "catalogSourceHash",
                    plan.required("catalogSourceHash")?.clone(),
                ),
                ("method", "turn/start".into()),
                ("params", params),
            ]);
            let operation_id = derive_operation_id_js(
                text(&input.value, "operationId")?,
                &"chat:native:turn/start".into(),
            )?;
            let call = self
                .native()
                .prepare(
                    &input.pin.object_id,
                    &operation_id,
                    &request,
                    NativeOrigin::default(),
                    false,
                )
                .await?;
            self.attach(input, Slot::Turn, &call).await
        }
        .await;
        match work {
            Err(e) if contention(&e) || e.code == "chat_native_busy" => Ok(()),
            Err(e)
                if ["chat_image_invalid", "chat_image_mismatch"].contains(&e.code.as_str())
                    && (slot_pin(&input.value, Slot::Turn)?).is_null() =>
            {
                let unchanged = self.head(Some(&input.pin.object_id)).await?;
                require(
                    unchanged.is_some_and(|h| h.input.pin == input.pin),
                    "revision_conflict",
                    "An image refusal must still own its unchanged undispatched input.",
                )?;
                let at = self.store().now_iso()?;
                let mut value = input.value.clone();
                value.insert("state", "failed".into());
                value.insert("finishedAt", at.clone().into());
                value.insert("updatedAt", at.into());
                value.insert(
                    "error",
                    Json::object([
                        ("code", Json::String(e.code_js())),
                        ("outcome", "not_executed".into()),
                        (
                            "detail",
                            e.message.chars().take(2048).collect::<String>().into(),
                        ),
                    ]),
                );
                self.store()
                    .write(
                        "chat-bridge/input",
                        &value,
                        &mutation(
                            &input.pin.object_id,
                            &format!("refuse-image:{}:{}", input.pin.revision, hash_json(&value)?),
                        )?,
                        Destination::Update {
                            object_id: &input.pin.object_id,
                            expected_revision: input.pin.revision,
                        },
                    )
                    .await
                    .map_err(|e| e.into_ivy())?;
                Ok(())
            }
            other => other,
        }
    }
}
struct DeliveryGuard<'a, 'b> {
    delivery: &'a ChatDelivery<'b>,
    input: &'a Document,
    binding: &'a Json,
    slot: Slot,
    selected: &'a Pin,
}
impl NativeDispatchGuard for DeliveryGuard<'_, '_> {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(
            self.delivery
                .guard(self.input, self.binding, self.slot, self.selected),
        )
    }
}
impl CollectionRuntime for ChatDelivery<'_> {
    fn head<'a>(
        &'a self,
        input_id: Option<&'a str>,
    ) -> PublicationFuture<'a, Option<CollectionHead>> {
        Box::pin(async move {
            Ok(ChatDelivery::head(self, input_id)
                .await?
                .map(|h| CollectionHead {
                    input: h.input,
                    binding: h.binding,
                }))
        })
    }
    fn call<'a>(&'a self, input_id: &'a str, selected: &'a Pin) -> PublicationFuture<'a, Document> {
        Box::pin(self.native().get(input_id, Reference::Pinned(selected)))
    }
    fn ready(&self) -> PublicationFuture<'_, Json> {
        Box::pin(self.native().ready())
    }
    fn read(&self, request: Json) -> PublicationFuture<'_, Json> {
        Box::pin(self.native().management("read", request))
    }
    fn verify_owner(&self) -> PublicationFuture<'_, ()> {
        Box::pin(self.native().verify_owner())
    }
    fn recover_pending(&self) -> PublicationFuture<'_, ()> {
        Box::pin(self.main.recover_pending())
    }
}
