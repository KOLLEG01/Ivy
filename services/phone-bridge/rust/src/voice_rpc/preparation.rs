//! Original prepared-task turn recovery and readiness over the owned control RPC.
use super::{VoiceControl, VoiceControlOperation};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, hash::digest};
use ivy_host_runtime::codex_rpc::{NativeNotification, RequestOptions};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::watch, task::AbortHandle};

const TASK_KEY: &str = "prepared-phone-voice";
const PROMPT: &str = concat!(
    "Prepare for a PhoneBridge call; no caller or assignment exists yet. ",
    "During preparation, use only read-only tools or technical checks without external effects. ",
    "Do not call, message, change tickets/settings/voices, or restart tasks. ",
    "Use supported discovery (including code-mode tools if needed) to find the regular Ivy MCP catalog, ",
    "then successfully call ivy_instructions and phone_bridge_status through it. ",
    "Instructions, an installed connector, ivy_dev, or direct HTTP alone do not prove readiness. ",
    "If discovery or either call fails or is unavailable, finish with NOT_READY: <brief reason>; otherwise finish with only READY. ",
    "On later explicit user requests, PhoneBridge tools can select conversation settings or restart the Voice task with its original greeting; no separate greeting tool exists. ",
    "Preparation limits end when the actual call context arrives; then follow authorized conversation requests under normal rules."
);

/// Required original Phone journal boundary. Implementations must validate cache
/// ownership and acknowledge each write only after its journal transaction.
pub type VoiceJournalFuture<'a, T> = Pin<Box<dyn Future<Output = IvyResult<T>> + Send + 'a>>;
pub trait VoicePreparationJournal: Send + Sync {
    fn task_cache<'a>(
        &'a self,
        principal_id: &'a str,
        fingerprint: &'a str,
    ) -> VoiceJournalFuture<'a, Option<IvyJson>>;
    fn retain_task<'a>(
        &'a self,
        principal_id: &'a str,
        cache: IvyJson,
    ) -> VoiceJournalFuture<'a, ()>;
}

struct Pending {
    turn_id: Option<JsString>,
    completed: Option<IvyJson>,
    result: watch::Sender<Option<IvyResult<()>>>,
    timer: AbortHandle,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.timer.abort();
    }
}
#[derive(Default)]
struct State {
    operations: BTreeMap<JsString, VoiceControlOperation<()>>,
    pending: BTreeMap<JsString, Pending>,
}
struct Inner {
    control: VoiceControl,
    journal: Arc<dyn VoicePreparationJournal>,
    state: Mutex<State>,
}

/// The enclosing Voice owner forwards native notifications and connection loss
/// synchronously here. These callbacks never await another command on that RPC.
#[derive(Clone)]
pub struct VoicePreparation(Arc<Inner>);
impl VoicePreparation {
    pub fn new(control: VoiceControl, journal: Arc<dyn VoicePreparationJournal>) -> Self {
        Self(Arc::new(Inner {
            control,
            journal,
            state: Mutex::new(State::default()),
        }))
    }
    /// Observers join the same original work. Losing an observer cannot abandon
    /// a retained native preparation request or its journal acknowledgement.
    pub fn prepare(&self, thread_id: JsString) -> VoiceControlOperation<()> {
        self.prepare_inner(thread_id, true)
    }
    /// The enclosing owner has already admitted its connection. Close drains
    /// that preparation before closing native ownership, without reopening it.
    pub(super) fn prepare_opened(&self, thread_id: JsString) -> VoiceControlOperation<()> {
        self.prepare_inner(thread_id, false)
    }
    fn prepare_inner(&self, thread_id: JsString, open: bool) -> VoiceControlOperation<()> {
        let (result, receiver) = watch::channel(None);
        let operation = VoiceControlOperation::from_receiver(receiver);
        {
            let mut state = self.0.state.lock().unwrap();
            if let Some(original) = state.operations.get(&thread_id) {
                return original.clone();
            }
            state
                .operations
                .insert(thread_id.clone(), operation.clone());
        }
        let owner = self.0.clone();
        tokio::spawn(async move {
            let work = owner.clone();
            let thread = thread_id.clone();
            let outcome = tokio::spawn(async move {
                if open {
                    work.control.open().wait().await?;
                }
                work.run(&thread).await
            })
            .await
            .unwrap_or_else(|_| Err(IvyError::internal()));
            {
                let mut state = owner.state.lock().unwrap();
                state.pending.remove(&thread_id);
                state.operations.remove(&thread_id);
            }
            result.send_replace(Some(outcome));
        });
        operation
    }
    pub fn notification(&self, notification: &NativeNotification) {
        if notification.method != JsString::from("turn/completed") {
            return;
        }
        let Some(thread_id) = notification
            .params
            .get("threadId")
            .and_then(IvyJson::as_js_string)
        else {
            return;
        };
        let Some(turn) = notification
            .params
            .get("turn")
            .filter(|turn| turn.get("id").and_then(IvyJson::as_js_string).is_some())
        else {
            return;
        };
        let mut state = self.0.state.lock().unwrap();
        if let Some(pending) = state.pending.get_mut(thread_id) {
            pending.completed = Some(turn.clone());
            complete(pending);
        }
    }
    pub fn connection_lost(&self, code: &str) {
        let pending = std::mem::take(&mut self.0.state.lock().unwrap().pending);
        for (_, pending) in pending {
            settle(
                &pending.result,
                Err(IvyError::new(
                    code,
                    "Voice task preparation lost its control connection.",
                )),
            );
        }
    }
}

fn settle(sender: &watch::Sender<Option<IvyResult<()>>>, result: IvyResult<()>) {
    sender.send_if_modified(|current| {
        if current.is_some() {
            false
        } else {
            *current = Some(result);
            true
        }
    });
}
fn complete(pending: &Pending) {
    let Some(turn) = &pending.completed else {
        return;
    };
    if turn.get("id").and_then(IvyJson::as_js_string) != pending.turn_id.as_ref()
        || turn.get("status") == Some(&"inProgress".into())
    {
        return;
    }
    pending.timer.abort();
    settle(
        &pending.result,
        if turn.get("status") == Some(&"completed".into()) {
            Ok(())
        } else {
            Err(IvyError::new(
                "phone_voice_prepare_failed",
                "The original Voice preparation turn did not complete.",
            ))
        },
    );
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
pub(super) fn thread<'a>(value: &'a IvyJson, control: &VoiceControl) -> IvyResult<&'a IvyJson> {
    let settings = control.native_settings();
    let thread = value.get("thread");
    let no_project = IvyJson::Null;
    let project = settings
        .get("projectId")
        .filter(|value| !value.is_null())
        .unwrap_or(&no_project);
    require(
        thread.is_some_and(|thread| {
            thread.get("id").and_then(IvyJson::as_js_string).is_some()
                && thread.get("projectId") == Some(project)
                && thread
                    .get("ephemeral")
                    .is_none_or(|value| value == &false.into())
        }),
        "phone_voice_task_changed",
        "Prepared Voice task must retain its configured home, project and directory.",
    )?;
    let thread = thread.unwrap();
    let cwd = thread
        .get("cwd")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)?;
    let expected = settings
        .get("cwd")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)?;
    require(
        control
            .paths()
            .resolve(std::slice::from_ref(cwd))
            .to_utf8()?
            .to_lowercase()
            == control
                .paths()
                .resolve(std::slice::from_ref(expected))
                .to_utf8()?
                .to_lowercase(),
        "phone_voice_task_changed",
        "Prepared Voice task must retain its configured home, project and directory.",
    )?;
    Ok(thread)
}
impl Inner {
    async fn call(
        &self,
        method: &str,
        params: IvyJson,
        hooks: RequestOptions,
    ) -> IvyResult<IvyJson> {
        let rpc = self.control.current().ok_or_else(|| {
            IvyError::new(
                "phone_voice_unavailable",
                "Codex Voice transport is unavailable.",
            )
        })?;
        rpc.call(method, params, hooks, None).await
    }
    async fn read_turn(
        &self,
        thread_id: &JsString,
        cache: &Arc<Mutex<IvyJson>>,
    ) -> IvyResult<Option<IvyJson>> {
        let value = self
            .call(
                "thread/read",
                IvyJson::object([
                    ("threadId", IvyJson::String(thread_id.clone())),
                    ("includeTurns", true.into()),
                ]),
                RequestOptions::default(),
            )
            .await?;
        let thread = thread(&value, &self.control)?;
        let Some(turns) = thread.get("turns").filter(|value| !value.is_null()) else {
            return Ok(None);
        };
        let turns = turns.as_array().ok_or_else(IvyError::internal)?;
        let cache = cache.lock().unwrap();
        for turn in turns {
            if cache
                .get("preparationTurnId")
                .and_then(IvyJson::as_js_string)
                .is_some()
            {
                if turn.get("id") == cache.get("preparationTurnId") {
                    return Ok(Some(turn.clone()));
                }
            } else {
                let items = turn
                    .get("items")
                    .and_then(IvyJson::as_array)
                    .ok_or_else(IvyError::internal)?;
                if items.iter().any(|item| {
                    item.get("type") == Some(&"userMessage".into())
                        && item.get("clientId") == cache.get("preparationOperationId")
                }) {
                    return Ok(Some(turn.clone()));
                }
            }
        }
        Ok(None)
    }
    async fn run(&self, thread_id: &JsString) -> IvyResult<()> {
        let fingerprint = self.control.fingerprint().ok_or_else(IvyError::internal)?;
        let cache = self.journal.task_cache(TASK_KEY, &fingerprint).await?;
        require(
            cache.as_ref().and_then(|value| value.get("threadId"))
                == Some(&IvyJson::String(thread_id.clone())),
            "phone_voice_task_changed",
            "Voice preparation no longer owns its cached task.",
        )?;
        let cache = Arc::new(Mutex::new(cache.unwrap()));
        let recovering = cache
            .lock()
            .unwrap()
            .get("preparationOperationId")
            .and_then(IvyJson::as_js_string)
            .is_some();
        let recovered = if recovering {
            self.read_turn(thread_id, &cache).await?
        } else {
            None
        };
        if recovering {
            require(
                recovered.is_some(),
                "phone_voice_prepare_unknown",
                "The original Voice preparation requires reconciliation; it cannot be repeated.",
            )?;
            let recovered = recovered.as_ref().unwrap();
            if recovered.get("status") == Some(&"completed".into()) {
                if cache.lock().unwrap().get("preparationPromptHash")
                    == Some(&IvyJson::from(digest(PROMPT)))
                {
                    return validate_preparation(Some(recovered));
                }
                *cache.lock().unwrap() = IvyJson::object([
                    ("fingerprint", fingerprint.into()),
                    ("threadId", IvyJson::String(thread_id.clone())),
                    ("creationOperationId", IvyJson::Null),
                ]);
            } else {
                require(
                    recovered.get("status") == Some(&"inProgress".into()),
                    "phone_voice_prepare_failed",
                    "The original Voice preparation turn did not complete.",
                )?;
            }
        }
        let continuing = recovered
            .as_ref()
            .is_some_and(|turn| turn.get("status") == Some(&"inProgress".into()));
        let (finished, receiver) = watch::channel(None);
        let timer_result = finished.clone();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(120_000);
        let timer = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            settle(
                &timer_result,
                Err(IvyError::new(
                    "phone_voice_prepare_unknown",
                    "Voice task preparation remains unconfirmed.",
                )
                .with_outcome(Outcome::Unknown)),
            );
        });
        self.state.lock().unwrap().pending.insert(
            thread_id.clone(),
            Pending {
                turn_id: if continuing {
                    recovered
                        .as_ref()
                        .and_then(|turn| turn.get("id"))
                        .and_then(IvyJson::as_js_string)
                        .cloned()
                } else {
                    None
                },
                completed: None,
                result: finished,
                timer: timer.abort_handle(),
            },
        );
        if continuing {
            let current = self.read_turn(thread_id, &cache).await?;
            if current
                .as_ref()
                .is_some_and(|turn| turn.get("status") != Some(&"inProgress".into()))
            {
                if let Some(pending) = self.state.lock().unwrap().pending.get_mut(thread_id) {
                    pending.completed = current;
                }
            }
        } else {
            let operation_id: JsString = uuid::Uuid::new_v4().to_string().into();
            let mut updated = cache.lock().unwrap().clone();
            updated.insert(
                "preparationOperationId",
                IvyJson::String(operation_id.clone()),
            );
            updated.insert("preparationPromptHash", digest(PROMPT).into());
            *cache.lock().unwrap() = updated.clone();
            self.journal.retain_task(TASK_KEY, updated).await?;
            let acknowledged_cache = cache.clone();
            let journal = self.journal.clone();
            let value = self.call("turn/start", IvyJson::object([
                ("threadId", IvyJson::String(thread_id.clone())),
                ("clientUserMessageId", IvyJson::String(operation_id.clone())),
                ("input", IvyJson::Array(vec![IvyJson::object([("type", "text".into()), ("text", PROMPT.into()), ("text_elements", IvyJson::Array(vec![]))])])),
            ]), RequestOptions {
                request_id: Some(operation_id),
                before_resolve: Some(Arc::new(move |_, reply| {
                    let cache = acknowledged_cache.clone(); let journal = journal.clone();
                    Box::pin(async move {
                        if let Some(result) = reply.get("result") {
                            let turn = result.get("turn");
                            let id = turn.and_then(|turn| turn.get("id")).and_then(IvyJson::as_js_string);
                            require(id.is_some(), "phone_voice_prepare_unknown", "Native Voice preparation did not acknowledge its original turn.")?;
                            let mut updated = cache.lock().unwrap().clone();
                            updated.insert("preparationTurnId", IvyJson::String(id.unwrap().clone()));
                            *cache.lock().unwrap() = updated.clone();
                            journal.retain_task(TASK_KEY, updated).await?;
                        }
                        Ok(())
                    })
                })),
                ..Default::default()
            }).await?;
            let turn = value.get("turn").ok_or_else(IvyError::internal)?;
            if let Some(pending) = self.state.lock().unwrap().pending.get_mut(thread_id) {
                pending.turn_id = turn.get("id").and_then(IvyJson::as_js_string).cloned();
                if turn.get("status") != Some(&"inProgress".into()) {
                    pending.completed = Some(turn.clone());
                }
            }
        }
        if let Some(pending) = self.state.lock().unwrap().pending.get(thread_id) {
            complete(pending);
        }
        VoiceControlOperation::from_receiver(receiver)
            .wait()
            .await?;
        validate_preparation(self.read_turn(thread_id, &cache).await?.as_ref())
    }
}

fn validate_preparation(turn: Option<&IvyJson>) -> IvyResult<()> {
    let items = match turn.and_then(|turn| turn.get("items")) {
        None | Some(IvyJson::Null) => &[][..],
        Some(value) => value.as_array().ok_or_else(IvyError::internal)?,
    };
    if items.iter().any(IvyJson::is_null) {
        return Err(IvyError::internal());
    }
    let final_item = items.iter().rev().find(|item| {
        item.get("type") == Some(&"agentMessage".into())
            && item
                .get("phase")
                .is_none_or(|phase| phase.is_null() || *phase == IvyJson::from("final_answer"))
    });
    let completed = turn.is_some_and(|turn| turn.get("status") == Some(&"completed".into()));
    let text = if completed {
        final_item
            .and_then(|item| item.get("text"))
            .and_then(IvyJson::as_js_string)
            .map(JsString::to_utf8)
            .transpose()?
    } else {
        None
    };
    let ready = text.is_some_and(|text| text.trim() == "READY");
    let calls: Vec<_> = items
        .iter()
        .filter(|item| {
            item.get("type") == Some(&"mcpToolCall".into())
                && item.get("status") == Some(&"completed".into())
                && item.get("error").is_none_or(IvyJson::is_null)
                && item.get("result").is_some_and(|result| {
                    result.as_object().is_some()
                        && result
                            .get("isError")
                            .is_none_or(|value| value == &false.into())
                })
        })
        .collect();
    let instructions = calls
        .iter()
        .find(|item| item.get("tool") == Some(&"ivy_instructions".into()));
    require(
        turn.is_some_and(|turn| turn.get("status") == Some(&"completed".into()))
            && ready
            && instructions.is_some_and(|instructions| {
                instructions.get("server") != Some(&"ivy_dev".into())
                    && calls.iter().any(|call| {
                        call.get("server") == instructions.get("server")
                            && call.get("tool") == Some(&"phone_bridge_status".into())
                    })
            }),
        "phone_voice_not_ready",
        "Voice preparation did not confirm READY with successful regular Ivy MCP checks.",
    )
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_voice_preparation.rs"]
mod tests;
