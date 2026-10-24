//! Native protocol client bound to the retained transport journal.
use super::{
    auth_backend::NativeAuthStore,
    identity::identity,
    journal::{Outgoing, OutputState},
    journal_owner::Journal,
    message_wire::message_json,
    runtime::{RuntimeCallbacks, RuntimeSocket},
    sender::{self, MessageKey},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{parse, stringify},
};
use ivy_host_runtime::config::atomic_json;
use ivy_sdk::{
    clock::{Clock, SystemClock, date_to_iso, parse_date},
    service::ServiceFuture,
};
use rusqlite::OptionalExtension;
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, watch};
use wacore::{
    store::traits::DeviceStore,
    types::{
        events::{Event, EventKind, InboundMessage},
        presence::ReceiptType,
    },
};
use whatsapp_rust::{
    InboundDurabilityHook, Jid,
    bot::{Bot, BotHandle, EventDelivery},
    client::Client,
    http::{HttpClient, UreqHttpClient},
    send::SendOptions,
    transport::{TokioWebSocketTransportFactory, TransportFactory},
};

pub(super) fn same_user(left: &str, right: &str) -> bool {
    if let (Some(left), Some(right)) = (sender::jid_phone(left), sender::jid_phone(right)) {
        return left == right;
    }
    match (left.parse::<Jid>(), right.parse::<Jid>()) {
        (Ok(left), Ok(right)) => left.to_non_ad() == right.to_non_ad(),
        _ => false,
    }
}
pub(super) fn lid_for_phone<'a>(expected: &'a str, peer: &str) -> Option<(&'a str, Jid)> {
    let phone = sender::jid_phone(expected)?;
    let jid = peer.parse::<Jid>().ok()?;
    (jid.is_lid() && jid.integrator == 0).then_some((phone, jid))
}
fn storage_error() -> IvyError {
    IvyError::new("storage_unavailable", "WhatsApp transport storage failed.")
}
fn protocol_error(code: &str) -> IvyError {
    IvyError::new(code, "The native WhatsApp operation failed.")
}
fn field(value: &Json, name: &str) -> IvyResult<String> {
    value.required(name)?.as_str()
}
fn integer(value: &Json, name: &str) -> IvyResult<i64> {
    value
        .required(name)?
        .as_f64()
        .filter(|n| n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0)
        .map(|n| n as i64)
        .ok_or_else(storage_error)
}
struct Sender {
    number: String,
    principal: String,
    channel: Json,
}
struct Config {
    account: Json,
    bot: String,
    since: i64,
    heartbeat: i64,
    senders: Vec<Sender>,
}
impl Config {
    fn read(value: &Json) -> IvyResult<Self> {
        let bot = field(value, "botPhoneNumber")?;
        let bot = sender::phone(&bot)
            .ok_or_else(IvyError::internal)?
            .to_owned();
        let since = parse_date(
            value
                .required("since")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
        );
        if !since.is_finite() {
            return Err(IvyError::internal());
        }
        Ok(Self {
            account: value.required("accountId")?.clone(),
            bot,
            since: since as i64,
            heartbeat: value
                .get("heartbeatMs")
                .map(|_| integer(value, "heartbeatMs"))
                .transpose()?
                .unwrap_or(120_000),
            senders: value
                .required("senders")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(|sender| {
                    Ok(Sender {
                        number: field(sender, "phoneNumber")?,
                        principal: field(sender, "principalId")?,
                        channel: sender.required("channelId")?.clone(),
                    })
                })
                .collect::<IvyResult<_>>()?,
        })
    }
}
struct State {
    stopped: bool,
    connected: bool,
    name: &'static str,
    error: Option<String>,
    online: Option<bool>,
    privacy: Json,
    last_online: Option<i64>,
    last_send_ms: Option<u64>,
    busy: HashSet<String>,
    busy_at: i64,
    composing: HashSet<String>,
    composing_at: i64,
}
impl Default for State {
    fn default() -> Self {
        Self {
            stopped: false,
            connected: false,
            name: "stopped",
            error: None,
            online: None,
            privacy: Json::Null,
            last_online: None,
            last_send_ms: None,
            busy: HashSet::new(),
            busy_at: 0,
            composing: HashSet::new(),
            composing_at: 0,
        }
    }
}
struct Inner {
    config: Config,
    file_config: Json,
    journal: Journal,
    backend: Arc<NativeAuthStore>,
    root: PathBuf,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    callbacks: Mutex<Option<Arc<RuntimeCallbacks>>>,
    client: OnceLock<Arc<Client>>,
    lifecycle: Mutex<()>,
    bot: Mutex<Option<Bot>>,
    handle: Mutex<Option<BotHandle>>,
    inbound: AsyncMutex<()>,
    output: AsyncMutex<()>,
    reading: AsyncMutex<()>,
    presence: AsyncMutex<()>,
    closing: Mutex<Option<watch::Receiver<Option<IvyResult<()>>>>>,
}
#[derive(Clone)]
pub struct WhatsAppSocket(Arc<Inner>);
struct DurableInbox(Weak<Inner>);
#[async_trait::async_trait]
impl InboundDurabilityHook for DurableInbox {
    async fn on_messages(
        &self,
        _: Arc<Client>,
        messages: &[InboundMessage],
    ) -> whatsapp_rust::anyhow::Result<()> {
        let owner = self
            .0
            .upgrade()
            .ok_or_else(|| whatsapp_rust::anyhow::anyhow!("Transport closed"))?;
        let socket = WhatsAppSocket(owner);
        match socket.receive(messages).await {
            Ok(()) => Ok(()),
            Err(error) => {
                socket.error("whatsapp_inbound_failed");
                Err(whatsapp_rust::anyhow::anyhow!(
                    "Inbound journal refused the batch ({})",
                    error.code
                ))
            }
        }
    }
}
impl WhatsAppSocket {
    /// Construct without connecting. The runtime starts networking after it has
    /// bound the policy callbacks and acquired the account resources.
    pub async fn open(
        config: Json,
        journal: Journal,
        backend: Arc<NativeAuthStore>,
        root: PathBuf,
    ) -> IvyResult<Self> {
        Self::with_network(
            config,
            journal,
            backend,
            root,
            Arc::new(SystemClock),
            TokioWebSocketTransportFactory::new(),
            UreqHttpClient::new(),
        )
        .await
    }
    /// Hosts may supply their native network adapters; tests use offline ones.
    pub async fn with_network<F: TransportFactory + 'static, H: HttpClient + 'static>(
        config: Json,
        journal: Journal,
        backend: Arc<NativeAuthStore>,
        root: PathBuf,
        clock: Arc<dyn Clock>,
        transport: F,
        http: H,
    ) -> IvyResult<Self> {
        let file_config = config.clone();
        let config = Config::read(&config)?;
        if backend
            .load()
            .await
            .map_err(|_| storage_error())?
            .and_then(|device| device.pn)
            .is_some_and(|jid| sender::jid_phone(&jid.to_string()) != Some(config.bot.as_str()))
        {
            return Err(protocol_error("whatsapp_account_mismatch"));
        }
        let owner = Self(Arc::new(Inner {
            config,
            file_config,
            journal,
            backend: backend.clone(),
            root,
            clock,
            state: Mutex::new(State::default()),
            callbacks: Mutex::new(None),
            client: OnceLock::new(),
            lifecycle: Mutex::new(()),
            bot: Mutex::new(None),
            handle: Mutex::new(None),
            inbound: AsyncMutex::new(()),
            output: AsyncMutex::new(()),
            reading: AsyncMutex::new(()),
            presence: AsyncMutex::new(()),
            closing: Mutex::new(None),
        }));
        let weak = Arc::downgrade(&owner.0);
        let bot = Bot::builder()
            .with_backend_arc(backend)
            .with_transport_factory(transport)
            .with_http_client(http)
            .with_device_props(wacore::store::DevicePropsOverride {
                os: Some("Ivy ChatBridge".into()),
                ..Default::default()
            })
            .skip_history_sync()
            .with_event_delivery(EventDelivery::Ordered { capacity: 256 })
            .with_inbound_durability_hook(DurableInbox(weak.clone()))
            .on_event_for(
                &[
                    EventKind::Connected,
                    EventKind::Disconnected,
                    EventKind::LoggedOut,
                    EventKind::PairingQrCode,
                    EventKind::Receipt,
                    EventKind::ServerAck,
                ],
                move |event, _| {
                    let weak = weak.clone();
                    async move {
                        if let Some(inner) = weak.upgrade() {
                            let socket = WhatsAppSocket(inner);
                            if let Err(error) = socket.observe(&event).await {
                                socket.error(&error.code);
                            }
                        }
                    }
                },
            )
            .build()
            .await
            .map_err(|_| protocol_error("whatsapp_start_failed"))?;
        owner.0.backend.bind_journal(owner.0.journal.clone())?;
        owner
            .0
            .client
            .set(bot.client())
            .map_err(|_| IvyError::internal())?;
        *owner.0.bot.lock().unwrap() = Some(bot);
        Ok(owner)
    }
    fn client(&self) -> Arc<Client> {
        self.0.client.get().unwrap().clone()
    }
    fn active(&self) -> bool {
        let state = self.0.state.lock().unwrap();
        state.connected && !state.stopped
    }
    fn wake(&self) {
        let callback = self.0.callbacks.lock().unwrap().clone();
        if let Some(c) = callback {
            (c.wake)();
        }
    }
    fn error(&self, code: &str) {
        self.0.state.lock().unwrap().error = Some(code.to_owned());
    }
    fn clear_error(&self, code: &str) {
        let mut state = self.0.state.lock().unwrap();
        if state.error.as_deref() == Some(code) {
            state.error = None;
        }
    }
    /// Awaited by the native durability hook. Journal commits precede native
    /// acknowledgement; redelivered envelopes reuse the retained content hash.
    pub async fn receive(&self, messages: &[InboundMessage]) -> IvyResult<()> {
        let _serial = self.0.inbound.lock().await;
        if self.0.state.lock().unwrap().stopped {
            return Err(protocol_error("whatsapp_stopped"));
        }
        let snapshot = self.client().persistence_manager().get_device_snapshot();
        if snapshot.pn.as_ref().is_none_or(|jid| {
            sender::jid_phone(&jid.to_string()) != Some(self.0.config.bot.as_str())
        }) {
            return Err(protocol_error("whatsapp_account_mismatch"));
        }
        let mut admitted = false;
        for incoming in messages {
            let info = &incoming.info;
            let remote = info.source.chat.to_string();
            let alternate = info
                .source
                .sender_alt
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            let key = MessageKey {
                id: &info.id,
                remote_jid: &remote,
                remote_jid_alt: &alternate,
                from_me: info.source.is_from_me,
                participant: if info.source.is_group { "group" } else { "" },
            };
            if !sender::eligible(&key) {
                continue;
            }
            let mapped = if sender::needs_lid_mapping(&key) {
                self.client()
                    .get_lid_pn_entry(&info.source.chat)
                    .await
                    .map_err(|_| storage_error())?
                    .map(|entry| format!("{}@s.whatsapp.net", entry.phone_number))
            } else {
                None
            };
            let numbers: Vec<_> = self
                .0
                .config
                .senders
                .iter()
                .map(|s| s.number.as_str())
                .collect();
            let Some(index) = sender::admitted_sender(&key, mapped.as_deref(), &numbers) else {
                continue;
            };
            let sender = &self.0.config.senders[index];
            let now = self.0.clock.utc_millis();
            let received = info.timestamp.timestamp_millis();
            if received < self.0.config.since.max(now.saturating_sub(7 * 86_400_000))
                || received > now.saturating_add(300_000)
            {
                continue;
            }
            let content = message_json(&incoming.message)?;
            let envelope_key = Json::object([
                ("remoteJid", remote.clone().into()),
                ("fromMe", false.into()),
                ("id", info.id.clone().into()),
            ]);
            let envelope = Json::object([
                ("key", envelope_key.clone()),
                ("message", content.clone()),
                (
                    "messageTimestamp",
                    info.timestamp.timestamp().to_string().into(),
                ),
                ("pushName", info.push_name.clone().into()),
            ]);
            let raw = stringify(&envelope, usize::MAX)?;
            if raw.len() > 1024 * 1024 {
                self.error("whatsapp_message_too_large");
                continue;
            }
            let id = identity(&[
                self.0.config.account.clone(),
                sender.channel.clone(),
                info.id.clone().into(),
            ])?;
            let fingerprint = identity(&[stringify(&content, usize::MAX)?.into()])?;
            if self.0.state.lock().unwrap().stopped {
                return Err(protocol_error("whatsapp_stopped"));
            }
            self.0.journal.access(|journal| {
                let prior: Option<(String, String)> = journal
                    .connection()
                    .query_row("SELECT sender,message FROM inbox WHERE id=?", [&id], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .optional()
                    .map_err(|_| storage_error())?;
                if let Some((principal, original)) = prior {
                    let saved = journal.get(&format!("inbox-content:{id}"))?;
                    let hash = if saved.is_null() {
                        let value = parse(&original, usize::MAX)?;
                        identity(&[stringify(value.required("message")?, usize::MAX)?.into()])?
                    } else {
                        saved.as_str()?
                    };
                    if principal != sender.principal || hash != fingerprint {
                        self.error("whatsui_identity_conflict");
                    }
                    return Ok(());
                }
                journal.try_transaction(|journal| -> IvyResult<()> {
                    journal.accept(&id, &sender.principal, &remote, &raw, received)?;
                    journal.set(&format!("inbox-content:{id}"), &fingerprint.clone().into())?;
                    journal.set(
                        &format!("read-receipt:{id}"),
                        &Json::object([("key", envelope_key), ("done", false.into())]),
                    )
                })
            })?;
            admitted = true;
        }
        if admitted {
            self.0.journal.flush().await?;
            self.wake();
        }
        Ok(())
    }
    /// Native events are limited to transport status and correlated receipts;
    /// message admission runs exclusively through the durability hook above.
    pub async fn observe(&self, event: &Event) -> IvyResult<()> {
        if self.0.state.lock().unwrap().stopped {
            return Ok(());
        }
        match event {
            Event::Connected(_) => {
                let snapshot = self.client().persistence_manager().get_device_snapshot();
                if snapshot.pn.as_ref().is_none_or(|jid| {
                    sender::jid_phone(&jid.to_string()) != Some(self.0.config.bot.as_str())
                }) {
                    {
                        let mut state = self.0.state.lock().unwrap();
                        state.name = "account_mismatch";
                        state.connected = false;
                        state.error = Some("whatsapp_account_mismatch".into());
                    }
                    self.client().disconnect().await;
                    self.wake();
                    return Ok(());
                }
                {
                    let mut state = self.0.state.lock().unwrap();
                    state.connected = true;
                    state.name = "connected";
                    state.error = None;
                    state.last_online = None;
                }
                atomic_json(
                    self.0.root.join("whatsapp-pairing.json"),
                    Json::object([("qr", Json::Null), ("expiresAt", Json::Null)]),
                )
                .await?;
                if let Ok(settings) = self.client().fetch_privacy_settings().await {
                    let value = |name| {
                        settings
                            .settings
                            .iter()
                            .find(|s| s.category.as_str() == name)
                            .map(|s| Json::from(s.value.as_str()))
                            .unwrap_or(Json::Null)
                    };
                    self.0.state.lock().unwrap().privacy =
                        Json::object([("online", value("online")), ("lastSeen", value("last"))]);
                }
                self.wake();
            }
            Event::Disconnected(_) => {
                let mut state = self.0.state.lock().unwrap();
                state.connected = false;
                state.online = Some(false);
                state.busy.clear();
                state.composing.clear();
                state.composing_at = 0;
                if !["logged_out", "account_mismatch"].contains(&state.name) {
                    state.name = "reconnecting";
                    state.error = Some("whatsapp_disconnected".into());
                }
            }
            Event::LoggedOut(_) => {
                let mut state = self.0.state.lock().unwrap();
                state.connected = false;
                state.online = Some(false);
                state.name = "logged_out";
                state.error = Some("whatsapp_logged_out".into());
            }
            Event::PairingQrCode(qr) => {
                self.0.state.lock().unwrap().name = "awaiting_qr";
                let expires = self
                    .0
                    .clock
                    .utc_millis()
                    .saturating_add(i64::try_from(qr.timeout.as_millis()).unwrap_or(60_000));
                atomic_json(
                    self.0.root.join("whatsapp-pairing.json"),
                    Json::object([
                        ("qr", qr.code.clone().into()),
                        ("expiresAt", Json::String(date_to_iso(expires as f64)?)),
                    ]),
                )
                .await?;
            }
            Event::Receipt(receipt)
                if matches!(
                    receipt.r#type,
                    ReceiptType::Delivered
                        | ReceiptType::Sent
                        | ReceiptType::Read
                        | ReceiptType::ReadSelf
                        | ReceiptType::Played
                        | ReceiptType::PlayedSelf
                ) =>
            {
                for id in &receipt.message_ids {
                    self.confirm(id, &receipt.source.chat.to_string()).await?;
                }
            }
            Event::ServerAck(ack)
                if ack.class.as_deref() == Some("message") && ack.error.is_none() =>
            {
                if let Some(jid) = &ack.from {
                    self.confirm(&ack.id, &jid.to_string()).await?;
                }
            }
            _ => {}
        }
        self.wake();
        Ok(())
    }
    async fn confirm(&self, id: &str, jid: &str) -> IvyResult<()> {
        let original: Option<String> = self.0.journal.access(|journal| {
            journal
                .connection()
                .query_row("SELECT jid FROM outgoing WHERE id=?", [id], |r| r.get(0))
                .optional()
                .map_err(|_| storage_error())
        })?;
        let Some(original) = original else {
            return Ok(());
        };
        let matched = if same_user(&original, jid) {
            true
        } else if let Some((phone, lid)) = lid_for_phone(&original, jid) {
            self.client()
                .get_lid_pn_entry(&lid)
                .await
                .map_err(|_| storage_error())?
                .is_some_and(|entry| entry.phone_number.as_ref() == phone)
        } else {
            false
        };
        if matched {
            self.0
                .journal
                .access(|journal| crate::files::mark(journal, id, OutputState::Sent, None, self.0.clock.utc_millis()))?;
            self.0.journal.flush().await?;
        }
        Ok(())
    }
    fn outgoing(&self, automatic: bool) -> IvyResult<Vec<Outgoing>> {
        self.0.journal.access(|journal| {
            let sql = if automatic {
                "SELECT id,jid,text,state,sequence FROM outgoing WHERE state IN ('pending','sending') ORDER BY sequence LIMIT 64"
            } else {
                "SELECT id,jid,text,state,sequence FROM outgoing WHERE state IN ('pending','sending') AND NOT EXISTS (SELECT 1 FROM kv WHERE key='out-automatic:'||outgoing.id) ORDER BY sequence LIMIT 64"
            };
            let mut query = journal.connection().prepare(sql).map_err(|_| storage_error())?;
            let rows = query.query_map([], |r| Ok(Outgoing { id: r.get(0)?, jid: r.get(1)?, text: r.get(2)?, state: r.get(3)?, sequence: r.get(4)? })).map_err(|_| storage_error())?;
            rows.collect::<rusqlite::Result<_>>().map_err(|_| storage_error())
        })
    }
    async fn contact_allowed(&self) -> bool {
        let callback = self.0.callbacks.lock().unwrap().clone();
        let value = match callback {
            Some(c) => (c.automatic_notifications)().await,
            None => Ok(false),
        };
        match value {
            Ok(allowed) => {
                self.clear_error("whatsapp_contact_policy_unavailable");
                allowed
            }
            Err(_) => {
                self.error("whatsapp_contact_policy_unavailable");
                false
            }
        }
    }
    async fn flush_outputs(&self) -> IvyResult<()> {
        let Ok(_serial) = self.0.output.try_lock() else {
            return Ok(());
        };
        if !self.active() {
            return Ok(());
        }
        let mut rows = self.outgoing(true)?;
        let mut automatic = false;
        for row in &rows {
            if self
                .0
                .journal
                .get(&format!("out-automatic:{}", row.id))?
                .as_bool()
                == Some(true)
            {
                automatic = true;
                break;
            }
        }
        if automatic && !self.contact_allowed().await {
            rows = self.outgoing(false)?;
        }
        let mut blocked = HashSet::new();
        for row in rows {
            if blocked.contains(&row.jid) || !self.active() {
                continue;
            }
            if !self.0.journal.get(&format!("out-file:{}", row.id))?.is_null() {
                if self.flush_file(&row).await? { blocked.insert(row.jid); }
                continue;
            }
            if self
                .0
                .journal
                .get(&format!("out-automatic:{}", row.id))?
                .as_bool()
                == Some(true)
                && !self.contact_allowed().await
            {
                continue;
            }
            if !self.active() {
                break;
            }
            let key = format!("send-attempt:{}", row.id);
            let attempt = self.0.journal.get(&key)?;
            let (count, at) = if attempt.is_null() {
                (0, 0)
            } else {
                (integer(&attempt, "count")?, integer(&attempt, "at")?)
            };
            if count >= 3 {
                self.0
                    .journal
                    .access(|j| j.mark(&row.id, OutputState::Unknown))?;
                self.error("whatsapp_delivery_unknown");
                continue;
            }
            if count < 0 {
                return Err(storage_error());
            }
            let now = self.0.clock.utc_millis();
            if at != 0 && now.saturating_sub(at) < 2000 * (1 << count) {
                blocked.insert(row.jid);
                continue;
            }
            self.0.journal.access(|j| {
                j.try_transaction(|j| -> IvyResult<()> {
                    j.mark(&row.id, OutputState::Sending)?;
                    j.set(
                        &key,
                        &Json::object([
                            ("count", Json::Number((count + 1) as f64)),
                            ("at", Json::Number(now as f64)),
                        ]),
                    )
                })
            })?;
            self.0.journal.flush().await?;
            let started = self.0.clock.monotonic();
            let jid = row
                .jid
                .parse::<Jid>()
                .map_err(|_| protocol_error("whatsapp_recipient_invalid"))?;
            let result = self
                .client()
                .send_message_with_options(
                    jid,
                    waproto::whatsapp::Message {
                        conversation: Some(row.text),
                        ..Default::default()
                    },
                    SendOptions::default().with_message_id(row.id.clone()),
                )
                .await;
            if result.is_ok() {
                self.0.state.lock().unwrap().last_send_ms = Some(
                    self.0
                        .clock
                        .monotonic()
                        .saturating_duration_since(started)
                        .as_millis() as u64,
                );
                self.0
                    .journal
                    .access(|j| j.mark(&row.id, OutputState::Sent))?;
            } else {
                blocked.insert(row.jid);
                self.error("whatsapp_delivery_unknown");
            }
        }
        let unsettled = self.0.journal.access(|j| {
            j.connection()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM outgoing WHERE state IN ('sending','unknown'))",
                    [],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(|_| storage_error())
        })?;
        if !unsettled {
            self.clear_error("whatsapp_delivery_unknown");
        }
        Ok(())
    }
    async fn flush_file(&self, row: &Outgoing) -> IvyResult<bool> {
        let files = crate::files::ChatFiles::with_clock(self.0.journal.clone(), self.file_config(), self.0.clock.clone());
        if row.state == "sending" {
            let submitted = self.0.journal.get(&format!("file-submitted:{}", row.id))?.as_f64();
            if submitted.is_none_or(|at| self.0.clock.utc_millis() as f64 - at >= 60_000.0) {
                files.mark(&row.id, OutputState::Unknown, Some("whatsapp_delivery_unknown"))?;
                self.0.journal.flush().await?;
                return Ok(false);
            }
            return Ok(true);
        }
        let callback = self.0.callbacks.lock().unwrap().clone();
        let Some(callback) = callback else { return Ok(true); };
        let admitted: IvyResult<_> = async {
            let caller = files.caller(&row.id)?;
            let target = files.target(&(callback.file_workspace)(caller.clone()).await?)?;
            files.admitted(&row.id, &target)?;
            let payload = files.payload(&row.id)?;
            let target = files.target(&(callback.file_workspace)(caller).await?)?;
            files.admitted(&row.id, &target)?;
            Ok(payload)
        }.await;
        let payload = match admitted {
            Ok(payload) => payload,
            Err(error) => {
                if matches!(error.code.as_str(), "chat_file_target_changed" | "chat_file_integrity" | "chat_file_type" | "limit_exceeded") {
                    files.mark(&row.id, OutputState::Failed, Some(error.code.as_str()))?;
                    self.0.journal.flush().await?;
                    return Ok(false);
                }
                return Ok(true);
            }
        };
        if !self.active() { return Ok(true); }
        // Persist dispatch before uploading. A lost reply or restart must never
        // submit another media message under an uncertain original operation.
        files.mark(&row.id, OutputState::Sending, None)?;
        self.0.journal.flush().await?;
        let started = self.0.clock.monotonic();
        let result = self.send_file(row, payload).await;
        if result.is_ok() {
            self.0.state.lock().unwrap().last_send_ms = Some(self.0.clock.monotonic().saturating_duration_since(started).as_millis() as u64);
            self.0.journal.set(&format!("file-submitted:{}", row.id), &Json::Number(self.0.clock.utc_millis() as f64))?;
        } else {
            files.mark(&row.id, OutputState::Unknown, Some("whatsapp_delivery_unknown"))?;
            self.error("whatsapp_delivery_unknown");
        }
        self.0.journal.flush().await?;
        // Only the original recipient's server acknowledgement makes it sent.
        Ok(!self.0.journal.access(|j| j.sent(&[row.id.clone()]))?)
    }
    fn file_config(&self) -> Json { self.0.file_config.clone() }
    async fn send_file(&self, row: &Outgoing, payload: crate::files::FilePayload) -> IvyResult<()> {
        use whatsapp_rust::{media::{self, ImageOptions, DocumentOptions}, upload::UploadOptions};
        let mime = payload.file.required("mediaType")?.as_str()?;
        let filename = payload.file.required("filename")?.as_str()?;
        let upload = self.client().upload(payload.bytes, if payload.image { wacore::download::MediaType::Image } else { wacore::download::MediaType::Document }, UploadOptions::default()).await.map_err(|_| protocol_error("whatsapp_delivery_unknown"))?;
        let message = if payload.image {
            media::image_message(upload, ImageOptions { caption: payload.caption, mimetype: Some(mime), ..Default::default() })
        } else {
            media::document_message(upload, DocumentOptions { caption: payload.caption, file_name: Some(filename), mimetype: Some(mime), ..Default::default() })
        };
        let jid = row.jid.parse::<Jid>().map_err(|_| protocol_error("whatsapp_recipient_invalid"))?;
        self.client().send_message_with_options(jid, message, SendOptions::default().with_message_id(row.id.clone())).await.map_err(|_| protocol_error("whatsapp_delivery_unknown"))?;
        Ok(())
    }
    async fn send_read_receipts(&self) -> IvyResult<()> {
        let Ok(_serial) = self.0.reading.try_lock() else {
            return Ok(());
        };
        if !self.active() {
            return Ok(());
        }
        self.0.journal.flush().await?;
        let rows = self.0.journal.access(|j| {
            let mut query = j.connection().prepare("SELECT key,value FROM kv WHERE key LIKE 'read-receipt:%' AND json_extract(value,'$.done')=0 LIMIT 32").map_err(|_| storage_error())?;
            query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|_| storage_error())?.collect::<rusqlite::Result<Vec<_>>>().map_err(|_| storage_error())
        })?;
        for (key, raw) in rows {
            if !self.active() {
                break;
            }
            let value = parse(&raw, usize::MAX)?;
            let message = value.required("key")?;
            let jid = field(message, "remoteJid")?
                .parse::<Jid>()
                .map_err(|_| storage_error())?;
            self.client()
                .mark_as_read(&jid, None, &[&field(message, "id")?])
                .await
                .map_err(|_| protocol_error("whatsapp_read_receipt_failed"))?;
            self.0.journal.set(&key, &Json::Null)?;
        }
        self.clear_error("whatsapp_read_receipt_failed");
        Ok(())
    }
    async fn send_presence(&self) -> IvyResult<()> {
        let Ok(_serial) = self.0.presence.try_lock() else {
            return Ok(());
        };
        if !self.active() {
            return Ok(());
        }
        let now = self.0.clock.utc_millis();
        let (active, composing, refresh, online) = {
            let s = self.0.state.lock().unwrap();
            (
                if now.saturating_sub(s.busy_at) < 20_000 {
                    s.busy.clone()
                } else {
                    HashSet::new()
                },
                s.composing.clone(),
                now.saturating_sub(s.composing_at) >= 4000,
                s.last_online
                    .is_none_or(|at| now.saturating_sub(at) >= self.0.config.heartbeat),
            )
        };
        for jid in composing.difference(&active) {
            let to = jid
                .parse::<Jid>()
                .map_err(|_| protocol_error("whatsapp_presence_failed"))?;
            self.client()
                .chatstate()
                .send_paused(&to)
                .await
                .map_err(|_| protocol_error("whatsapp_presence_failed"))?;
            self.0.state.lock().unwrap().composing.remove(jid);
        }
        for jid in &active {
            if refresh || !composing.contains(jid) {
                let to = jid
                    .parse::<Jid>()
                    .map_err(|_| protocol_error("whatsapp_presence_failed"))?;
                self.client()
                    .chatstate()
                    .send_composing(&to)
                    .await
                    .map_err(|_| protocol_error("whatsapp_presence_failed"))?;
                self.0.state.lock().unwrap().composing.insert(jid.clone());
            }
        }
        if refresh && !active.is_empty() {
            self.0.state.lock().unwrap().composing_at = now;
        }
        if online {
            self.client()
                .presence()
                .set_available()
                .await
                .map_err(|_| protocol_error("whatsapp_presence_failed"))?;
            if self.active() {
                let mut state = self.0.state.lock().unwrap();
                state.last_online = Some(now);
                state.online = Some(true);
            }
        }
        Ok(())
    }
}
impl RuntimeSocket for WhatsAppSocket {
    fn bind(&self, callbacks: RuntimeCallbacks) {
        *self.0.callbacks.lock().unwrap() = Some(Arc::new(callbacks));
    }
    fn start(&self) {
        let _lifecycle = self.0.lifecycle.lock().unwrap();
        if self.0.state.lock().unwrap().stopped {
            return;
        }
        let Some(bot) = self.0.bot.lock().unwrap().take() else {
            return;
        };
        self.0.state.lock().unwrap().name = "connecting";
        *self.0.handle.lock().unwrap() = Some(bot.spawn());
    }
    fn state(&self) -> JsString {
        self.0.state.lock().unwrap().name.into()
    }
    fn status(&self) -> Json {
        let state = self.0.state.lock().unwrap();
        Json::object([
            ("state", state.name.into()),
            ("connected", state.connected.into()),
            ("online", state.online.map(Json::Bool).unwrap_or(Json::Null)),
            ("privacy", state.privacy.clone()),
            (
                "lastOnlinePresenceAt",
                state
                    .last_online
                    .and_then(|n| date_to_iso(n as f64).ok())
                    .map(Json::String)
                    .unwrap_or(Json::Null),
            ),
            (
                "errorCode",
                state.error.as_deref().map(Json::from).unwrap_or(Json::Null),
            ),
            (
                "lastSendMs",
                state
                    .last_send_ms
                    .map(|n| Json::Number(n as f64))
                    .unwrap_or(Json::Null),
            ),
        ])
    }
    fn flush(&self) -> ServiceFuture<()> {
        let owner = self.clone();
        Box::pin(async move { owner.flush_outputs().await })
    }
    fn read_receipts(&self) -> ServiceFuture<()> {
        let owner = self.clone();
        Box::pin(async move {
            if let Err(error) = owner.send_read_receipts().await {
                owner.error(&error.code);
            }
            Ok(())
        })
    }
    fn set_busy(&self, jids: Vec<JsString>) {
        let mut s = self.0.state.lock().unwrap();
        s.busy = jids.into_iter().filter_map(|j| j.to_utf8().ok()).collect();
        s.busy_at = self.0.clock.utc_millis();
    }
    fn presence(&self) -> ServiceFuture<()> {
        let owner = self.clone();
        Box::pin(async move {
            if let Err(error) = owner.send_presence().await {
                owner.error(&error.code);
            }
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<()> {
        let mut closing = self.0.closing.lock().unwrap();
        if let Some(receive) = &*closing {
            return Box::pin(wait_closed(receive.clone()));
        }
        let _lifecycle = self.0.lifecycle.lock().unwrap();
        let (done, receive) = watch::channel(None);
        *closing = Some(receive.clone());
        {
            let mut state = self.0.state.lock().unwrap();
            state.stopped = true;
            state.connected = false;
            state.name = "stopped";
        }
        let handle = self.0.handle.lock().unwrap().take();
        self.0.bot.lock().unwrap().take();
        let owner = self.clone();
        tokio::spawn(async move {
            if let Some(handle) = handle {
                handle.shutdown().await;
            } else {
                owner.client().disconnect().await;
            }
            let _inbound = owner.0.inbound.lock().await;
            let _output = owner.0.output.lock().await;
            let _reading = owner.0.reading.lock().await;
            let _presence = owner.0.presence.lock().await;
            let signal = owner
                .client()
                .flush_pending_signal_state()
                .await
                .map_err(|_| storage_error());
            let device = owner
                .client()
                .persistence_manager()
                .flush()
                .await
                .map_err(|_| storage_error());
            let journal = owner.0.journal.flush().await;
            let account = owner.0.backend.close();
            done.send_replace(Some(signal.and(device).and(journal).and(account)));
        });
        Box::pin(wait_closed(receive))
    }
}
async fn wait_closed(mut receive: watch::Receiver<Option<IvyResult<()>>>) -> IvyResult<()> {
    loop {
        if let Some(result) = receive.borrow_and_update().clone() {
            return result;
        }
        receive.changed().await.map_err(|_| IvyError::internal())?;
    }
}
