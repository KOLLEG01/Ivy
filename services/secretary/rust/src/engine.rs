//! Engine context for one admitted generation. Validate settings before opening
//! the journal; the generation owns this context and its independent workers.
use crate::{
    assignment_schema::assignment_name,
    inbox_evidence::{InboxEvidence, source_identity, source_name, truthy},
    inbox_recovery::{InboxRecovery, RecoveryScan},
    schema::{Schema, validate},
    store::{Record, mutation},
    voice_target::discover_voice_target,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::{
    clock::{Clock, TimeZone, date_to_iso},
    main_chat::{MainChatFailure, main_chat_checked},
    rpc::{RequestOptions, query_document},
    tools::ToolClients,
};
use std::{cell::RefCell, collections::HashSet};

fn need(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn nonempty(value: Option<&IvyJson>) -> bool {
    value
        .and_then(IvyJson::as_js_string)
        .is_some_and(|s| !s.0.is_empty())
}
pub fn validate_settings(settings: &IvyJson) -> IvyResult<()> {
    validate(Schema::Settings, settings)?;
    let policy = settings.required("policy")?;
    let research = policy
        .required("researchMaxMinutes")?
        .as_f64()
        .ok_or_else(IvyError::internal)?;
    need(
        research.fract() == 0.0 && (0.0..=5.0).contains(&research),
        "invalid_arguments",
        "Read-only research must be bounded to at most five minutes.",
    )?;
    if let Some(silent) = policy.get("silentTime").filter(|v| !v.is_null()) {
        need(
            silent.get("start") != silent.get("end"),
            "invalid_arguments",
            "Silent time must have distinct start and end.",
        )?;
        need(
            string(silent, "timeZone")?
                .to_utf8()
                .is_ok_and(|zone| TimeZone::local_parts(0, &zone).is_ok()),
            "invalid_arguments",
            "Silent time needs a supported IANA timezone.",
        )?;
    }
    let mut ids = HashSet::new();
    for source in settings
        .required("sources")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        need(
            ids.insert(string(source, "sourceId")?),
            "invalid_arguments",
            "Each Secretary source ID must be unique.",
        )?;
    }
    Ok(())
}
pub struct SecretaryEngine<'a, 'engine, 'journal> {
    pub evidence: &'a InboxEvidence<'engine, 'journal>,
    pub clock: &'a dyn Clock,
    /// Original Date.now operation admission/retention clock, distinct from the
    /// engine's injectable observation clock.
    pub operation_clock: &'a dyn Clock,
    pub runtime_epoch: &'a JsString,
    /// Generation composition supplies real UUIDs; deterministic reference tests
    /// supply the same UUID stream as the original crypto boundary.
    pub new_uuid: fn() -> JsString,
    pub tools: &'a ToolClients,
    pub issues: &'a RefCell<RecoveryScan>,
}
pub fn random_uuid() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
impl SecretaryEngine<'_, '_, '_> {
    pub(crate) fn now(&self) -> IvyResult<JsString> {
        date_to_iso(self.clock.utc_millis() as f64).map_err(|_| IvyError::internal())
    }
    pub async fn initialize(&self) -> IvyResult<()> {
        let evidence = self.evidence;
        let store = evidence.store;
        evidence.verify_owner().await?;
        let root = store
            .client
            .request(
                "objects.stat",
                IvyJson::object([("objectId", IvyJson::String(store.root.clone()))]),
                RequestOptions::default(),
            )
            .await?;
        need(
            !root.get("effectivelyArchived").is_some_and(truthy),
            "secretary_scope_conflict",
            "The Secretary root is archived.",
        )?;
        store.journal.cleanup(0, 0, None, None)?;
        for definition in evidence
            .settings
            .required("sources")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let id = definition.required("sourceId")?;
            let value = IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("definition", definition.clone()),
                ("definitionHash", hash_json(definition)?.into()),
                ("cursor", IvyJson::Null),
                ("checkpointOperation", IvyJson::Null),
                ("updatedAt", IvyJson::Null),
            ]);
            store
                .create(
                    Record::Source,
                    &source_name(id)?,
                    &value,
                    &mutation(&[
                        IvyJson::String(store.root.clone()),
                        "source".into(),
                        source_identity(definition)?,
                    ])?,
                )
                .await?;
            let source = evidence.source(id).await?;
            if hash_json(source.required("value")?.required("definition")?)?
                != hash_json(definition)?
            {
                evidence.verify_owner().await?;
                let mut next = source.required("value")?.clone();
                next.insert("definition", definition.clone());
                next.insert("definitionHash", hash_json(definition)?.into());
                store.amend(&source, Record::Source, &next).await?;
            }
        }
        Ok(())
    }
    pub async fn tick(&self) -> IvyResult<()> {
        RecoveryScan::tick_shared(
            self.issues,
            &InboxRecovery {
                evidence: self.evidence,
            },
            self.evidence
                .settings
                .required("recordsPerTick")?
                .as_f64()
                .ok_or_else(IvyError::internal)? as usize,
        )
        .await
    }
    pub async fn notice_target(&self) -> IvyResult<IvyJson> {
        self.notice_target_checked()
            .await
            .map_err(MainChatFailure::into_error)
    }
    async fn notice_target_checked(&self) -> Result<IvyJson, MainChatFailure> {
        let target = main_chat_checked(self.evidence.store.client.as_ref()).await?;
        need(
            target.expected_bridge.get("callerPrincipalId")
                == self
                    .evidence
                    .settings
                    .required("identity")?
                    .get("principalId"),
            "secretary_notice_unavailable",
            "Main must identify Secretary as the actual caller.",
        )?;
        Ok(target.to_json())
    }
    pub async fn status(&self, caller: &IvyJson, scope: &IvyJson) -> IvyResult<IvyJson> {
        let evidence = self.evidence;
        evidence.authorize(caller, scope, "status", None)?;
        evidence.verify_owner().await?;
        let mut sources = Vec::new();
        for definition in evidence
            .settings
            .required("sources")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let source = evidence.source(definition.required("sourceId")?).await?;
            sources.push(IvyJson::object([
                ("object", source.required("pin")?.clone()),
                ("data", source.required("value")?.clone()),
            ]));
        }
        let mut policy = evidence.settings.required("policy")?.clone();
        if policy.get("voiceEscalation").is_none_or(IvyJson::is_null) {
            let target = discover_voice_target(evidence.store.client.as_ref(), self.tools).await?;
            policy.insert("voiceEscalation", target.unwrap_or(IvyJson::Null));
        }
        let notice_configured = match self.notice_target_checked().await {
            Ok(_) => true,
            // This is the original instanceof IvyError catch. A coded
            // internal_error is swallowed; an uncoded boundary is propagated.
            Err(MainChatFailure::Ivy(_)) => false,
            Err(boundary) => return Err(boundary.into_error()),
        };
        let issues = self
            .issues
            .borrow()
            .issues
            .iter()
            .map(|(id, code)| {
                IvyJson::object([
                    ("objectId", id.clone().into()),
                    ("code", code.clone().into()),
                ])
            })
            .collect();
        Ok(IvyJson::object([
            ("identity", evidence.settings.required("identity")?.clone()),
            ("sources", IvyJson::Array(sources)),
            ("policy", policy),
            ("noticeConfigured", notice_configured.into()),
            ("recoveryIssues", IvyJson::Array(issues)),
            ("observedAt", self.now()?.into()),
        ]))
    }
    pub async fn list_assignments(
        &self,
        caller: &IvyJson,
        request: &IvyJson,
    ) -> IvyResult<IvyJson> {
        validate(Schema::ListAssignmentsRequest, request)?;
        let evidence = self.evidence;
        evidence.authorize(caller, request.required("expectedScope")?, "status", None)?;
        evidence.verify_owner().await?;
        let mut predicates = vec![IvyJson::object([
            ("op", "eq".into()),
            ("field", "object.parentId".into()),
            ("value", IvyJson::String(evidence.store.root.clone())),
        ])];
        for (key, field) in [
            ("assignmentId", "data:/assignmentId"),
            ("enabled", "data:/enabled"),
            ("triggerKind", "data:/trigger/kind"),
        ] {
            if request.get(key).is_some() && (key == "enabled" || nonempty(request.get(key))) {
                predicates.push(IvyJson::object([
                    ("op", "eq".into()),
                    ("field", field.into()),
                    ("value", request.required(key)?.clone()),
                ]));
            }
        }
        let mut query = IvyJson::object([
            ("contractKey", "secretary/assignment".into()),
            ("includeContent", true.into()),
            (
                "limit",
                request
                    .get("limit")
                    .cloned()
                    .unwrap_or(IvyJson::Number(50.0)),
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
                IvyJson::object([("op", "and".into()), ("args", IvyJson::Array(predicates))]),
            ),
        ]);
        if nonempty(request.get("cursor")) {
            query.insert("cursor", request.required("cursor")?.clone());
        }
        let page = evidence
            .store
            .client
            .request("objects.query", query, RequestOptions::default())
            .await?;
        let mut assignments = Vec::new();
        for row in page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let current = evidence
                .store
                .decode(Record::Assignment, query_document(row)?)?;
            assignments.push(IvyJson::object([
                ("pin", current.required("pin")?.clone()),
                ("assignment", current.required("value")?.clone()),
            ]));
        }
        Ok(IvyJson::object([
            ("assignments", IvyJson::Array(assignments)),
            (
                "nextCursor",
                page.get("nextCursor").cloned().unwrap_or(IvyJson::Null),
            ),
            ("observedAt", self.now()?.into()),
        ]))
    }
    pub async fn get_assignment(&self, caller: &IvyJson, request: &IvyJson) -> IvyResult<IvyJson> {
        validate(Schema::GetAssignmentRequest, request)?;
        let evidence = self.evidence;
        evidence.authorize(caller, request.required("expectedScope")?, "status", None)?;
        evidence.verify_owner().await?;
        let current = evidence
            .store
            .named(
                Record::Assignment,
                &assignment_name(string(request, "assignmentId")?)?.into(),
            )
            .await?;
        need(
            current
                .as_ref()
                .and_then(|v| v.get("value"))
                .and_then(|v| v.get("assignmentId"))
                == request.get("assignmentId"),
            "not_found",
            "The Secretary assignment was not found.",
        )?;
        let current = current.ok_or_else(IvyError::internal)?;
        Ok(IvyJson::object([
            ("pin", current.required("pin")?.clone()),
            ("assignment", current.required("value")?.clone()),
            ("observedAt", self.now()?.into()),
        ]))
    }
}
