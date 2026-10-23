use super::*;
use crate::{
    contracts::OUTCOME_BYTES,
    service_settings::{PhoneConfigurationSection, phone_configuration_section, phone_credentials},
    service_speech::synthesize_announcement,
};

struct AdmissionGuard {
    flow: PhoneFlow,
    id: JsString,
}
impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        self.flow
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admissions
            .remove(&self.id);
    }
}
impl PhoneFlow {
    fn admit(&self, action: FlowFuture<IvyJson>) -> PhoneFlowWork {
        let mut state = self.0.state.lock().unwrap();
        if state.closed {
            return work(async { Err(IvyError::new("service_not_ready", "Phone is stopping.")) });
        }
        let id: JsString = uuid::Uuid::new_v4().to_string().into();
        let flow = self.clone();
        let key = id.clone();
        let pending = work(async move {
            let _admission = AdmissionGuard {
                flow: flow.clone(),
                id: key,
            };
            let call = action.await?;
            if flow.0.state.lock().unwrap().closed {
                flow.cleanup(text(&call, "principalId")?, text(&call, "callId")?)
                    .wait()
                    .await?;
                return Err(IvyError::new(
                    "service_not_ready",
                    "Phone stopped during original admission.",
                ));
            }
            flow.schedule(call.clone()).await?;
            Ok(call)
        });
        state.admissions.insert(id, pending.clone());
        pending
    }
    /// The caller is supplied by the service, never by the public tool input.
    /// The remaining fields are operationId, recipientId, route, destination and
    /// voicePrompt. A prompt for an existing Voice recipient stays on that call.
    pub fn request(&self, args: IvyJson) -> PhoneFlowWork {
        let flow = self.clone();
        self.admit(Box::pin(async move { flow.request_call(args).await }))
    }
    async fn request_call(&self, mut args: IvyJson) -> IvyResult<IvyJson> {
        let principal = text(&args, "principalId")?;
        let operation = text(&args, "operationId")?;
        let selected = args
            .get("route")
            .filter(|v| !v.is_null())
            .cloned()
            .or_else(|| {
                self.0
                    .settings
                    .get("outgoingRoute")
                    .filter(|v| !v.is_null())
                    .cloned()
            })
            .unwrap_or("windows".into());
        args.insert("route", selected.clone());
        if let Some(prior) = self.calls().journal().operation(operation.clone()).await? {
            let intent = field(&prior, "intent")?;
            require(
                intent.get("method") == Some(&"call.forwardVoice".into())
                    && intent
                        .get("callId")
                        .and_then(IvyJson::as_js_string)
                        .is_some(),
                "mutation_conflict",
                "Phone request belongs to another command.",
            )?;
            args.insert("epoch", field(intent, "epoch")?.clone());
            let admission = self.calls().admission().outgoing(&args)?;
            require(
                intent.get("requestHash") == Some(&hash_json(&admission)?.into()),
                "mutation_conflict",
                "Original forwarded Phone request changed.",
            )?;
            let call = self
                .original(principal, text(intent, "callId")?, "read")
                .await?;
            return self.forward_voice(call, admission).await;
        }
        if selected == "voice".into() {
            require(
                self.0.voice.is_some() && self.codex().is_some(),
                "phone_voice_unconfigured",
                "Voice calls require the configured Codex CLI runtime.",
            )?;
            let prompt = args
                .get("voicePrompt")
                .and_then(IvyJson::as_js_string)
                .filter(|v| !v.0.is_empty());
            let recipient = args
                .get("recipientId")
                .and_then(IvyJson::as_js_string)
                .filter(|v| !v.0.is_empty());
            if let (Some(_), Some(recipient)) = (prompt, recipient) {
                if self
                    .calls()
                    .journal()
                    .call_for_operation(operation)
                    .await?
                    .is_none()
                {
                    for call in self.calls().journal().current_calls().await? {
                        if self.call_route(&call) == "voice".into()
                            && self
                                .calls()
                                .admission()
                                .matches_recipient(&call, recipient)?
                        {
                            args.insert("epoch", self.0.native.epoch().clone().into());
                            return self
                                .forward_voice(call, self.calls().admission().outgoing(&args)?)
                                .await;
                        }
                    }
                }
            }
        } else {
            self.windows_audio()?;
        }
        self.calls().admit_outgoing(args).wait().await
    }
    pub fn accept(&self, principal: JsString, operation: JsString, id: JsString) -> PhoneFlowWork {
        let flow = self.clone();
        self.admit(Box::pin(async move {
            let mut args = IvyJson::object([
                ("principalId", principal.into()),
                ("operationId", operation.into()),
                ("callId", id.into()),
            ]);
            if let Some(route) = flow
                .0
                .settings
                .get("incomingRoute")
                .filter(|v| !v.is_null())
            {
                args.insert("route", route.clone());
            }
            flow.calls().admit_incoming(args).wait().await
        }))
    }
    pub fn screen(&self, mut args: IvyJson) -> PhoneFlowWork {
        let flow = self.clone();
        self.admit(Box::pin(async move {
            flow.windows_audio()?;
            require(
                flow.0.settings.get("speech").is_some_and(|v| !v.is_null()),
                "phone_speech_unconfigured",
                "Screening requires configured speech synthesis.",
            )?;
            args.insert("route", "windows".into());
            flow.calls().admit_outgoing(args).wait().await
        }))
    }
    async fn forward_voice(&self, call: IvyJson, admission: IvyJson) -> IvyResult<IvyJson> {
        let id = text(&call, "callId")?;
        let operation = text(&admission, "operationId")?;
        let hash = hash_json(&admission)?;
        loop {
            let gate = self.0.commands.lock().await;
            let pending = self.0.state.lock().unwrap().work.get(&id).cloned();
            if let Some(pending) = pending {
                drop(gate);
                let _ = pending.wait().await;
                continue;
            }
            if let Some(prior) = self.calls().journal().operation(operation.clone()).await? {
                let intent = field(&prior, "intent")?;
                require(
                    intent.get("method") == Some(&"call.forwardVoice".into())
                        && intent.get("callId") == Some(&id.clone().into())
                        && intent.get("requestHash") == Some(&hash.clone().into()),
                    "mutation_conflict",
                    "Original forwarded Voice request changed.",
                )?;
                if prior.get("phase") == Some(&"result".into())
                    && prior.get("receipt").is_some_and(|r| {
                        r.get("ok") == Some(&true.into())
                            && r.get("result").and_then(|r| r.get("state")) == Some(&"sent".into())
                    })
                {
                    return Ok(call);
                }
                return Err(IvyError::new(
                    "phone_voice_prompt_unknown",
                    "Original forwarded Voice prompt remains unknown and cannot be repeated.",
                )
                .with_outcome(Outcome::Unknown));
            }
            self.continuing(&call).await?;
            let flow = self.clone();
            let original = call.clone();
            let operation = operation.clone();
            let hash = hash.clone();
            let prompt = text(&admission, "voicePrompt")?;
            let pending = self.reserve(
                id.clone(),
                Box::pin(async move {
                    flow.continuing(&original).await?;
                    let id = text(&original, "callId")?;
                    let generation = flow
                        .calls()
                        .journal()
                        .latest_voice_generation(id.clone())
                        .await?;
                    let thread = match generation {
                        Some(g) => flow.calls().journal().voice_task(id, g).await?,
                        None => None,
                    };
                    require(
                        flow.0.voice.is_some() && thread.is_some(),
                        "phone_voice_task_missing",
                        "Current call has no bound Voice task.",
                    )?;
                    flow.voice()?
                        .prompt(
                            original.clone(),
                            thread.unwrap(),
                            prompt,
                            operation,
                            hash.into(),
                            flow.connected_guard(original),
                        )
                        .await?;
                    Ok(IvyJson::Null)
                }),
            )?;
            drop(gate);
            pending.wait().await?;
            return Ok(call);
        }
    }
    async fn schedule(&self, call: IvyJson) -> IvyResult<()> {
        let id = text(&call, "callId")?;
        let _gate = self.0.commands.lock().await;
        let running = self.0.state.lock().unwrap().work.contains_key(&id);
        if running
            || call.get("epoch") != Some(&self.0.native.epoch().clone().into())
            || self
                .calls()
                .journal()
                .current_call(Some(id.clone()))
                .await?
                .is_none()
        {
            return Ok(());
        }
        let command = if call.get("direction") == Some(&"outgoing".into()) {
            "call.dial"
        } else {
            "call.answer"
        };
        if self
            .calls()
            .journal()
            .call_command(id.clone(), command.into(), 0)
            .await?
            .is_some()
        {
            return Ok(());
        }
        let flow = self.clone();
        self.reserve(
            id.clone(),
            Box::pin(async move {
                if let Err(cause) = flow.connect(&call).await {
                    flow.0.state.lock().unwrap().abandoned.insert(id.clone());
                    flow.report(&id, &cause);
                    if let Err(cleanup) = flow.cleanup_when_observed(&call, None).await {
                        flow.report(&id, &cleanup);
                    }
                }
                Ok(IvyJson::Null)
            }),
        )?;
        Ok(())
    }
    async fn prepare_voice(
        &self,
        call: &IvyJson,
        generation: u32,
    ) -> IvyResult<(JsString, JsString)> {
        self.continuing(call).await?;
        require(
            self.0.voice.is_some() && self.codex().is_some(),
            "phone_voice_unconfigured",
            "Voice requires its Codex runtime.",
        )?;
        if self.0.state.lock().unwrap().models.is_empty() {
            self.voice_model_catalog().await?;
        }
        let id = text(call, "callId")?;
        let selection = self.voice_selection(&id).await?;
        selection.validate(&IvyJson::Array(self.0.state.lock().unwrap().models.clone()))?;
        self.0
            .state
            .lock()
            .unwrap()
            .selections
            .insert(id.clone(), selection.clone());
        let queue = self
            .codex()
            .and_then(|v| v.get("queueMs"))
            .and_then(IvyJson::as_f64)
            .unwrap_or(80.0) as u32;
        let prebuffer = self
            .codex()
            .and_then(|v| v.get("playbackPrebufferMs"))
            .and_then(IvyJson::as_f64)
            .map(|v| v as u32);
        let media = self.calls().prepare_realtime(
            text(call, "principalId")?,
            id.clone(),
            generation,
            Some(queue),
            prebuffer,
        );
        let (thread, media) = tokio::join!(
            self.voice()?.prepare(call.clone(), generation, selection),
            media.wait()
        );
        let thread = thread?;
        let offer = operation_result(&media?)?;
        require(
            offer.get("callId") == Some(&id.into())
                && offer.get("generation") == Some(&f64::from(generation).into())
                && offer.get("sdp").and_then(IvyJson::as_js_string).is_some(),
            "phone_voice_sdp_invalid",
            "Native WebRTC offer belongs to another call or generation.",
        )?;
        self.continuing(call).await?;
        Ok((thread, text(&offer, "sdp")?))
    }
    async fn connect(&self, call: &IvyJson) -> IvyResult<()> {
        self.continuing(call).await?;
        let usage = self.calls().journal().status().await?;
        let limits = self.calls().journal().limits();
        require(
            field(&usage, "operations")?
                .as_f64()
                .unwrap_or(f64::INFINITY)
                + 16.0
                <= limits.max_operations as f64
                && field(&usage, "reservedBytes")?
                    .as_f64()
                    .unwrap_or(f64::INFINITY)
                    + 16.0 * OUTCOME_BYTES as f64
                    <= limits.max_bytes as f64,
            "phone_journal_capacity",
            "Phone requires room for the complete original call and cleanup before starting.",
        )?;
        let principal = text(call, "principalId")?;
        let id = text(call, "callId")?;
        let incoming = call.get("direction") == Some(&"incoming".into());
        let creation = if incoming {
            self.calls().claim(principal.clone(), id.clone())
        } else {
            self.calls().prepare(principal.clone(), id.clone())
        };
        self.call_result(
            call,
            &creation.wait().await?,
            if incoming {
                &["ringing"]
            } else {
                &["prepared"]
            },
        )?;
        let challenged = incoming
            && self
                .calls()
                .admission()
                .challenges_incoming(field(call, "incoming")?)?;
        let challenge = if challenged {
            let path = self
                .0
                .settings
                .get("accessCodePath")
                .and_then(IvyJson::as_js_string)
                .cloned()
                .ok_or_else(|| {
                    IvyError::new(
                        "phone_access_unconfigured",
                        "Incoming challenge requires protected code settings.",
                    )
                })?;
            let value =
                phone_configuration_section(path, PhoneConfigurationSection::Access).await?;
            validate_phone("CallAccessSettings", &value)?;
            value
        } else {
            IvyJson::Null
        };
        operation_result(
            &self
                .calls()
                .features(principal.clone(), id.clone(), challenge.clone())
                .wait()
                .await?,
        )?;
        let screening = call.get("screening").filter(|v| !v.is_null());
        if let Some(screening) = screening {
            let settings = self
                .0
                .settings
                .get("speech")
                .filter(|v| !v.is_null())
                .ok_or_else(|| {
                    IvyError::new(
                        "phone_speech_unconfigured",
                        "Screening requires speech settings.",
                    )
                })?;
            let mut announcement = synthesize_announcement(
                self.calls().journal(),
                call,
                text(screening, "announcement")?,
                settings,
            )
            .await?;
            self.continuing(call).await?;
            for key in ["timeoutSeconds", "repeatCount"] {
                announcement.insert(key, field(screening, key)?.clone());
            }
            operation_result(
                &self
                    .calls()
                    .prepare_screening(principal.clone(), id.clone(), announcement)
                    .wait()
                    .await?,
            )?;
        }
        let route = self.call_route(call);
        let mut prepared = None;
        if incoming {
            self.continuing(call).await?;
            self.call_result(
                call,
                &self
                    .calls()
                    .answer(principal.clone(), id.clone(), false)
                    .wait()
                    .await?,
                &["connected"],
            )?;
        } else {
            let credentials = phone_credentials(
                self.0
                    .settings
                    .get("credentialsPath")
                    .and_then(IvyJson::as_js_string)
                    .cloned(),
            )
            .await?;
            self.continuing(call).await?;
            let dial = self.calls().dial(
                principal.clone(),
                id.clone(),
                credentials
                    .get("username")
                    .and_then(IvyJson::as_js_string)
                    .cloned(),
                credentials
                    .get("password")
                    .and_then(IvyJson::as_js_string)
                    .cloned(),
                field(&self.0.settings, "ringSeconds")?
                    .as_f64()
                    .unwrap_or(30.0) as u32,
                false,
            );
            let prepare = async {
                if screening.is_none() && !challenged && route == "voice".into() {
                    self.prepare_voice(call, 0).await.map(Some)
                } else {
                    Ok(None)
                }
            };
            let (dial, voice) = tokio::join!(dial.wait(), prepare);
            self.call_result(call, &dial?, &["connected"])?;
            prepared = voice?;
        }
        let _ =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.0.on_connected)(&id)));
        if screening.is_some() {
            return Ok(());
        }
        if challenged {
            let deadline = tokio::time::Instant::now()
                + Duration::from_secs(
                    challenge
                        .get("timeoutSeconds")
                        .and_then(IvyJson::as_f64)
                        .unwrap_or(60.0) as u64
                        + 5,
                );
            loop {
                self.continuing(call).await?;
                let status = self.0.native.observe(Some(id.clone())).await?;
                let observed = status
                    .get("call")
                    .filter(|v| {
                        v.get("id") == Some(&id.clone().into())
                            && v.get("state") == Some(&"connected".into())
                    })
                    .ok_or_else(|| {
                        IvyError::new("phone_access_failed", "Original challenge call ended.")
                    })?;
                let access = observed.get("features").and_then(|v| v.get("access"));
                if access == Some(&"authenticated".into()) {
                    break;
                }
                require(
                    access == Some(&"awaiting_code".into())
                        && tokio::time::Instant::now() < deadline,
                    "phone_access_failed",
                    "Original access challenge did not authenticate.",
                )?;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            operation_result(
                &self
                    .calls()
                    .upgrade_codec(principal.clone(), id.clone())
                    .wait()
                    .await?,
            )?;
        }
        if route == "windows".into() {
            operation_result(
                &self
                    .calls()
                    .connect_windows(principal, id, self.windows_audio()?)
                    .wait()
                    .await?,
            )?;
            return Ok(());
        }
        let (thread, sdp) = match prepared {
            Some(prepared) => prepared,
            None => self.prepare_voice(call, 0).await?,
        };
        self.start_voice(call, thread, sdp, 0).await
    }
}
