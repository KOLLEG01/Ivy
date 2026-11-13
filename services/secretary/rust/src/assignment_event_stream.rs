//! One filtered stream and at most one original retirement per assignment.
use crate::{assignment_scheduler::AssignmentScheduler, store::Record};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::rpc::RequestOptions;

fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
pub(crate) fn stable_name(key: &JsString) -> IvyResult<JsString> {
    Ok(format!(
        "secretary-assignment-{}",
        &hash_json(&IvyJson::String(key.clone()))?[7..48]
    )
    .into())
}
fn stream_name(key: &JsString, initial: &IvyJson, filter: &IvyJson) -> IvyResult<JsString> {
    Ok(format!(
        "{}-s-{}-{}",
        stable_name(key)?.to_utf8()?,
        initial.as_f64().ok_or_else(IvyError::internal)? as u64,
        &hash_json(filter)?[7..23]
    )
    .into())
}
pub(crate) fn owned_name(key: &JsString, name: &JsString) -> IvyResult<()> {
    let stable = stable_name(key)?;
    if name == &stable
        || name
            .0
            .starts_with(&JsString::from(format!("{}-s-", stable.to_utf8()?)).0)
    {
        Ok(())
    } else {
        Err(IvyError::new(
            "secretary_event_progress_mismatch",
            "The event subscription belongs to another assignment.",
        ))
    }
}

/// Include assignment changes from every authorized writer. Hive combines filter
/// fields with AND, so a producer-only source filter would hide those changes.
pub fn event_filter(progress: &IvyJson) -> IvyResult<IvyJson> {
    if progress.get("needsRebase") == Some(&true.into()) {
        // Rebase against the first global event after a gap, before filtering.
        return Ok(IvyJson::Object(vec![]));
    }
    let own = progress
        .required("assignment")?
        .required("objectId")?
        .clone();
    let assignment = progress.required("assignmentSnapshot")?;
    let trigger = assignment.required("trigger")?;
    let enabled = progress.get("active") == Some(&true.into())
        && assignment.get("enabled") == Some(&true.into());
    let mut topics = vec![IvyJson::from("hive.object.changed")];
    let mut objects = Some(vec![own]);
    if enabled && trigger.get("kind") == Some(&"event".into()) {
        let topic = trigger.required("topic")?;
        if !topics.contains(topic) {
            topics.push(topic.clone());
        }
        objects = None;
    } else if enabled && trigger.get("kind") == Some(&"object-change".into()) {
        objects.as_mut().unwrap().extend(
            trigger
                .required("objectIds")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .cloned(),
        );
    }
    let mut filter = IvyJson::object([("topics", IvyJson::Array(topics))]);
    if let Some(mut ids) = objects {
        ids.sort_by(|a, b| {
            a.as_js_string()
                .unwrap()
                .0
                .cmp(&b.as_js_string().unwrap().0)
        });
        ids.dedup();
        filter.insert("objectIds", IvyJson::Array(ids));
    }
    Ok(filter)
}

pub(crate) struct EventStream {
    pub name: JsString,
    pub filter: IvyJson,
    initial: IvyJson,
}
impl EventStream {
    pub fn current(key: &JsString, progress: Option<&IvyJson>) -> IvyResult<Self> {
        if let Some(value) = progress.and_then(|p| p.get("subscription")) {
            let progress = progress.unwrap();
            let filter = event_filter(progress)?;
            let initial = value.required("initialSequence")?.clone();
            let name = text(value, "name")?;
            if name != stream_name(key, &initial, &filter)?
                || initial.as_f64() > progress.required("processedThrough")?.as_f64()
            {
                return Err(IvyError::new(
                    "secretary_event_progress_mismatch",
                    "The filtered subscription differs from its saved cursor and assignment.",
                ));
            }
            return Ok(Self {
                name,
                filter,
                initial,
            });
        }
        // Adopt the existing empty subscription only through an issued page.
        Ok(Self {
            name: stable_name(key)?,
            filter: IvyJson::Object(vec![]),
            initial: 0.0.into(),
        })
    }
    pub fn parameters(&self) -> IvyJson {
        IvyJson::object([
            ("name", IvyJson::String(self.name.clone())),
            ("filter", self.filter.clone()),
            ("initialSequence", self.initial.clone()),
            ("durable", true.into()),
            ("limit", 100.0.into()),
        ])
    }
}
impl AssignmentScheduler<'_, '_, '_> {
    pub(crate) async fn retire_assignment_event_streams(
        &self,
        key: &JsString,
        object_id: &JsString,
        retirement: &JsString,
    ) -> IvyResult<()> {
        let progress_name: JsString =
            format!("event-{}", &hash_json(&IvyJson::String(key.clone()))?[7..]).into();
        let progress = self
            .evidence
            .store
            .named(Record::EventProgress, &progress_name)
            .await?;
        let value = progress.as_ref().map(|p| p.required("value")).transpose()?;
        if let Some(value) = value {
            if value.get("assignmentId").and_then(IvyJson::as_js_string) != Some(key)
                || value
                    .required("assignment")?
                    .get("objectId")
                    .and_then(IvyJson::as_js_string)
                    != Some(object_id)
            {
                return Err(IvyError::new(
                    "secretary_event_progress_mismatch",
                    "The archived assignment cannot retire another event stream.",
                ));
            }
        }
        let current = EventStream::current(key, value)?;
        let mut requests = Vec::new();
        if let Some(request) = value.and_then(|p| p.get("retiringSubscription")) {
            owned_name(key, &text(request, "name")?)?;
            requests.push(request.clone());
        }
        let mutation_id = self
            .operation(vec![
                "retire-assignment-subscription".into(),
                IvyJson::String(object_id.clone()),
                IvyJson::String(retirement.clone()),
                IvyJson::String(current.name.clone()),
            ])
            .await?;
        requests.push(IvyJson::object([
            ("name", IvyJson::String(current.name)),
            ("mutationId", IvyJson::String(mutation_id)),
        ]));
        for request in requests {
            self.evidence
                .store
                .client
                .request("events.unsubscribe", request, RequestOptions::default())
                .await?;
        }
        Ok(())
    }
    pub(crate) async fn switch_event_stream(
        &self,
        saved: &IvyJson,
        mut next: IvyJson,
        stream: &EventStream,
        boundary: IvyJson,
    ) -> IvyResult<IvyJson> {
        let key = text(&next, "assignmentId")?;
        let filter = event_filter(&next)?;
        let name = stream_name(&key, &boundary, &filter)?;
        if name == stream.name || next.get("retiringSubscription").is_some() {
            return Err(IvyError::new(
                "secretary_event_progress_mismatch",
                "Finish the original stream retirement before another handover.",
            ));
        }
        let mutation_id = self
            .operation(vec![
                "retire-event-stream".into(),
                IvyJson::String(key),
                IvyJson::String(stream.name.clone()),
                IvyJson::String(name.clone()),
            ])
            .await?;
        next.insert(
            "subscription",
            IvyJson::object([
                ("name", IvyJson::String(name)),
                ("initialSequence", boundary.clone()),
            ]),
        );
        next.insert(
            "retiringSubscription",
            IvyJson::object([
                ("name", IvyJson::String(stream.name.clone())),
                ("mutationId", IvyJson::String(mutation_id)),
            ]),
        );
        next.insert("processedThrough", boundary);
        next.insert(
            "updatedAt",
            IvyJson::String(ivy_sdk::clock::date_to_iso(self.clock.utc_millis() as f64)?),
        );
        self.evidence
            .store
            .amend(saved, Record::EventProgress, &next)
            .await
    }
    /// Called only after the replacement's durable subscribe succeeded. A lost
    /// reply reuses this exact saved mutation, including across process restarts.
    pub(crate) async fn retire_event_stream(
        &self,
        saved: IvyJson,
        stream: &EventStream,
    ) -> IvyResult<IvyJson> {
        let value = saved.required("value")?;
        let Some(request) = value.get("retiringSubscription") else {
            return Ok(saved);
        };
        let key = text(value, "assignmentId")?;
        let name = text(request, "name")?;
        owned_name(&key, &name)?;
        if name == stream.name {
            return Err(IvyError::internal());
        }
        self.evidence.verify_owner().await?;
        self.evidence
            .store
            .client
            .request(
                "events.unsubscribe",
                request.clone(),
                RequestOptions::default(),
            )
            .await?;
        let mut next = value.clone();
        next.remove("retiringSubscription");
        self.evidence
            .store
            .amend(&saved, Record::EventProgress, &next)
            .await
    }
}
