//! Original WhatsApp command intent and recovery over the real native owner.
use super::{identity::identity, journal_owner::Journal, messages::Translator};
use crate::main_context::truthy;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome, json::js_string};
use ivy_sdk::{
    clock::Clock,
    native_operation::NativeOperationCall,
    native_owner::{GuardFuture, NativeDispatchGuard, NativeOwner},
    rpc::RequestFuture,
};
use std::sync::{Arc, Mutex};

pub trait CommandParameters: Send + Sync {
    fn build(&self) -> RequestFuture<'_>;
}
pub struct ReadyParameters(pub Json);
impl CommandParameters for ReadyParameters {
    fn build(&self) -> RequestFuture<'_> {
        Box::pin(async { Ok(self.0.clone()) })
    }
}
pub trait NativeCommands: Send + Sync {
    fn command<'a>(
        &'a self,
        key: &'a str,
        method: &'a str,
        parameters: &'a dyn CommandParameters,
        guard: Option<&'a dyn NativeDispatchGuard>,
    ) -> RequestFuture<'a>;
    fn read<'a>(&'a self, method: &'a str, params: Json) -> RequestFuture<'a>;
    fn invalidate(&self);
}
/// The account/session owner supplies its actual bound reads and verified history.
pub trait CommandSession: Send + Sync {
    fn read<'a>(&'a self, method: &'a str, params: Json) -> RequestFuture<'a>;
    fn turn_page<'a>(&'a self, thread: &'a JsString, cursor: Json) -> RequestFuture<'a>;
    fn busy_revision(&self) -> u64;
    fn started(&self, observed_revision: u64);
    fn rejected_start(&self);
    fn invalidate(&self);
}
pub trait CommandOwner: Send + Sync {
    fn binding<'a>(&'a self, method: &'a JsString) -> RequestFuture<'a>;
    fn operation<'a>(&'a self, call: &'a NativeOperationCall) -> RequestFuture<'a>;
    fn interaction<'a>(&'a self, id: &'a JsString) -> RequestFuture<'a>;
    fn interact<'a>(
        &'a self,
        call: &'a NativeOperationCall,
        guard: &'a dyn NativeDispatchGuard,
    ) -> RequestFuture<'a>;
}
impl CommandOwner for NativeOwner {
    fn binding<'a>(&'a self, method: &'a JsString) -> RequestFuture<'a> {
        Box::pin(async {
            Ok(NativeOwner::binding(self, method, None)
                .await?
                .value()
                .clone())
        })
    }
    fn operation<'a>(&'a self, call: &'a NativeOperationCall) -> RequestFuture<'a> {
        Box::pin(NativeOwner::operation(self, call))
    }
    fn interaction<'a>(&'a self, id: &'a JsString) -> RequestFuture<'a> {
        Box::pin(NativeOwner::interaction(self, id))
    }
    fn interact<'a>(
        &'a self,
        call: &'a NativeOperationCall,
        guard: &'a dyn NativeDispatchGuard,
    ) -> RequestFuture<'a> {
        Box::pin(NativeOwner::interact(self, call, Some(guard)))
    }
}
pub trait RetryIdentity: Send + Sync {
    fn next(&self) -> JsString;
}
pub struct RandomRetryIdentity;
impl RetryIdentity for RandomRetryIdentity {
    fn next(&self) -> JsString {
        format!("wa-retry-{}", uuid::Uuid::new_v4()).into()
    }
}

fn text(value: &Json, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn call(value: &Json) -> IvyResult<NativeOperationCall> {
    Ok(NativeOperationCall {
        operation_id: text(value, "operationId")?,
        method: text(value, "method")?,
        params: value.required("params")?.clone(),
        definition_hash: text(value, "definitionHash")?,
    })
}
fn require(ok: bool, code: &str, message: JsString) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, "").with_js_message(message))
    }
}
fn error_message(value: &Json) -> IvyResult<JsString> {
    text(value, "message")
}
fn concat(a: &str, b: &JsString) -> JsString {
    let mut units: Vec<_> = a.encode_utf16().collect();
    units.extend_from_slice(&b.0);
    JsString(units)
}
fn contains(haystack: &[u16], needle: &str) -> bool {
    let n: Vec<_> = needle.encode_utf16().collect();
    haystack.windows(n.len()).any(|v| v == n)
}
fn busy_error(message: &JsString) -> bool {
    let lower = message.to_lowercase();
    lower
        .0
        .split(|u| matches!(u, 10 | 13 | 0x2028 | 0x2029))
        .any(|line| {
            let turn: Vec<_> = "turn".encode_utf16().collect();
            line.windows(turn.len()).enumerate().any(|(offset, w)| {
                w == turn
                    && (contains(&line[offset + 4..], "in progress")
                        || contains(&line[offset + 4..], "already active")
                        || contains(&line[..offset], "active")
                        || contains(&line[..offset], "in progress"))
            })
        })
}
struct Attempt<'a> {
    saved: Arc<Mutex<Json>>,
    journal: Journal,
    key: &'a str,
    read_only: bool,
    guard: Option<&'a dyn NativeDispatchGuard>,
}
impl NativeDispatchGuard for Attempt<'_> {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async {
            if let Some(guard) = self.guard {
                guard.check().await?;
            }
            let value = {
                let mut saved = self.saved.lock().map_err(|_| IvyError::internal())?;
                saved.insert("attempted", true.into());
                saved.clone()
            };
            if !self.read_only {
                self.journal.set(self.key, &value)?;
                self.journal.flush().await?;
            }
            Ok(())
        })
    }
}

pub struct NativeCommandDispatcher<'a> {
    pub journal: Journal,
    pub owner: &'a dyn CommandOwner,
    pub session: &'a dyn CommandSession,
    pub clock: &'a dyn Clock,
    pub identities: &'a dyn RetryIdentity,
    pub translator: Translator,
    last_dispatch_ms: Mutex<Option<u64>>,
}
impl<'a> NativeCommandDispatcher<'a> {
    pub fn new(
        journal: Journal,
        owner: &'a dyn CommandOwner,
        session: &'a dyn CommandSession,
        clock: &'a dyn Clock,
        identities: &'a dyn RetryIdentity,
        translator: Translator,
    ) -> Self {
        Self {
            journal,
            owner,
            session,
            clock,
            identities,
            translator,
            last_dispatch_ms: Mutex::new(None),
        }
    }
    pub fn last_dispatch_ms(&self) -> Option<u64> {
        *self.last_dispatch_ms.lock().unwrap()
    }
    async fn execute(
        &self,
        key: &str,
        method: &str,
        parameters: &dyn CommandParameters,
        guard: Option<&dyn NativeDispatchGuard>,
    ) -> IvyResult<Json> {
        let read_only = ["model/list", "thread/list"].contains(&method);
        let journal_key = format!("interaction:{key}");
        let retained = self.journal.get(&format!("native:{key}"))?;
        if retained.get("attempted").is_some_and(truthy) {
            let previous = self
                .owner
                .operation(&call(retained.required("call")?)?)
                .await?;
            require(
                previous.get("kind").is_none()
                    && previous.get("phase") != Some(&"outcome_unknown".into()),
                "interaction_expired",
                self.translator.translate("native.previous_unclear", None)?,
            )?;
            if [
                Some(&Json::from("accepted")),
                Some(&Json::from("dispatched")),
            ]
            .contains(&previous.get("phase"))
            {
                return Ok(Json::Null);
            }
            require(
                previous
                    .get("reply")
                    .is_some_and(|v| truthy(v) && v.get("result").is_some()),
                "whatsapp_native_command_failed",
                self.translator.translate("native.previous_failed", None)?,
            )?;
            return Ok(previous.required("reply")?.required("result")?.clone());
        }
        let mut saved = if read_only {
            Json::Null
        } else {
            self.journal.get(&journal_key)?
        };
        if !truthy(&saved) {
            let method = JsString::from(method);
            let binding = self.owner.binding(&method).await?;
            let params = parameters.build().await?;
            let operation_id = format!(
                "wa-{}",
                identity(&[key.into(), Json::String(method.clone()), params.clone()])?
            );
            saved = Json::object([
                (
                    "call",
                    Json::object([
                        ("operationId", operation_id.into()),
                        ("method", Json::String(method)),
                        ("params", params),
                        (
                            "definitionHash",
                            binding.required("definitionHash")?.clone(),
                        ),
                    ]),
                ),
                ("attempted", false.into()),
            ]);
            if !read_only {
                self.journal.set(&journal_key, &saved)?;
            }
        }
        require(
            saved.required("call")?.get("method") == Some(&method.into()),
            "whatsui_identity_conflict",
            "Command recovery cannot change the native method.".into(),
        )?;
        if let Some(result) = saved.get("result") {
            return Ok(result.clone());
        }
        let call = call(saved.required("call")?)?;
        let revision = self.session.busy_revision();
        let started = self.clock.monotonic();
        let recovering = saved.get("attempted").is_some_and(truthy);
        let owned = Arc::new(Mutex::new(saved));
        let attempted = Attempt {
            saved: owned.clone(),
            journal: self.journal.clone(),
            key: &journal_key,
            read_only,
            guard,
        };
        let response = if recovering {
            self.owner.interaction(&call.operation_id).await
        } else {
            self.owner.interact(&call, &attempted).await
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                if error.code == "interaction_expired" && method == "turn/start" {
                    let thread = text(&call.params, "threadId")?;
                    let mut cursor = Json::Null;
                    for _ in 0..64 {
                        let history = self.session.turn_page(&thread, cursor).await?;
                        for turn in history
                            .required("data")?
                            .as_array()
                            .ok_or_else(IvyError::internal)?
                        {
                            if turn
                                .required("items")?
                                .as_array()
                                .ok_or_else(IvyError::internal)?
                                .iter()
                                .any(|item| {
                                    item.get("type") == Some(&"userMessage".into())
                                        && item.get("clientId")
                                            == call.params.get("clientUserMessageId")
                                })
                            {
                                return Ok(Json::object([("turn", turn.clone())]));
                            }
                        }
                        cursor = history.get("nextCursor").cloned().unwrap_or(Json::Null);
                        if !truthy(&cursor) {
                            break;
                        }
                    }
                }
                if !recovering && error.outcome == Outcome::NotExecuted {
                    let value = {
                        let mut saved = owned.lock().unwrap();
                        saved.insert("attempted", false.into());
                        saved.clone()
                    };
                    if !read_only {
                        self.journal.set(&journal_key, &value)?;
                        self.journal.flush().await?;
                    }
                }
                return Err(error);
            }
        };
        *self.last_dispatch_ms.lock().unwrap() = Some(
            self.clock
                .monotonic()
                .saturating_duration_since(started)
                .as_secs_f64()
                .mul_add(1000.0, 0.0)
                .round() as u64,
        );
        if let Some(error) = response.required("reply")?.get("error") {
            let message = error_message(error)?;
            let thread = call
                .params
                .get("threadId")
                .map(js_string)
                .unwrap_or_else(|| "undefined".into());
            if method == "turn/start"
                && error.get("code") == Some(&Json::Number(-32600.0))
                && (message == concat("thread not loaded: ", &thread) || busy_error(&message))
            {
                let value = {
                    let mut saved = owned.lock().unwrap();
                    saved.insert("attempted", false.into());
                    let mut updated = saved.required("call")?.clone();
                    updated.insert("operationId", Json::String(self.identities.next()));
                    saved.insert("call", updated);
                    saved.clone()
                };
                self.journal.set(&journal_key, &value)?;
                self.session.rejected_start();
                return Err(IvyError::new("native_read_pending", "").with_js_message(message));
            }
            return Err(IvyError::new("whatsapp_native_command_failed", "")
                .with_js_message(message)
                .with_outcome(Outcome::Completed));
        }
        let result = response.required("reply")?.required("result")?.clone();
        let value = {
            let mut saved = owned.lock().unwrap();
            saved.insert("result", result.clone());
            saved.clone()
        };
        if !read_only {
            self.journal.set(&journal_key, &value)?;
        }
        if method == "turn/start" {
            self.session.started(revision);
        }
        Ok(result)
    }
}
impl NativeCommands for NativeCommandDispatcher<'_> {
    fn command<'a>(
        &'a self,
        key: &'a str,
        method: &'a str,
        parameters: &'a dyn CommandParameters,
        guard: Option<&'a dyn NativeDispatchGuard>,
    ) -> RequestFuture<'a> {
        Box::pin(self.execute(key, method, parameters, guard))
    }
    fn read<'a>(&'a self, method: &'a str, params: Json) -> RequestFuture<'a> {
        self.session.read(method, params)
    }
    fn invalidate(&self) {
        self.session.invalidate();
    }
}
