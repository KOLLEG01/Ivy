//! Inbox admission prepares one immutable accepted action before recovery owns
//! its effect. Retries always use the retained original action and operation ID.
use crate::{
    engine::SecretaryEngine,
    inbox_evidence::{
        identity_hash, ignored_reason, message_hash, notice_text, operation_name, truthy,
    },
    inbox_recovery::InboxRecovery,
    policy::{self, Disposition, Notification, Urgency},
    schema::{Schema, validate},
    store::{Record, mutation},
    voice_target::discover_voice_target,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, hash::hash_json, operation::parse_operation_id_js,
};
use ivy_sdk::{
    clock::{date_to_iso, parse_date},
    rpc::RequestOptions,
};
use std::collections::HashSet;

fn need(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn array(value: &IvyJson) -> IvyResult<&[IvyJson]> {
    value.as_array().ok_or_else(IvyError::internal)
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn member<'a>(value: Option<&'a IvyJson>, key: &str) -> IvyResult<Option<&'a IvyJson>> {
    match value {
        None | Some(IvyJson::Null) => Err(IvyError::internal()),
        Some(value) => Ok(value.get(key)),
    }
}
fn urgency(value: &IvyJson) -> IvyResult<Urgency> {
    match value.as_str()?.as_str() {
        "low" => Ok(Urgency::Low),
        "normal" => Ok(Urgency::Normal),
        "high" => Ok(Urgency::High),
        "critical" => Ok(Urgency::Critical),
        _ => Err(IvyError::internal()),
    }
}
fn suppression(assessment: &IvyJson, policy: &IvyJson) -> IvyResult<Option<&'static str>> {
    let notification = match assessment.required("notification")?.as_str()?.as_str() {
        "needs_attention" => Notification::NeedsAttention,
        "none" => Notification::None,
        "main" => Notification::Main,
        "voice" => Notification::Voice,
        _ => return Err(IvyError::internal()),
    };
    let disposition = match assessment.required("disposition")?.as_str()?.as_str() {
        "ignore" => Disposition::Ignore,
        "record" => Disposition::Record,
        // For non-none notifications only urgency matters. This enum's Task
        // represents all other admitted dispositions in that shared policy.
        _ => Disposition::Task,
    };
    Ok(policy::suppression(
        notification,
        disposition,
        urgency(assessment.required("urgency")?)?,
        urgency(policy.required("minimumMainUrgency")?)?,
    ))
}
fn validate_message(message: &IvyJson, source: &IvyJson) -> IvyResult<()> {
    need(
        message.get("accountId") == source.get("accountId"),
        "secretary_account_mismatch",
        "The observation must belong to the selected source account.",
    )?;
    for key in ["observedAt", "occurredAt"] {
        let stamp = string(message, key)?;
        let parsed = parse_date(stamp);
        need(
            parsed.is_finite() && date_to_iso(parsed).is_ok_and(|iso| iso == *stamp),
            "invalid_arguments",
            "Observation times must be real UTC instants.",
        )?;
    }
    if let Some(value) = message.get("url").filter(|value| truthy(value)) {
        // Web URL consumes a USVString. Only this URL boundary replaces lone
        // surrogates; message identity and saved bytes remain lossless UTF16.
        let usv = String::from_utf16_lossy(&value.as_js_string().ok_or_else(IvyError::internal)?.0);
        let url = url::Url::parse(&usv).map_err(|_| {
            IvyError::new(
                "invalid_arguments",
                "Source links must be absolute HTTP(S) URLs.",
            )
        })?;
        need(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none_or(str::is_empty),
            "invalid_arguments",
            "Source links must be absolute HTTP(S) URLs without credentials.",
        )?;
    }
    Ok(())
}
impl SecretaryEngine<'_, '_, '_> {
    async fn validate_assessment(&self, assessment: &IvyJson, policy: &IvyJson) -> IvyResult<()> {
        let task = assessment.required("task")?;
        let disposition = assessment.required("disposition")?;
        let notification = assessment.required("notification")?;
        need(
            (disposition == &IvyJson::from("task")) == !task.is_null(),
            "invalid_arguments",
            "Only task disposition must identify an already saved TaskBoard task.",
        )?;
        need(
            (disposition == &IvyJson::from("ignore") || disposition == &IvyJson::from("record"))
                == (notification == &IvyJson::from("none")),
            "invalid_arguments",
            "Ignored or recorded items must use no notification; actionable items must select Main, Voice or needs-attention.",
        )?;
        need(
            notification != &IvyJson::from("voice")
                || assessment.get("urgency") == Some(&"critical".into())
                    && policy.get("voiceEscalation").is_some_and(truthy),
            "invalid_arguments",
            "Voice requires one available escalation target and a critical assessment.",
        )?;
        if task.is_null() {
            return Ok(());
        }
        let raw = self
            .evidence
            .store
            .client
            .request("objects.read", task.clone(), RequestOptions::default())
            .await?;
        let object = member(Some(&raw), "object")?;
        // SEC01 is deliberately unchanged: the exact original version set.
        let valid = !member(object, "effectivelyArchived")?.is_some_and(truthy)
            && member(object, "contractKey")? == Some(&"task-board/task".into())
            && member(member(Some(&raw), "revision")?, "contractVersion")? == Some(&"1.0.0".into())
            && member(member(Some(&raw), "content")?, "encoding")? == Some(&"json".into())
            && {
                let value = member(member(Some(&raw), "content")?, "value")?.ok_or_else(|| {
                    IvyError::new("invalid_arguments", "Value is outside the JSON domain.")
                })?;
                Some(&IvyJson::from(hash_json(value)?))
                    == member(member(Some(&raw), "revision")?, "contentHash")?
            };
        need(
            valid,
            "secretary_task_mismatch",
            "The linked delegation must be an exact active saved TaskBoard task.",
        )
    }
    async fn validate_media(&self, item: &IvyJson, media: &[IvyJson]) -> IvyResult<()> {
        let mut distinct = HashSet::new();
        for value in media {
            distinct.insert(hash_json(value)?);
        }
        need(
            !media.is_empty() && media.len() <= 64 && distinct.len() == media.len(),
            "invalid_arguments",
            "Media attachments must be distinct and bounded.",
        )?;
        for entry in media {
            let pin = entry.required("pin")?;
            need(
                (!pin.is_null()) == !entry.required("contentHash")?.is_null()
                    && pin.is_null() == !entry.required("errorCode")?.is_null(),
                "invalid_arguments",
                "Media must contain either one exact artifact or one explicit gap.",
            )?;
            if pin.is_null() {
                continue;
            }
            let raw = self
                .evidence
                .store
                .client
                .request("objects.read", pin.clone(), RequestOptions::default())
                .await?;
            let object = member(Some(&raw), "object")?;
            need(
                member(object, "contractKey")? == entry.get("contractKey")
                    && member(object, "ownerObjectId")? == item.required("pin")?.get("objectId")
                    && !member(object, "effectivelyArchived")?.is_some_and(truthy)
                    && member(member(Some(&raw), "revision")?, "contentHash")?
                        == entry.get("contentHash"),
                "secretary_media_mismatch",
                "The media artifact must be exact final data owned by this Secretary item.",
            )?;
        }
        Ok(())
    }
    pub async fn operation(
        &self,
        caller: &IvyJson,
        scope: &IvyJson,
        id: &IvyJson,
    ) -> IvyResult<IvyJson> {
        self.evidence.authorize(caller, scope, "operation", None)?;
        self.evidence.verify_owner().await?;
        self.evidence
            .find(caller, id)
            .await?
            .ok_or_else(|| {
                IvyError::new(
                    "not_found",
                    "The caller has no retained operation with this ID.",
                )
            })?
            .required("value")
            .cloned()
    }
    pub async fn action(&self, caller: &IvyJson, request: &IvyJson) -> IvyResult<IvyJson> {
        validate(Schema::Request, request)?;
        let evidence = self.evidence;
        let action = request.required("action")?.as_str()?;
        let id = request.required("operationId")?;
        evidence.authorize(
            caller,
            request.required("expectedScope")?,
            &action,
            request.get("sourceId"),
        )?;
        need(
            parse_operation_id_js(id.as_js_string().ok_or_else(IvyError::internal)?)?.runtime_epoch
                == *self.runtime_epoch,
            "operation_expired",
            "The Secretary operation belongs to another runtime.",
        )?;
        need(
            self.prior_workflow(caller, id)?.is_none(),
            "mutation_conflict",
            "The operation identity already names an assignment action.",
        )?;
        if action == "capture" {
            validate_message(
                request.required("message")?,
                evidence.source_definition(request.required("sourceId")?)?,
            )?;
        }
        evidence.verify_owner().await?;
        let recovery = InboxRecovery { evidence };
        if let Some(prior) = evidence.find(caller, id).await? {
            need(
                prior.required("value")?.get("requestHash") == Some(&hash_json(request)?.into()),
                "mutation_conflict",
                "This operation ID already names different arguments.",
            )?;
            return recovery.apply(&prior).await?.required("value").cloned();
        }
        self.workflow()
            .validate_operation_id(id.as_js_string().ok_or_else(IvyError::internal)?)?;
        let at = self.now()?;
        let source_id = match request.get("sourceId") {
            Some(id) => id.clone(),
            None => evidence
                .item(request.required("item")?.required("objectId")?)
                .await?
                .required("value")?
                .required("sourceId")?
                .clone(),
        };
        let snapshot = evidence.source(&source_id).await?;
        let mut accepted = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("action", action.clone().into()),
            ("operationId", id.clone()),
            ("callerPrincipalId", caller.clone()),
            ("requestHash", hash_json(request)?.into()),
            ("request", request.clone()),
            ("sourceId", "".into()),
            ("createdAt", IvyJson::String(at.clone())),
            ("source", snapshot.required("value")?.clone()),
            ("prepared", IvyJson::Null),
            ("existingEffect", IvyJson::Null),
            ("ignoredReason", IvyJson::Null),
            ("captureIdentity", IvyJson::Null),
            ("checkpointCaptures", IvyJson::Array(Vec::new())),
            ("phase", "accepted".into()),
            ("effect", IvyJson::Null),
            ("errorCode", IvyJson::Null),
        ]);
        let prepared = match action.as_str() {
            "capture" => {
                accepted.insert("sourceId", source_id.clone());
                let source = evidence.source_definition(&source_id)?;
                let message = request.required("message")?;
                if let Some(reason) = ignored_reason(source, message)? {
                    accepted.insert("ignoredReason", reason.into());
                    accepted.insert("request", IvyJson::Null);
                    None
                } else {
                    let identity = identity_hash(&source_id, message)?;
                    accepted.insert("captureIdentity", identity.clone().into());
                    let item = IvyJson::object([
                        ("schemaVersion", IvyJson::Number(1.0)),
                        ("identityHash", identity.clone().into()),
                        ("sourceId", source_id),
                        ("messageHash", message_hash(message)?.into()),
                        ("message", message.clone()),
                        ("media", IvyJson::Array(Vec::new())),
                        ("decision", IvyJson::Null),
                        ("notice", IvyJson::Null),
                    ]);
                    let name: JsString = format!("item-{}", &identity[7..]).into();
                    if let Some(existing) = evidence.store.named(Record::Item, &name).await? {
                        accepted.insert(
                            "existingEffect",
                            recovery.capture_duplicate(&existing, &item).await?,
                        );
                        None
                    } else {
                        Some((
                            "secretary/item",
                            IvyJson::object([("createName", IvyJson::String(name))]),
                            item,
                        ))
                    }
                }
            }
            "assess" => {
                let current = evidence
                    .item(request.required("item")?.required("objectId")?)
                    .await?;
                let mut value = current.required("value")?.clone();
                need(
                    same(current.required("pin")?, request.required("item")?)?
                        && value.required("decision")?.is_null(),
                    "revision_conflict",
                    "Assess the exact current unassessed item once.",
                )?;
                let assessment = request.required("assessment")?;
                let mut policy = evidence.settings.required("policy")?.clone();
                if policy.get("voiceEscalation").is_none_or(IvyJson::is_null) {
                    let target = if assessment.get("notification") == Some(&"voice".into()) {
                        discover_voice_target(evidence.store.client.as_ref(), self.tools)
                            .await?
                            .unwrap_or(IvyJson::Null)
                    } else {
                        IvyJson::Null
                    };
                    policy.insert("voiceEscalation", target);
                }
                self.validate_assessment(assessment, &policy).await?;
                let source_id = value.required("sourceId")?.clone();
                accepted.insert("sourceId", source_id.clone());
                let source = evidence.source_definition(&source_id)?;
                let reason = suppression(assessment, &policy)?;
                let voice = if assessment.get("notification") == Some(&"voice".into()) {
                    IvyJson::object([
                        ("state", "queued".into()),
                        ("operationId", IvyJson::String((self.new_uuid)())),
                        ("callId", IvyJson::Null),
                        ("errorCode", IvyJson::Null),
                    ])
                } else {
                    IvyJson::Null
                };
                let pin = current.required("pin")?;
                let notice = IvyJson::object([
                    (
                        "state",
                        if reason.is_some() {
                            "suppressed"
                        } else {
                            "queued"
                        }
                        .into(),
                    ),
                    ("reason", reason.map(IvyJson::from).unwrap_or(IvyJson::Null)),
                    (
                        "text",
                        IvyJson::String(notice_text(
                            assessment,
                            value.required("message")?,
                            source,
                        )?),
                    ),
                    (
                        "source",
                        IvyJson::object([
                            ("objectId", pin.required("objectId")?.clone()),
                            (
                                "revision",
                                IvyJson::Number(
                                    pin.required("revision")?
                                        .as_f64()
                                        .ok_or_else(IvyError::internal)?
                                        + 1.0,
                                ),
                            ),
                        ]),
                    ),
                    ("target", IvyJson::Null),
                    ("request", IvyJson::Null),
                    ("preparedRevision", IvyJson::Null),
                    ("evidence", IvyJson::Null),
                    ("voice", voice),
                    ("updatedAt", IvyJson::String(at.clone())),
                ]);
                value.insert(
                    "decision",
                    IvyJson::object([
                        ("assessment", assessment.clone()),
                        ("actor", caller.clone()),
                        ("operationId", id.clone()),
                        ("decidedAt", IvyJson::String(at.clone())),
                        ("policy", policy),
                    ]),
                );
                value.insert("notice", notice);
                Some((
                    "secretary/item",
                    IvyJson::object([("object", pin.clone())]),
                    value,
                ))
            }
            "attach" => {
                accepted.insert("sourceId", source_id.clone());
                let current = evidence
                    .item(request.required("item")?.required("objectId")?)
                    .await?;
                let mut value = current.required("value")?.clone();
                need(
                    same(current.required("pin")?, request.required("item")?)?
                        && value.get("sourceId") == Some(&source_id),
                    "revision_conflict",
                    "Attach media to the exact current source item.",
                )?;
                let media = array(request.required("media")?)?;
                self.validate_media(&current, media).await?;
                let mut merged = array(value.required("media")?)?.to_vec();
                for entry in media {
                    let hash = hash_json(entry)?;
                    let mut present = false;
                    for old in &merged {
                        if hash_json(old)? == hash {
                            present = true;
                            break;
                        }
                    }
                    if !present {
                        merged.push(entry.clone());
                    }
                }
                need(
                    merged.len() <= 64,
                    "limit_exceeded",
                    "The Secretary item media list is full.",
                )?;
                value.insert("media", IvyJson::Array(merged));
                Some((
                    "secretary/item",
                    IvyJson::object([("object", current.required("pin")?.clone())]),
                    value,
                ))
            }
            "checkpoint" => {
                accepted.insert("sourceId", source_id.clone());
                let captures = array(request.required("captures")?)?;
                let unique: HashSet<_> = captures
                    .iter()
                    .map(|v| v.as_js_string().ok_or_else(IvyError::internal))
                    .collect::<IvyResult<_>>()?;
                need(
                    request.get("nextCursor") != request.get("previousCursor")
                        && unique.len() == captures.len(),
                    "invalid_arguments",
                    "A checkpoint must advance with distinct capture operations.",
                )?;
                let mut pins = Vec::new();
                for id in captures {
                    let capture = evidence.find(caller, id).await?;
                    need(
                        capture
                            .as_ref()
                            .and_then(|row| row.get("value"))
                            .is_some_and(|value| {
                                value.get("phase") == Some(&"succeeded".into())
                                    && value.get("action") == Some(&"capture".into())
                                    && value.get("sourceId") == Some(&source_id)
                            }),
                        "secretary_capture_pending",
                        "Every checkpoint member must be this producer’s completed capture from this source.",
                    )?;
                    pins.push(
                        capture
                            .ok_or_else(IvyError::internal)?
                            .required("pin")?
                            .clone(),
                    );
                }
                accepted.insert("checkpointCaptures", IvyJson::Array(pins));
                let source = evidence.source(&source_id).await?;
                let mut value = source.required("value")?.clone();
                need(
                    value.get("cursor") == request.get("previousCursor"),
                    "revision_conflict",
                    "The source has a different current checkpoint.",
                )?;
                value.insert("cursor", request.required("nextCursor")?.clone());
                value.insert(
                    "checkpointOperation",
                    IvyJson::object([("principalId", caller.clone()), ("operationId", id.clone())]),
                );
                value.insert("updatedAt", IvyJson::String(at));
                Some((
                    "secretary/source",
                    IvyJson::object([("object", source.required("pin")?.clone())]),
                    value,
                ))
            }
            _ => return Err(IvyError::internal()),
        };
        if let Some((key, destination, value)) = prepared {
            accepted.insert(
                "prepared",
                IvyJson::object([
                    ("key", key.into()),
                    ("destination", destination),
                    ("value", value),
                ]),
            );
        }
        let operation = evidence
            .store
            .create(
                Record::Operation,
                &operation_name(caller, id)?,
                &accepted,
                &mutation(&[caller.clone(), id.clone(), "accept".into()])?,
            )
            .await?;
        need(
            operation.required("value")?.get("requestHash") == accepted.get("requestHash"),
            "mutation_conflict",
            "The winning operation names different original arguments.",
        )?;
        let checked = evidence.checked(&operation).await?;
        recovery.apply(&checked).await?.required("value").cloned()
    }
}
