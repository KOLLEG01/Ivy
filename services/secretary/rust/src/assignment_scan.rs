//! Bounded execution pages and durable retry recovery, in the original scan order.
use crate::{
    assignment_advance::AssignmentAdvance,
    assignment_driver::{CollectedPreflight, PreflightWork},
    assignment_runner::{ExecutionPhase, execution_failure_disposition, release_lease},
    inbox_recovery::{RecoveryIssues, RecoveryScan},
    store::Record,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::{clock::parse_date, rpc::RequestOptions};
use std::collections::HashSet;

const RETRY_KEY: &str = "secretary/execution-retry";
#[derive(Default)]
pub struct AssignmentScan {
    pub cursor: Option<JsString>,
    pub delivery_cursor: Option<JsString>,
    pub delete_cursor: Option<JsString>,
    pub delete_before: Option<JsString>,
}
fn group(op: &str, args: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([("op", op.into()), ("args", IvyJson::Array(args))])
}
fn predicate(op: &str, field: &str, value: IvyJson) -> IvyJson {
    IvyJson::object([("op", op.into()), ("field", field.into()), ("value", value)])
}
fn phases(values: &[ExecutionPhase]) -> IvyJson {
    predicate(
        "in",
        "data:/phase",
        IvyJson::Array(values.iter().map(|v| v.json()).collect()),
    )
}
fn cleanup_eligible() -> IvyJson {
    group(
        "or",
        vec![
            IvyJson::object([
                ("op", "isNull".into()),
                ("field", "data:/cleanup/state".into()),
            ]),
            predicate("ne", "data:/cleanup/state", "blocked".into()),
        ],
    )
}
fn present(field: &str) -> IvyJson {
    IvyJson::object([
        ("op", "not".into()),
        (
            "arg",
            IvyJson::object([("op", "isNull".into()), ("field", field.into())]),
        ),
    ])
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn cursor(page: &IvyJson) -> Option<JsString> {
    page.get("nextCursor")
        .and_then(IvyJson::as_js_string)
        .cloned()
}
fn nonempty(value: Option<&JsString>) -> bool {
    value.is_some_and(|v| !v.0.is_empty())
}
fn terminal(value: &IvyJson) -> bool {
    ExecutionPhase::read(value.get("phase")).is_some_and(ExecutionPhase::terminal)
}
impl AssignmentScan {
    /// Returns newly fenced jobs for the owning runtime's preflight driver.
    /// A runtime must serialize calls to this scanner, as with the original tick.
    pub async fn scan(
        &mut self,
        advance: &AssignmentAdvance<'_, '_, '_, '_>,
        preflight: &mut HashSet<JsString>,
        issues: &mut RecoveryScan,
    ) -> IvyResult<Vec<IvyJson>> {
        let work = CollectedPreflight::new(preflight);
        self.scan_with_work(advance, &work, &std::cell::RefCell::new(issues))
            .await?;
        Ok(work.jobs.into_inner())
    }
    pub(crate) async fn scan_with_work(
        &mut self,
        advance: &AssignmentAdvance<'_, '_, '_, '_>,
        preflight: &dyn PreflightWork,
        issues: &dyn RecoveryIssues,
    ) -> IvyResult<()> {
        let execution = advance.execution;
        let evidence = execution.native.evidence;
        let store = evidence.store;
        evidence.verify_owner().await?;
        let limit = evidence.settings.required("recordsPerTick")?.clone();
        let parent = predicate("eq", "object.parentId", IvyJson::String(store.root.clone()));
        let order = IvyJson::Array(vec![IvyJson::object([
            ("field", "object.id".into()),
            ("direction", "asc".into()),
        ])]);
        let request = |filter: IvyJson, selected: Option<&JsString>, sorted: bool| {
            let mut request = IvyJson::object([
                ("contractKey", "secretary/execution".into()),
                ("limit", limit.clone()),
                ("where", group("and", vec![parent.clone(), filter])),
            ]);
            if sorted {
                request.insert("orderBy", order.clone());
            }
            if let Some(selected) = selected.filter(|s| !s.0.is_empty()) {
                request.insert("cursor", IvyJson::String(selected.clone()));
            }
            request
        };
        let page = store
            .client
            .request(
                "objects.query",
                request(
                    group(
                        "and",
                        vec![
                            cleanup_eligible(),
                            group(
                                "or",
                                vec![
                                    phases(&[
                                        ExecutionPhase::Queued,
                                        ExecutionPhase::Preflight,
                                        ExecutionPhase::Starting,
                                        ExecutionPhase::Running,
                                        ExecutionPhase::Archiving,
                                        ExecutionPhase::Deleting,
                                    ]),
                                    group(
                                        "and",
                                        vec![
                                            predicate("eq", "data:/phase", "failed".into()),
                                            predicate(
                                                "eq",
                                                "data:/errorCode",
                                                "native_backend_required".into(),
                                            ),
                                            present("data:/archivedAt"),
                                            present("data:/result"),
                                        ],
                                    ),
                                ],
                            ),
                        ],
                    ),
                    self.cursor.as_ref(),
                    true,
                ),
                RequestOptions::default(),
            )
            .await?;
        self.cursor = cursor(&page);
        let deliveries = store
            .client
            .request(
                "objects.query",
                request(
                    group(
                        "or",
                        vec![
                            predicate("eq", "data:/phase", "completed".into()),
                            group(
                                "and",
                                vec![
                                    phases(&[ExecutionPhase::Archived, ExecutionPhase::Settled]),
                                    predicate(
                                        "eq",
                                        "data:/delivery/state",
                                        "outcome_unknown".into(),
                                    ),
                                    IvyJson::object([
                                        ("op", "not".into()),
                                        (
                                            "arg",
                                            IvyJson::object([
                                                ("op", "isNull".into()),
                                                (
                                                    "field",
                                                    "data:/delivery/evidence/objectId".into(),
                                                ),
                                            ]),
                                        ),
                                    ]),
                                ],
                            ),
                        ],
                    ),
                    self.delivery_cursor.as_ref(),
                    true,
                ),
                RequestOptions::default(),
            )
            .await?;
        self.delivery_cursor = cursor(&deliveries);
        if !nonempty(self.delete_cursor.as_ref()) {
            self.delete_before = Some(execution.now()?);
        }
        let mut due_request = request(IvyJson::Null, self.delete_cursor.as_ref(), true);
        due_request.insert(
            "where",
            group(
                "and",
                vec![
                    parent.clone(),
                    predicate("eq", "data:/phase", "archived".into()),
                    cleanup_eligible(),
                    predicate(
                        "lte",
                        "data:/deleteAfter",
                        IvyJson::String(self.delete_before.clone().ok_or_else(IvyError::internal)?),
                    ),
                ],
            ),
        );
        let due = store
            .client
            .request("objects.query", due_request, RequestOptions::default())
            .await?;
        self.delete_cursor = cursor(&due);
        let running = store
            .client
            .request(
                "objects.query",
                request(
                    phases(&[
                        ExecutionPhase::Starting,
                        ExecutionPhase::Preflight,
                        ExecutionPhase::Running,
                    ]),
                    None,
                    false,
                ),
                RequestOptions::default(),
            )
            .await?;
        let mut active = running
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .len() as f64;
        for row in [&page, &deliveries, &due]
            .into_iter()
            .map(|page| {
                page.required("items")
                    .and_then(|v| v.as_array().ok_or_else(IvyError::internal))
            })
            .collect::<IvyResult<Vec<_>>>()?
            .into_iter()
            .flatten()
        {
            let mut current = None;
            let mut key = format!("execution:{}", row.required("objectId")?.as_str()?);
            let attempt: IvyResult<()> = async {
                current = Some(
                    store
                        .read(
                            Record::Execution,
                            &IvyJson::object([
                                ("objectId", row.required("objectId")?.clone()),
                                ("revision", row.required("revision")?.clone()),
                            ]),
                        )
                        .await?,
                );
                let current = current.as_ref().unwrap();
                let value = current.required("value")?;
                if value.get("phase") == Some(&"failed".into()) {
                    self.repair_cleanup_failure(advance, current).await?;
                    return Ok(());
                }
                let id = text(value, "executionId")?.to_utf8()?;
                key = format!("execution:{id}");
                if value.get("phase") == Some(&"queued".into()) {
                    if active >= limit.as_f64().ok_or_else(IvyError::internal)? {
                        return Ok(());
                    }
                    active += 1.0;
                }
                let retry = store.journal.named(RETRY_KEY, &id, true)?;
                if retry.as_ref().is_some_and(|r| {
                    r.value
                        .get("nextAt")
                        .and_then(IvyJson::as_f64)
                        .is_some_and(|at| at > execution.clock.utc_millis() as f64)
                }) {
                    return Ok(());
                }
                advance
                    .advance_with_work(current, preflight, issues)
                    .await?;
                if let Some(retry) = retry {
                    store
                        .journal
                        .delete_technical(RETRY_KEY, &retry.pin.object_id)?;
                }
                issues.clear(&key);
                Ok(())
            }
            .await;
            if let Err(error) = attempt {
                if evidence.cancellation.cancellation.is_cancelled() {
                    return Err(error);
                }
                let Some(current) = current else {
                    issues.issue(&key, &error.code);
                    continue;
                };
                if let Err(error) = self
                    .recover(advance, &current, &error.code, &key, issues)
                    .await
                {
                    issues.issue(&key, &error.code);
                }
            }
        }
        Ok(())
    }
    async fn repair_cleanup_failure(
        &self,
        advance: &AssignmentAdvance<'_, '_, '_, '_>,
        current: &IvyJson,
    ) -> IvyResult<()> {
        let value = current.required("value")?;
        let pin = current.required("pin")?;
        let revision = pin
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)?;
        if revision <= 1.0 || value.get("errorCode") != Some(&"native_backend_required".into()) {
            return Ok(());
        }
        let previous = advance
            .execution
            .native
            .evidence
            .store
            .read(
                Record::Execution,
                &IvyJson::object([
                    ("objectId", pin.required("objectId")?.clone()),
                    ("revision", (revision - 1.0).into()),
                ]),
            )
            .await?;
        let prior = previous.required("value")?;
        if !crate::assignment_cleanup::failed_delete_matches(prior, value) {
            return Ok(());
        }
        let mut next = crate::assignment_cleanup::CleanupAction::Delete.issue(
            value,
            "native_backend_required",
            &advance.execution.now()?,
            true,
        );
        next.insert("phase", ExecutionPhase::Deleting.json());
        next.insert("errorCode", IvyJson::Null);
        advance.execution.update(current, &next).await?;
        Ok(())
    }
    async fn recover(
        &self,
        advance: &AssignmentAdvance<'_, '_, '_, '_>,
        current: &IvyJson,
        code: &str,
        key: &str,
        issues: &dyn RecoveryIssues,
    ) -> IvyResult<()> {
        let execution = advance.execution;
        let store = execution.native.evidence.store;
        let mut current = store
            .read(
                Record::Execution,
                current.required("pin")?.required("objectId")?,
            )
            .await?;
        let value = current.required("value")?;
        let id = text(value, "executionId")?.to_utf8()?;
        let retry = store.journal.named(RETRY_KEY, &id, true)?;
        let attempts = retry
            .as_ref()
            .filter(|r| r.value.get("code") == Some(&code.into()))
            .and_then(|r| r.value.get("attempts"))
            .and_then(IvyJson::as_f64)
            .unwrap_or(0.0)
            + 1.0;
        let age = execution.clock.utc_millis() as f64 - parse_date(&text(value, "updatedAt")?);
        let disposition = execution_failure_disposition(code, attempts, age);
        let cleanup = crate::assignment_cleanup::CleanupAction::for_execution(value);
        let cleanup_blocked = cleanup.is_some()
            && (disposition == "terminal" || crate::assignment_cleanup::identity_missing(code));
        if let Some(action) = cleanup {
            let next = action.issue(value, code, &execution.now()?, cleanup_blocked);
            current = execution.update(&current, &next).await?;
        }
        let value = current.required("value")?;
        if cleanup_blocked {
            if let Some(retry) = retry {
                store
                    .journal
                    .delete_technical(RETRY_KEY, &retry.pin.object_id)?;
            }
            issues.issue(key, code);
            return Ok(());
        }
        if disposition != "terminal" {
            let backoff = if disposition == "pending" {
                5000.0
            } else {
                (30_000.0 * 2.0_f64.powf(attempts - 1.0)).min(300_000.0)
            };
            let next = IvyJson::object([
                ("code", code.into()),
                ("attempts", IvyJson::Number(attempts)),
                (
                    "nextAt",
                    IvyJson::Number(execution.clock.utc_millis() as f64 + backoff),
                ),
                (
                    "expiresAt",
                    IvyJson::Number(execution.clock.utc_millis() as f64 + 90.0 * 86_400_000.0),
                ),
            ]);
            if let Some(retry) = retry {
                store
                    .journal
                    .amend_technical(RETRY_KEY, &retry.pin, &next)?;
            } else {
                store.journal.create_technical(RETRY_KEY, &id, &next)?;
            }
            if disposition == "pending" {
                issues.clear(key);
            } else {
                issues.issue(key, code);
            }
            return Ok(());
        }
        if let Some(retry) = retry {
            store
                .journal
                .delete_technical(RETRY_KEY, &retry.pin.object_id)?;
        }
        if cleanup.is_some() {
            issues.issue(key, code);
            return Ok(());
        }
        let now = execution.now()?;
        // Delivery happens after the original turn. Its own terminal record
        // retains any operation/evidence; it must not overwrite that turn.
        if value.get("phase") == Some(&"completed".into())
            && value.get("result").is_some_and(|v| !v.is_null())
        {
            let mut next = value.clone();
            let mut delivery = if let Some(delivery) = value.get("delivery") {
                delivery.clone()
            } else {
                IvyJson::object([
                    ("executionId", value.required("executionId")?.clone()),
                    (
                        "resultHash",
                        ivy_contracts::hash::hash_json(value.required("result")?)?.into(),
                    ),
                    ("operationId", IvyJson::Null),
                    ("request", IvyJson::Null),
                    ("evidence", IvyJson::Null),
                    ("expiresAt", IvyJson::Null),
                ])
            };
            let uncertain = delivery.get("operationId").is_some_and(|v| !v.is_null());
            delivery.insert(
                "state",
                if uncertain {
                    "outcome_unknown"
                } else {
                    "failed"
                }
                .into(),
            );
            delivery.insert("reason", code.into());
            delivery.insert("terminal", true.into());
            next.insert("delivery", delivery);
            next.insert("updatedAt", IvyJson::String(now));
            execution.update(&current, &next).await?;
            issues.issue(key, code);
            return Ok(());
        }
        let reused = value
            .get("assignmentSnapshot")
            .and_then(|v| v.get("execution"))
            .and_then(|v| v.get("reuse"))
            .is_some_and(|v| !v.is_null() && v != &"new".into());
        let can_archive = value
            .get("threadId")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|s| !s.0.is_empty())
            && !reused
            && !["archiving", "archived", "deleting", "deleted"]
                .iter()
                .any(|p| value.get("phase") == Some(&(*p).into()))
            && !matches!(
                code,
                "native_owner_mismatch" | "native_evidence_mismatch" | "tool_definition_changed"
            );
        if !terminal(value) {
            let mut next = value.clone();
            next.insert(
                "phase",
                if can_archive { "archiving" } else { "failed" }.into(),
            );
            next.insert("errorCode", code.into());
            next.insert(
                "completedAt",
                value
                    .get("completedAt")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(IvyJson::String(now.clone())),
            );
            next.insert("updatedAt", IvyJson::String(now));
            current = execution.update(&current, &next).await?;
        }
        release_lease(store.journal, &current)?;
        issues.issue(key, code);
        Ok(())
    }
}
