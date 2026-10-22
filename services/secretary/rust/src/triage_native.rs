//! Dormant triage's original native calls and shared retained thread reservation.
use crate::{
    config::triage_configuration,
    engine::SecretaryEngine,
    inbox_evidence::{ignored_reason, truthy},
    media_input::Prepared,
    microsoft_store::native_call,
    triage_media::{self, Context, Inputs},
    triage_prompt::{triage_native_params, triage_prompt, triage_tool_configuration},
    triage_store::{
        TriageDocument, TriageRecord as R, TriageStore, check, evidence, pin, pin_value, record,
        same, text, triage_name,
    },
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::try_js_string};
use ivy_sdk::{
    native_evidence::NativeOwnerIdentity,
    native_operation::{
        NativeOperationJournal, NativeReconciliation, reconcile_native_operation_local,
    },
    native_owner::NativeOwner,
    operation_ids::new_operation_id,
    tools::ToolClients,
};
use std::{cell::RefCell, collections::VecDeque, future::Future, pin::Pin as FuturePin, sync::Arc};

pub const TRIAGE_METHODS: [&str; 4] = [
    "thread/start",
    "thread/resume",
    "turn/start",
    "thread/unsubscribe",
];
pub type TriageFuture<'a, T> = FuturePin<Box<dyn Future<Output = IvyResult<T>> + 'a>>;
/// Only the internal verified original-coverage reader implements this boundary.
pub trait TriageMediaContext {
    fn context(&self) -> &Context;
    fn input(&self, index: usize, maximum: usize) -> TriageFuture<'_, Option<Prepared>>;
}
pub trait TriageMediaResolver {
    fn resolve<'a>(
        &'a self,
        source_id: &'a JsString,
        item: &'a crate::journal::Pin,
    ) -> TriageFuture<'a, Option<Box<dyn TriageMediaContext + 'a>>>;
}
#[derive(Clone, Debug)]
pub struct TriageCall {
    pub stored: TriageDocument,
    pub params: IvyJson,
    pub observation: Option<IvyJson>,
}
impl TriageCall {
    pub fn value(&self) -> IvyJson {
        let mut value = self.stored.value.clone();
        value.insert("params", self.params.clone());
        value.insert(
            "observation",
            self.observation.clone().unwrap_or(IvyJson::Null),
        );
        IvyJson::object([
            ("pin", pin_value(&self.stored.pin)),
            ("stored", self.stored.value()),
            ("value", value),
        ])
    }
    pub fn original(&self) -> IvyResult<ivy_sdk::native_operation::NativeOperationCall> {
        let mut value = self.stored.value.clone();
        value.insert("params", self.params.clone());
        native_call(&value)
    }
}
pub struct TriageStarted {
    pub turn: TriageCall,
    pub thread_id: JsString,
    pub epoch: JsString,
}
enum Before<'a> {
    Epoch(&'a JsString),
    Turn {
        start: &'a TriageCall,
        thread: &'a JsString,
    },
}
pub struct TriageNative<'a, 'engine, 'store, 'journal> {
    pub engine: &'a SecretaryEngine<'engine, 'store, 'journal>,
    pub settings: IvyJson,
    pub active_thread_id: RefCell<Option<JsString>>,
    tools: Arc<ToolClients>,
    notification_policy: JsString,
    resolver: Option<&'a dyn TriageMediaResolver>,
    plans: RefCell<VecDeque<(crate::journal::Pin, TriageDocument)>>,
}
impl<'a, 'engine, 'store, 'journal> TriageNative<'a, 'engine, 'store, 'journal> {
    pub fn new(
        engine: &'a SecretaryEngine<'engine, 'store, 'journal>,
        settings: &IvyJson,
        tools: Arc<ToolClients>,
        notification_policy: JsString,
        resolver: Option<&'a dyn TriageMediaResolver>,
    ) -> IvyResult<Self> {
        Ok(Self {
            engine,
            settings: triage_configuration(settings)?,
            tools,
            notification_policy,
            resolver,
            active_thread_id: RefCell::new(None),
            plans: RefCell::new(VecDeque::new()),
        })
    }
    pub fn store(&self) -> TriageStore<'_> {
        TriageStore {
            journal: self.engine.evidence.store.journal,
        }
    }
    pub fn owner(&self, plan: &TriageDocument) -> IvyResult<NativeOwner> {
        let target = plan.value.required("settings")?.required("target")?;
        Ok(NativeOwner::new(
            self.tools.clone(),
            text(
                self.engine.evidence.settings.required("identity")?,
                "principalId",
            )?,
            NativeOwnerIdentity {
                service_node_id: text(target, "serviceNodeId")?,
                host_id: Some(text(target, "hostId")?),
                native_version: text(target, "nativeVersion")?,
                native_executable_hash: text(target, "nativeExecutableHash")?,
                catalog_hash: text(target, "catalogHash")?,
            },
            self.engine.evidence.cancellation.clone(),
        ))
    }
    pub fn work(&self, item: &IvyJson) -> IvyResult<Option<TriageDocument>> {
        let Some(work) = self
            .store()
            .find(R::Work, &triage_name("work", item)?, false)?
        else {
            return Ok(None);
        };
        evidence(same(work.value.required("item")?, item)?)?;
        let phase = work.value.required("phase")?;
        let completion = work.value.required("completion")?;
        let assessment = work.value.required("assessment")?;
        let error = work.value.required("errorCode")?;
        evidence(if phase == &IvyJson::from("pending") {
            completion.is_null() && assessment.is_null() && error.is_null()
        } else if phase == &IvyJson::from("assessed") {
            truthy(completion) && truthy(assessment) && error.is_null()
        } else {
            !error.is_null() && assessment.is_null()
        })?;
        Ok(Some(work))
    }
    pub fn plan(&self, work: &TriageDocument) -> IvyResult<TriageDocument> {
        let selected = pin(work.value.required("plan")?)?;
        if let Some((_, plan)) = self.plans.borrow().iter().find(|(p, _)| p == &selected) {
            return Ok(plan.clone());
        }
        let mut plan = self.store().read(
            R::Plan,
            work.value.required("plan")?,
            Some(&triage_name("plan", work.value.required("item")?)?),
            true,
        )?;
        let stored = record(Some(
            &self
                .store()
                .native_json("parameters", plan.value.required("nativeParams")?)?,
        ));
        plan.value
            .insert("nativeParams", record(stored.get("params")));
        let mut plans = self.plans.borrow_mut();
        if plans.len() >= 2 {
            plans.pop_front();
        }
        plans.push_back((selected, plan.clone()));
        Ok(plan)
    }
    async fn media_input(&self, item: &IvyJson) -> IvyResult<Inputs> {
        let value = item.required("value")?;
        if value.required("message")?.get("attachments") == Some(&"none".into()) {
            return Ok(Inputs {
                media: IvyJson::Null,
                inputs: vec![],
            });
        }
        let source_id = text(value, "sourceId")?;
        let item_pin = pin(item.required("pin")?)?;
        let context = match self.resolver {
            Some(resolver) => resolver.resolve(&source_id, &item_pin).await?,
            None => None,
        };
        check(context.is_some(), "secretary_triage_media_pending")?;
        let context = context.unwrap();
        triage_media::assemble(
            context.context(),
            &item_pin,
            &source_id,
            |index, maximum| context.input(index, maximum),
        )
        .await
    }
    pub async fn guard(&self, plan: &TriageDocument, cleanup: bool) -> IvyResult<()> {
        self.engine.evidence.verify_owner().await?;
        let selected = plan.value.required("input")?.required("item")?;
        let work = self.work(selected)?;
        let item = self
            .engine
            .evidence
            .item(selected.required("objectId")?)
            .await?;
        check(
            match work {
                Some(work) => {
                    same(work.value.required("plan")?, &pin_value(&plan.pin))?
                        && (cleanup
                            || work.value.get("phase") == Some(&"pending".into())
                                && same(item.required("pin")?, selected)?
                                && !item.required("value")?.get("decision").is_some_and(truthy))
                }
                None => false,
            },
            "secretary_triage_item_changed",
        )
    }
    pub async fn prepare(&self, item_pin: &IvyJson) -> IvyResult<TriageDocument> {
        self.engine.evidence.verify_owner().await?;
        if let Some(work) = self.work(item_pin)? {
            self.plan(&work)?;
            return Ok(work);
        }
        let item = self
            .engine
            .evidence
            .item(item_pin.required("objectId")?)
            .await?;
        check(
            same(item.required("pin")?, item_pin)?
                && !item.required("value")?.get("decision").is_some_and(truthy),
            "secretary_triage_item_changed",
        )?;
        let name = triage_name("plan", item_pin)?;
        let plan = match self.store().find(R::Plan, &name, true)? {
            Some(plan) => plan,
            None => {
                let media = self.media_input(&item).await?;
                let value = item.required("value")?;
                let input = IvyJson::object([
                    ("item", item_pin.clone()),
                    (
                        "source",
                        self.engine
                            .evidence
                            .source_definition(value.required("sourceId")?)?
                            .clone(),
                    ),
                    ("message", value.required("message")?.clone()),
                    (
                        "contentScope",
                        if truthy(&media.media) {
                            "verified_media"
                        } else {
                            "captured_text_or_caption"
                        }
                        .into(),
                    ),
                    ("media", media.media),
                    (
                        "policy",
                        self.engine.evidence.settings.required("policy")?.clone(),
                    ),
                ]);
                let shell = TriageDocument {
                    pin: pin(item_pin)?,
                    value: IvyJson::object([("settings", self.settings.clone())]),
                };
                let owner = self.owner(&shell)?;
                let mut bindings = Vec::new();
                for method in TRIAGE_METHODS {
                    let binding = owner.binding(&method.into(), None).await?;
                    bindings.push(IvyJson::object([
                        ("method", method.into()),
                        (
                            "definitionHash",
                            binding.value().required("definitionHash")?.clone(),
                        ),
                        ("outputSchema", record(None)),
                    ]));
                }
                let prompt = triage_prompt(&input, &self.settings, &self.notification_policy)?;
                let mut params = triage_native_params(&self.settings, &prompt, &media.inputs)?;
                params.insert(
                    "threadStart",
                    owner
                        .thread_start_params(params.required("threadStart")?)
                        .await?,
                );
                let native_params = self.store().save_native_json(
                    "parameters",
                    item_pin,
                    &IvyJson::object([
                        ("schemaVersion", IvyJson::Number(1.0)),
                        ("item", item_pin.clone()),
                        ("params", params),
                    ]),
                )?;
                let mut value = IvyJson::object([
                    ("schemaVersion", IvyJson::Number(1.0)),
                    (
                        "identity",
                        self.engine.evidence.settings.required("identity")?.clone(),
                    ),
                    ("settings", self.settings.clone()),
                    ("input", input),
                    ("nativeParams", pin_value(&native_params)),
                    ("bindings", IvyJson::Array(bindings)),
                    ("createdAt", IvyJson::String(self.engine.now()?)),
                ]);
                for (key, part) in prompt.as_object().unwrap() {
                    value.insert(key.clone(), part.clone());
                }
                self.store().create(R::Plan, &name, &value, true)?
            }
        };
        let operation_id =
            new_operation_id(self.engine.evidence.store.client.clone(), None).await?;
        let work = self.store().create(
            R::Work,
            &triage_name("work", item_pin)?,
            &IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("item", item_pin.clone()),
                ("plan", pin_value(&plan.pin)),
                ("operationId", IvyJson::String(operation_id)),
                ("phase", "pending".into()),
                ("completion", IvyJson::Null),
                ("assessment", IvyJson::Null),
                ("errorCode", IvyJson::Null),
            ]),
            false,
        )?;
        self.plan(&work)?;
        Ok(work)
    }
    pub fn params(
        &self,
        plan: &TriageDocument,
        method: &JsString,
        thread_id: Option<&JsString>,
    ) -> IvyResult<IvyJson> {
        let params = plan.value.required("nativeParams")?;
        if method == &JsString::from("thread/start") {
            return Ok(params.required("threadStart")?.clone());
        }
        check(
            thread_id.is_some_and(|id| !id.0.is_empty()),
            "secretary_triage_thread_missing",
        )?;
        let mut value = if method == &JsString::from("thread/resume") {
            params.required("threadResume")?.clone()
        } else if method == &JsString::from("thread/unsubscribe") {
            record(None)
        } else {
            params.required("turnStart")?.clone()
        };
        value.insert("threadId", IvyJson::String(thread_id.unwrap().clone()));
        Ok(value)
    }
    pub async fn call(
        &self,
        plan: &TriageDocument,
        slot: &JsString,
        method: &JsString,
        thread: Option<&JsString>,
        create: bool,
    ) -> IvyResult<TriageCall> {
        let identity = IvyJson::Array(vec![pin_value(&plan.pin), IvyJson::String(slot.clone())]);
        let name = triage_name("call", &identity)?;
        let params = self.params(plan, method, thread)?;
        let binding = plan
            .value
            .required("bindings")?
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b.get("method").and_then(IvyJson::as_js_string) == Some(method))
            .ok_or_else(IvyError::internal)?;
        let stored = match self.store().find(R::Call, &name, false)? {
            Some(call) => call,
            None => {
                let operation_id =
                    new_operation_id(self.engine.evidence.store.client.clone(), None).await?;
                let expected = IvyJson::object([
                    ("schemaVersion", IvyJson::Number(1.0)),
                    ("plan", pin_value(&plan.pin)),
                    ("slot", IvyJson::String(slot.clone())),
                    ("method", IvyJson::String(method.clone())),
                    ("operationId", IvyJson::String(operation_id.clone())),
                    (
                        "definitionHash",
                        binding.required("definitionHash")?.clone(),
                    ),
                    ("params", params.clone()),
                ]);
                check(create, "secretary_triage_call_missing")?;
                let cleanup = method == &JsString::from("thread/unsubscribe");
                self.guard(plan, cleanup).await?;
                let request = self
                    .store()
                    .save_native_json("request", &identity, &expected)?;
                self.guard(plan, cleanup).await?;
                let mut value = expected;
                value.remove("params");
                value.insert("request", pin_value(&request));
                value.insert("preparedAt", IvyJson::String(self.engine.now()?));
                value.insert("observation", IvyJson::Null);
                self.store().create(R::Call, &name, &value, false)?
            }
        };
        self.hydrate(plan, stored, params)
    }
    fn hydrate(
        &self,
        plan: &TriageDocument,
        stored: TriageDocument,
        params: IvyJson,
    ) -> IvyResult<TriageCall> {
        let observed = stored.value.required("observation")?;
        let observation = if truthy(observed) {
            Some(self.store().native_json("operation", observed)?)
        } else {
            None
        };
        let call = TriageCall {
            stored,
            params,
            observation,
        };
        if let Some(observed) = &call.observation {
            self.owner(plan)?
                .check_operation(observed, &call.original()?)?;
        }
        Ok(call)
    }
    async fn retain_observation(
        &self,
        plan: &TriageDocument,
        call: &TriageCall,
        observed: IvyJson,
        cleanup: bool,
    ) -> IvyResult<TriageCall> {
        self.guard(plan, cleanup).await?;
        let observation = self.store().save_native_json(
            "operation",
            &IvyJson::Array(vec![
                pin_value(&plan.pin),
                call.stored.value.required("slot")?.clone(),
            ]),
            &observed,
        )?;
        self.guard(plan, cleanup).await?;
        let mut value = call.stored.value.clone();
        value.insert("observation", pin_value(&observation));
        self.hydrate(
            plan,
            self.store().amend(R::Call, &call.stored, &value)?,
            call.params.clone(),
        )
    }
    pub async fn advance(&self, plan: &TriageDocument, call: &TriageCall) -> IvyResult<TriageCall> {
        self.advance_checked(plan, call, None).await
    }
    async fn advance_checked(
        &self,
        plan: &TriageDocument,
        call: &TriageCall,
        before: Option<Before<'_>>,
    ) -> IvyResult<TriageCall> {
        let owner = self.owner(plan)?;
        let original = call.original()?;
        let cleanup = original.method == JsString::from("thread/unsubscribe");
        let owner_ref = &owner;
        let result = reconcile_native_operation_local(
            &owner,
            &original,
            NativeOperationJournal {
                current: call.clone(),
                previous: call.observation.clone(),
                was_observed: call.observation.is_some(),
                retain: |observed| self.retain_observation(plan, call, observed, cleanup),
            },
            Some(|original, absence: IvyJson| async move {
                let status = owner_ref.status().await?;
                check(
                    status.get("epoch") == absence.get("epoch"),
                    "secretary_triage_epoch_changed",
                )?;
                if let Some(before) = before {
                    match before {
                        Before::Epoch(epoch) => check(
                            status.get("epoch").and_then(IvyJson::as_js_string) == Some(epoch),
                            "secretary_triage_epoch_changed",
                        )?,
                        Before::Turn { start, thread } => {
                            let epoch = text(&status, "epoch")?;
                            check(
                                Box::pin(self.attached(plan, start, thread, &epoch)).await?,
                                "secretary_triage_attachment_pending",
                            )?;
                            let reader = self.owner(plan)?;
                            let read = reader
                                .read(
                                    &"thread/read".into(),
                                    IvyJson::object([
                                        ("threadId", IvyJson::String(thread.clone())),
                                        ("includeTurns", false.into()),
                                    ]),
                                    Some(&epoch),
                                    false,
                                )
                                .await?;
                            let state = reader.thread_state(&read, &epoch).await?;
                            check(
                                state.thread.get("id").and_then(IvyJson::as_js_string)
                                    == Some(thread)
                                    && state.thread.get("ephemeral") == Some(&false.into())
                                    && state.can_start_turn,
                                "secretary_triage_thread_busy",
                            )?;
                        }
                    }
                }
                owner_ref
                    .dispatch_local(&original, || async {
                        self.guard(plan, cleanup).await?;
                        if !cleanup {
                            let params = plan.value.required("nativeParams")?;
                            let mut policy = true;
                            for key in ["threadStart", "threadResume", "turnStart"] {
                                let value = params.required(key)?;
                                policy &= value.get("permissions") == Some(&":read-only".into())
                                    && value.get("approvalPolicy") == Some(&"never".into());
                            }
                            for key in ["threadStart", "threadResume"] {
                                policy &= same(
                                    params.required(key)?.required("config")?,
                                    &triage_tool_configuration(),
                                )?;
                            }
                            policy &= params
                                .required("threadStart")?
                                .get("baseInstructions")
                                .is_none();
                            check(policy, "secretary_triage_execution_policy_changed")?;
                            let input = plan.value.required("input")?;
                            let source = self
                                .engine
                                .evidence
                                .settings
                                .required("sources")?
                                .as_array()
                                .unwrap()
                                .iter()
                                .find(|s| {
                                    s.get("sourceId")
                                        == input
                                            .required("source")
                                            .ok()
                                            .and_then(|s| s.get("sourceId"))
                                });
                            check(
                                match source {
                                    Some(source) => {
                                        ignored_reason(source, input.required("message")?)?
                                            .is_none()
                                    }
                                    None => false,
                                },
                                "secretary_triage_authorization_changed",
                            )?;
                        }
                        Ok(())
                    })
                    .await
            }),
            Some("secretary_triage_absence_conflict"),
        )
        .await?;
        match result {
            NativeReconciliation::Observed { value, .. } => Ok(value),
            _ => {
                check(false, "secretary_triage_outcome_unavailable")?;
                unreachable!()
            }
        }
    }
    async fn observe_closed(
        &self,
        plan: &TriageDocument,
        call: &TriageCall,
    ) -> IvyResult<Option<TriageCall>> {
        self.guard(plan, true).await?;
        let owner = self.owner(plan)?;
        let absent: Option<fn(_, _) -> std::future::Ready<IvyResult<Option<IvyJson>>>> = None;
        let result = reconcile_native_operation_local(
            &owner,
            &call.original()?,
            NativeOperationJournal {
                current: call.clone(),
                previous: call.observation.clone(),
                was_observed: call.observation.is_some(),
                retain: |observed| self.retain_observation(plan, call, observed, true),
            },
            absent,
            Some("secretary_triage_absence_conflict"),
        )
        .await?;
        Ok(match result {
            NativeReconciliation::Observed { value, .. } => Some(value),
            _ => None,
        })
    }
    pub fn thread_id(&self, call: &TriageCall) -> IvyResult<Option<JsString>> {
        let Some(observed) = call
            .observation
            .as_ref()
            .filter(|o| o.get("phase") == Some(&"succeeded".into()))
        else {
            return Ok(None);
        };
        let reply = observed.get("reply");
        evidence(reply.is_some_and(|r| r.get("result").is_some()))?;
        let result = record(reply.and_then(|r| r.get("result")));
        let thread = record(result.get("thread"));
        let id = thread.get("id").and_then(IvyJson::as_js_string);
        check(
            id.is_some_and(|id| !id.0.is_empty()) && thread.get("ephemeral") == Some(&false.into()),
            "secretary_triage_thread_missing",
        )?;
        check(
            result.get("model") == call.params.get("model"),
            "secretary_triage_model_changed",
        )?;
        Ok(id.cloned())
    }
    pub fn pending(&self, call: &TriageCall) -> IvyResult<()> {
        check(
            call.observation.as_ref().and_then(|o| o.get("phase")) != Some(&"failed".into()),
            "secretary_triage_native_failed",
        )?;
        check(
            call.observation.as_ref().and_then(|o| o.get("phase"))
                != Some(&"outcome_unknown".into()),
            "secretary_triage_outcome_unknown",
        )
    }
    pub fn released(&self, call: &TriageCall) -> IvyResult<bool> {
        if call.observation.as_ref().and_then(|o| o.get("phase")) != Some(&"succeeded".into()) {
            self.pending(call)?;
            return Ok(false);
        }
        let reply = call.observation.as_ref().unwrap().get("reply");
        let result = record(reply.and_then(|r| r.get("result")));
        let status = result
            .get("status")
            .map(try_js_string)
            .transpose()?
            .unwrap_or_else(|| "undefined".into());
        check(
            reply.is_some_and(|r| r.get("result").is_some())
                && ["unsubscribed", "notLoaded", "notSubscribed"]
                    .iter()
                    .any(|s| status == JsString::from(*s)),
            "secretary_triage_cleanup_failed",
        )?;
        Ok(true)
    }
    fn thread_name(&self, plan: &TriageDocument) -> IvyResult<JsString> {
        let settings = plan.value.required("settings")?;
        triage_name(
            "shared-thread",
            &IvyJson::Array(vec![
                settings
                    .required("target")?
                    .required("serviceNodeId")?
                    .clone(),
                settings.required("threadCwd")?.clone(),
            ]),
        )
    }
    fn shared_thread(&self, plan: &TriageDocument) -> IvyResult<TriageDocument> {
        let name = self.thread_name(plan)?;
        if let Some(saved) = self.store().find(R::Thread, &name, false)? {
            if truthy(saved.value.required("plan")?) {
                return Ok(saved);
            }
            let mut next = saved.value.clone();
            next.insert("plan", pin_value(&plan.pin));
            return self.store().amend(R::Thread, &saved, &next);
        }
        self.store().create(
            R::Thread,
            &name,
            &IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("plan", pin_value(&plan.pin)),
                ("threadId", IvyJson::Null),
            ]),
            false,
        )
    }
    fn release_unstarted(&self, plan: &TriageDocument) -> IvyResult<()> {
        if let Some(shared) = self
            .store()
            .find(R::Thread, &self.thread_name(plan)?, false)?
        {
            if !truthy(shared.value.required("threadId")?)
                && same(shared.value.required("plan")?, &pin_value(&plan.pin))?
            {
                let mut next = shared.value.clone();
                next.insert("plan", IvyJson::Null);
                self.store().amend(R::Thread, &shared, &next)?;
            }
        }
        Ok(())
    }
    pub fn detach_plan(&self, plan: &TriageDocument) -> IvyResult<()> {
        if let Some(shared) = self
            .store()
            .find(R::Thread, &self.thread_name(plan)?, false)?
        {
            if same(shared.value.required("plan")?, &pin_value(&plan.pin))? {
                let mut next = shared.value.clone();
                next.insert("plan", IvyJson::Null);
                self.store().amend(R::Thread, &shared, &next)?;
            }
        }
        Ok(())
    }
    fn remember_thread(&self, plan: &TriageDocument, thread: &JsString) -> IvyResult<()> {
        let shared = self.shared_thread(plan)?;
        if !truthy(shared.value.required("threadId")?) {
            let mut next = shared.value.clone();
            next.insert("threadId", IvyJson::String(thread.clone()));
            self.store().amend(R::Thread, &shared, &next)?;
        }
        Ok(())
    }
    async fn thread_call(
        &self,
        plan: &TriageDocument,
        stored: &TriageDocument,
    ) -> IvyResult<TriageCall> {
        let request = record(Some(
            &self
                .store()
                .native_json("request", stored.value.required("request")?)?,
        ));
        let params = record(request.get("params"));
        self.call(
            plan,
            &"thread".into(),
            &text(&stored.value, "method")?,
            params.get("threadId").and_then(IvyJson::as_js_string),
            false,
        )
        .await
    }
    pub async fn attached(
        &self,
        plan: &TriageDocument,
        start: &TriageCall,
        thread: &JsString,
        epoch: &JsString,
    ) -> IvyResult<bool> {
        if start
            .observation
            .as_ref()
            .and_then(|o| o.get("epoch"))
            .and_then(IvyJson::as_js_string)
            == Some(epoch)
        {
            return Ok(true);
        }
        let mut slot = JsString::from("resume:");
        slot.0.extend(&epoch.0);
        let call = self
            .call(plan, &slot, &"thread/resume".into(), Some(thread), true)
            .await?;
        let resumed = self
            .advance_checked(plan, &call, Some(Before::Epoch(epoch)))
            .await?;
        let Some(id) = self.thread_id(&resumed)? else {
            self.pending(&resumed)?;
            return Ok(false);
        };
        check(
            &id == thread
                && resumed
                    .observation
                    .as_ref()
                    .and_then(|o| o.get("epoch"))
                    .and_then(IvyJson::as_js_string)
                    == Some(epoch),
            "secretary_triage_thread_changed",
        )?;
        Ok(true)
    }
    pub async fn begin_turn(&self, plan: &TriageDocument) -> IvyResult<Option<TriageStarted>> {
        let existing = self.store().find(
            R::Call,
            &triage_name(
                "call",
                &IvyJson::Array(vec![pin_value(&plan.pin), "thread".into()]),
            )?,
            false,
        )?;
        let shared = self.shared_thread(plan)?;
        if existing.is_none()
            && !truthy(shared.value.required("threadId")?)
            && !same(shared.value.required("plan")?, &pin_value(&plan.pin))?
        {
            return Ok(None);
        }
        let call = match existing {
            Some(call) => self.thread_call(plan, &call).await?,
            None => {
                self.call(
                    plan,
                    &"thread".into(),
                    &if truthy(shared.value.required("threadId")?) {
                        "thread/resume"
                    } else {
                        "thread/start"
                    }
                    .into(),
                    shared.value.get("threadId").and_then(IvyJson::as_js_string),
                    true,
                )
                .await?
            }
        };
        let start = self.advance(plan, &call).await?;
        let Some(thread) = self.thread_id(&start)? else {
            self.pending(&start)?;
            return Ok(None);
        };
        *self.active_thread_id.borrow_mut() = Some(thread.clone());
        self.remember_thread(plan, &thread)?;
        let call = self
            .call(
                plan,
                &"turn".into(),
                &"turn/start".into(),
                Some(&thread),
                true,
            )
            .await?;
        let turn = self
            .advance_checked(
                plan,
                &call,
                Some(Before::Turn {
                    start: &start,
                    thread: &thread,
                }),
            )
            .await?;
        if turn.observation.as_ref().and_then(|o| o.get("phase")) != Some(&"succeeded".into()) {
            self.pending(&turn)?;
            return Ok(None);
        }
        let epoch = text(&self.owner(plan)?.status().await?, "epoch")?;
        if !self.attached(plan, &start, &thread, &epoch).await? {
            return Ok(None);
        }
        Ok(Some(TriageStarted {
            turn,
            thread_id: thread,
            epoch,
        }))
    }
    pub async fn release_closed(&self, plan: &TriageDocument) -> IvyResult<bool> {
        let find = |slot: &str| {
            self.store().find(
                R::Call,
                &triage_name(
                    "call",
                    &IvyJson::Array(vec![pin_value(&plan.pin), slot.into()]),
                )?,
                false,
            )
        };
        let Some(original) = find("thread")? else {
            self.release_unstarted(plan)?;
            return Ok(true);
        };
        let Some(start) = self
            .observe_closed(plan, &self.thread_call(plan, &original).await?)
            .await?
        else {
            self.release_unstarted(plan)?;
            return Ok(true);
        };
        if start.observation.as_ref().and_then(|o| o.get("phase")) == Some(&"failed".into()) {
            self.release_unstarted(plan)?;
            return Ok(true);
        }
        let Some(thread) = self.thread_id(&start)? else {
            self.pending(&start)?;
            return Ok(false);
        };
        self.remember_thread(plan, &thread)?;
        if find("cleanup")?.is_some() {
            let call = self
                .call(
                    plan,
                    &"cleanup".into(),
                    &"thread/unsubscribe".into(),
                    Some(&thread),
                    false,
                )
                .await?;
            return self.released(&self.advance(plan, &call).await?);
        }
        let turn = if find("turn")?.is_some() {
            let turn = self
                .observe_closed(
                    plan,
                    &self
                        .call(
                            plan,
                            &"turn".into(),
                            &"turn/start".into(),
                            Some(&thread),
                            false,
                        )
                        .await?,
                )
                .await?;
            if let Some(turn) = &turn {
                if !turn.observation.as_ref().is_some_and(|o| {
                    ["succeeded", "failed"]
                        .iter()
                        .any(|p| o.get("phase") == Some(&(*p).into()))
                }) {
                    self.pending(turn)?;
                    return Ok(false);
                }
            }
            turn
        } else {
            None
        };
        let owner = self.owner(plan)?;
        let epoch = text(&owner.status().await?, "epoch")?;
        let read = owner
            .read(
                &"thread/read".into(),
                IvyJson::object([
                    ("threadId", IvyJson::String(thread.clone())),
                    ("includeTurns", false.into()),
                ]),
                Some(&epoch),
                false,
            )
            .await?;
        let state = owner.thread_state(&read, &epoch).await?;
        check(
            state.thread.get("id").and_then(IvyJson::as_js_string) == Some(&thread)
                && state.thread.get("ephemeral") == Some(&false.into()),
            "secretary_triage_thread_changed",
        )?;
        if !state.can_start_turn {
            return Ok(false);
        }
        if let Some(turn) = turn.filter(|t| {
            t.observation.as_ref().and_then(|o| o.get("phase")) == Some(&"succeeded".into())
        }) {
            let expected = turn
                .observation
                .as_ref()
                .and_then(|o| o.get("reply"))
                .and_then(|r| r.get("result"))
                .and_then(|r| r.get("turn"))
                .and_then(|t| t.get("id"))
                .and_then(IvyJson::as_js_string);
            let last = owner
                .read(
                    &"thread/turns/list".into(),
                    crate::triage_completion::read_params(&thread, false),
                    Some(&epoch),
                    false,
                )
                .await?;
            let result = record(last.get("reply").and_then(|r| r.get("result")));
            let rows = result.get("data").and_then(IvyJson::as_array);
            check(
                expected.is_some() && rows.is_some_and(|r| r.len() == 1),
                "secretary_triage_turn_changed",
            )?;
            let observed = record(Some(&rows.unwrap()[0]));
            check(
                observed.get("id").and_then(IvyJson::as_js_string) == expected,
                "secretary_triage_turn_changed",
            )?;
            let status = observed
                .get("status")
                .map(try_js_string)
                .transpose()?
                .unwrap_or_else(|| "undefined".into());
            if !["completed", "failed", "interrupted"]
                .iter()
                .any(|s| status == JsString::from(*s))
            {
                return Ok(false);
            }
        }
        let call = self
            .call(
                plan,
                &"cleanup".into(),
                &"thread/unsubscribe".into(),
                Some(&thread),
                true,
            )
            .await?;
        self.released(
            &self
                .advance_checked(plan, &call, Some(Before::Epoch(&epoch)))
                .await?,
        )
    }
}
