//! Durable Voice admission and reconciliation for one assignment execution.
use crate::{
    assignment_voice::{
        ForwardedVoiceOutcome, VoiceMethod, call_voice, forwarded_voice_outcome, voice_prompt,
    },
    inbox_evidence::InboxEvidence,
    store::Record,
    voice_target::discover_voice_target,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, hash::hash_json};
use ivy_sdk::{
    clock::{Clock, automatic_notifications_allowed},
    rpc::RequestOptions,
    tools::ToolClients,
};

#[derive(Debug, PartialEq, Eq)]
pub enum VoiceResult {
    Confirmed,
    Pending,
    Fallback,
}
impl VoiceResult {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Pending => "pending",
            Self::Fallback => "fallback",
        }
    }
}
fn need(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("secretary_voice_mismatch", message))
    }
}
fn has_call(voice: &IvyJson) -> bool {
    voice
        .get("callId")
        .and_then(IvyJson::as_js_string)
        .is_some_and(|s| !s.0.is_empty())
}
fn changed(voice: &IvyJson, state: &str, reason: IvyJson) -> IvyJson {
    let mut next = voice.clone();
    next.insert("state", state.into());
    next.insert("reason", reason);
    next
}

/// Borrows the current engine generation, its Store and the same RPC tool cache.
pub struct AssignmentVoice<'a, 'engine, 'journal> {
    pub evidence: &'a InboxEvidence<'engine, 'journal>,
    pub tools: &'a ToolClients,
    pub clock: &'a dyn Clock,
}
impl AssignmentVoice<'_, '_, '_> {
    fn expiry(&self) -> IvyJson {
        IvyJson::Number(self.clock.utc_millis() as f64 + 8.0 * 86_400_000.0)
    }
    async fn save(&self, latest: &mut IvyJson, voice: IvyJson) -> IvyResult<IvyJson> {
        let mut next = latest.required("value")?.clone();
        next.insert("voice", voice.clone());
        *latest = self
            .evidence
            .store
            .amend(latest, Record::Execution, &next)
            .await?;
        Ok(voice)
    }
    pub async fn complete_fallback(&self, execution: &IvyJson) -> IvyResult<()> {
        let mut latest = self
            .evidence
            .store
            .read(
                Record::Execution,
                execution.required("pin")?.required("objectId")?,
            )
            .await?;
        if let Some(voice) = latest
            .required("value")?
            .get("voice")
            .filter(|v| !v.is_null())
        {
            if matches!(
                voice.get("state").and_then(|s| s.as_str().ok()).as_deref(),
                Some("failed" | "fallback")
            ) && voice.get("expiresAt") == Some(&IvyJson::Null)
            {
                let mut next = voice.clone();
                next.insert("expiresAt", self.expiry());
                self.save(&mut latest, next).await?;
            }
        }
        Ok(())
    }
    async fn call(
        &self,
        method: VoiceMethod,
        voice: &IvyJson,
        args: IvyJson,
        operation_id: Option<JsString>,
    ) -> IvyResult<IvyJson> {
        call_voice(
            self.evidence.store.client.as_ref(),
            self.tools,
            method,
            voice.required("target")?,
            args,
            operation_id,
        )
        .await
    }
    pub async fn step(
        &self,
        execution: &IvyJson,
        decision: &IvyJson,
        eligible: bool,
    ) -> IvyResult<VoiceResult> {
        self.evidence.verify_owner().await?;
        let value = execution.required("value")?;
        let result_hash = IvyJson::from(hash_json(value.required("result")?)?);
        let expires_at = self.expiry();
        let mut latest = self
            .evidence
            .store
            .read(
                Record::Execution,
                execution.required("pin")?.required("objectId")?,
            )
            .await?;
        // Read the legacy record even when the current execution already has Voice,
        // matching the original migration boundary and its storage failures.
        let legacy = self.evidence.store.journal.named(
            "secretary/assignment-voice",
            &value.required("executionId")?.as_str()?,
            true,
        )?;
        let mut saved = if let Some(voice) = latest
            .required("value")?
            .get("voice")
            .filter(|v| !v.is_null())
        {
            voice.clone()
        } else if let Some(legacy) = legacy {
            self.save(&mut latest, legacy.value).await?
        } else {
            if !eligible {
                return Ok(VoiceResult::Fallback);
            }
            let target = match self
                .evidence
                .settings
                .required("policy")?
                .get("voiceEscalation")
                .filter(|v| !v.is_null())
            {
                Some(target) => Some(target.clone()),
                None => {
                    discover_voice_target(self.evidence.store.client.as_ref(), self.tools).await?
                }
            };
            let Some(target) = target else {
                return Ok(VoiceResult::Fallback);
            };
            let request = IvyJson::object([
                ("operationId", uuid::Uuid::new_v4().to_string().into()),
                ("recipientId", target.required("recipientId")?.clone()),
                ("route", "voice".into()),
                (
                    "voicePrompt",
                    IvyJson::String(voice_prompt(
                        decision
                            .required("text")?
                            .as_js_string()
                            .ok_or_else(IvyError::internal)?,
                    )),
                ),
            ]);
            self.save(
                &mut latest,
                IvyJson::object([
                    ("executionId", value.required("executionId")?.clone()),
                    ("resultHash", result_hash.clone()),
                    ("source", execution.required("pin")?.clone()),
                    ("target", target),
                    ("request", request),
                    ("callId", IvyJson::Null),
                    ("state", "dispatching".into()),
                    ("reason", IvyJson::Null),
                    ("expiresAt", IvyJson::Null),
                ]),
            )
            .await?
        };
        let request = saved.required("request")?;
        need(
            saved.get("executionId") == value.get("executionId")
                && saved.get("resultHash") == Some(&result_hash)
                && saved.required("source")?.get("objectId")
                    == execution.required("pin")?.get("objectId")
                && request.get("recipientId") == saved.required("target")?.get("recipientId")
                && request.get("route") == Some(&"voice".into())
                && request.get("voicePrompt")
                    == Some(&IvyJson::String(voice_prompt(
                        decision
                            .required("text")?
                            .as_js_string()
                            .ok_or_else(IvyError::internal)?,
                    ))),
            "The original Voice request changed.",
        )?;
        match saved.required("state")?.as_str()?.as_str() {
            "confirmed" => return Ok(VoiceResult::Confirmed),
            "failed" | "fallback" => return Ok(VoiceResult::Fallback),
            _ => {}
        }
        match self.dispatch(&mut latest, &mut saved, expires_at).await {
            Ok(result) => Ok(result),
            Err(error) => {
                if error.outcome == Outcome::NotExecuted && !has_call(&saved) {
                    self.save(
                        &mut latest,
                        changed(&saved, "failed", error.code.clone().into()),
                    )
                    .await?;
                    return Ok(VoiceResult::Fallback);
                }
                if has_call(&saved)
                    && !matches!(
                        error.code.as_str(),
                        "revision_conflict" | "mutation_conflict"
                    )
                {
                    self.save(
                        &mut latest,
                        changed(&saved, "fallback", error.code.clone().into()),
                    )
                    .await?;
                    return Ok(VoiceResult::Fallback);
                }
                if saved.get("state") != Some(&"outcome_unknown".into())
                    || saved.get("reason") != Some(&error.code.clone().into())
                {
                    self.save(
                        &mut latest,
                        changed(&saved, "outcome_unknown", error.code.clone().into()),
                    )
                    .await?;
                }
                Err(error)
            }
        }
    }
    async fn dispatch(
        &self,
        latest: &mut IvyJson,
        saved: &mut IvyJson,
        expires_at: IvyJson,
    ) -> IvyResult<VoiceResult> {
        if !has_call(saved) {
            if !automatic_notifications_allowed(
                self.evidence.store.client.as_ref(),
                self.clock.utc_millis(),
                RequestOptions::default(),
            )
            .await?
            {
                return Ok(VoiceResult::Pending);
            }
            let request = saved.required("request")?;
            let operation_id = request
                .required("operationId")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?;
            let call = self
                .call(
                    VoiceMethod::Request,
                    saved,
                    request.clone(),
                    Some(operation_id),
                )
                .await?;
            need(
                call.get("callId").and_then(IvyJson::as_js_string).is_some(),
                "PhoneBridge returned no original call identity.",
            )?;
            let call_id = call.required("callId")?.clone();
            if call.get("operationId") != request.get("operationId") {
                let forwarded = self
                    .call(
                        VoiceMethod::Operation,
                        saved,
                        IvyJson::object([
                            ("callId", call_id.clone()),
                            ("operationId", request.required("operationId")?.clone()),
                        ]),
                        None,
                    )
                    .await?;
                need(
                    !forwarded.is_null(),
                    "PhoneBridge returned no forwarded request outcome.",
                )?;
                forwarded_voice_outcome(
                    &forwarded,
                    &call_id,
                    request.required("operationId")?,
                    request.required("voicePrompt")?,
                )?;
            } else {
                need(
                    call.get("recipientId") == request.get("recipientId")
                        && call.get("principalId")
                            == self
                                .evidence
                                .settings
                                .required("identity")?
                                .get("principalId")
                        && call.get("route") == Some(&"voice".into())
                        && call.get("voicePrompt") == request.get("voicePrompt"),
                    "PhoneBridge changed the original Voice admission.",
                )?;
            }
            let mut next = changed(saved, "dispatching", IvyJson::Null);
            next.insert("callId", call_id);
            *saved = self.save(latest, next).await?;
        }
        let call_id = saved.required("callId")?.clone();
        let request = saved.required("request")?;
        let forwarded = self
            .call(
                VoiceMethod::Operation,
                saved,
                IvyJson::object([
                    ("callId", call_id.clone()),
                    ("operationId", request.required("operationId")?.clone()),
                ]),
                None,
            )
            .await?;
        if !forwarded.is_null() {
            if forwarded_voice_outcome(
                &forwarded,
                &call_id,
                request.required("operationId")?,
                request.required("voicePrompt")?,
            )? == ForwardedVoiceOutcome::Pending
            {
                return Ok(VoiceResult::Pending);
            }
        } else {
            let dial = self
                .call(
                    VoiceMethod::Operation,
                    saved,
                    IvyJson::object([("callId", call_id.clone()), ("method", "call.dial".into())]),
                    None,
                )
                .await?;
            if dial.is_null() || dial.get("phase") == Some(&"submitted".into()) {
                return Ok(VoiceResult::Pending);
            }
            if dial.get("phase") == Some(&"outcome_unknown".into()) {
                return Err(IvyError::new(
                    "secretary_voice_outcome_unknown",
                    "The original phone connection outcome is unknown.",
                )
                .with_outcome(Outcome::Unknown));
            }
            if !receipt_sent(&dial, "connected") {
                self.save(
                    latest,
                    changed(saved, "failed", "phone_not_answered".into()),
                )
                .await?;
                return Ok(VoiceResult::Fallback);
            }
            let prompted = self
                .call(
                    VoiceMethod::Operation,
                    saved,
                    IvyJson::object([
                        ("callId", call_id),
                        ("method", "call.promptVoice".into()),
                        ("generation", IvyJson::Number(0.0)),
                    ]),
                    None,
                )
                .await?;
            if prompted.is_null() || prompted.get("phase") == Some(&"submitted".into()) {
                return Ok(VoiceResult::Pending);
            }
            if prompted.get("phase") == Some(&"outcome_unknown".into()) {
                return Err(IvyError::new(
                    "secretary_voice_prompt_outcome_unknown",
                    "The original Voice prompt outcome is unknown.",
                )
                .with_outcome(Outcome::Unknown));
            }
            if !receipt_sent(&prompted, "sent") {
                self.save(
                    latest,
                    changed(saved, "failed", "phone_prompt_failed".into()),
                )
                .await?;
                return Ok(VoiceResult::Fallback);
            }
        }
        let mut next = changed(saved, "confirmed", IvyJson::Null);
        next.insert("expiresAt", expires_at);
        self.save(latest, next).await?;
        Ok(VoiceResult::Confirmed)
    }
}
fn receipt_sent(operation: &IvyJson, state: &str) -> bool {
    let receipt = operation.get("receipt");
    receipt.and_then(|r| r.get("ok")) == Some(&IvyJson::Bool(true))
        && receipt
            .and_then(|r| r.get("result"))
            .and_then(|r| r.get("state"))
            == Some(&state.into())
}
