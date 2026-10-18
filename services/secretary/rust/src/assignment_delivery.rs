//! Retain original Main admission and reconcile assignment delivery receipts.
use crate::{
    assignment_decision::assignment_decision,
    assignment_schema::{assignment_name, effective_rules},
    assignment_voice_engine::{AssignmentVoice, VoiceResult},
    browser_notice::secretary_browser_notice,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    store::Record,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::{
    clock::{
        Clock, ContactWindow, automatic_notifications_allowed, contact_window_open, parse_date,
    },
    main_chat::{CHAT_INTERFACE_VERSION, main_chat, main_notification},
    operation_ids::new_operation_id,
    rpc::RequestOptions,
    tools::{BoundTool, InterfaceRequirement, ToolCallOptions, ToolClients, call_bound},
};

fn need(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn field<'a>(v: &'a IvyJson, key: &str) -> &'a IvyJson {
    v.get(key).unwrap_or(&IvyJson::Null)
}
fn is(v: &IvyJson, key: &str, s: &str) -> bool {
    v.get(key) == Some(&s.into())
}
fn yes(v: &IvyJson, key: &str) -> bool {
    v.get(key) == Some(&IvyJson::Bool(true))
}
fn number(v: &IvyJson, key: &str, default: f64) -> f64 {
    v.get(key).and_then(IvyJson::as_f64).unwrap_or(default)
}
fn updated(v: &IvyJson, changes: impl IntoIterator<Item = (&'static str, IvyJson)>) -> IvyJson {
    let mut next = v.clone();
    for (k, v) in changes {
        next.insert(k, v);
    }
    next
}
fn open(rule: &IvyJson, now: i64) -> IvyResult<bool> {
    let rule = field(rule, "window");
    if rule.is_null() {
        return Ok(true);
    }
    contact_window_open(
        Some(&ContactWindow {
            time_zone: rule.required("timeZone")?.as_str()?,
            start: rule.required("start")?.as_str()?,
            end: rule.required("end")?.as_str()?,
        }),
        now,
    )
}
fn rank(v: &IvyJson) -> usize {
    ["low", "normal", "high", "critical"]
        .iter()
        .position(|s| v == &IvyJson::from(*s))
        .unwrap_or(0)
}

pub struct AssignmentDelivery<'a, 'engine, 'journal> {
    pub evidence: &'a InboxEvidence<'engine, 'journal>,
    pub tools: &'a ToolClients,
    pub clock: &'a dyn Clock,
    pub minimum_notification_age_minutes: f64,
}
impl AssignmentDelivery<'_, '_, '_> {
    fn voice(&self) -> AssignmentVoice<'_, '_, '_> {
        AssignmentVoice {
            evidence: self.evidence,
            tools: self.tools,
            clock: self.clock,
        }
    }
    async fn settled(&self, execution: &IvyJson, decision: &IvyJson) -> IvyResult<bool> {
        if is(decision, "notification", "voice") {
            self.voice().complete_fallback(execution).await?;
        }
        Ok(true)
    }
    async fn save(&self, latest: &mut IvyJson, handoff: IvyJson) -> IvyResult<IvyJson> {
        self.evidence.verify_owner().await?;
        let next = updated(latest.required("value")?, [("delivery", handoff.clone())]);
        *latest = self
            .evidence
            .store
            .amend(latest, Record::Execution, &next)
            .await?;
        Ok(handoff)
    }
    pub async fn bound(
        &self,
        name: &str,
        target: &IvyJson,
    ) -> IvyResult<std::sync::Arc<BoundTool>> {
        need(
            !target.is_null(),
            "secretary_assignment_channel_unconfigured",
            "Secretary has no Main notification target.",
        )?;
        let id = target
            .required("serviceNodeId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let node = self
            .evidence
            .store
            .client
            .request(
                "serviceNodes.get",
                IvyJson::object([("serviceNodeId", IvyJson::String(id.clone()))]),
                RequestOptions::default(),
            )
            .await?;
        need(
            is(&node, "serviceName", "chat-bridge")
                && ["connected", "synced", "ready"]
                    .iter()
                    .all(|k| yes(&node, k)),
            "secretary_assignment_channel_unavailable",
            "The selected ChatBridge owner is unavailable.",
        )?;
        self.tools
            .service_tools(
                id.clone(),
                &[InterfaceRequirement {
                    namespace: "chat".into(),
                    interface_version: CHAT_INTERFACE_VERSION.into(),
                }],
            )?
            .binding(&format!("chat.{name}").into(), None)
            .await
    }
    async fn call(
        &self,
        name: &str,
        target: &IvyJson,
        args: IvyJson,
        id: Option<JsString>,
    ) -> IvyResult<IvyJson> {
        let binding = self.bound(name, target).await?;
        call_bound(
            self.evidence.store.client.as_ref(),
            &binding,
            args,
            id,
            ToolCallOptions::default(),
        )
        .await
    }
    pub async fn step(&self, execution: &IvyJson, issues: &mut RecoveryScan) -> IvyResult<bool> {
        self.evidence.verify_owner().await?;
        let value = execution.required("value")?;
        need(
            !value.required("result")?.is_null(),
            "secretary_assignment_result_invalid",
            "An assignment needs its retained result.",
        )?;
        let decision = assignment_decision(
            value
                .required("result")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
        )?;
        let store = self.evidence.store;
        let client = store.client.as_ref();
        let assignment = store
            .named(
                Record::Assignment,
                &assignment_name(
                    value
                        .required("assignmentSnapshot")?
                        .required("assignmentId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?,
                )?
                .into(),
            )
            .await?;
        let configuration = store
            .named(Record::Configuration, &"configuration".into())
            .await?;
        need(
            assignment.is_some() && configuration.is_some(),
            "secretary_assignment_delivery_unavailable",
            "The current assignment and contact rules must be available.",
        )?;
        let assignment = assignment.unwrap();
        let assignment = assignment.required("value")?;
        let configuration = configuration.unwrap();
        let rules = effective_rules(
            configuration.required("value")?.required("rules")?,
            assignment.required("rules")?,
        )?;
        let contact_allowed = automatic_notifications_allowed(
            client,
            self.clock.utc_millis(),
            RequestOptions::default(),
        )
        .await?;
        let mut urgent_voice_fallback = false;
        if is(&decision, "notification", "voice") {
            let eligible = yes(assignment, "enabled")
                && yes(
                    value.required("effectiveRules")?.required("voice")?,
                    "enabled",
                )
                && yes(rules.required("voice")?, "enabled")
                && is(&decision, "urgency", "critical")
                && contact_allowed
                && open(rules.required("voice")?, self.clock.utc_millis())?;
            match self.voice().step(execution, &decision, eligible).await? {
                VoiceResult::Confirmed => return Ok(true),
                VoiceResult::Pending => return Ok(false),
                VoiceResult::Fallback => {}
            }
            need(
                !eligible || yes(rules.required("main")?, "enabled"),
                "secretary_voice_fallback_unavailable",
                "A failed Voice call has no enabled Main fallback.",
            )?;
            urgent_voice_fallback = eligible;
        }
        let expires_at = IvyJson::Number(self.clock.utc_millis() as f64 + 8.0 * 86_400_000.0);
        let mut latest = store
            .read(
                Record::Execution,
                execution.required("pin")?.required("objectId")?,
            )
            .await?;
        let legacy = store.journal.named(
            "secretary/assignment-handoff",
            &value.required("executionId")?.as_str()?,
            true,
        )?;
        let mut saved = if !field(latest.required("value")?, "delivery").is_null() {
            latest.required("value")?.required("delivery")?.clone()
        } else if let Some(legacy) = legacy {
            self.save(&mut latest, legacy.value).await?
        } else {
            self.save(
                &mut latest,
                IvyJson::object([
                    ("executionId", value.required("executionId")?.clone()),
                    ("resultHash", hash_json(value.required("result")?)?.into()),
                    ("operationId", IvyJson::Null),
                    ("request", IvyJson::Null),
                    ("state", "queued".into()),
                    ("reason", IvyJson::Null),
                    ("evidence", IvyJson::Null),
                    ("expiresAt", IvyJson::Null),
                ]),
            )
            .await?
        };
        need(
            saved.get("executionId") == value.get("executionId")
                && saved.get("resultHash") == Some(&hash_json(value.required("result")?)?.into()),
            "secretary_assignment_result_changed",
            "The retained decision changed after delivery began.",
        )?;
        let issue_id = format!("delivery:{}", value.required("executionId")?.as_str()?);
        if is(&saved, "state", "confirmed") || is(&saved, "state", "suppressed") {
            issues.issues.retain(|(id, _)| id != &issue_id);
            return self.settled(execution, &decision).await;
        }
        let reconcile_only = is(&saved, "state", "outcome_unknown")
            && (yes(&saved, "terminal")
                || ["archived", "settled"]
                    .iter()
                    .any(|s| is(latest.get("value").unwrap(), "phase", s)));
        if is(&saved, "state", "failed") || reconcile_only && field(&saved, "evidence").is_null() {
            issues.issue(
                &issue_id,
                &field(&saved, "reason")
                    .as_str()
                    .unwrap_or_else(|_| "secretary_assignment_notice_outcome_unknown".into()),
            );
            return self.settled(execution, &decision).await;
        }
        let now = self.clock.utc_millis();
        if number(&saved, "retryAt", 0.0) > now as f64 {
            return Ok(false);
        }
        let rule = rules.required("main")?;
        let suppressed = if is(&decision, "notification", "none") {
            Some("notification_none")
        } else if !yes(assignment, "enabled") {
            Some("assignment_disabled")
        } else if !yes(rule, "enabled") {
            Some("main_disabled")
        } else if rank(decision.required("urgency")?) < rank(rule.required("minimumUrgency")?) {
            Some("below_minimum_urgency")
        } else {
            None
        };
        let age = number(
            assignment,
            "minimumNotificationAgeMinutes",
            self.minimum_notification_age_minutes,
        );
        let occurred = parse_date(
            value
                .required("trigger")?
                .required("occurredAt")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
        );
        let deferred = if !urgent_voice_fallback && (now as f64) < occurred + age * 60_000.0 {
            Some("minimum_message_age")
        } else if !contact_allowed || !open(rule, now)? {
            Some("outside_contact_window")
        } else {
            None
        };
        if !reconcile_only && suppressed.is_none() && deferred.is_none() {
            secretary_browser_notice(
                store,
                self.evidence.service_node_id,
                &hash_json(&IvyJson::Array(vec![
                    value.required("executionId")?.clone(),
                    value.required("result")?.clone(),
                ]))?
                .into(),
                decision
                    .required("text")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
                self.clock,
            )
            .await?;
        }
        if field(&saved, "request").is_null() {
            if let Some(reason) = suppressed {
                let reason = if is(&decision, "notification", "none") {
                    decision.required("reason")?.clone()
                } else {
                    reason.into()
                };
                self.save(
                    &mut latest,
                    updated(
                        &saved,
                        [
                            ("state", "suppressed".into()),
                            ("reason", reason),
                            ("expiresAt", expires_at),
                        ],
                    ),
                )
                .await?;
                return self.settled(execution, &decision).await;
            }
            if let Some(reason) = deferred {
                if !is(&saved, "reason", reason) {
                    self.save(&mut latest, updated(&saved, [("reason", reason.into())]))
                        .await?;
                }
                return Ok(false);
            }
            let target = main_chat(client).await?;
            let operation_id = new_operation_id(store.client.clone(), None).await?;
            need(
                target.expected_bridge.get("callerPrincipalId")
                    == self
                        .evidence
                        .settings
                        .required("identity")?
                        .get("principalId"),
                "secretary_assignment_channel_unavailable",
                "Main must identify Secretary as the actual caller.",
            )?;
            let mut text = decision
                .required("text")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?;
            text.0.extend(
                JsString::from(format!(
                    "\n\nSecretary reference: {}",
                    execution.required("pin")?.required("objectId")?.as_str()?
                ))
                .0,
            );
            let request = IvyJson::object([
                ("action", "notify".into()),
                ("operationId", IvyJson::String(operation_id.clone())),
                ("expectedBridge", target.expected_bridge),
                ("channel", target.channel.clone()),
                ("source", execution.required("pin")?.clone()),
                ("text", IvyJson::String(text)),
            ]);
            saved = self
                .save(
                    &mut latest,
                    updated(
                        &saved,
                        [
                            ("operationId", IvyJson::String(operation_id)),
                            (
                                "target",
                                IvyJson::object([
                                    ("serviceNodeId", IvyJson::String(target.service_node_id)),
                                    ("channel", target.channel),
                                ]),
                            ),
                            ("request", request),
                            ("state", "dispatching".into()),
                            ("reason", IvyJson::Null),
                        ],
                    ),
                )
                .await?;
        }
        let request = saved.required("request")?;
        let target = if !field(&saved, "target").is_null() {
            saved.required("target")?.clone()
        } else {
            let main = main_chat(client).await?;
            need(
                ["principalId", "workspaceId", "callerPrincipalId"]
                    .iter()
                    .all(|key| {
                        main.expected_bridge.get(key)
                            == request.get("expectedBridge").and_then(|r| r.get(key))
                    }),
                "secretary_assignment_notice_mismatch",
                "The retained handoff belongs to another Main or caller.",
            )?;
            IvyJson::object([
                ("serviceNodeId", IvyJson::String(main.service_node_id)),
                ("channel", request.required("channel")?.clone()),
            ])
        };
        need(
            saved.get("operationId") == request.get("operationId")
                && request.required("source")?.get("objectId")
                    == execution.required("pin")?.get("objectId")
                && same(target.required("channel")?, request.required("channel")?)?,
            "secretary_assignment_notice_mismatch",
            "The original handoff source, target or operation changed.",
        )?;
        let result = self
            .deliver(
                execution,
                &decision,
                &mut latest,
                &saved,
                &target,
                reconcile_only,
                suppressed.or(deferred),
                now,
                &expires_at,
                &issue_id,
                issues,
            )
            .await;
        if let Err(error) = &result {
            if !is(&saved, "reason", &error.code) {
                self.save(
                    &mut latest,
                    updated(&saved, [("reason", error.code.clone().into())]),
                )
                .await?;
            }
        }
        result
    }
    async fn deliver(
        &self,
        execution: &IvyJson,
        decision: &IvyJson,
        latest: &mut IvyJson,
        saved: &IvyJson,
        target: &IvyJson,
        reconcile_only: bool,
        reason: Option<&str>,
        now: i64,
        expires_at: &IvyJson,
        issue_id: &str,
        issues: &mut RecoveryScan,
    ) -> IvyResult<bool> {
        let request = saved.required("request")?;
        let observed = async {
            let operation = self
                .call(
                    "operation",
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
            self.check_operation(
                &operation,
                request,
                "The original Main handoff identity changed.",
            )?;
            if !same(operation.required("request")?, request)? {
                self.save_transport(latest, saved, target, operation.required("request")?)
                    .await?;
                return Ok(None);
            }
            self.call(
                "notice",
                target,
                IvyJson::object([("operationId", request.required("operationId")?.clone())]),
                None,
            )
            .await
            .map(Some)
        }
        .await;
        let reply = match observed {
            Ok(Some(reply)) => reply,
            Ok(None) => return Ok(false),
            Err(error) => {
                if !matches!(
                    error.code.as_str(),
                    "not_found" | "chat_publication_pending"
                ) {
                    return Err(error);
                }
                if reconcile_only {
                    return self.settled(execution, decision).await;
                }
                if let Some(reason) = reason {
                    if !is(saved, "reason", reason) {
                        self.save(latest, updated(saved, [("reason", reason.into())]))
                            .await?;
                    }
                    return Ok(false);
                }
                let result = self
                    .call(
                        "notify",
                        target,
                        main_notification(request),
                        request.required("operationId")?.as_js_string().cloned(),
                    )
                    .await?;
                self.check_operation(
                    &result,
                    request,
                    "ChatBridge changed the original notification request.",
                )?;
                if !same(result.required("request")?, request)? {
                    self.save_transport(latest, saved, target, result.required("request")?)
                        .await?;
                    return Ok(false);
                }
                if is(&result, "phase", "succeeded")
                    && is(field(&result, "outcome"), "action", "notify")
                {
                    field(field(&result, "outcome"), "reply").clone()
                } else if is(&result, "phase", "failed")
                    && is(field(&result, "error"), "outcome", "not_executed")
                {
                    let attempts = number(saved, "attempts", 0.0) + 1.0;
                    let terminal = attempts >= 5.0;
                    let code = result.required("error")?.required("code")?.clone();
                    self.save(
                        latest,
                        updated(
                            saved,
                            [
                                (
                                    "operationId",
                                    if terminal {
                                        request.required("operationId")?.clone()
                                    } else {
                                        IvyJson::Null
                                    },
                                ),
                                (
                                    "request",
                                    if terminal {
                                        request.clone()
                                    } else {
                                        IvyJson::Null
                                    },
                                ),
                                ("state", if terminal { "failed" } else { "queued" }.into()),
                                ("reason", code.clone()),
                                ("attempts", IvyJson::Number(attempts)),
                                (
                                    "retryAt",
                                    IvyJson::Number(if terminal {
                                        0.0
                                    } else {
                                        now as f64
                                            + (30_000.0 * 2f64.powf(attempts - 1.0)).min(300_000.0)
                                    }),
                                ),
                                (
                                    "expiresAt",
                                    if terminal {
                                        expires_at.clone()
                                    } else {
                                        IvyJson::Null
                                    },
                                ),
                            ],
                        ),
                    )
                    .await?;
                    if terminal {
                        issues.issue(issue_id, &code.as_str()?);
                        return self.settled(execution, decision).await;
                    }
                    return Ok(false);
                } else if is(&result, "phase", "failed") {
                    let code = field(field(&result, "error"), "code");
                    let code = if code.is_null() {
                        "secretary_assignment_notice_unresolved".into()
                    } else {
                        code.clone()
                    };
                    self.save(
                        latest,
                        updated(
                            saved,
                            [
                                ("state", "outcome_unknown".into()),
                                ("reason", code.clone()),
                                ("expiresAt", expires_at.clone()),
                                ("terminal", IvyJson::Bool(true)),
                            ],
                        ),
                    )
                    .await?;
                    issues.issue(issue_id, &code.as_str()?);
                    return self.settled(execution, decision).await;
                } else {
                    need(
                        is(&result, "phase", "accepted") || is(&result, "phase", "applying"),
                        "secretary_assignment_notice_unresolved",
                        "Main did not accept the handoff.",
                    )?;
                    return Ok(false);
                }
            }
        };
        assignment_reply_evidence(
            self.evidence.store.client.as_ref(),
            self.evidence
                .settings
                .required("identity")?
                .required("principalId")?,
            request,
            &reply,
        )
        .await?;
        if is(reply.required("data")?, "state", "outcome_unknown") {
            self.save(
                latest,
                updated(
                    saved,
                    [
                        ("state", "outcome_unknown".into()),
                        (
                            "reason",
                            "secretary_assignment_notice_outcome_unknown".into(),
                        ),
                        ("evidence", reply.required("object")?.clone()),
                        ("expiresAt", expires_at.clone()),
                        ("terminal", IvyJson::Bool(true)),
                        ("retryAt", IvyJson::Number(now as f64 + 30_000.0)),
                    ],
                ),
            )
            .await?;
            issues.issue(issue_id, "secretary_assignment_notice_outcome_unknown");
            return self.settled(execution, decision).await;
        }
        if !is(reply.required("data")?, "state", "confirmed") {
            return Ok(false);
        }
        self.save(
            latest,
            updated(
                saved,
                [
                    ("state", "confirmed".into()),
                    ("reason", IvyJson::Null),
                    ("evidence", reply.required("object")?.clone()),
                    ("expiresAt", expires_at.clone()),
                ],
            ),
        )
        .await?;
        issues.issues.retain(|(id, _)| id != issue_id);
        self.settled(execution, decision).await
    }
    fn check_operation(
        &self,
        operation: &IvyJson,
        request: &IvyJson,
        message: &str,
    ) -> IvyResult<()> {
        let original = operation.required("request")?;
        need(
            is(original, "action", "notify")
                && operation.get("callerPrincipalId")
                    == self
                        .evidence
                        .settings
                        .required("identity")?
                        .get("principalId")
                && same(&main_notification(original), &main_notification(request))?,
            "secretary_assignment_notice_mismatch",
            message,
        )
    }
    async fn save_transport(
        &self,
        latest: &mut IvyJson,
        saved: &IvyJson,
        target: &IvyJson,
        request: &IvyJson,
    ) -> IvyResult<()> {
        self.save(
            latest,
            updated(
                saved,
                [
                    ("request", request.clone()),
                    (
                        "target",
                        IvyJson::object([
                            ("serviceNodeId", target.required("serviceNodeId")?.clone()),
                            ("channel", request.required("channel")?.clone()),
                        ]),
                    ),
                ],
            ),
        )
        .await?;
        Ok(())
    }
}
pub async fn assignment_reply_evidence(
    client: &dyn ivy_sdk::rpc::RpcClient,
    principal: &IvyJson,
    request: &IvyJson,
    reply: &IvyJson,
) -> IvyResult<()> {
    let mismatch = |ok, message| need(ok, "secretary_assignment_notice_mismatch", message);
    let data = field(reply, "data");
    mismatch(
        !reply.is_null()
            && same(field(data, "channel"), field(request, "channel"))?
            && is(field(data, "origin"), "kind", "native"),
        "Main did not retain the original reply.",
    )?;
    let read = client
        .request(
            "objects.read",
            reply.required("object")?.clone(),
            RequestOptions::default(),
        )
        .await?;
    let supported = |read: &IvyJson, key: &str| {
        is(field(read, "object"), "contractKey", key)
            && !yes(field(read, "object"), "effectivelyArchived")
            && ["1.2.0", "1.3.0"]
                .iter()
                .any(|v| is(field(read, "revision"), "contractVersion", v))
            && is(field(read, "content"), "encoding", "json")
    };
    mismatch(
        supported(&read, "chat-bridge/reply")
            && same(field(field(&read, "content"), "value"), data)?
            && field(field(&read, "revision"), "contentHash") == &IvyJson::from(hash_json(data)?),
        "The Main reply evidence is invalid.",
    )?;
    let origin = data.required("origin")?;
    let input = client
        .request(
            "objects.read",
            IvyJson::object([("objectId", origin.required("inputId")?.clone())]),
            RequestOptions::default(),
        )
        .await?;
    let handoff = field(field(&input, "content"), "value");
    mismatch(
        supported(&input, "chat-bridge/input")
            && !handoff.is_null()
            && field(field(&input, "revision"), "contentHash")
                == &IvyJson::from(hash_json(handoff)?)
            && handoff.get("operationId") == request.get("operationId")
            && field(handoff, "identity").get("senderPrincipalId") == Some(principal)
            && same(
                field(field(handoff, "identity"), "channel"),
                request.required("channel")?,
            )?
            && same(field(handoff, "binding"), origin.required("binding")?)?
            && same(field(handoff, "result"), origin.required("result")?)?,
        "The Main reply belongs to a different handoff.",
    )
}
