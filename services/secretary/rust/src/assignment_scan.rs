//! Bounded execution pages and durable retry recovery, in the original scan order.
use crate::{
    assignment_advance::AssignmentAdvance,
    assignment_runner::{execution_failure_disposition, release_lease},
    inbox_recovery::RecoveryScan,
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
fn phases(values: &[&str]) -> IvyJson {
    predicate(
        "in",
        "data:/phase",
        IvyJson::Array(values.iter().map(|v| (*v).into()).collect()),
    )
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
    ["settled", "deleted", "failed"]
        .iter()
        .any(|p| value.get("phase") == Some(&(*p).into()))
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
                    phases(&[
                        "queued",
                        "preflight",
                        "starting",
                        "running",
                        "archiving",
                        "deleting",
                    ]),
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
                                    phases(&["archived", "settled"]),
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
                request(phases(&["starting", "preflight", "running"]), None, false),
                RequestOptions::default(),
            )
            .await?;
        let mut active = running
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .len() as f64;
        let mut jobs = Vec::new();
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
                if let Some(job) = advance.advance(current, preflight, issues).await? {
                    jobs.push(job);
                }
                if let Some(retry) = retry {
                    store
                        .journal
                        .delete_technical(RETRY_KEY, &retry.pin.object_id)?;
                }
                issues.issues.retain(|(id, _)| id != &key);
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
        Ok(jobs)
    }
    async fn recover(
        &self,
        advance: &AssignmentAdvance<'_, '_, '_, '_>,
        current: &IvyJson,
        code: &str,
        key: &str,
        issues: &mut RecoveryScan,
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
                issues.issues.retain(|(id, _)| id != key);
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
        let now = execution.now()?;
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
