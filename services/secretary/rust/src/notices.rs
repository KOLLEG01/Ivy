//! Materialized inbox notice dispatch and recovery of the original Main receipt.
use crate::{
    browser_notice::secretary_browser_notice,
    engine::SecretaryEngine,
    inbox_evidence::{ignored_reason, truthy},
    notice_evidence::{NoticeMethod, NoticeProvider, notice_evidence},
    policy::assessment_suppression,
    store::{Record, mutation},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, hash::hash_json};
use ivy_sdk::{
    clock::{
        ContactWindow, automatic_notifications_allowed, contact_window_open, date_to_iso,
        parse_date,
    },
    main_chat::main_notification,
    operation_ids::new_operation_id,
    rpc::RequestOptions,
};

const RETRY_KEY: &str = "secretary/notice-retry";
pub(crate) fn need(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
pub(crate) fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    static NULL: IvyJson = IvyJson::Null;
    value.get(key).unwrap_or(&NULL)
}
pub(crate) fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
pub(crate) fn is(value: &IvyJson, key: &str, expected: &str) -> bool {
    field(value, key) == &IvyJson::from(expected)
}
pub(crate) fn changed(
    value: &IvyJson,
    updates: impl IntoIterator<Item = (&'static str, IvyJson)>,
) -> IvyJson {
    let mut next = value.clone();
    for (key, value) in updates {
        next.insert(key, value);
    }
    next
}
fn quiet(policy: &IvyJson, urgency: &IvyJson, now: i64) -> IvyResult<bool> {
    let silent = field(policy, "silentTime");
    if !truthy(silent)
        || truthy(field(policy, "urgentBypass")) && urgency == &IvyJson::from("critical")
    {
        return Ok(false);
    }
    contact_window_open(
        Some(&ContactWindow {
            time_zone: silent.required("timeZone")?.as_str()?,
            start: silent.required("start")?.as_str()?,
            end: silent.required("end")?.as_str()?,
        }),
        now,
    )
}
fn reference_text(notice: &IvyJson, pin: &IvyJson) -> IvyResult<IvyJson> {
    let mut value = text(notice, "text")?;
    value.0.extend("\n\nSecretary reference: ".encode_utf16());
    value.0.extend(text(pin, "objectId")?.0);
    Ok(IvyJson::String(value))
}
pub struct SecretaryNotices<'a, 'engine, 'journal> {
    pub engine: &'a SecretaryEngine<'a, 'engine, 'journal>,
    minimum_message_age_minutes: f64,
}
impl<'a, 'engine, 'journal> SecretaryNotices<'a, 'engine, 'journal> {
    pub fn new(
        engine: &'a SecretaryEngine<'a, 'engine, 'journal>,
        minimum_message_age_minutes: f64,
    ) -> IvyResult<Self> {
        need(
            minimum_message_age_minutes.fract() == 0.0
                && (0.0..=1440.0).contains(&minimum_message_age_minutes),
            "invalid_arguments",
            "Secretary notification delay must be a whole number of minutes between zero and one day.",
        )?;
        Ok(Self {
            engine,
            minimum_message_age_minutes,
        })
    }
    pub(crate) async fn update(&self, item: &IvyJson, notice: &IvyJson) -> IvyResult<()> {
        if same(field(item.required("value")?, "notice"), notice)? {
            return Ok(());
        }
        self.engine.evidence.verify_owner().await?;
        let operation_id = field(field(item.required("value")?, "decision"), "operationId");
        need(
            truthy(operation_id),
            "secretary_notice_mismatch",
            "A notice update needs its assessment operation identity.",
        )?;
        let pin = item.required("pin")?;
        self.engine
            .evidence
            .store
            .write(
                Record::Item,
                &changed(item.required("value")?, [("notice", notice.clone())]),
                &IvyJson::object([("object", pin.clone())]),
                &mutation(&[operation_id.clone(), pin.clone(), notice.clone()])?,
            )
            .await?;
        Ok(())
    }
    fn provider(&self) -> NoticeProvider<'_, '_, '_> {
        NoticeProvider {
            evidence: self.engine.evidence,
            tools: self.engine.tools,
        }
    }
    fn issue(&self, id: &JsString, code: &str) {
        self.engine
            .issues
            .borrow_mut()
            .issue(&format!("notice:{}", String::from_utf16_lossy(&id.0)), code);
    }
    fn clear_issue(&self, id: &JsString) {
        let key = format!("notice:{}", String::from_utf16_lossy(&id.0));
        self.engine
            .issues
            .borrow_mut()
            .issues
            .retain(|(id, _)| id != &key);
    }
    pub async fn step(&self, id: &JsString) -> IvyResult<()> {
        let engine = self.engine;
        let evidence = engine.evidence;
        let store = evidence.store;
        evidence.verify_owner().await?;
        let item = evidence.item(&IvyJson::String(id.clone())).await?;
        let value = item.required("value")?;
        let decision = field(value, "decision");
        let notice = field(value, "notice");
        if !truthy(decision) || !truthy(notice) {
            return Ok(());
        }
        let pin = item.required("pin")?;
        need(
            field(notice.required("source")?, "objectId") == field(pin, "objectId")
                && !text(decision, "actor")?.0.is_empty()
                && !text(decision, "operationId")?.0.is_empty(),
            "secretary_notice_mismatch",
            "Dispatch requires the materialized assessment outcome.",
        )?;
        // The original reads the retained source before checking its policy.
        evidence.source(value.required("sourceId")?).await?;
        let assessment = decision.required("assessment")?;
        if let Some(suppressed) = assessment_suppression(assessment, decision.required("policy")?)?
        {
            need(
                is(notice, "state", "suppressed")
                    && is(notice, "reason", suppressed)
                    && !truthy(field(notice, "request"))
                    && !truthy(field(notice, "evidence")),
                "secretary_notice_mismatch",
                "A suppressed assessment cannot gain a dispatch.",
            )?;
            return Ok(());
        }
        need(
            !is(notice, "state", "suppressed"),
            "secretary_notice_mismatch",
            "A required notice cannot be silently suppressed.",
        )?;
        let now = engine.clock.utc_millis();
        let at = IvyJson::String(date_to_iso(now as f64).map_err(|_| IvyError::internal())?);
        let is_quiet =
            !automatic_notifications_allowed(store.client.as_ref(), now, RequestOptions::default())
                .await?
                || quiet(
                    evidence.settings.required("policy")?,
                    assessment.required("urgency")?,
                    now,
                )?;
        let too_young = !is(assessment, "notification", "voice")
            && (now as f64)
                < parse_date(&text(value.required("message")?, "occurredAt")?)
                    + self.minimum_message_age_minutes * 60_000.0;
        let current_source = evidence
            .settings
            .required("sources")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|source| source.get("sourceId") == value.get("sourceId"));
        let authorized = match current_source {
            Some(source) => ignored_reason(source, value.required("message")?)?.is_none(),
            None => false,
        };
        if authorized && !is_quiet && !too_young && !is(notice, "state", "confirmed") {
            secretary_browser_notice(
                store,
                evidence.service_node_id,
                &hash_json(&IvyJson::Array(vec![
                    IvyJson::String(id.clone()),
                    decision.required("operationId")?.clone(),
                ]))?
                .into(),
                &text(assessment, "summary")?,
                engine.clock,
            )
            .await?;
        }
        let idempotency = mutation(&[
            IvyJson::String(id.clone()),
            decision.required("actor")?.clone(),
            decision.required("operationId")?.clone(),
            "notice".into(),
        ])?;
        let retry = store
            .journal
            .named(RETRY_KEY, &String::from_utf16_lossy(&id.0), true)?;
        if is(notice, "state", "outcome_unknown")
            && !is(notice, "reason", "display_outcome_unknown")
        {
            return Ok(());
        }
        let request = field(notice, "request");
        if truthy(request)
            && retry
                .as_ref()
                .is_some_and(|r| r.value.get("failedOperationId") == request.get("operationId"))
        {
            let retry_value = &retry.as_ref().unwrap().value;
            if field(retry_value, "nextAt").as_f64().unwrap_or(f64::NAN) > now as f64 {
                return Ok(());
            }
            if field(retry_value, "attempts").as_f64().unwrap_or(f64::NAN) >= 5.0 {
                self.update(
                    &item,
                    &changed(
                        notice,
                        [
                            ("state", "outcome_unknown".into()),
                            ("reason", "chat_operation_failed".into()),
                            ("updatedAt", at),
                        ],
                    ),
                )
                .await?;
                self.issue(id, "chat_operation_failed");
                return Ok(());
            }
            let source = IvyJson::object([
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
            ]);
            let request = changed(
                request,
                [
                    (
                        "operationId",
                        IvyJson::String(new_operation_id(store.client.clone(), None).await?),
                    ),
                    ("source", source.clone()),
                ],
            );
            return self
                .update(
                    &item,
                    &changed(
                        notice,
                        [
                            ("source", source.clone()),
                            ("request", request),
                            ("preparedRevision", source.required("revision")?.clone()),
                            ("state", "dispatching".into()),
                            ("reason", IvyJson::Null),
                            ("updatedAt", at),
                        ],
                    ),
                )
                .await;
        }
        if !truthy(request) {
            need(
                is(notice, "state", "queued")
                    && !truthy(field(notice, "target"))
                    && field(notice, "preparedRevision").is_null()
                    && !truthy(field(notice, "evidence")),
                "secretary_notice_mismatch",
                "Unprepared notices must remain queued.",
            )?;
            let target = engine.notice_target().await?;
            let reason = if !authorized {
                Some("authorization_changed")
            } else if too_young {
                Some("minimum_message_age")
            } else if is_quiet {
                Some("silent_time")
            } else if !truthy(&target) {
                Some("channel_not_configured")
            } else {
                None
            };
            if let Some(reason) = reason {
                if !is(notice, "reason", reason) {
                    self.update(
                        &item,
                        &changed(notice, [("reason", reason.into()), ("updatedAt", at)]),
                    )
                    .await?;
                }
                return Ok(());
            }
            let source = IvyJson::object([
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
            ]);
            let request = IvyJson::object([
                ("action", "notify".into()),
                ("operationId", IvyJson::String(idempotency)),
                ("expectedBridge", target.required("expectedBridge")?.clone()),
                ("channel", target.required("channel")?.clone()),
                ("source", source.clone()),
                ("text", reference_text(notice, pin)?),
            ]);
            return self
                .update(
                    &item,
                    &changed(
                        notice,
                        [
                            ("source", source.clone()),
                            ("target", target),
                            ("request", request),
                            ("preparedRevision", source.required("revision")?.clone()),
                            ("state", "dispatching".into()),
                            ("reason", IvyJson::Null),
                            ("updatedAt", at),
                        ],
                    ),
                )
                .await;
        }
        let target = field(notice, "target");
        need(
            truthy(target) && !field(notice, "preparedRevision").is_null(),
            "secretary_notice_mismatch",
            "A dispatched notice must retain its prepared target revision.",
        )?;
        need(
            is(request, "action", "notify")
                && same(field(request, "source"), notice.required("source")?)?
                && [
                    notice.required("text")?.clone(),
                    reference_text(notice, pin)?,
                ]
                .contains(request.required("text")?)
                && same(
                    field(request, "expectedBridge"),
                    target.required("expectedBridge")?,
                )?
                && same(field(request, "channel"), target.required("channel")?)?
                && target.required("expectedBridge")?.get("callerPrincipalId")
                    == evidence.settings.required("identity")?.get("principalId"),
            "secretary_notice_mismatch",
            "Recovery cannot change the original prepared recipient or notice arguments.",
        )?;
        if is(notice, "state", "confirmed") {
            self.clear_issue(id);
            return self.voice(&item, notice).await;
        }
        let outcome = self
            .reconcile(
                &item,
                notice,
                authorized,
                is_quiet,
                retry.as_ref(),
                now,
                &at,
                id,
            )
            .await;
        if let Err(error) = outcome {
            if matches!(
                error.code.as_str(),
                "revision_conflict" | "mutation_conflict"
            ) {
                return Err(error);
            }
            if matches!(
                error.code.as_str(),
                "secretary_notice_mismatch"
                    | "secretary_evidence_conflict"
                    | "tool_definition_changed"
            ) {
                self.update(
                    &item,
                    &changed(
                        notice,
                        [
                            ("state", "outcome_unknown".into()),
                            ("reason", error.code.clone().into()),
                            ("updatedAt", at),
                        ],
                    ),
                )
                .await?;
                self.issue(id, &error.code);
                return Ok(());
            }
            if error.outcome == Outcome::NotExecuted {
                return self
                    .retry(&item, notice, retry.as_ref(), now, &at, id, &error.code)
                    .await;
            }
            if field(notice, "reason") != &IvyJson::from(error.code.clone()) {
                self.update(
                    &item,
                    &changed(
                        notice,
                        [
                            ("state", "dispatching".into()),
                            ("reason", error.code.into()),
                            ("updatedAt", at),
                        ],
                    ),
                )
                .await?;
            }
        }
        Ok(())
    }
    async fn retry(
        &self,
        item: &IvyJson,
        notice: &IvyJson,
        retry: Option<&crate::journal::Document>,
        now: i64,
        at: &IvyJson,
        id: &JsString,
        code: &str,
    ) -> IvyResult<()> {
        let attempts = retry
            .map(|r| field(&r.value, "attempts").as_f64().unwrap_or(f64::NAN))
            .unwrap_or(0.0)
            + 1.0;
        let next = IvyJson::object([
            (
                "failedOperationId",
                notice.required("request")?.required("operationId")?.clone(),
            ),
            ("attempts", IvyJson::Number(attempts)),
            (
                "nextAt",
                IvyJson::Number(
                    now as f64 + (30_000.0 * 2_f64.powf(attempts - 1.0)).min(300_000.0),
                ),
            ),
            (
                "expiresAt",
                if attempts >= 5.0 {
                    IvyJson::Number(now as f64 + 8.0 * 86_400_000.0)
                } else {
                    IvyJson::Null
                },
            ),
        ]);
        let journal = self.engine.evidence.store.journal;
        if let Some(retry) = retry {
            journal.amend_technical(RETRY_KEY, &retry.pin, &next)?;
        } else {
            journal.create_technical(RETRY_KEY, &String::from_utf16_lossy(&id.0), &next)?;
        }
        self.update(
            item,
            &changed(
                notice,
                [
                    (
                        "state",
                        if attempts >= 5.0 {
                            "outcome_unknown".into()
                        } else {
                            "dispatching".into()
                        },
                    ),
                    ("reason", code.into()),
                    ("updatedAt", at.clone()),
                ],
            ),
        )
        .await?;
        if attempts >= 5.0 {
            self.issue(id, code);
        }
        Ok(())
    }
    async fn reconcile(
        &self,
        item: &IvyJson,
        notice: &IvyJson,
        authorized: bool,
        quiet: bool,
        retry: Option<&crate::journal::Document>,
        now: i64,
        at: &IvyJson,
        id: &JsString,
    ) -> IvyResult<()> {
        let request = notice.required("request")?;
        let target = notice.required("target")?;
        let principal = self
            .engine
            .evidence
            .settings
            .required("identity")?
            .required("principalId")?;
        let prior: IvyResult<Option<IvyJson>> = async {
            let operation = self
                .provider()
                .call(
                    NoticeMethod::Operation,
                    target,
                    IvyJson::object([
                        (
                            "expectedBridge",
                            request.required("expectedBridge")?.clone(),
                        ),
                        ("operationId", request.required("operationId")?.clone()),
                    ]),
                    None,
                )
                .await?;
            let original = operation.required("request")?;
            need(
                is(original, "action", "notify")
                    && operation.get("callerPrincipalId") == Some(principal)
                    && same(&main_notification(original), &main_notification(request))?,
                "secretary_notice_mismatch",
                "The original Main handoff identity changed.",
            )?;
            if !same(original, request)? {
                self.adopt(item, notice, original, at).await?;
                return Ok(None);
            }
            Ok(Some(
                self.provider()
                    .call(
                        NoticeMethod::Notice,
                        target,
                        IvyJson::object([(
                            "operationId",
                            request.required("operationId")?.clone(),
                        )]),
                        None,
                    )
                    .await?,
            ))
        }
        .await;
        let reply = match prior {
            Ok(Some(reply)) => reply,
            Ok(None) => return Ok(()),
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "not_found" | "chat_publication_pending"
                ) =>
            {
                let reason = if !authorized {
                    Some("authorization_changed")
                } else if quiet {
                    Some("silent_time")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    if !is(notice, "reason", reason) {
                        self.update(
                            item,
                            &changed(
                                notice,
                                [("reason", reason.into()), ("updatedAt", at.clone())],
                            ),
                        )
                        .await?;
                    }
                    return Ok(());
                }
                let result = self
                    .provider()
                    .call(
                        NoticeMethod::Notify,
                        target,
                        main_notification(request),
                        Some(text(request, "operationId")?),
                    )
                    .await?;
                let original = result.required("request")?;
                need(
                    result.get("callerPrincipalId") == Some(principal)
                        && is(original, "action", "notify")
                        && same(&main_notification(original), &main_notification(request))?,
                    "secretary_notice_unresolved",
                    "The original notification action changed identity.",
                )?;
                if !same(original, request)? && is(original, "action", "notify") {
                    return self.adopt(item, notice, original, at).await;
                }
                if is(&result, "phase", "succeeded")
                    && is(field(&result, "outcome"), "action", "notify")
                {
                    field(&result, "outcome").required("reply")?.clone()
                } else if is(&result, "phase", "failed")
                    && is(field(&result, "error"), "outcome", "not_executed")
                {
                    return self
                        .retry(
                            item,
                            notice,
                            retry,
                            now,
                            at,
                            id,
                            &field(&result, "error").required("code")?.as_str()?,
                        )
                        .await;
                } else if is(&result, "phase", "failed") {
                    let code = field(field(&result, "error"), "code")
                        .as_str()
                        .unwrap_or_else(|_| "chat_operation_failed".into());
                    self.update(
                        item,
                        &changed(
                            notice,
                            [
                                ("state", "outcome_unknown".into()),
                                ("reason", code.clone().into()),
                                ("updatedAt", at.clone()),
                            ],
                        ),
                    )
                    .await?;
                    self.issue(id, &code);
                    return Ok(());
                } else {
                    need(
                        is(&result, "phase", "accepted") || is(&result, "phase", "applying"),
                        "secretary_notice_unresolved",
                        "The original notification action cannot be completed by Main.",
                    )?;
                    if !is(notice, "state", "dispatching") || !is(notice, "reason", "awaiting_main")
                    {
                        self.update(
                            item,
                            &changed(
                                notice,
                                [
                                    ("state", "dispatching".into()),
                                    ("reason", "awaiting_main".into()),
                                    ("updatedAt", at.clone()),
                                ],
                            ),
                        )
                        .await?;
                    }
                    return Ok(());
                }
            }
            Err(error) => return Err(error),
        };
        need(
            truthy(&reply),
            "secretary_notice_unresolved",
            "The original notification action has no Main reply.",
        )?;
        notice_evidence(
            self.engine.evidence.store.client.as_ref(),
            principal,
            &reply,
            notice,
        )
        .await?;
        let state = if is(reply.required("data")?, "state", "confirmed") {
            "confirmed"
        } else if is(reply.required("data")?, "state", "outcome_unknown") {
            "outcome_unknown"
        } else {
            "dispatching"
        };
        let reason = match state {
            "confirmed" => IvyJson::Null,
            "outcome_unknown" => "display_outcome_unknown".into(),
            _ => "awaiting_display".into(),
        };
        if !is(notice, "state", state)
            || field(notice, "reason") != &reason
            || !same(field(notice, "evidence"), reply.required("object")?)?
        {
            self.update(
                item,
                &changed(
                    notice,
                    [
                        ("state", state.into()),
                        ("reason", reason),
                        ("evidence", reply.required("object")?.clone()),
                        ("updatedAt", at.clone()),
                    ],
                ),
            )
            .await?;
        }
        if state == "outcome_unknown" {
            self.issue(id, "secretary_notice_outcome_unknown");
        }
        if state == "confirmed" {
            self.clear_issue(id);
            if let Some(retry) = retry {
                self.engine
                    .evidence
                    .store
                    .journal
                    .delete_technical(RETRY_KEY, &retry.pin.object_id)?;
            }
        }
        Ok(())
    }
    async fn adopt(
        &self,
        item: &IvyJson,
        notice: &IvyJson,
        request: &IvyJson,
        at: &IvyJson,
    ) -> IvyResult<()> {
        self.update(
            item,
            &changed(
                notice,
                [
                    ("request", request.clone()),
                    (
                        "target",
                        changed(
                            notice.required("target")?,
                            [
                                (
                                    "expectedBridge",
                                    request.required("expectedBridge")?.clone(),
                                ),
                                ("channel", request.required("channel")?.clone()),
                            ],
                        ),
                    ),
                    ("updatedAt", at.clone()),
                ],
            ),
        )
        .await
    }
}
