use super::*;

impl PhoneCodexVoice {
    pub fn archive(&self) -> VoiceControlOperation<()> {
        let (finished, receive) = watch::channel(None);
        let candidate = VoiceControlOperation::from_receiver(receive);
        {
            let mut state = self.0.state.lock().unwrap();
            if let Some(work) = &state.archiving {
                return work.clone();
            }
            state.archiving = Some(candidate.clone());
        }
        let owner = self.0.clone();
        let work = owner.clone();
        tokio::spawn(async move {
            let result = tokio::spawn(async move { work.archive_core().await })
                .await
                .unwrap_or_else(|_| Err(IvyError::internal()));
            owner.state.lock().unwrap().archiving = None;
            finished.send_replace(Some(result));
        });
        candidate
    }
}
impl Inner {
    pub(super) fn archive_issue(&self, thread_id: Option<&JsString>, error: &IvyError) {
        let message = IvyJson::object([
            ("event", "phone_codex_archive_issue".into()),
            (
                "threadId",
                thread_id.cloned().map_or(IvyJson::Null, Into::into),
            ),
            ("code", error.code_js().into()),
        ]);
        if let Ok(message) = stringify(&message, MANAGEMENT_FRAME_BYTES) {
            eprintln!("{message}");
        }
    }
    async fn archive_idle(&self) -> IvyResult<bool> {
        let available = !self.control.is_stopping()
            && self
                .control
                .current()
                .is_some_and(|rpc| rpc.transport().connected());
        let idle = {
            let state = self.state.lock().unwrap();
            state.sessions.is_empty() && state.reservations.is_empty() && state.preparing.is_empty()
        };
        Ok(available && idle && self.journal.current_calls().await?.is_empty())
    }
    async fn archive_protected(&self, id: &JsString) -> IvyResult<bool> {
        let fingerprint = self.fingerprint()?;
        Ok(self
            .journal
            .task_cache("prepared-phone-voice", &fingerprint)
            .await?
            .as_ref()
            .and_then(|c| c.get("threadId"))
            == Some(&id.clone().into())
            || self
                .journal
                .voice_conversation(&fingerprint)
                .await?
                .as_ref()
                .and_then(|c| c.get("threadId"))
                == Some(&id.clone().into()))
    }
    async fn archive_call(
        &self,
        method: &str,
        params: IvyJson,
        hooks: RequestOptions,
    ) -> IvyResult<IvyJson> {
        self.rpc()?.archive_call(method, params, hooks).await
    }
    async fn archive_core(&self) -> IvyResult<()> {
        if !self.archive_idle().await? {
            return Ok(());
        }
        let fingerprint = self.fingerprint()?;
        let cursor = self.state.lock().unwrap().binding_cursor.clone();
        let bindings = self.journal.codex_binding_page(&cursor, 64).await?;
        for operation in &bindings {
            let intent = field(operation, "intent")?;
            self.state.lock().unwrap().binding_cursor = text(intent, "operationId")?.to_utf8()?;
            if intent.get("method") != Some(&"call.bindVoice".into()) {
                continue;
            }
            let id = text(intent, "threadId")?;
            let generation = intent
                .get("voiceGeneration")
                .cloned()
                .unwrap_or(IvyJson::Number(0.0));
            if intent.get("requestHash")
                == Some(
                    &hash_json(&IvyJson::object([
                        ("threadId", id.clone().into()),
                        ("generation", generation),
                        ("fingerprint", fingerprint.clone().into()),
                    ]))?
                    .into(),
                )
            {
                self.journal.remember_codex_task(&fingerprint, id).await?;
            }
        }
        if bindings.len() < 64 {
            self.state.lock().unwrap().binding_cursor.clear();
        }
        let cursor = self.state.lock().unwrap().archive_cursor.clone();
        let mut tasks = self
            .journal
            .codex_archive_page(&fingerprint, &cursor, 16)
            .await?;
        if tasks.is_empty() && !cursor.is_empty() {
            self.state.lock().unwrap().archive_cursor.clear();
            tasks = self
                .journal
                .codex_archive_page(&fingerprint, "", 16)
                .await?;
        }
        for task in tasks {
            if !self.archive_idle().await? {
                return Ok(());
            }
            let id = text(&task, "threadId")?;
            self.state.lock().unwrap().archive_cursor = id.to_utf8()?;
            if self.archive_protected(&id).await? {
                continue;
            }
            let result = self.archive_task(&fingerprint, &task, &id).await;
            match result {
                Ok(false) => return Ok(()),
                Ok(true) => (),
                Err(cause) => self.archive_issue(Some(&id), &cause),
            }
        }
        Ok(())
    }
    async fn archive_task(
        &self,
        fingerprint: &str,
        task: &IvyJson,
        id: &JsString,
    ) -> IvyResult<bool> {
        if task
            .get("operationId")
            .and_then(IvyJson::as_js_string)
            .is_some()
        {
            let mut cursor: Option<JsString> = None;
            let mut confirmed = false;
            let mut cursors = BTreeSet::new();
            loop {
                if !self.archive_idle().await? {
                    return Ok(false);
                }
                let mut params = IvyJson::object([
                    ("archived", true.into()),
                    ("useStateDbOnly", true.into()),
                    (
                        "projectId",
                        self.settings()
                            .get("projectId")
                            .cloned()
                            .unwrap_or(IvyJson::Null),
                    ),
                    ("cwd", field(self.settings(), "cwd")?.clone()),
                    ("limit", IvyJson::Number(100.0)),
                ]);
                if let Some(cursor) = &cursor {
                    params.insert("cursor", IvyJson::String(cursor.clone()));
                }
                let page = self
                    .archive_call("thread/list", params, Default::default())
                    .await?;
                if field(&page, "data")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .any(|v| v.get("id") == Some(&id.clone().into()))
                {
                    confirmed = true;
                    break;
                }
                cursor = page
                    .get("nextCursor")
                    .filter(|v| !v.is_null())
                    .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
                    .transpose()?;
                require(
                    cursor.as_ref().is_none_or(|c| {
                        c.0.is_empty() || !cursors.contains(c) && cursors.len() < 16
                    }),
                    "phone_voice_archive_unknown",
                    "Original Codex archival remains unconfirmed.",
                )?;
                match &cursor {
                    Some(c) if !c.0.is_empty() => {
                        cursors.insert(c.clone());
                    }
                    _ => break,
                }
            }
            if confirmed {
                self.journal
                    .finish_codex_archive(fingerprint, task.clone())
                    .await?;
                self.forget_selection(id);
            } else {
                self.archive_issue(
                    Some(id),
                    &IvyError::new(
                        "phone_voice_archive_unknown",
                        "Original Codex archival remains unconfirmed.",
                    ),
                );
            }
            return Ok(true);
        }
        let observed = self.thread(
            &self
                .archive_call(
                    "thread/read",
                    IvyJson::object([
                        ("threadId", id.clone().into()),
                        ("includeTurns", false.into()),
                    ]),
                    Default::default(),
                )
                .await?,
        )?;
        if ![Some(&"idle".into()), Some(&"notLoaded".into())]
            .contains(&field(&observed, "status")?.get("type"))
        {
            return Ok(true);
        }
        let children = self
            .archive_call(
                "thread/list",
                IvyJson::object([
                    ("ancestorThreadId", id.clone().into()),
                    ("archived", false.into()),
                    (
                        "sourceKinds",
                        IvyJson::Array(
                            [
                                "cli",
                                "vscode",
                                "exec",
                                "appServer",
                                "subAgent",
                                "subAgentReview",
                                "subAgentCompact",
                                "subAgentThreadSpawn",
                                "subAgentOther",
                                "unknown",
                            ]
                            .map(IvyJson::from)
                            .to_vec(),
                        ),
                    ),
                    ("useStateDbOnly", true.into()),
                    ("limit", IvyJson::Number(100.0)),
                ]),
                Default::default(),
            )
            .await?;
        if children
            .get("nextCursor")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|cursor| !cursor.0.is_empty())
            || field(&children, "data")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .any(|v| {
                    ![Some(&"idle".into()), Some(&"notLoaded".into())]
                        .contains(&v.get("status").and_then(|s| s.get("type")))
                })
        {
            return Ok(true);
        }
        if !self.archive_idle().await? || self.archive_protected(id).await? {
            return Ok(false);
        }
        let operation_id = uuid();
        self.journal
            .begin_codex_archive(fingerprint, task.clone(), operation_id.clone())
            .await?;
        let mut retained = task.clone();
        retained.insert("operationId", operation_id.clone().into());
        let journal = self.journal.clone();
        let fingerprint = fingerprint.to_owned();
        self.archive_call(
            "thread/archive",
            IvyJson::object([("threadId", id.clone().into())]),
            RequestOptions {
                request_id: Some(operation_id),
                before_resolve: Some(Arc::new(move |_, reply| {
                    let journal = journal.clone();
                    let fingerprint = fingerprint.clone();
                    let retained = retained.clone();
                    Box::pin(async move {
                        if reply.get("result").is_some() {
                            journal.finish_codex_archive(&fingerprint, retained).await?;
                        }
                        Ok(())
                    })
                })),
                ..Default::default()
            },
        )
        .await?;
        self.forget_selection(id);
        let message = IvyJson::object([
            ("event", "phone_codex_task_archived".into()),
            ("threadId", id.clone().into()),
        ]);
        println!("{}", stringify(&message, MANAGEMENT_FRAME_BYTES)?);
        Ok(true)
    }
}
