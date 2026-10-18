//! Durable producer-independent message and object event windows.
use crate::assignment_error::{SchedulerFailure, SchedulerResult, untyped};
use crate::{
    assignment_decision::trimmed,
    assignment_message::{
        MESSAGE_BATCH_KIND, MESSAGE_WINDOW_KEY, MESSAGE_WINDOW_MS, message_channel,
    },
    assignment_object::{OBJECT_WINDOW_KEY, Observation, object_projection, observation_ready},
    assignment_scheduler::AssignmentScheduler,
    journal::Document,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json, json::try_js_string};
use ivy_sdk::{
    clock::{date_to_iso, parse_date},
    rpc::RequestOptions,
};

fn need(ok: bool, code: &str, message: &str) -> SchedulerResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message).into())
    }
}
fn text(value: &IvyJson, key: &str) -> SchedulerResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or(SchedulerFailure::Uncoded)
}
fn number(value: &IvyJson, key: &str) -> SchedulerResult<f64> {
    value
        .required(key)?
        .as_f64()
        .ok_or(SchedulerFailure::Uncoded)
}
fn pending(value: &IvyJson) -> Option<&IvyJson> {
    value.get("pending").filter(|v| !v.is_null())
}
fn iso(value: f64) -> SchedulerResult<JsString> {
    date_to_iso(value).map_err(SchedulerFailure::uncoded)
}
fn projection(value: &IvyJson, trigger: &IvyJson) -> SchedulerResult<IvyJson> {
    let paths = trigger
        .required("paths")?
        .as_array()
        .ok_or(SchedulerFailure::Uncoded)?
        .iter()
        .map(|v| v.as_js_string().cloned().ok_or(SchedulerFailure::Uncoded))
        .collect::<SchedulerResult<Vec<_>>>()?;
    Ok(object_projection(value, &paths))
}
fn joined(parts: &[IvyJson]) -> SchedulerResult<JsString> {
    let mut result = JsString::default();
    for part in parts {
        result.0.extend(coerce_string(part)?.0);
    }
    Ok(result)
}
pub(crate) fn revision_number(value: Option<&IvyJson>) -> SchedulerResult<f64> {
    let n = match value {
        Some(IvyJson::Number(n)) => *n,
        Some(IvyJson::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(IvyJson::Null) => 0.0,
        Some(v @ (IvyJson::String(_) | IvyJson::Array(_))) => {
            let s = coerce_string(v)?;
            let s = String::from_utf16(trimmed(&s.0)).unwrap_or_default();
            if s.is_empty() {
                0.0
            } else {
                let radix = if s.starts_with("0x") || s.starts_with("0X") {
                    Some(16)
                } else if s.starts_with("0b") || s.starts_with("0B") {
                    Some(2)
                } else if s.starts_with("0o") || s.starts_with("0O") {
                    Some(8)
                } else {
                    None
                };
                if let Some(radix) = radix {
                    if s.len() == 2 {
                        f64::NAN
                    } else {
                        s[2..]
                            .chars()
                            .try_fold(0.0, |n, c| {
                                c.to_digit(radix).map(|d| n * radix as f64 + d as f64)
                            })
                            .unwrap_or(f64::NAN)
                    }
                } else {
                    s.parse().unwrap_or(f64::NAN)
                }
            }
        }
        Some(v @ IvyJson::Object(_)) => {
            coerce_string(v)?;
            f64::NAN
        }
        None => f64::NAN,
    };
    Ok(n)
}
// Only Number(payload.revision)'s positive-safe-integer result is admissible.
fn object_revision(value: Option<&IvyJson>) -> SchedulerResult<f64> {
    let n = revision_number(value)?;
    need(
        n.is_finite() && n.fract() == 0.0 && n > 0.0 && n <= 9_007_199_254_740_991.0,
        "secretary_object_event_invalid",
        "Object change has no valid revision.",
    )?;
    Ok(n)
}
impl AssignmentScheduler<'_, '_, '_> {
    pub(crate) async fn collect_object_event_inner(
        &self,
        assignment: &IvyJson,
        event: &IvyJson,
    ) -> SchedulerResult<()> {
        let value = assignment.required("value")?;
        let trigger = value.required("trigger")?;
        if trigger.get("kind") != Some(&"object-change".into()) {
            return Ok(());
        }
        let payload = event.required("payload")?;
        if payload.is_null() {
            return Err(SchedulerFailure::Uncoded);
        }
        let object_id = payload
            .get("objectId")
            .map(coerce_string)
            .transpose()?
            .unwrap_or_else(|| "undefined".into());
        let revision = object_revision(payload.get("revision"))?;
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
    pub(crate) async fn flush_due_object_windows_inner(
        &self,
        assignment: &IvyJson,
        configuration: &IvyJson,
        now: i64,
    ) -> SchedulerResult<()> {
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
                    Ok::<_, SchedulerFailure>(Observation {
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
                    IvyJson::String(joined(&[
                        "object-batch:".into(),
                        window.required("firstSequence")?.clone(),
                        ":".into(),
                        window.required("objectId")?.clone(),
                    ])?),
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
    pub(crate) async fn flush_message_window_inner(
        &self,
        saved: &Document,
        configuration: &IvyJson,
    ) -> SchedulerResult<()> {
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
            .unwrap_or_else(|| "undefined".into());
        let event = IvyJson::object([
            ("kind", "event".into()),
            (
                "key",
                IvyJson::String(joined(&[
                    "message-batch:".into(),
                    window.required("firstSequence")?.clone(),
                    ":".into(),
                    first_id,
                ])?),
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
    pub(crate) async fn collect_message_event_inner(
        &self,
        assignment: &IvyJson,
        event: &IvyJson,
        configuration: &IvyJson,
    ) -> SchedulerResult<()> {
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
                self.flush_message_window_inner(saved.as_ref().unwrap(), configuration)
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
                    .ok_or(SchedulerFailure::Uncoded)?
                    .to_vec();
                ids.push(item_id.clone());
                next.insert("itemIds", IvyJson::Array(ids));
            }
            let mut ids = window
                .required("eventIds")?
                .as_array()
                .ok_or(SchedulerFailure::Uncoded)?
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
    pub(crate) async fn flush_due_message_windows_inner(
        &self,
        assignment: &IvyJson,
        configuration: &IvyJson,
        now: i64,
    ) -> SchedulerResult<()> {
        for saved in self.evidence.store.journal.list(MESSAGE_WINDOW_KEY, true)? {
            if saved.value.get("assignmentId") == assignment.required("value")?.get("assignmentId")
            {
                if let Some(window) = pending(&saved.value) {
                    if parse_date(&text(window, "dueAt")?) <= now as f64 {
                        self.flush_message_window_inner(&saved, configuration)
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn coerce_string(value: &IvyJson) -> SchedulerResult<JsString> {
    untyped(try_js_string(value))
}

impl AssignmentScheduler<'_, '_, '_> {
    pub async fn collect_object_event(
        &self,
        assignment: &IvyJson,
        event: &IvyJson,
    ) -> IvyResult<()> {
        self.collect_object_event_inner(assignment, event)
            .await
            .map_err(IvyError::from)
    }
    pub async fn flush_due_object_windows(
        &self,
        assignment: &IvyJson,
        configuration: &IvyJson,
        now: i64,
    ) -> IvyResult<()> {
        self.flush_due_object_windows_inner(assignment, configuration, now)
            .await
            .map_err(IvyError::from)
    }
    pub async fn flush_message_window(
        &self,
        saved: &Document,
        configuration: &IvyJson,
    ) -> IvyResult<()> {
        self.flush_message_window_inner(saved, configuration)
            .await
            .map_err(IvyError::from)
    }
    pub async fn collect_message_event(
        &self,
        assignment: &IvyJson,
        event: &IvyJson,
        configuration: &IvyJson,
    ) -> IvyResult<()> {
        self.collect_message_event_inner(assignment, event, configuration)
            .await
            .map_err(IvyError::from)
    }
    pub async fn flush_due_message_windows(
        &self,
        assignment: &IvyJson,
        configuration: &IvyJson,
        now: i64,
    ) -> IvyResult<()> {
        self.flush_due_message_windows_inner(assignment, configuration, now)
            .await
            .map_err(IvyError::from)
    }
}
