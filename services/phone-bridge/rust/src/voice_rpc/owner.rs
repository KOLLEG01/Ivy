//! One configured Voice owner: durable task preparation, active sessions and
//! idle archival share the same control connection and original Phone journal.
use super::*;
use crate::{journal::PhoneJournal, voice_selection::VoiceSelection};
use ivy_contracts::{
    JsString, Outcome,
    hash::{digest, hash_json},
    json::{canonical, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::{
    codex_home::HomeContext,
    codex_rpc::{NativeNotification, NativeRequest},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    sync::{Mutex, Weak},
    time::Duration,
};
use tokio::{sync::watch, task::AbortHandle};

mod archive;
pub(super) mod instructions;
mod sessions;
mod tasks;
mod work;

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_voice_owner.rs"]
mod tests;

pub type VoiceGuard = Arc<dyn Fn() -> IvyResult<()> + Send + Sync>;
pub type VoiceAsyncGuard = Arc<dyn Fn() -> VoiceClose + Send + Sync>;
#[derive(Clone)]
pub enum VoiceSelectionCommand {
    Sequence(u32),
    Operation(JsString),
    Update {
        operation_id: JsString,
        voice_id: JsString,
    },
}

struct Session {
    call: IvyJson,
    thread_id: JsString,
    generation: u32,
    starting: Option<VoiceControlOperation<JsString>>,
    stopping: Option<VoiceControlOperation<()>>,
    prompting: Option<VoiceControlOperation<()>>,
    cancelled: bool,
    failed: bool,
    realtime_requested: bool,
    started: bool,
    items: Vec<IvyJson>,
    transcript_ids: VecDeque<JsString>,
    speaker_voice_id: Option<JsString>,
}
struct PendingSdp {
    result: watch::Sender<Option<IvyResult<JsString>>>,
    timer: AbortHandle,
}
impl Drop for PendingSdp {
    fn drop(&mut self) {
        self.timer.abort();
    }
}
#[derive(Default)]
struct State {
    reconciled: BTreeSet<JsString>,
    prepared_selections: BTreeMap<JsString, String>,
    reservations: BTreeMap<JsString, JsString>,
    preparing: BTreeMap<JsString, VoiceControlOperation<JsString>>,
    // Native Map iteration is insertion ordered, including the four-input page.
    sessions: Vec<(JsString, Arc<Mutex<Session>>)>,
    continuations: BTreeMap<JsString, IvyJson>,
    sdps: BTreeMap<JsString, PendingSdp>,
    pending_inputs: Vec<(IvyJson, NativeRequest)>,
    archiving: Option<VoiceControlOperation<()>>,
    archive_cursor: String,
    binding_cursor: String,
    input_epoch: JsString,
    rejoining: BTreeSet<JsString>,
    interrupting: BTreeMap<JsString, VoiceControlOperation<()>>,
    interrupted: BTreeSet<JsString>,
    observation_retry_at: BTreeMap<JsString, i64>,
    notified: BTreeSet<String>,
    notifying: BTreeMap<String, VoiceControlOperation<()>>,
}
struct Inner {
    work_observer: Mutex<Option<WorkObserver>>,
    control: VoiceControl,
    preparation: VoicePreparation,
    journal: PhoneJournal,
    on_error: Arc<dyn Fn(&str) + Send + Sync>,
    state: Mutex<State>,
    works: crate::voice_work::PhoneVoiceWorks,
    maintenance: tokio::sync::Mutex<()>,
    notify_input: Mutex<Option<work::InputNotice>>,
}
#[derive(Clone)]
pub struct PhoneCodexVoice(Arc<Inner>);
pub type WorkObserver = Arc<dyn Fn(&JsString, Option<&str>) + Send + Sync>;

fn operation<T: Clone + Send + Sync + 'static>(
    work: impl Future<Output = IvyResult<T>> + Send + 'static,
) -> VoiceControlOperation<T> {
    let (finished, receiver) = watch::channel(None);
    tokio::spawn(async move {
        let result = tokio::spawn(work)
            .await
            .unwrap_or_else(|_| Err(IvyError::internal()));
        finished.send_replace(Some(result));
    });
    VoiceControlOperation::from_receiver(receiver)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(v: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    v.get(key).ok_or_else(IvyError::internal)
}
fn text(v: &IvyJson, key: &str) -> IvyResult<JsString> {
    field(v, key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn selection_key(selection: &VoiceSelection) -> IvyResult<String> {
    canonical(&selection.to_wire(), MANAGEMENT_FRAME_BYTES)
}
fn uuid() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
fn native_error(cause: &IvyError, code: f64, message: &JsString) -> bool {
    cause
        .details
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(IvyJson::as_f64)
        == Some(code)
        && cause
            .details
            .as_ref()
            .and_then(|v| v.get("message"))
            .and_then(IvyJson::as_js_string)
            == Some(message)
}
fn missing_or_dead(cause: &IvyError, thread_id: &JsString) -> bool {
    let mut missing = JsString::from("no rollout found for thread id ");
    missing.0.extend(&thread_id.0);
    native_error(cause, -32600.0, &missing)
        || native_error(
            cause,
            -32603.0,
            &"failed to stop realtime conversation: internal error; agent loop died unexpectedly"
                .into(),
        )
}
impl PhoneCodexVoice {
    pub fn new(
        settings: VoiceControlSettings,
        journal: PhoneJournal,
        connector: Arc<dyn VoiceConnector>,
        schemas: Arc<dyn VoiceSchemas>,
        on_error: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> IvyResult<Self> {
        let relay = Arc::new(Mutex::new(Weak::<Inner>::new()));
        let opened = relay.clone();
        let notifications = relay.clone();
        let requests = relay.clone();
        let closed = relay.clone();
        let control = VoiceControl::new(
            settings,
            connector,
            schemas,
            VoiceControlHooks {
                on_open: Arc::new(move || {
                    if let Some(owner) = opened.lock().unwrap().upgrade() {
                        owner.reset_connection_state();
                    }
                }),
                on_error: on_error.clone(),
                events: Events {
                    on_request: Arc::new(move |request| {
                        let owner = requests.lock().unwrap().upgrade();
                        Box::pin(async move {
                            if let Some(owner) = owner {
                                owner.input(request).await?;
                            }
                            Ok(())
                        })
                    }),
                    on_notification: Arc::new(move |notification| {
                        let owner = notifications.lock().unwrap().upgrade();
                        Box::pin(async move {
                            if let Some(owner) = owner {
                                owner.notification(notification).await?;
                            }
                            Ok(())
                        })
                    }),
                    on_close: Arc::new(move |code| {
                        let owner = closed.lock().unwrap().upgrade();
                        Box::pin(async move {
                            if let Some(owner) = owner {
                                owner.connection_lost(&code);
                            }
                            Ok(())
                        })
                    }),
                },
            },
        )?;
        let preparation = VoicePreparation::new(control.clone(), Arc::new(journal.clone()));
        let owner = Arc::new(Inner {
            work_observer: Mutex::new(None),
            control,
            preparation,
            works: crate::voice_work::PhoneVoiceWorks::new(journal.clone()),
            journal,
            on_error,
            state: Mutex::new(State::default()),
            maintenance: tokio::sync::Mutex::new(()),
            notify_input: Mutex::new(None),
        });
        *relay.lock().unwrap() = Arc::downgrade(&owner);
        Inner::start_work_timer(&owner);
        Ok(Self(owner))
    }
    /// Same executing-account snapshot, submitted schemas and process owner as
    /// VoiceControl::configured. Construction never opens another connection.
    pub fn configured(
        native: IvyJson,
        artifact_root: JsString,
        data_root: JsString,
        credential: Option<JsString>,
        context: &HomeContext<'_>,
        journal: PhoneJournal,
        on_error: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> IvyResult<Self> {
        Self::new(
            VoiceControlSettings {
                native,
                artifact_root,
                data_root,
                credential,
                paths: super::process::paths_snapshot(context.paths),
            },
            journal,
            Arc::new(NativeVoiceConnector::new(context)),
            Arc::new(ivy_contracts::schema::SchemaValidators::new()),
            on_error,
        )
    }
    pub async fn models(&self) -> IvyResult<IvyJson> {
        self.0.control.models().await
    }
    pub async fn speaker_voices(&self) -> IvyResult<IvyJson> {
        self.0.control.speaker_voices().await
    }
    pub fn speaker_voice(&self, call_id: &JsString) -> Option<JsString> {
        self.0.session(call_id).and_then(|s| {
            let s = s.lock().unwrap();
            if s.cancelled || s.failed {
                None
            } else {
                s.speaker_voice_id.clone()
            }
        })
    }
    pub fn failed(&self, call_id: &JsString) -> bool {
        self.0
            .session(call_id)
            .is_some_and(|s| s.lock().unwrap().failed)
    }
    pub fn inputs(&self, call_id: &JsString) -> IvyResult<Vec<IvyJson>> {
        let session = self.0.session(call_id);
        let thread_id = session
            .map(|s| s.lock().unwrap().thread_id.clone())
            .or_else(|| {
                self.0
                    .works
                    .for_call(call_id)
                    .and_then(|w| text(&w, "threadId").ok())
            });
        let epoch = self.0.state.lock().unwrap().input_epoch.clone();
        let state = self.0.state.lock().unwrap();
        let rpc = self.0.control.current();
        state
            .pending_inputs
            .iter()
            .filter(|(_, r)| {
                r.params.get("threadId").and_then(IvyJson::as_js_string) == thread_id.as_ref()
            })
            .take(4)
            .map(|(_, r)| {
                let schema = rpc
                    .as_ref()
                    .and_then(|rpc| rpc.catalog.get("serverRequests"))
                    .and_then(IvyJson::as_array)
                    .and_then(|defs| {
                        defs.iter()
                            .find(|d| d.get("method") == Some(&r.method.clone().into()))
                    })
                    .and_then(|d| d.get("outputSchema"))
                    .cloned()
                    .ok_or_else(IvyError::internal)?;
                Ok(IvyJson::object([
                    ("id", r.id.clone()),
                    ("inputToken", Inner::token_for(&epoch, r)?.into()),
                    ("method", r.method.clone().into()),
                    ("params", r.params.clone()),
                    ("replySchema", schema),
                ]))
            })
            .collect()
    }
    pub fn answer(
        &self,
        call_id: JsString,
        id: IvyJson,
        reply: IvyJson,
        input_token: JsString,
    ) -> IvyResult<VoiceControlOperation<()>> {
        let key = id.clone();
        let request = self
            .0
            .state
            .lock()
            .unwrap()
            .pending_inputs
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, r)| r.clone());
        let session = self.0.session(&call_id);
        let work = self.0.works.for_call(&call_id);
        let thread = session
            .as_ref()
            .map(|s| s.lock().unwrap().thread_id.clone())
            .or_else(|| work.as_ref().and_then(|w| text(w, "threadId").ok()));
        require(
            request.as_ref().is_some_and(|r| {
                self.0.input_token(r).ok().as_ref() == Some(&input_token)
                    && r.params.get("threadId").and_then(IvyJson::as_js_string) == thread.as_ref()
            }),
            "phone_voice_input_missing",
            "Voice input no longer belongs to this call.",
        )?;
        require(
            work.as_ref()
                .map(|w| {
                    self.0.works.can_answer(w)
                        && request.as_ref().is_some_and(|r| {
                            r.params
                                .get("turnId")
                                .is_none_or(|turn| turn.is_null() || Some(turn) == w.get("turnId"))
                        })
                })
                .unwrap_or_else(|| {
                    session.as_ref().is_some_and(|s| {
                        let s = s.lock().unwrap();
                        !s.cancelled && !s.failed
                    })
                }),
            "phone_call_cancelled",
            "Voice input belongs to an ended session.",
        )?;
        let request = request.unwrap();
        let rpc = self.0.rpc()?;
        if let Some(result) = reply.get("result") {
            let definition = rpc
                .catalog
                .get("serverRequests")
                .and_then(IvyJson::as_array)
                .and_then(|defs| {
                    defs.iter()
                        .find(|d| d.get("method") == Some(&request.method.clone().into()))
                })
                .ok_or_else(IvyError::internal)?;
            rpc.schemas
                .validate(field(definition, "outputSchema")?, result, 256 * 1024)?;
        }
        let owner = Arc::downgrade(&self.0);
        Ok(operation(async move {
            rpc.transport()
                .answer(
                    request.method,
                    id,
                    reply,
                    Arc::new(move |_| {
                        let owner = owner.clone();
                        let key = key.clone();
                        let input_token = input_token.clone();
                        let call_id = call_id.clone();
                        Box::pin(async move {
                            if let Some(owner) = owner.upgrade() {
                                let current = owner
                                    .state
                                    .lock()
                                    .unwrap()
                                    .pending_inputs
                                    .iter()
                                    .find(|(k, _)| k == &key)
                                    .map(|(_, r)| r.clone());
                                require(
                                    current.as_ref().is_some_and(|r| {
                                        owner.input_token(r).ok().as_ref() == Some(&input_token)
                                    }),
                                    "phone_voice_input_missing",
                                    "Native input was replaced before submission.",
                                )?;
                                if let Some(work) = owner.works.for_call(&call_id) {
                                    require(
                                        owner.works.can_answer(&work) && current.as_ref().is_some_and(|r| {
                                            r.params.get("threadId") == work.get("threadId")
                                                && r.params.get("turnId").is_none_or(|turn| turn.is_null() || Some(turn) == work.get("turnId"))
                                        }),
                                        "phone_call_cancelled",
                                        "Original Voice deadline expired before submission.",
                                    )?;
                                }
                                owner
                                    .state
                                    .lock()
                                    .unwrap()
                                    .pending_inputs
                                    .retain(|(k, _)| k != &key);
                            }
                            Ok(())
                        })
                    }),
                )
                .await
        }))
    }
    pub fn close(&self) -> VoiceControlOperation<()> {
        let owner = self.0.clone();
        self.0.control.close(Box::pin(async move {
            let _maintenance = owner.maintenance.lock().await;
            let halts = owner.claim_deadlines(true).await?;
            let notices = owner
                .state
                .lock()
                .unwrap()
                .notifying
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for notice in notices {
                let _ = notice.wait().await;
            }
            let preparing = owner
                .state
                .lock()
                .unwrap()
                .preparing
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for work in preparing {
                let _ = work.wait().await;
            }
            let archive = owner.state.lock().unwrap().archiving.clone();
            if let Some(archive) = archive {
                let _ = archive.wait().await;
            }
            let calls = owner
                .state
                .lock()
                .unwrap()
                .sessions
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            let mut stops = tokio::task::JoinSet::new();
            for id in calls {
                let work = PhoneCodexVoice(owner.clone()).stop(id, None);
                stops.spawn(async move { work.wait().await });
            }
            while let Some(result) = stops.join_next().await {
                result.map_err(|_| IvyError::internal())??;
            }
            for halt in halts {
                let _ = halt.wait().await;
            }
            let interrupting = owner
                .state
                .lock()
                .unwrap()
                .interrupting
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for halt in interrupting {
                let _ = halt.wait().await;
            }
            Ok(())
        }))
    }
}
impl Inner {
    fn settings(&self) -> &IvyJson {
        self.control.native_settings()
    }
    fn fingerprint(&self) -> IvyResult<String> {
        self.control.fingerprint().ok_or_else(IvyError::internal)
    }
    fn rpc(&self) -> IvyResult<VoiceRpc> {
        self.control.current().ok_or_else(|| {
            IvyError::new(
                "phone_voice_unavailable",
                "Codex Voice transport is unavailable.",
            )
        })
    }
    async fn call(
        &self,
        method: &str,
        params: IvyJson,
        hooks: RequestOptions,
    ) -> IvyResult<IvyJson> {
        self.rpc()?.call(method, params, hooks, None).await
    }
    fn session(&self, id: &JsString) -> Option<Arc<Mutex<Session>>> {
        self.state
            .lock()
            .unwrap()
            .sessions
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, s)| s.clone())
    }
    fn reset_connection_state(&self) {
        let mut s = self.state.lock().unwrap();
        s.reconciled.clear();
        s.prepared_selections.clear();
        s.input_epoch = uuid();
    }
    fn connection_lost(&self, code: &str) {
        self.preparation.connection_lost(code);
        let mut state = self.state.lock().unwrap();
        for (_, s) in &state.sessions {
            s.lock().unwrap().failed = true;
        }
        for (_, pending) in std::mem::take(&mut state.sdps) {
            pending.result.send_replace(Some(Err(IvyError::new(
                code,
                "Codex Voice connection ended.",
            ))));
        }
        state.reconciled.clear();
        state.prepared_selections.clear();
        state.pending_inputs.clear();
    }
    async fn input(&self, request: NativeRequest) -> IvyResult<()> {
        self.works.initialize().await?;
        let thread = request
            .params
            .get("threadId")
            .and_then(IvyJson::as_js_string)
            .cloned();
        let retained = thread
            .as_ref()
            .is_some_and(|id| self.works.get(id).is_some());
        {
            let mut state = self.state.lock().unwrap();
            if !retained
                && !state.sessions.iter().any(|(_, s)| {
                    request.params.get("threadId")
                        == Some(&s.lock().unwrap().thread_id.clone().into())
                })
            {
                return Ok(());
            }
            require(
                state.pending_inputs.len() < 64,
                "phone_voice_input_capacity",
                "Too many pending Voice inputs.",
            )?;
            let key = request.id.clone();
            if let Some((_, old)) = state.pending_inputs.iter_mut().find(|(k, _)| k == &key) {
                *old = request;
            } else {
                state.pending_inputs.push((key, request));
            }
        }
        if let Some(thread) = thread {
            self.works.waiting(&thread).await?;
        }
        Ok(())
    }
    async fn notification(&self, notification: NativeNotification) -> IvyResult<()> {
        self.works.initialize().await?;
        self.work_notification(&notification).await?;
        self.preparation.notification(&notification);
        let Some(thread_id) = notification
            .params
            .get("threadId")
            .and_then(IvyJson::as_js_string)
        else {
            return Ok(());
        };
        let mut state = self.state.lock().unwrap();
        if notification.method == JsString::from("thread/realtime/item/completed")
            && self.settings().get("resumeIncomingConversation") == Some(&true.into())
        {
            if let Some(item) = notification.params.get("item")
                && item.get("type") == Some(&"transcriptSegment".into())
                && let (Some(id), Some(_)) = (
                    item.get("id").and_then(IvyJson::as_js_string),
                    item.get("text").and_then(IvyJson::as_js_string),
                )
                && [Some(&"user".into()), Some(&"assistant".into())].contains(&item.get("role"))
                && let Some((_, session)) = state
                    .sessions
                    .iter()
                    .find(|(_, s)| &s.lock().unwrap().thread_id == thread_id)
            {
                let mut s = session.lock().unwrap();
                if !s.transcript_ids.contains(id) {
                    s.transcript_ids.push_back(id.clone());
                    if s.transcript_ids.len() > 512 {
                        s.transcript_ids.pop_front();
                    }
                    let mut items = s.items.clone();
                    items.push(IvyJson::object([
                        ("role", field(item, "role")?.clone()),
                        ("text", field(item, "text")?.clone()),
                    ]));
                    s.items = bounded_history(&items, 24000, 126)?;
                }
            }
        }
        if notification.method == JsString::from("thread/status/changed")
            && notification
                .params
                .get("status")
                .and_then(|s| s.get("type"))
                == Some(&"notLoaded".into())
        {
            state.reconciled.remove(thread_id);
            state.prepared_selections.remove(thread_id);
        }
        if notification.method == JsString::from("thread/realtime/sdp")
            && let Some(sdp) = notification
                .params
                .get("sdp")
                .and_then(IvyJson::as_js_string)
            && let Some(pending) = state.sdps.remove(thread_id)
        {
            pending.result.send_replace(Some(Ok(sdp.clone())));
        }
        if notification.method == JsString::from("thread/realtime/error")
            || notification.method == JsString::from("thread/realtime/closed")
        {
            let active = state
                .sessions
                .iter()
                .find(|(_, s)| &s.lock().unwrap().thread_id == thread_id)
                .map(|(_, s)| s.clone())
                .is_some_and(|s| {
                    let mut s = s.lock().unwrap();
                    if s.cancelled {
                        false
                    } else {
                        s.failed = true;
                        true
                    }
                });
            if notification.method == JsString::from("thread/realtime/error") || active {
                state.reconciled.remove(thread_id);
                state.prepared_selections.remove(thread_id);
            }
            if let Some(pending) = state.sdps.remove(thread_id) {
                pending.result.send_replace(Some(Err(IvyError::new(
                    "phone_voice_session_ended",
                    "Codex Voice ended before media connection.",
                ))));
            }
            drop(state);
            if active {
                (self.on_error)("phone_voice_session_ended");
            }
        }
        Ok(())
    }
    async fn owner(&self, call: &IvyJson) -> IvyResult<()> {
        let id = text(call, "callId")?;
        require(
            !self.control.is_stopping()
                && call.get("epoch")
                    == Some(
                        &self
                            .journal
                            .epoch()
                            .await?
                            .map_or(IvyJson::Null, Into::into),
                    )
                && self.journal.current_call(Some(id.clone())).await?.is_some()
                && self
                    .journal
                    .call_command(id.clone(), "call.hangup".into(), 0)
                    .await?
                    .is_none()
                && self
                    .journal
                    .call_command(id, "call.release".into(), 0)
                    .await?
                    .is_none(),
            "phone_call_cancelled",
            "Original call ended during Voice task preparation.",
        )
    }
    fn reserve(&self, thread_id: &JsString, call_id: &JsString) -> IvyResult<()> {
        let mut state = self.state.lock().unwrap();
        require(
            state
                .reservations
                .get(thread_id)
                .is_none_or(|id| id == call_id),
            "phone_voice_task_busy",
            "Prepared Voice task is reserved for another original call.",
        )?;
        state
            .reservations
            .insert(thread_id.clone(), call_id.clone());
        Ok(())
    }
    fn forget_selection(&self, id: &JsString) {
        let mut s = self.state.lock().unwrap();
        s.reconciled.remove(id);
        s.prepared_selections.remove(id);
    }
    async fn unload(&self, id: &JsString) -> IvyResult<()> {
        self.forget_selection(id);
        self.call(
            "thread/unsubscribe",
            IvyJson::object([("threadId", id.clone().into())]),
            Default::default(),
        )
        .await?;
        Ok(())
    }
}
fn bounded_history(items: &[IvyJson], budget: usize, limit: usize) -> IvyResult<Vec<IvyJson>> {
    let mut kept = Vec::new();
    let mut bytes = 0;
    for item in items.iter().rev() {
        let size = text(item, "text")?.to_utf8()?.len();
        if bytes + size > budget || kept.len() >= limit {
            break;
        }
        kept.push(item.clone());
        bytes += size;
    }
    kept.reverse();
    Ok(kept)
}
fn party_key(call: &IvyJson) -> IvyResult<String> {
    let uri = call
        .get("incoming")
        .and_then(|v| v.get("fromUri"))
        .filter(|v| !v.is_null())
        .or_else(|| call.get("destination"))
        .ok_or_else(IvyError::internal)?;
    let uri = uri.as_js_string().ok_or_else(IvyError::internal)?;
    Ok(digest(
        crate::admission::phone_caller_identity(uri).to_utf8()?,
    ))
}
