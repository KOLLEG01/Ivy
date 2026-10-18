//! Assignment retirement and durable, revision-aware schedule catch-up.
use crate::assignment_error::{SchedulerFailure, SchedulerResult, untyped};
use crate::{
    assignment_scheduler::AssignmentScheduler,
    assignment_schema::{assignment_name, validate_assignment},
    inbox_recovery::{RecoveryIssues, RecoveryScan},
    schedule::{self, LocalMinute, Payload, Schedule},
    store::Record,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json, json::try_js_string};
use ivy_sdk::{
    clock::{TimeZone, date_to_iso, parse_date},
    rpc::RequestOptions,
};
use std::{cell::RefCell, collections::HashMap};

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
fn time(value: &IvyJson, key: &str) -> SchedulerResult<f64> {
    Ok(parse_date(&text(value, key)?))
}
fn iso(value: f64) -> SchedulerResult<JsString> {
    // A failed Date.toISOString is an ordinary JavaScript exception.
    date_to_iso(value).map_err(SchedulerFailure::uncoded)
}
fn written_at(document: &IvyJson, fallback: &str) -> SchedulerResult<JsString> {
    match document.get("writtenAt").filter(|v| !v.is_null()) {
        Some(value) => value
            .as_js_string()
            .cloned()
            .ok_or(SchedulerFailure::Uncoded),
        None => text(document.required("value")?, fallback),
    }
}
fn max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}
#[derive(Default)]
pub struct SchedulerAssignments {
    pub cursor: Option<JsString>,
    pub retired: HashMap<JsString, JsString>,
}
impl SchedulerAssignments {
    /// The owning runtime serializes scans and retains this state between ticks.
    pub async fn assignments(
        &mut self,
        scheduler: &AssignmentScheduler<'_, '_, '_>,
        issues: &mut RecoveryScan,
    ) -> IvyResult<Vec<IvyJson>> {
        self.assignments_with_issues(scheduler, &RefCell::new(issues))
            .await
    }
    pub(crate) async fn assignments_with_issues(
        &mut self,
        scheduler: &AssignmentScheduler<'_, '_, '_>,
        issues: &dyn RecoveryIssues,
    ) -> IvyResult<Vec<IvyJson>> {
        let evidence = scheduler.evidence;
        let store = evidence.store;
        let mut request = IvyJson::object([
            ("contractKey", "secretary/assignment".into()),
            (
                "limit",
                evidence.settings.required("recordsPerTick")?.clone(),
            ),
            ("includeArchived", true.into()),
            (
                "select",
                IvyJson::Array(
                    [
                        "object.effectivelyArchived",
                        "object.archivedAt",
                        "object.name",
                        "data:/assignmentId",
                    ]
                    .iter()
                    .map(|s| (*s).into())
                    .collect(),
                ),
            ),
            (
                "orderBy",
                IvyJson::Array(vec![IvyJson::object([
                    ("field", "object.id".into()),
                    ("direction", "asc".into()),
                ])]),
            ),
            (
                "where",
                IvyJson::object([
                    ("op", "eq".into()),
                    ("field", "object.parentId".into()),
                    ("value", IvyJson::String(store.root.clone())),
                ]),
            ),
        ]);
        if let Some(cursor) = self.cursor.as_ref().filter(|c| !c.0.is_empty()) {
            request.insert("cursor", IvyJson::String(cursor.clone()));
        }
        let page = store
            .client
            .request("objects.query", request, RequestOptions::default())
            .await?;
        self.cursor = page
            .get("nextCursor")
            .and_then(IvyJson::as_js_string)
            .cloned();
        let mut result = Vec::new();
        for row in page
            .required("items")?
            .as_array()
            .ok_or(SchedulerFailure::Uncoded)?
        {
            let object_id = text(row, "objectId")?;
            let issue_key = format!("assignment-record:{}", object_id.to_utf8()?);
            let outcome: SchedulerResult<Option<IvyJson>> = async {
                let values = row.required("values")?;
                let archived =
                    values.get("object.effectivelyArchived") == Some(&IvyJson::Bool(true));
                let mut retirement = coerce_string(row.required("revision")?)?;
                retirement.0.push(58);
                retirement.0.extend(
                    values
                        .get("object.archivedAt")
                        .map(coerce_string)
                        .transpose()?
                        .unwrap_or_else(|| "undefined".into())
                        .0,
                );
                if archived && self.retired.get(&object_id) == Some(&retirement) {
                    return Ok(None);
                }
                let assignment = if archived {
                    let key = values
                        .get("data:/assignmentId")
                        .and_then(IvyJson::as_js_string);
                    need(
                        key.is_some()
                            && values.get("object.name")
                                == Some(&assignment_name(key.unwrap())?.into()),
                        "secretary_assignment_identity_conflict",
                        "The archived assignment must retain its original identity.",
                    )?;
                    let key = key.unwrap();
                    let name = format!(
                        "secretary-assignment-{}",
                        &hash_json(&IvyJson::String(key.clone()))?[7..48]
                    );
                    let legacy = store.journal.named(
                        "secretary/event-subscription",
                        &String::from_utf16_lossy(&key.0),
                        true,
                    )?;
                    let mut names = vec![IvyJson::from(name)];
                    if let Some(legacy) = &legacy {
                        let selected = legacy.value.required("name")?;
                        if !names.contains(selected) {
                            names.push(selected.clone());
                        }
                    }
                    for selected in names {
                        let mutation_id = scheduler
                            .operation(vec![
                                "retire-assignment-subscription".into(),
                                IvyJson::String(object_id.clone()),
                                IvyJson::String(retirement.clone()),
                                selected.clone(),
                            ])
                            .await?;
                        store
                            .client
                            .request(
                                "events.unsubscribe",
                                IvyJson::object([
                                    ("name", selected),
                                    ("mutationId", IvyJson::String(mutation_id)),
                                ]),
                                RequestOptions::default(),
                            )
                            .await?;
                    }
                    if let Some(legacy) = legacy {
                        store.journal.delete_technical(
                            "secretary/event-subscription",
                            &legacy.pin.object_id,
                        )?;
                    }
                    self.retired.insert(object_id.clone(), retirement);
                    let assignment_issue =
                        format!("assignment:{}", String::from_utf16_lossy(&key.0));
                    issues.clear(&assignment_issue);
                    None
                } else {
                    self.retired.remove(&object_id);
                    let selected = IvyJson::object([
                        ("objectId", IvyJson::String(object_id.clone())),
                        ("revision", row.required("revision")?.clone()),
                    ]);
                    let assignment = store.read(Record::Assignment, &selected).await?;
                    validate_assignment(assignment.required("value")?)?;
                    Some(assignment)
                };
                issues.clear(&issue_key);
                Ok(assignment)
            }
            .await;
            match outcome {
                Ok(Some(assignment)) => result.push(assignment),
                Ok(None) => (),
                Err(error) if evidence.cancellation.cancellation.is_cancelled() => {
                    return Err(error.into());
                }
                Err(error) => issues.issue(&issue_key, &error.code("secretary_assignment_invalid")),
            }
        }
        Ok(result)
    }
}

impl AssignmentScheduler<'_, '_, '_> {
    pub(crate) fn occurrences_inner(
        &self,
        assignment: &IvyJson,
        now: i64,
        checked_at: &JsString,
    ) -> SchedulerResult<IvyJson> {
        let since = parse_date(checked_at);
        need(
            since.is_finite(),
            "secretary_schedule_progress_invalid",
            "The retained schedule cursor has an invalid time.",
        )?;
        let trigger = assignment.required("trigger")?;
        let schedule = trigger.get("kind") == Some(&"schedule".into());
        if schedule && since >= (now as f64).min(since + 7.0 * 24.0 * 60.0 * 60_000.0) {
            return Ok(IvyJson::object([
                ("items", IvyJson::Array(vec![])),
                ("checkedAt", IvyJson::String(checked_at.clone())),
            ]));
        }
        let cadence = trigger
            .get("cadence")
            .and_then(|v| v.as_str().ok())
            .unwrap_or_default();
        let zone = trigger
            .get("timeZone")
            .and_then(|v| v.as_str().ok())
            .unwrap_or_default();
        let local_time = trigger
            .get("localTime")
            .and_then(|v| v.as_str().ok())
            .unwrap_or_default();
        let weekdays = trigger
            .get("weekdays")
            .and_then(IvyJson::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(IvyJson::as_f64)
                    .map(|n| n as u8)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let selected = if !schedule {
            Schedule::Other
        } else if cadence == "interval" {
            Schedule::Interval {
                minutes: trigger
                    .required("intervalMinutes")?
                    .as_f64()
                    .ok_or(SchedulerFailure::Uncoded)? as u32,
            }
        } else {
            Schedule::Civil {
                weekly: cadence != "daily",
                zone: &zone,
                time: &local_time,
                weekdays: &weekdays,
            }
        };
        let page = schedule::occurrences(
            selected,
            now,
            since as i64,
            self.evidence
                .settings
                .required("recordsPerTick")?
                .as_f64()
                .ok_or(SchedulerFailure::Uncoded)? as u32,
            |at, zone| {
                let p = untyped(TimeZone::local_parts(at, zone))?;
                Ok::<_, SchedulerFailure>(LocalMinute {
                    date: format!("{:04}-{:02}-{:02}", p.year, p.month, p.day),
                    time: format!("{:02}:{:02}", p.hour, p.minute),
                    weekday: p.weekday as u8,
                })
            },
        )?;
        let items = page
            .items
            .into_iter()
            .map(|item| {
                let payload = match item.payload {
                    Payload::Interval { slot } => IvyJson::object([
                        ("cadence", "interval".into()),
                        ("slot", IvyJson::Number(slot as f64)),
                    ]),
                    Payload::Civil {
                        weekly,
                        date,
                        time,
                        zone,
                    } => IvyJson::object([
                        (
                            "cadence",
                            if weekly {
                                "weekly".into()
                            } else {
                                "daily".into()
                            },
                        ),
                        ("localDate", date.into()),
                        ("localTime", time.into()),
                        ("timeZone", zone.into()),
                    ]),
                };
                Ok(IvyJson::object([
                    ("kind", "schedule".into()),
                    ("key", item.key.into()),
                    ("occurredAt", IvyJson::String(iso(item.occurred_at as f64)?)),
                    ("payload", payload),
                ]))
            })
            .collect::<SchedulerResult<Vec<_>>>()?;
        Ok(IvyJson::object([
            ("items", IvyJson::Array(items)),
            ("checkedAt", IvyJson::String(iso(page.checked_at as f64)?)),
        ]))
    }
    pub(crate) async fn schedules_inner(
        &self,
        assignment: &IvyJson,
        configuration: &IvyJson,
        now: i64,
    ) -> SchedulerResult<()> {
        let value = assignment.required("value")?;
        let assignment_id = text(value, "assignmentId")?;
        let pin = assignment.required("pin")?;
        let name = JsString::from(format!(
            "schedule-{}",
            &hash_json(&IvyJson::String(assignment_id.clone()))?[7..]
        ));
        let store = self.evidence.store;
        let mut saved = if let Some(saved) = store.named(Record::ScheduleProgress, &name).await? {
            saved
        } else {
            let legacy = store.journal.named(
                "secretary/schedule-cursor",
                &String::from_utf16_lossy(&assignment_id.0),
                true,
            )?;
            let current = legacy
                .as_ref()
                .is_some_and(|l| l.value.get("updatedAt") == value.get("updatedAt"));
            let original = if current {
                assignment.clone()
            } else {
                store.original(Record::Assignment, assignment).await?
            };
            let checked_at = if (current
                || pin.get("revision").and_then(IvyJson::as_f64) == Some(1.0))
                && legacy.is_some()
            {
                legacy
                    .as_ref()
                    .unwrap()
                    .value
                    .required("checkedAt")?
                    .clone()
            } else {
                IvyJson::String(written_at(&original, "createdAt")?)
            };
            let progress = IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("assignmentId", IvyJson::String(assignment_id.clone())),
                ("assignment", original.required("pin")?.clone()),
                ("assignmentSnapshot", original.required("value")?.clone()),
                ("checkedAt", checked_at),
                ("updatedAt", IvyJson::String(iso(now as f64)?)),
            ]);
            let operation = self
                .operation(vec![
                    "schedule-progress".into(),
                    IvyJson::String(assignment_id.clone()),
                ])
                .await?;
            let saved = store
                .create(Record::ScheduleProgress, &name, &progress, &operation)
                .await?;
            if let Some(legacy) = legacy {
                store
                    .journal
                    .delete_technical("secretary/schedule-cursor", &legacy.pin.object_id)?;
            }
            saved
        };
        let progress = saved.required("value")?;
        need(
            progress.get("assignmentId") == Some(&IvyJson::String(assignment_id))
                && progress.required("assignment")?.get("objectId") == pin.get("objectId"),
            "secretary_schedule_progress_mismatch",
            "The durable schedule cursor belongs to another assignment.",
        )?;
        for _ in 0..4 {
            let progress = saved.required("value")?;
            let retained = progress
                .required("assignment")?
                .required("revision")?
                .as_f64()
                .ok_or(SchedulerFailure::Uncoded)?;
            let latest = pin
                .required("revision")?
                .as_f64()
                .ok_or(SchedulerFailure::Uncoded)?;
            if retained > latest {
                return Ok(());
            }
            let next = if retained < latest {
                Some(
                    store
                        .read(
                            Record::Assignment,
                            &IvyJson::object([
                                ("objectId", pin.required("objectId")?.clone()),
                                ("revision", IvyJson::Number(retained + 1.0)),
                            ]),
                        )
                        .await?,
                )
            } else {
                None
            };
            let transition_at = next
                .as_ref()
                .map(|d| written_at(d, "updatedAt").map(|s| parse_date(&s)))
                .transpose()?
                .unwrap_or(f64::INFINITY);
            let can_switch = next.is_some() && transition_at <= now as f64;
            let through = if can_switch {
                transition_at as i64
            } else {
                now
            };
            let result = if progress.required("assignmentSnapshot")?.get("enabled")
                == Some(&IvyJson::Bool(true))
            {
                self.occurrences_inner(
                    progress.required("assignmentSnapshot")?,
                    through,
                    &text(progress, "checkedAt")?,
                )?
            } else {
                IvyJson::object([
                    ("items", IvyJson::Array(vec![])),
                    (
                        "checkedAt",
                        IvyJson::String(iso(max(time(progress, "checkedAt")?, through as f64))?),
                    ),
                ])
            };
            let original = IvyJson::object([
                ("pin", progress.required("assignment")?.clone()),
                ("value", progress.required("assignmentSnapshot")?.clone()),
            ]);
            let items = result
                .required("items")?
                .as_array()
                .ok_or(SchedulerFailure::Uncoded)?;
            for occurrence in items {
                self.enqueue(&original, occurrence, configuration).await?;
            }
            let switched = can_switch && time(&result, "checkedAt")? >= transition_at;
            let mut next_value = progress.clone();
            if switched {
                let next = next.as_ref().unwrap();
                next_value.insert("assignment", next.required("pin")?.clone());
                next_value.insert("assignmentSnapshot", next.required("value")?.clone());
                next_value.insert(
                    "checkedAt",
                    IvyJson::String(iso(max(time(&result, "checkedAt")?, transition_at))?),
                );
            } else {
                next_value.insert("checkedAt", result.required("checkedAt")?.clone());
            }
            next_value.insert("updatedAt", IvyJson::String(iso(now as f64)?));
            let progressed = time(&next_value, "checkedAt")? - time(progress, "checkedAt")?;
            if !switched && items.is_empty() && progressed < 60.0 * 60_000.0 {
                return Ok(());
            }
            saved = store
                .amend(&saved, Record::ScheduleProgress, &next_value)
                .await?;
            if !switched && time(&result, "checkedAt")? >= through as f64 {
                return Ok(());
            }
        }
        Ok(())
    }
}

fn coerce_string(value: &IvyJson) -> SchedulerResult<JsString> {
    untyped(try_js_string(value))
}

impl AssignmentScheduler<'_, '_, '_> {
    pub fn occurrences(
        &self,
        assignment: &IvyJson,
        now: i64,
        checked_at: &JsString,
    ) -> IvyResult<IvyJson> {
        self.occurrences_inner(assignment, now, checked_at)
            .map_err(IvyError::from)
    }
    pub async fn schedules(
        &self,
        assignment: &IvyJson,
        configuration: &IvyJson,
        now: i64,
    ) -> IvyResult<()> {
        self.schedules_inner(assignment, configuration, now)
            .await
            .map_err(IvyError::from)
    }
}
