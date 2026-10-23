//! Original call admission and native-command reconciliation. The journal owns
//! immutable intents/receipts; this owner retains admitted work through observers.
use crate::{
    admission::PhoneAdmission,
    journal::{NativeAdmission, PhoneJournal},
    native_client::{PhoneNativeClient, validate_phone},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

#[derive(Clone)]
pub struct PhoneCallWork(watch::Receiver<Option<IvyResult<IvyJson>>>);
impl PhoneCallWork {
    pub async fn wait(&self) -> IvyResult<IvyJson> {
        let mut receive = self.0.clone();
        loop {
            if let Some(result) = receive.borrow_and_update().clone() {
                return result;
            }
            receive.changed().await.map_err(|_| retained())?;
        }
    }
}
fn work(future: impl Future<Output = IvyResult<IvyJson>> + Send + 'static) -> PhoneCallWork {
    let (done, result) = watch::channel(None);
    tokio::spawn(async move {
        done.send_replace(Some(future.await));
    });
    PhoneCallWork(result)
}
fn field<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a IvyJson> {
    value.get(name).ok_or_else(IvyError::internal)
}
fn text(value: &IvyJson, name: &str) -> IvyResult<JsString> {
    field(value, name)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn retained() -> IvyError {
    IvyError::new(
        "phone_operation_retained",
        "Original native call outcome requires reconciliation; it cannot be dispatched again.",
    )
    .with_outcome(Outcome::Unknown)
}
struct Pending {
    call_id: JsString,
    method: String,
    key: u32,
    epoch: IvyJson,
    request_hash: String,
    work: PhoneCallWork,
}
struct Inner {
    admission: PhoneAdmission,
    journal: PhoneJournal,
    native: PhoneNativeClient,
    dispatch: tokio::sync::Mutex<()>,
    pending: Mutex<BTreeMap<JsString, Pending>>,
}
#[derive(Clone)]
pub struct PhoneCallCommands(Arc<Inner>);
#[derive(Clone, Copy)]
pub enum DesktopVoiceTransition {
    Pause,
    Resume,
}
impl PhoneCallCommands {
    pub fn new(
        admission: PhoneAdmission,
        journal: PhoneJournal,
        native: PhoneNativeClient,
    ) -> Self {
        Self(Arc::new(Inner {
            admission,
            journal,
            native,
            dispatch: tokio::sync::Mutex::new(()),
            pending: Mutex::new(BTreeMap::new()),
        }))
    }
    pub fn admission(&self) -> &PhoneAdmission {
        &self.0.admission
    }
    pub fn journal(&self) -> &PhoneJournal {
        &self.0.journal
    }
    /// Arguments carry principalId, operationId, optional recipientId, route,
    /// screening, destination and voicePrompt. The owner supplies the epoch.
    pub fn admit_outgoing(&self, args: IvyJson) -> PhoneCallWork {
        let owner = self.clone();
        work(async move {
            let operation_id = text(&args, "operationId")?;
            loop {
                let prior = owner
                    .journal()
                    .call_for_operation(operation_id.clone())
                    .await?;
                let mut args = args.clone();
                args.insert(
                    "epoch",
                    prior
                        .as_ref()
                        .and_then(|v| v.get("epoch"))
                        .cloned()
                        .unwrap_or_else(|| owner.0.native.epoch().clone().into()),
                );
                let admission = owner.admission().outgoing(&args)?;
                if prior.is_some() {
                    return owner.journal().admit_call(admission).await;
                }
                let observed = owner.0.native.observe(None).await?;
                if let Some(call) = owner
                    .journal()
                    .admit_native_observation(
                        owner.admission().clone(),
                        NativeAdmission::Outgoing(admission),
                        owner.0.native.epoch().clone(),
                        observed,
                    )
                    .await?
                {
                    return Ok(call);
                }
            }
        })
    }
    /// Bind only an observed original incoming call; supplied metadata is never
    /// accepted in place of a native observation.
    pub fn admit_incoming(&self, args: IvyJson) -> PhoneCallWork {
        let owner = self.clone();
        work(async move {
            let operation_id = text(&args, "operationId")?;
            let call_id = text(&args, "callId")?;
            let principal = text(&args, "principalId")?;
            loop {
                if let Some(prior) = owner
                    .journal()
                    .call_for_operation(operation_id.clone())
                    .await?
                {
                    require(
                        prior.get("direction") == Some(&"incoming".into())
                            && prior.get("callId") == Some(&call_id.clone().into())
                            && prior.get("principalId") == Some(&principal.clone().into())
                            && prior.get("policyHash") == Some(&owner.admission().hash().into())
                            && prior.get("route") == args.get("route"),
                        "mutation_conflict",
                        "Original incoming admission arguments or policy changed.",
                    )?;
                    owner.admission().authorize(&principal, &prior, "read")?;
                    return Ok(prior);
                }
                let observed = owner.0.native.observe(Some(call_id.clone())).await?;
                if let Some(call) = owner
                    .journal()
                    .admit_native_observation(
                        owner.admission().clone(),
                        NativeAdmission::Incoming(args.clone()),
                        owner.0.native.epoch().clone(),
                        observed,
                    )
                    .await?
                {
                    return Ok(call);
                }
            }
        })
    }
    pub async fn read(
        &self,
        principal: JsString,
        call_id: JsString,
        method: JsString,
        generation: u32,
    ) -> IvyResult<Option<IvyJson>> {
        self.journal()
            .authorized_call(self.admission().clone(), principal, call_id.clone(), "read")
            .await?;
        self.journal()
            .call_command(call_id, method, generation)
            .await
    }
    pub fn cancel_unprepared(&self, principal: JsString, call_id: JsString) -> PhoneCallWork {
        let owner = self.clone();
        work(async move {
            let _gate = owner.0.dispatch.lock().await;
            let call = owner
                .journal()
                .authorized_call(
                    owner.admission().clone(),
                    principal,
                    call_id.clone(),
                    "cleanup",
                )
                .await?;
            let pending = owner
                .0
                .pending
                .lock()
                .unwrap()
                .values()
                .any(|p| p.call_id == call_id);
            if pending
                && owner
                    .journal()
                    .call_command(
                        call_id.clone(),
                        if call.get("direction") == Some(&"incoming".into()) {
                            "call.claim"
                        } else {
                            "call.prepare"
                        }
                        .into(),
                        0,
                    )
                    .await?
                    .is_none()
            {
                return Err(IvyError::new(
                    "phone_release_unconfirmed",
                    if call.get("direction") == Some(&"incoming".into()) {
                        "A claimed or uncertain incoming offer still requires native cleanup."
                    } else {
                        "Possible native call creation requires original cleanup."
                    },
                ));
            }
            owner.journal().discard_unprepared_call(call_id).await?;
            Ok(IvyJson::Null)
        })
    }
    fn command<const N: usize>(
        &self,
        principal: JsString,
        call_id: JsString,
        method: &'static str,
        fields: [(&str, IvyJson); N],
        timeout_ms: u64,
    ) -> PhoneCallWork {
        let mut params = IvyJson::object(fields);
        params.insert("callId", call_id.clone().into());
        self.invoke(principal, call_id, method.into(), params, timeout_ms)
    }
    pub fn prepare(&self, p: JsString, call: JsString) -> PhoneCallWork {
        self.command(p, call, "call.prepare", [], 15000)
    }
    pub fn features(&self, p: JsString, call: JsString, challenge: IvyJson) -> PhoneCallWork {
        self.command(p, call, "call.features", [("challenge", challenge)], 15000)
    }
    pub fn prepare_screening(
        &self,
        p: JsString,
        call: JsString,
        settings: IvyJson,
    ) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.screening.prepare",
            [("settings", settings)],
            15000,
        )
    }
    pub fn bridge_screening(
        &self,
        p: JsString,
        call: JsString,
        settings: IvyJson,
    ) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.screening.bridge",
            [("settings", settings)],
            15000,
        )
    }
    pub fn connect_windows(&self, p: JsString, call: JsString, settings: IvyJson) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.windows.connect",
            [("settings", settings)],
            15000,
        )
    }
    pub fn upgrade_codec(&self, p: JsString, call: JsString) -> PhoneCallWork {
        self.command(p, call, "call.codec.upgrade", [], 15000)
    }
    pub fn dial(
        &self,
        principal: JsString,
        call_id: JsString,
        username: Option<JsString>,
        password: Option<JsString>,
        ring_seconds: u32,
        waiting: bool,
    ) -> PhoneCallWork {
        let owner = self.clone();
        work(async move {
            let call = owner
                .journal()
                .authorized_call(
                    owner.admission().clone(),
                    principal.clone(),
                    call_id.clone(),
                    "connect",
                )
                .await?;
            require(
                call.get("direction") == Some(&"outgoing".into()),
                "phone_call_conflict",
                "Incoming calls cannot be dialled.",
            )?;
            owner
                .command(
                    principal,
                    call_id,
                    "call.dial",
                    [
                        ("destination", field(&call, "destination")?.clone()),
                        ("username", username.map_or(IvyJson::Null, IvyJson::from)),
                        ("password", password.map_or(IvyJson::Null, IvyJson::from)),
                        ("ringSeconds", (ring_seconds as f64).into()),
                        ("waiting", waiting.into()),
                    ],
                    ((u64::from(ring_seconds) + 20) * 1000).min(150000),
                )
                .wait()
                .await
        })
    }
    pub fn prepare_audio(
        &self,
        p: JsString,
        call: JsString,
        settings: IvyJson,
        desktop: IvyJson,
    ) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.audio.prepare",
            [("settings", settings), ("desktop", desktop)],
            15000,
        )
    }
    pub fn rebind_audio(
        &self,
        p: JsString,
        call: JsString,
        desktop: IvyJson,
        thread_id: JsString,
    ) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.audio.rebind",
            [("desktop", desktop), ("threadId", thread_id.into())],
            20000,
        )
    }
    pub fn answer(&self, p: JsString, call: JsString, waiting: bool) -> PhoneCallWork {
        self.command(p, call, "call.answer", [("waiting", waiting.into())], 20000)
    }
    pub fn end_waiting(&self, p: JsString, call: JsString) -> PhoneCallWork {
        self.command(p, call, "call.waiting.end", [], 15000)
    }
    pub fn feedback(
        &self,
        p: JsString,
        call: JsString,
        sequence: u32,
        success: bool,
    ) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.command.feedback",
            [
                ("commandSequence", (sequence as f64).into()),
                ("success", success.into()),
            ],
            15000,
        )
    }
    pub fn claim(&self, p: JsString, call: JsString) -> PhoneCallWork {
        self.command(p, call, "call.claim", [], 15000)
    }
    pub fn launch_desktop(
        &self,
        p: JsString,
        call: JsString,
        application: IvyJson,
    ) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.desktop.launch",
            [("application", application)],
            15000,
        )
    }
    pub fn start_voice(
        &self,
        p: JsString,
        call: JsString,
        desktop: IvyJson,
        voice_input: IvyJson,
    ) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.desktop.startVoice",
            [("desktop", desktop), ("voiceInput", voice_input)],
            60000,
        )
    }
    pub fn transition_voice(
        &self,
        p: JsString,
        call: JsString,
        action: DesktopVoiceTransition,
        generation: u32,
        desktop: IvyJson,
        voice_input: IvyJson,
    ) -> PhoneCallWork {
        self.command(
            p,
            call,
            match action {
                DesktopVoiceTransition::Pause => "call.desktop.pauseVoice",
                DesktopVoiceTransition::Resume => "call.desktop.resumeVoice",
            },
            [
                ("desktop", desktop),
                ("voiceInput", voice_input),
                ("generation", (generation as f64).into()),
            ],
            40000,
        )
    }
    pub fn stop_voice(
        &self,
        p: JsString,
        call: JsString,
        desktop: IvyJson,
        voice_input: IvyJson,
    ) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.desktop.stopVoice",
            [("desktop", desktop), ("voiceInput", voice_input)],
            25000,
        )
    }
    pub fn prepare_realtime(
        &self,
        p: JsString,
        call: JsString,
        generation: u32,
        queue_ms: Option<u32>,
        playback_prebuffer_ms: Option<u32>,
    ) -> PhoneCallWork {
        let mut params = IvyJson::object([
            ("callId", call.clone().into()),
            ("generation", (generation as f64).into()),
            ("queueMs", (queue_ms.unwrap_or(80) as f64).into()),
        ]);
        if let Some(value) = playback_prebuffer_ms {
            params.insert("playbackPrebufferMs", (value as f64).into());
        }
        self.invoke(p, call, "call.realtime.prepare".into(), params, 15000)
    }
    pub fn answer_realtime(
        &self,
        p: JsString,
        call: JsString,
        generation: u32,
        sdp: JsString,
    ) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.realtime.answer",
            [
                ("generation", (generation as f64).into()),
                ("sdp", sdp.into()),
            ],
            20000,
        )
    }
    pub fn stop_realtime(&self, p: JsString, call: JsString, generation: u32) -> PhoneCallWork {
        self.command(
            p,
            call,
            "call.realtime.stop",
            [("generation", (generation as f64).into())],
            20000,
        )
    }
    pub fn hangup(&self, p: JsString, call: JsString) -> PhoneCallWork {
        self.command(p, call, "call.hangup", [], 15000)
    }
    pub fn release(&self, p: JsString, call: JsString) -> PhoneCallWork {
        self.command(p, call, "call.release", [], 30000)
    }

    fn invoke(
        &self,
        principal: JsString,
        call_id: JsString,
        method: String,
        params: IvyJson,
        timeout: u64,
    ) -> PhoneCallWork {
        let (done, result) = watch::channel(None);
        let work = PhoneCallWork(result);
        let observer = work.clone();
        let owner = self.clone();
        tokio::spawn(async move {
            let result = owner
                .invoke_inner(principal, call_id, method, params, timeout, observer)
                .await;
            done.send_replace(Some(result));
        });
        work
    }
    async fn invoke_inner(
        &self,
        principal: JsString,
        call_id: JsString,
        method: String,
        params: IvyJson,
        timeout: u64,
        observer: PhoneCallWork,
    ) -> IvyResult<IvyJson> {
        let gate = self.0.dispatch.lock().await;
        let snapshot = self
            .journal()
            .native_command_snapshot(
                self.admission().clone(),
                principal,
                call_id.clone(),
                method.clone(),
                params.clone(),
                self.0.native.epoch().clone(),
            )
            .await?;
        if let Some(prior) = snapshot.prior {
            let intent = field(&prior, "intent")?;
            require(
                intent.get("epoch") == snapshot.call.get("epoch")
                    && intent.get("requestHash") == Some(&snapshot.request_hash.into()),
                "mutation_conflict",
                "Original native call arguments changed.",
            )?;
            if prior.get("phase") == Some(&"result".into()) {
                drop(gate);
                return self.settle(&call_id, &method, &params, prior).await;
            }
            let pending = self
                .0
                .pending
                .lock()
                .unwrap()
                .get(&text(intent, "operationId")?)
                .map(|p| p.work.clone());
            drop(gate);
            return match pending {
                Some(pending) => pending.wait().await,
                None => Err(retained()),
            };
        }
        // Original beforeSend stores its intent synchronously before request()
        // returns. Cover that same admission interval while the Rust worker is
        // receiving its intent; this is the existing pending owner, not a retry.
        let pending = self
            .0
            .pending
            .lock()
            .unwrap()
            .values()
            .find(|p| p.call_id == call_id && p.method == method && p.key == snapshot.key)
            .map(|p| (p.epoch.clone(), p.request_hash.clone(), p.work.clone()));
        if let Some((epoch, hash, pending)) = pending {
            require(
                Some(&epoch) == snapshot.call.get("epoch") && hash == snapshot.request_hash,
                "mutation_conflict",
                "Original native call arguments changed.",
            )?;
            drop(gate);
            return pending.wait().await;
        }
        require(
            snapshot.current,
            "phone_call_conflict",
            "Only the original current call can dispatch another native command.",
        )?;
        let operation_id: JsString = uuid::Uuid::new_v4().to_string().into();
        self.0.pending.lock().unwrap().insert(
            operation_id.clone(),
            Pending {
                call_id: call_id.clone(),
                method: method.clone(),
                key: snapshot.key,
                epoch: field(&snapshot.call, "epoch")?.clone(),
                request_hash: snapshot.request_hash,
                work: observer,
            },
        );
        let request =
            self.0
                .native
                .request(&method, params.clone(), Some(operation_id.clone()), timeout);
        drop(gate);
        let result = async {
            request?.wait().await?;
            let operation = self
                .journal()
                .operation(operation_id.clone())
                .await?
                .ok_or_else(|| {
                    IvyError::new(
                        "phone_receipt_missing",
                        "Original native receipt was not retained.",
                    )
                })?;
            self.settle(&call_id, &method, &params, operation).await
        }
        .await;
        self.0.pending.lock().unwrap().remove(&operation_id);
        result
    }
    async fn settle(
        &self,
        call_id: &JsString,
        method: &str,
        params: &IvyJson,
        operation: IvyJson,
    ) -> IvyResult<IvyJson> {
        let receipt = operation.get("receipt");
        let completed = operation.get("phase") == Some(&"result".into());
        let ok = receipt.and_then(|r| r.get("ok")) == Some(&true.into());
        if completed && ok {
            let result = receipt
                .and_then(|r| r.get("result"))
                .ok_or_else(IvyError::internal)?;
            if let Some(action) = method.strip_prefix("call.desktop.") {
                validate_phone("CallDesktopResult", result)?;
                require(
                    result.get("callId") == Some(&call_id.clone().into())
                        && result.get("action") == Some(&action.into()),
                    "phone_receipt_conflict",
                    "Desktop receipt belongs to another original call or action.",
                )?;
            }
            if method == "call.release" {
                self.journal()
                    .release_call(
                        call_id.clone(),
                        text(field(&operation, "intent")?, "operationId")?,
                    )
                    .await?;
            }
            if method == "call.command.feedback" {
                require(
                    result.get("callId") == Some(&call_id.clone().into())
                        && result.get("commandSequence") == params.get("commandSequence")
                        && result.get("success") == params.get("success"),
                    "phone_receipt_conflict",
                    "Keypad feedback receipt differs from the original command.",
                )?;
            }
        }
        if method == "call.prepare"
            && completed
            && receipt.and_then(|r| r.get("ok")) == Some(&false.into())
            && receipt
                .and_then(|r| r.get("error"))
                .and_then(IvyJson::as_js_string)
                .is_some_and(|s| {
                    s == &"operation_conflict".into() || s == &"runtime_not_ready".into()
                })
        {
            self.journal()
                .discard_unprepared_call(call_id.clone())
                .await?;
        }
        Ok(operation)
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/phone_call_commands.rs"]
mod tests;
