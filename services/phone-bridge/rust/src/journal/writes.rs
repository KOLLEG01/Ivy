use super::contracts::{field, intent_shape, uuid, validate, validate_native};
use super::*;
use ivy_contracts::{
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};

pub(super) fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn receipt(result: IvyJson) -> IvyJson {
    IvyJson::object([
        ("ok", true.into()),
        ("result", result),
        ("error", IvyJson::Null),
    ])
}
fn string(value: &IvyJson, name: &str) -> IvyResult<JsString> {
    field(value, name)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn generation(intent: &IvyJson) -> u32 {
    intent
        .get("voiceGeneration")
        .and_then(IvyJson::as_f64)
        .unwrap_or(0.0) as u32
}
impl PhoneJournal {
    /// Admit one possible write. A retained original never authorizes a replay.
    pub async fn submit(&self, intent: IvyJson) -> IvyResult<()> {
        self.request(move |db| db.submit(&intent)).await
    }
    pub async fn finish(&self, intent: IvyJson, reply: IvyJson) -> IvyResult<IvyJson> {
        self.request(move |db| {
            intent_shape(&intent)?;
            validate_native("Response", &reply)?;
            let ok = reply.get("ok") == Some(&true.into());
            require(
                reply.get("epoch") == intent.get("epoch")
                    && ok == (reply.get("error") == Some(&IvyJson::Null))
                    && (ok || reply.get("result") == Some(&IvyJson::Null)),
                "phone_receipt_conflict",
                "Phone reply differs from the original owner or result envelope.",
            )?;
            db.finish_receipt(
                &intent,
                IvyJson::object([
                    ("ok", field(&reply, "ok")?.clone()),
                    ("result", field(&reply, "result")?.clone()),
                    ("error", field(&reply, "error")?.clone()),
                ]),
                None,
            )
        })
        .await
    }
    /// App Tools receipts share the journal; these operations are not native IPC.
    pub async fn finish_archive(&self, intent: IvyJson, result: IvyJson) -> IvyResult<IvyJson> {
        self.request(move |db| {
            validate("PhoneVoiceArchiveResult", &result)?;
            let planned = field(field(&intent, "archivePlan")?, "candidates")?.as_array().ok_or_else(IvyError::internal)?
                .iter().map(|candidate| field(candidate, "threadId").cloned()).collect::<IvyResult<Vec<_>>>()?;
            let archived = field(&result, "archivedIds")?.as_array().ok_or_else(IvyError::internal)?;
            let prefix = &planned[..archived.len().min(planned.len())];
            require(canonical(&IvyJson::Array(archived.to_vec()), MANAGEMENT_FRAME_BYTES)? == canonical(&IvyJson::Array(prefix.to_vec()), MANAGEMENT_FRAME_BYTES)?
                && (result.get("state") != Some(&"archived".into()) || archived.len() == planned.len() && result.get("failedThreadId") == Some(&IvyJson::Null)),
                "phone_receipt_conflict", "Archive acknowledgements must match the original ordered plan; partial work is never complete.")?;
            db.finish_receipt(&intent, receipt(result), None)
        }).await
    }
    pub async fn finish_voice_binding(
        &self,
        intent: IvyJson,
        startup_thread_id: Option<JsString>,
    ) -> IvyResult<IvyJson> {
        self.request(move |db| {
            if let Some(id) = &startup_thread_id {
                uuid(Some(&id.clone().into()))?;
                require(
                    intent.get("threadId") != Some(&id.clone().into()),
                    "phone_voice_task_changed",
                    "The transient startup chat must differ from the reused chat.",
                )?;
            }
            let result = IvyJson::object([
                ("threadId", field(&intent, "threadId")?.clone()),
                ("bound", true.into()),
            ]);
            validate("PhoneVoiceBindingResult", &result)?;
            db.finish_receipt(&intent, receipt(result), startup_thread_id.as_ref())
        })
        .await
    }
    pub async fn finish_voice_creation(
        &self,
        intent: IvyJson,
        thread_id: JsString,
    ) -> IvyResult<IvyJson> {
        self.request(move |db| {
            let result =
                IvyJson::object([("threadId", thread_id.into()), ("created", true.into())]);
            validate("PhoneVoiceCreationResult", &result)?;
            db.finish_receipt(&intent, receipt(result), None)
        })
        .await
    }
    pub async fn finish_voice_stop(&self, intent: IvyJson) -> IvyResult<IvyJson> {
        self.request(move |db| {
            let result = IvyJson::object([
                ("threadId", field(&intent, "threadId")?.clone()),
                ("stopped", true.into()),
            ]);
            db.finish_receipt(&intent, receipt(result), None)
        })
        .await
    }
    pub async fn finish_voice_prompt(
        &self,
        intent: IvyJson,
        state: JsString,
    ) -> IvyResult<IvyJson> {
        self.request(move |db| {
            let result = IvyJson::object([
                ("threadId", field(&intent, "threadId")?.clone()),
                ("state", state.into()),
            ]);
            validate("PhoneVoicePromptResult", &result)?;
            db.finish_receipt(&intent, receipt(result), None)
        })
        .await
    }
    pub async fn finish_voice_selection(
        &self,
        intent: IvyJson,
        state: JsString,
    ) -> IvyResult<IvyJson> {
        self.finish_voice_prompt(intent, state).await
    }
    pub async fn finish_speaker_voice_selection(
        &self,
        intent: IvyJson,
        result: IvyJson,
    ) -> IvyResult<IvyJson> {
        self.request(move |db| db.finish_receipt(&intent, receipt(result), None))
            .await
    }
    /// Flow's speaker change has no intervening external effect after its
    /// connected check. Keep the original submit/finish boundary uninterrupted.
    pub(crate) async fn retain_speaker_voice_selection(
        &self,
        intent: IvyJson,
        result: IvyJson,
    ) -> IvyResult<IvyJson> {
        self.request(move |db| {
            db.submit(&intent)?;
            db.finish_receipt(&intent, receipt(result), None)
        })
        .await
    }
    pub async fn finish_voice_restart(
        &self,
        intent: IvyJson,
        thread_id: Option<JsString>,
    ) -> IvyResult<IvyJson> {
        self.request(move |db| {
            let value = match thread_id {
                Some(id) => {
                    uuid(Some(&id.clone().into()))?;
                    receipt(IvyJson::object([
                        ("threadId", id.into()),
                        ("restarted", true.into()),
                    ]))
                }
                None => IvyJson::object([
                    ("ok", false.into()),
                    ("result", IvyJson::Null),
                    ("error", "operation_outcome_unknown".into()),
                ]),
            };
            db.finish_receipt(&intent, value, None)
        })
        .await
    }
    pub async fn voice_selection_command(
        &self,
        call_id: JsString,
        sequence: u32,
    ) -> IvyResult<Option<IvyJson>> {
        self.request(move |db| db.sequence_command(&call_id, sequence, true))
            .await
    }
    pub async fn feedback_command(
        &self,
        call_id: JsString,
        sequence: u32,
    ) -> IvyResult<Option<IvyJson>> {
        self.request(move |db| db.sequence_command(&call_id, sequence, false))
            .await
    }
    pub async fn selected_speaker_voice(&self, call_id: JsString) -> IvyResult<Option<JsString>> {
        self.request(move |db| {
            uuid(Some(&call_id.clone().into()))?;
            let call_id = call_id.to_utf8()?;
            let row: Option<String> = db.db.query_row("SELECT value FROM commands WHERE call_id=? AND method='call.selectSpeakerVoice' AND phase='result' AND json_extract(value,'$.receipt.ok')=1 ORDER BY rowid DESC LIMIT 1", [&call_id], |row| row.get(0)).optional().map_err(error)?;
            let row = match row {Some(row) => Some(row), None => db.db.query_row("SELECT value FROM receipt_archive WHERE kind='command' AND json_extract(value,'$.intent.callId')=? AND json_extract(value,'$.intent.method')='call.selectSpeakerVoice' AND json_extract(value,'$.receipt.ok')=1 ORDER BY rowid DESC LIMIT 1", [&call_id], |row| row.get(0)).optional().map_err(error)?};
            match row {Some(row) => Ok(parse(&row, usize::MAX).map_err(|_| IvyError::internal())?.get("intent").and_then(|v| v.get("voiceId")).and_then(IvyJson::as_js_string).cloned()), None => Ok(None)}
        }).await
    }
    pub async fn unresolved_archive(&self) -> IvyResult<bool> {
        self.request(|db| {
            Ok(db
                .db
                .query_row(
                    "SELECT 1 FROM commands WHERE method='call.archiveVoice' AND
      (phase!='result' OR json_extract(value,'$.receipt.result.state') IS NOT 'archived') AND
      json_extract(value,'$.archiveResolution.state') IS NOT 'observed_archived' LIMIT 1",
                    [],
                    |_| Ok(()),
                )
                .optional()
                .map_err(error)?
                .is_some())
        })
        .await
    }
    /// Record a later read-only observation without replacing the original receipt.
    pub async fn resolve_archive(&self, operation_id: JsString) -> IvyResult<IvyJson> {
        self.request(move |db| {
            db.transaction(|db| {
                let original = db.operation(&operation_id)?;
                require(
                    original
                        .as_ref()
                        .and_then(|v| v.get("intent"))
                        .and_then(|v| v.get("method"))
                        == Some(&"call.archiveVoice".into()),
                    "phone_archive_missing",
                    "Original archive intent required.",
                )?;
                let mut original = original.ok_or_else(IvyError::internal)?;
                if original
                    .get("archiveResolution")
                    .and_then(IvyJson::as_object)
                    .is_some()
                {
                    return Ok(original);
                }
                let now = timestamp();
                original.insert(
                    "archiveResolution",
                    IvyJson::object([
                        ("state", "observed_archived".into()),
                        ("checkedAt", now.clone().into()),
                    ]),
                );
                original.insert("updatedAt", now.into());
                db.save(original)
            })
        })
        .await
    }
}
impl JournalDb<'_> {
    pub(super) fn save(&self, value: IvyJson) -> IvyResult<IvyJson> {
        validate("Operation", &value)?;
        let encoded = canonical(&value, OUTCOME_BYTES)?;
        self.db
            .execute(
                "UPDATE commands SET phase=?,value=? WHERE operation_id=?",
                params![
                    field(&value, "phase")?.as_str()?,
                    encoded,
                    field(field(&value, "intent")?, "operationId")?.as_str()?
                ],
            )
            .map_err(error)?;
        Ok(value)
    }
    pub(super) fn sequence_command(
        &self,
        call_id: &JsString,
        sequence: u32,
        selection: bool,
    ) -> IvyResult<Option<IvyJson>> {
        uuid(Some(&call_id.clone().into()))?;
        require(
            (1..=2_147_483_647).contains(&sequence),
            "invalid_arguments",
            if selection {
                "Positive bounded Voice selection command sequence required."
            } else {
                "Positive bounded command feedback sequence required."
            },
        )?;
        let sql = if selection {
            "SELECT operation_id FROM commands WHERE call_id=? AND method='call.selectVoice' AND json_extract(value,'$.intent.commandSequence')=? LIMIT 1"
        } else {
            "SELECT operation_id FROM commands WHERE call_id=? AND method='call.command.feedback' AND json_extract(value,'$.intent.commandSequence')=? LIMIT 1"
        };
        let row: Option<String> = self
            .db
            .query_row(sql, params![call_id.to_utf8()?, sequence], |row| row.get(0))
            .optional()
            .map_err(error)?;
        let id = match row {
            Some(row) => Some(row),
            None => self.archive.read(
                if selection {
                    "voice-selection"
                } else {
                    "command-feedback"
                },
                &canonical(
                    &IvyJson::Array(vec![
                        call_id.clone().into(),
                        IvyJson::Number(f64::from(sequence)),
                    ]),
                    MANAGEMENT_FRAME_BYTES,
                )?,
            )?,
        };
        match id.filter(|id| !id.is_empty()) {
            Some(id) => self.operation(&id.into()),
            None => Ok(None),
        }
    }
    fn submit(&self, intent: &IvyJson) -> IvyResult<()> {
        intent_shape(intent)?;
        self.compact()?;
        self.transaction(|db| {
            let operation_id = string(intent, "operationId")?;
            if let Some(prior) = db.operation(&operation_id)? {
                require(canonical(field(&prior, "intent")?, MANAGEMENT_FRAME_BYTES)? == canonical(intent, MANAGEMENT_FRAME_BYTES)?,
                    "mutation_conflict", "The retained Phone operation has different original arguments or ownership.")?;
                let saved_receipt = prior.get("receipt");
                let result = saved_receipt.and_then(|v| v.get("result"));
                let nested = result.and_then(|v| v.get("result"));
                let unknown = prior.get("phase") != Some(&"result".into())
                    || saved_receipt.and_then(|v| v.get("error")) == Some(&"operation_outcome_unknown".into())
                    || result.and_then(|v| v.get("state")) == Some(&"outcome_unknown".into())
                    || nested.and_then(|v| v.get("phase")) == Some(&"outcome_unknown".into())
                    || nested.and_then(|v| v.get("state")) == Some(&"outcome_unknown".into());
                return Err(IvyError::new("phone_operation_retained", "Read the retained original Phone outcome; this command cannot be sent again.")
                    .with_outcome(if unknown {Outcome::Unknown} else {Outcome::Completed}));
            }
            require(intent.get("epoch") == Some(&db.epoch_json()?), "phone_epoch_mismatch", "Only the active Phone process can admit a command.")?;
            let method = field(intent, "method")?.as_str()?;
            if let Some(call_id) = intent.get("callId").and_then(IvyJson::as_js_string) {
                let admitted = db.call(call_id)?;
                if let Some(admitted) = &admitted {
                    if !["call.hangup", "call.release", "call.stopVoice", "call.realtime.stop", "call.desktop.stopVoice"].contains(&method.as_str()) {
                        require(db.current_call(Some(&string(admitted, "callId")?))?.is_some() && admitted.get("epoch") == intent.get("epoch"),
                            "phone_call_conflict", "Only the original active admission can issue another positive call action.")?;
                        require(if admitted.get("direction") == Some(&"outgoing".into()) {method != "call.answer"} else {!["call.prepare", "call.dial"].contains(&method.as_str())},
                            "phone_call_conflict", "Phone command direction differs from the original call admission.")?;
                    }
                }
                let previous: Option<String> = db.db.query_row("SELECT epoch FROM commands WHERE call_id=? LIMIT 1", [call_id.to_utf8()?], |row| row.get(0)).optional().map_err(error)?;
                require(admitted.as_ref().is_none_or(|v| v.get("epoch") == intent.get("epoch"))
                    && previous.as_ref().is_none_or(|v| intent.get("epoch") == Some(&v.clone().into())),
                    "phone_call_conflict", "A Phone call identity cannot move to another process epoch.")?;
                if method.starts_with("call.realtime.") || method.starts_with("call.screening.") || method.starts_with("call.desktop.")
                    || ["call.stopVoice", "call.features", "call.windows.connect", "call.codec.upgrade", "call.waiting.end", "call.archiveVoice", "call.createVoice", "call.bindVoice", "call.promptVoice", "call.prepare", "call.claim", "call.audio.prepare", "call.audio.rebind", "call.dial", "call.answer"].contains(&method.as_str()) {
                    let (first, second) = if ["call.dial", "call.answer"].contains(&method.as_str()) {("call.dial", "call.answer")} else {(method.as_str(), method.as_str())};
                    let exists = db.db.query_row("SELECT operation_id FROM commands WHERE call_id=? AND method IN (?,?) AND COALESCE(json_extract(value,'$.intent.voiceGeneration'),0)=? LIMIT 1", params![call_id.to_utf8()?, first, second, generation(intent)], |_| Ok(())).optional().map_err(error)?.is_some();
                    require(!exists, "phone_call_conflict", "The original call already has its prepare or connect attempt; a new operation cannot repeat it.")?;
                }
                if method == "call.selectVoice" && intent.get("commandSequence").is_some() {
                    let sequence = field(intent, "commandSequence")?.as_f64().ok_or_else(IvyError::internal)? as u32;
                    require(db.sequence_command(call_id, sequence, true)?.is_none(), "phone_call_conflict", "This Voice selection command was already submitted.")?;
                }
                if method == "call.restartVoice" {
                    require(db.call_command(call_id, &method.clone().into(), generation(intent))?.is_none(), "phone_call_conflict", "This Voice generation already has its restart command.")?;
                }
                if method == "call.command.feedback" {
                    let sequence = field(intent, "commandSequence")?.as_f64().ok_or_else(IvyError::internal)? as u32;
                    require(db.sequence_command(call_id, sequence, false)?.is_none(), "phone_call_conflict", "This original keypad command already has feedback.")?;
                }
            }
            let usage = db.usage()?;
            let cleanup = ["call.hangup", "call.release", "call.stopVoice", "call.realtime.stop", "call.desktop.stopVoice", "call.archiveVoice"].contains(&method.as_str());
            let reserve = (db.current_calls()?.len() * 4).saturating_sub(if cleanup {4} else {0});
            require(usage.operations + (reserve as f64) < db.limits.max_operations as f64
                && usage.reserved_bytes + (reserve + 1) as f64 * OUTCOME_BYTES as f64 <= db.limits.max_bytes as f64,
                "phone_journal_capacity", "Phone cannot reserve another complete command outcome.")?;
            let now = timestamp();
            let value = IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("intent", intent.clone()), ("phase", "submitted".into()),
                ("receipt", IvyJson::Null), ("createdAt", now.clone().into()), ("updatedAt", now.into())]);
            validate("Operation", &value)?;
            let call_id = intent.get("callId").and_then(IvyJson::as_js_string).map(JsString::to_utf8).transpose()?;
            db.db.execute("INSERT INTO commands VALUES (?,?,?,?,?,?)", params![operation_id.to_utf8()?, field(intent, "epoch")?.as_str()?, call_id, method, "submitted", canonical(&value, OUTCOME_BYTES)?]).map_err(error)?;
            db.set_counter("count", usage.operations + 1.0)?;
            db.set_counter("epochOperations", db.counter("epochOperations")? + 1.0)
        })
    }
    fn finish_receipt(
        &self,
        intent: &IvyJson,
        receipt: IvyJson,
        startup_thread_id: Option<&JsString>,
    ) -> IvyResult<IvyJson> {
        intent_shape(intent)?;
        validate("Receipt", &receipt)?;
        self.transaction(|db| {
            let prior = db.operation(&string(intent, "operationId")?)?;
            let expected_intent = canonical(intent, MANAGEMENT_FRAME_BYTES)?;
            require(
                prior
                    .as_ref()
                    .map(|v| canonical(field(v, "intent")?, MANAGEMENT_FRAME_BYTES))
                    .transpose()?
                    .is_some_and(|prior| prior == expected_intent),
                "phone_receipt_conflict",
                "Phone reply has no matching original intent.",
            )?;
            let mut prior = prior.ok_or_else(IvyError::internal)?;
            if prior.get("phase") == Some(&"result".into()) {
                require(
                    canonical(field(&prior, "receipt")?, MANAGEMENT_FRAME_BYTES)?
                        == canonical(&receipt, MANAGEMENT_FRAME_BYTES)?,
                    "phone_receipt_conflict",
                    "Phone cannot replace a saved original result.",
                )?;
                return Ok(prior);
            }
            require(
                prior.get("phase") == Some(&"submitted".into())
                    && intent.get("epoch") == Some(&db.epoch_json()?),
                "phone_receipt_conflict",
                "A retired Phone owner cannot resolve an unknown operation.",
            )?;
            prior.insert("phase", "result".into());
            prior.insert("receipt", receipt.clone());
            prior.insert("updatedAt", timestamp().into());
            let saved = db.save(prior)?;
            if intent.get("method") == Some(&"call.bindVoice".into())
                && receipt.get("ok") == Some(&true.into())
                && intent
                    .get("callId")
                    .and_then(IvyJson::as_js_string)
                    .is_some()
            {
                let call = db.call(&string(intent, "callId")?)?;
                require(
                    call.is_some(),
                    "phone_storage_invalid",
                    "Bound Voice task lost its original call.",
                )?;
                let call = call.ok_or_else(IvyError::internal)?;
                let principal = String::from_utf16_lossy(&string(&call, "principalId")?.0);
                if let Some(previous) = db
                    .meta(&format!("voiceReuse:{principal}"))?
                    .filter(|v| !v.is_empty())
                {
                    let old = parse(&previous, usize::MAX).map_err(|_| IvyError::internal())?;
                    let number = old.get("generation").and_then(IvyJson::as_f64);
                    require(
                        old.get("threadId")
                            .and_then(IvyJson::as_js_string)
                            .is_some()
                            && old.get("callId").and_then(IvyJson::as_js_string).is_some()
                            && number.is_some_and(|v| v.is_finite() && v.fract() == 0.0),
                        "phone_storage_invalid",
                        "Previous Voice task pointer is invalid.",
                    )?;
                    uuid(old.get("threadId"))?;
                    if old.get("threadId") != intent.get("threadId") {
                        uuid(old.get("callId"))?;
                        let number = number.ok_or_else(IvyError::internal)?;
                        require(
                            (0.0..=128.0).contains(&number),
                            "invalid_arguments",
                            "Bounded Voice generation required.",
                        )?;
                        if db
                            .created_voice_task(&string(&old, "callId")?, number as u32)?
                            .as_ref()
                            == old.get("threadId").and_then(IvyJson::as_js_string)
                        {
                            db.retire_voice_task(
                                &call,
                                field(&old, "threadId")?.clone(),
                                field(intent, "threadId")?.clone(),
                            )?;
                        }
                    }
                }
                db.set_meta(
                    &format!("voiceReuse:{principal}"),
                    &canonical(
                        &IvyJson::object([
                            ("threadId", field(intent, "threadId")?.clone()),
                            ("callId", field(intent, "callId")?.clone()),
                            (
                                "generation",
                                intent
                                    .get("voiceGeneration")
                                    .cloned()
                                    .unwrap_or(IvyJson::Number(0.0)),
                            ),
                        ]),
                        MANAGEMENT_FRAME_BYTES,
                    )?,
                )?;
                if let Some(id) = startup_thread_id.filter(|id| !id.0.is_empty()) {
                    db.retire_voice_task(
                        &call,
                        id.clone().into(),
                        field(intent, "threadId")?.clone(),
                    )?;
                }
            }
            Ok(saved)
        })
    }
    fn retire_voice_task(
        &self,
        call: &IvyJson,
        thread_id: IvyJson,
        successor_id: IvyJson,
    ) -> IvyResult<()> {
        let key = format!("voiceRetired:{}", thread_id.as_str()?);
        if self.meta(&key)?.is_none_or(|v| v.is_empty()) {
            self.set_meta(
                &key,
                &canonical(
                    &IvyJson::object([
                        ("threadId", thread_id),
                        ("principalId", field(call, "principalId")?.clone()),
                        ("successorId", successor_id),
                        ("callId", field(call, "callId")?.clone()),
                        ("phase", "pending_archive".into()),
                        ("archiveOperationId", IvyJson::Null),
                        ("deleteOperationId", IvyJson::Null),
                        ("archivedAt", IvyJson::Null),
                        ("deleteAfter", IvyJson::Null),
                    ]),
                    MANAGEMENT_FRAME_BYTES,
                )?,
            )?;
        }
        Ok(())
    }
}
