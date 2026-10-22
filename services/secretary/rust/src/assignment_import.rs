//! One-time schedule and subscription conversion. Call only while the selected
//! journal's normal workers are stopped. Original local records stay readable.
use crate::{
    assignment_events::EventScan,
    assignment_progress::written_at,
    assignment_scheduler::AssignmentScheduler,
    assignment_schema::assignment_name,
    inbox_recovery::{RecoveryIssues, RecoveryScan},
    journal::Document,
    store::Record,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::hash_json,
    json::{canonical, parse},
    operation::parse_operation_id_js,
};
use ivy_sdk::{
    clock::{date_to_iso, parse_date},
    operation_ids::new_operation_id,
    rpc::RequestOptions,
};
use ivy_store::{rusqlite::OptionalExtension, sqlite};

const CURSOR: &str = "secretary/schedule-cursor";
const SUBSCRIPTION: &str = "secretary/event-subscription";

fn text<'a>(value: &'a IvyJson, field: &str) -> IvyResult<&'a JsString> {
    value
        .required(field)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn identity(ok: bool) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(
            "secretary_import_identity_conflict",
            "The retained import identity changed.",
        ))
    }
}
fn stable_name(id: &JsString) -> IvyResult<JsString> {
    Ok(format!(
        "secretary-assignment-{}",
        &hash_json(&IvyJson::String(id.clone()))?[7..48]
    )
    .into())
}

pub struct ImportedSchedule {
    pub source: Document,
    pub before: Option<IvyJson>,
    pub after: IvyJson,
}

/// Preserve the original current-updatedAt/revision-one cursor selection. A
/// durable progress value wins; source revisions are never compacted or removed.
pub async fn import_schedule_progress(
    scheduler: &AssignmentScheduler<'_, '_, '_>,
    assignment: &IvyJson,
    now: i64,
) -> IvyResult<Option<ImportedSchedule>> {
    let evidence = scheduler.evidence;
    evidence.verify_owner().await?;
    let store = evidence.store;
    let latest = crate::import_record::read(
        store,
        Record::Assignment,
        assignment.required("pin")?.required("objectId")?,
    )
    .await?;
    let value = latest.required("value")?;
    let id = text(value, "assignmentId")?;
    identity(
        Some(id)
            == assignment
                .required("value")?
                .get("assignmentId")
                .and_then(IvyJson::as_js_string),
    )?;
    let Some(source) = store
        .journal
        .named(CURSOR, &String::from_utf16_lossy(&id.0), true)?
    else {
        return Ok(None);
    };
    let name = JsString::from(format!(
        "schedule-{}",
        &hash_json(&IvyJson::String(id.clone()))?[7..]
    ));
    let before = store.named(Record::ScheduleProgress, &name).await?;
    let after = if let Some(saved) = &before {
        saved.clone()
    } else {
        let current = source.value.get("updatedAt") == value.get("updatedAt");
        let original = if current {
            latest.clone()
        } else {
            crate::import_record::original(store, Record::Assignment, &latest).await?
        };
        let checked_at =
            if current || latest.required("pin")?.get("revision") == Some(&IvyJson::Number(1.0)) {
                source.value.required("checkedAt")?.clone()
            } else {
                IvyJson::String(written_at(&original, "createdAt")?)
            };
        let progress = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("assignmentId", IvyJson::String(id.clone())),
            ("assignment", original.required("pin")?.clone()),
            ("assignmentSnapshot", original.required("value")?.clone()),
            ("checkedAt", checked_at),
            ("updatedAt", IvyJson::String(date_to_iso(now as f64)?)),
        ]);
        let operation = scheduler
            .operation(vec![
                "schedule-progress".into(),
                IvyJson::String(id.clone()),
            ])
            .await?;
        store
            .create(Record::ScheduleProgress, &name, &progress, &operation)
            .await?
    };
    let saved = after.required("value")?;
    if saved.get("assignmentId") != Some(&IvyJson::String(id.clone()))
        || saved.required("assignment")?.get("objectId") != latest.required("pin")?.get("objectId")
    {
        return Err(IvyError::new(
            "secretary_schedule_progress_mismatch",
            "The durable schedule cursor belongs to another assignment.",
        ));
    }
    Ok(Some(ImportedSchedule {
        source,
        before,
        after,
    }))
}

pub struct ImportedEventGap {
    pub before: Document,
    pub after: Document,
}
/// Resolve the old implicit gap classification once. Irrelevant gaps become
/// explicit false values instead of deleting their original values/history.
pub async fn import_event_gap(
    scheduler: &AssignmentScheduler<'_, '_, '_>,
    assignment: &IvyJson,
    issues: &std::cell::RefCell<RecoveryScan>,
) -> IvyResult<Option<ImportedEventGap>> {
    scheduler.evidence.verify_owner().await?;
    let store = scheduler.evidence.store;
    let latest = crate::import_record::read(
        store,
        Record::Assignment,
        assignment.required("pin")?.required("objectId")?,
    )
    .await?;
    let id = text(latest.required("value")?, "assignmentId")?;
    identity(
        Some(id)
            == assignment
                .required("value")?
                .get("assignmentId")
                .and_then(IvyJson::as_js_string),
    )?;
    let name = String::from_utf16_lossy(&id.0);
    let Some(before) = store.journal.named("secretary/event-gap", &name, true)? else {
        return Ok(None);
    };
    if before.value.get("relevant").is_some() {
        return Ok(None);
    }
    let first = store
        .client
        .request(
            "events.read",
            IvyJson::object([
                (
                    "afterSequence",
                    before.value.required("prunedThroughSequence")?.clone(),
                ),
                ("filter", IvyJson::Object(vec![])),
                ("limit", IvyJson::Number(1.0)),
            ]),
            RequestOptions::default(),
        )
        .await?;
    let original = crate::import_record::original(store, Record::Assignment, &latest).await?;
    let written = written_at(&original, "createdAt")?;
    let before_assignment = first.get("gap").is_none_or(IvyJson::is_null)
        && first
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .first()
            .is_some_and(|event| {
                event
                    .get("occurredAt")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|at| parse_date(at) < parse_date(&written))
            });
    let mut value = before.value.clone();
    value.insert("relevant", (!before_assignment).into());
    scheduler.evidence.verify_owner().await?;
    let after =
        store
            .journal
            .import_technical_revision("secretary/event-gap", &before.pin, &value)?;
    let issue = format!("event-gap:{name}");
    if before_assignment {
        issues.clear(&issue);
    } else {
        issues.issue(&issue, "secretary_event_gap");
    }
    Ok(Some(ImportedEventGap { before, after }))
}

/// Archived input is the original root-filtered assignment query row, including
/// its identity/name/archive revision. Active input is a decoded Assignment.
pub enum SubscriptionAssignment<'a> {
    Active {
        assignment: &'a IvyJson,
        configuration: &'a IvyJson,
    },
    Archived(&'a IvyJson),
}
pub struct ImportedSubscription {
    pub source: Document,
    pub before: Option<IvyJson>,
    pub after: Option<IvyJson>,
    pub completed: bool,
}

fn state_key(source: &Document) -> String {
    format!(
        "secretary/import/event-subscription/{}",
        source.pin.object_id
    )
}
fn read_state(
    scheduler: &AssignmentScheduler<'_, '_, '_>,
    key: &str,
) -> IvyResult<Option<IvyJson>> {
    let raw: Option<String> = scheduler
        .evidence
        .store
        .journal
        .db
        .query_row("SELECT value FROM journal_state WHERE key=?", [key], |r| {
            r.get(0)
        })
        .optional()
        .map_err(sqlite::error)?;
    raw.map(|raw| parse(&raw, 33_554_432)).transpose()
}
fn validate_state(state: &IvyJson, expected: &IvyJson, names: &[JsString]) -> IvyResult<usize> {
    identity(
        state.get("schemaVersion") == Some(&IvyJson::Number(1.0))
            && canonical(state.required("identity")?, 33_554_432)?
                == canonical(expected, 33_554_432)?,
    )?;
    let requests = state
        .required("requests")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    identity(requests.len() == names.len())?;
    for (request, name) in requests.iter().zip(names) {
        identity(request.get("name") == Some(&IvyJson::String(name.clone())))?;
        parse_operation_id_js(text(request, "mutationId")?)?;
    }
    let completed = state
        .required("completed")?
        .as_f64()
        .ok_or_else(IvyError::internal)?;
    identity(
        completed.is_finite()
            && completed.fract() == 0.0
            && completed >= 0.0
            && completed <= names.len() as f64,
    )?;
    Ok(completed as usize)
}

/// Move one obsolete subscription into the stable subscription, or retire an
/// archived assignment's subscriptions. One active call handles at most one
/// current page. No native execution worker or provider delivery is started.
///
/// Requests are frozen in namespaced journal_state rows before unsubscribe.
/// Unknown replies retain those exact IDs across process/runtime-epoch changes.
/// This adds import receipts without invoking normal quota cleanup or modifying
/// any legacy document/revision/blob. Repeated completed import is read-only.
pub async fn import_event_subscription(
    scheduler: &AssignmentScheduler<'_, '_, '_>,
    selected: SubscriptionAssignment<'_>,
    issues: &mut RecoveryScan,
) -> IvyResult<Option<ImportedSubscription>> {
    scheduler.evidence.verify_owner().await?;
    let store = scheduler.evidence.store;
    let (id, object_id, archived, retirement, assignment, configuration) = match selected {
        SubscriptionAssignment::Active {
            assignment,
            configuration,
        } => {
            let latest = store
                .read(
                    Record::Assignment,
                    assignment.required("pin")?.required("objectId")?,
                )
                .await?;
            let id = text(latest.required("value")?, "assignmentId")?.clone();
            identity(
                Some(&id)
                    == assignment
                        .required("value")?
                        .get("assignmentId")
                        .and_then(IvyJson::as_js_string),
            )?;
            (
                id,
                latest.required("pin")?.required("objectId")?.clone(),
                false,
                IvyJson::Null,
                Some(latest),
                Some(configuration),
            )
        }
        SubscriptionAssignment::Archived(row) => {
            let values = row.required("values")?;
            let id = text(values, "data:/assignmentId")?.clone();
            if values.get("object.effectivelyArchived") != Some(&true.into())
                || values.get("object.name") != Some(&assignment_name(&id)?.into())
            {
                return Err(IvyError::new(
                    "secretary_assignment_identity_conflict",
                    "The archived assignment must retain its original identity.",
                ));
            }
            let revision = row
                .required("revision")?
                .as_f64()
                .ok_or_else(IvyError::internal)?;
            let archived_at = values
                .get("object.archivedAt")
                .filter(|value| !value.is_null())
                .map(IvyJson::as_str)
                .transpose()?
                .unwrap_or_else(|| "null".to_owned());
            let retirement: JsString = format!("{revision}:{archived_at}").into();
            (
                id,
                row.required("objectId")?.clone(),
                true,
                IvyJson::String(retirement),
                None,
                None,
            )
        }
    };
    let Some(source) = store
        .journal
        .named(SUBSCRIPTION, &String::from_utf16_lossy(&id.0), true)?
    else {
        return Ok(None);
    };
    let stable = stable_name(&id)?;
    let old_name = text(&source.value, "name")?.clone();
    let names = if archived {
        if old_name == stable {
            vec![stable.clone()]
        } else {
            vec![stable.clone(), old_name]
        }
    } else if old_name == stable {
        vec![]
    } else {
        vec![old_name]
    };
    let expected = IvyJson::object([
        ("rootObjectId", IvyJson::String(store.root.clone())),
        (
            "serviceNodeId",
            IvyJson::String(scheduler.evidence.service_node_id.clone()),
        ),
        (
            "secretaryId",
            scheduler
                .evidence
                .settings
                .required("identity")?
                .required("scope")?
                .required("secretaryId")?
                .clone(),
        ),
        ("assignmentId", IvyJson::String(id)),
        ("objectId", object_id.clone()),
        ("archived", archived.into()),
        (
            "source",
            IvyJson::object([
                ("objectId", source.pin.object_id.clone().into()),
                ("revision", IvyJson::Number(source.pin.revision as f64)),
            ]),
        ),
        ("sourceHash", hash_json(&source.value)?.into()),
    ]);
    let key = state_key(&source);
    let before = read_state(scheduler, &key)?;
    let mut state = if let Some(saved) = &before {
        validate_state(saved, &expected, &names)?;
        saved.clone()
    } else {
        if !archived && !names.is_empty() {
            let assignment = assignment.as_ref().unwrap();
            let progress_name = JsString::from(format!(
                "event-{}",
                &hash_json(assignment.required("value")?.required("assignmentId")?)?[7..]
            ));
            let progress = store.named(Record::EventProgress, &progress_name).await?;
            let EventScan {
                ready_to_retire, ..
            } = scheduler
                .process_events_inner(
                    assignment,
                    configuration.unwrap(),
                    1,
                    &std::cell::RefCell::new(issues),
                    progress,
                )
                .await?;
            if !ready_to_retire {
                return Ok(Some(ImportedSubscription {
                    source,
                    before,
                    after: None,
                    completed: false,
                }));
            }
        }
        let mut requests = Vec::new();
        for name in &names {
            let operation = if archived {
                scheduler
                    .operation(vec![
                        "retire-assignment-subscription".into(),
                        object_id.clone(),
                        retirement.clone(),
                        IvyJson::String(name.clone()),
                    ])
                    .await?
            } else {
                new_operation_id(store.client.clone(), None).await?
            };
            requests.push(IvyJson::object([
                ("name", IvyJson::String(name.clone())),
                ("mutationId", IvyJson::String(operation)),
            ]));
        }
        let prepared = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("identity", expected.clone()),
            ("requests", IvyJson::Array(requests)),
            ("completed", IvyJson::Number(0.0)),
        ]);
        store
            .journal
            .db
            .execute(
                "INSERT OR IGNORE INTO journal_state(key,value) VALUES(?,?)",
                [&key, &canonical(&prepared, 33_554_432)?],
            )
            .map_err(sqlite::error)?;
        read_state(scheduler, &key)?.ok_or_else(IvyError::internal)?
    };
    let completed = validate_state(&state, &expected, &names)?;
    for index in completed..names.len() {
        scheduler.evidence.verify_owner().await?;
        let request = &state.required("requests")?.as_array().unwrap()[index];
        let request = IvyJson::object([
            ("name", request.required("name")?.clone()),
            ("mutationId", request.required("mutationId")?.clone()),
        ]);
        store
            .client
            .request("events.unsubscribe", request, RequestOptions::default())
            .await?;
        let previous = canonical(&state, 33_554_432)?;
        state.insert("completed", IvyJson::Number((index + 1) as f64));
        let changed = store
            .journal
            .db
            .execute(
                "UPDATE journal_state SET value=? WHERE key=? AND value=?",
                [&canonical(&state, 33_554_432)?, &key, &previous],
            )
            .map_err(sqlite::error)?;
        identity(changed == 1)?;
    }
    Ok(Some(ImportedSubscription {
        source,
        before,
        after: Some(state),
        completed: true,
    }))
}
