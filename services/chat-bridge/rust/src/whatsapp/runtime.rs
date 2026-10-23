//! WhatsApp task lanes and lifetime. Protocol/media and Host resources retain
//! their owners; this controller never changes an admitted operation identity.
pub use super::runtime_attachment::{
    BoolFuture, RuntimeAssembly, RuntimeAttached, RuntimeAttachmentFactory,
};
use super::{
    commands::parse_command,
    journal::{HistoryLimits, Inbox},
    journal_owner::Journal,
    messages::Translator,
    sender::phone,
};
use crate::{generation::ChatGeneration, main_context::object_options};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, json::parse};
use ivy_sdk::{
    clock::{Clock, SystemClock, date_to_iso},
    rpc::RequestFuture,
    service::ServiceFuture,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;

pub trait RuntimeMedia: Send + Sync {
    fn command_text(&self, row: &Inbox) -> IvyResult<JsString>;
    fn content(&self, row: &Inbox) -> IvyResult<Json>;
    fn payload<'a>(
        &'a self,
        row: &'a Inbox,
        lease: &'a crate::generation::ChatGenerationLease,
        same_host: bool,
    ) -> RequestFuture<'a>;
    fn prune(&self) -> ServiceFuture<()>;
}
pub trait RuntimeSocket: Send + Sync {
    fn bind(&self, callbacks: RuntimeCallbacks);
    fn start(&self);
    fn state(&self) -> JsString;
    fn status(&self) -> Json;
    fn flush(&self) -> ServiceFuture<()>;
    fn read_receipts(&self) -> ServiceFuture<()>;
    fn set_busy(&self, jids: Vec<JsString>);
    fn presence(&self) -> ServiceFuture<()>;
    fn close(&self) -> ServiceFuture<()>;
}
pub struct RuntimeCallbacks {
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub automatic_notifications: Arc<dyn Fn() -> BoolFuture<'static> + Send + Sync>,
}
pub struct RuntimeResources {
    pub status: Arc<dyn Fn(Json) -> ServiceFuture<()> + Send + Sync>,
    pub retire_account: Arc<dyn Fn() -> ServiceFuture<()> + Send + Sync>,
}
pub trait RuntimeTimer: Send + Sync {
    fn clear(&self);
}
pub trait RuntimeTimers: Send + Sync {
    fn every(
        &self,
        name: &'static str,
        millis: u64,
        tick: Arc<dyn Fn() + Send + Sync>,
    ) -> Box<dyn RuntimeTimer>;
}
pub struct TokioTimers;
struct TokioTimer(tokio::task::AbortHandle);
impl RuntimeTimer for TokioTimer {
    fn clear(&self) {
        self.0.abort();
    }
}
impl RuntimeTimers for TokioTimers {
    fn every(
        &self,
        _name: &'static str,
        millis: u64,
        tick: Arc<dyn Fn() + Send + Sync>,
    ) -> Box<dyn RuntimeTimer> {
        let task = tokio::spawn(async move {
            let delay = Duration::from_millis(millis);
            let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + delay, delay);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                tick();
            }
        });
        Box::new(TokioTimer(task.abort_handle()))
    }
}
#[derive(Clone, Copy)]
pub enum RuntimeWork {
    Control,
    Input,
    Listener,
    Output,
    Status,
    ReadReceipts,
    Activity,
    Retention,
    NativeEvents,
    InvalidateListen,
}
impl RuntimeWork {
    fn name(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Input => "input",
            Self::Listener | Self::InvalidateListen => "listener",
            Self::Output => "output",
            Self::Status => "status",
            Self::ReadReceipts => "read-receipts",
            Self::Activity => "activity",
            Self::Retention => "retention",
            Self::NativeEvents => "native-events",
        }
    }
}
#[derive(Clone)]
enum Job {
    Work(RuntimeWork),
    PrepareListen(Arc<dyn RuntimeAttached>),
    Outbox(Arc<dyn RuntimeAttached>),
}
impl Job {
    fn name(&self) -> &'static str {
        match self {
            Self::Work(w) => w.name(),
            Self::PrepareListen(_) => "listener",
            Self::Outbox(_) => "chat-outbox",
        }
    }
}
type Completion = watch::Receiver<Option<IvyResult<()>>>;
struct State {
    attached: Option<Arc<dyn RuntimeAttached>>,
    tasks: HashMap<&'static str, Completion>,
    again: HashMap<&'static str, Job>,
    errors: VecDeque<(&'static str, JsString)>,
    closed: bool,
    timers: Vec<Box<dyn RuntimeTimer>>,
    native_busy: bool,
    native_busy_at: i64,
    event_revision: u64,
    events: VecDeque<Json>,
    last_sequence: f64,
    last_epoch: Option<JsString>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            attached: None,
            tasks: HashMap::new(),
            again: HashMap::new(),
            errors: VecDeque::new(),
            closed: false,
            timers: vec![],
            native_busy: false,
            native_busy_at: 0,
            event_revision: 0,
            events: VecDeque::new(),
            last_sequence: 0.0,
            last_epoch: None,
        }
    }
}
struct Inner {
    config: Json,
    host: JsString,
    journal: Journal,
    socket: Arc<dyn RuntimeSocket>,
    media: Arc<dyn RuntimeMedia>,
    factory: Arc<dyn RuntimeAttachmentFactory>,
    resources: RuntimeResources,
    translator: Translator,
    clock: Arc<dyn Clock>,
    timers: Arc<dyn RuntimeTimers>,
    state: Mutex<State>,
    closing: Mutex<Option<Completion>>,
}
#[derive(Clone)]
pub struct WhatsAppRuntime(Arc<Inner>);
pub struct RuntimeParts {
    pub config: Json,
    pub host_id: JsString,
    pub journal: Journal,
    pub socket: Arc<dyn RuntimeSocket>,
    pub media: Arc<dyn RuntimeMedia>,
    pub factory: Arc<dyn RuntimeAttachmentFactory>,
    pub resources: RuntimeResources,
    pub translator: Translator,
}
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn string(value: Option<&Json>) -> IvyResult<JsString> {
    value
        .ok_or_else(IvyError::internal)?
        .as_str()
        .map_err(|_| IvyError::internal())
        .map(Into::into)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn object(value: &Json) -> bool {
    value.as_object().is_some()
}
fn native_changed(code: &str) -> bool {
    [
        "tool_definition_changed",
        "native_definition_mismatch",
        "native_owner_mismatch",
    ]
    .contains(&code)
}
fn same(current: &Option<Arc<dyn RuntimeAttached>>, selected: &Arc<dyn RuntimeAttached>) -> bool {
    current.as_ref().is_some_and(|a| Arc::ptr_eq(a, selected))
}
pub fn retryable_whatsapp_error(code: &str) -> bool {
    [
        "outcome_unknown",
        "service_not_ready",
        "service_unavailable",
        "storage_unavailable",
        "deadline_exceeded",
        "native_unavailable",
        "native_capacity",
        "stale_generation",
        "chat_pending_action",
        "revision_conflict",
        "native_read_pending",
        "not_found",
    ]
    .contains(&code)
}
async fn completed(mut receive: Completion) -> IvyResult<()> {
    loop {
        if let Some(value) = receive.borrow_and_update().clone() {
            return value;
        }
        receive.changed().await.map_err(|_| IvyError::internal())?;
    }
}
impl WhatsAppRuntime {
    pub fn new(parts: RuntimeParts) -> Self {
        Self::with_dependencies(parts, Arc::new(SystemClock), Arc::new(TokioTimers))
    }
    pub fn with_dependencies(
        parts: RuntimeParts,
        clock: Arc<dyn Clock>,
        timers: Arc<dyn RuntimeTimers>,
    ) -> Self {
        let runtime = Self(Arc::new(Inner {
            config: parts.config,
            host: parts.host_id,
            journal: parts.journal,
            socket: parts.socket,
            media: parts.media,
            factory: parts.factory,
            resources: parts.resources,
            translator: parts.translator,
            clock,
            timers,
            state: Mutex::new(State::default()),
            closing: Mutex::new(None),
        }));
        let weak = Arc::downgrade(&runtime.0);
        let policy = weak.clone();
        runtime.0.socket.bind(RuntimeCallbacks {
            wake: Arc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    Self(inner).wake();
                }
            }),
            automatic_notifications: Arc::new(move || {
                let selected = policy.upgrade();
                Box::pin(async move {
                    match selected {
                        Some(inner) => Self(inner).automatic_notifications().await,
                        None => Ok(false),
                    }
                })
            }),
        });
        runtime
    }
    fn attached(&self) -> Option<Arc<dyn RuntimeAttached>> {
        self.0.state.lock().unwrap().attached.clone()
    }
    pub fn run(&self, work: RuntimeWork) {
        self.run_job(Job::Work(work));
    }
    fn run_job(&self, job: Job) {
        let name = job.name();
        let (done, receive) = watch::channel(None);
        {
            let mut state = self.0.state.lock().unwrap();
            if state.closed {
                return;
            }
            if state.tasks.contains_key(name) {
                state.again.insert(name, job);
                return;
            }
            state.tasks.insert(name, receive);
        }
        let owner = self.clone();
        tokio::spawn(async move {
            let result = owner.execute(job).await;
            let next = {
                let mut state = owner.0.state.lock().unwrap();
                match &result {
                    Ok(()) => state.errors.retain(|(key, _)| *key != name),
                    Err(error) if error.code == "native_read_pending" => {
                        state.errors.retain(|(key, _)| *key != name)
                    }
                    Err(error) => {
                        if let Some((_, v)) = state.errors.iter_mut().find(|(key, _)| *key == name)
                        {
                            *v = error.code_js();
                        } else {
                            state.errors.push_back((name, error.code_js()));
                        }
                    }
                }
                state.tasks.remove(name);
                state.again.remove(name)
            };
            if let Some(job) = next {
                owner.run_job(job);
            }
            done.send_replace(Some(result));
        });
    }
    async fn execute(&self, job: Job) -> IvyResult<()> {
        match job {
            Job::PrepareListen(a) => {
                a.prepare().await?;
                self.listen().await
            }
            Job::Outbox(a) => {
                a.notices().await?;
                a.outbox().await?;
                self.run(RuntimeWork::Output);
                Ok(())
            }
            Job::Work(work) => match work {
                RuntimeWork::Control => self.control().await,
                RuntimeWork::Input => self.input().await,
                RuntimeWork::Listener => self.listen().await,
                RuntimeWork::InvalidateListen => {
                    if let Some(a) = self.attached() {
                        a.invalidate(false);
                    }
                    self.listen().await
                }
                RuntimeWork::Output => self.0.socket.flush().await,
                RuntimeWork::Status => self.status().await,
                RuntimeWork::ReadReceipts => self.0.socket.read_receipts().await,
                RuntimeWork::Activity => self.activity().await,
                RuntimeWork::Retention => self.retention().await,
                RuntimeWork::NativeEvents => self.native_events().await,
            },
        }
    }
    pub fn wake(&self) {
        for work in [
            RuntimeWork::Control,
            RuntimeWork::Input,
            RuntimeWork::ReadReceipts,
            RuntimeWork::Output,
        ] {
            self.run(work);
        }
    }
    pub fn changed(&self) {
        self.0
            .state
            .lock()
            .unwrap()
            .errors
            .retain(|(key, _)| *key != "native-events");
        if let Some(a) = self.attached() {
            a.invalidate(false);
        }
        self.outbox();
        self.run(RuntimeWork::Listener);
        self.wake();
    }
    fn outbox(&self) {
        if let Some(a) = self.attached() {
            self.run_job(Job::Outbox(a));
        }
    }
    pub fn attach(&self, attached: Option<Arc<dyn RuntimeAttached>>) {
        self.reset_notification_source();
        self.install(attached);
    }
    fn reset_notification_source(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.errors.retain(|(key, _)| *key != "native-events");
        state.last_sequence = 0.0;
        state.last_epoch = None;
    }
    fn install(&self, attached: Option<Arc<dyn RuntimeAttached>>) {
        {
            let mut state = self.0.state.lock().unwrap();
            // No new work is admitted after close. Release a late constructor's
            // lease too, including when asynchronous cleanup has already ended.
            if state.closed && attached.is_some() {
                return;
            }
            state.attached = attached.clone();
            if attached.is_none() {
                state.again.clear();
                state.events.clear();
                state.native_busy = false;
                state.event_revision = state.event_revision.wrapping_add(1);
                return;
            }
        }
        self.run_job(Job::PrepareListen(attached.unwrap()));
        self.wake();
    }
    pub fn start(&self) {
        self.0.socket.start();
        for (work, ms) in [
            (RuntimeWork::Control, 1000),
            (RuntimeWork::Input, 1000),
            (RuntimeWork::InvalidateListen, 30000),
            (RuntimeWork::Output, 5000),
            (RuntimeWork::Status, 10000),
            (RuntimeWork::ReadReceipts, 1000),
            (RuntimeWork::Activity, 1000),
            (RuntimeWork::Retention, 3600000),
        ] {
            let weak = Arc::downgrade(&self.0);
            let tick = Arc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    Self(inner).run(work);
                }
            });
            let timer = self.0.timers.every(work.name(), ms, tick);
            self.0.state.lock().unwrap().timers.push(timer);
        }
    }
    pub async fn settled(&self) {
        let tasks: Vec<_> = self
            .0
            .state
            .lock()
            .unwrap()
            .tasks
            .values()
            .cloned()
            .collect();
        let mut waits = tokio::task::JoinSet::new();
        for task in tasks {
            waits.spawn(completed(task));
        }
        while waits.join_next().await.is_some() {}
    }
    pub fn close(&self) -> ServiceFuture<()> {
        let mut closing = self.0.closing.lock().unwrap();
        if let Some(work) = &*closing {
            let work = work.clone();
            return Box::pin(completed(work));
        }
        {
            let mut state = self.0.state.lock().unwrap();
            state.closed = true;
            state.again.clear();
            for timer in state.timers.drain(..) {
                timer.clear();
            }
        }
        let (done, receive) = watch::channel(None);
        *closing = Some(receive.clone());
        let owner = self.clone();
        tokio::spawn(async move {
            let socket = owner.0.socket.close().await;
            owner.settled().await;
            owner.0.state.lock().unwrap().attached = None;
            let journal = owner.0.journal.close().await;
            // The account owner closes auth before releasing its lock. Preserve
            // finally precedence while awaiting its actual child retirement.
            let account = (owner.0.resources.retire_account)().await;
            let result = account.and(journal).and(socket);
            done.send_replace(Some(result));
        });
        Box::pin(completed(receive))
    }
}
impl WhatsAppRuntime {
    fn t(&self, key: &str, parameters: Json) -> IvyResult<JsString> {
        self.0.translator.translate(key, Some(&parameters))
    }
    fn complete(&self, row: &Inbox, text: Option<JsString>) -> IvyResult<()> {
        self.0.journal.access(|j| {
            j.try_transaction(|j| -> IvyResult<()> {
                if let Some(text) = text.filter(|t| !t.0.is_empty()) {
                    j.enqueue(
                        &format!("response:{}", row.id).into(),
                        &row.jid,
                        &text,
                        false,
                        self.0.clock.utc_millis(),
                    )?;
                }
                j.finish(&row.id)
            })
        })?;
        self.run(RuntimeWork::Output);
        Ok(())
    }
    pub async fn control(&self) -> IvyResult<()> {
        let Some(a) = self.attached() else {
            return Ok(());
        };
        let pending = self.0.journal.access(|j| j.pending())?;
        for (index, row) in pending.iter().enumerate() {
            let Some(command) = parse_command(&self.0.media.command_text(row)?) else {
                continue;
            };
            let name = command.name.to_utf8()?;
            if ["new", "model", "reasoning", "fast"].contains(&name.as_str())
                && !command.args.0.is_empty()
                && index > 0
            {
                continue;
            }
            if name == "new" && index > 0 {
                continue;
            }
            match a.command(row, &self.0.media.command_text(row)?).await {
                Ok(Some(text)) => self.complete(row, Some(text))?,
                Ok(None) => {}
                Err(error) => {
                    if native_changed(&error.code) {
                        a.invalidate(false);
                        return Err(error);
                    }
                    if retryable_whatsapp_error(&error.code) {
                        continue;
                    }
                    let detail = if error.code == "invalid_arguments" {
                        format!(" — {}", error.message).into()
                    } else {
                        JsString::default()
                    };
                    self.complete(
                        row,
                        Some(self.t(
                            "command.failed",
                            Json::object([
                                ("code", Json::String(error.code_js())),
                                ("detail", Json::String(detail)),
                            ]),
                        )?),
                    )?;
                }
            }
        }
        Ok(())
    }
    fn sender_channel(&self, row: &Inbox) -> IvyResult<Json> {
        let senders = self
            .0
            .config
            .required("senders")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let matching: Vec<_> = senders
            .iter()
            .filter(|s| s.get("principalId") == Some(&Json::from(row.sender.clone())))
            .collect();
        require(
            matching.len() == 1,
            "chat_sender_refused",
            "Each WhatsApp sender needs one unique principal.",
        )?;
        Ok(f(matching[0], "channelId").clone())
    }
    pub async fn input(&self) -> IvyResult<()> {
        let Some(a) = self.attached() else {
            return Ok(());
        };
        let Some(row) = self
            .0
            .journal
            .access(|j| Ok(j.pending()?.into_iter().next()))?
        else {
            return Ok(());
        };
        if parse_command(&self.0.media.command_text(&row)?).is_some() {
            return Ok(());
        }
        let result: IvyResult<()> = async {
            let mut request = match row.request.as_deref().filter(|v| !v.is_empty()) {
                Some(raw) => parse(raw, usize::MAX).map_err(|_| IvyError::internal())?,
                None => Json::Null,
            };
            if (&request).is_null() {
                let operation = a
                    .operation_id(Json::Array(vec![
                        "whatsapp-send".into(),
                        row.id.clone().into(),
                    ]))
                    .await?;
                a.ensure(&row.sender.clone().into(), &operation).await?;
                a.invalidate(false);
                let bound = a.bound().await?;
                let binding = f(f(&bound, "main"), "value")
                    .get("binding")
                    .filter(|v| !(v).is_null());
                if binding.is_none() {
                    return Err(IvyError::new_js(
                        "chat_main_missing".into(),
                        self.t("main.pending", Json::Object(vec![]))?,
                    ));
                }
                let senders = self
                    .0
                    .config
                    .required("senders")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?;
                let mut sender = None;
                for value in senders {
                    if value.get("principalId") == Some(&row.sender.clone().into())
                        && value.get("channelId") == Some(&self.sender_channel(&row)?)
                    {
                        sender = Some(value);
                        break;
                    }
                }
                require(
                    sender.is_some(),
                    "chat_sender_refused",
                    "Sender no longer admitted.",
                )?;
                let sender = sender.unwrap();
                let content = self.0.media.content(&row)?;
                let same_host = if !(f(&content, "documentMessage")).is_null()
                    || !(f(&content, "imageMessage")).is_null()
                {
                    a.same_host(&self.0.host).await?
                } else {
                    false
                };
                let payload = a.payload(&row, same_host).await?;
                let mut payload = object_options(&payload);
                payload.insert("turnOptions", a.turn_options()?);
                request = Json::object([
                    ("action", "send".into()),
                    ("operationId", operation),
                    ("messageId", row.id.clone().into()),
                    ("expectedBridge", a.expected(&row.sender.clone().into())),
                    ("expectedBinding", binding.unwrap().clone()),
                    (
                        "channel",
                        Json::object([
                            ("adapter", "whatsapp".into()),
                            ("accountId", f(&self.0.config, "accountId").clone()),
                            ("channelId", f(sender, "channelId").clone()),
                        ]),
                    ),
                    ("payload", payload),
                ]);
                self.0.journal.access(|j| {
                    j.try_transaction(|j| -> IvyResult<()> {
                        j.set(&format!("direct-input:{}", row.id), &true.into())?;
                        j.save_request(&row.id, &request)
                    })
                })?;
            }
            if (&self.0.journal.get(&format!("direct-input:{}", row.id))?)
                .as_bool()
                .unwrap_or(false)
            {
                if !a
                    .send(&row.sender.clone().into(), &request)
                    .await?
                    .is_null()
                {
                    self.complete(&row, None)?;
                }
                return Ok(());
            }
            let result = a.queue_send(&row.sender.clone().into(), &request).await?;
            if result.get("phase") == Some(&Json::from("succeeded")) {
                self.complete(&row, None)?;
            } else if result.get("phase") == Some(&Json::from("failed")) {
                let code = f(&result, "error")
                    .get("code")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or("unknown".into());
                self.complete(
                    &row,
                    Some(self.t("message.rejected", Json::object([("code", code)]))?),
                )?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if native_changed(&error.code) {
                a.invalidate(false);
                return Err(error);
            }
            if error.code == "chat_auth_required" {
                let key = format!("auth-required:{}", row.id);
                self.0.journal.access(|j| {
                    if !j.has_output(&key.clone().into())? {
                        j.enqueue(
                            &key.into(),
                            &row.jid,
                            &self.t("message.auth_required", Json::Object(vec![]))?,
                            false,
                            self.0.clock.utc_millis(),
                        )?;
                    }
                    Ok(())
                })?;
                self.run(RuntimeWork::Output);
                return Err(error);
            }
            if retryable_whatsapp_error(&error.code) {
                return Ok(());
            }
            let key = match error.code.as_str() {
                "interaction_expired" => "message.expired",
                "chat_main_unavailable" => "main.unavailable",
                _ => "message.failed",
            };
            self.complete(
                &row,
                Some(self.t(key, Json::object([("code", Json::String(error.code_js()))]))?),
            )?;
        }
        Ok(())
    }
    pub async fn listen(&self) -> IvyResult<()> {
        let Some(a) = self.attached() else {
            return Ok(());
        };
        let revision = self.0.state.lock().unwrap().event_revision;
        let result: IvyResult<()> = async {
            let commentary = (f(&a.preferences()?, "commentary"))
                .as_bool()
                .unwrap_or(false);
            let busy = a.tick(commentary).await?;
            {
                let mut state = self.0.state.lock().unwrap();
                if same(&state.attached, &a) && revision == state.event_revision {
                    state.native_busy = busy;
                    state.native_busy_at = self.0.clock.utc_millis();
                    a.set_busy(busy);
                }
            }
            self.run(RuntimeWork::Output);
            if a.needs_recovery() {
                self.run(RuntimeWork::Listener);
            }
            Ok(())
        }
        .await;
        self.outbox();
        result
    }
    pub async fn activity(&self) -> IvyResult<()> {
        let busy = {
            let state = self.0.state.lock().unwrap();
            state.attached.is_some()
                && state.native_busy
                && self.0.clock.utc_millis() - state.native_busy_at < 90000
        };
        let mut jids = Vec::new();
        if busy {
            for sender in self
                .0
                .config
                .required("senders")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                let number = string(sender.get("phoneNumber"))?;
                let number = String::from_utf16(&number.0)
                    .ok()
                    .and_then(|s| phone(&s).map(str::to_owned))
                    .unwrap_or_else(|| "null".into());
                jids.push(format!("{number}@s.whatsapp.net").into());
            }
        }
        self.0.socket.set_busy(jids);
        self.0.socket.presence().await
    }
    pub async fn retention(&self) -> IvyResult<()> {
        self.0
            .journal
            .access(|j| j.prune(HistoryLimits::default()))?;
        if !self.0.state.lock().unwrap().native_busy {
            self.0.media.prune().await?;
        }
        Ok(())
    }
    pub async fn automatic_notifications(&self) -> IvyResult<bool> {
        match self.attached() {
            Some(a) => a.automatic_notifications(self.0.clock.utc_millis()).await,
            None => Ok(false),
        }
    }
    pub async fn status(&self) -> IvyResult<()> {
        let socket = self.0.socket.status();
        let (attached, error) = {
            let state = self.0.state.lock().unwrap();
            (
                state.attached.clone(),
                state
                    .errors
                    .front()
                    .map(|(_, v)| Json::String(v.clone()))
                    .unwrap_or(Json::Null),
            )
        };
        let code = socket
            .get("errorCode")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(error);
        let mut status = Json::object([(
            "observedAt",
            Json::String(date_to_iso(self.0.clock.utc_millis() as f64)?),
        )]);
        for name in [
            "state",
            "connected",
            "online",
            "privacy",
            "lastOnlinePresenceAt",
        ] {
            if let Some(value) = socket.get(name) {
                status.insert(name, value.clone());
            }
        }
        status.insert("errorCode", code);
        status.insert("attached", attached.is_some().into());
        status.insert(
            "lastNativeDispatchMs",
            attached
                .as_ref()
                .and_then(|a| a.last_dispatch_ms())
                .map(|n| Json::Number(n as f64))
                .unwrap_or(Json::Null),
        );
        if let Some(value) = socket.get("lastSendMs") {
            status.insert("lastWhatsAppSendMs", value.clone());
        }
        let (inputs, outputs) = self.0.journal.access(|j| {
            let count = |query| {
                j.connection()
                    .query_row(query, [], |r| r.get::<_, i64>(0))
                    .map_err(|_| IvyError::internal())
            };
            Ok((
                count("SELECT count(*) FROM inbox WHERE state='pending'")?,
                count("SELECT count(*) FROM outgoing WHERE state<>'sent'")?,
            ))
        })?;
        status.insert("pendingInputs", Json::Number(inputs as f64));
        status.insert("pendingOutputs", Json::Number(outputs as f64));
        (self.0.resources.status)(status).await
    }
    pub fn notification(&self, value: &Json) -> IvyResult<()> {
        let params = value.required("params").map_err(|_| IvyError::internal())?;
        if params.is_null() {
            return Err(IvyError::internal());
        }
        let event = f(params, "payload");
        let Some(a) = self.attached() else {
            return Ok(());
        };
        if params.get("serviceNodeId") != f(a.definition(), "project").get("serviceNodeId")
            || !object(event)
            || event.get("serviceNodeId") != params.get("serviceNodeId")
            || event.get("nativeVersion") != f(a.definition(), "nativePlan").get("nativeVersion")
        {
            return Ok(());
        }
        if let (Some(sequence), Some(epoch)) = (
            event.get("sequence").and_then(Json::as_f64),
            event.get("epoch").and_then(Json::as_js_string),
        ) {
            let changed = {
                let mut state = self.0.state.lock().unwrap();
                let changed = state.last_epoch.as_ref().is_some_and(|e| e != epoch);
                let gap = state.last_sequence != 0.0
                    && !state.last_sequence.is_nan()
                    && sequence > state.last_sequence + 1.0;
                state.last_epoch = Some(epoch.clone());
                state.last_sequence = sequence;
                if changed || gap { Some(changed) } else { None }
            };
            if let Some(changed) = changed {
                a.invalidate(changed);
                self.run_job(Job::PrepareListen(a));
            }
        }
        let method = string(event.get("method"))?;
        if !["turn/started", "turn/completed", "item/completed"]
            .iter()
            .any(|v| method == JsString::from(*v))
            || !object(f(event, "params"))
        {
            return Ok(());
        }
        let overflow = {
            let mut state = self.0.state.lock().unwrap();
            let overflow = state.events.len() >= 256;
            if overflow {
                state.events.pop_front();
            }
            state.events.push_back(event.clone());
            overflow
        };
        if overflow {
            self.run(RuntimeWork::Listener);
        }
        self.run(RuntimeWork::NativeEvents);
        Ok(())
    }
    async fn native_events(&self) -> IvyResult<()> {
        loop {
            let selected = {
                let mut state = self.0.state.lock().unwrap();
                match (state.attached.clone(), state.events.pop_front()) {
                    (Some(a), Some(event)) => Some((a, event)),
                    (_, Some(event)) => {
                        state.events.push_front(event);
                        None
                    }
                    _ => None,
                }
            };
            let Some((a, event)) = selected else {
                return Ok(());
            };
            let commentary = (f(&a.preferences()?, "commentary"))
                .as_bool()
                .unwrap_or(false);
            let result = a.notification(&event, commentary).await?;
            let current = {
                let state = self.0.state.lock().unwrap();
                same(&state.attached, &a)
            };
            if !(&result).is_null() && current {
                if result.get("busy") != Some(&Json::Null) {
                    let busy = result
                        .get("busy")
                        .and_then(Json::as_bool)
                        .ok_or_else(IvyError::internal)?;
                    {
                        let mut state = self.0.state.lock().unwrap();
                        state.event_revision = state.event_revision.wrapping_add(1);
                        state.native_busy = busy;
                        state.native_busy_at = self.0.clock.utc_millis();
                        a.set_busy(busy);
                    }
                    self.run(RuntimeWork::Activity);
                    if !busy {
                        self.wake();
                    }
                }
                self.run(RuntimeWork::Output);
            }
        }
    }
}
impl crate::runtime::ChatTransport for WhatsAppRuntime {
    fn start(&self) {
        WhatsAppRuntime::start(self)
    }
    fn attach(&self, generation: Option<Arc<ChatGeneration>>) -> IvyResult<()> {
        self.reset_notification_source();
        // Construction must finish before replacing the old attachment. The
        // original constructor error rejects service reconciliation directly.
        let attached = generation.map(|g| self.0.factory.create(g)).transpose()?;
        self.install(attached);
        Ok(())
    }
    fn changed(&self) {
        WhatsAppRuntime::changed(self)
    }
    fn notification(&self, frame: &Json) {
        let _ = WhatsAppRuntime::notification(self, frame);
    }
    fn state(&self) -> JsString {
        self.0.socket.state()
    }
    fn created_main(&self, id: &str) -> IvyResult<()> {
        self.0
            .journal
            .set(&format!("listener-new:{id}"), &true.into())
    }
    fn settled(&self) -> ServiceFuture<()> {
        let owner = self.clone();
        Box::pin(async move {
            WhatsAppRuntime::settled(&owner).await;
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<()> {
        WhatsAppRuntime::close(self)
    }
}
