//! Final immutable media publication. Acquisition and native read work remain
//! in the separate spool until the caller verifies final Hive coverage.
use crate::{
    engine::SecretaryEngine,
    inbox_evidence::{identity_hash, message_hash, truthy},
    media_schema::{MediaSchema, validate_media},
    media_spool::{SpoolJournal, Validation},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
    json::{canonical, js_string, parse},
    operation::derive_operation_id_js,
};
use ivy_sdk::{operation_ids::new_operation_id, rpc::RequestOptions};
use std::{collections::HashSet, future::Future, pin::Pin};

const CHUNK_BYTES: usize = 1_048_576;
const FRAME: usize = 32 * 1024 * 1024;
fn check(value: bool) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(
            "media_publication_invalid",
            "Media publication needs its exact admitted original item and complete immutable bytes.",
        ))
    }
}
fn array(value: &IvyJson) -> IvyResult<&[IvyJson]> {
    value.as_array().ok_or_else(IvyError::internal)
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(canonical(a, FRAME)? == canonical(b, FRAME)?)
}
fn named_text(prefix: &str, value: &IvyJson) -> IvyResult<JsString> {
    let mut units = prefix.encode_utf16().collect::<Vec<_>>();
    units.extend(js_string(value).0);
    Ok(JsString(units))
}
fn name_for(kind: &str, intent: &IvyJson, index: Option<usize>) -> IvyResult<JsString> {
    Ok(format!(
        "media-{kind}-{}",
        &hash_json(&IvyJson::Array(vec![
            intent.clone(),
            index
                .map(|v| IvyJson::Number(v as f64))
                .unwrap_or(IvyJson::Null)
        ]))?[7..]
    )
    .into())
}
fn pin(object: &IvyJson, revision: &IvyJson) -> IvyResult<IvyJson> {
    Ok(IvyJson::object([
        ("objectId", object.required("id")?.clone()),
        ("revision", revision.required("revision")?.clone()),
    ]))
}
fn references(values: &[IvyJson], kind: &str) -> IvyJson {
    IvyJson::object(
        values
            .iter()
            .enumerate()
            .map(|(n, v)| (format!("{kind}-{n}"), v.clone())),
    )
}
fn coverage_references(value: &IvyJson) -> IvyResult<IvyJson> {
    Ok(IvyJson::object(
        array(value.required("attachments")?)?
            .iter()
            .enumerate()
            .filter_map(|(n, v)| {
                v.get("manifest")
                    .filter(|v| truthy(v))
                    .map(|v| (format!("manifest-{n}"), v.clone()))
            }),
    ))
}

// Buffer.from(text,'base64') accepts URL alphabet, ignores non-alphabet bytes,
// stops at padding and truncates UTF-16 units to bytes. Preserve that read rule.
fn original_base64(text: &JsString) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut bits = 0;
    let mut value = 0_u32;
    for unit in &text.0 {
        let byte = *unit as u8;
        let next = match byte {
            b'=' => break,
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        };
        value = (value << 6) | u32::from(next);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((value >> bits) as u8);
        }
    }
    bytes
}

pub type AdmissionFuture<'a> = Pin<Box<dyn Future<Output = IvyResult<()>> + 'a>>;
pub trait MediaAdmissionGuard {
    fn verify<'a>(
        &'a self,
        admission: &'a IvyJson,
        capture_operation_id: &'a IvyJson,
    ) -> AdmissionFuture<'a>;
}
pub struct MediaStore<'a, 'engine, 'store, 'journal> {
    pub engine: &'a SecretaryEngine<'engine, 'store, 'journal>,
    pub admission_guard: &'a dyn MediaAdmissionGuard,
}

/// The collector supplies its existing source configuration. This checks the
/// successful original capture and complete item identity before publication.
pub async fn verify_outlook_media_admission(
    engine: &SecretaryEngine<'_, '_, '_>,
    configuration: &IvyJson,
    admission: &IvyJson,
    capture_operation_id: &IvyJson,
) -> IvyResult<()> {
    validate_media(MediaSchema::Admission, admission)?;
    let evidence = engine.evidence;
    let definition = evidence.source_definition(admission.required("sourceId")?)?;
    check(
        definition.get("kind") == Some(&"email".into())
            && definition.get("accountId") == admission.get("accountId")
            && configuration.get("sourceId") == admission.get("sourceId")
            && admission.get("producerPrincipalId")
                == evidence.settings.required("identity")?.get("principalId")
            && array(definition.required("producerPrincipalIds")?)?
                .contains(admission.required("producerPrincipalId")?),
    )?;
    let capture = evidence
        .find(
            admission.required("producerPrincipalId")?,
            capture_operation_id,
        )
        .await?;
    check(capture.is_some())?;
    let capture = capture.unwrap();
    let value = capture.required("value")?;
    let request = value.required("request")?;
    check(
        value.get("phase") == Some(&"succeeded".into())
            && value.get("action") == Some(&"capture".into())
            && value.get("callerPrincipalId") == admission.get("producerPrincipalId")
            && value.get("ignoredReason") == Some(&IvyJson::Null)
            && value.get("effect").and_then(|v| v.get("objectId")) == admission.get("itemObjectId")
            && request.get("action") == Some(&"capture".into()),
    )?;
    check(
        request.get("sourceId") == admission.get("sourceId")
            && same(
                request.required("expectedScope")?,
                evidence.settings.required("identity")?.required("scope")?,
            )?
            && value.get("requestHash") == Some(&hash_json(request)?.into())
            && request.get("operationId") == Some(capture_operation_id),
    )?;
    let item = evidence.item(admission.required("itemObjectId")?).await?;
    let value = item.required("value")?;
    let message = request.required("message")?;
    check(
        value.get("sourceId") == admission.get("sourceId")
            && value.required("message")?.get("attachments") == Some(&"expected".into())
            && same(value.required("message")?, message)?
            && value.get("identityHash") == admission.get("identityHash")
            && value.get("messageHash") == admission.get("messageHash")
            && admission.get("identityHash")
                == Some(&identity_hash(admission.required("sourceId")?, message)?.into())
            && admission.get("messageHash") == Some(&message_hash(message)?.into())
            && message.get("accountId") == admission.get("accountId")
            && message.get("messageId")
                == Some(
                    &hash_json(&IvyJson::Array(vec![
                        "outlook-message".into(),
                        admission.required("providerMessageId")?.clone(),
                    ]))?
                    .into(),
                ),
    )?;
    check(message.get("attachments") == Some(&"expected".into()))
}

impl MediaStore<'_, '_, '_, '_> {
    async fn named(&self, key: &str, name: &JsString) -> IvyResult<Option<IvyJson>> {
        let page = self
            .engine
            .evidence
            .store
            .client
            .request(
                "objects.query",
                IvyJson::object([
                    ("contractKey", key.into()),
                    ("includeArchived", true.into()),
                    ("limit", IvyJson::Number(2.0)),
                    (
                        "where",
                        IvyJson::object([
                            ("op", "and".into()),
                            (
                                "args",
                                IvyJson::Array(vec![
                                    IvyJson::object([
                                        ("op", "eq".into()),
                                        ("field", "object.parentId".into()),
                                        (
                                            "value",
                                            IvyJson::String(
                                                self.engine.evidence.store.root.clone(),
                                            ),
                                        ),
                                    ]),
                                    IvyJson::object([
                                        ("op", "eq".into()),
                                        ("field", "object.name".into()),
                                        ("value", IvyJson::String(name.clone())),
                                    ]),
                                ]),
                            ),
                        ]),
                    ),
                ]),
                RequestOptions::default(),
            )
            .await?;
        let items = array(page.required("items")?)?;
        check(items.len() <= 1 && !page.get("nextCursor").is_some_and(truthy))?;
        items
            .first()
            .map(|v| {
                Ok(IvyJson::object([
                    ("objectId", v.required("objectId")?.clone()),
                    ("revision", v.required("revision")?.clone()),
                ]))
            })
            .transpose()
    }
    async fn read(&self, key: &str, name: &JsString, pin: &IvyJson) -> IvyResult<IvyJson> {
        let result = self
            .engine
            .evidence
            .store
            .client
            .request("objects.read", pin.clone(), RequestOptions::default())
            .await?;
        let object = result.required("object")?;
        let revision = result.required("revision")?;
        check(
            object.get("parentId")
                == Some(&IvyJson::String(self.engine.evidence.store.root.clone()))
                && object.get("name") == Some(&IvyJson::String(name.clone()))
                && object.get("contractKey") == Some(&key.into())
                && !object.get("effectivelyArchived").is_some_and(truthy)
                && pin.get("revision") == Some(&IvyJson::Number(1.0))
                && object.get("currentRevision") == Some(&IvyJson::Number(1.0))
                && revision.get("revision") == Some(&IvyJson::Number(1.0))
                && revision.get("contractVersion") == Some(&"1.0.0".into()),
        )?;
        Ok(result)
    }
    async fn chunk(
        &self,
        pin: &IvyJson,
        owner: &IvyJson,
        name: Option<&JsString>,
    ) -> IvyResult<Vec<u8>> {
        let result = self
            .engine
            .evidence
            .store
            .client
            .request("objects.read", pin.clone(), RequestOptions::default())
            .await?;
        let object = result.required("object")?;
        let revision = result.required("revision")?;
        let content = result.required("content")?;
        check(
            object.get("id") == pin.get("objectId")
                && object.get("ownerObjectId") == Some(owner)
                && object.get("contractKey") == Some(&"secretary/media-chunk".into())
                && !object.get("effectivelyArchived").is_some_and(truthy)
                && object.get("currentRevision") == pin.get("revision")
                && revision.get("contractVersion") == Some(&"1.0.0".into())
                && name
                    .is_none_or(|name| object.get("name") == Some(&IvyJson::String(name.clone()))),
        )?;
        check(
            content.get("encoding") == Some(&"base64".into())
                && revision.get("mediaType") == Some(&"application/octet-stream".into()),
        )?;
        let bytes = original_base64(
            content
                .required("value")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
        );
        check(
            !bytes.is_empty()
                && bytes.len() <= CHUNK_BYTES
                && revision.get("byteLength") == Some(&IvyJson::Number(bytes.len() as f64))
                && revision.get("contentHash") == Some(&digest(&bytes).into()),
        )?;
        Ok(bytes)
    }
    async fn write(
        &self,
        key: &str,
        name: &JsString,
        content: IvyJson,
        owner: &IvyJson,
        references: IvyJson,
    ) -> IvyResult<IvyJson> {
        if let Some(old) = self.named(key, name).await? {
            return Ok(old);
        }
        self.engine.evidence.verify_owner().await?;
        let result = async {
            let id = new_operation_id(self.engine.evidence.store.client.clone(), None).await?;
            let result = self
                .engine
                .evidence
                .store
                .client
                .request(
                    "objects.write",
                    IvyJson::object([
                        ("mutationId", IvyJson::String(id)),
                        ("references", references),
                        ("contractVersion", "1.0.0".into()),
                        (
                            "create",
                            IvyJson::object([
                                (
                                    "parentId",
                                    IvyJson::String(self.engine.evidence.store.root.clone()),
                                ),
                                ("contractKey", key.into()),
                                ("name", IvyJson::String(name.clone())),
                                ("ownerObjectId", owner.clone()),
                            ]),
                        ),
                        ("content", content),
                    ]),
                    RequestOptions::default(),
                )
                .await?;
            pin(result.required("object")?, result.required("revision")?)
        }
        .await;
        match result {
            Ok(pin) => Ok(pin),
            Err(error) => match self.named(key, name).await? {
                Some(saved) => Ok(saved),
                None => Err(error),
            },
        }
    }
    pub async fn publish<V: Validation>(
        &self,
        spool: &SpoolJournal<V>,
        operation_id: &JsString,
    ) -> IvyResult<IvyJson> {
        let (attempt, bytes) = spool.payload(operation_id)?;
        let intent = &attempt.intent;
        check(attempt.evidence.is_some())?;
        let evidence = attempt.evidence.unwrap();
        let admission = intent.required("admission")?;
        self.admission_guard
            .verify(admission, intent.required("captureOperationId")?)
            .await?;
        let mut chunks = Vec::new();
        for part in bytes.chunks(CHUNK_BYTES) {
            let name = name_for("chunk", intent, Some(chunks.len()))?;
            let pin = self
                .write(
                    "secretary/media-chunk",
                    &name,
                    IvyJson::object([
                        ("encoding", "base64".into()),
                        ("value", STANDARD.encode(part).into()),
                    ]),
                    admission.required("itemObjectId")?,
                    IvyJson::Object(vec![]),
                )
                .await?;
            check(
                self.chunk(&pin, admission.required("itemObjectId")?, Some(&name))
                    .await?
                    == part,
            )?;
            chunks.push(pin);
        }
        let value = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("admission", admission.clone()),
            ("evidence", evidence),
            ("chunks", IvyJson::Array(chunks.clone())),
        ]);
        validate_media(MediaSchema::Manifest, &value)?;
        let pin = self
            .write(
                "secretary/media-manifest",
                &name_for("manifest", intent, None)?,
                IvyJson::object([("encoding", "json".into()), ("value", value.clone())]),
                admission.required("itemObjectId")?,
                references(&chunks, "chunk"),
            )
            .await?;
        let saved = self.manifest(&pin, Some(admission)).await?;
        check(same(&saved, &value)?)?;
        Ok(IvyJson::object([("pin", pin), ("value", saved)]))
    }
    pub async fn manifest(&self, pin: &IvyJson, expected: Option<&IvyJson>) -> IvyResult<IvyJson> {
        let result = self
            .engine
            .evidence
            .store
            .client
            .request("objects.read", pin.clone(), RequestOptions::default())
            .await?;
        let object = result.required("object")?;
        let revision = result.required("revision")?;
        let content = result.required("content")?;
        check(
            object.get("id") == pin.get("objectId")
                && object.get("contractKey") == Some(&"secretary/media-manifest".into())
                && !object.get("effectivelyArchived").is_some_and(truthy)
                && object.get("currentRevision") == pin.get("revision")
                && revision.get("contractVersion") == Some(&"1.0.0".into()),
        )?;
        check(
            content.get("encoding") == Some(&"json".into())
                && revision.get("mediaType") == Some(&"application/json".into()),
        )?;
        let value = content.required("value")?;
        validate_media(MediaSchema::Manifest, value)?;
        let admission = value.required("admission")?;
        let chunks = array(value.required("chunks")?)?;
        let mut unique = HashSet::new();
        for chunk in chunks {
            unique.insert(
                chunk
                    .required("objectId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
            );
        }
        let length = value
            .required("evidence")?
            .required("byteLength")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as usize;
        check(
            match expected {
                Some(expected) => same(admission, expected)?,
                None => true,
            } && object.get("ownerObjectId") == admission.get("itemObjectId")
                && revision.get("contentHash") == Some(&hash_json(value)?.into())
                && revision.get("byteLength")
                    == Some(&IvyJson::Number(canonical(value, FRAME)?.len() as f64))
                && value
                    .required("evidence")?
                    .required("attachment")?
                    .get("messageId")
                    == admission.get("providerMessageId")
                && chunks.len() == length.div_ceil(CHUNK_BYTES)
                && unique.len() == chunks.len()
                && same(
                    revision.required("references")?,
                    &references(chunks, "chunk"),
                )?,
        )?;
        Ok(value.clone())
    }
    pub async fn payload(&self, pin: &IvyJson, expected: Option<&IvyJson>) -> IvyResult<Vec<u8>> {
        let value = self.manifest(pin, expected).await?;
        let evidence = value.required("evidence")?;
        let length = evidence
            .required("byteLength")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as usize;
        let mut bytes = Vec::with_capacity(length);
        for (index, pin) in array(value.required("chunks")?)?.iter().enumerate() {
            let part = self
                .chunk(
                    pin,
                    value.required("admission")?.required("itemObjectId")?,
                    None,
                )
                .await?;
            check(part.len() == CHUNK_BYTES.min(length - index * CHUNK_BYTES))?;
            bytes.extend(part);
        }
        check(evidence.get("contentHash") == Some(&digest(&bytes).into()))?;
        Ok(bytes)
    }
    pub async fn coverage(&self, item: &IvyJson) -> IvyResult<Option<IvyJson>> {
        let name = named_text("media-coverage-", item.required("objectId")?)?;
        let Some(pin) = self.named("secretary/media-coverage", &name).await? else {
            return Ok(None);
        };
        let result = self.read("secretary/media-coverage", &name, &pin).await?;
        let content = result.required("content")?;
        let revision = result.required("revision")?;
        check(
            content.get("encoding") == Some(&"json".into())
                && revision.get("mediaType") == Some(&"application/json".into()),
        )?;
        let value = content.required("value")?;
        validate_media(MediaSchema::Coverage, value)?;
        check(
            value.required("admission")?.get("itemObjectId") == item.get("objectId")
                && revision.get("contentHash") == Some(&hash_json(value)?.into())
                && revision.get("byteLength")
                    == Some(&IvyJson::Number(canonical(value, FRAME)?.len() as f64))
                && same(
                    revision.required("references")?,
                    &coverage_references(value)?,
                )?,
        )?;
        for attachment in array(value.required("attachments")?)? {
            let Some(pin) = attachment.get("manifest").filter(|v| truthy(v)) else {
                continue;
            };
            let raw = self
                .engine
                .evidence
                .store
                .client
                .request("objects.read", pin.clone(), RequestOptions::default())
                .await?;
            check(raw.required("content")?.get("encoding") == Some(&"json".into()))?;
            let manifest = raw.required("content")?.required("value")?;
            validate_media(MediaSchema::Manifest, manifest)?;
            check(
                same(
                    manifest.required("admission")?,
                    value.required("admission")?,
                )? && same(
                    manifest.required("evidence")?.required("attachment")?,
                    attachment.required("attachment")?,
                )?,
            )?;
            self.manifest(pin, Some(value.required("admission")?))
                .await?;
        }
        Ok(Some(IvyJson::object([
            ("pin", pin),
            ("value", value.clone()),
        ])))
    }
    pub async fn save_coverage(&self, value: &IvyJson, plan: &IvyJson) -> IvyResult<IvyJson> {
        validate_media(MediaSchema::Coverage, value)?;
        let admission = value.required("admission")?;
        self.admission_guard
            .verify(admission, plan.required("captureOperationId")?)
            .await?;
        check(same(admission, plan.required("admission")?)?)?;
        let pin = self
            .write(
                "secretary/media-coverage",
                &named_text("media-coverage-", admission.required("itemObjectId")?)?,
                IvyJson::object([("encoding", "json".into()), ("value", value.clone())]),
                admission.required("itemObjectId")?,
                coverage_references(value)?,
            )
            .await?;
        let saved = self.coverage(plan.required("item")?).await?;
        check(saved.is_some())?;
        let saved = saved.unwrap();
        check(same(saved.required("value")?, value)?)?;
        let item = self
            .engine
            .evidence
            .item(admission.required("itemObjectId")?)
            .await?;
        let media = IvyJson::object([
            ("contractKey", "secretary/media-coverage".into()),
            ("pin", pin),
            ("contentHash", hash_json(value)?.into()),
            ("name", "Outlook attachments".into()),
            ("mediaType", "application/json".into()),
            ("errorCode", IvyJson::Null),
        ]);
        let mut attached = false;
        for entry in array(item.required("value")?.required("media")?)? {
            if same(entry, &media)? {
                attached = true;
                break;
            }
        }
        if !attached {
            let operation = derive_operation_id_js(
                plan.required("captureOperationId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
                &IvyJson::Array(vec!["outlook-media".into(), hash_json(value)?.into()]),
            )?;
            let request = IvyJson::object([
                ("action", "attach".into()),
                ("operationId", IvyJson::String(operation)),
                (
                    "expectedScope",
                    self.engine
                        .evidence
                        .settings
                        .required("identity")?
                        .required("scope")?
                        .clone(),
                ),
                ("sourceId", admission.required("sourceId")?.clone()),
                ("item", item.required("pin")?.clone()),
                ("media", IvyJson::Array(vec![media])),
            ]);
            let attached = self
                .engine
                .action(admission.required("producerPrincipalId")?, &request)
                .await?;
            check(
                attached.get("phase") == Some(&"succeeded".into())
                    && attached.get("effect").and_then(|v| v.get("objectId"))
                        == item.required("pin")?.get("objectId"),
            )?;
        }
        Ok(saved)
    }
}

/// Dormant collector composition uses this unchanged additional registry. The
/// ordinary Secretary service does not register or start media work by default.
pub fn media_registry() -> IvyResult<IvyJson> {
    let schema = parse(
        include_str!("../../../../specs/schemas/secretary-media.schema.json"),
        1024 * 1024,
    )?;
    let mut contracts = Vec::new();
    let mut required = Vec::new();
    for (key, definition, media_type, description) in [
        (
            "secretary/media-manifest",
            Some("MediaManifest"),
            "application/json",
            "Final verified media manifest owned by its retained Secretary item. Its exact chunk references protect the required bytes.",
        ),
        (
            "secretary/media-coverage",
            Some("MediaCoverage"),
            "application/json",
            "Final useful media coverage summary owned by its Secretary item, with exact references to successful manifests. Acquisition state remains local.",
        ),
        (
            "secretary/media-chunk",
            None,
            "application/octet-stream",
            "One final binary chunk owned by the retained Secretary item and protected by its manifest reference. Never render untrusted attachment content as HTML.",
        ),
    ] {
        let mut contract = IvyJson::object([
            ("key", key.into()),
            ("version", "1.0.0".into()),
            (
                "owner",
                IvyJson::object([
                    ("kind", "service".into()),
                    ("serviceName", "secretary".into()),
                ]),
            ),
            ("mediaType", media_type.into()),
            (
                "retention",
                IvyJson::object([
                    ("objects", IvyJson::object([("mode", "owned".into())])),
                    ("revisions", IvyJson::object([("mode", "current".into())])),
                ]),
            ),
            ("specMarkdown", description.into()),
        ]);
        if let Some(definition) = definition {
            let mut bundled = schema.clone();
            bundled.insert("$ref", format!("#/$defs/{definition}").into());
            contract.insert("jsonSchema", bundled);
        }
        contracts.push(contract);
        required.push(IvyJson::object([
            ("key", key.into()),
            ("readVersions", IvyJson::Array(vec!["1.0.0".into()])),
            ("writeVersions", IvyJson::Array(vec!["1.0.0".into()])),
        ]));
    }
    Ok(IvyJson::object([
        ("contracts", IvyJson::Array(contracts)),
        ("requiredContracts", IvyJson::Array(required)),
    ]))
}
