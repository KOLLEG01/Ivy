//! Private source candidate, included only by synthetic integration tests.
//! The inert wire is explicit: no production socket constructor is selected.
use super::candidate::{CandidateAccount, CandidateEvent, Reply, store::Grant};
use ivy_chat_bridge::whatsapp::{
    identity::identity,
    journal::{Outgoing, OutputState},
    journal_io::StorageUnavailable,
    journal_owner::Journal,
    runtime::{RuntimeCallbacks, RuntimeSocket},
    sender,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{parse, stringify},
};
use ivy_sdk::{
    clock::{Clock, date_to_iso, parse_date},
    service::ServiceFuture,
};
use rusqlite::{OptionalExtension, types::Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;

type Completion = watch::Receiver<Option<IvyResult<()>>>;
type Pairing = Arc<dyn Fn(Json) -> ServiceFuture<()> + Send + Sync>;
struct State {
    stopped: bool,
    generation: u64,
    connected: bool,
    phase: String,
    error: Option<String>,
    online: Option<bool>,
    privacy: Json,
    last_online: Option<i64>,
    last_send: Option<f64>,
    busy: BTreeSet<String>,
    busy_at: i64,
    composing: BTreeSet<String>,
    composing_at: i64,
    backoff: u64,
    retry: Option<tokio::task::AbortHandle>,
    heartbeat: Option<tokio::task::AbortHandle>,
    jobs: BTreeMap<&'static str, Completion>,
    close: Option<Completion>,
    callbacks: Option<RuntimeCallbacks>,
}
struct Inner {
    owner: CandidateAccount,
    journal: Journal,
    config: Json,
    clock: Arc<dyn Clock>,
    pairing: Pairing,
    fixture_identity: String,
    state: Mutex<State>,
    events: tokio::sync::Mutex<()>,
}
#[derive(Clone)]
pub struct CandidateSocket(Arc<Inner>);
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn text(v: &Json, k: &str) -> String {
    field(v, k).as_str().unwrap_or_default()
}
fn number(v: &Json) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().ok().and_then(|s| s.parse().ok()))
        .unwrap_or(f64::NAN)
}
fn encoded(v: &Json) -> IvyResult<String> {
    stringify(v, usize::MAX)
}
fn normalized(jid: &str) -> String {
    // Supplied library jidDecode/jidNormalizedUser: drop device/agent, normalize c.us.
    let Some((local, server)) = jid.split_once('@') else {
        return String::new();
    };
    let user = local
        .split(':')
        .next()
        .unwrap_or("")
        .split('_')
        .next()
        .unwrap_or("");
    format!(
        "{user}@{}",
        if server == "c.us" {
            "s.whatsapp.net"
        } else {
            server
        }
    )
}
async fn observe(mut rx: Completion) -> IvyResult<()> {
    loop {
        if let Some(v) = rx.borrow().clone() {
            return v;
        }
        rx.changed().await.map_err(|_| IvyError::internal())?;
    }
}
fn decoded(reply: Reply) -> IvyResult<Json> {
    match reply {
        Reply::Value(v) => parse(&v, usize::MAX),
        Reply::LibraryError(_) => Err(IvyError::new(
            "whatsapp_protocol_failed",
            "The original protocol method failed.",
        )),
    }
}
impl CandidateSocket {
    pub fn for_inert_source_fixture(
        owner: CandidateAccount,
        journal: Journal,
        config: Json,
        fixture_identity: String,
        clock: Arc<dyn Clock>,
        pairing: Pairing,
    ) -> Self {
        let socket = Self(Arc::new(Inner {
            owner: owner.clone(),
            journal,
            config,
            fixture_identity,
            clock,
            pairing,
            events: tokio::sync::Mutex::new(()),
            state: Mutex::new(State {
                stopped: false,
                generation: 0,
                connected: false,
                phase: "stopped".into(),
                error: None,
                online: None,
                privacy: Json::Null,
                last_online: None,
                last_send: None,
                busy: BTreeSet::new(),
                busy_at: 0,
                composing: BTreeSet::new(),
                composing_at: 0,
                backoff: 1000,
                retry: None,
                heartbeat: None,
                jobs: BTreeMap::new(),
                close: None,
                callbacks: None,
            }),
        }));
        let weak = Arc::downgrade(&socket.0);
        owner.set_event_sink(Arc::new(move |event| {
            let weak = weak.clone();
            Box::pin(async move {
                let socket = CandidateSocket(weak.upgrade().ok_or_else(IvyError::internal)?);
                socket.event(event).await
            })
        }));
        socket
    }
    fn error(&self, code: &str) {
        self.0.state.lock().unwrap().error = Some(code.into());
    }
    fn wake(&self) {
        let callback = self
            .0
            .state
            .lock()
            .unwrap()
            .callbacks
            .as_ref()
            .map(|c| c.wake.clone());
        if let Some(c) = callback {
            c();
        }
    }
    fn active(&self, generation: u64) -> bool {
        let s = self.0.state.lock().unwrap();
        !s.stopped && s.generation == generation && s.connected
    }
    fn allowed(&self) -> bool {
        !self.0.state.lock().unwrap().stopped && self.0.owner.accepting()
    }
    // Every spawned policy job is owned independently of its returned observer.
    // Separate lanes avoid serializing send, event callbacks and presence globally.
    fn job(&self, name: &'static str, work: ServiceFuture<()>) -> ServiceFuture<()> {
        let mut state = self.0.state.lock().unwrap();
        if state.stopped {
            return Box::pin(async { Ok(()) });
        }
        if let Some(rx) = state.jobs.get(name) {
            return Box::pin(observe(rx.clone()));
        }
        let (done, rx) = watch::channel(None);
        state.jobs.insert(name, rx.clone());
        let socket = self.clone();
        tokio::spawn(async move {
            let result = work.await;
            socket.0.state.lock().unwrap().jobs.remove(name);
            done.send_replace(Some(result));
        });
        Box::pin(observe(rx))
    }
    async fn wire(&self, method: &str, args: Json) -> IvyResult<Json> {
        decoded(self.0.owner.call("outbound", method, args).await?.1)
    }
    pub fn connect(&self) -> ServiceFuture<()> {
        let socket = self.clone();
        self.job(
            "connect",
            Box::pin(async move { socket.connect_owned().await }),
        )
    }
    async fn connect_owned(&self) -> IvyResult<()> {
        if !self.allowed() {
            return Ok(());
        }
        let generation = {
            let mut s = self.0.state.lock().unwrap();
            s.generation = s.generation.checked_add(1).ok_or_else(IvyError::internal)?;
            s.phase = "connecting".into();
            s.generation
        };
        let result = self
            .0
            .owner
            .call(
                "socket-open",
                "open",
                Json::object([("user", self.0.fixture_identity.as_str().into())]),
            )
            .await;
        let result = match result.and_then(|(_, r)| decoded(r)) {
            Ok(v) => v,
            Err(e) => {
                self.reconnect();
                return Err(e);
            }
        };
        // Explicit fixture guard prevents this candidate from adopting a real constructor.
        if result.get("synthetic") != Some(&Json::from(true)) {
            return Err(IvyError::internal());
        }
        if !self.allowed() || self.0.state.lock().unwrap().generation != generation {
            return Ok(());
        }
        if sender::jid_phone(&text(&result, "user"))
            != sender::phone(&text(&self.0.config, "botPhoneNumber"))
        {
            let mut s = self.0.state.lock().unwrap();
            s.phase = "account_mismatch".into();
            s.error = Some("whatsapp_account_mismatch".into());
            return Ok(());
        }
        {
            let mut s = self.0.state.lock().unwrap();
            s.connected = true;
            s.phase = "connected".into();
            s.error = None;
            s.backoff = 1000;
            s.last_online = None;
            s.online = result.get("online").and_then(Json::as_bool);
        }
        (self.0.pairing)(Json::object([
            ("qr", Json::Null),
            ("expiresAt", Json::Null),
        ]))
        .await?;
        if self.active(generation) && self.allowed() {
            if let Ok(privacy) = self.wire("privacy", Json::Null).await {
                self.0.state.lock().unwrap().privacy = privacy;
            }
        }
        if !self.active(generation) {
            return Ok(());
        }
        self.heartbeat();
        drop(self.presence());
        self.wake();
        Ok(())
    }
    fn heartbeat(&self) {
        let weak = Arc::downgrade(&self.0);
        let mut s = self.0.state.lock().unwrap();
        if s.stopped {
            return;
        }
        if let Some(t) = s.heartbeat.take() {
            t.abort();
        }
        let task = tokio::spawn(async move {
            let mut timer = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_secs(5),
                Duration::from_secs(5),
            );
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let Some(inner) = weak.upgrade() else { break };
                drop(CandidateSocket(inner).presence());
            }
        });
        s.heartbeat = Some(task.abort_handle());
    }
    fn reconnect(&self) {
        let weak = Arc::downgrade(&self.0);
        let mut s = self.0.state.lock().unwrap();
        s.connected = false;
        s.busy.clear();
        s.composing.clear();
        s.composing_at = 0;
        if let Some(t) = s.heartbeat.take() {
            t.abort();
        }
        if s.stopped || matches!(s.phase.as_str(), "logged_out" | "account_mismatch") {
            return;
        }
        if let Some(t) = s.retry.take() {
            t.abort();
        }
        s.phase = "reconnecting".into();
        let delay = s.backoff;
        s.backoff = (s.backoff * 2).min(60000);
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            if let Some(inner) = weak.upgrade() {
                drop(CandidateSocket(inner).connect());
            }
        });
        s.retry = Some(task.abort_handle());
    }
    async fn event(&self, event: CandidateEvent) -> IvyResult<Json> {
        let _ordered = self.0.events.lock().await;
        if text(&event.value, "type") == "journal.getMessage" {
            return self.get_message(field(&event.value, "key"));
        }
        let generation = self.0.state.lock().unwrap().generation;
        if !self.active(generation) {
            return Ok(Json::Null);
        }
        let result = match text(&event.value, "type").as_str() {
            "messages.upsert"
                if matches!(
                    text(&event.value, "eventType").as_str(),
                    "notify" | "append"
                ) =>
            {
                for message in field(&event.value, "messages").as_array().unwrap_or(&[]) {
                    self.inbound(message, &event.operation, generation).await?;
                }
                Ok(())
            }
            "messages.update" => self.updates(field(&event.value, "updates")),
            "creds.update" => {
                if self.allowed() {
                    decoded(
                        self.0
                            .owner
                            .call_within(
                                event.operation,
                                "credential-save",
                                "credentials",
                                field(&event.value, "creds").clone(),
                            )
                            .await?
                            .1,
                    )?;
                }
                Ok(())
            }
            "connection.update" => self.connection_event(field(&event.value, "update")).await,
            _ => Ok(()),
        };
        if result.is_err() {
            self.error("whatsapp_inbound_failed");
        }
        result.map(|_| Json::Null)
    }
    async fn connection_event(&self, update: &Json) -> IvyResult<()> {
        if let Some(online) = update.get("isOnline").and_then(Json::as_bool) {
            self.0.state.lock().unwrap().online = Some(online);
        }
        if update.get("qr").is_some_and(|v| !v.is_null()) {
            self.0.state.lock().unwrap().phase = "awaiting_qr".into();
            (self.0.pairing)(Json::object([
                ("qr", field(update, "qr").clone()),
                (
                    "expiresAt",
                    Json::String(date_to_iso((self.0.clock.utc_millis() + 60000) as f64)?),
                ),
            ]))
            .await?;
        }
        if text(update, "connection") == "close" {
            let logged_out = update.get("loggedOut") == Some(&Json::from(true));
            let mut s = self.0.state.lock().unwrap();
            s.online = Some(false);
            s.error = Some(
                if logged_out {
                    "whatsapp_logged_out"
                } else {
                    "whatsapp_disconnected"
                }
                .into(),
            );
            if logged_out {
                s.phase = "logged_out".into();
            }
            drop(s);
            self.reconnect();
        }
        Ok(())
    }
    fn get_message(&self, key: &Json) -> IvyResult<Json> {
        if text(key, "id").is_empty() || text(key, "remoteJid").is_empty() {
            return Ok(Json::Null);
        }
        self.0.journal.access(|j| {
            let row: Option<(String, String)> = j
                .connection()
                .query_row(
                    "SELECT jid,text FROM outgoing WHERE id=?",
                    [text(key, "id")],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(StorageUnavailable::from)?;
            Ok(row
                .filter(|(jid, _)| normalized(jid) == normalized(&text(key, "remoteJid")))
                .map(|(_, body)| Json::object([("conversation", body.into())]))
                .unwrap_or(Json::Null))
        })
    }
    async fn inbound(&self, message: &Json, parent: &Grant, generation: u64) -> IvyResult<()> {
        let k = field(message, "key");
        let (id, jid, alt, participant) = (
            text(k, "id"),
            text(k, "remoteJid"),
            text(k, "remoteJidAlt"),
            text(k, "participant"),
        );
        let key = sender::MessageKey {
            id: &id,
            remote_jid: &jid,
            remote_jid_alt: &alt,
            from_me: k.get("fromMe") == Some(&Json::from(true)),
            participant: &participant,
        };
        if !sender::eligible(&key) {
            return Ok(());
        }
        let mapped = if sender::needs_lid_mapping(&key) {
            if !self.allowed() {
                return Ok(());
            }
            Some(decoded(
                self.0
                    .owner
                    .call_within(
                        parent.clone(),
                        "repository",
                        "invoke",
                        Json::object([
                            ("target", "lid".into()),
                            ("method", "getPNForLID".into()),
                            ("arguments", Json::Array(vec![key.remote_jid.into()])),
                        ]),
                    )
                    .await?
                    .1,
            )?)
        } else {
            None
        };
        let senders = field(&self.0.config, "senders").as_array().unwrap_or(&[]);
        let phones: Vec<_> = senders.iter().map(|s| text(s, "phoneNumber")).collect();
        let phones: Vec<_> = phones.iter().map(String::as_str).collect();
        let mapped = mapped.as_ref().and_then(|v| v.as_str().ok());
        let Some(index) = sender::admitted_sender(&key, mapped.as_deref(), &phones) else {
            return Ok(());
        };
        if !self.active(generation) {
            return Ok(());
        }
        let at = number(field(message, "messageTimestamp")) * 1000.;
        let now = self.0.clock.utc_millis() as f64;
        let since = parse_date(&text(&self.0.config, "since").into());
        if !at.is_finite()
            || at < since.max(now - 7. * 86400000.)
            || at > now + 300000.
            || field(message, "message").is_null()
        {
            return Ok(());
        }
        let raw = encoded(message)?;
        if raw.len() > 1024 * 1024 {
            self.error("whatsapp_message_too_large");
            return Ok(());
        }
        let sender = &senders[index];
        let id = identity(&[
            field(&self.0.config, "accountId").clone(),
            field(sender, "channelId").clone(),
            field(k, "id").clone(),
        ])?;
        let fingerprint = identity(&[encoded(field(message, "message"))?.into()])?;
        let conflict = self.0.journal.access(|j| {
            let prior: Option<(String, String)> = j
                .connection()
                .query_row("SELECT sender,message FROM inbox WHERE id=?", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()
                .map_err(StorageUnavailable::from)?;
            if let Some((principal, old)) = prior {
                let previous = j.get(&format!("inbox-content:{id}"))?;
                let previous = if previous.is_null() {
                    identity(&[encoded(field(&parse(&old, usize::MAX)?, "message"))?.into()])?
                } else {
                    previous.as_str()?
                };
                return Ok(principal != text(sender, "principalId") || previous != fingerprint);
            }
            j.try_transaction(|j| -> IvyResult<()> {
                j.accept(
                    &id,
                    &text(sender, "principalId"),
                    key.remote_jid,
                    &raw,
                    at as i64,
                )?;
                j.set(&format!("inbox-content:{id}"), &fingerprint.into())?;
                j.set(
                    &format!("read-receipt:{id}"),
                    &Json::object([("key", k.clone()), ("done", false.into())]),
                )?;
                Ok(())
            })?;
            Ok(false)
        })?;
        if conflict {
            self.error("whatsui_identity_conflict");
        } else {
            self.wake();
        }
        Ok(())
    }
    fn updates(&self, updates: &Json) -> IvyResult<()> {
        for item in updates.as_array().unwrap_or(&[]) {
            let key = field(item, "key");
            let status = field(field(item, "update"), "status").as_f64();
            if key.get("fromMe") != Some(&Json::from(true))
                || text(key, "id").is_empty()
                || text(key, "remoteJid").is_empty()
                || !status.is_some_and(|v| v >= 2.)
            {
                continue;
            }
            let changed = self.0.journal.access(|j| {
                let jid: Option<String> = j
                    .connection()
                    .query_row(
                        "SELECT jid FROM outgoing WHERE id=?",
                        [text(key, "id")],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(StorageUnavailable::from)?;
                if jid.is_some_and(|jid| normalized(&jid) == normalized(&text(key, "remoteJid"))) {
                    j.mark(&text(key, "id"), OutputState::Sent)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            })?;
            if changed {
                self.wake();
            }
        }
        Ok(())
    }
    async fn contact_allowed(&self) -> bool {
        let callback = self
            .0
            .state
            .lock()
            .unwrap()
            .callbacks
            .as_ref()
            .map(|c| c.automatic_notifications.clone());
        match callback {
            Some(c) => match c().await {
                Ok(v) => {
                    let mut s = self.0.state.lock().unwrap();
                    if s.error.as_deref() == Some("whatsapp_contact_policy_unavailable") {
                        s.error = None;
                    }
                    v
                }
                Err(_) => {
                    self.error("whatsapp_contact_policy_unavailable");
                    false
                }
            },
            None => false,
        }
    }
    fn outgoing(&self, manual_only: bool) -> IvyResult<Vec<Outgoing>> {
        self.0.journal.access(|j|{let sql=if manual_only{"SELECT id,jid,text,state,sequence FROM outgoing WHERE state IN ('pending','sending') AND NOT EXISTS(SELECT 1 FROM kv WHERE key='out-automatic:'||outgoing.id) ORDER BY sequence LIMIT 64"}else{"SELECT id,jid,text,state,sequence FROM outgoing WHERE state IN ('pending','sending') ORDER BY sequence LIMIT 64"};
            let mut q=j.connection().prepare(sql).map_err(StorageUnavailable::from)?;let rows=q.query_map([],|r|Ok(Outgoing{id:r.get(0)?,jid:r.get(1)?,text:r.get(2)?,state:r.get(3)?,sequence:r.get(4)?})).map_err(StorageUnavailable::from)?.collect::<Result<Vec<_>,_>>().map_err(StorageUnavailable::from)?;Ok(rows)})
    }
    async fn flush_owned(&self) -> IvyResult<()> {
        let generation = self.0.state.lock().unwrap().generation;
        if !self.active(generation) {
            return Ok(());
        }
        let mut rows = self.outgoing(false)?;
        let mut blocked = BTreeSet::new();
        let mut automatic = false;
        for row in &rows {
            automatic |=
                self.0.journal.get(&format!("out-automatic:{}", row.id))? == Json::from(true);
        }
        if automatic && !self.contact_allowed().await {
            rows = self.outgoing(true)?;
        }
        for row in rows {
            if blocked.contains(&row.jid) || !self.active(generation) || !self.allowed() {
                continue;
            }
            if self.0.journal.get(&format!("out-automatic:{}", row.id))? == Json::from(true)
                && !self.contact_allowed().await
            {
                continue;
            }
            if !self.active(generation) || !self.allowed() {
                continue;
            }
            let key = format!("send-attempt:{}", row.id);
            let attempt = self.0.journal.get(&key)?;
            let count = field(&attempt, "count").as_f64().unwrap_or(0.) as u32;
            let at = field(&attempt, "at").as_f64().unwrap_or(0.);
            let now = self.0.clock.utc_millis();
            if count >= 3 {
                self.0
                    .journal
                    .access(|j| j.mark(&row.id, OutputState::Unknown))?;
                self.error("whatsapp_delivery_unknown");
                continue;
            }
            if at != 0. && (now as f64) - at < 2000. * 2f64.powi(count as i32) {
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
                    )?;
                    Ok(())
                })
            })?;
            self.0.journal.flush().await?;
            if !self.allowed() {
                self.error("whatsapp_delivery_unknown");
                break;
            }
            let started = self.0.clock.monotonic();
            let result = self
                .0
                .owner
                .call_for(
                    format!("{key}:{}", count + 1),
                    "outbound",
                    "sendMessage",
                    Json::object([
                        ("jid", row.jid.as_str().into()),
                        ("text", row.text.as_str().into()),
                        ("messageId", row.id.as_str().into()),
                    ]),
                )
                .await
                .and_then(|(_, r)| decoded(r));
            if result.is_ok() {
                self.0.state.lock().unwrap().last_send = Some(started.elapsed().as_millis() as f64);
                self.0
                    .journal
                    .access(|j| j.mark(&row.id, OutputState::Sent))?;
            } else {
                blocked.insert(row.jid);
                self.error("whatsapp_delivery_unknown");
            }
        }
        let unresolved = self.0.journal.access(|j| {
            Ok(j.connection()
                .query_row(
                    "SELECT 1 FROM outgoing WHERE state IN ('sending','unknown') LIMIT 1",
                    [],
                    |_| Ok(()),
                )
                .optional()
                .map_err(StorageUnavailable::from)?
                .is_some())
        })?;
        let mut s = self.0.state.lock().unwrap();
        if !unresolved && s.error.as_deref() == Some("whatsapp_delivery_unknown") {
            s.error = None;
        }
        Ok(())
    }
    async fn receipts_owned(&self) -> IvyResult<()> {
        let generation = self.0.state.lock().unwrap().generation;
        if !self.active(generation) || !self.allowed() {
            return Ok(());
        }
        self.0.journal.flush().await?;
        let rows=self.0.journal.access(|j|{let mut q=j.connection().prepare("SELECT key,value FROM kv WHERE key LIKE 'read-receipt:%' AND json_extract(value,'$.done')=0 LIMIT 32").map_err(StorageUnavailable::from)?;let rows=q.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(StorageUnavailable::from)?.collect::<Result<Vec<_>,_>>().map_err(StorageUnavailable::from)?;Ok(rows)})?;
        for (key, value) in rows {
            if !self.active(generation) || !self.allowed() {
                break;
            }
            let value = parse(&value, usize::MAX)?;
            if self
                .wire(
                    "readMessages",
                    Json::Array(vec![field(&value, "key").clone()]),
                )
                .await
                .is_err()
            {
                self.error("whatsapp_read_receipt_failed");
                return Ok(());
            }
            self.0.journal.set(&key, &Json::Null)?;
        }
        let mut s = self.0.state.lock().unwrap();
        if s.error.as_deref() == Some("whatsapp_read_receipt_failed") {
            s.error = None;
        }
        Ok(())
    }
    async fn presence_owned(&self) -> IvyResult<()> {
        let (generation, active, composing, refresh, online) = {
            let s = self.0.state.lock().unwrap();
            if s.stopped || !s.connected {
                return Ok(());
            }
            let now = self.0.clock.utc_millis();
            (
                s.generation,
                if now - s.busy_at < 20000 {
                    s.busy.clone()
                } else {
                    BTreeSet::new()
                },
                s.composing.clone(),
                now - s.composing_at >= 4000,
                s.last_online.is_none_or(|at| {
                    now - at
                        >= field(&self.0.config, "heartbeatMs")
                            .as_f64()
                            .unwrap_or(120000.) as i64
                }),
            )
        };
        let result = async {
            for jid in composing.difference(&active) {
                if !self.active(generation) || !self.allowed() {
                    return Ok(());
                }
                self.wire(
                    "presence",
                    Json::object([("state", "paused".into()), ("jid", jid.as_str().into())]),
                )
                .await?;
                self.0.state.lock().unwrap().composing.remove(jid);
            }
            for jid in &active {
                if refresh || !composing.contains(jid) {
                    if !self.active(generation) || !self.allowed() {
                        return Ok(());
                    }
                    self.wire(
                        "presence",
                        Json::object([("state", "composing".into()), ("jid", jid.as_str().into())]),
                    )
                    .await?;
                    self.0.state.lock().unwrap().composing.insert(jid.clone());
                }
            }
            if !active.is_empty() && refresh {
                self.0.state.lock().unwrap().composing_at = self.0.clock.utc_millis();
            }
            if online && self.active(generation) && self.allowed() {
                self.wire("presence", Json::object([("state", "available".into())]))
                    .await?;
                if self.active(generation) {
                    self.0.state.lock().unwrap().last_online = Some(self.0.clock.utc_millis());
                }
            }
            Ok::<_, IvyError>(())
        }
        .await;
        if result.is_err() {
            self.error("whatsapp_presence_failed");
        }
        Ok(())
    }
}
impl RuntimeSocket for CandidateSocket {
    fn bind(&self, callbacks: RuntimeCallbacks) {
        self.0.state.lock().unwrap().callbacks = Some(callbacks);
    }
    fn start(&self) {
        drop(self.connect());
    }
    fn state(&self) -> JsString {
        self.0.state.lock().unwrap().phase.as_str().into()
    }
    fn status(&self) -> Json {
        let s = self.0.state.lock().unwrap();
        Json::object([
            ("state", s.phase.as_str().into()),
            ("connected", s.connected.into()),
            (
                "errorCode",
                s.error.as_deref().map(Json::from).unwrap_or(Json::Null),
            ),
            ("online", s.online.map(Json::from).unwrap_or(Json::Null)),
            ("privacy", s.privacy.clone()),
            (
                "lastOnlinePresenceAt",
                s.last_online
                    .and_then(|at| date_to_iso(at as f64).ok())
                    .map(Json::String)
                    .unwrap_or(Json::Null),
            ),
            (
                "lastSendMs",
                s.last_send.map(Json::Number).unwrap_or(Json::Null),
            ),
        ])
    }
    fn flush(&self) -> ServiceFuture<()> {
        let socket = self.clone();
        self.job("flush", Box::pin(async move { socket.flush_owned().await }))
    }
    fn read_receipts(&self) -> ServiceFuture<()> {
        let socket = self.clone();
        self.job(
            "read",
            Box::pin(async move { socket.receipts_owned().await }),
        )
    }
    fn set_busy(&self, jids: Vec<JsString>) {
        let mut s = self.0.state.lock().unwrap();
        s.busy = jids
            .into_iter()
            .map(|s| String::from_utf16_lossy(&s.0))
            .collect();
        s.busy_at = self.0.clock.utc_millis();
    }
    fn presence(&self) -> ServiceFuture<()> {
        let socket = self.clone();
        self.job(
            "presence",
            Box::pin(async move { socket.presence_owned().await }),
        )
    }
    fn close(&self) -> ServiceFuture<()> {
        let mut s = self.0.state.lock().unwrap();
        if let Some(rx) = &s.close {
            return Box::pin(observe(rx.clone()));
        }
        s.stopped = true;
        s.generation = s.generation.saturating_add(1);
        s.connected = false;
        s.phase = "stopped".into();
        if let Some(t) = s.retry.take() {
            t.abort();
        }
        if let Some(t) = s.heartbeat.take() {
            t.abort();
        }
        let jobs = s.jobs.values().cloned().collect::<Vec<_>>();
        let (done, rx) = watch::channel(None);
        s.close = Some(rx.clone());
        let cleanup = self.0.owner.quiesce_settled();
        tokio::spawn(async move {
            let result = cleanup.await;
            for job in jobs {
                let _ = observe(job).await;
            }
            done.send_replace(Some(result));
        });
        Box::pin(observe(rx))
    }
}
