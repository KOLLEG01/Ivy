//! Wire-order observations, including native clock inputs arriving during launch.
use crate::{
    browser_notices::{BrowserNotices, FormQuery, input_browser_notice, native_browser_notice},
    inventory_manager::InventoryManager,
    native_clock::answer_native_clock,
    native_dispatch::{Observations, journal_events},
    native_journal::{JournalClock, NativeJournal},
    native_operations::NativeOperations,
    notice_policy::NativeNoticePolicy,
    notifications::NativeNotifications,
    service_link::ServiceLink,
    service_work::AgentWork,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::codex_rpc::{Events, NativeNotification, NativeRpc};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

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
    pub notifications: Arc<Mutex<NativeNotifications>>,
    pub policy: Arc<Mutex<NativeNoticePolicy>>,
    pub query: Arc<FormQuery>,
    pub work: AgentWork,
}
struct Inner {
    settings: ServiceEventSettings,
    state: Mutex<State>,
    clock: Arc<JournalClock>,
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
            settings.service_node_id.clone(),
            epoch.clone(),
            &settings.operations,
            Observations {
                on_input: Arc::new(move |value| {
                    let result = input.input(value);
                    Box::pin(async move { result })
                }),
                on_notification: Arc::new(move |value| {
                    let result = notifications.notification(&epoch, value);
                    Box::pin(async move { result })
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
    fn journal(&self) -> Arc<Mutex<NativeJournal>> {
        self.0.settings.operations.journal()
    }
    fn answer_clock(&self, rpc: &NativeRpc, input: &IvyJson) {
        // The clock owner starts eagerly; dropping this observation does not
        // cancel dispatch, and no callback waits on its own native RPC actor.
        drop(answer_native_clock(self.journal(), rpc, input));
    }
    fn send(&self, thread: JsString, notice: IvyJson, turn: Option<JsString>) -> IvyResult<()> {
        let notices = self
            .0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .notices
            .clone();
        if let Some(notices) = notices {
            let _ = self.0.settings.work.track(Box::pin(async move {
                notices.send(thread, notice, turn).await?;
                Ok(IvyJson::Null)
            }))?;
        }
        Ok(())
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
            if let Some(notice) = input_browser_notice(&input, settings.query.as_ref())? {
                self.send(
                    input
                        .required("threadId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?
                        .clone(),
                    notice,
                    None,
                )?;
            }
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
    fn notification(&self, epoch: &JsString, value: NativeNotification) -> IvyResult<()> {
        let settings = &self.0.settings;
        settings
            .policy
            .lock()
            .map_err(|_| IvyError::internal())?
            .observe(&IvyJson::object([
                ("method", IvyJson::String(value.method.clone())),
                ("params", value.params.clone()),
            ]));
        let event = settings
            .notifications
            .lock()
            .map_err(|_| IvyError::internal())?
            .observe_js(
                epoch,
                &value.method,
                value.params.clone(),
                &(self.0.clock)(),
            )?;
        let ready = settings.service.ready() && !settings.closing.load(Ordering::Acquire);
        if ready {
            if let Some(notice) =
                native_browser_notice(&settings.service_node_id, &event, settings.query.as_ref())?
            {
                self.send(
                    value
                        .params
                        .required("threadId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?
                        .clone(),
                    notice,
                    Some(
                        value
                            .params
                            .required("turn")?
                            .required("id")?
                            .as_js_string()
                            .ok_or_else(IvyError::internal)?
                            .clone(),
                    ),
                )?;
            }
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
            .any(|v| method == &JsString::from(*v));
            let internal_activity = ["thread/status/changed", "turn/started", "turn/completed"]
                .iter()
                .any(|v| method == &JsString::from(*v))
                && value
                    .params
                    .get("threadId")
                    .and_then(IvyJson::as_js_string)
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
                inventory.request_threads(ready)?;
            }
            if method == &JsString::from("project/changed") {
                inventory.request_projects(ready)?;
                inventory.request_threads(ready)?;
            }
        }
        Ok(())
    }
}
