//! Public Phone tools over the call, journal and native owners.
use crate::{
    native_client::PhoneNativeClient,
    service_flow::{PhoneFlow, PhoneVoiceControl},
    service_registry::{PHONE_TOOLS, validate_phone_service},
    voice_selection::VoiceSelection,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::service::ToolHandler;
use std::{collections::HashMap, sync::Arc};
use tokio_util::sync::CancellationToken;

mod audio;

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    field(value, key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn number(value: &IvyJson, key: &str, default: u32) -> u32 {
    value
        .get(key)
        .and_then(IvyJson::as_f64)
        .map(|v| v as u32)
        .unwrap_or(default)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn not_cancelled(signal: &CancellationToken) -> IvyResult<()> {
    require(
        !signal.is_cancelled(),
        "request_cancelled",
        "Phone request was cancelled.",
    )
}
#[derive(Clone)]
pub struct PhoneBridge {
    flow: PhoneFlow,
    native: PhoneNativeClient,
    reconnect: Arc<tokio::sync::Mutex<()>>,
}
impl PhoneBridge {
    pub fn new(flow: PhoneFlow, native: PhoneNativeClient) -> Self {
        Self {
            flow,
            native,
            reconnect: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
    pub fn handlers(&self) -> HashMap<JsString, ToolHandler> {
        PHONE_TOOLS
            .iter()
            .map(|tool| {
                let bridge = self.clone();
                let name = tool.name;
                let handler: ToolHandler = Arc::new(move |args, context| {
                    let bridge = bridge.clone();
                    Box::pin(async move {
                        bridge
                            .invoke(name, args, context.caller_principal_id, &context.signal)
                            .await
                    })
                });
                (format!("phone.{name}").into(), handler)
            })
            .collect()
    }
    async fn original(&self, caller: JsString, id: JsString) -> IvyResult<IvyJson> {
        self.flow
            .calls()
            .journal()
            .authorized_call(self.flow.calls().admission().clone(), caller, id, "read")
            .await
    }
    async fn view(&self, caller: JsString, id: JsString) -> IvyResult<IvyJson> {
        let call = self.original(caller.clone(), id.clone()).await?;
        let journal = self.flow.calls().journal();
        let mut current = journal.current_call(Some(id.clone())).await?.is_some();
        let mut observation = IvyJson::Null;
        if current {
            let observed = self.native.observe(Some(id.clone())).await;
            current = journal.current_call(Some(id.clone())).await?.is_some();
            if current {
                if let Ok(observed) = observed {
                    if let Some(call) = observed
                        .get("call")
                        .filter(|c| c.get("id") == Some(&id.clone().into()))
                    {
                        observation = call.clone();
                    }
                }
            }
        }
        Ok(IvyJson::object([
            ("call", call),
            ("current", current.into()),
            ("observation", observation),
            (
                "work",
                self.flow
                    .voice_work(caller.clone(), id.clone())
                    .await?
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "inputs",
                IvyJson::Array(self.flow.voice_inputs(caller, id).await?),
            ),
        ]))
    }
    async fn idle(&self, message: &str) -> IvyResult<()> {
        require(
            self.flow
                .calls()
                .journal()
                .current_calls()
                .await?
                .is_empty(),
            "phone_call_busy",
            message,
        )
    }
    async fn native_result(
        &self,
        method: &str,
        operation: Option<JsString>,
        timeout: u64,
        code: &str,
    ) -> IvyResult<IvyJson> {
        let reply = self
            .native
            .request(
                method,
                IvyJson::object([] as [(&str, IvyJson); 0]),
                operation,
                timeout,
            )?
            .wait()
            .await?;
        require(
            reply.get("ok") == Some(&true.into()),
            code,
            "Original native Phone action was not confirmed.",
        )?;
        Ok(field(&reply, "result")?.clone())
    }
    /// Caller identity and cancellation come from the authenticated service
    /// invocation. Public arguments cannot replace either of them.
    pub async fn invoke(
        &self,
        name: &str,
        args: IvyJson,
        caller: JsString,
        signal: &CancellationToken,
    ) -> IvyResult<IvyJson> {
        not_cancelled(signal)?;
        let tool = PHONE_TOOLS
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| IvyError::new("tool_unavailable", "Phone tool is unavailable."))?;
        validate_phone_service(tool.input, &args)?;
        let result = match name {
            "voiceWork" => IvyJson::object([(
                "work",
                self.flow
                    .voice_work(caller, text(&args, "callId")?)
                    .await?
                    .unwrap_or(IvyJson::Null),
            )]),
            "speakerVoices" => {
                let id = args.get("callId").and_then(IvyJson::as_js_string).cloned();
                if let Some(id) = &id {
                    self.original(caller, id.clone()).await?;
                }
                let mut catalog = self.flow.speaker_voice_catalog().await?;
                let selection = match id {
                    Some(id) => self.flow.speaker_voice_state(id, &catalog).await?,
                    None => IvyJson::Null,
                };
                catalog.insert("selection", selection);
                catalog
            }
            "selectSpeakerVoice" => {
                self.flow
                    .select_speaker_voice(
                        caller,
                        text(&args, "callId")?,
                        text(&args, "operationId")?,
                        text(&args, "voiceId")?,
                    )
                    .wait()
                    .await?
            }
            "probeLoopback" => {
                self.idle("Loopback probing requires idle Phone.").await?;
                self.native_result("audio.probe", None, 15000, "phone_probe_unavailable")
                    .await?
            }
            "audioSetup" => {
                let inventory = self
                    .native_result("inventory", None, 15000, "phone_inventory_unavailable")
                    .await?;
                validate_phone_service("PhoneInventory", &inventory)?;
                audio::setup(&inventory, &self.flow.audio_profiles())?
            }
            "codecTest" => {
                self.idle("Codec diagnostics require idle Phone.").await?;
                self.native_result("codec.test", None, 15000, "phone_codec_test_unavailable")
                    .await?
            }
            "inventory" => {
                self.native_result("inventory", None, 15000, "phone_inventory_unavailable")
                    .await?
            }
            "reconnect" => {
                let _gate = self.reconnect.lock().await;
                let id = text(&args, "operationId")?;
                if let Some(prior) = self.flow.calls().journal().operation(id.clone()).await? {
                    require(
                        prior.get("intent").and_then(|v| v.get("method"))
                            == Some(&"registration.reconnect".into()),
                        "mutation_conflict",
                        "Operation belongs to another command.",
                    )?;
                    require(
                        prior.get("phase") == Some(&"result".into())
                            && prior.get("receipt").and_then(|v| v.get("ok")) == Some(&true.into()),
                        "phone_operation_retained",
                        "Original reconnect is unconfirmed and cannot be repeated.",
                    )?;
                    field(field(&prior, "receipt")?, "result")?.clone()
                } else {
                    self.idle("Reconnect requires all original calls to finish.")
                        .await?;
                    self.native_result(
                        "registration.reconnect",
                        Some(id),
                        15000,
                        "phone_reconnect_failed",
                    )
                    .await?
                }
            }
            "calls" => IvyJson::object([(
                "calls",
                IvyJson::Array(self.flow.calls().journal().current_calls().await?),
            )]),
            "status" => self.status(caller).await?,
            "request" | "voiceCall" => {
                let mut request = args.clone();
                request.insert("principalId", caller.into());
                if name == "voiceCall" {
                    request.insert("route", "voice".into());
                    request.insert("voicePrompt", field(&args, "initialPrompt")?.clone());
                    if let IvyJson::Object(items) = &mut request {
                        items.retain(|(key, _)| key != &JsString::from("initialPrompt"));
                    }
                }
                self.flow.request(request).wait().await?
            }
            "selectVoice" | "restartVoice" => {
                let control = if name == "restartVoice" {
                    PhoneVoiceControl::Restart
                } else {
                    PhoneVoiceControl::Select(VoiceSelection {
                        model: text(&args, "model")?,
                        reasoning_effort: text(&args, "reasoningEffort")?,
                    })
                };
                self.flow
                    .control_voice(
                        caller,
                        text(&args, "callId")?,
                        text(&args, "operationId")?,
                        control,
                    )
                    .wait()
                    .await?
            }
            "update" => {
                let id = text(&args, "callId")?;
                let operation = text(&args, "operationId")?;
                if args.get("model").is_none() && args.get("reasoningEffort").is_none() {
                    self.flow
                        .select_speaker_voice(caller, id, operation, text(&args, "voiceId")?)
                        .wait()
                        .await?
                } else {
                    self.original(caller.clone(), id.clone()).await?;
                    let prior = self
                        .flow
                        .calls()
                        .journal()
                        .operation(operation.clone())
                        .await?;
                    let base = if let Some(intent) = prior
                        .as_ref()
                        .and_then(|p| p.get("intent"))
                        .filter(|i| i.get("method") == Some(&"call.selectVoice".into()))
                    {
                        intent.clone()
                    } else {
                        self.flow.voice_selection(&id).await?.to_wire()
                    };
                    let selection = VoiceSelection {
                        model: args
                            .get("model")
                            .unwrap_or(base.required("model")?)
                            .as_js_string()
                            .cloned()
                            .ok_or_else(IvyError::internal)?,
                        reasoning_effort: args
                            .get("reasoningEffort")
                            .unwrap_or(base.required("reasoningEffort")?)
                            .as_js_string()
                            .cloned()
                            .ok_or_else(IvyError::internal)?,
                    };
                    let control =
                        if let Some(voice) = args.get("voiceId").and_then(IvyJson::as_js_string) {
                            PhoneVoiceControl::Update {
                                selection,
                                voice_id: voice.clone(),
                            }
                        } else {
                            PhoneVoiceControl::Select(selection)
                        };
                    self.flow
                        .control_voice(caller, id, operation, control)
                        .wait()
                        .await?
                }
            }
            "history" => IvyJson::object([(
                "calls",
                IvyJson::Array(
                    self.flow
                        .calls()
                        .journal()
                        .history(None, number(&args, "limit", 20))
                        .await?,
                ),
            )]),
            "screen" => {
                let request = IvyJson::object([
                    ("principalId", caller.into()),
                    ("operationId", field(&args, "operationId")?.clone()),
                    ("recipientId", field(&args, "recipientId")?.clone()),
                    (
                        "screening",
                        IvyJson::object([
                            ("announcement", field(&args, "announcement")?.clone()),
                            (
                                "timeoutSeconds",
                                f64::from(number(&args, "timeoutSeconds", 30)).into(),
                            ),
                            (
                                "repeatCount",
                                f64::from(number(&args, "repeatCount", 1)).into(),
                            ),
                        ]),
                    ),
                ]);
                self.flow.screen(request).wait().await?
            }
            "bridgeScreening" => {
                self.flow
                    .bridge_screening(caller, text(&args, "callId")?)
                    .wait()
                    .await?
            }
            "accept" => {
                self.flow
                    .accept(caller, text(&args, "operationId")?, text(&args, "callId")?)
                    .wait()
                    .await?
            }
            "call" => self.view(caller, text(&args, "callId")?).await?,
            "voiceInputs" => IvyJson::object([(
                "inputs",
                IvyJson::Array(
                    self.flow
                        .voice_inputs(caller, text(&args, "callId")?)
                        .await?,
                ),
            )]),
            "answerVoiceInput" => {
                self.flow
                    .answer_voice_input(
                        caller,
                        text(&args, "callId")?,
                        field(&args, "id")?.clone(),
                        field(&args, "reply")?.clone(),
                        text(&args, "inputToken")?,
                    )
                    .wait()
                    .await?;
                IvyJson::object([("state", "submitted".into())])
            }
            "logs" => {
                let id = text(&args, "callId")?;
                self.original(caller, id.clone()).await?;
                let entries = self
                    .flow
                    .logs()
                    .map(|l| l.read(&id, number(&args, "limit", 20)))
                    .transpose()?
                    .unwrap_or_default();
                IvyJson::object([("entries", IvyJson::Array(entries))])
            }
            "operation" => {
                let id = text(&args, "callId")?;
                let operation = if let Some(operation) =
                    args.get("operationId").and_then(IvyJson::as_js_string)
                {
                    self.original(caller, id.clone()).await?;
                    let prior = self
                        .flow
                        .calls()
                        .journal()
                        .operation(operation.clone())
                        .await?;
                    require(
                        prior.as_ref().is_none_or(|p| {
                            p.get("intent").and_then(|i| i.get("callId"))
                                == Some(&id.clone().into())
                        }),
                        "mutation_conflict",
                        "Operation belongs to another call.",
                    )?;
                    prior
                } else {
                    self.flow
                        .calls()
                        .read(
                            caller,
                            id,
                            text(&args, "method")?,
                            number(&args, "generation", 0),
                        )
                        .await?
                };
                operation.unwrap_or(IvyJson::Null)
            }
            "hangup" => {
                let id = text(&args, "callId")?;
                self.flow.hangup(caller.clone(), id.clone()).wait().await?;
                self.view(caller, id).await?
            }
            _ => return Err(IvyError::internal()),
        };
        validate_phone_service(tool.output, &result)?;
        not_cancelled(signal)?;
        Ok(result)
    }
    async fn status(&self, caller: JsString) -> IvyResult<IvyJson> {
        let observed = self.native.observe(None).await?;
        let journal = self.flow.calls().journal();
        let calls = journal.current_calls().await?;
        let mut pending = vec![];
        let ids = match observed.get("callIds").and_then(IvyJson::as_array) {
            Some(ids) => ids.to_vec(),
            None => observed
                .get("call")
                .and_then(|v| v.get("id"))
                .cloned()
                .into_iter()
                .collect(),
        };
        for id in ids {
            let id = id.as_js_string().cloned().ok_or_else(IvyError::internal)?;
            if journal.call(id.clone()).await?.is_some() {
                continue;
            }
            let scoped =
                if observed.get("call").and_then(|v| v.get("id")) == Some(&id.clone().into()) {
                    observed.clone()
                } else {
                    self.native.observe(Some(id)).await?
                };
            if let Some(candidate) = scoped.get("call").filter(|c| {
                c.get("direction") == Some(&"incoming".into())
                    && c.get("state") == Some(&"ringing".into())
            }) {
                match self.flow.calls().admission().incoming(
                    self.native.epoch().clone().into(),
                    uuid::Uuid::new_v4().to_string().into(),
                    caller.clone().into(),
                    candidate,
                ) {
                    Ok(_) => pending.push(candidate.clone()),
                    Err(e) if e.code == "phone_caller_refused" => {}
                    Err(e) => return Err(e),
                }
            }
        }
        let voice = calls
            .iter()
            .find(|c| self.flow.call_route(c) == "voice".into());
        let (models, models_error) = match self.flow.voice_model_catalog().await {
            Ok(m) => (m, IvyJson::Null),
            Err(e) => (IvyJson::Array(vec![]), e.code.into()),
        };
        let mut speaker = IvyJson::Null;
        let mut speaker_error = IvyJson::Null;
        let selection = if let Some(call) = voice {
            let id = text(call, "callId")?;
            match self.flow.speaker_voice_catalog().await {
                Ok(catalog) => {
                    speaker = self.flow.speaker_voice_state(id.clone(), &catalog).await?
                }
                Err(e) => speaker_error = e.code.into(),
            }
            self.flow.voice_selection(&id).await?.to_wire()
        } else {
            IvyJson::Null
        };
        let recipients = field(self.flow.calls().admission().definition(), "recipients")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|r| field(r, "id").cloned())
            .collect::<IvyResult<Vec<_>>>()?;
        Ok(IvyJson::object([
            ("epoch", self.native.epoch().clone().into()),
            ("registration", field(&observed, "registration")?.clone()),
            (
                "micro",
                observed.get("micro").cloned().unwrap_or(IvyJson::Null),
            ),
            ("configuration", self.flow.configuration().await?),
            (
                "busy",
                (!calls.is_empty() || observed.get("call").is_some_and(|c| !c.is_null())).into(),
            ),
            ("recipients", IvyJson::Array(recipients)),
            ("voiceSelection", selection),
            ("voiceModels", models),
            ("voiceModelsError", models_error),
            ("speakerVoice", speaker),
            ("speakerVoicesError", speaker_error),
            ("call", calls.first().cloned().unwrap_or(IvyJson::Null)),
            (
                "incoming",
                pending.first().cloned().unwrap_or(IvyJson::Null),
            ),
            ("calls", IvyJson::Array(calls)),
            ("incomingCalls", IvyJson::Array(pending)),
        ]))
    }
}
