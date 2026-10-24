use super::*;
use crate::voice_work::{active, view};
pub type InputNotice = Arc<
    dyn Fn(
            IvyJson,
            IvyJson,
            Arc<dyn Fn() -> bool + Send + Sync>,
        ) -> std::pin::Pin<Box<dyn Future<Output = IvyResult<bool>> + Send>>
        + Send
        + Sync,
>;

impl PhoneCodexVoice {
    pub fn set_work_observer(&self, observer: WorkObserver) {
        *self.0.work_observer.lock().unwrap() = Some(observer);
    }
    pub fn set_input_notice(&self, notice: InputNotice) {
        *self.0.notify_input.lock().unwrap() = Some(notice);
    }
    pub async fn work(&self, call: &JsString) -> IvyResult<Option<IvyJson>> {
        self.0.works.initialize().await?;
        self.0.works.for_call(call).as_ref().map(view).transpose()
    }
    pub fn has_work(&self) -> bool {
        self.0.works.has_work()
    }
    pub async fn disconnected(&self, call: &JsString, ended: Option<i64>) -> IvyResult<()> {
        if let Some(session) = self.0.session(call) {
            session.lock().unwrap().cancelled = true;
        }
        self.0.works.detach(call, ended).await?;
        self.0.claim_deadlines(false).await?;
        Ok(())
    }
    pub fn detach(&self, call: JsString, ended: Option<i64>) -> VoiceControlOperation<()> {
        let owner = self.0.clone();
        operation(async move {
            PhoneCodexVoice(owner.clone())
                .disconnected(&call, ended)
                .await?;
            let session = owner.session(&call);
            let observation = session.as_ref().and_then(|s| {
                let s = s.lock().unwrap();
                s.started.then(|| (s.thread_id.clone(), s.generation))
            });
            if let Some((thread, generation)) = observation {
                owner
                    .works
                    .retain_observation(IvyJson::object([
                        ("callId", call.clone().into()),
                        ("threadId", thread.into()),
                        ("generation", (generation as f64).into()),
                        ("fingerprint", owner.fingerprint()?.into()),
                        (
                            "endedAt",
                            ended
                                .map(|n| IvyJson::from(n as f64))
                                .unwrap_or(IvyJson::Null),
                        ),
                    ]))
                    .await?;
            }
            PhoneCodexVoice(owner.clone())
                .stop_media(call, None, true)
                .wait()
                .await?;
            // Reconciliation runs separately; media cleanup cannot wait on a read.
            let report = owner.clone();
            tokio::spawn(async move {
                if let Err(e) = report.observe_detached().await {
                    report.report_work(&"owner".into(), Some(&e.code));
                }
            });
            Ok(())
        })
    }
}
impl Inner {
    fn report_work(&self, id: &JsString, code: Option<&str>) {
        if let Some(observer) = self.work_observer.lock().unwrap().clone() {
            observer(id, code);
        }
    }
    pub(super) fn start_work_timer(owner: &Arc<Self>) {
        let weak = Arc::downgrade(owner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(owner) = weak.upgrade() else {
                    return;
                };
                if owner.control.is_stopping() {
                    return;
                }
                let Ok(_gate) = owner.maintenance.try_lock() else {
                    continue;
                };
                let result = async {
                    owner.works.initialize().await?;
                    owner.claim_deadlines(false).await?;
                    owner.observe_detached_inner().await?;
                    if owner.works.has_work()
                        && !owner
                            .control
                            .current()
                            .is_some_and(|rpc| rpc.transport().connected())
                    {
                        owner.recover_works().await?;
                    }
                    owner.notify_inputs()?;
                    Ok::<_, IvyError>(())
                }
                .await;
                owner.report_work(
                    &"owner".into(),
                    result.as_ref().err().map(|error| error.code.as_str()),
                );
            }
        });
    }
    pub(super) fn input_token(&self, request: &NativeRequest) -> IvyResult<JsString> {
        let epoch = self.state.lock().unwrap().input_epoch.clone();
        Self::token_for(&epoch, request)
    }
    pub(super) fn token_for(epoch: &JsString, request: &NativeRequest) -> IvyResult<JsString> {
        Ok(hash_json(&IvyJson::object([
            ("epoch", epoch.clone().into()),
            (
                "request",
                IvyJson::object([
                    ("id", request.id.clone()),
                    ("method", request.method.clone().into()),
                    ("params", request.params.clone()),
                ]),
            ),
        ]))?
        .into())
    }
    pub(super) async fn work_notification(
        &self,
        notification: &NativeNotification,
    ) -> IvyResult<()> {
        let params = &notification.params;
        let Some(thread) = params.get("threadId").and_then(IvyJson::as_js_string) else {
            return Ok(());
        };
        if notification.method == JsString::from("serverRequest/resolved") {
            self.state.lock().unwrap().pending_inputs.retain(|(id, r)| {
                Some(id) != params.get("requestId")
                    || r.params.get("threadId") != Some(&thread.clone().into())
            });
        }
        if notification.method == JsString::from("turn/started")
            && !self.preparation.is_preparing(thread)
        {
            let session = self
                .state
                .lock()
                .unwrap()
                .sessions
                .iter()
                .find_map(|(_, s)| {
                    let s = s.lock().unwrap();
                    (s.thread_id == *thread && !s.cancelled).then(|| (s.call.clone(), s.started))
                });
            let observation = self.works.observation(thread);
            let detached = if session.is_none() && self.works.get(thread).is_none() {
                if let Some(o) = &observation {
                    self.journal.call(text(o, "callId")?).await?
                } else {
                    None
                }
            } else {
                None
            };
            let call = session
                .as_ref()
                .filter(|(call, started)| {
                    self.works.get(thread).is_none()
                        || *started
                            && !self
                                .state
                                .lock()
                                .unwrap()
                                .rejoining
                                .contains(&text(call, "callId").unwrap_or_default())
                })
                .map(|(c, _)| c)
                .or(detached.as_ref());
            if let Some(turn) = params
                .get("turn")
                .and_then(|v| v.get("id"))
                .and_then(IvyJson::as_js_string)
                && let Some(fingerprint) = self.control.fingerprint()
            {
                let party = call.map(party_key).transpose()?.unwrap_or_default();
                self.works
                    .started(call, thread, turn, &fingerprint, &party)
                    .await?;
                if let Some(call) = detached {
                    self.works
                        .detach(
                            &text(&call, "callId")?,
                            observation
                                .as_ref()
                                .and_then(|o| o.get("endedAt"))
                                .and_then(IvyJson::as_f64)
                                .map(|n| n as i64),
                        )
                        .await?;
                }
            }
        }
        if notification.method == JsString::from("turn/completed")
            && !self.preparation.is_preparing(thread)
            && let Some(turn) = params.get("turn")
        {
            if let (Ok(id), Ok(status)) = (text(turn, "id"), text(turn, "status")) {
                self.works.completed(thread, &id, &status).await?;
                self.state.lock().unwrap().pending_inputs.retain(|(_, r)| {
                    r.params.get("threadId") != Some(&thread.clone().into())
                        || r.params.get("turnId") != Some(&id.clone().into())
                });
            }
        }
        Ok(())
    }
    async fn observed(&self, thread: &JsString) -> IvyResult<()> {
        self.works.observed(thread).await?;
        self.state
            .lock()
            .unwrap()
            .observation_retry_at
            .remove(thread);
        Ok(())
    }
    pub(super) async fn subscribe_work(&self, thread: &JsString) -> IvyResult<()> {
        if self.state.lock().unwrap().reconciled.contains(thread) {
            return Ok(());
        }
        let result = self
            .rpc()?
            .call(
                "thread/resume",
                IvyJson::object([
                    ("threadId", thread.clone().into()),
                    ("cwd", field(self.settings(), "cwd")?.clone()),
                    ("excludeTurns", true.into()),
                ]),
                RequestOptions {
                    detach_on_timeout: true,
                    ..Default::default()
                },
                Some(5000),
            )
            .await?;
        self.thread(&result)?;
        self.state.lock().unwrap().reconciled.insert(thread.clone());
        Ok(())
    }
    async fn read_work(&self, thread: &JsString) -> IvyResult<IvyJson> {
        let value = self
            .rpc()?
            .call(
                "thread/read",
                IvyJson::object([
                    ("threadId", thread.clone().into()),
                    ("includeTurns", true.into()),
                ]),
                RequestOptions {
                    detach_on_timeout: true,
                    ..Default::default()
                },
                Some(5000),
            )
            .await?;
        self.thread(&value)?;
        Ok(value)
    }
    pub(super) async fn observe_work(&self, thread: &JsString) -> IvyResult<()> {
        let Some(work) = self.works.get(thread) else {
            return Ok(());
        };
        let revision = work.get("revision").cloned();
        let value = self.read_work(thread).await?;
        if self
            .works
            .get(thread)
            .as_ref()
            .and_then(|w| w.get("revision"))
            != revision.as_ref()
        {
            return Ok(());
        }
        let turn = value
            .required("thread")?
            .get("turns")
            .and_then(IvyJson::as_array)
            .and_then(|ts| ts.iter().find(|t| t.get("id") == work.get("turnId")));
        if let Some(turn) = turn {
            if turn.get("status") != Some(&"inProgress".into()) {
                self.works
                    .completed(thread, &text(turn, "id")?, &text(turn, "status")?)
                    .await?;
            }
        } else {
            self.works
                .unknown(thread, "phone_work_turn_missing")
                .await?;
        }
        self.observed(thread).await
    }
    pub(super) async fn recover_works(&self) -> IvyResult<()> {
        self.works.initialize().await?;
        self.control.open_cleanup().wait().await?;
        for work in self.works.all().into_iter().filter(|w| {
            active(w)
                || ["halting", "outcome_unknown"]
                    .iter()
                    .any(|s| w.get("state") == Some(&(*s).into()))
        }) {
            require(
                work.get("fingerprint") == Some(&self.fingerprint()?.into()),
                "phone_work_owner_changed",
                "Saved work belongs to another native owner.",
            )?;
            let thread = text(&work, "threadId")?;
            self.subscribe_work(&thread).await?;
            self.observe_work(&thread).await?;
        }
        Ok(())
    }
    async fn observe_detached(self: &Arc<Self>) -> IvyResult<()> {
        let _gate = self.maintenance.lock().await;
        self.observe_detached_inner().await
    }
    async fn observe_detached_inner(&self) -> IvyResult<()> {
        self.works.initialize().await?;
        let observation = self.works.observations().into_iter().find(|o| {
            text(o, "threadId").ok().is_some_and(|id| {
                self.state
                    .lock()
                    .unwrap()
                    .observation_retry_at
                    .get(&id)
                    .copied()
                    .unwrap_or(0)
                    <= self.works.now()
            })
        });
        let Some(observation) = observation else {
            return Ok(());
        };
        let thread = text(&observation, "threadId")?;
        let call_id = text(&observation, "callId")?;
        let revision = self
            .works
            .get(&thread)
            .as_ref()
            .and_then(|w| w.get("revision"))
            .cloned();
        let result = async {
            self.control.open_cleanup().wait().await?;
            require(
                observation.get("fingerprint") == Some(&self.fingerprint()?.into()),
                "phone_work_owner_changed",
                "Saved observation belongs to another native owner.",
            )?;
            self.subscribe_work(&thread).await?;
            let value = self.read_work(&thread).await?;
            if self.works.observation(&thread).as_ref() != Some(&observation) {
                return Ok(());
            }
            let current = self.works.get(&thread);
            if current.as_ref().and_then(|w| w.get("revision")) == revision.as_ref()
                && current.as_ref().is_none_or(|w| {
                    w.get("attachedCallId")
                        .is_none_or(|id| id.is_null() || id == &call_id.clone().into())
                })
            {
                let turns = value
                    .required("thread")?
                    .get("turns")
                    .and_then(IvyJson::as_array);
                if let Some(turn) = turns.and_then(|ts| {
                    ts.iter()
                        .find(|t| t.get("status") == Some(&"inProgress".into()))
                }) {
                    let call = self
                        .journal
                        .call(call_id.clone())
                        .await?
                        .ok_or_else(IvyError::internal)?;
                    self.works
                        .started(
                            Some(&call),
                            &thread,
                            &text(turn, "id")?,
                            &self.fingerprint()?,
                            &party_key(&call)?,
                        )
                        .await?;
                } else if let Some(turn) = turns.and_then(|ts| {
                    ts.iter()
                        .find(|t| current.as_ref().and_then(|w| w.get("turnId")) == t.get("id"))
                }) {
                    self.works
                        .completed(&thread, &text(turn, "id")?, &text(turn, "status")?)
                        .await?;
                }
                self.works
                    .detach(
                        &call_id,
                        observation
                            .get("endedAt")
                            .and_then(IvyJson::as_f64)
                            .map(|n| n as i64),
                    )
                    .await?;
            }
            self.observed(&thread).await
        }
        .await;
        let observer = self.work_observer.lock().unwrap().clone();
        if let Some(observer) = observer {
            observer(&call_id, result.as_ref().err().map(|e| e.code.as_str()));
        }
        if let Err(error) = result {
            self.state
                .lock()
                .unwrap()
                .observation_retry_at
                .insert(thread, self.works.now() + 15_000);
        }
        Ok(())
    }
    pub(super) async fn claim_deadlines(
        self: &Arc<Self>,
        force: bool,
    ) -> IvyResult<Vec<VoiceControlOperation<()>>> {
        let mut operations = vec![];
        for claim in self.works.claim_expired(force).await? {
            operations.push(self.interrupt_work(claim)?);
        }
        Ok(operations)
    }
    pub(super) fn interrupt_work(
        self: &Arc<Self>,
        claim: IvyJson,
    ) -> IvyResult<VoiceControlOperation<()>> {
        let id = text(&claim, "interruptId")?;
        let mut state = self.state.lock().unwrap();
        if let Some(prior) = state.interrupting.get(&id) {
            return Ok(prior.clone());
        }
        if state.interrupted.contains(&id) {
            return Ok(operation(async { Ok(()) }));
        }
        let owner = self.clone();
        let work_id = id.clone();
        let work = operation(async move {
            let result = async {
                owner.control.open_cleanup().wait().await?;
                require(
                    claim.get("fingerprint") == Some(&owner.fingerprint()?.into()),
                    "phone_work_owner_changed",
                    "Saved work belongs to another native owner.",
                )?;
                let thread = text(&claim, "threadId")?;
                owner.subscribe_work(&thread).await?;
                if owner.works.get(&thread).is_none_or(|w| {
                    w.get("state") != Some(&"halting".into())
                        || w.get("interruptId") != claim.get("interruptId")
                }) {
                    return Ok(());
                }
                owner
                    .state
                    .lock()
                    .unwrap()
                    .interrupted
                    .insert(work_id.clone());
                owner
                    .rpc()?
                    .call(
                        "turn/interrupt",
                        IvyJson::object([
                            ("threadId", thread.clone().into()),
                            ("turnId", field(&claim, "turnId")?.clone()),
                        ]),
                        RequestOptions {
                            request_id: Some(work_id.clone()),
                            detach_on_timeout: true,
                            ..Default::default()
                        },
                        Some(5000),
                    )
                    .await?;
                owner.observe_work(&thread).await?;
                if owner
                    .works
                    .get(&thread)
                    .is_some_and(|w| w.get("state") == Some(&"halting".into()))
                {
                    owner
                        .works
                        .unknown(&thread, "phone_work_interrupt_unconfirmed")
                        .await?;
                }
                Ok::<_, IvyError>(())
            }
            .await;
            if let Err(error) = result {
                owner.works.interrupted(&claim, &error.code).await?;
                owner.report_work(&text(&claim, "threadId")?, Some("phone_work_stop_unknown"));
            }
            owner.state.lock().unwrap().interrupting.remove(&work_id);
            Ok(())
        });
        state.interrupting.insert(id, work.clone());
        Ok(work)
    }
    fn notify_inputs(self: &Arc<Self>) -> IvyResult<()> {
        let Some(notify) = self.notify_input.lock().unwrap().clone() else {
            return Ok(());
        };
        let requests = self
            .state
            .lock()
            .unwrap()
            .pending_inputs
            .iter()
            .map(|(_, r)| r.clone())
            .collect::<Vec<_>>();
        for request in requests {
            if ![
                "item/commandExecution/requestApproval",
                "item/fileChange/requestApproval",
                "item/permissions/requestApproval",
                "item/tool/requestUserInput",
                "mcpServer/elicitation/request",
            ]
            .iter()
            .any(|m| request.method == JsString::from(*m))
            {
                continue;
            }
            let Some(work) = request
                .params
                .get("threadId")
                .and_then(IvyJson::as_js_string)
                .and_then(|id| self.works.get(id))
                .filter(|w| {
                    w.get("attachedCallId") == Some(&IvyJson::Null) && self.works.can_answer(w)
                })
            else {
                continue;
            };
            let token = self.input_token(&request)?.to_utf8()?;
            {
                let s = self.state.lock().unwrap();
                if s.notified.contains(&token) || s.notifying.contains_key(&token) {
                    continue;
                }
            }
            let call = work
                .required("callIds")?
                .as_array()
                .and_then(|ids| ids.last())
                .and_then(IvyJson::as_js_string)
                .ok_or_else(IvyError::internal)?
                .clone();
            let Some(input) = PhoneCodexVoice(self.clone())
                .inputs(&call)?
                .into_iter()
                .find(|v| v.get("inputToken") == Some(&token.clone().into()))
            else {
                self.state.lock().unwrap().notifying.remove(&token);
                continue;
            };
            let owner = self.clone();
            let notify = notify.clone();
            let expected = request.clone();
            let thread = text(&work, "threadId")?;
            let weak = Arc::downgrade(self);
            let token_check = token.clone();
            let current: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                weak.upgrade().is_some_and(|o| {
                    !o.control.is_stopping()
                        && o.works.get(&thread).is_some_and(|w| {
                            w.get("attachedCallId") == Some(&IvyJson::Null)
                                && o.works.can_answer(&w)
                        })
                        && o.input_token(&expected)
                            .ok()
                            .is_some_and(|t| t.to_utf8().ok().as_ref() == Some(&token_check))
                        && o.state.lock().unwrap().pending_inputs.iter().any(|(_, r)| {
                            r.id == expected.id
                                && r.method == expected.method
                                && r.params == expected.params
                        })
                })
            });
            let mut state = self.state.lock().unwrap();
            if state.notified.contains(&token) || state.notifying.contains_key(&token) {
                continue;
            }
            let task_token = token.clone();
            let pending = operation(async move {
                let result = notify(work, input, current).await;
                if !matches!(result, Ok(false)) {
                    let observer = owner.work_observer.lock().unwrap().clone();
                    if let Some(observer) = observer {
                        observer(&call, result.as_ref().err().map(|e| e.code.as_str()));
                    }
                }
                match result {
                    Ok(true) => {
                        owner
                            .state
                            .lock()
                            .unwrap()
                            .notified
                            .insert(task_token.clone());
                    }
                    Ok(false) => {}
                    Err(_) => {}
                }
                owner.state.lock().unwrap().notifying.remove(&task_token);
                Ok(())
            });
            state.notifying.insert(token, pending);
        }
        Ok(())
    }
}
