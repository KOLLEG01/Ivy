use super::{
    contracts::{field, uuid},
    *,
};
use ivy_contracts::{
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};

impl PhoneJournal {
    pub async fn reusable_voice_task(&self, principal_id: JsString) -> IvyResult<Option<JsString>> {
        self.request(move |db| {
            validate_shared("Identifier", &principal_id.clone().into())?;
            let principal_id = String::from_utf16_lossy(&principal_id.0);
            let row: Option<(String, f64)> = db
                .db
                .query_row(
                    "SELECT json_extract(b.value,'$.intent.callId') AS call_id,
      COALESCE(json_extract(b.value,'$.intent.voiceGeneration'),0) AS generation FROM (
        SELECT value FROM commands WHERE method='call.bindVoice'
        UNION ALL SELECT value FROM receipt_archive WHERE kind='command'
          AND json_extract(value,'$.intent.method')='call.bindVoice'
      ) b JOIN (
        SELECT call_id,value FROM calls
        UNION ALL SELECT key AS call_id,value FROM receipt_archive WHERE kind='call'
      ) c ON c.call_id=json_extract(b.value,'$.intent.callId')
      WHERE json_extract(c.value,'$.principalId')=? AND json_extract(b.value,'$.phase')='result'
        AND json_extract(b.value,'$.receipt.ok')=1
      ORDER BY json_extract(b.value,'$.updatedAt') DESC LIMIT 1",
                    [principal_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(error)?;
            let Some((id, generation)) = row else {
                return Ok(None);
            };
            uuid(Some(&id.clone().into()))?;
            require(
                generation.is_finite()
                    && generation.fract() == 0.0
                    && (0.0..=128.0).contains(&generation),
                "invalid_arguments",
                "Bounded Voice generation required.",
            )?;
            db.voice_task(&id.into(), generation as u32)
        })
        .await
    }
    pub async fn retired_voice_task(&self, thread_id: JsString) -> IvyResult<Option<IvyJson>> {
        self.request(move |db| db.retired_voice_task(&thread_id))
            .await
    }
    pub async fn retired_voice_page(&self, limit: u32, after: JsString) -> IvyResult<Vec<IvyJson>> {
        self.request(move |db| {
            require(
                (1..=64).contains(&limit),
                "invalid_arguments",
                "Retired Voice scan requires a bounded page.",
            )?;
            let rows = db
                .db
                .prepare_cached(
                    "SELECT value FROM meta WHERE key>? AND key GLOB 'voiceRetired:*' AND
      json_extract(value,'$.phase') IN ('pending_archive','archiving')
      ORDER BY key LIMIT ?",
                )
                .map_err(error)?
                .query_map(
                    params![
                        format!("voiceRetired:{}", String::from_utf16_lossy(&after.0)),
                        limit
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?;
            rows.into_iter()
                .map(|row| {
                    db.retired_row(parse(&row, usize::MAX).map_err(|_| IvyError::internal())?)
                })
                .collect()
        })
        .await
    }
    pub async fn begin_voice_archive(
        &self,
        thread_id: JsString,
        operation_id: JsString,
    ) -> IvyResult<()> {
        self.request(move |db| {
            uuid(Some(&operation_id.clone().into()))?;
            db.transaction(|db| {
                let task = db.retired_voice_task(&thread_id)?;
                require(
                    task.as_ref().and_then(|v| v.get("phase")) == Some(&"pending_archive".into()),
                    "phone_voice_retention_changed",
                    "Voice task is not awaiting archival.",
                )?;
                let mut task = task.ok_or_else(IvyError::internal)?;
                task.insert("phase", "archiving".into());
                task.insert("archiveOperationId", operation_id.into());
                db.set_meta(
                    &format!("voiceRetired:{}", thread_id.to_utf8()?),
                    &canonical(&task, MANAGEMENT_FRAME_BYTES)?,
                )
            })
        })
        .await
    }
}
impl JournalDb<'_> {
    fn retired_voice_task(&self, thread_id: &JsString) -> IvyResult<Option<IvyJson>> {
        uuid(Some(&thread_id.clone().into()))?;
        self.meta(&format!("voiceRetired:{}", thread_id.to_utf8()?))?
            .filter(|v| !v.is_empty())
            .map(|row| self.retired_row(parse(&row, usize::MAX).map_err(|_| IvyError::internal())?))
            .transpose()
    }
    fn retired_row(&self, task: IvyJson) -> IvyResult<IvyJson> {
        for name in ["threadId", "successorId", "callId"] {
            uuid(task.get(name))?;
        }
        let id = field(&task, "callId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let phase = task.get("phase").and_then(IvyJson::as_js_string);
        let allowed = [
            "pending_archive",
            "archiving",
            "archived",
            "deleting",
            "deleted",
            "retained",
        ]
        .iter()
        .any(|s| phase == Some(&JsString::from(*s)));
        require(
            allowed
                && self.call(id)?.is_some()
                && task.get("threadId") != task.get("successorId")
                && [
                    "archiveOperationId",
                    "deleteOperationId",
                    "archivedAt",
                    "deleteAfter",
                ]
                .iter()
                .all(|key| {
                    task.get(key)
                        .is_some_and(|v| v.is_null() || v.as_js_string().is_some())
                }),
            "phone_storage_invalid",
            "Retired Voice task evidence is invalid.",
        )?;
        Ok(task)
    }
}
