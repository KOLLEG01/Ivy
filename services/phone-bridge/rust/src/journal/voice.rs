use super::contracts::uuid;
use super::*;
use crate::voice_rpc::{VoiceJournalFuture, VoicePreparationJournal};
use ivy_contracts::{
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};

fn hash(value: Option<&IvyJson>) -> IvyResult<bool> {
    let Some(IvyJson::String(value)) = value else {
        return Ok(false);
    };
    let value = value.to_utf8()?;
    Ok(value.len() == 71
        && value.starts_with("sha256:")
        && value.as_bytes()[7..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c)))
}
fn cache_ids(cache: &IvyJson) -> IvyResult<()> {
    for name in [
        "threadId",
        "creationOperationId",
        "preparationOperationId",
        "preparationTurnId",
    ] {
        if let Some(value) = cache.get(name).filter(|value| !value.is_null()) {
            uuid(Some(value))?;
        }
    }
    Ok(())
}
fn parse_stored(value: &str) -> IvyResult<IvyJson> {
    // Writes enforce the record bound before storing JSON.
    parse(value, usize::MAX).map_err(|_| IvyError::internal())
}

impl PhoneJournal {
    pub async fn voice_conversation(&self, fingerprint: &str) -> IvyResult<Option<IvyJson>> {
        let fingerprint = fingerprint.to_owned();
        self.request(move |db| {
            let Some(value) = db
                .meta(&format!("voiceConversation:{fingerprint}"))?
                .filter(|value| !value.is_empty())
            else {
                return Ok(None);
            };
            let context = parse_stored(&value)?;
            validate_conversation(&context)?;
            require(
                context.get("fingerprint") == Some(&fingerprint.into()),
                "phone_storage_invalid",
                "Voice context belongs to another runtime.",
            )?;
            Ok(Some(context))
        })
        .await
    }
    pub async fn retain_voice_conversation(&self, context: IvyJson) -> IvyResult<()> {
        self.request(move |db| {
            validate_conversation(&context)?;
            let call_id = context
                .get("callId")
                .and_then(IvyJson::as_js_string)
                .ok_or_else(IvyError::internal)?;
            let generation = context
                .get("generation")
                .and_then(IvyJson::as_f64)
                .ok_or_else(IvyError::internal)? as u32;
            let stop = db.call_command(call_id, &"call.stopVoice".into(), generation)?;
            require(
                stop.as_ref().is_some_and(|stop| {
                    stop.get("phase") == Some(&"result".into())
                        && stop.get("receipt").and_then(|receipt| receipt.get("ok"))
                            == Some(&true.into())
                        && stop.get("intent").and_then(|intent| intent.get("method"))
                            == Some(&"call.stopVoice".into())
                        && stop.get("intent").and_then(|intent| intent.get("threadId"))
                            == context.get("threadId")
                }),
                "phone_voice_stop_unknown",
                "Only a positively stopped Voice session may be continued.",
            )?;
            db.set_meta(
                &format!(
                    "voiceConversation:{}",
                    field_string(&context, "fingerprint")?
                ),
                &canonical(&context, 128 * 1024)?,
            )
        })
        .await
    }
    pub async fn forget_voice_conversation(&self, fingerprint: &str) -> IvyResult<()> {
        let fingerprint = fingerprint.to_owned();
        self.request(move |db| {
            db.db
                .prepare_cached("DELETE FROM meta WHERE key=?")
                .map_err(error)?
                .execute([format!("voiceConversation:{fingerprint}")])
                .map_err(error)?;
            Ok(())
        })
        .await
    }
    pub async fn task_cache(
        &self,
        principal_id: &str,
        fingerprint: &str,
    ) -> IvyResult<Option<IvyJson>> {
        let (principal_id, fingerprint) = (principal_id.to_owned(), fingerprint.to_owned());
        self.request(move |db| db.task_cache(&principal_id, &fingerprint))
            .await
    }
    pub async fn retain_task(&self, principal_id: &str, cache: IvyJson) -> IvyResult<()> {
        let principal_id = principal_id.to_owned();
        self.request(move |db| db.retain_task(&principal_id, &cache))
            .await
    }
    pub async fn remembered_voice_reasoning(&self) -> IvyResult<Option<JsString>> {
        self.request(|db| {
            let value = db.meta("voiceReasoning")?.map(JsString::from);
            require(
                value.as_ref().is_none_or(|v| (1..=64).contains(&v.0.len())),
                "phone_storage_invalid",
                "Remembered Voice reasoning is invalid.",
            )?;
            Ok(value)
        })
        .await
    }
    pub async fn remember_voice_reasoning(&self, value: JsString) -> IvyResult<()> {
        self.request(move |db| {
            require(
                (1..=64).contains(&value.0.len()),
                "invalid_arguments",
                "Unsupported Voice reasoning.",
            )?;
            db.set_meta("voiceReasoning", &value.to_utf8()?)
        })
        .await
    }
    pub async fn remember_codex_task(
        &self,
        fingerprint: &str,
        thread_id: JsString,
    ) -> IvyResult<()> {
        let fingerprint = fingerprint.to_owned();
        self.request(move |db| db.remember_codex_task(&fingerprint, &IvyJson::String(thread_id)))
            .await
    }
    pub async fn codex_binding_page(&self, after: &str, limit: usize) -> IvyResult<Vec<IvyJson>> {
        let after = after.to_owned();
        self.request(move |db| {
            require(
                (1..=64).contains(&limit),
                "invalid_arguments",
                "Codex binding scan requires a bounded page.",
            )?;
            let mut statement = db
                .db
                .prepare_cached(
                    "SELECT value FROM (
      SELECT operation_id AS id,value FROM commands WHERE method='call.bindVoice'
      UNION ALL SELECT key AS id,value FROM receipt_archive WHERE kind='command'
        AND json_extract(value,'$.intent.method')='call.bindVoice'
    ) WHERE id>? AND json_extract(value,'$.phase')='result' AND json_extract(value,'$.receipt.ok')=1
      ORDER BY id LIMIT ?",
                )
                .map_err(error)?;
            let rows = statement
                .query_map(params![after, limit as i64], |row| row.get::<_, String>(0))
                .map_err(error)?;
            rows.map(|row| parse_stored(&row.map_err(error)?)).collect()
        })
        .await
    }
    pub async fn codex_archive_page(
        &self,
        fingerprint: &str,
        after: &str,
        limit: usize,
    ) -> IvyResult<Vec<IvyJson>> {
        let (fingerprint, after) = (fingerprint.to_owned(), after.to_owned());
        self.request(move |db| {
            require(
                hash(Some(&fingerprint.clone().into()))? && (1..=64).contains(&limit),
                "invalid_arguments",
                "Codex archive scan requires a runtime fingerprint and bounded page.",
            )?;
            let prefix = format!("codexArchive:{fingerprint}:");
            let mut statement = db
                .db
                .prepare_cached(
                    "SELECT value FROM meta WHERE key GLOB ? AND key>? ORDER BY key LIMIT ?",
                )
                .map_err(error)?;
            let rows = statement
                .query_map(
                    params![format!("{prefix}*"), prefix + &after, limit as i64],
                    |row| row.get::<_, String>(0),
                )
                .map_err(error)?;
            rows.map(|row| {
                let task = parse_stored(&row.map_err(error)?)?;
                uuid(task.get("threadId"))?;
                if task.get("operationId") != Some(&IvyJson::Null) {
                    uuid(task.get("operationId"))?;
                }
                Ok(task)
            })
            .collect()
        })
        .await
    }
    pub async fn begin_codex_archive(
        &self,
        fingerprint: &str,
        task: IvyJson,
        operation_id: JsString,
    ) -> IvyResult<()> {
        let fingerprint = fingerprint.to_owned();
        self.request(move |db| {
            let operation_id = IvyJson::String(operation_id);
            uuid(Some(&operation_id))?;
            let thread_id = field_string(&task, "threadId")?;
            let key = format!("codexArchive:{fingerprint}:{thread_id}");
            require(
                db.meta(&key)?.as_deref() == Some(&canonical(&task, MANAGEMENT_FRAME_BYTES)?)
                    && task.get("operationId") == Some(&IvyJson::Null),
                "phone_voice_archive_changed",
                "Codex archival requires its original owned task.",
            )?;
            let mut updated = task;
            updated.insert("operationId", operation_id);
            db.set_meta(&key, &canonical(&updated, MANAGEMENT_FRAME_BYTES)?)
        })
        .await
    }
    pub async fn finish_codex_archive(&self, fingerprint: &str, task: IvyJson) -> IvyResult<()> {
        let fingerprint = fingerprint.to_owned();
        self.request(move |db| {
            require(
                task.get("operationId") != Some(&IvyJson::Null),
                "phone_voice_archive_changed",
                "Codex archival requires its original request.",
            )?;
            let thread_id = field_string(&task, "threadId")?;
            let key = format!("{fingerprint}:{thread_id}");
            db.transaction(|db| {
                let encoded = canonical(&task, MANAGEMENT_FRAME_BYTES)?;
                let archive = &db.archive;
                if let Some(completed) = archive
                    .read("codex-voice-archive", &key)?
                    .filter(|value| !value.is_empty())
                {
                    return require(
                        completed == encoded,
                        "phone_voice_archive_changed",
                        "Codex archive receipt changed.",
                    );
                }
                require(
                    db.meta(&format!("codexArchive:{key}"))?.as_deref() == Some(&encoded),
                    "phone_voice_archive_changed",
                    "Codex archive request changed.",
                )?;
                archive.retain("codex-voice-archive", &key, &encoded)?;
                db.db
                    .prepare_cached("DELETE FROM meta WHERE key=?")
                    .map_err(error)?
                    .execute([format!("codexArchive:{key}")])
                    .map_err(error)?;
                Ok(())
            })
        })
        .await
    }
    pub async fn used_codex_voice_task(&self, thread_id: JsString) -> IvyResult<bool> {
        self.request(move |db| {
            uuid(Some(&IvyJson::String(thread_id.clone())))?;
            let thread_id = thread_id.to_utf8()?;
            Ok(db
                .db
                .prepare_cached(
                    "SELECT 1 FROM commands WHERE method='call.promptVoice'
      AND json_extract(value,'$.intent.threadId')=?
      UNION ALL SELECT 1 FROM receipt_archive WHERE kind='command'
      AND json_extract(value,'$.intent.method')='call.promptVoice'
      AND json_extract(value,'$.intent.threadId')=? LIMIT 1",
                )
                .map_err(error)?
                .query_row(params![thread_id, thread_id], |_| Ok(()))
                .optional()
                .map_err(error)?
                .is_some())
        })
        .await
    }
}
fn validate_conversation(context: &IvyJson) -> IvyResult<()> {
    uuid(context.get("threadId"))?;
    uuid(context.get("callId"))?;
    let generation = context.get("generation").and_then(IvyJson::as_f64);
    let items = context.get("items").and_then(IvyJson::as_array);
    require(
        hash(context.get("fingerprint"))?
            && hash(context.get("partyKey"))?
            && generation.is_some_and(|n| n.floor() == n && (0.0..=128.0).contains(&n))
            && items.is_some_and(|items| {
                items.len() <= 126
                    && items.iter().all(|item| {
                        let role = item.get("role");
                        (role == Some(&"user".into()) || role == Some(&"assistant".into()))
                            && item.get("text").and_then(IvyJson::as_js_string).is_some()
                    })
                    && items
                        .iter()
                        .try_fold(0usize, |bytes, item| {
                            bytes.checked_add(item.get("text")?.as_str().ok()?.len())
                        })
                        .is_some_and(|bytes| bytes <= 24000)
            }),
        "phone_storage_invalid",
        "Voice continuation exceeds its bounded transcript context.",
    )
}
fn field_string(value: &IvyJson, key: &str) -> IvyResult<String> {
    value.get(key).ok_or_else(IvyError::internal)?.as_str()
}
impl JournalDb<'_> {
    fn task_cache(&self, principal_id: &str, fingerprint: &str) -> IvyResult<Option<IvyJson>> {
        validate_shared("Identifier", &principal_id.into())?;
        let Some(value) = self
            .meta(&format!("codexVoice:{principal_id}:{fingerprint}"))?
            .filter(|v| !v.is_empty())
        else {
            return Ok(None);
        };
        let cache = parse_stored(&value)?;
        require(
            cache.get("fingerprint") == Some(&fingerprint.into())
                && ["threadId", "creationOperationId"]
                    .iter()
                    .all(|key| matches!(cache.get(key), Some(IvyJson::Null | IvyJson::String(_)))),
            "phone_storage_invalid",
            "Cached Codex Voice task has invalid ownership.",
        )?;
        cache_ids(&cache)?;
        Ok(Some(cache))
    }
    fn retain_task(&self, principal_id: &str, cache: &IvyJson) -> IvyResult<()> {
        validate_shared("Identifier", &principal_id.into())?;
        cache_ids(cache)?;
        let created = cache
            .get("threadId")
            .and_then(IvyJson::as_js_string)
            .is_some();
        let creating = cache
            .get("creationOperationId")
            .and_then(IvyJson::as_js_string)
            .is_some();
        require(
            hash(cache.get("fingerprint"))?
                && ["threadId", "creationOperationId"]
                    .iter()
                    .all(|key| matches!(cache.get(key), Some(IvyJson::Null | IvyJson::String(_))))
                && (created != creating),
            "invalid_arguments",
            "Cached Voice task must identify either a created task or its original pending creation.",
        )?;
        let fingerprint = field_string(cache, "fingerprint")?;
        self.transaction(|db| {
            if let Some(thread) = cache.get("threadId").filter(|value| !value.is_null()) {
                db.remember_codex_task(&fingerprint, thread)?;
            }
            db.set_meta(
                &format!("codexVoice:{principal_id}:{fingerprint}"),
                &canonical(cache, MANAGEMENT_FRAME_BYTES)?,
            )
        })
    }
    fn remember_codex_task(&self, fingerprint: &str, thread_id: &IvyJson) -> IvyResult<()> {
        uuid(Some(thread_id))?;
        require(
            hash(Some(&fingerprint.into()))?,
            "invalid_arguments",
            "Codex task requires its runtime fingerprint.",
        )?;
        let key = format!("{fingerprint}:{}", thread_id.as_str()?);
        if self
            .meta(&format!("codexArchive:{key}"))?
            .is_none_or(|v| v.is_empty())
            && self
                .archive
                .read("codex-voice-archive", &key)?
                .is_none_or(|v| v.is_empty())
        {
            self.set_meta(
                &format!("codexArchive:{key}"),
                &canonical(
                    &IvyJson::object([
                        ("threadId", thread_id.clone()),
                        ("operationId", IvyJson::Null),
                    ]),
                    MANAGEMENT_FRAME_BYTES,
                )?,
            )?;
        }
        Ok(())
    }
}

impl VoicePreparationJournal for PhoneJournal {
    fn task_cache<'a>(
        &'a self,
        principal_id: &'a str,
        fingerprint: &'a str,
    ) -> VoiceJournalFuture<'a, Option<IvyJson>> {
        Box::pin(PhoneJournal::task_cache(self, principal_id, fingerprint))
    }
    fn retain_task<'a>(
        &'a self,
        principal_id: &'a str,
        cache: IvyJson,
    ) -> VoiceJournalFuture<'a, ()> {
        Box::pin(PhoneJournal::retain_task(self, principal_id, cache))
    }
}
