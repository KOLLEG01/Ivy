//! Durable producer-independent message and object event windows.
use crate::{
    assignment_message::{
        MESSAGE_BATCH_KIND, MESSAGE_WINDOW_KEY, MESSAGE_WINDOW_MS, message_channel,
    },
    assignment_object::{OBJECT_WINDOW_KEY, Observation, object_projection, observation_ready},
    assignment_scheduler::AssignmentScheduler,
    inbox_recovery::RecoveryIssues,
    journal::Document,
    store::Record,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::{
    clock::{date_to_iso, parse_date},
    rpc::RequestOptions,
};

fn need(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn number(value: &IvyJson, key: &str) -> IvyResult<f64> {
    value.required(key)?.as_f64().ok_or_else(IvyError::internal)
}
fn pending(value: &IvyJson) -> Option<&IvyJson> {
    value.get("pending").filter(|v| !v.is_null())
}
fn iso(value: f64) -> IvyResult<JsString> {
    date_to_iso(value).map_err(|_| IvyError::internal())
}
fn projection(value: &IvyJson, trigger: &IvyJson) -> IvyResult<IvyJson> {
    let paths = trigger
        .required("paths")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
        .collect::<IvyResult<Vec<_>>>()?;
    Ok(object_projection(value, &paths))
}

// Revisions are positive JSON integers within the public wire range.
pub(crate) fn object_revision(value: Option<&IvyJson>) -> IvyResult<f64> {
    let n = value.and_then(IvyJson::as_f64).unwrap_or(f64::NAN);
    need(
        n.is_finite() && n.fract() == 0.0 && n > 0.0 && n <= 9_007_199_254_740_991.0,
        "secretary_object_event_invalid",
        "Object change has no valid revision.",
    )?;
    Ok(n)
}
impl AssignmentScheduler<'_, '_, '_> {
    pub async fn collect_object_event(
        &self,
        assignment: &IvyJson,
        event: &IvyJson,
    ) -> IvyResult<()> {
        let value = assignment.required("value")?;
        let trigger = value.required("trigger")?;
        if trigger.get("kind") != Some(&"object-change".into()) {
            return Ok(());
        }
        let payload = event.required("payload")?;
        if payload.is_null() {
            return Err(IvyError::internal());
        }
        let revision = object_revision(payload.get("revision"))?;
        let object_id = text(payload, "objectId")?;
        let name = hash_json(&IvyJson::Array(vec![
            value.required("assignmentId")?.clone(),
            assignment.required("pin")?.required("revision")?.clone(),
            IvyJson::String(object_id.clone()),
        ]))?;
        let store = self.evidence.store;
        let mut saved = store.journal.named(OBJECT_WINDOW_KEY, &name, true)?;
        if saved.as_ref().is_some_and(|s| {
            number(event, "sequence")
                .is_ok_and(|n| n <= number(&s.value, "lastSequence").unwrap_or(f64::NAN))
        }) {
            return Ok(());
        }
        if let Some(prior) = saved.as_ref().filter(|s| number(&s.value, "revision").is_ok_and(|old| revision <= old)) {
            let mut next = prior.value.clone();
            next.insert("lastSequence", event.required("sequence")?.clone());
            store.journal.amend_technical(OBJECT_WINDOW_KEY, &prior.pin, &next)?;
            return Ok(());
        }
        let read = store
            .client
            .request(
                "objects.read",
                IvyJson::object([
                    ("objectId", IvyJson::String(object_id.clone())),
                    ("revision", IvyJson::Number(revision)),
                ]),
                RequestOptions::default(),
            )
            .await?;
        need(
            read.required("content")?.get("encoding") == Some(&"json".into()),
            "secretary_object_content_invalid",
            "Observed object must contain JSON.",
        )?;
        let projected = projection(read.required("content")?.required("value")?, trigger)?;
        let fingerprint = hash_json(&projected)?;
        if saved.is_none() {
            let (previous, previous_revision) = if revision > 1.0 {
                let prior = store
                    .client
                    .request(
                        "objects.read",
                        IvyJson::object([
                            ("objectId", IvyJson::String(object_id.clone())),
                            ("revision", IvyJson::Number(revision - 1.0)),
                        ]),
                        RequestOptions::default(),
                    )
                    .await?;
                need(
                    prior.required("content")?.get("encoding") == Some(&"json".into()),
                    "secretary_object_content_invalid",
                    "Previous observed revision must contain JSON.",
                )?;
                (
                    projection(prior.required("content")?.required("value")?, trigger)?,
                    revision - 1.0,
                )
            } else {
                (projected.clone(), revision)
            };
            saved = Some(store.journal.create_technical(
                OBJECT_WINDOW_KEY,
                &name,
                &IvyJson::object([
                    ("assignmentId", value.required("assignmentId")?.clone()),
                    ("objectId", IvyJson::String(object_id.clone())),
                    ("lastSequence", IvyJson::Number(0.0)),
                    ("fingerprint", hash_json(&previous)?.into()),
                    ("projection", previous),
                    ("revision", IvyJson::Number(previous_revision)),
                    ("pending", IvyJson::Null),
                ]),
            )?);
        }
        let saved = saved.unwrap();
        let state = &saved.value;
        let mut window = pending(state).cloned();
        if state.get("fingerprint") != Some(&fingerprint.clone().into()) {
            window = Some(if let Some(mut window) = window {
                window.insert("lastRevision", IvyJson::Number(revision));
                window.insert("after", projected.clone());
                window
            } else {
                IvyJson::object([
                    ("assignment", assignment.required("pin")?.clone()),
                    ("assignmentSnapshot", value.clone()),
                    ("objectId", IvyJson::String(object_id)),
                    ("firstSequence", event.required("sequence")?.clone()),
                    ("firstRevision", state.required("revision")?.clone()),
                    ("lastRevision", IvyJson::Number(revision)),
                    ("occurredAt", event.required("occurredAt")?.clone()),
                    (
                        "dueAt",
                        IvyJson::String(iso(parse_date(&text(event, "occurredAt")?)
                            + number(trigger, "delaySeconds")? * 1000.0)?),
                    ),
                    ("before", state.required("projection")?.clone()),
                    ("after", projected.clone()),
                ])
            });
            if hash_json(window.as_ref().unwrap().required("before")?)? == fingerprint {
                window = None;
            }
        }
        let mut next = state.clone();
        next.insert("lastSequence", event.required("sequence")?.clone());
        next.insert("fingerprint", fingerprint.into());
        next.insert("projection", projected);
        next.insert("revision", IvyJson::Number(revision));
        next.insert("pending", window.unwrap_or(IvyJson::Null));
        store
            .journal
            .amend_technical(OBJECT_WINDOW_KEY, &saved.pin, &next)?;
        Ok(())
    }
    /// Rebuild only triggers whose authoritative owners retain enough state.
    pub(crate) async fn recover_event_gap(&self, assignment: &IvyJson, now: i64, issues: &dyn RecoveryIssues) -> IvyResult<()> {
        let value = assignment.required("value")?;
        let id = text(value, "assignmentId")?;
        let id_text = id.to_utf8()?;
        let issue = format!("event-gap:{id_text}");
        let store = self.evidence.store;
        let Some(gap) = store.journal.named("secretary/event-gap", &id_text, true)? else { return Ok(()); };
        if gap.value.get("disposition").is_some_and(|v| !v.is_null()) { issues.clear(&issue); return Ok(()); }
        let trigger = value.required("trigger")?;
        let disposition = if value.get("enabled") == Some(&false.into()) {
            "inactive_history"
        } else if trigger.get("kind") == Some(&"schedule".into()) {
            let name: JsString = format!("schedule-{}", &hash_json(&IvyJson::String(id.clone()))?[7..]).into();
            let Some(progress) = store.named(Record::ScheduleProgress, &name).await? else { return Ok(()); };
            let progress = progress.required("value")?;
            if progress.required("assignment")?.get("revision") != assignment.required("pin")?.get("revision") || parse_date(&text(progress, "checkedAt")?) < now as f64 { return Ok(()); }
            "retained_schedule_replayed"
        } else if trigger.get("kind") == Some(&"object-change".into()) {
            let name: JsString = format!("event-{}", &hash_json(&IvyJson::String(id.clone()))?[7..]).into();
            let Some(progress) = store.named(Record::EventProgress, &name).await? else { return Ok(()); };
            let processed = progress.required("value")?.required("processedThrough")?;
            for object_id in trigger.required("objectIds")?.as_array().ok_or_else(IvyError::internal)? {
                let current = store.client.request("objects.read", IvyJson::object([("objectId", object_id.clone())]), RequestOptions::default()).await?;
                need(current.required("content")?.get("encoding") == Some(&"json".into()), "secretary_gap_source_unavailable", "Gap recovery needs the authoritative JSON source.")?;
                let name = hash_json(&IvyJson::Array(vec![IvyJson::String(id.clone()), assignment.required("pin")?.required("revision")?.clone(), object_id.clone()]))?;
                let previous = store.journal.named(OBJECT_WINDOW_KEY, &name, true)?;
                let projected = projection(current.required("content")?.required("value")?, trigger)?;
                let fingerprint = hash_json(&projected)?;
                let revision = current.required("revision")?.required("revision")?;
                if let Some(previous) = previous {
                    let prior_pending = pending(&previous.value);
                    let before = prior_pending.and_then(|v| v.get("before")).unwrap_or(previous.value.required("projection")?);
                    let next_pending = if hash_json(before)? == fingerprint { IvyJson::Null } else {
                        IvyJson::object([
                            ("assignment", assignment.required("pin")?.clone()), ("assignmentSnapshot", value.clone()), ("objectId", object_id.clone()),
                            ("firstSequence", prior_pending.and_then(|v| v.get("firstSequence")).unwrap_or(processed).clone()),
                            ("firstRevision", prior_pending.and_then(|v| v.get("firstRevision")).unwrap_or(previous.value.required("revision")?).clone()),
                            ("lastRevision", revision.clone()), ("occurredAt", IvyJson::String(iso(now as f64)?)),
                            ("dueAt", prior_pending.and_then(|v| v.get("dueAt")).cloned().unwrap_or(IvyJson::String(iso(now as f64 + number(trigger,"delaySeconds")? * 1000.0)?))),
                            ("before", before.clone()), ("after", projected.clone())
                        ])
                    };
                    let mut next = previous.value.clone();
                    next.insert("lastSequence", IvyJson::Number(number(&previous.value,"lastSequence")?.max(processed.as_f64().ok_or_else(IvyError::internal)?)));
                    next.insert("fingerprint", fingerprint.into()); next.insert("projection", projected); next.insert("revision", revision.clone()); next.insert("pending", next_pending);
                    store.journal.amend_technical(OBJECT_WINDOW_KEY, &previous.pin, &next)?;
                } else {
                    store.journal.create_technical(OBJECT_WINDOW_KEY, &name, &IvyJson::object([
                        ("assignmentId", IvyJson::String(id.clone())), ("objectId", object_id.clone()), ("lastSequence", processed.clone()),
                        ("fingerprint", fingerprint.into()), ("projection", projected), ("revision", revision.clone()), ("pending", IvyJson::Null)
                    ]))?;
                }
            }
            "authoritative_snapshot_rebased"
        } else {
            issues.issue(&issue, "secretary_event_gap_unrecoverable"); return Ok(());
        };
        let mut next = gap.value.clone(); next.insert("disposition", disposition.into()); next.insert("resolvedAt", IvyJson::String(iso(now as f64)?)); next.insert("assignment", assignment.required("pin")?.clone());
        store.journal.amend_technical("secretary/event-gap", &gap.pin, &next)?;
        issues.clear(&issue);
        Ok(())
    }
    pub async fn flush_due_object_windows(
        &self,
        assignment: &IvyJson,
        configuration: &IvyJson,
        now: i64,
    ) -> IvyResult<()> {
        let store = self.evidence.store;
        for saved in store.journal.list(OBJECT_WINDOW_KEY, true)? {
            if saved.value.get("assignmentId") != assignment.required("value")?.get("assignmentId")
            {
                continue;
            }
            let Some(window) = pending(&saved.value) else {
                continue;
            };
            if parse_date(&text(window, "dueAt")?) > now as f64 {
                continue;
            }
            let trigger = window.required("assignmentSnapshot")?.required("trigger")?;
            if trigger.get("kind") != Some(&"object-change".into()) {
                continue;
            }
            let current = store
                .client
                .request(
                    "objects.read",
                    IvyJson::object([("objectId", window.required("objectId")?.clone())]),
                    RequestOptions::default(),
                )
                .await?;
            need(
                current.required("object")?.get("effectivelyArchived")
                    != Some(&IvyJson::Bool(true))
                    && current.required("content")?.get("encoding") == Some(&"json".into()),
                "secretary_object_content_invalid",
                "Observed object is unavailable or has no JSON content.",
            )?;
            let content = current.required("content")?.required("value")?;
            let projected = projection(content, trigger)?;
            if saved.value.get("fingerprint") != Some(&hash_json(&projected)?.into()) {
                continue;
            }
            let condition = trigger
                .get("observation")
                .filter(|v| !v.is_null())
                .map(|v| {
                    Ok::<_, IvyError>(Observation {
                        complete_path: text(v, "completePath")?,
                        observed_at_path: text(v, "observedAtPath")?,
                        maximum_age_seconds: number(v, "maximumAgeSeconds")?,
                    })
                })
                .transpose()?;
            if !observation_ready(
                content,
                condition.as_ref(),
                &text(window, "dueAt")?,
                now as f64,
            ) {
                continue;
            }
            let original = IvyJson::object([
                ("pin", window.required("assignment")?.clone()),
                ("value", window.required("assignmentSnapshot")?.clone()),
            ]);
            let event = IvyJson::object([
                ("kind", "event".into()),
                (
                    "key",
                    format!(
                        "object-batch:{}:{}",
                        number(window, "firstSequence")?,
                        text(window, "objectId")?.to_utf8()?
                    )
                    .into(),
                ),
                ("occurredAt", window.required("occurredAt")?.clone()),
                (
                    "payload",
                    IvyJson::object([
                        ("kind", "secretary.object.batch".into()),
                        ("objectId", window.required("objectId")?.clone()),
                        ("beforeRevision", window.required("firstRevision")?.clone()),
                        (
                            "revision",
                            current.required("revision")?.required("revision")?.clone(),
                        ),
                        ("dueAt", window.required("dueAt")?.clone()),
                        ("paths", trigger.required("paths")?.clone()),
                        ("before", window.required("before")?.clone()),
                        ("after", projected),
                    ]),
                ),
            ]);
            self.enqueue(&original, &event, configuration).await?;
            let mut next = saved.value.clone();
            next.insert("pending", IvyJson::Null);
            store
                .journal
                .amend_technical(OBJECT_WINDOW_KEY, &saved.pin, &next)?;
        }
        Ok(())
    }
    pub async fn flush_message_window(
        &self,
        saved: &Document,
        configuration: &IvyJson,
    ) -> IvyResult<()> {
        let Some(window) = pending(&saved.value) else {
            return Ok(());
        };
        let original = IvyJson::object([
            ("pin", window.required("assignment")?.clone()),
            ("value", window.required("assignmentSnapshot")?.clone()),
        ]);
        let first_id = window
            .required("eventIds")?
            .as_array()
            .and_then(|a| a.first())
            .cloned()
            .ok_or_else(IvyError::internal)?;
        let event = IvyJson::object([
            ("kind", "event".into()),
            (
                "key",
                format!(
                    "message-batch:{}:{}",
                    number(window, "firstSequence")?,
                    first_id.as_str()?
                )
                .into(),
            ),
            ("occurredAt", window.required("firstMessageAt")?.clone()),
            (
                "payload",
                IvyJson::object([
                    ("kind", MESSAGE_BATCH_KIND.into()),
                    ("dueAt", window.required("dueAt")?.clone()),
                    ("serviceNodeId", window.required("serviceNodeId")?.clone()),
                    ("channel", window.required("channel")?.clone()),
                    ("itemIds", window.required("itemIds")?.clone()),
                    ("eventIds", window.required("eventIds")?.clone()),
                ]),
            ),
        ]);
        self.enqueue(&original, &event, configuration).await?;
        let mut next = saved.value.clone();
        next.insert("pending", IvyJson::Null);
        self.evidence
            .store
            .journal
            .amend_technical(MESSAGE_WINDOW_KEY, &saved.pin, &next)?;
        Ok(())
    }
    pub async fn collect_message_event(
        &self,
        assignment: &IvyJson,
        event: &IvyJson,
        configuration: &IvyJson,
    ) -> IvyResult<()> {
        let absent = IvyJson::Null;
        let payload = event
            .get("payload")
            .filter(|v| v.as_object().is_some())
            .unwrap_or(&absent);
        let channel = message_channel(payload);
        let event_id = payload.get("eventId").and_then(IvyJson::as_js_string);
        let source = text(event, "source")?;
        let prefix = JsString::from("service:");
        need(
            event_id.is_some() && channel.is_some() && source.0.starts_with(&prefix.0),
            "secretary_message_event_invalid",
            "The message event has no source identity or channel.",
        )?;
        let channel = channel.unwrap();
        let service_id = JsString(source.0[prefix.0.len()..].to_vec());
        let item_id = channel
            .get("itemId")
            .filter(|v| v.as_js_string().is_some_and(|s| !s.0.is_empty()));
        let name = hash_json(&IvyJson::Array(vec![
            assignment
                .required("value")?
                .required("assignmentId")?
                .clone(),
            IvyJson::String(service_id.clone()),
            channel.required("key")?.clone(),
        ]))?;
        let journal = self.evidence.store.journal;
        let mut saved = journal.named(MESSAGE_WINDOW_KEY, &name, true)?;
        if saved.as_ref().is_some_and(|s| {
            number(event, "sequence")
                .is_ok_and(|n| n <= number(&s.value, "lastSequence").unwrap_or(f64::NAN))
        }) {
            return Ok(());
        }
        if let Some(window) = saved.as_ref().and_then(|s| pending(&s.value)) {
            need(
                hash_json(window.required("channel")?.required("reference")?)?
                    == hash_json(channel.required("reference")?)?,
                "secretary_message_channel_conflict",
                "The message channel reference changed within one window.",
            )?;
        }
        let first_message_at = text(&channel, "occurredAt")?;
        let occurred_at = parse_date(&first_message_at);
        need(
            occurred_at.is_finite(),
            "secretary_message_event_invalid",
            "The message time is invalid.",
        )?;
        if let Some(window) = saved.as_ref().and_then(|s| pending(&s.value)) {
            if occurred_at >= parse_date(&text(window, "dueAt")?) {
                self.flush_message_window(saved.as_ref().unwrap(), configuration)
                    .await?;
                saved = journal.named(MESSAGE_WINDOW_KEY, &name, true)?;
            }
        }
        let window = if let Some(window) = saved.as_ref().and_then(|s| pending(&s.value)) {
            let mut next = window.clone();
            if let Some(item_id) = item_id {
                let mut ids = window
                    .required("itemIds")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .to_vec();
                ids.push(item_id.clone());
                next.insert("itemIds", IvyJson::Array(ids));
            }
            let mut ids = window
                .required("eventIds")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .to_vec();
            ids.push(IvyJson::String(event_id.unwrap().clone()));
            next.insert("eventIds", IvyJson::Array(ids));
            next
        } else {
            IvyJson::object([
                ("assignment", assignment.required("pin")?.clone()),
                ("assignmentSnapshot", assignment.required("value")?.clone()),
                ("firstSequence", event.required("sequence")?.clone()),
                ("firstMessageAt", IvyJson::String(first_message_at)),
                (
                    "dueAt",
                    IvyJson::String(iso(occurred_at + MESSAGE_WINDOW_MS as f64)?),
                ),
                ("serviceNodeId", IvyJson::String(service_id)),
                ("channel", channel.clone()),
                (
                    "itemIds",
                    IvyJson::Array(item_id.cloned().into_iter().collect()),
                ),
                (
                    "eventIds",
                    IvyJson::Array(vec![IvyJson::String(event_id.unwrap().clone())]),
                ),
            ])
        };
        let next = IvyJson::object([
            (
                "assignmentId",
                assignment
                    .required("value")?
                    .required("assignmentId")?
                    .clone(),
            ),
            ("lastSequence", event.required("sequence")?.clone()),
            ("pending", window),
        ]);
        if let Some(saved) = saved {
            journal.amend_technical(MESSAGE_WINDOW_KEY, &saved.pin, &next)?;
        } else {
            journal.create_technical(MESSAGE_WINDOW_KEY, &name, &next)?;
        }
        Ok(())
    }
    pub async fn flush_due_message_windows(
        &self,
        assignment: &IvyJson,
        configuration: &IvyJson,
        now: i64,
    ) -> IvyResult<()> {
        for saved in self.evidence.store.journal.list(MESSAGE_WINDOW_KEY, true)? {
            if saved.value.get("assignmentId") == assignment.required("value")?.get("assignmentId")
            {
                if let Some(window) = pending(&saved.value) {
                    if parse_date(&text(window, "dueAt")?) <= now as f64 {
                        self.flush_message_window(&saved, configuration).await?;
                    }
                }
            }
        }
        Ok(())
    }
}
