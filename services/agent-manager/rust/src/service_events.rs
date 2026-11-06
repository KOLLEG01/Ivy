//! Wire-order observations, including native clock inputs arriving during launch.
use crate::{
    browser_notices::{
        BrowserNotices, FormQuery, Notice, input_browser_notice, native_browser_notice,
    },
    inventory_manager::InventoryManager,
    local_state::LocalAgentState,
    native_clock::answer_native_clock,
    native_dispatch::{Observations, journal_events},
    native_journal::{JournalClock, NativeJournal},
    native_operations::{NativeOperations, NativeRun, ReceiptWork},
    notice_policy::NativeNoticePolicy,
    notifications::NativeNotifications,
    service_link::ServiceLink,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::codex_rpc::{Events, NativeNotification, NativeRpc};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;

#[derive(Default)]
struct State {
    rpc: Option<NativeRpc>,
    clocks: Vec<IvyJson>,
    inventory: Option<InventoryManager>,
    notices: Option<BrowserNotices>,
}
pub struct ServiceEventSettings {
    pub service_node_id: JsString,
    pub service: ServiceLink,
    pub closing: Arc<AtomicBool>,
    pub operations: NativeOperations,
    pub state: Arc<Mutex<LocalAgentState>>,
    pub receipts: ReceiptWork,
    pub notifications: Arc<Mutex<NativeNotifications>>,
    pub policy: Arc<Mutex<NativeNoticePolicy>>,
    pub query: Arc<FormQuery>,
}
struct Inner {
    settings: ServiceEventSettings,
    state: Mutex<State>,
    clock: Arc<JournalClock>,
    activity: watch::Sender<()>,
}
#[derive(Clone)]
pub struct ServiceEvents(Arc<Inner>);
impl ServiceEvents {
    pub fn new(settings: ServiceEventSettings) -> Self {
        Self::with_clock(
            settings,
            Arc::new(|| {
                chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into()
            }),
        )
    }
    pub fn with_clock(settings: ServiceEventSettings, clock: Arc<JournalClock>) -> Self {
        Self(Arc::new(Inner {
            settings,
            state: Mutex::new(State::default()),
            clock,
            activity: watch::channel(()).0,
        }))
    }
    pub fn begin_epoch(&self, epoch: JsString) -> IvyResult<Events> {
        let settings = &self.0.settings;
        settings
            .operations
            .journal()
            .lock()
            .map_err(|_| IvyError::internal())?
            .begin_epoch(&epoch)?;
        let input = self.clone();
        let notifications = self.clone();
        let close = self.clone();
        Ok(journal_events(
            epoch.clone(),
            &settings.operations,
            Observations {
                on_input: Arc::new(move |value| {
                    let result = input.input(value);
                    Box::pin(async move { result })
                }),
                on_notification: Arc::new(move |value| {
                    let result = notifications.notification(&epoch, value);
                    Box::pin(async move {
                        if let Some(run) = result? {
                            run.wait().await?;
                        }
                        Ok(())
                    })
                }),
                on_close: Arc::new(move |_| {
                    let result =
                        close
                            .0
                            .state
                            .lock()
                            .map_err(|_| IvyError::internal())
                            .map(|mut state| {
                                state.rpc = None;
                            });
                    Box::pin(async move { result })
                }),
            },
        ))
    }
    pub fn attach_native(&self, rpc: NativeRpc) -> IvyResult<()> {
        let clocks = {
            let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            state.rpc = Some(rpc.clone());
            std::mem::take(&mut state.clocks)
        };
        for input in clocks {
            self.answer_clock(&rpc, &input);
        }
        Ok(())
    }
    pub fn install(&self, inventory: InventoryManager, notices: BrowserNotices) -> IvyResult<()> {
        let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
        state.inventory = Some(inventory);
        state.notices = Some(notices);
        Ok(())
    }
    /// Called after invocation admission has closed and accepted RPCs settled.
    /// Events invalidate the metadata scan, including activity on child threads.
    pub async fn wait_idle(&self) -> IvyResult<()> {
        let mut activity = self.0.activity.subscribe();
        let (rpc, inventory) = {
            let state = self.0.state.lock().map_err(|_| IvyError::internal())?;
            (state.rpc.clone(), state.inventory.clone())
        };
        let (Some(rpc), Some(inventory)) = (rpc, inventory) else {
            // Startup can stop before handlers and inventory are installed.
            return Ok(());
        };
        loop {
            activity.borrow_and_update();
            let result = tokio::select! {
                _ = rpc.closed() => return Ok(()),
                result = inventory.has_active_threads() => result,
            };
            if activity.has_changed().map_err(|_| IvyError::internal())? {
                continue;
            }
            if matches!(result, Ok(false)) {
                return Ok(());
            }
            // A failed read proves no idle boundary. Retry only the read; never
            // resend a turn or a tool action. Normal active work waits on events.
            tokio::select! {
                _ = rpc.closed() => return Ok(()),
                _ = activity.changed() => {},
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)), if result.is_err() => {},
            }
        }
    }
    fn journal(&self) -> Arc<Mutex<NativeJournal>> {
        self.0.settings.operations.journal()
    }
    fn answer_clock(&self, rpc: &NativeRpc, input: &IvyJson) {
        // The clock owner starts eagerly; dropping this observation does not
        // cancel dispatch, and no callback waits on its own native RPC actor.
        drop(answer_native_clock(self.journal(), rpc, input));
    }
    fn send(&self, create: impl FnOnce() -> IvyResult<Option<Notice>>) -> IvyResult<bool> {
        let notices = self
            .0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .notices
            .clone();
        if let Some(notices) = notices {
            // Notices are detached in the original service. Including them in
            // invocation drain would change the shutdown wait boundary.
            return notices.send(create);
        }
        Ok(false)
    }
    fn suppress_completion(
        &self,
        event: &IvyJson,
        epoch: &JsString,
    ) -> IvyResult<Option<NativeRun>> {
        if event.get("method") != Some(&"turn/completed".into()) {
            return Ok(None);
        }
        fn text(value: &IvyJson) -> Option<&JsString> {
            value.as_js_string().filter(|v| !v.0.is_empty())
        }
        let Some(thread) = event
            .get("params")
            .and_then(|v| v.get("threadId"))
            .and_then(text)
        else {
            return Ok(None);
        };
        let Some(turn) = event
            .get("params")
            .and_then(|v| v.get("turn"))
            .and_then(|v| v.get("id"))
            .and_then(text)
        else {
            return Ok(None);
        };
        // SQLite's existing string boundary is UTF-8. Oversized evidence never
        // truncates an identity or releases an unproven original receipt.
        if thread.0.len() > 8192 || turn.0.len() > 8192 {
            return Ok(None);
        }
        let (thread, turn, epoch) = (
            String::from_utf16_lossy(&thread.0),
            String::from_utf16_lossy(&turn.0),
            String::from_utf16_lossy(&epoch.0),
        );
        let state = self.0.settings.state.clone();
        self.0
            .settings
            .receipts
            .track(async move {
                tokio::task::spawn_blocking(move || {
                    state
                        .lock()
                        .map_err(|_| IvyError::internal())?
                        .suppress_completion(&thread, &turn, &epoch)
                })
                .await
                .map_err(|_| IvyError::internal())?
            })
            .map(Some)
    }
    fn input(&self, input: IvyJson) -> IvyResult<()> {
        let settings = &self.0.settings;
        if input.get("method") == Some(&"currentTime/read".into()) {
            let rpc = {
                let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
                if state.rpc.is_none() {
                    state.clocks.push(input.clone());
                }
                state.rpc.clone()
            };
            if let Some(rpc) = rpc {
                self.answer_clock(&rpc, &input);
            }
        } else if settings.service.ready() && !settings.closing.load(Ordering::Acquire) {
            self.send(|| {
                let Some(payload) = input_browser_notice(&input, settings.query.as_ref())? else {
                    return Ok(None);
                };
                Ok(Some(Notice {
                    thread: input
                        .required("threadId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?
                        .clone(),
                    payload,
                    turn: None,
                }))
            })?;
            let _ = settings.service.notify(
                "inputs",
                IvyJson::object([
                    ("identity", input.required("identity")?.clone()),
                    ("threadId", input.required("threadId")?.clone()),
                ]),
            );
        }
        Ok(())
    }
    fn notification(
        &self,
        epoch: &JsString,
        value: NativeNotification,
    ) -> IvyResult<Option<NativeRun>> {
        let settings = &self.0.settings;
        let inventory_thread = value
            .params
            .get("threadId")
            .and_then(IvyJson::as_js_string)
            .or_else(|| {
                value
                    .params
                    .get("thread")
                    .and_then(|thread| thread.get("id"))
                    .and_then(IvyJson::as_js_string)
            })
            .cloned();
        if self.0.activity.receiver_count() != 0
            && [
                "thread/started",
                "thread/closed",
                "thread/status/changed",
                "turn/started",
                "turn/completed",
            ]
            .iter()
            .any(|method| value.method.eq_str(method))
        {
            self.0.activity.send_replace(());
        }
        settings
            .policy
            .lock()
            .map_err(|_| IvyError::internal())?
            .observe(&value.method, &value.params);
        let activity_thread = ["thread/status/changed", "turn/started", "turn/completed"]
            .iter()
            .any(|method| value.method.eq_str(method))
            .then(|| {
                value
                    .params
                    .get("threadId")
                    .and_then(IvyJson::as_js_string)
                    .cloned()
            })
            .flatten();
        let observed_at = (self.0.clock)();
        let event = settings
            .notifications
            .lock()
            .map_err(|_| IvyError::internal())?
            .observe_js(epoch, &value.method, value.params, &observed_at)?;
        let ready = settings.service.ready() && !settings.closing.load(Ordering::Acquire);
        let sent = self.send(|| {
            let Some(payload) =
                native_browser_notice(&settings.service_node_id, &event, settings.query.as_ref())?
            else {
                return Ok(None);
            };
            let params = event.get("params").unwrap();
            Ok(Some(Notice {
                thread: params
                    .required("threadId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?
                    .clone(),
                payload,
                turn: Some(
                    params
                        .required("turn")?
                        .required("id")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?
                        .clone(),
                ),
            }))
        })?;
        let cleanup = if sent {
            None
        } else {
            self.suppress_completion(&event, epoch)?
        };
        if ready {
            let _ = settings.service.notify("notification", event);
        }
        let inventory = self
            .0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .inventory
            .clone();
        if let Some(inventory) = inventory {
            let method = &value.method;
            let refresh = [
                "thread/started",
                "thread/name/updated",
                "thread/project/updated",
                "thread/archived",
                "thread/unarchived",
                "thread/deleted",
                "thread/status/changed",
                "turn/started",
                "turn/completed",
            ]
            .iter()
            .any(|v| method.eq_str(v));
            let internal_activity = activity_thread
                .as_ref()
                .map(|thread| {
                    settings
                        .policy
                        .lock()
                        .map_err(|_| IvyError::internal())?
                        .is_internal(thread)
                })
                .transpose()?
                .flatten()
                == Some(true);
            if refresh && !internal_activity {
                inventory.observe_thread(ready, method, inventory_thread, &observed_at)?;
            }
            if method.eq_str("project/changed") {
                inventory.request_projects(ready)?;
                inventory.request_threads(ready)?;
            }
        }
        Ok(cleanup)
    }
}
