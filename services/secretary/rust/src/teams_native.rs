//! Dormant native Teams reads. Capture/checkpoint publication is a separate workflow.
use crate::{
    engine::SecretaryEngine,
    inbox_evidence::source_identity,
    microsoft_schema::{MICROSOFT_METHODS, MicrosoftRecord, validate_microsoft},
    microsoft_store::{
        MicrosoftDocument, MicrosoftEpochBoundaries, MicrosoftGuard, MicrosoftGuardFuture,
        MicrosoftStore, native_call, pin_value,
    },
    outlook_projection::{microsoft_mcp, microsoft_profile},
    source_identity::catalogue_account_hash,
    teams_projection::{teams_arguments, teams_page, teams_source},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json, json::js_string};
use ivy_host_runtime::node_path::{Style, is_absolute};
use ivy_sdk::{
    clock::date_to_iso,
    native_evidence::{NativeOwnerIdentity, native_instant, native_operation_terminal},
    native_operation::{
        NativeOperationCall, NativeOperationJournal, NativeReconciliation,
        reconcile_native_operation_local,
    },
    native_owner::NativeOwner,
    operation_ids::new_operation_id,
    tools::ToolClients,
};
use std::sync::Arc;

fn check(value: bool, code: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(
            code,
            "Native Teams work requires its original account, source, calls and selected page.",
        ))
    }
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn instant(value: &IvyJson, key: &str) -> IvyResult<i64> {
    native_instant(&text(value, key)?, None)
}
fn object(value: Option<&IvyJson>) -> IvyResult<&IvyJson> {
    value.filter(|v| v.as_object().is_some()).ok_or_else(|| {
        IvyError::new(
            "microsoft_evidence_invalid",
            "Native Teams work requires its original account, source, calls and selected page.",
        )
    })
}
pub fn teams_name(kind: &str, value: &IvyJson) -> IvyResult<JsString> {
    Ok(format!("teams-{kind}-{}", &hash_json(value)?[7..]).into())
}
fn call_value(call: &NativeOperationCall) -> IvyJson {
    IvyJson::object([
        ("method", IvyJson::String(call.method.clone())),
        ("params", call.params.clone()),
        ("operationId", IvyJson::String(call.operation_id.clone())),
        (
            "definitionHash",
            IvyJson::String(call.definition_hash.clone()),
        ),
    ])
}

struct State {
    next: Option<usize>,
    thread_id: Option<JsString>,
    gap: Option<String>,
    retry_after: Option<f64>,
    page: Option<IvyJson>,
    calls: Vec<IvyJson>,
    observed_at: JsString,
}
pub struct TeamsNative<'a, 'engine, 'store, 'journal> {
    pub engine: &'a SecretaryEngine<'engine, 'store, 'journal>,
    pub configuration: IvyJson,
    pub owner: NativeOwner,
}
impl MicrosoftGuard for TeamsNative<'_, '_, '_, '_> {
    fn check(&self) -> MicrosoftGuardFuture<'_> {
        Box::pin(self.engine.evidence.verify_owner())
    }
}
struct PlanGuard<'a, 'b, 'engine, 'store, 'journal> {
    native: &'a TeamsNative<'b, 'engine, 'store, 'journal>,
    plan: &'a MicrosoftDocument,
}
impl MicrosoftGuard for PlanGuard<'_, '_, '_, '_, '_> {
    fn check(&self) -> MicrosoftGuardFuture<'_> {
        Box::pin(self.native.guard(self.plan))
    }
}

impl<'a, 'engine, 'store, 'journal> TeamsNative<'a, 'engine, 'store, 'journal> {
    pub fn new(
        engine: &'a SecretaryEngine<'engine, 'store, 'journal>,
        configuration: &IvyJson,
        tools: Arc<ToolClients>,
    ) -> IvyResult<Self> {
        validate_microsoft("TeamsCollector", configuration)?;
        let source = engine
            .evidence
            .source_definition(configuration.required("sourceId")?)?;
        teams_source(configuration, source)?;
        check(
            source
                .required("producerPrincipalIds")?
                .as_array()
                .unwrap()
                .contains(
                    engine
                        .evidence
                        .settings
                        .required("identity")?
                        .required("principalId")?,
                ),
            "teams_source_config_invalid",
        )?;
        check(
            is_absolute(
                Style::native(),
                configuration.required("threadCwd")?.as_js_string().unwrap(),
            ) && same(
                &microsoft_profile(configuration.required("profile")?)?,
                configuration.required("profile")?,
            )?,
            "microsoft_source_config_invalid",
        )?;
        instant(configuration, "since")?;
        let target = configuration.required("target")?;
        let owner = NativeOwner::new(
            tools,
            text(
                engine.evidence.settings.required("identity")?,
                "principalId",
            )?,
            NativeOwnerIdentity {
                service_node_id: text(target, "serviceNodeId")?,
                host_id: Some(text(target, "hostId")?),
                native_version: text(target, "nativeVersion")?,
                native_executable_hash: text(target, "nativeExecutableHash")?,
                catalog_hash: text(target, "catalogHash")?,
            },
            engine.evidence.cancellation.clone(),
        );
        Ok(Self {
            engine,
            configuration: configuration.clone(),
            owner,
        })
    }
    pub fn store(&self) -> MicrosoftStore<'_> {
        MicrosoftStore {
            journal: self.engine.evidence.store.journal,
            guard: self,
        }
    }
    fn boundaries(&self) -> MicrosoftEpochBoundaries<'_> {
        MicrosoftEpochBoundaries {
            journal: self.engine.evidence.store.journal,
            owner: &self.owner,
            namespace: "secretary/microsoft",
        }
    }
    pub async fn raw_head(&self) -> IvyResult<MicrosoftDocument> {
        let source = self
            .engine
            .evidence
            .source_definition(self.configuration.required("sourceId")?)?;
        let head = self
            .store()
            .create(
                MicrosoftRecord::TeamsHead,
                &teams_name("head", self.configuration.required("sourceId")?)?,
                &IvyJson::object([
                    ("schemaVersion", IvyJson::Number(1.0)),
                    ("configuration", self.configuration.clone()),
                    ("source", source.clone()),
                    ("pending", IvyJson::Null),
                    ("lastPage", IvyJson::Null),
                    ("continuation", IvyJson::Null),
                    ("nextPollAt", IvyJson::Null),
                    ("lastCompleteAt", IvyJson::Null),
                ]),
                false,
            )
            .await?;
        check(
            same(head.value.required("configuration")?, &self.configuration)?
                && same(
                    &source_identity(head.value.required("source")?)?,
                    &source_identity(source)?,
                )?,
            "microsoft_source_config_invalid",
        )?;
        Ok(head)
    }
    pub fn plan(&self, pin: &IvyJson) -> IvyResult<MicrosoftDocument> {
        let plan = self
            .store()
            .read(MicrosoftRecord::TeamsPlan, pin, true, None)?;
        let value = &plan.value;
        check(
            same(value.required("configuration")?, &self.configuration)?
                && same(
                    &source_identity(value.required("source")?)?,
                    &source_identity(
                        self.engine
                            .evidence
                            .source_definition(self.configuration.required("sourceId")?)?,
                    )?,
                )?,
            "microsoft_source_config_invalid",
        )?;
        teams_arguments(&self.configuration, value.required("selection")?)?;
        let bindings = value.required("bindings")?.as_array().unwrap();
        check(
            bindings.len() == MICROSOFT_METHODS.len()
                && bindings
                    .iter()
                    .zip(MICROSOFT_METHODS)
                    .all(|(b, m)| b.get("method") == Some(&m.into())),
            "microsoft_evidence_invalid",
        )?;
        instant(value, "preparedAt")?;
        teams_arguments(&self.configuration, value.required("selection")?)?;
        let named = self.store().find(
            MicrosoftRecord::TeamsPlan,
            &teams_name("plan", value.required("head")?)?,
            true,
        )?;
        check(
            named.is_some_and(|d| pin_value(&d.pin) == *pin),
            "microsoft_evidence_invalid",
        )?;
        Ok(plan)
    }
    pub async fn guard(&self, plan: &MicrosoftDocument) -> IvyResult<()> {
        self.engine.evidence.verify_owner().await?;
        let head = self.raw_head().await?;
        check(
            head.value.get("pending") == Some(&pin_value(&plan.pin)),
            "microsoft_plan_changed",
        )?;
        let source = self
            .engine
            .evidence
            .source(self.configuration.required("sourceId")?)
            .await?;
        check(
            source.required("pin")?.get("objectId")
                == plan.value.required("sourceCheckpoint")?.get("objectId")
                && source.required("value")?.get("cursor") == plan.value.get("previousCursor"),
            "microsoft_source_cursor_changed",
        )
    }
    pub async fn prepare(&self, head: &MicrosoftDocument) -> IvyResult<Option<MicrosoftDocument>> {
        if let Some(pin) = head.value.get("pending").filter(|v| !v.is_null()) {
            return self.plan(pin).map(Some);
        }
        let prepared_at = self.engine.now()?;
        if head
            .value
            .get("nextPollAt")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|at| prepared_at < *at)
        {
            return Ok(None);
        }
        let continuation = head.value.get("continuation").filter(|v| !v.is_null());
        if continuation.is_none() && text(&self.configuration, "since")? >= prepared_at {
            return Ok(None);
        }
        let name = teams_name("plan", &pin_value(&head.pin))?;
        let mut plan = self.store().find(MicrosoftRecord::TeamsPlan, &name, true)?;
        if plan.is_none() {
            let status = self.owner.status().await?;
            let mut bindings = vec![];
            let source = self
                .engine
                .evidence
                .source(self.configuration.required("sourceId")?)
                .await?;
            for method in MICROSOFT_METHODS {
                let binding = self.owner.binding(&method.into(), None).await?;
                bindings.push(IvyJson::object([
                    ("method", method.into()),
                    (
                        "definitionHash",
                        binding.value().required("definitionHash")?.clone(),
                    ),
                    (
                        "outputSchema",
                        IvyJson::object(Vec::<(String, IvyJson)>::new()),
                    ),
                ]));
            }
            check(
                self.owner.status().await?.get("epoch") == status.get("epoch"),
                "microsoft_epoch_changed",
            )?;
            plan = Some(
                self.store()
                    .create(
                        MicrosoftRecord::TeamsPlan,
                        &name,
                        &IvyJson::object([
                            ("schemaVersion", IvyJson::Number(1.0)),
                            ("head", pin_value(&head.pin)),
                            ("configuration", self.configuration.clone()),
                            (
                                "source",
                                source.required("value")?.required("definition")?.clone(),
                            ),
                            ("sourceCheckpoint", source.required("pin")?.clone()),
                            (
                                "previousCursor",
                                source.required("value")?.required("cursor")?.clone(),
                            ),
                            (
                                "selection",
                                continuation
                                    .and_then(|v| v.get("selection"))
                                    .cloned()
                                    .unwrap_or_else(|| {
                                        IvyJson::object([
                                            (
                                                "since",
                                                self.configuration.get("since").unwrap().clone(),
                                            ),
                                            ("until", IvyJson::String(prepared_at.clone())),
                                            ("chat", IvyJson::Null),
                                        ])
                                    }),
                            ),
                            ("epoch", status.required("epoch")?.clone()),
                            ("preparedAt", IvyJson::String(prepared_at)),
                            ("bindings", IvyJson::Array(bindings)),
                        ]),
                        true,
                    )
                    .await?,
            );
        }
        let plan = self.plan(&pin_value(&plan.unwrap().pin))?;
        let mut next = head.value.clone();
        next.insert("pending", pin_value(&plan.pin));
        if let Err(error) = self
            .store()
            .amend(MicrosoftRecord::TeamsHead, head, &next)
            .await
        {
            if self.raw_head().await?.value.get("pending") != Some(&pin_value(&plan.pin)) {
                return Err(error);
            }
        }
        self.guard(&plan).await?;
        Ok(Some(plan))
    }
    pub async fn invocation(
        &self,
        plan: &MicrosoftDocument,
        slot: usize,
        thread_id: Option<&JsString>,
        operation_id: Option<JsString>,
    ) -> IvyResult<IvyJson> {
        let (method, params) = if slot == 0 || slot == 5 {
            (
                "account/read",
                IvyJson::object([("refreshToken", false.into())]),
            )
        } else if slot == 1 {
            (
                "thread/start",
                IvyJson::object([
                    ("cwd", self.configuration.required("threadCwd")?.clone()),
                    ("ephemeral", true.into()),
                    ("permissions", ":danger-full-access".into()),
                    ("approvalPolicy", "never".into()),
                ]),
            )
        } else {
            check(
                thread_id.is_some_and(|v| !v.0.is_empty()),
                "microsoft_evidence_invalid",
            )?;
            let thread = IvyJson::String(thread_id.unwrap().clone());
            if slot == 6 {
                (
                    "thread/unsubscribe",
                    IvyJson::object([("threadId", thread)]),
                )
            } else {
                (
                    "mcpServer/tool/call",
                    IvyJson::object([
                        ("threadId", thread),
                        ("server", "codex_apps".into()),
                        (
                            "tool",
                            format!(
                                "microsoft_teams.{}",
                                if slot == 3 {
                                    if plan
                                        .value
                                        .required("selection")?
                                        .required("chat")?
                                        .is_null()
                                    {
                                        "list_chats"
                                    } else {
                                        "list_chat_messages"
                                    }
                                } else {
                                    "get_profile"
                                }
                            )
                            .into(),
                        ),
                        (
                            "arguments",
                            if slot == 3 {
                                teams_arguments(
                                    &self.configuration,
                                    plan.value.required("selection")?,
                                )?
                            } else {
                                IvyJson::object(Vec::<(String, IvyJson)>::new())
                            },
                        ),
                    ]),
                )
            }
        };
        let operation_id = match operation_id {
            Some(id) => id,
            None => new_operation_id(self.engine.evidence.store.client.clone(), None).await?,
        };
        let binding = plan
            .value
            .required("bindings")?
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b.get("method") == Some(&method.into()))
            .ok_or_else(IvyError::internal)?;
        Ok(IvyJson::object([
            ("method", method.into()),
            ("params", params),
            ("operationId", IvyJson::String(operation_id)),
            (
                "definitionHash",
                binding.required("definitionHash")?.clone(),
            ),
        ]))
    }
    async fn call(
        &self,
        plan: &MicrosoftDocument,
        slot: usize,
        thread_id: Option<&JsString>,
    ) -> IvyResult<Option<MicrosoftDocument>> {
        let name = teams_name(
            "call",
            &IvyJson::Array(vec![pin_value(&plan.pin), IvyJson::Number(slot as f64)]),
        )?;
        let Some(call) = self
            .store()
            .find(MicrosoftRecord::TeamsCall, &name, false)?
        else {
            return Ok(None);
        };
        self.store().check_call_history(&call, &self.owner)?;
        let original = call_value(&native_call(&call.value)?);
        check(
            call.value.get("plan") == Some(&pin_value(&plan.pin))
                && call.value.get("slot") == Some(&IvyJson::Number(slot as f64))
                && same(
                    &self
                        .invocation(
                            plan,
                            slot,
                            thread_id,
                            Some(text(&call.value, "operationId")?),
                        )
                        .await?,
                    &original,
                )?,
            "microsoft_evidence_invalid",
        )?;
        check(
            instant(&call.value, "preparedAt")? >= instant(&plan.value, "preparedAt")?,
            "microsoft_evidence_invalid",
        )?;
        Ok(Some(call))
    }
    fn evaluate(
        &self,
        plan: &MicrosoftDocument,
        state: &mut State,
        call: &MicrosoftDocument,
        operation: &IvyJson,
    ) -> IvyResult<()> {
        self.owner
            .check_operation(operation, &native_call(&call.value)?)?;
        check(
            native_operation_terminal(operation)
                && instant(operation, "createdAt")? >= instant(&call.value, "preparedAt")?
                && text(&call.value, "preparedAt")? >= state.observed_at,
            "microsoft_evidence_invalid",
        )?;
        let slot = call.value.required("slot")?.as_f64().unwrap() as usize;
        state.observed_at = text(operation, "updatedAt")?;
        state.calls.push(pin_value(&call.pin));
        let reply = operation.get("reply").filter(|v| !v.is_null());
        let result = reply.and_then(|r| r.get("result"));
        let succeeded = operation.get("phase") == Some(&"succeeded".into()) && result.is_some();
        if slot == 6 {
            check(succeeded, "microsoft_cleanup_failed")?;
            check(
                object(result)?.get("status").is_some_and(|v| {
                    matches!(
                        js_string(v).to_utf8().unwrap_or_default().as_str(),
                        "unsubscribed" | "notLoaded" | "notSubscribed"
                    )
                }),
                "microsoft_cleanup_failed",
            )?;
            state.next = None;
            return Ok(());
        }
        let evaluated = (|| -> IvyResult<()> {
            check(succeeded, "microsoft_native_failed")?;
            check(
                operation.get("epoch") == plan.value.get("epoch"),
                "microsoft_epoch_changed",
            )?;
            let result = result.unwrap();
            match slot {
                0 | 5 => check(
                    self.configuration.get("expectedAccountHash")
                        == Some(&catalogue_account_hash(result)?.into()),
                    "microsoft_account_mismatch",
                )?,
                1 => {
                    let thread = object(object(Some(result))?.get("thread"))?
                        .get("id")
                        .and_then(IvyJson::as_js_string);
                    check(
                        thread.is_some_and(|v| !v.0.is_empty()),
                        "microsoft_evidence_invalid",
                    )?;
                    state.thread_id = thread.cloned();
                }
                2 | 4 => check(
                    same(
                        &microsoft_profile(&microsoft_mcp(result)?)?,
                        self.configuration.required("profile")?,
                    )?,
                    "microsoft_account_mismatch",
                )?,
                _ => {
                    let page = teams_page(
                        &microsoft_mcp(result)?,
                        &self.configuration,
                        plan.value.required("source")?,
                        plan.value.required("selection")?,
                        operation.required("updatedAt")?,
                    )?;
                    state.page = Some(page);
                }
            }
            state.next = Some(slot + 1);
            Ok(())
        })();
        if let Err(error) = evaluated {
            state.retry_after = if error.code == "microsoft_rate_limited" {
                object(error.details.as_ref())?
                    .get("retryAfterSeconds")
                    .and_then(IvyJson::as_f64)
            } else {
                None
            };
            state.gap = Some(error.code);
            state.page = None;
            state.next = state.thread_id.as_ref().map(|_| 6);
        }
        Ok(())
    }
    async fn state(&self, plan: &MicrosoftDocument) -> IvyResult<State> {
        let mut state = State {
            next: Some(0),
            thread_id: None,
            gap: None,
            retry_after: None,
            page: None,
            calls: vec![],
            observed_at: text(&plan.value, "preparedAt")?,
        };
        while let Some(slot) = state.next {
            let Some(call) = self.call(plan, slot, state.thread_id.as_ref()).await? else {
                break;
            };
            if let Some(boundary) = self.boundaries().read(
                &pin_value(&plan.pin),
                &call.value,
                plan.value.required("epoch")?,
            )? {
                check(
                    call.value.required("seen")?.is_null()
                        && call.value.required("observation")?.is_null()
                        && slot != 6
                        && text(boundary.required("status")?, "observedAt")? >= state.observed_at,
                    "microsoft_evidence_invalid",
                )?;
                state.calls.push(pin_value(&call.pin));
                state.observed_at = text(boundary.required("status")?, "observedAt")?;
                state.gap = Some("microsoft_epoch_changed".into());
                state.page = None;
                state.next = state.thread_id.as_ref().map(|_| 6);
                continue;
            }
            let observation = call.value.required("observation")?;
            if observation.is_null() {
                break;
            }
            self.evaluate(
                plan,
                &mut state,
                &call,
                &self.store().operation(
                    &pin_value(&plan.pin),
                    call.value.required("slot")?,
                    observation,
                )?,
            )?;
        }
        Ok(state)
    }
    fn evidence_value(plan: &MicrosoftDocument, state: &State) -> IvyJson {
        IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("plan", pin_value(&plan.pin)),
            ("calls", IvyJson::Array(state.calls.clone())),
            ("observedAt", IvyJson::String(state.observed_at.clone())),
            (
                "gap",
                state
                    .gap
                    .clone()
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "retryAfterSeconds",
                state
                    .retry_after
                    .map(IvyJson::Number)
                    .unwrap_or(IvyJson::Null),
            ),
            ("page", state.page.clone().unwrap_or(IvyJson::Null)),
        ])
    }
    pub async fn evidence(&self, pin: &IvyJson) -> IvyResult<MicrosoftDocument> {
        let proof = self
            .store()
            .read(MicrosoftRecord::TeamsEvidence, pin, true, None)?;
        let plan = self.plan(proof.value.required("plan")?)?;
        let state = self.state(&plan).await?;
        let named = self.store().find(
            MicrosoftRecord::TeamsEvidence,
            &teams_name("evidence", &pin_value(&plan.pin))?,
            true,
        )?;
        check(
            state.next.is_none()
                && same(&proof.value, &Self::evidence_value(&plan, &state))?
                && named.is_some_and(|d| pin_value(&d.pin) == *pin),
            "microsoft_evidence_invalid",
        )?;
        Ok(proof)
    }
    pub async fn collect(&self, plan: &MicrosoftDocument) -> IvyResult<Option<MicrosoftDocument>> {
        let plan = self.plan(&pin_value(&plan.pin))?;
        let mut state = self.state(&plan).await?;
        while let Some(slot) = state.next {
            let mut call = self.call(&plan, slot, state.thread_id.as_ref()).await?;
            if call.is_none() {
                self.guard(&plan).await?;
                let mut value = self
                    .invocation(&plan, slot, state.thread_id.as_ref(), None)
                    .await?;
                value.insert("schemaVersion", IvyJson::Number(1.0));
                value.insert("plan", pin_value(&plan.pin));
                value.insert("slot", IvyJson::Number(slot as f64));
                value.insert("preparedAt", IvyJson::String(self.engine.now()?));
                value.insert("seen", IvyJson::Null);
                value.insert("observation", IvyJson::Null);
                self.store()
                    .create(
                        MicrosoftRecord::TeamsCall,
                        &teams_name(
                            "call",
                            &IvyJson::Array(vec![
                                pin_value(&plan.pin),
                                IvyJson::Number(slot as f64),
                            ]),
                        )?,
                        &value,
                        false,
                    )
                    .await?;
                call = self.call(&plan, slot, state.thread_id.as_ref()).await?;
                check(call.is_some(), "microsoft_evidence_invalid")?;
            }
            let retained = call.unwrap();
            let previous = retained
                .value
                .get("observation")
                .filter(|v| !v.is_null())
                .or_else(|| retained.value.get("seen").filter(|v| !v.is_null()));
            let previous = previous
                .map(|v| {
                    self.store()
                        .operation(&pin_value(&plan.pin), &IvyJson::Number(slot as f64), v)
                })
                .transpose()?;
            let original = native_call(&retained.value)?;
            let result = reconcile_native_operation_local(
                &self.owner,
                &original,
                NativeOperationJournal {
                    current: retained.clone(),
                    was_observed: previous.is_some(),
                    previous,
                    retain: |operation: IvyJson| {
                        let plan = &plan;
                        let retained = &retained;
                        async move {
                            let terminal = native_operation_terminal(&operation);
                            let evidence = self
                                .store()
                                .save_operation(
                                    &pin_value(&plan.pin),
                                    &IvyJson::Number(slot as f64),
                                    &operation,
                                )
                                .await?;
                            self.guard(plan).await?;
                            let mut value = retained.value.clone();
                            value.insert(if terminal { "observation" } else { "seen" }, evidence);
                            self.store()
                                .amend(MicrosoftRecord::TeamsCall, retained, &value)
                                .await
                        }
                    },
                },
                Some(|original: NativeOperationCall, absence: IvyJson| {
                    let plan = &plan;
                    async move {
                        if slot != 6 && absence.get("epoch") != plan.value.get("epoch") {
                            self.boundaries()
                                .record(
                                    &pin_value(&plan.pin),
                                    &call_value(&original),
                                    plan.value.required("epoch")?,
                                    &absence,
                                    &PlanGuard { native: self, plan },
                                )
                                .await?;
                            return Ok(None);
                        }
                        self.owner
                            .dispatch_local(&original, || async {
                                self.guard(plan).await?;
                                let status = self.owner.status().await?;
                                check(
                                    status.get("epoch") == absence.get("epoch")
                                        && (slot == 6
                                            || status.get("epoch") == plan.value.get("epoch")),
                                    "microsoft_epoch_changed",
                                )
                            })
                            .await
                    }
                }),
                Some("microsoft_native_absence_conflict"),
            )
            .await?;
            let NativeReconciliation::Observed {
                value: call,
                observation,
            } = result
            else {
                return Ok(None);
            };
            if !native_operation_terminal(&observation) {
                return Ok(None);
            }
            self.evaluate(&plan, &mut state, &call, &observation)?;
        }
        self.guard(&plan).await?;
        let proof = self
            .store()
            .create(
                MicrosoftRecord::TeamsEvidence,
                &teams_name("evidence", &pin_value(&plan.pin))?,
                &Self::evidence_value(&plan, &state),
                true,
            )
            .await?;
        self.evidence(&pin_value(&proof.pin)).await.map(Some)
    }
    pub fn next_head(
        &self,
        head: &IvyJson,
        plan: &MicrosoftDocument,
        proof: &MicrosoftDocument,
    ) -> IvyResult<IvyJson> {
        let value = &proof.value;
        let page = value.get("page").filter(|v| !v.is_null());
        let gap = value.get("gap").is_some_and(crate::inbox_evidence::truthy);
        check(gap == page.is_none(), "microsoft_evidence_invalid")?;
        let remaining: &[IvyJson] = if let Some(page) = page {
            if plan
                .value
                .required("selection")?
                .required("chat")?
                .is_null()
            {
                page.required("chats")?.as_array().unwrap()
            } else {
                head.required("continuation")?
                    .required("remainingChats")?
                    .as_array()
                    .unwrap()
            }
        } else {
            &[]
        };
        let continuation = if let Some(first) = remaining.first() {
            let mut selection = plan.value.required("selection")?.clone();
            selection.insert("chat", first.clone());
            IvyJson::object([
                ("selection", selection),
                ("remainingChats", IvyJson::Array(remaining[1..].to_vec())),
            ])
        } else {
            IvyJson::Null
        };
        let next_poll = if !continuation.is_null() {
            value.required("observedAt")?.clone()
        } else {
            IvyJson::String(
                date_to_iso(
                    instant(value, "observedAt")? as f64
                        + self
                            .configuration
                            .required("pollMs")?
                            .as_f64()
                            .unwrap()
                            .max(
                                value
                                    .get("retryAfterSeconds")
                                    .and_then(IvyJson::as_f64)
                                    .unwrap_or(0.0)
                                    * 1000.0,
                            ),
                )
                .map_err(|_| IvyError::internal())?,
            )
        };
        let complete = page.is_some() && continuation.is_null();
        let mut next = head.clone();
        next.insert("pending", IvyJson::Null);
        next.insert("lastPage", pin_value(&proof.pin));
        next.insert("continuation", continuation);
        next.insert("nextPollAt", next_poll);
        if complete {
            next.insert("lastCompleteAt", value.required("observedAt")?.clone());
        }
        Ok(next)
    }
}
