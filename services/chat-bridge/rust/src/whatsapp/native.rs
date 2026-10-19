//! Direct Main session and verified native history. Cached work retains its
//! original owner and result; it never replaces a current dispatch guard.
use super::{
    command_native::{
        CommandOwner, CommandParameters, CommandSession, NativeCommandDispatcher, NativeCommands,
        RandomRetryIdentity, RetryIdentity,
    },
    commands::CommandMain,
    identity::identity,
    journal_owner::Journal,
    messages::Translator,
};
use crate::{
    chat_main::ChatMain,
    generation::ChatGenerationLease,
    input_media::admitted_native_input,
    main_context::{main_turn_context, spread, truthy},
    native_plan::chat_turn_options,
    store::{Document, Pin, Reference},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::hash_json,
    json::{canonical, try_js_string},
};
use ivy_sdk::{
    clock::{Clock, SystemClock},
    native_observations::NativeObservations,
    native_operation::NativeOperationCall,
    native_owner::{GuardFuture, NativeDispatchGuard, NativeOwner},
    native_turn_contents::{
        NativeTurnOwner, read_native_turn_contents, verify_native_turn_contents,
    },
    operation_ids::scoped_operation_id,
    rpc::RequestFuture,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

pub trait SessionMain: CommandMain {
    fn change_version(&self) -> u64;
    fn catalogs(&self) -> &CheckedNativeContracts;
    fn instructions(&self) -> &JsString;
    fn admit(&self, sender: &JsString, request: &Json) -> IvyResult<()>;
    fn input<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a>;
}
fn document(value: Document) -> Json {
    Json::object([("pin", value.pin.json()), ("value", value.value)])
}
impl CommandMain for ChatGenerationLease {
    fn find(&self) -> RequestFuture<'_> {
        Box::pin(async {
            let native = self.native()?;
            let operations = self.operations()?;
            let main = ChatMain::new(&operations, &native, &self.config().notice_instructions)?;
            Ok(main.find().await?.map(document).unwrap_or(Json::Null))
        })
    }
    fn binding(&self, reference: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            let pin = Pin {
                object_id: reference.required("objectId")?.as_str()?,
                revision: reference
                    .required("revision")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)? as i64,
            };
            Ok(document(
                self.store()
                    .read("chat-bridge/binding", Reference::Pinned(&pin))
                    .await
                    .map_err(|e| e.into_ivy())?,
            ))
        })
    }
    fn plan(&self) -> RequestFuture<'_> {
        Box::pin(async { self.native()?.resolve_plan() })
    }
    fn expected(&self, sender: &JsString) -> Json {
        self.config().admission.expected(sender)
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            Ok(Json::String(
                scoped_operation_id(self.store().client.clone(), &scope).await?,
            ))
        })
    }
    fn create<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async {
            let native = self.native()?;
            let operations = self.operations()?;
            ChatMain::new(&operations, &native, &self.config().notice_instructions)?
                .create(sender, request)
                .await
        })
    }
}
impl SessionMain for ChatGenerationLease {
    fn change_version(&self) -> u64 {
        self.store().change_version()
    }
    fn catalogs(&self) -> &CheckedNativeContracts {
        &self.config().catalogs
    }
    fn instructions(&self) -> &JsString {
        &self.config().instructions
    }
    fn admit(&self, sender: &JsString, request: &Json) -> IvyResult<()> {
        self.config().admission.send(sender, request).map(|_| ())
    }
    fn input<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async {
            Ok(Json::Array(
                admitted_native_input(
                    &self.config().admission,
                    self.store().client.as_ref(),
                    sender,
                    request,
                )
                .await?
                .input,
            ))
        })
    }
}
pub trait SessionOwner: CommandOwner + NativeTurnOwner {
    fn status(&self) -> RequestFuture<'_>;
    fn observation<'a>(&'a self, method: &'a str, params: Json) -> RequestFuture<'a>;
}
impl SessionOwner for NativeOwner {
    fn status(&self) -> RequestFuture<'_> {
        Box::pin(NativeOwner::status(self))
    }
    fn observation<'a>(&'a self, method: &'a str, params: Json) -> RequestFuture<'a> {
        Box::pin(async move { NativeOwner::read(self, &method.into(), params, None, true).await })
    }
}
pub trait SessionIdentity: Send + Sync {
    fn next(&self, kind: &str) -> JsString;
}
pub struct RandomSessionIdentity;
impl SessionIdentity for RandomSessionIdentity {
    fn next(&self, kind: &str) -> JsString {
        format!("wa-{kind}-{}", uuid::Uuid::new_v4()).into()
    }
}

// The spawned work owns the generation lease. Dropping an awaiting caller does
// not cancel an accepted read/resume or make a second caller repeat it.
struct Pending {
    result: watch::Receiver<Option<IvyResult<Json>>>,
}
impl Pending {
    fn new() -> (Arc<Self>, watch::Sender<Option<IvyResult<Json>>>) {
        let (sender, result) = watch::channel(None);
        (Arc::new(Self { result }), sender)
    }
    async fn wait(&self) -> IvyResult<Json> {
        let mut result = self.result.clone();
        loop {
            if let Some(value) = result.borrow_and_update().clone() {
                return value;
            }
            result.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}
struct CompletedTurn {
    key: String,
    epoch: Json,
    stamp: String,
    turn: Json,
    bytes: usize,
}
#[derive(Default)]
struct State {
    version: Option<u64>,
    current: Option<Arc<Pending>>,
    prepared: Option<(Json, Arc<Pending>)>,
    host: Option<Arc<Pending>>,
    busy: bool,
    busy_revision: u64,
    auth_retry_at: i64,
    completed_turns: VecDeque<CompletedTurn>,
    last_dispatch_ms: Option<u64>,
}
struct Inner {
    main: Arc<dyn SessionMain>,
    owner: Arc<dyn SessionOwner>,
    journal: Journal,
    translator: Translator,
    clock: Arc<dyn Clock>,
    identities: Arc<dyn SessionIdentity>,
    retry_identities: Arc<dyn RetryIdentity>,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct WhatsAppNative(Arc<Inner>);
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &Json, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn concat(prefix: &str, value: &JsString) -> JsString {
    JsString(
        prefix
            .encode_utf16()
            .chain(value.0.iter().copied())
            .collect(),
    )
}
fn error_is(reply: &Json, message: &JsString) -> bool {
    f(f(reply, "error"), "code").as_f64() == Some(-32600.0)
        && f(f(reply, "error"), "message").as_js_string() == Some(message)
}
impl WhatsAppNative {
    pub fn new(
        main: Arc<ChatGenerationLease>,
        journal: Journal,
        translator: Translator,
    ) -> IvyResult<Self> {
        let owner = Arc::new(main.native()?.native_owner()?);
        Ok(Self::with_dependencies(
            main,
            owner,
            journal,
            translator,
            Arc::new(SystemClock),
            Arc::new(RandomSessionIdentity),
            Arc::new(RandomRetryIdentity),
        ))
    }
    pub fn with_dependencies(
        main: Arc<dyn SessionMain>,
        owner: Arc<dyn SessionOwner>,
        journal: Journal,
        translator: Translator,
        clock: Arc<dyn Clock>,
        identities: Arc<dyn SessionIdentity>,
        retry_identities: Arc<dyn RetryIdentity>,
    ) -> Self {
        Self(Arc::new(Inner {
            main,
            owner,
            journal,
            translator,
            clock,
            identities,
            retry_identities,
            state: Mutex::new(State::default()),
        }))
    }
    fn translated(&self, code: &str, key: &str) -> IvyError {
        match self.0.translator.translate(key, None) {
            Ok(message) => IvyError::new(code, "").with_js_message(message),
            Err(error) => error,
        }
    }
    pub fn owner(&self) -> &dyn SessionOwner {
        self.0.owner.as_ref()
    }
    pub fn last_dispatch_ms(&self) -> Option<u64> {
        self.0.state.lock().unwrap().last_dispatch_ms
    }
    pub fn set_busy(&self, busy: bool) {
        let mut state = self.0.state.lock().unwrap();
        state.busy = busy;
        state.busy_revision += 1;
    }
    pub fn invalidate(&self, connection_changed: bool) {
        let mut state = self.0.state.lock().unwrap();
        state.current = None;
        if connection_changed {
            state.prepared = None;
            state.host = None;
            state.busy = false;
        }
    }
    pub async fn same_host(&self, host_id: &JsString) -> IvyResult<bool> {
        let pending = {
            let mut state = self.0.state.lock().unwrap();
            state
                .host
                .get_or_insert_with(|| {
                    let (pending, sender) = Pending::new();
                    let this = self.clone();
                    let work = pending.clone();
                    tokio::spawn(async move {
                        let result = async {
                            Ok(Json::String(text(&this.0.owner.status().await?, "hostId")?))
                        }
                        .await;
                        if result.is_err() {
                            let mut state = this.0.state.lock().unwrap();
                            if state.host.as_ref().is_some_and(|p| Arc::ptr_eq(p, &work)) {
                                state.host = None;
                            }
                        }
                        sender.send_replace(Some(result));
                    });
                    pending
                })
                .clone()
        };
        Ok(pending.wait().await?.as_js_string() == Some(host_id))
    }
    /// The exact binding pin follows Main; a store change invalidates only this lookup.
    pub async fn bound(&self) -> IvyResult<Json> {
        let pending = {
            let mut state = self.0.state.lock().unwrap();
            let version = self.0.main.change_version();
            if state.version != Some(version) {
                state.current = None;
                state.version = Some(version);
            }
            state
                .current
                .get_or_insert_with(|| {
                    let (pending, sender) = Pending::new();
                    let this = self.clone();
                    let work = pending.clone();
                    tokio::spawn(async move {
                        let result = async {
                            let main = this.0.main.find().await?;
                            let pin = f(f(&main, "value"), "binding");
                            if !truthy(pin) {
                                return Err(
                                    this.translated("chat_main_missing", "main.first_message")
                                );
                            }
                            let binding = this.0.main.binding(pin.clone()).await?;
                            Ok(Json::object([("main", main), ("binding", binding)]))
                        }
                        .await;
                        if result.is_err() {
                            let mut state = this.0.state.lock().unwrap();
                            if state
                                .current
                                .as_ref()
                                .is_some_and(|p| Arc::ptr_eq(p, &work))
                            {
                                state.current = None;
                            }
                        }
                        sender.send_replace(Some(result));
                    });
                    pending
                })
                .clone()
        };
        pending.wait().await
    }
    pub async fn prepare(&self) -> IvyResult<()> {
        let bound = self.bound().await?;
        let binding = f(&bound, "binding").clone();
        let id = f(f(&binding, "pin"), "objectId").clone();
        let pending = {
            let mut state = self.0.state.lock().unwrap();
            if state
                .prepared
                .as_ref()
                .is_none_or(|(saved, _)| saved != &id)
            {
                let (pending, sender) = Pending::new();
                let this = self.clone();
                let work = pending.clone();
                let revision = state.busy_revision;
                tokio::spawn(async move {
                    let result = this.resume(&binding, revision).await.map(|_| Json::Null);
                    if result.is_err() {
                        let mut state = this.0.state.lock().unwrap();
                        if state
                            .prepared
                            .as_ref()
                            .is_some_and(|(_, p)| Arc::ptr_eq(p, &work))
                        {
                            state.prepared = None;
                        }
                    }
                    sender.send_replace(Some(result));
                });
                state.prepared = Some((id, pending));
            }
            state.prepared.as_ref().unwrap().1.clone()
        };
        pending.wait().await.map(|_| ())
    }
    async fn resume(&self, binding: &Json, busy_revision: u64) -> IvyResult<()> {
        let plan = self.0.main.plan().await?;
        let bound = self.0.owner.binding(&"thread/resume".into()).await?;
        let thread = text(f(f(binding, "value"), "primary"), "nativeId")?;
        let mut params = spread(f(&plan, "threadResume"));
        params.insert("threadId", Json::String(thread.clone()));
        params.insert("excludeTurns", true.into());
        let call = NativeOperationCall {
            operation_id: self.0.identities.next("resume"),
            method: "thread/resume".into(),
            definition_hash: text(&bound, "definitionHash")?,
            params,
        };
        let response = self.0.owner.interact(&call, &Allow).await?;
        let reply = f(&response, "reply");
        let observed;
        let result = if error_is(reply, &concat("no rollout found for thread id ", &thread)) {
            observed = self
                .0
                .owner
                .observation(
                    "thread/read",
                    Json::object([
                        ("threadId", Json::String(thread.clone())),
                        ("includeTurns", false.into()),
                    ]),
                )
                .await?;
            if error_is(
                f(&observed, "reply"),
                &concat("thread not loaded: ", &thread),
            ) {
                return Err(self.translated("chat_main_unavailable", "main.unavailable"));
            }
            require(
                f(&observed, "reply").get("result").is_some(),
                "native_read_failed",
                "Main availability could not be observed.",
            )?;
            f(f(&observed, "reply"), "result")
        } else {
            require(
                reply.get("result").is_some(),
                "native_read_failed",
                "Main could not be attached to native notifications.",
            )?;
            f(reply, "result")
        };
        let contract = self.0.main.catalogs().checked_native_contract(
            &text(f(f(binding, "value"), "nativePlan"), "nativeVersion")?,
            None,
        )?;
        let observed = NativeObservations::new(&contract).native_thread_state(result)?;
        let mut state = self.0.state.lock().unwrap();
        if state.busy_revision == busy_revision {
            state.busy = !observed.can_start_turn;
        }
        Ok(())
    }
    async fn require_account(&self) -> IvyResult<()> {
        if self.0.clock.utc_millis() < self.0.state.lock().unwrap().auth_retry_at {
            return Err(self.translated("chat_auth_required", "message.auth_required"));
        }
        let binding = self.0.owner.binding(&"account/read".into()).await?;
        let call = NativeOperationCall {
            operation_id: self.0.identities.next("account"),
            method: "account/read".into(),
            definition_hash: text(&binding, "definitionHash")?,
            params: Json::object([("refreshToken", false.into())]),
        };
        let response = self.0.owner.interact(&call, &Allow).await?;
        let account = f(f(&response, "reply"), "result");
        require(
            matches!(f(account, "requiresOpenaiAuth"), Json::Bool(_)),
            "native_read_failed",
            "Native account readiness could not be read.",
        )?;
        if f(account, "requiresOpenaiAuth") == &Json::Bool(true) && !truthy(f(account, "account")) {
            self.0.state.lock().unwrap().auth_retry_at = self.0.clock.utc_millis() + 30_000;
            return Err(self.translated("chat_auth_required", "message.auth_required"));
        }
        Ok(())
    }
    /// Original admitted input and the transport intent precede any native effect.
    pub async fn send(&self, sender: &JsString, request: &Json) -> IvyResult<Json> {
        let original = request.clone();
        self.0.main.admit(sender, &original)?;
        let expected = f(&original, "expectedBinding");
        require(
            truthy(expected),
            "chat_main_missing",
            "Direct WhatsApp delivery needs the automatically selected Main binding.",
        )?;
        let id = original.required("messageId")?.as_str()?;
        let key = format!("direct-request:{id}");
        let hash = Json::from(hash_json(&Json::object([
            ("sender", Json::String(sender.clone())),
            ("request", original.clone()),
        ]))?);
        let prior = self.0.journal.get(&key)?;
        require(
            !truthy(&prior) || prior == hash,
            "whatsui_identity_conflict",
            "The admitted direct input cannot change.",
        )?;
        if !truthy(&prior) {
            self.0.journal.set(&key, &hash)?;
        }
        let bound = self.bound().await?;
        let binding = f(&bound, "binding");
        let pin = f(binding, "pin");
        if f(pin, "objectId") != f(expected, "objectId")
            && truthy(&self.0.journal.get(&format!("recover-main:{id}"))?)
        {
            self.recover_main(sender, &original).await?;
            return Ok(Json::Null);
        }
        require(
            f(pin, "objectId") == f(expected, "objectId")
                && f(pin, "revision") == f(expected, "revision"),
            "chat_binding_mismatch",
            "Main changed before input delivery.",
        )?;
        let thread = text(f(f(binding, "value"), "primary"), "nativeId")?;
        let key = format!("input:{id}");
        let result = self
            .command(
                &key,
                "turn/start",
                &InputParameters {
                    session: self,
                    sender,
                    original: &original,
                    thread,
                },
                Some(&InputGuard {
                    session: self,
                    expected,
                }),
            )
            .await;
        match result {
            Err(error) if error.code == "chat_main_unavailable" => {
                self.recover_main(sender, &original).await?;
                Ok(Json::Null)
            }
            result => result,
        }
    }
    async fn recover_main(&self, sender: &JsString, original: &Json) -> IvyResult<()> {
        let id = original.required("messageId")?.as_str()?;
        let key = format!("interaction:input:{id}");
        let attempted = self.0.journal.get(&key)?;
        let legacy = self.0.journal.get(&format!("native:input:{id}"))?;
        if truthy(f(&attempted, "attempted"))
            || truthy(f(&legacy, "attempted"))
            || attempted.get("result").is_some()
        {
            return Err(self.translated("interaction_expired", "message.expired"));
        }
        let recovery_key = format!("recover-main:{id}");
        let mut recovery = self.0.journal.get(&recovery_key)?;
        if !truthy(&recovery) {
            let current = self.0.main.find().await?;
            let binding = f(f(&current, "value"), "binding");
            let expected = f(original, "expectedBinding");
            require(
                truthy(&current) && truthy(binding) && truthy(expected),
                "native_read_pending",
                "Main recovery requires its original binding.",
            )?;
            let value = f(&current, "value");
            require(
                f(binding, "objectId") == f(expected, "objectId")
                    && f(binding, "revision") == f(expected, "revision")
                    && !truthy(f(value, "pendingAction"))
                    && f(value, "queue").as_array().is_some_and(|q| q.is_empty()),
                "native_read_pending",
                "Main changed before its missing native session could be recovered.",
            )?;
            recovery = Json::object([
                ("action", "createMain".into()),
                (
                    "operationId",
                    self.0
                        .main
                        .operation_id(Json::Array(vec![
                            "whatsapp-recover-main".into(),
                            id.clone().into(),
                            f(expected, "objectId").clone(),
                        ]))
                        .await?,
                ),
                ("expectedBridge", self.0.main.expected(sender)),
                (
                    "expectedMainRevision",
                    f(f(&current, "pin"), "revision").clone(),
                ),
                (
                    "reason",
                    "Automatically recover Main after its native session was confirmed missing."
                        .into(),
                ),
            ]);
            self.0.journal.set(&recovery_key, &recovery)?;
            self.0.journal.flush().await?;
        }
        let result = self.0.main.create(sender, &recovery).await?;
        if f(&result, "phase") != &Json::from("succeeded") {
            if f(&result, "phase") == &Json::from("failed") {
                let error = f(&result, "error");
                return Err(IvyError::new_js(
                    f(error, "code")
                        .as_js_string()
                        .cloned()
                        .unwrap_or_else(|| "chat_operation_failed".into()),
                    f(error, "detail")
                        .as_js_string()
                        .cloned()
                        .unwrap_or_else(|| "Main recovery failed.".into()),
                ));
            }
            return Err(IvyError::new(
                "native_read_pending",
                "The original Main recovery is still pending.",
            ));
        }
        let outcome = f(&result, "outcome");
        require(
            f(outcome, "action") == &Json::from("createMain"),
            "chat_native_mismatch",
            "Main recovery needs its binding receipt.",
        )?;
        let binding = f(f(outcome, "binding"), "object");
        let mut next = original.clone();
        next.insert("expectedBinding", binding.clone());
        let listener = format!("listener-new:{}", binding.required("objectId")?.as_str()?);
        self.0.journal.access(|io| {
            io.try_transaction(|io| -> IvyResult<()> {
                io.rebind_request(&id, original, &next)?;
                io.set(&key, &Json::Null)?;
                io.set(&format!("direct-request:{id}"), &Json::Null)?;
                io.set(&listener, &true.into())?;
                io.set(&recovery_key, &Json::Null)?;
                Ok(())
            })
        })?;
        self.0.journal.flush().await?;
        self.invalidate(true);
        Ok(())
    }
    pub async fn read(&self, method: &str, params: Json) -> IvyResult<Json> {
        let observed = self.0.owner.observation(method, params.clone()).await?;
        self.read_result(&observed, method, &params).await
    }
    async fn read_result(&self, observed: &Json, method: &str, params: &Json) -> IvyResult<Json> {
        let reply = f(observed, "reply");
        if method == "thread/turns/list" && params.as_object().is_some() {
            if let Some(thread) = f(params, "threadId").as_js_string() {
                let mut message = concat("thread ", thread);
                message.0.extend(" is not materialized yet; thread/turns/list is unavailable before first user message".encode_utf16());
                if error_is(reply, &message) {
                    let bound = self.bound().await?;
                    require(
                        f(f(f(f(&bound, "binding"), "value"), "primary"), "nativeId")
                            == f(params, "threadId"),
                        "chat_native_mismatch",
                        "An empty-history observation must target the bound Main.",
                    )?;
                    return Ok(Json::object([
                        ("data", Json::Array(vec![])),
                        ("nextCursor", Json::Null),
                    ]));
                }
            }
        }
        if f(f(reply, "error"), "code").as_f64() == Some(-32600.0)
            && f(f(reply, "error"), "message")
                .as_js_string()
                .is_some_and(|m| {
                    m.0.starts_with(&"thread not loaded: ".encode_utf16().collect::<Vec<_>>())
                })
        {
            let bound = self.bound().await?;
            let thread = text(f(f(f(&bound, "binding"), "value"), "primary"), "nativeId")?;
            require(
                params.as_object().is_some()
                    && f(params, "threadId").as_js_string() == Some(&thread)
                    && error_is(reply, &concat("thread not loaded: ", &thread)),
                "chat_native_mismatch",
                "Resume must target the bound Main.",
            )?;
            self.0.state.lock().unwrap().prepared = None;
            self.prepare().await?;
            return Err(IvyError::new(
                "native_read_pending",
                "The bound Main is being resumed; retry its original read.",
            ));
        }
        require(
            reply.get("result").is_some(),
            "native_read_failed",
            "Native read failed.",
        )?;
        Ok(f(reply, "result").clone())
    }
    pub async fn turn_page(&self, thread_id: &JsString, cursor: Json) -> IvyResult<Json> {
        let params = Json::object([
            ("threadId", Json::String(thread_id.clone())),
            ("cursor", cursor),
            ("limit", Json::Number(1.0)),
            ("sortDirection", "desc".into()),
            ("itemsView", "notLoaded".into()),
        ]);
        let snapshot = self
            .0
            .owner
            .observation("thread/turns/list", params.clone())
            .await?;
        let mut page = self
            .read_result(&snapshot, "thread/turns/list", &params)
            .await?;
        let data = f(&page, "data").as_array();
        require(
            data.is_some_and(|d| d.len() <= 1),
            "native_read_failed",
            "History recovery requires its one-turn metadata page.",
        )?;
        if data.unwrap().is_empty() {
            return Ok(page);
        }
        let turn = data.unwrap()[0].clone();
        let turn_id = f(&turn, "id").as_js_string().ok_or_else(|| {
            IvyError::new(
                "native_read_failed",
                "History recovery requires the original turn identity.",
            )
        })?;
        let mut metadata = spread(&turn);
        metadata.remove("items");
        metadata.remove("itemsView");
        let key = canonical(
            &Json::Array(vec![
                Json::String(thread_id.clone()),
                Json::String(turn_id.clone()),
            ]),
            usize::MAX,
        )?;
        let stamp = canonical(&metadata, usize::MAX)?;
        {
            let mut state = self.0.state.lock().unwrap();
            if let Some(saved) = state
                .completed_turns
                .iter()
                .find(|t| t.key == key && t.epoch == *f(&snapshot, "epoch") && t.stamp == stamp)
            {
                page.insert("data", Json::Array(vec![saved.turn.clone()]));
                return Ok(page);
            }
            state.completed_turns.retain(|t| t.key != key);
        }
        let owner: &dyn NativeTurnOwner = self.0.owner.as_ref();
        let contents = read_native_turn_contents(
            owner,
            self.0.main.catalogs(),
            &snapshot,
            thread_id,
            turn_id,
            None,
        )
        .await?;
        let verified = verify_native_turn_contents(
            owner,
            self.0.main.catalogs(),
            &contents,
            thread_id,
            turn_id,
        )
        .await?;
        let mut complete = spread(&verified.turn);
        complete.insert("itemsView", "full".into());
        complete.insert(
            "items",
            Json::Array(
                verified
                    .items
                    .into_iter()
                    .map(|item| -> IvyResult<Json> {
                        let kind = item
                            .get("type")
                            .map(try_js_string)
                            .transpose()?
                            .unwrap_or_else(|| "undefined".into());
                        if ["userMessage", "steeringUserMessage", "agentMessage"]
                            .iter()
                            .any(|expected| kind == JsString::from(*expected))
                        {
                            Ok(item)
                        } else {
                            let mut projected =
                                Json::object([("id", item.required("id")?.clone())]);
                            if let Some(kind) = item.get("type") {
                                projected.insert("type", kind.clone());
                            }
                            Ok(projected)
                        }
                    })
                    .collect::<IvyResult<Vec<_>>>()?,
            ),
        );
        if ["completed", "failed", "interrupted"].iter().any(|status| {
            try_js_string(f(&turn, "status")).ok().as_ref() == Some(&JsString::from(*status))
        }) && f(&turn, "completedAt").as_f64().is_some_and(f64::is_finite)
        {
            let bytes = canonical(&complete, usize::MAX)?.len();
            if bytes <= 32 * 1024 * 1024 {
                let mut state = self.0.state.lock().unwrap();
                state.completed_turns.retain(|t| t.key != key);
                while state.completed_turns.len() >= 16
                    || state.completed_turns.iter().map(|t| t.bytes).sum::<usize>() + bytes
                        > 32 * 1024 * 1024
                {
                    state.completed_turns.pop_front();
                }
                state.completed_turns.push_back(CompletedTurn {
                    key,
                    epoch: f(&snapshot, "epoch").clone(),
                    stamp,
                    turn: complete.clone(),
                    bytes,
                });
            }
        }
        page.insert("data", Json::Array(vec![complete]));
        Ok(page)
    }
}
struct InputParameters<'a> {
    session: &'a WhatsAppNative,
    sender: &'a JsString,
    original: &'a Json,
    thread: JsString,
}
impl CommandParameters for InputParameters<'_> {
    fn build(&self) -> RequestFuture<'_> {
        Box::pin(async {
            let plan = self.session.0.main.plan().await?;
            let id = self.original.required("messageId")?.as_str()?;
            let local = self.session.0.journal.get(&format!("local-input:{id}"))?;
            let payload = self.original.required("payload")?;
            let images = payload
                .required("images")?
                .as_array()
                .ok_or_else(IvyError::internal)?;
            let input = if !images.is_empty() {
                self.session
                    .0
                    .main
                    .input(self.sender, self.original)
                    .await?
            } else {
                let mut input = Vec::new();
                if truthy(f(payload, "text")) {
                    input.push(Json::object([
                        ("type", "text".into()),
                        ("text", f(payload, "text").clone()),
                    ]));
                }
                if !local.is_null() {
                    input.extend(
                        local
                            .as_array()
                            .ok_or_else(IvyError::internal)?
                            .iter()
                            .cloned(),
                    );
                }
                Json::Array(input)
            };
            let mut params = main_turn_context(
                &chat_turn_options(plan.required("turnStart")?, payload.get("turnOptions"))?,
                &"user message from WhatsApp".into(),
                self.session.0.main.instructions(),
            )?;
            params.insert("threadId", Json::String(self.thread.clone()));
            params.insert("input", input);
            params.insert(
                "clientUserMessageId",
                format!("wa-{}", identity(&[Json::from(id)])?).into(),
            );
            Ok(params)
        })
    }
}
struct InputGuard<'a> {
    session: &'a WhatsAppNative,
    expected: &'a Json,
}
impl NativeDispatchGuard for InputGuard<'_> {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async {
            self.session.prepare().await?;
            self.session.require_account().await?;
            let bound = self.session.bound().await?;
            let value = f(f(&bound, "main"), "value");
            let binding = f(value, "binding");
            require(
                f(binding, "objectId") == f(self.expected, "objectId")
                    && f(binding, "revision") == f(self.expected, "revision"),
                "chat_binding_mismatch",
                "Main changed before the original input was delivered.",
            )?;
            require(
                !truthy(f(value, "pendingAction"))
                    && f(value, "queue").as_array().is_some_and(|q| q.is_empty()),
                "native_read_pending",
                "Earlier Main work is still pending.",
            )?;
            require(
                !self.session.0.state.lock().unwrap().busy,
                "native_read_pending",
                "The native Main has an active turn.",
            )
        })
    }
}
struct Allow;
impl NativeDispatchGuard for Allow {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}
impl CommandSession for WhatsAppNative {
    fn read<'a>(&'a self, method: &'a str, params: Json) -> RequestFuture<'a> {
        Box::pin(WhatsAppNative::read(self, method, params))
    }
    fn turn_page<'a>(&'a self, thread: &'a JsString, cursor: Json) -> RequestFuture<'a> {
        Box::pin(WhatsAppNative::turn_page(self, thread, cursor))
    }
    fn busy_revision(&self) -> u64 {
        self.0.state.lock().unwrap().busy_revision
    }
    fn started(&self, observed_revision: u64) {
        let mut state = self.0.state.lock().unwrap();
        if state.busy_revision == observed_revision {
            state.busy = true;
        }
    }
    fn rejected_start(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.prepared = None;
        state.current = None;
    }
    fn invalidate(&self) {
        WhatsAppNative::invalidate(self, false)
    }
}
impl NativeCommands for WhatsAppNative {
    fn command<'a>(
        &'a self,
        key: &'a str,
        method: &'a str,
        parameters: &'a dyn CommandParameters,
        guard: Option<&'a dyn NativeDispatchGuard>,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let owner: &dyn CommandOwner = self.0.owner.as_ref();
            let dispatcher = NativeCommandDispatcher::new(
                self.0.journal.clone(),
                owner,
                self,
                self.0.clock.as_ref(),
                self.0.retry_identities.as_ref(),
                self.0.translator.clone(),
            );
            let result = dispatcher.command(key, method, parameters, guard).await;
            if let Some(ms) = dispatcher.last_dispatch_ms() {
                self.0.state.lock().unwrap().last_dispatch_ms = Some(ms);
            }
            result
        })
    }
    fn read<'a>(&'a self, method: &'a str, params: Json) -> RequestFuture<'a> {
        Box::pin(WhatsAppNative::read(self, method, params))
    }
    fn invalidate(&self) {
        WhatsAppNative::invalidate(self, false)
    }
}
