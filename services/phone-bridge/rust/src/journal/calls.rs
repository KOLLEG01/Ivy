use super::contracts::{field, uuid, validate_admission, validate_native};
use super::*;
use ivy_contracts::{
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};

const CALL_BYTES: usize = 65_536;

impl PhoneJournal {
    pub async fn call(&self, call_id: JsString) -> IvyResult<Option<IvyJson>> {
        self.request(move |db| db.call(&call_id)).await
    }
    pub async fn call_for_operation(&self, operation_id: JsString) -> IvyResult<Option<IvyJson>> {
        self.request(move |db| db.call_for_operation(&operation_id))
            .await
    }
    pub async fn current_call(&self, call_id: Option<JsString>) -> IvyResult<Option<IvyJson>> {
        self.request(move |db| db.current_call(call_id.as_ref()))
            .await
    }
    pub async fn current_calls(&self) -> IvyResult<Vec<IvyJson>> {
        self.request(|db| db.current_calls()).await
    }
    pub async fn history(
        &self,
        principal_id: Option<JsString>,
        limit: u32,
    ) -> IvyResult<Vec<IvyJson>> {
        self.request(move |db| {
            require((1..=64).contains(&limit), "invalid_arguments", "History limit must be between 1 and 64.")?;
            let principal = principal_id.map(|id| String::from_utf16_lossy(&id.0));
            let mut statement = db.db.prepare_cached("SELECT value FROM (
      SELECT value,0 AS section,rowid AS sequence FROM calls
      UNION ALL SELECT value,1 AS section,rowid AS sequence FROM receipt_archive WHERE kind='call'
    ) WHERE (? IS NULL OR json_extract(value,'$.principalId')=?) ORDER BY section,sequence DESC LIMIT ?").map_err(error)?;
            let rows = statement.query_map(params![principal, principal, limit], |row| row.get::<_, String>(0)).map_err(error)?;
            let mut calls = Vec::new();
            let mut bytes = 0;
            for row in rows {
                let encoded = row.map_err(error)?;
                bytes += encoded.len() + 1;
                if bytes > 128 * 1024 { break; }
                let call = parse(&encoded, usize::MAX).map_err(|_| IvyError::internal())?;
                validate_admission("PhoneCall", &call)?;
                calls.push(call);
            }
            Ok(calls)
        }).await
    }
    pub async fn target(&self, call_id: JsString) -> IvyResult<Option<IvyJson>> {
        self.request(move |db| db.target(&call_id)).await
    }
    pub async fn retain_target(&self, value: IvyJson) -> IvyResult<IvyJson> {
        self.request(move |db| {
            validate_native("PhoneCallTarget", &value)?;
            let encoded = canonical(&value, CALL_BYTES - 512)?;
            let call_id = field(&value, "callId")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?;
            db.transaction(|db| {
                if let Some(prior) = db.target(call_id)? {
                    require(
                        canonical(&prior, MANAGEMENT_FRAME_BYTES)? == encoded,
                        "mutation_conflict",
                        "Original Phone target cannot be replaced.",
                    )?;
                    return Ok(prior);
                }
                require(
                    db.current_call(Some(call_id))?.is_some(),
                    "phone_call_conflict",
                    "Only the original current admission can retain its target.",
                )?;
                require(
                    db.usage()?.reserved_bytes + OUTCOME_BYTES as f64 <= db.limits.max_bytes as f64,
                    "phone_journal_capacity",
                    "Phone cannot reserve its original target before Voice effects.",
                )?;
                db.db
                    .execute(
                        "INSERT INTO targets VALUES (?,?)",
                        params![call_id.to_utf8()?, encoded],
                    )
                    .map_err(error)?;
                db.set_counter("targets", db.counter("targets")? + 1.0)?;
                parse(&encoded, usize::MAX)
            })
        })
        .await
    }
    /// Policy must already have admitted this request before any native effect.
    pub async fn admit_call(&self, admission: IvyJson) -> IvyResult<IvyJson> {
        self.request(move |db| db.admit_call(admission)).await
    }
}
impl JournalDb<'_> {
    pub(super) fn admit_call(&self, admission: IvyJson) -> IvyResult<IvyJson> {
        let db = self;
        validate_admission("PhoneCallAdmission", &admission)?;
        require(
            admission.get("direction") == Some(&"incoming".into())
                || admission.get("callId") == Some(&IvyJson::Null),
            "invalid_arguments",
            "Outgoing call UUIDs are allocated by original admission.",
        )?;
        db.compact()?;
        db.transaction(|db| {
            let operation_id = field(&admission, "operationId")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?;
            if let Some(prior) = db.call_for_operation(operation_id)? {
                let mut original = prior.clone();
                if prior.get("direction") == Some(&"outgoing".into()) {
                    original.insert("callId", IvyJson::Null);
                }
                require(
                    canonical(&original, MANAGEMENT_FRAME_BYTES)?
                        == canonical(&admission, MANAGEMENT_FRAME_BYTES)?,
                    "mutation_conflict",
                    "Phone admission arguments or ownership changed.",
                )?;
                return Ok(prior);
            }
            require(
                admission.get("epoch") == Some(&db.epoch_json()?),
                "phone_epoch_mismatch",
                "Call admission requires the current native epoch.",
            )?;
            let active = db.current_calls()?.len();
            require(
                active < db.max_concurrent_calls,
                "phone_call_busy",
                "Configured call concurrency is occupied.",
            )?;
            if let Some(call_id) = admission.get("callId").and_then(IvyJson::as_js_string) {
                require(
                    db.call(call_id)?.is_none(),
                    "phone_call_conflict",
                    "An incoming call already has its original admission.",
                )?;
            }
            let usage = db.usage()?;
            require(
                usage.calls < db.limits.max_operations as f64
                    && usage.operations + (active * 4 + 16) as f64
                        <= db.limits.max_operations as f64
                    && usage.reserved_bytes
                        + CALL_BYTES as f64
                        + (17 + active * 4) as f64 * OUTCOME_BYTES as f64
                        <= db.limits.max_bytes as f64,
                "phone_journal_capacity",
                "Phone call admission history is full.",
            )?;
            let mut call = admission.clone();
            if call.get("callId") == Some(&IvyJson::Null) {
                call.insert("callId", uuid::Uuid::new_v4().to_string().into());
            }
            validate_admission("PhoneCall", &call)?;
            let id = field(&call, "callId")?.as_str()?;
            db.db
                .execute(
                    "INSERT INTO calls VALUES (?,?,?)",
                    params![id, operation_id.to_utf8()?, canonical(&call, CALL_BYTES)?],
                )
                .map_err(error)?;
            db.set_counter("calls", usage.calls + 1.0)?;
            db.db
                .execute("INSERT INTO active_calls VALUES (?)", [&id])
                .map_err(error)?;
            Ok(call)
        })
    }
}
impl PhoneJournal {
    /// Only its saved positive native receipt can release the same-epoch slot.
    pub async fn release_call(&self, call_id: JsString, operation_id: JsString) -> IvyResult<()> {
        self.request(move |db| {
            uuid(Some(&call_id.clone().into()))?;
            uuid(Some(&operation_id.clone().into()))?;
            db.transaction(|db| {
                let call = db.call(&call_id)?;
                let release = db.operation(&operation_id)?;
                let intent = release.as_ref().and_then(|v| v.get("intent"));
                let receipt = release.as_ref().and_then(|v| v.get("receipt"));
                let result = receipt.and_then(|v| v.get("result"));
                require(
                    call.is_some()
                        && release.as_ref().and_then(|v| v.get("phase")) == Some(&"result".into())
                        && intent.and_then(|v| v.get("epoch"))
                            == call.as_ref().and_then(|v| v.get("epoch"))
                        && intent.and_then(|v| v.get("callId")) == Some(&call_id.clone().into())
                        && intent.and_then(|v| v.get("method")) == Some(&"call.release".into())
                        && receipt.and_then(|v| v.get("ok")) == Some(&true.into())
                        && receipt.and_then(|v| v.get("error")) == Some(&IvyJson::Null)
                        && result.and_then(|v| v.get("callId")) == Some(&call_id.clone().into())
                        && result.and_then(|v| v.get("released")) == Some(&true.into()),
                    "phone_release_unconfirmed",
                    "Call slot requires its original confirmed native release.",
                )?;
                db.db
                    .execute(
                        "DELETE FROM active_calls WHERE call_id=?",
                        [call_id.to_utf8()?],
                    )
                    .map_err(error)?;
                Ok(())
            })
        })
        .await
    }
    pub async fn discard_unprepared_call(&self, call_id: JsString) -> IvyResult<()> {
        self.request(move |db| {
            uuid(Some(&call_id.clone().into()))?;
            db.transaction(|db| {
                let call = db.call(&call_id)?;
                require(
                    call.is_some(),
                    "phone_release_unconfirmed",
                    "Original call admission is required.",
                )?;
                let count: i64 = db
                    .db
                    .query_row(
                        "SELECT count(*) FROM commands WHERE call_id=?",
                        [call_id.to_utf8()?],
                        |row| row.get(0),
                    )
                    .map_err(error)?;
                let incoming =
                    call.as_ref().and_then(|v| v.get("direction")) == Some(&"incoming".into());
                let command = db.call_command(
                    &call_id,
                    &if incoming {
                        "call.claim"
                    } else {
                        "call.prepare"
                    }
                    .into(),
                    0,
                )?;
                let receipt = command.as_ref().and_then(|v| v.get("receipt"));
                let refusal = receipt
                    .and_then(|v| v.get("error"))
                    .and_then(IvyJson::as_js_string);
                let allowed = if incoming {
                    refusal == Some(&"phone_offer_expired".into())
                } else {
                    refusal == Some(&"operation_conflict".into())
                        || refusal == Some(&"runtime_not_ready".into())
                };
                require(
                    count == 0
                        || count == 1
                            && command.as_ref().and_then(|v| v.get("phase"))
                                == Some(&"result".into())
                            && receipt.and_then(|v| v.get("ok")) == Some(&false.into())
                            && allowed,
                    "phone_release_unconfirmed",
                    if incoming {
                        "A claimed or uncertain incoming offer still requires native cleanup."
                    } else {
                        "Possible native call creation requires original cleanup."
                    },
                )?;
                db.db
                    .execute(
                        "DELETE FROM active_calls WHERE call_id=?",
                        [call_id.to_utf8()?],
                    )
                    .map_err(error)?;
                Ok(())
            })
        })
        .await
    }
}
impl JournalDb<'_> {
    pub(super) fn call(&self, call_id: &JsString) -> IvyResult<Option<IvyJson>> {
        uuid(Some(&call_id.clone().into()))?;
        let id = call_id.to_utf8()?;
        let row: Option<String> = self
            .db
            .query_row("SELECT value FROM calls WHERE call_id=?", [&id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(error)?;
        let encoded = match row {
            Some(row) => Some(row),
            None => self.archive.read("call", &id)?,
        };
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        let value = parse(&encoded, usize::MAX).map_err(|_| IvyError::internal())?;
        validate_admission("PhoneCall", &value)?;
        require(
            value.get("callId") == Some(&call_id.clone().into()),
            "phone_storage_invalid",
            "Phone admission belongs to another original call.",
        )?;
        Ok(Some(value))
    }
    pub(super) fn call_for_operation(&self, operation_id: &JsString) -> IvyResult<Option<IvyJson>> {
        uuid(Some(&operation_id.clone().into()))?;
        let id = operation_id.to_utf8()?;
        let row: Option<String> = self
            .db
            .query_row(
                "SELECT call_id FROM calls WHERE operation_id=?",
                [&id],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        let id = match row {
            Some(row) => Some(row),
            None => self.archive.read("call-operation", &id)?,
        };
        let call = match id.filter(|id| !id.is_empty()) {
            Some(id) => self.call(&id.into())?,
            None => None,
        };
        require(
            call.as_ref()
                .is_none_or(|call| call.get("operationId") == Some(&operation_id.clone().into())),
            "phone_storage_invalid",
            "Phone admission has another original operation.",
        )?;
        Ok(call)
    }
    pub(super) fn current_call(&self, call_id: Option<&JsString>) -> IvyResult<Option<IvyJson>> {
        let id: Option<String> = if let Some(id) = call_id.filter(|id| !id.0.is_empty()) {
            self.db
                .query_row(
                    "SELECT call_id FROM active_calls WHERE call_id=?",
                    [id.to_utf8()?],
                    |row| row.get(0),
                )
                .optional()
                .map_err(error)?
        } else {
            self.db
                .query_row("SELECT call_id FROM active_calls LIMIT 1", [], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(error)?
        };
        let call = match id {
            Some(id) => self.call(&id.into())?,
            None => None,
        };
        let epoch = self.epoch_json()?;
        Ok(call.filter(|call| call.get("epoch") == Some(&epoch)))
    }
    pub(super) fn current_calls(&self) -> IvyResult<Vec<IvyJson>> {
        let ids = self
            .db
            .prepare_cached("SELECT call_id FROM active_calls")
            .map_err(error)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let mut calls = Vec::new();
        for id in ids {
            if let Some(call) = self.current_call(Some(&id.into()))? {
                calls.push(call);
            }
        }
        Ok(calls)
    }
    fn target(&self, call_id: &JsString) -> IvyResult<Option<IvyJson>> {
        uuid(Some(&call_id.clone().into()))?;
        let id = call_id.to_utf8()?;
        let row: Option<String> = self
            .db
            .query_row("SELECT value FROM targets WHERE call_id=?", [&id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(error)?;
        let encoded = match row {
            Some(row) => Some(row),
            None => self.archive.read("target", &id)?,
        };
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        let value = parse(&encoded, usize::MAX).map_err(|_| IvyError::internal())?;
        validate_native("PhoneCallTarget", &value)?;
        require(
            value.get("callId") == Some(&call_id.clone().into()) && self.call(call_id)?.is_some(),
            "phone_storage_invalid",
            "Phone target belongs to another original admission.",
        )?;
        Ok(Some(value))
    }
}
