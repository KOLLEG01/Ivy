//! Durable event effects, historical snapshots, gap rebasing and ACK ordering.
use crate::assignment_error::{SchedulerFailure, SchedulerResult, untyped};
use crate::{
    assignment_message::message_channel, assignment_scheduler::AssignmentScheduler,
    assignment_windows::revision_number, inbox_recovery::RecoveryScan, store::Record,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json, json::try_js_string};
use ivy_sdk::{
    clock::{date_to_iso, parse_date},
    operation_ids::new_operation_id,
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
fn joined(parts: &[IvyJson]) -> SchedulerResult<JsString> {
    let mut result = JsString::default();
    for part in parts {
        result.0.extend(coerce_string(part)?.0);
    }
    Ok(result)
}
impl AssignmentScheduler<'_, '_, '_> {
    pub(crate) fn event_matches_inner(
        &self,
        assignment: &IvyJson,
        event: &IvyJson,
    ) -> SchedulerResult<bool> {
        let trigger = assignment.required("trigger")?;
        let own_source = joined(&[
            "principal:".into(),
            self.evidence
                .settings
                .required("identity")?
                .required("principalId")?
                .clone(),
        ])?;
        let object_event = event.get("topic") == Some(&"hive.object.changed".into());
        let own = event.get("source") == Some(&IvyJson::String(own_source));
        let payload = event.get("payload");
        if trigger.get("kind") == Some(&"object-change".into()) {
            if !object_event || own {
                return Ok(false);
            }
            return Ok(payload.is_some_and(|p| {
                p.get("operation") == Some(&"write".into())
                    && p.get("objectId").and_then(IvyJson::as_js_string).is_some()
                    && trigger
                        .get("objectIds")
                        .and_then(IvyJson::as_array)
                        .is_some_and(|ids| ids.contains(p.get("objectId").unwrap()))
            }));
        }
        if trigger.get("kind") != Some(&"event".into())
            || event.get("topic") != trigger.get("topic")
            || event.get("topicVersion") != trigger.get("topicVersion")
            || (object_event && own)
        {
            return Ok(false);
        }
        if let Some(service_id) = trigger.get("sourceServiceNodeId").filter(|v| !v.is_null()) {
            if event.get("source")
                != Some(&IvyJson::String(joined(&[
                    "service:".into(),
                    service_id.clone(),
                ])?))
            {
                return Ok(false);
            }
        }
        Ok(trigger.get("eventKind").is_none_or(IvyJson::is_null)
            || payload
                .filter(|p| p.as_object().is_some())
                .and_then(|p| p.get("kind"))
                == trigger.get("eventKind"))
    }
    pub(crate) async fn revision_at_inner(
        &self,
        assignment: &IvyJson,
        at: &JsString,
    ) -> SchedulerResult<Option<IvyJson>> {
        let pin = assignment.required("pin")?;
        let latest = number(pin, "revision")? as i64;
        for revision in (1..=latest).rev() {
            let candidate = if revision == latest {
                assignment.clone()
            } else {
                self.evidence
                    .store
                    .read(
                        Record::Assignment,
                        &IvyJson::object([
                            ("objectId", pin.required("objectId")?.clone()),
                            ("revision", IvyJson::Number(revision as f64)),
                        ]),
                    )
                    .await?
            };
            let written_at = candidate
                .get("writtenAt")
                .filter(|v| !v.is_null())
                .unwrap_or(candidate.required("value")?.required("updatedAt")?);
            if parse_date(written_at.as_js_string().ok_or(SchedulerFailure::Uncoded)?)
                <= parse_date(at)
            {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }
    pub(crate) async fn events_inner(
        &self,
        assignment: &IvyJson,
        configuration: &IvyJson,
        budget: usize,
        issues: &mut RecoveryScan,
    ) -> SchedulerResult<bool> {
        let store = self.evidence.store;
        let value = assignment.required("value")?;
        let key = text(value, "assignmentId")?;
        let key_text = String::from_utf16_lossy(&key.0);
        let gap_issue = format!("event-gap:{key_text}");
        let hash = hash_json(&IvyJson::String(key.clone()))?;
        let name = JsString::from(format!("secretary-assignment-{}", &hash[7..48]));
        let progress_name = JsString::from(format!("event-{}", &hash[7..]));
        let mut progress = store.named(Record::EventProgress, &progress_name).await?;
        if progress.is_none()
            && assignment.required("pin")?.get("revision") == Some(&IvyJson::Number(1.0))
            && value.required("trigger")?.get("kind") == Some(&"schedule".into())
        {
            return Ok(true);
        }
        for page in 0..budget {
            let batch = store
                .client
                .request(
                    "events.subscribe",
                    IvyJson::object([
                        ("name", IvyJson::String(name.clone())),
                        ("filter", IvyJson::Object(vec![])),
                        ("initialSequence", IvyJson::Number(0.0)),
                        ("durable", true.into()),
                        ("limit", IvyJson::Number(100.0)),
                    ]),
                    RequestOptions::default(),
                )
                .await?;
            let items = batch
                .required("items")?
                .as_array()
                .ok_or(SchedulerFailure::Uncoded)?;
            let gap = batch.get("gap").filter(|v| !v.is_null());
            if progress.is_none() {
                need(
                    gap.is_some()
                        || batch.get("throughSequence") == Some(&IvyJson::Number(0.0))
                        || items.first().and_then(|e| e.get("sequence"))
                            == Some(&IvyJson::Number(1.0)),
                    "secretary_event_progress_missing",
                    "The event subscription advanced without its durable assignment progress.",
                )?;
                let original = store.original(Record::Assignment, assignment).await?;
                let initial = IvyJson::object([
                    ("schemaVersion", IvyJson::Number(1.0)),
                    ("assignmentId", IvyJson::String(key.clone())),
                    ("assignment", original.required("pin")?.clone()),
                    ("assignmentSnapshot", original.required("value")?.clone()),
                    ("processedThrough", IvyJson::Number(0.0)),
                    ("active", false.into()),
                    ("needsRebase", false.into()),
                    (
                        "updatedAt",
                        IvyJson::String(
                            date_to_iso(self.clock.utc_millis() as f64)
                                .map_err(SchedulerFailure::uncoded)?,
                        ),
                    ),
                ]);
                let operation = self
                    .operation(vec!["event-progress".into(), IvyJson::String(key.clone())])
                    .await?;
                progress = Some(
                    store
                        .create(Record::EventProgress, &progress_name, &initial, &operation)
                        .await?,
                );
            }
            let saved = progress.as_ref().unwrap();
            let current = saved.required("value")?;
            need(
                current.get("assignmentId") == Some(&IvyJson::String(key.clone()))
                    && current.required("assignment")?.get("objectId")
                        == assignment.required("pin")?.get("objectId"),
                "secretary_event_progress_mismatch",
                "The durable event cursor belongs to another assignment.",
            )?;
            let processed = number(current, "processedThrough")?;
            let through = number(&batch, "throughSequence")?;
            let mut ack = IvyJson::object([
                ("name", IvyJson::String(name.clone())),
                (
                    "throughSequence",
                    batch.required("throughSequence")?.clone(),
                ),
            ]);
            if let Some(gap) = gap {
                ack.insert(
                    "gapThroughSequence",
                    gap.required("prunedThroughSequence")?.clone(),
                );
            }
            if processed > through {
                need(
                    batch.get("hasMore") == Some(&IvyJson::Bool(true)),
                    "secretary_event_progress_conflict",
                    "The event subscription moved behind its durable progress.",
                )?;
                store
                    .client
                    .request("events.ack", ack, RequestOptions::default())
                    .await?;
                continue;
            }
            if processed < through {
                let mut next = current.clone();
                let mut state_changed = false;
                if let Some(gap) = gap {
                    let prior = store
                        .journal
                        .named("secretary/event-gap", &key_text, true)?;
                    let relevant = prior
                        .as_ref()
                        .is_some_and(|p| p.value.get("relevant") == Some(&IvyJson::Bool(true)))
                        || current.get("active") == Some(&IvyJson::Bool(true));
                    let value = IvyJson::object([
                        (
                            "prunedThroughSequence",
                            gap.required("prunedThroughSequence")?.clone(),
                        ),
                        (
                            "relevant",
                            if relevant { true.into() } else { IvyJson::Null },
                        ),
                    ]);
                    if let Some(prior) = prior {
                        store
                            .journal
                            .amend_technical("secretary/event-gap", &prior.pin, &value)?;
                    } else {
                        store
                            .journal
                            .create_technical("secretary/event-gap", &key_text, &value)?;
                    }
                    if relevant {
                        issues.issue(&gap_issue, "secretary_event_gap");
                    }
                    next.insert("needsRebase", true.into());
                    state_changed = true;
                }
                if next.get("needsRebase") == Some(&IvyJson::Bool(true)) && !items.is_empty() {
                    let rebased = self
                        .revision_at_inner(assignment, &text(&items[0], "occurredAt")?)
                        .await?;
                    if let Some(gap) =
                        store
                            .journal
                            .named("secretary/event-gap", &key_text, true)?
                    {
                        if rebased.is_none()
                            && gap.value.get("relevant") != Some(&IvyJson::Bool(true))
                        {
                            store
                                .journal
                                .delete_technical("secretary/event-gap", &gap.pin.object_id)?;
                            issues.issues.retain(|(id, _)| id != &gap_issue);
                        } else {
                            let mut value = gap.value.clone();
                            value.insert("relevant", true.into());
                            store.journal.amend_technical(
                                "secretary/event-gap",
                                &gap.pin,
                                &value,
                            )?;
                            issues.issue(&gap_issue, "secretary_event_gap");
                        }
                    }
                    if let Some(rebased) = &rebased {
                        next.insert("assignment", rebased.required("pin")?.clone());
                        next.insert("assignmentSnapshot", rebased.required("value")?.clone());
                    }
                    next.insert("active", rebased.is_some().into());
                    next.insert("needsRebase", false.into());
                    state_changed = true;
                }
                for event in items {
                    if number(event, "sequence")? <= processed {
                        continue;
                    }
                    if next.get("active") == Some(&IvyJson::Bool(true))
                        && next.required("assignmentSnapshot")?.get("enabled")
                            == Some(&IvyJson::Bool(true))
                        && self.event_matches_inner(next.required("assignmentSnapshot")?, event)?
                    {
                        let selected = IvyJson::object([
                            ("pin", next.required("assignment")?.clone()),
                            ("value", next.required("assignmentSnapshot")?.clone()),
                        ]);
                        if selected.required("value")?.required("trigger")?.get("kind")
                            == Some(&"object-change".into())
                        {
                            self.collect_object_event_inner(&selected, event).await?;
                        } else if text(event, "source")?
                            .0
                            .starts_with(&JsString::from("service:").0)
                            && event.get("payload").and_then(message_channel).is_some()
                        {
                            self.collect_message_event_inner(&selected, event, configuration)
                                .await?;
                        } else {
                            let trigger = IvyJson::object([
                                ("kind", "event".into()),
                                (
                                    "key",
                                    IvyJson::String(joined(&[
                                        "event:".into(),
                                        event.required("sequence")?.clone(),
                                        ":".into(),
                                        event.required("mutationId")?.clone(),
                                    ])?),
                                ),
                                ("occurredAt", event.required("occurredAt")?.clone()),
                                (
                                    "payload",
                                    IvyJson::object([
                                        ("topic", event.required("topic")?.clone()),
                                        ("topicVersion", event.required("topicVersion")?.clone()),
                                        ("source", event.required("source")?.clone()),
                                        ("mutationId", event.required("mutationId")?.clone()),
                                        ("data", event.required("payload")?.clone()),
                                    ]),
                                ),
                            ]);
                            self.enqueue(&selected, &trigger, configuration).await?;
                        }
                    }
                    if event.get("topic") == Some(&"hive.object.changed".into()) {
                        if let Some(payload) =
                            event.get("payload").filter(|p| p.as_object().is_some())
                        {
                            if payload.get("objectId")
                                == assignment.required("pin")?.get("objectId")
                            {
                                let revision = revision_number(payload.get("revision"))?;
                                let retained = number(next.required("assignment")?, "revision")?;
                                if revision.is_finite()
                                    && revision.fract() == 0.0
                                    && revision.abs() <= 9_007_199_254_740_991.0
                                    && revision > retained
                                {
                                    let changed = store
                                        .read(
                                            Record::Assignment,
                                            &IvyJson::object([
                                                (
                                                    "objectId",
                                                    assignment
                                                        .required("pin")?
                                                        .required("objectId")?
                                                        .clone(),
                                                ),
                                                ("revision", IvyJson::Number(revision)),
                                            ]),
                                        )
                                        .await?;
                                    next.insert("assignment", changed.required("pin")?.clone());
                                    next.insert(
                                        "assignmentSnapshot",
                                        changed.required("value")?.clone(),
                                    );
                                    next.insert("active", true.into());
                                    state_changed = true;
                                } else if revision == retained
                                    && next.get("active") != Some(&IvyJson::Bool(true))
                                {
                                    next.insert("active", true.into());
                                    state_changed = true;
                                }
                            }
                        }
                    }
                }
                if state_changed {
                    next.insert(
                        "processedThrough",
                        batch.required("throughSequence")?.clone(),
                    );
                    next.insert(
                        "updatedAt",
                        IvyJson::String(
                            date_to_iso(self.clock.utc_millis() as f64)
                                .map_err(SchedulerFailure::uncoded)?,
                        ),
                    );
                    progress = Some(store.amend(saved, Record::EventProgress, &next).await?);
                }
            }
            store
                .client
                .request("events.ack", ack, RequestOptions::default())
                .await?;
            if page == 0 {
                if let Some(legacy) =
                    store
                        .journal
                        .named("secretary/event-subscription", &key_text, true)?
                {
                    if legacy.value.get("name") != Some(&IvyJson::String(name.clone())) {
                        let mutation_id = new_operation_id(store.client.clone(), None).await?;
                        store
                            .client
                            .request(
                                "events.unsubscribe",
                                IvyJson::object([
                                    ("name", legacy.value.required("name")?.clone()),
                                    ("mutationId", IvyJson::String(mutation_id)),
                                ]),
                                RequestOptions::default(),
                            )
                            .await?;
                        store.journal.delete_technical(
                            "secretary/event-subscription",
                            &legacy.pin.object_id,
                        )?;
                    }
                }
            }
            if batch.get("hasMore") != Some(&IvyJson::Bool(true)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn coerce_string(value: &IvyJson) -> SchedulerResult<JsString> {
    untyped(try_js_string(value))
}

impl AssignmentScheduler<'_, '_, '_> {
    pub fn event_matches(&self, assignment: &IvyJson, event: &IvyJson) -> IvyResult<bool> {
        self.event_matches_inner(assignment, event)
            .map_err(IvyError::from)
    }
    pub async fn revision_at(
        &self,
        assignment: &IvyJson,
        at: &JsString,
    ) -> IvyResult<Option<IvyJson>> {
        self.revision_at_inner(assignment, at)
            .await
            .map_err(IvyError::from)
    }
    pub async fn events(
        &self,
        assignment: &IvyJson,
        configuration: &IvyJson,
        budget: usize,
        issues: &mut RecoveryScan,
    ) -> IvyResult<bool> {
        self.events_inner(assignment, configuration, budget, issues)
            .await
            .map_err(IvyError::from)
    }
}
