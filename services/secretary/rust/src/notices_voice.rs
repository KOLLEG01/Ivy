//! Reconcile the Voice admission attached to a confirmed inbox notice.
use crate::{
    assignment_voice::{ForwardedVoiceOutcome, forwarded_voice_outcome, voice_prompt},
    notices::{SecretaryNotices, changed, field, is, need, text},
};
use ivy_contracts::{IvyJson, IvyResult};
use ivy_sdk::{
    clock::automatic_notifications_allowed,
    rpc::RequestOptions,
    tools::{InterfaceRequirement, ToolCallOptions, call_bound},
};

impl SecretaryNotices<'_, '_, '_> {
    pub(crate) async fn voice(&self, item: &IvyJson, notice: &IvyJson) -> IvyResult<()> {
        let decision = field(item.required("value")?, "decision");
        let escalation = field(field(decision, "policy"), "voiceEscalation");
        let voice = field(notice, "voice");
        if (voice).is_null() || is(voice, "state", "confirmed") {
            return Ok(());
        }
        need(
            is(field(decision, "assessment"), "notification", "voice")
                && is(field(decision, "assessment"), "urgency", "critical")
                && !(escalation).is_null(),
            "secretary_voice_mismatch",
            "Voice escalation requires the original critical decision and configured target.",
        )?;
        let result = self.reconcile_voice(item, notice, voice, escalation).await;
        if let Err(error) = result {
            if matches!(
                error.code.as_str(),
                "revision_conflict" | "mutation_conflict"
            ) {
                return Err(error);
            }
            self.update(
                item,
                &changed(
                    notice,
                    [
                        (
                            "voice",
                            changed(
                                voice,
                                [
                                    ("state", "outcome_unknown".into()),
                                    ("errorCode", error.code.into()),
                                ],
                            ),
                        ),
                        ("updatedAt", IvyJson::String(self.engine.now()?)),
                    ],
                ),
            )
            .await?;
        }
        Ok(())
    }
    async fn reconcile_voice(
        &self,
        item: &IvyJson,
        notice: &IvyJson,
        voice: &IvyJson,
        escalation: &IvyJson,
    ) -> IvyResult<()> {
        let evidence = self.engine.evidence;
        let client = evidence.store.client.as_ref();
        evidence.verify_owner().await?;
        let node = client
            .request(
                "serviceNodes.get",
                IvyJson::object([(
                    "serviceNodeId",
                    escalation.required("serviceNodeId")?.clone(),
                )]),
                RequestOptions::default(),
            )
            .await?;
        need(
            is(&node, "serviceName", "phone-bridge")
                && ["connected", "synced", "ready"]
                    .iter()
                    .all(|key| field(&node, key) == &IvyJson::Bool(true)),
            "secretary_voice_unavailable",
            "The configured Phone owner is not ready.",
        )?;
        let tools = self.engine.tools.service_tools(
            text(escalation, "serviceNodeId")?,
            &[InterfaceRequirement {
                namespace: "phone".into(),
                interface_version: "1.0.0".into(),
            }],
        )?;
        // The original checks the request binding even when it only reads an
        // already admitted call. Keep that check before all operation lookups.
        let request = tools.binding(&"phone.request".into(), None).await?;
        need(
            request.value().get("definitionHash") == escalation.get("requestDefinitionHash"),
            "tool_definition_changed",
            "The configured Phone request definition changed.",
        )?;
        let mut call_id = field(voice, "callId").clone();
        let prompt = IvyJson::String(voice_prompt(&text(notice, "text")?));
        if (&call_id).is_null() {
            if !automatic_notifications_allowed(
                client,
                self.engine.clock.utc_millis(),
                RequestOptions::default(),
            )
            .await?
            {
                return Ok(());
            }
            let result = call_bound(
                client,
                &request,
                IvyJson::object([
                    ("operationId", voice.required("operationId")?.clone()),
                    ("recipientId", escalation.required("recipientId")?.clone()),
                    ("route", "voice".into()),
                    ("voicePrompt", prompt.clone()),
                ]),
                Some(text(voice, "operationId")?),
                ToolCallOptions::default(),
            )
            .await?;
            call_id = result
                .get("callId")
                .filter(|v| v.as_js_string().is_some())
                .cloned()
                .unwrap_or(IvyJson::Null);
            need(
                !(&call_id).is_null(),
                "secretary_voice_unresolved",
                "PhoneBridge returned no original call identity.",
            )?;
            if result.get("operationId") != voice.get("operationId") {
                let operation = tools.binding(&"phone.operation".into(), None).await?;
                let forwarded = call_bound(
                    client,
                    &operation,
                    IvyJson::object([
                        ("callId", call_id.clone()),
                        ("operationId", voice.required("operationId")?.clone()),
                    ]),
                    None,
                    ToolCallOptions::default(),
                )
                .await?;
                need(
                    !(&forwarded).is_null(),
                    "secretary_voice_unresolved",
                    "PhoneBridge returned no forwarded request outcome.",
                )?;
                forwarded_voice_outcome(
                    &forwarded,
                    &call_id,
                    voice.required("operationId")?,
                    &prompt,
                )?;
            } else {
                need(
                    result.get("recipientId") == escalation.get("recipientId")
                        && result.get("principalId")
                            == evidence.settings.required("identity")?.get("principalId"),
                    "secretary_voice_unresolved",
                    "Phone admission changed the original call.",
                )?;
            }
            return self
                .update(
                    item,
                    &changed(
                        notice,
                        [
                            (
                                "voice",
                                changed(
                                    voice,
                                    [
                                        ("state", "queued".into()),
                                        ("callId", call_id),
                                        ("errorCode", IvyJson::Null),
                                    ],
                                ),
                            ),
                            ("updatedAt", IvyJson::String(self.engine.now()?)),
                        ],
                    ),
                )
                .await;
        }
        let operation = tools.binding(&"phone.operation".into(), None).await?;
        let forwarded = call_bound(
            client,
            &operation,
            IvyJson::object([
                ("callId", call_id.clone()),
                ("operationId", voice.required("operationId")?.clone()),
            ]),
            None,
            ToolCallOptions::default(),
        )
        .await?;
        if !(&forwarded).is_null() {
            if forwarded_voice_outcome(
                &forwarded,
                &call_id,
                voice.required("operationId")?,
                &prompt,
            )? == ForwardedVoiceOutcome::Pending
            {
                return Ok(());
            }
        } else {
            let dial = call_bound(
                client,
                &operation,
                IvyJson::object([("callId", call_id.clone()), ("method", "call.dial".into())]),
                None,
                ToolCallOptions::default(),
            )
            .await?;
            if (&dial).is_null() || is(&dial, "phase", "submitted") {
                return Ok(());
            }
            let receipt = field(&dial, "receipt");
            need(
                !is(&dial, "phase", "outcome_unknown")
                    && field(receipt, "ok") == &IvyJson::Bool(true)
                    && is(field(receipt, "result"), "state", "connected"),
                "secretary_voice_not_connected",
                "The original Phone call did not connect.",
            )?;
            let prompted = call_bound(
                client,
                &operation,
                IvyJson::object([
                    ("callId", call_id.clone()),
                    ("method", "call.promptVoice".into()),
                    ("generation", IvyJson::Number(0.0)),
                ]),
                None,
                ToolCallOptions::default(),
            )
            .await?;
            if (&prompted).is_null() || is(&prompted, "phase", "submitted") {
                return Ok(());
            }
            let receipt = field(&prompted, "receipt");
            need(
                !is(&prompted, "phase", "outcome_unknown")
                    && field(receipt, "ok") == &IvyJson::Bool(true)
                    && is(field(receipt, "result"), "state", "sent"),
                "secretary_voice_prompt_unresolved",
                "The connected Voice prompt was not confirmed.",
            )?;
        }
        self.update(
            item,
            &changed(
                notice,
                [
                    (
                        "voice",
                        changed(
                            voice,
                            [
                                ("state", "confirmed".into()),
                                ("callId", call_id),
                                ("errorCode", IvyJson::Null),
                            ],
                        ),
                    ),
                    ("updatedAt", IvyJson::String(self.engine.now()?)),
                ],
            ),
        )
        .await
    }
}
