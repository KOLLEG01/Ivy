use super::{Principal, Store, require, sql_error, text};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::hash_json,
    json::{canonical, js_string, number_from_string, parse, try_js_string},
    limits::{
        MANAGEMENT_FRAME_BYTES, MUTATION_RECEIPT_COUNT, MUTATION_RECEIPT_JOURNAL_BYTES,
        MUTATION_RECEIPT_RESULT_BYTES,
    },
    operation::parse_operation_id_js,
};
use rusqlite::{OptionalExtension, params};

/// Captured by authenticated request admission before dispatch to the owner.
pub struct AuthenticatedContext {
    pub principal_id: JsString,
    pub credential_digest: JsString,
}
#[derive(Clone, Copy, Default)]
pub enum ReceiptKind {
    #[default]
    Snapshot,
    Contract,
    Event,
}
impl ReceiptKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Contract => "contract",
            Self::Event => "event",
        }
    }
}
pub struct LedgerResult {
    pub value: IvyJson,
    pub kind: ReceiptKind,
    pub reference: Option<IvyJson>,
}
impl LedgerResult {
    pub fn snapshot(value: IvyJson) -> Self {
        Self {
            value,
            kind: ReceiptKind::Snapshot,
            reference: None,
        }
    }
}
impl Store {
    pub fn runtime_epoch(&self) -> IvyResult<JsString> {
        self.metadata("runtime_epoch")?
            .map(JsString::from)
            .ok_or_else(IvyError::internal)
    }
    pub(crate) fn metadata_number(&self, key: &str) -> IvyResult<f64> {
        Ok(number_from_string(
            &self.metadata(key)?.unwrap_or_else(|| "0".into()).into(),
        ))
    }
    pub fn mutation_receipt_bytes(&self) -> IvyResult<f64> {
        self.metadata_number("mutation_receipt_bytes")
    }
    pub fn cleanup_mutation_receipts(
        &self,
        reserved_bytes: usize,
        reserved_count: usize,
    ) -> IvyResult<()> {
        require(
            reserved_bytes <= MUTATION_RECEIPT_JOURNAL_BYTES,
            "result_too_large",
            "Mutation receipt exceeds the journal byte limit.",
        )?;
        let mut count = self
            .db
            .query_row("SELECT COUNT(*) FROM mutations", [], |r| r.get::<_, i64>(0))
            .map_err(sql_error)? as f64;
        let mut bytes = self.mutation_receipt_bytes()?;
        let mut expired = self.metadata_number("mutations_expired_before")?;
        let maximum_count = MUTATION_RECEIPT_COUNT as f64 - reserved_count as f64;
        let maximum_bytes = MUTATION_RECEIPT_JOURNAL_BYTES as f64 - reserved_bytes as f64;
        if count <= maximum_count && bytes <= maximum_bytes {
            return Ok(());
        }
        while count > maximum_count || bytes > maximum_bytes {
            let limit = if bytes > maximum_bytes {
                256
            } else {
                (count - maximum_count).min(256.0) as i64
            };
            let oldest = self
                .db
                .prepare_cached(
                    "SELECT rowid,issued_at_ms,receipt_bytes FROM mutations ORDER BY rowid LIMIT ?",
                )
                .map_err(sql_error)?
                .query_map([limit], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, f64>(2)?,
                    ))
                })
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            require(
                !oldest.is_empty(),
                "storage_invalid",
                "Mutation receipt usage does not match stored receipts.",
            )?;
            let mut boundary = 0;
            for (rowid, issued, size) in oldest {
                boundary = rowid;
                count -= 1.0;
                bytes -= size;
                expired = if expired.is_nan() || issued.is_nan() {
                    f64::NAN
                } else {
                    expired.max(issued)
                };
                if count <= maximum_count && bytes <= maximum_bytes {
                    break;
                }
            }
            self.db
                .execute("DELETE FROM mutations WHERE rowid<=?", [boundary])
                .map_err(sql_error)?;
        }
        self.set_metadata(
            "mutation_receipt_bytes",
            &text(&js_string(&IvyJson::Number(bytes))),
        )?;
        self.set_metadata(
            "mutations_expired_before",
            &text(&js_string(&IvyJson::Number(expired))),
        )
    }
    /// Work, receipt eviction and replay fences share the same write transaction.
    /// The caller supplies its wall-clock observation, allowing deterministic replay tests.
    pub fn mutate(
        &mut self,
        context: &AuthenticatedContext,
        method: &JsString,
        parameters: &IvyJson,
        now_ms: u64,
        work: impl FnOnce(&mut Self, &Principal) -> IvyResult<LedgerResult>,
    ) -> IvyResult<IvyJson> {
        self.transaction(|store| {
            let principal=Principal {principal_id:context.principal_id.clone(),credential_digest:context.credential_digest.clone()};
            let mut arguments=parameters.clone();
            let mutation_id=arguments.remove("mutationId").and_then(IvyJson::into_js_string).ok_or_else(||IvyError::new("invalid_arguments","Operation identity must be <runtimeEpoch>:<issuedAtUnixMs>:<nonce>."))?;
            let identity=parse_operation_id_js(&mutation_id)?;
            require(identity.runtime_epoch==store.runtime_epoch()?, "mutation_expired", "Mutation identity belongs to another runtime.")?;
            let request_hash=hash_json(&IvyJson::object([("method",IvyJson::String(method.clone())),("arguments",arguments)]))?;
            let prior=store.db.query_row("SELECT request_hash,result_kind,result_json FROM mutations WHERE principal_id=? AND mutation_id=?",params![text(&principal.principal_id),text(&mutation_id)],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional().map_err(sql_error)?;
            if let Some((hash,kind,encoded))=prior {
                require(hash==request_hash,"mutation_conflict","The operation identity was already used with different arguments.")?;
                let reference=parse(&encoded,MANAGEMENT_FRAME_BYTES).map_err(sql_error)?;
                if kind=="contract" {
                    let key=reference.get("key").map(try_js_string).transpose()?.unwrap_or_else(||"undefined".into());
                    let version=reference.get("version").map(try_js_string).transpose()?.unwrap_or_else(||"undefined".into());
                    return store.contract(&key,&version);
                }
                return Ok(reference);
            }
            require(identity.issued_at_unix_ms as f64 <= now_ms as f64 + 60_000.0 && identity.issued_at_unix_ms as f64 > store.metadata_number("mutations_expired_before")?, "mutation_expired", "Mutation identity is in the future or predates the receipt ring's replay boundary.")?;
            let result=work(store,&principal)?;
            let reference=result.reference.as_ref().filter(|v|!v.is_null()).unwrap_or(&result.value);
            let receipt=canonical(reference,MUTATION_RECEIPT_RESULT_BYTES)?;
            let kind=result.kind.as_str();
            let mut units=principal.principal_id.0.clone();
            units.extend_from_slice(&mutation_id.0);
            units.extend(request_hash.encode_utf16()); units.extend(kind.encode_utf16()); units.extend(receipt.encode_utf16());
            let bytes=String::from_utf16_lossy(&units).len()+16;
            store.cleanup_mutation_receipts(bytes,1)?;
            store.db.execute("INSERT INTO mutations VALUES (?, ?, ?, ?, ?, ?, ?, ?)",params![text(&principal.principal_id),text(&mutation_id),request_hash,identity.issued_at_unix_ms as i64,i64::try_from(now_ms).map_err(sql_error)?,kind,receipt,bytes as i64]).map_err(sql_error)?;
            store.set_metadata("mutation_receipt_bytes", &text(&js_string(&IvyJson::Number(store.mutation_receipt_bytes()? + bytes as f64))))?;
            Ok(result.value)
        })
    }
    pub fn object_write_receipt(
        &self,
        context: &AuthenticatedContext,
        mutation_id: &JsString,
        expected_hash: &str,
    ) -> IvyResult<Option<IvyJson>> {
        let row=self.db.query_row("SELECT request_hash,result_kind,result_json FROM mutations WHERE principal_id=? AND mutation_id=?",params![text(&context.principal_id),text(mutation_id)],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional().map_err(sql_error)?;
        let Some((hash, kind, encoded)) = row else {
            return Ok(None);
        };
        require(
            hash == expected_hash && kind == "snapshot",
            "mutation_conflict",
            "The original mutation does not match this object write.",
        )?;
        Ok(Some(
            parse(&encoded, MANAGEMENT_FRAME_BYTES).map_err(sql_error)?,
        ))
    }
    /// Returns an owned definition; caller mutation cannot alter stored contracts.
    pub fn contract(&self, key: &JsString, version: &JsString) -> IvyResult<IvyJson> {
        let encoded = self
            .db
            .query_row(
                "SELECT definition FROM contracts WHERE key=? AND version=?",
                params![text(key), text(version)],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(sql_error)?;
        let encoded = encoded.ok_or_else(|| {
            IvyError::new(
                "contract_not_found",
                "The exact Data Contract is not registered.",
            )
        })?;
        parse(&encoded, MANAGEMENT_FRAME_BYTES).map_err(sql_error)
    }
    pub fn adjust_retention_usage(
        &self,
        key: &JsString,
        objects: i64,
        revisions: i64,
        bytes: i64,
    ) -> IvyResult<()> {
        self.db.execute("UPDATE retention_families SET summary_json=json_set(
        summary_json,
        '$.objectCount',max(0,COALESCE(json_extract(summary_json,'$.objectCount'),0)+CAST(? AS INTEGER)),
        '$.revisionCount',max(0,COALESCE(json_extract(summary_json,'$.revisionCount'),0)+CAST(? AS INTEGER)),
        '$.byteLength',max(0,COALESCE(json_extract(summary_json,'$.byteLength'),0)+CAST(? AS INTEGER))
      ) WHERE contract_key=?
        AND json_type(summary_json,'$.objectCount')='integer'
        AND json_type(summary_json,'$.revisionCount')='integer'
        AND json_type(summary_json,'$.byteLength')='integer'",params![objects,revisions,bytes,text(key)]).map_err(sql_error)?;
        Ok(())
    }
}
