//! Fair acquisition from retained original captures, followed by verified retirement.
use crate::{
    inbox_evidence::truthy,
    media_input::{AcquiredMediaPreparer, Prepared, Source},
    media_schema::{MediaSchema, validate_media},
    media_spool::{SpoolJournal, Validation},
    microsoft_schema::MicrosoftRecord,
    outlook_media_native::{OutlookMediaDownloader, OutlookMediaNative},
    outlook_projection::{microsoft_mcp, outlook_has_media},
    outlook_source::OutlookSource,
    store::Record,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::rpc::RequestOptions;
use ivy_store::{
    rusqlite::{OptionalExtension, params},
    sqlite,
};
use std::cell::RefCell;

fn check(condition: bool, code: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(
            code,
            "Media context and acquisition require their complete original capture and coverage.",
        ))
    }
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn array(value: &IvyJson) -> IvyResult<&[IvyJson]> {
    value.as_array().ok_or_else(IvyError::internal)
}
fn issue(value: &IvyJson) -> IvyResult<Option<JsString>> {
    Ok(value
        .get("gap")
        .filter(|value| !value.is_null())
        .or_else(|| {
            value
                .get("attachments")
                .and_then(IvyJson::as_array)?
                .iter()
                .find_map(|attachment| attachment.get("errorCode").filter(|value| truthy(value)))
        })
        .and_then(IvyJson::as_js_string)
        .cloned())
}

pub struct OutlookMediaSource<'a, 'source, 'engine, 'store, 'journal, V> {
    pub native: OutlookMediaNative<'a, 'source, 'engine, 'store, 'journal, V>,
    preparer: &'a dyn AcquiredMediaPreparer,
    pending: Option<IvyJson>,
    not_before: i64,
    pub issue: Option<JsString>,
}
pub struct OutlookMediaContext<'context, 'a, 'source, 'engine, 'store, 'journal, V> {
    native: &'context OutlookMediaNative<'a, 'source, 'engine, 'store, 'journal, V>,
    preparer: &'context dyn AcquiredMediaPreparer,
    selected: IvyJson,
    original: IvyJson,
    pub item: IvyJson,
    pub coverage: IvyJson,
}
pub struct MediaAttachment {
    pub pin: IvyJson,
    pub value: IvyJson,
    pub bytes: Vec<u8>,
}
impl MediaAttachment {
    pub fn source(&self) -> IvyResult<Source> {
        let evidence = self.value.required("evidence")?;
        let attachment = evidence.required("attachment")?;
        Ok(Source {
            name: attachment
                .required("name")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?,
            media_type: attachment.required("mediaType")?.as_js_string().cloned(),
            byte_length: evidence
                .required("byteLength")?
                .as_f64()
                .ok_or_else(IvyError::internal)? as usize,
            content_hash: evidence.required("contentHash")?.as_str()?,
        })
    }
}
impl<V: Validation> OutlookMediaContext<'_, '_, '_, '_, '_, '_, V> {
    pub async fn read_attachment(&self, index: f64) -> IvyResult<Option<MediaAttachment>> {
        let value = self.original.required("value")?;
        let attachments = array(value.required("attachments")?)?;
        check(
            index.is_finite()
                && index.fract() == 0.0
                && index >= 0.0
                && index < attachments.len() as f64,
            "media_context_mismatch",
        )?;
        let engine = self.native.source.native.engine;
        engine.evidence.verify_owner().await?;
        let current = self.native.publication().coverage(&self.selected).await?;
        check(
            match current {
                Some(ref current) => same(current, &self.original)?,
                None => false,
            },
            "media_context_mismatch",
        )?;
        let attachment = &attachments[index as usize];
        let Some(pin) = attachment.get("manifest").filter(|value| truthy(value)) else {
            return Ok(None);
        };
        let raw = engine
            .evidence
            .store
            .client
            .request("objects.read", pin.clone(), RequestOptions::default())
            .await?;
        check(
            raw.required("content")?.get("encoding") == Some(&"json".into()),
            "media_context_mismatch",
        )?;
        validate_media(
            MediaSchema::Manifest,
            raw.required("content")?.required("value")?,
        )?;
        let manifest = self
            .native
            .publication()
            .manifest(pin, Some(value.required("admission")?))
            .await?;
        check(
            manifest.required("admission")?.get("itemObjectId") == self.selected.get("objectId")
                && same(
                    manifest.required("evidence")?.required("attachment")?,
                    attachment.required("attachment")?,
                )?,
            "media_context_mismatch",
        )?;
        let bytes = self
            .native
            .publication()
            .payload(pin, Some(value.required("admission")?))
            .await?;
        Ok(Some(MediaAttachment {
            pin: pin.clone(),
            value: manifest,
            bytes,
        }))
    }
    pub async fn input(&self, index: f64, maximum_input_bytes: f64) -> IvyResult<Option<Prepared>> {
        let Some(payload) = self.read_attachment(index).await? else {
            return Ok(None);
        };
        self.preparer
            .prepare(&payload.source()?, &payload.bytes, maximum_input_bytes)
            .await
            .map(Some)
    }
}
impl<'a, 'source, 'engine, 'store, 'journal, V: Validation>
    OutlookMediaSource<'a, 'source, 'engine, 'store, 'journal, V>
{
    pub fn new(
        source: &'a OutlookSource<'source, 'engine, 'store, 'journal>,
        spool: &'a RefCell<SpoolJournal<V>>,
        downloader: &'a dyn OutlookMediaDownloader,
        preparer: &'a dyn AcquiredMediaPreparer,
    ) -> Self {
        Self {
            native: OutlookMediaNative::new(source, spool, downloader),
            preparer,
            pending: None,
            not_before: 0,
            issue: None,
        }
    }
    pub async fn context(
        &self,
        item: &IvyJson,
    ) -> IvyResult<Option<OutlookMediaContext<'_, 'a, 'source, 'engine, 'store, 'journal, V>>> {
        let engine = self.native.source.native.engine;
        engine.evidence.verify_owner().await?;
        let admitted = engine.evidence.item(item.required("objectId")?).await?;
        let link = array(admitted.required("value")?.required("media")?)?
            .iter()
            .find(|value| {
                value.get("contractKey") == Some(&"secretary/media-coverage".into())
                    && value.get("pin").is_some_and(truthy)
            });
        let Some(link) = link else {
            return Ok(None);
        };
        let pin = link.required("pin")?;
        let raw = engine
            .evidence
            .store
            .client
            .request("objects.read", pin.clone(), RequestOptions::default())
            .await?;
        check(
            raw.required("object")?.get("ownerObjectId") == item.get("objectId")
                && raw.required("content")?.get("encoding") == Some(&"json".into())
                && raw.required("revision")?.get("contentHash") == link.get("contentHash"),
            "media_context_mismatch",
        )?;
        let value = raw.required("content")?.required("value")?;
        validate_media(MediaSchema::Coverage, value)?;
        check(
            value.required("admission")?.get("sourceId")
                == self.native.source.native.configuration.get("sourceId")
                && value.required("admission")?.get("itemObjectId") == item.get("objectId"),
            "media_context_mismatch",
        )?;
        let original = IvyJson::object([("pin", pin.clone()), ("value", value.clone())]);
        Ok(Some(OutlookMediaContext {
            native: &self.native,
            preparer: self.preparer,
            selected: item.clone(),
            original: original.clone(),
            item: admitted.required("pin")?.clone(),
            coverage: original,
        }))
    }
    fn scan_key(&self) -> IvyResult<String> {
        Ok(format!(
            "secretary/outlook-media-scan/{}",
            hash_json(
                self.native
                    .source
                    .native
                    .configuration
                    .required("sourceId")?
            )?
        ))
    }
    /// SEC08: one bounded page, ordered by immutable ID, with a restart cursor.
    /// Advance before examining each entry, so a failed old entry cannot starve
    /// later captures. Wrap only after exhausting the tail; no eviction is needed.
    fn publications(&self) -> IvyResult<Vec<String>> {
        let db = &self.native.source.native.engine.evidence.store.journal.db;
        let after: Option<String> = db
            .query_row(
                "SELECT value FROM journal_state WHERE key=?",
                [self.scan_key()?],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite::error)?;
        let page = |after: &str| -> IvyResult<Vec<String>> {
            db.prepare("SELECT id FROM technical_documents WHERE key='secretary/outlook-publication' AND id>? ORDER BY id LIMIT 8").map_err(sqlite::error)?
                .query_map([after], |row| row.get(0)).map_err(sqlite::error)?.collect::<Result<Vec<_>, _>>().map_err(sqlite::error)
        };
        let rows = page(after.as_deref().unwrap_or(""))?;
        if rows.is_empty() && after.is_some() {
            page("")
        } else {
            Ok(rows)
        }
    }
    async fn select(&mut self) -> IvyResult<Option<IvyJson>> {
        let source = &self.native.source.native;
        let engine = source.engine;
        for id in self.publications()? {
            engine.evidence.store.journal.db.execute("INSERT INTO journal_state(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![self.scan_key()?, id]).map_err(sqlite::error)?;
            let publication = source.store().read(
                MicrosoftRecord::OutlookPublication,
                &id.into(),
                false,
                None,
            )?;
            let raw = source.store().read(
                MicrosoftRecord::OutlookEvidence,
                publication.value.required("evidence")?,
                true,
                None,
            )?;
            let plan = source.store().read(
                MicrosoftRecord::OutlookPlan,
                raw.value.required("plan")?,
                true,
                None,
            )?;
            if plan.value.required("configuration")?.get("sourceId")
                != source.configuration.get("sourceId")
                || array(publication.value.required("captured")?)?.is_empty()
            {
                continue;
            }
            let proof = source
                .evidence(publication.value.required("evidence")?)
                .await?;
            if proof.value.get("gap").is_some_and(truthy)
                || !proof
                    .value
                    .get("page")
                    .and_then(|page| page.get("messagesWithAttachments"))
                    .is_some_and(truthy)
            {
                continue;
            }
            let mut list = None;
            for pin in array(proof.value.required("calls")?)? {
                let call = source
                    .store()
                    .read(MicrosoftRecord::OutlookCall, pin, false, None)?;
                if call.value.get("slot").and_then(IvyJson::as_f64) == Some(3.0) && list.is_none() {
                    list = Some(call);
                }
            }
            check(
                list.as_ref()
                    .is_some_and(|call| call.value.get("observation").is_some_and(truthy)),
                "media_source_evidence_missing",
            )?;
            let list = list.unwrap();
            let operation = source.store().operation(
                proof.value.required("plan")?,
                &IvyJson::Number(3.0),
                list.value.required("observation")?,
            )?;
            let result = operation.get("reply").and_then(|reply| reply.get("result"));
            check(result.is_some(), "media_source_evidence_missing")?;
            let page = microsoft_mcp(result.unwrap())?;
            let entries = array(page.required("value")?)?;
            for pin in array(publication.value.required("captured")?)? {
                let capture = engine.evidence.store.read(Record::Operation, pin).await?;
                let capture = capture.required("value")?;
                check(
                    capture.get("phase") == Some(&"succeeded".into())
                        && capture.get("action") == Some(&"capture".into())
                        && capture.get("effect").is_some_and(truthy),
                    "media_source_evidence_missing",
                )?;
                let item = engine.evidence.item(capture.required("effect")?).await?;
                let value = item.required("value")?;
                let message = value.required("message")?;
                let mut original = None;
                for entry in entries {
                    if let Some(id) = entry.get("id").filter(|id| id.as_js_string().is_some()) {
                        if message.get("messageId")
                            == Some(
                                &hash_json(&IvyJson::Array(vec![
                                    "outlook-message".into(),
                                    id.clone(),
                                ]))?
                                .into(),
                            )
                        {
                            original = Some(entry);
                            break;
                        }
                    }
                }
                let Some(original) = original else {
                    continue;
                };
                if !outlook_has_media(original)? {
                    continue;
                }
                let old = self
                    .native
                    .spool
                    .borrow()
                    .read_plan(item.required("pin")?)?;
                if let Some(covered) = self
                    .native
                    .publication()
                    .coverage(item.required("pin")?)
                    .await?
                {
                    if let Some(old) = old {
                        let saved = self
                            .native
                            .publication()
                            .save_coverage(covered.required("value")?, &old)
                            .await?;
                        check(same(&saved, &covered)?, "media_coverage_invalid")?;
                        self.native
                            .verify_coverage(&old, saved.required("value")?)
                            .await?;
                        let current = engine
                            .evidence
                            .item(old.required("item")?.required("objectId")?)
                            .await?;
                        let value = current.required("value")?;
                        let admission = saved.required("value")?.required("admission")?;
                        let media = IvyJson::object([
                            ("contractKey", "secretary/media-coverage".into()),
                            ("pin", saved.required("pin")?.clone()),
                            ("contentHash", hash_json(saved.required("value")?)?.into()),
                            ("name", "Outlook attachments".into()),
                            ("mediaType", "application/json".into()),
                            ("errorCode", IvyJson::Null),
                        ]);
                        let mut links = 0;
                        let mut coverage_links = 0;
                        for link in array(value.required("media")?)? {
                            if same(link, &media)? {
                                links += 1;
                            }
                            if link.get("contractKey") == Some(&"secretary/media-coverage".into()) {
                                coverage_links += 1;
                            }
                        }
                        check(
                            current.required("pin")?.get("objectId")
                                == old.required("item")?.get("objectId")
                                && value.get("sourceId") == admission.get("sourceId")
                                && value.get("identityHash") == admission.get("identityHash")
                                && value.get("messageHash") == admission.get("messageHash")
                                && links == 1
                                && coverage_links == 1,
                            "media_coverage_invalid",
                        )?;
                        self.native.spool.borrow().archive_published(&old)?;
                    }
                    if self.issue.is_none() {
                        self.issue = issue(covered.required("value")?)?;
                    }
                    continue;
                }
                let admission = IvyJson::object([
                    (
                        "sourceId",
                        source.configuration.required("sourceId")?.clone(),
                    ),
                    (
                        "producerPrincipalId",
                        engine
                            .evidence
                            .settings
                            .required("identity")?
                            .required("principalId")?
                            .clone(),
                    ),
                    ("accountId", message.required("accountId")?.clone()),
                    ("providerMessageId", original.required("id")?.clone()),
                    (
                        "itemObjectId",
                        item.required("pin")?.required("objectId")?.clone(),
                    ),
                    ("identityHash", value.required("identityHash")?.clone()),
                    ("messageHash", value.required("messageHash")?.clone()),
                ]);
                return self
                    .native
                    .prepare(
                        &admission,
                        item.required("pin")?,
                        capture.required("operationId")?,
                    )
                    .await
                    .map(Some);
            }
        }
        self.not_before = engine.operation_clock.utc_millis().saturating_add(
            source
                .configuration
                .required("pollMs")?
                .as_f64()
                .ok_or_else(IvyError::internal)? as i64,
        );
        Ok(None)
    }
    pub async fn step(&mut self) -> IvyResult<&'static str> {
        if self.pending.is_none()
            && self
                .native
                .source
                .native
                .engine
                .operation_clock
                .utc_millis()
                < self.not_before
        {
            return Ok("idle");
        }
        self.native
            .source
            .native
            .engine
            .evidence
            .verify_owner()
            .await?;
        if self.pending.is_none() {
            self.pending = self.select().await?;
        }
        let Some(plan) = self.pending.as_ref() else {
            return Ok("idle");
        };
        let result = self.native.step(plan).await?;
        if result.get("phase") == Some(&"pending".into()) {
            return Ok("pending");
        }
        let saved = self
            .native
            .publication()
            .save_coverage(result.required("coverage")?, plan)
            .await?;
        self.native
            .verify_coverage(plan, saved.required("value")?)
            .await?;
        self.native.spool.borrow().archive_published(plan)?;
        if self.issue.is_none() {
            self.issue = issue(saved.required("value")?)?;
        }
        self.pending = None;
        Ok("complete")
    }
}
