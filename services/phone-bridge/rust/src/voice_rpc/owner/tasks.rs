use super::*;
const TASK_KEY: &str = "prepared-phone-voice";

impl PhoneCodexVoice {
    pub fn prewarm(
        &self,
        _principal_id: JsString,
        selection: VoiceSelection,
    ) -> VoiceControlOperation<()> {
        let owner = self.0.clone();
        operation(async move {
            if !owner.state.lock().unwrap().sessions.is_empty()
                || !owner.journal.current_calls().await?.is_empty()
            {
                return Ok(());
            }
            owner.control.open().wait().await?;
            if owner.settings().get("resumeIncomingConversation") == Some(&true.into())
                && owner.state.lock().unwrap().sessions.is_empty()
                && owner.journal.current_calls().await?.is_empty()
            {
                if let Some(context) = owner
                    .journal
                    .voice_conversation(&owner.fingerprint()?)
                    .await?
                {
                    owner.resume_task(context, selection.clone(), None).await?;
                }
            }
            if !owner.state.lock().unwrap().sessions.is_empty()
                || !owner.journal.current_calls().await?.is_empty()
            {
                return Ok(());
            }
            owner.cached_task(selection, false, None).await?;
            let work = PhoneCodexVoice(owner.clone()).archive();
            let report = owner.clone();
            tokio::spawn(async move {
                if let Err(error) = work.wait().await {
                    report.archive_issue(None, &error);
                }
            });
            Ok(())
        })
    }
    pub fn prepare(
        &self,
        call: IvyJson,
        generation: u32,
        selection: VoiceSelection,
    ) -> VoiceControlOperation<JsString> {
        let owner = self.0.clone();
        operation(async move {
            owner.owner(&call).await?;
            let call_id = text(&call, "callId")?;
            if let Some(existing) = owner
                .journal
                .voice_task(call_id.clone(), generation)
                .await?
            {
                owner.reserve(&existing, &call_id)?;
                return Ok(existing);
            }
            owner.control.open().wait().await?;
            let fingerprint = owner.fingerprint()?;
            if generation > 0
                || call.get("direction") == Some(&"outgoing".into())
                    && owner.settings().get("resetAfterOutgoingCall") != Some(&false.into())
            {
                owner
                    .journal
                    .forget_voice_conversation(&fingerprint)
                    .await?;
            }
            let context = if generation == 0
                && call.get("direction") == Some(&"incoming".into())
                && owner.settings().get("resumeIncomingConversation") == Some(&true.into())
            {
                owner.journal.voice_conversation(&fingerprint).await?
            } else {
                None
            };
            let mut thread_id = None;
            if let Some(context) = context
                && context.get("partyKey") == Some(&party_key(&call)?.into())
            {
                thread_id = owner
                    .resume_task(context.clone(), selection.clone(), Some(call_id.clone()))
                    .await?;
                if thread_id.is_some() {
                    owner
                        .state
                        .lock()
                        .unwrap()
                        .continuations
                        .insert(call_id.clone(), context);
                }
            }
            let thread_id = match thread_id {
                Some(id) => id,
                None => {
                    owner
                        .cached_task(selection, generation > 0, Some(call_id.clone()))
                        .await?
                }
            };
            owner.owner(&call).await?;
            require(
                !owner.state.lock().unwrap().sessions.iter().any(|(_, s)| {
                    let s = s.lock().unwrap();
                    !s.cancelled && s.thread_id == thread_id
                }),
                "phone_voice_task_busy",
                "Prepared Voice task already belongs to another active call.",
            )?;
            owner.reserve(&thread_id, &call_id)?;
            let request_hash = hash_json(&IvyJson::object([
                ("threadId", thread_id.clone().into()),
                ("generation", IvyJson::Number(f64::from(generation))),
                ("fingerprint", fingerprint.into()),
            ]))?;
            let intent = IvyJson::object([
                ("method", "call.bindVoice".into()),
                ("epoch", field(&call, "epoch")?.clone()),
                ("callId", call_id.into()),
                ("operationId", uuid().into()),
                ("voiceGeneration", IvyJson::Number(f64::from(generation))),
                ("threadId", thread_id.clone().into()),
                ("requestHash", request_hash.into()),
            ]);
            owner.journal.submit(intent.clone()).await?;
            owner.journal.finish_voice_binding(intent, None).await?;
            Ok(thread_id)
        })
    }
}
impl Inner {
    pub(super) fn thread(&self, value: &IvyJson) -> IvyResult<IvyJson> {
        Ok(super::super::preparation::thread(value, &self.control)?.clone())
    }
    fn config(&self, selection: &VoiceSelection) -> IvyJson {
        let mut config = self
            .settings()
            .get("config")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or_else(|| IvyJson::object([] as [(&str, IvyJson); 0]));
        config.insert(
            "model_reasoning_effort",
            selection.reasoning_effort.clone().into(),
        );
        config
    }
    async fn resume_native(&self, id: &JsString, selection: &VoiceSelection) -> IvyResult<()> {
        self.thread(
            &self
                .call(
                    "thread/resume",
                    IvyJson::object([
                        ("threadId", id.clone().into()),
                        ("cwd", field(self.settings(), "cwd")?.clone()),
                        ("model", selection.model.clone().into()),
                        ("config", self.config(selection)),
                        ("excludeTurns", true.into()),
                    ]),
                    Default::default(),
                )
                .await?,
        )?;
        Ok(())
    }
    async fn update_selection(&self, id: &JsString, selection: &VoiceSelection) -> IvyResult<()> {
        self.call(
            "thread/settings/update",
            IvyJson::object([
                ("threadId", id.clone().into()),
                ("model", selection.model.clone().into()),
                ("effort", selection.reasoning_effort.clone().into()),
            ]),
            Default::default(),
        )
        .await?;
        Ok(())
    }
    async fn cached_task(
        self: &Arc<Self>,
        selection: VoiceSelection,
        replace: bool,
        owner_id: Option<JsString>,
    ) -> IvyResult<JsString> {
        loop {
            self.control.open().wait().await?;
            let key = if replace {
                uuid()
            } else {
                self.fingerprint()?.into()
            };
            let (finished, receive) = watch::channel(None);
            let candidate = VoiceControlOperation::from_receiver(receive);
            let prior = {
                let mut s = self.state.lock().unwrap();
                if let Some(prior) = s.preparing.get(&key) {
                    Some(prior.clone())
                } else {
                    s.preparing.insert(key.clone(), candidate.clone());
                    None
                }
            };
            if let Some(prior) = prior {
                prior.wait().await?;
                continue;
            }
            let owner = self.clone();
            let task_owner = owner.clone();
            let task_selection = selection.clone();
            let task_id = owner_id.clone();
            tokio::spawn(async move {
                let result = tokio::spawn(async move {
                    task_owner
                        .cached_core(task_selection, replace, task_id)
                        .await
                })
                .await
                .unwrap_or_else(|_| Err(IvyError::internal()));
                owner.state.lock().unwrap().preparing.remove(&key);
                finished.send_replace(Some(result));
            });
            return candidate.wait().await;
        }
    }
    async fn cached_core(
        self: Arc<Self>,
        selection: VoiceSelection,
        replace: bool,
        owner_id: Option<JsString>,
    ) -> IvyResult<JsString> {
        let fingerprint = self.fingerprint()?;
        let cache = if replace {
            None
        } else {
            self.journal.task_cache(TASK_KEY, &fingerprint).await?
        };
        require(
            cache
                .as_ref()
                .and_then(|c| c.get("creationOperationId"))
                .and_then(IvyJson::as_js_string)
                .is_none(),
            "phone_voice_create_unknown",
            "The original prepared Voice task creation requires reconciliation.",
        )?;
        let mut thread_id = cache
            .as_ref()
            .and_then(|c| c.get("threadId"))
            .and_then(IvyJson::as_js_string)
            .cloned();
        if thread_id.as_ref().is_some_and(|id| {
            self.state
                .lock()
                .unwrap()
                .reservations
                .get(id)
                .is_some_and(|v| Some(v) != owner_id.as_ref())
        }) {
            thread_id = None;
        }
        if let Some(id) = thread_id.clone() {
            require(
                self.state
                    .lock()
                    .unwrap()
                    .reservations
                    .get(&id)
                    .is_none_or(|v| Some(v) == owner_id.as_ref()),
                "phone_voice_task_busy",
                "Cached Voice task belongs to another original call.",
            )?;
            let used = self.journal.used_codex_voice_task(id.clone()).await?;
            let (reconciled, prepared) = {
                let s = self.state.lock().unwrap();
                (
                    s.reconciled.contains(&id),
                    s.prepared_selections.get(&id).cloned(),
                )
            };
            let verified = reconciled && prepared.is_some();
            if !used
                && (owner_id.is_some()
                    || self.settings().get("keepTaskLoaded") != Some(&false.into()))
                && reconciled
                && prepared.as_ref() == Some(&selection_key(&selection)?)
            {
                return Ok(id);
            }
            if used && verified {
                if self.settings().get("resumeIncomingConversation") != Some(&true.into()) {
                    self.forget_selection(&id);
                }
                thread_id = None;
            } else {
                let reconcile = async {
                    let read = match self
                        .call(
                            "thread/read",
                            IvyJson::object([
                                ("threadId", id.clone().into()),
                                ("includeTurns", false.into()),
                            ]),
                            Default::default(),
                        )
                        .await
                    {
                        Ok(value) => Some(self.thread(&value)?),
                        Err(cause) => {
                            let mut message = JsString::from("thread not loaded: ");
                            message.0.extend(&id.0);
                            if !native_error(&cause, -32600.0, &message) {
                                return Err(cause);
                            }
                            self.state.lock().unwrap().reconciled.remove(&id);
                            None
                        }
                    };
                    let unloaded = match &read {
                        None => true,
                        Some(read) => {
                            field(read, "status")?.get("type") == Some(&"notLoaded".into())
                        }
                    };
                    if unloaded || !self.state.lock().unwrap().reconciled.contains(&id) {
                        self.resume_native(&id, &selection).await?;
                    }
                    let reconciled = self.state.lock().unwrap().reconciled.contains(&id);
                    if !reconciled {
                        self.call(
                            "thread/realtime/stop",
                            IvyJson::object([("threadId", id.clone().into())]),
                            Default::default(),
                        )
                        .await?;
                        self.state.lock().unwrap().reconciled.insert(id.clone());
                    }
                    Ok(())
                }
                .await;
                if let Err(cause) = reconcile {
                    if !missing_or_dead(&cause, &id) {
                        return Err(cause);
                    }
                    self.forget_selection(&id);
                    thread_id = None;
                }
                if used && thread_id.is_some() {
                    if self.settings().get("resumeIncomingConversation") != Some(&true.into()) {
                        self.forget_selection(&id);
                    }
                    thread_id = None;
                }
            }
        }
        let id = match thread_id {
            Some(id) => id,
            None => {
                let creation_id = uuid();
                self.journal
                    .retain_task(
                        TASK_KEY,
                        IvyJson::object([
                            ("fingerprint", fingerprint.clone().into()),
                            ("threadId", IvyJson::Null),
                            ("creationOperationId", creation_id.clone().into()),
                        ]),
                    )
                    .await?;
                let owner = self.clone();
                let started = self
                    .call(
                        "thread/start",
                        IvyJson::object([
                            ("cwd", field(self.settings(), "cwd")?.clone()),
                            (
                                "projectId",
                                self.settings()
                                    .get("projectId")
                                    .cloned()
                                    .unwrap_or(IvyJson::Null),
                            ),
                            ("model", selection.model.clone().into()),
                            ("ephemeral", false.into()),
                            ("config", self.config(&selection)),
                        ]),
                        RequestOptions {
                            request_id: Some(creation_id),
                            before_resolve: Some(Arc::new(move |_, reply| {
                                let owner = owner.clone();
                                Box::pin(async move {
                                    if let Some(result) = reply.get("result") {
                                        let created = owner.thread(result)?;
                                        owner
                                            .journal
                                            .retain_task(
                                                TASK_KEY,
                                                IvyJson::object([
                                                    ("fingerprint", owner.fingerprint()?.into()),
                                                    ("threadId", field(&created, "id")?.clone()),
                                                    ("creationOperationId", IvyJson::Null),
                                                ]),
                                            )
                                            .await?;
                                    }
                                    Ok(())
                                })
                            })),
                            ..Default::default()
                        },
                    )
                    .await?;
                let id = text(&self.thread(&started)?, "id")?;
                self.state.lock().unwrap().reconciled.insert(id.clone());
                self.call(
                    "thread/name/set",
                    IvyJson::object([
                        ("threadId", id.clone().into()),
                        ("name", "Phone Voice".into()),
                    ]),
                    Default::default(),
                )
                .await?;
                id
            }
        };
        self.update_selection(&id, &selection).await?;
        self.preparation.prepare_opened(id.clone()).wait().await?;
        self.state
            .lock()
            .unwrap()
            .prepared_selections
            .insert(id.clone(), selection_key(&selection)?);
        if owner_id.is_none()
            && self.settings().get("keepTaskLoaded") == Some(&false.into())
            && self.journal.current_calls().await?.is_empty()
        {
            self.unload(&id).await?;
        }
        Ok(id)
    }
    async fn resume_task(
        self: &Arc<Self>,
        context: IvyJson,
        selection: VoiceSelection,
        owner_id: Option<JsString>,
    ) -> IvyResult<Option<JsString>> {
        let id = text(&context, "threadId")?;
        let key = {
            let mut k = JsString::from("resume:");
            k.0.extend(&id.0);
            k
        };
        loop {
            {
                let state = self.state.lock().unwrap();
                require(
                    state.sessions.is_empty()
                        && state
                            .reservations
                            .get(&id)
                            .is_none_or(|v| Some(v) == owner_id.as_ref()),
                    "phone_voice_task_busy",
                    "Previous Voice task still belongs to an original call.",
                )?;
                if state.reconciled.contains(&id)
                    && state.prepared_selections.get(&id) == Some(&selection_key(&selection)?)
                {
                    return Ok(Some(id));
                }
            }
            let (finished, receive) = watch::channel(None);
            let candidate = VoiceControlOperation::from_receiver(receive);
            let prior = {
                let mut s = self.state.lock().unwrap();
                if let Some(prior) = s.preparing.get(&key) {
                    Some(prior.clone())
                } else {
                    s.preparing.insert(key.clone(), candidate.clone());
                    None
                }
            };
            if let Some(prior) = prior {
                prior.wait().await?;
                if self
                    .journal
                    .voice_conversation(&self.fingerprint()?)
                    .await?
                    .as_ref()
                    .and_then(|c| c.get("threadId"))
                    != Some(&id.clone().into())
                {
                    return Ok(None);
                }
                continue;
            }
            let owner = self.clone();
            let work = self.clone();
            let thread_id = id.clone();
            let selection = selection.clone();
            let key = key.clone();
            tokio::spawn(async move {
                let result =
                    tokio::spawn(async move { work.resume_core(thread_id, selection).await })
                        .await
                        .unwrap_or_else(|_| Err(IvyError::internal()));
                owner.state.lock().unwrap().preparing.remove(&key);
                finished.send_replace(Some(result.map(|id| id.unwrap_or_default())));
            });
            let result = candidate.wait().await?;
            return Ok(if result.0.is_empty() {
                None
            } else {
                Some(result)
            });
        }
    }
    async fn resume_core(
        self: Arc<Self>,
        id: JsString,
        selection: VoiceSelection,
    ) -> IvyResult<Option<JsString>> {
        let result = async {
            let reconciled = self.state.lock().unwrap().reconciled.contains(&id);
            if !reconciled {
                self.resume_native(&id, &selection).await?;
                self.call(
                    "thread/realtime/stop",
                    IvyJson::object([("threadId", id.clone().into())]),
                    Default::default(),
                )
                .await?;
                self.state.lock().unwrap().reconciled.insert(id.clone());
            }
            self.update_selection(&id, &selection).await?;
            self.state
                .lock()
                .unwrap()
                .prepared_selections
                .insert(id.clone(), selection_key(&selection)?);
            Ok(Some(id.clone()))
        }
        .await;
        match result {
            Ok(id) => Ok(id),
            Err(cause) => {
                self.forget_selection(&id);
                if !missing_or_dead(&cause, &id) {
                    return Err(cause);
                }
                self.journal
                    .forget_voice_conversation(&self.fingerprint()?)
                    .await?;
                (self.on_error)("phone_voice_continuation_unavailable");
                Ok(None)
            }
        }
    }
}
