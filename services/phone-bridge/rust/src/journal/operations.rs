use super::contracts::{field, intent_shape, uuid, validate};
use super::*;
use ivy_contracts::{
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};

impl PhoneJournal {
    /// Read retained originals, including archived evidence and expiry tombstones.
    /// Reading an uncertain operation supplies no authority to submit it again.
    pub async fn operation(&self, operation_id: JsString) -> IvyResult<Option<IvyJson>> {
        self.request(move |db| db.operation(&operation_id)).await
    }
    pub async fn call_command(
        &self,
        call_id: JsString,
        method: JsString,
        generation: u32,
    ) -> IvyResult<Option<IvyJson>> {
        self.request(move |db| db.call_command(&call_id, &method, generation))
            .await
    }
    pub async fn voice_task(
        &self,
        call_id: JsString,
        generation: u32,
    ) -> IvyResult<Option<JsString>> {
        self.request(move |db| db.voice_task(&call_id, generation))
            .await
    }
    pub async fn created_voice_task(
        &self,
        call_id: JsString,
        generation: u32,
    ) -> IvyResult<Option<JsString>> {
        self.request(move |db| db.created_voice_task(&call_id, generation))
            .await
    }
    pub async fn latest_voice_generation(&self, call_id: JsString) -> IvyResult<Option<u32>> {
        self.request(move |db| {
            for generation in (0..=128).rev() {
                if db.voice_task(&call_id, generation)?.is_some() {
                    return Ok(Some(generation));
                }
            }
            Ok(None)
        })
        .await
    }
}
impl JournalDb<'_> {
    pub(super) fn operation(&self, operation_id: &JsString) -> IvyResult<Option<IvyJson>> {
        uuid(Some(&IvyJson::String(operation_id.clone())))?;
        let id = operation_id.to_utf8()?;
        let row: Option<String> = self
            .db
            .prepare_cached("SELECT value FROM commands WHERE operation_id=?")
            .map_err(error)?
            .query_row([&id], |row| row.get(0))
            .optional()
            .map_err(error)?;
        let encoded = match row {
            Some(row) => Some(row),
            None => self.archive.read("command", &id)?,
        };
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        let value = parse(&encoded, usize::MAX).map_err(|_| IvyError::internal())?;
        validate("Operation", &value)?;
        let intent = field(&value, "intent")?;
        intent_shape(intent)?;
        require(
            intent.get("operationId") == Some(&IvyJson::String(operation_id.clone())),
            "phone_storage_invalid",
            "Phone evidence has a different original operation identity.",
        )?;
        Ok(Some(value))
    }
    pub(super) fn call_command(
        &self,
        call_id: &JsString,
        method: &JsString,
        generation: u32,
    ) -> IvyResult<Option<IvyJson>> {
        uuid(Some(&IvyJson::String(call_id.clone())))?;
        require(
            generation <= 128,
            "invalid_arguments",
            "Bounded Voice generation required.",
        )?;
        let row: Option<String> = self.db.prepare_cached("SELECT operation_id FROM commands WHERE call_id=? AND method=? AND COALESCE(json_extract(value,'$.intent.voiceGeneration'),0)=? ORDER BY rowid LIMIT 1").map_err(error)?
            .query_row(params![call_id.to_utf8()?, method.to_utf8()?, generation], |row| row.get(0)).optional().map_err(error)?;
        let id = match row {
            Some(row) => Some(row),
            None => {
                let mut key = vec![
                    IvyJson::String(call_id.clone()),
                    IvyJson::String(method.clone()),
                ];
                if generation != 0 {
                    key.push(IvyJson::Number(f64::from(generation)));
                }
                self.archive.read(
                    "call-command",
                    &canonical(&IvyJson::Array(key), MANAGEMENT_FRAME_BYTES)?,
                )?
            }
        };
        match id.filter(|id| !id.is_empty()) {
            Some(id) => self.operation(&id.into()),
            None => Ok(None),
        }
    }
    pub(super) fn voice_task(
        &self,
        call_id: &JsString,
        generation: u32,
    ) -> IvyResult<Option<JsString>> {
        let Some(operation) = self.call_command(call_id, &"call.bindVoice".into(), generation)?
        else {
            return Ok(None);
        };
        let result = if operation.get("phase") == Some(&"result".into())
            && operation
                .get("receipt")
                .and_then(|receipt| receipt.get("ok"))
                == Some(&true.into())
        {
            operation
                .get("receipt")
                .and_then(|receipt| receipt.get("result"))
        } else {
            None
        };
        let intent = field(&operation, "intent")?;
        require(
            result.is_some_and(|result| {
                result.get("bound") == Some(&true.into())
                    && result
                        .get("threadId")
                        .and_then(IvyJson::as_js_string)
                        .is_some()
                    && intent.get("method") == Some(&"call.bindVoice".into())
                    && intent.get("threadId") == result.get("threadId")
            }),
            "phone_storage_invalid",
            "Voice task binding receipt is invalid.",
        )?;
        Ok(result
            .and_then(|result| result.get("threadId"))
            .and_then(IvyJson::as_js_string)
            .cloned())
    }
    pub(super) fn created_voice_task(
        &self,
        call_id: &JsString,
        generation: u32,
    ) -> IvyResult<Option<JsString>> {
        let Some(operation) = self.call_command(call_id, &"call.createVoice".into(), generation)?
        else {
            return Ok(None);
        };
        require(
            operation
                .get("intent")
                .and_then(|intent| intent.get("method"))
                == Some(&"call.createVoice".into()),
            "phone_storage_invalid",
            "Voice creation method changed.",
        )?;
        if operation.get("phase") != Some(&"result".into()) {
            return Err(IvyError::new(
                "phone_voice_create_unknown",
                "Original task creation is unresolved; no replacement task may be created.",
            )
            .with_outcome(Outcome::Unknown));
        }
        let result = operation
            .get("receipt")
            .and_then(|receipt| receipt.get("result"));
        require(
            result.is_some_and(|result| {
                result.get("created") == Some(&true.into())
                    && result
                        .get("threadId")
                        .and_then(IvyJson::as_js_string)
                        .is_some()
            }),
            "phone_storage_invalid",
            "Voice task creation receipt is invalid.",
        )?;
        Ok(result
            .and_then(|result| result.get("threadId"))
            .and_then(IvyJson::as_js_string)
            .cloned())
    }
}
