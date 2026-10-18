//! Durable admitted-action receipts. Main changes and native execution remain
//! separate transitions; only proven unapplied admission may be refused.
use crate::{
    admission::ChatAdmission,
    store::{ChatStore, Destination, Document, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, chat_schema::validate_chat,
    hash::hash_json, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};

pub fn contention(error: &IvyError) -> bool {
    matches!(
        error.code.as_str(),
        "revision_conflict" | "mutation_conflict"
    )
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
// Ordinary property access in the original saved-record checks. Missing/null
// receivers throw ordinary errors; primitive JSON receivers have no such fields.
fn property<'a>(value: Option<&'a Json>, key: &str) -> IvyResult<Option<&'a Json>> {
    match value {
        None | Some(Json::Null) => Err(IvyError::internal()),
        Some(value) => Ok(value.get(key)),
    }
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
pub struct ChatOperations<'a> {
    pub store: &'a ChatStore,
    pub admission: &'a ChatAdmission,
}
impl<'a> ChatOperations<'a> {
    pub fn new(store: &'a ChatStore, admission: &'a ChatAdmission) -> IvyResult<Self> {
        require(
            admission.definition().get("rootObjectId")
                == Some(
                    &store
                        .root_object_id
                        .as_deref()
                        .map(Json::from)
                        .unwrap_or(Json::Null),
                ),
            "chat_scope_mismatch",
            "The store must use the configured ChatBridge root.",
        )?;
        Ok(Self { store, admission })
    }
    pub fn name(&self, caller: &JsString, operation_id: &JsString) -> IvyResult<String> {
        Ok(format!(
            "Chat operation {}",
            &hash_json(&Json::object([
                (
                    "principalId",
                    self.admission.definition().required("principalId")?.clone()
                ),
                (
                    "workspaceId",
                    self.admission.definition().required("workspaceId")?.clone()
                ),
                ("caller", Json::String(caller.clone())),
                ("operationId", Json::String(operation_id.clone())),
            ]))?[7..]
        ))
    }
    pub fn check(&self, value: &Json) -> IvyResult<()> {
        let request = property(Some(value), "request")?;
        // Read request.action before other checks, as the original role selection does.
        let action = property(request, "action")?;
        let original = property(request, "expectedBridge")?;
        let definition = self.admission.definition();
        require(
            value.get("workspaceId") == definition.get("workspaceId")
                && value.get("definitionHash") == property(original, "definitionHash")?
                && property(request, "operationId")? == value.get("operationId")
                && value.get("requestHash")
                    == Some(&Json::from(hash_json(
                        request.ok_or_else(IvyError::internal)?,
                    )?))
                && property(original, "principalId")? == definition.get("principalId")
                && property(original, "callerPrincipalId")? == value.get("callerPrincipalId")
                && property(original, "workspaceId")? == definition.get("workspaceId"),
            "chat_identity_conflict",
            "The saved operation must retain its original Main, caller and request.",
        )?;
        if let Some(outcome) = value.get("outcome").filter(|v| truthy(v)) {
            require(
                outcome.get("operationId") == value.get("operationId")
                    && outcome.get("action") == action,
                "chat_identity_conflict",
                "The saved outcome belongs to another operation or action.",
            )?;
        }
        Ok(())
    }
    pub async fn find(
        &self,
        caller: &JsString,
        operation_id: &JsString,
    ) -> IvyResult<Option<Document>> {
        let result = self
            .store
            .named("chat-bridge/operation", &self.name(caller, operation_id)?)
            .await
            .map_err(|e| e.into_ivy())?;
        if let Some(result) = &result {
            self.check(&result.value)?;
            require(
                result.value.get("callerPrincipalId") == Some(&Json::String(caller.clone()))
                    && result.value.get("operationId") == Some(&Json::String(operation_id.clone())),
                "chat_identity_conflict",
                "The durable operation name has another caller or identity.",
            )?;
        }
        Ok(result)
    }
    pub async fn begin(&self, caller: &JsString, original: &Json) -> IvyResult<Document> {
        self.admission.authorize(
            caller,
            original.required("expectedBridge")?,
            original.get("channel").filter(|v| truthy(v)),
        )?;
        validate_chat("AdmittedActionRequest", original)?;
        let request = original.clone();
        let request_hash = hash_json(&request)?;
        let operation_id = string(&request, "operationId")?;
        if let Some(existing) = self.find(caller, operation_id).await? {
            require(
                existing.value.get("requestHash") == Some(&Json::from(request_hash)),
                "mutation_conflict",
                "This operation already names different ChatBridge arguments.",
            )?;
            return Ok(existing);
        }
        let at = self.store.now_iso()?;
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            (
                "workspaceId",
                self.admission.definition().required("workspaceId")?.clone(),
            ),
            (
                "definitionHash",
                request
                    .required("expectedBridge")?
                    .required("definitionHash")?
                    .clone(),
            ),
            ("callerPrincipalId", Json::String(caller.clone())),
            ("operationId", Json::String(operation_id.clone())),
            ("request", request.clone()),
            ("requestHash", request_hash.clone().into()),
            ("phase", "accepted".into()),
            ("nativeCall", Json::Null),
            ("outcome", Json::Null),
            ("error", Json::Null),
            ("createdAt", at.clone().into()),
            ("updatedAt", at.into()),
        ]);
        let name = self.name(caller, operation_id)?;
        let run = async {
            let pin = self
                .store
                .write(
                    "chat-bridge/operation",
                    &value,
                    &mutation(&name, "accept")?,
                    Destination::Create {
                        parent_id: self.store.root_object_id.as_deref(),
                        name: &name,
                    },
                )
                .await
                .map_err(|e| e.into_ivy())?;
            self.store
                .read("chat-bridge/operation", Reference::Pinned(&pin))
                .await
                .map_err(|e| e.into_ivy())
        }
        .await;
        match run {
            Ok(doc) => Ok(doc),
            Err(error) if contention(&error) => {
                let winner = self.find(caller, operation_id).await?;
                require(
                    winner.as_ref().is_some_and(|doc| {
                        doc.value.get("requestHash") == Some(&Json::from(request_hash.clone()))
                    }),
                    "mutation_conflict",
                    "The concurrent action has different original arguments.",
                )?;
                Ok(winner.unwrap())
            }
            Err(error) => Err(error),
        }
    }
    pub async fn finish(&self, id: &str, outcome: &Json) -> IvyResult<Document> {
        validate_chat("ActionOutcome", outcome)?;
        for _ in 0..16 {
            let current = self
                .store
                .read("chat-bridge/operation", Reference::Current(id))
                .await
                .map_err(|e| e.into_ivy())?;
            self.check(&current.value)?;
            require(
                current.value.get("operationId") == outcome.get("operationId")
                    && current.value.required("request")?.get("action") == outcome.get("action"),
                "chat_identity_conflict",
                "An action receipt must preserve its original identity.",
            )?;
            if current.value.get("phase") == Some(&Json::from("succeeded")) {
                require(
                    canonical(current.value.required("outcome")?, MANAGEMENT_FRAME_BYTES)?
                        == canonical(outcome, MANAGEMENT_FRAME_BYTES)?,
                    "mutation_conflict",
                    "The original action has another saved outcome.",
                )?;
                return Ok(current);
            }
            require(
                current.value.get("phase") != Some(&Json::from("failed")),
                "chat_operation_failed",
                "A failed action cannot be completed by a new receipt.",
            )?;
            let mut next = current.value.clone();
            next.insert("phase", "succeeded".into());
            next.insert("outcome", outcome.clone());
            next.insert("error", Json::Null);
            next.insert("updatedAt", self.store.now_iso()?.into());
            let run = async {
                let pin = self
                    .store
                    .write(
                        "chat-bridge/operation",
                        &next,
                        &mutation(
                            id,
                            &format!("finish:{}:{}", current.pin.revision, hash_json(&next)?),
                        )?,
                        Destination::Update {
                            object_id: id,
                            expected_revision: current.pin.revision,
                        },
                    )
                    .await
                    .map_err(|e| e.into_ivy())?;
                self.store
                    .read("chat-bridge/operation", Reference::Pinned(&pin))
                    .await
                    .map_err(|e| e.into_ivy())
            }
            .await;
            match run {
                Ok(doc) => return Ok(doc),
                Err(error) if contention(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "The action changed repeatedly. Recover its original operation.",
        )
        .with_outcome(Outcome::Unknown))
    }
    /// The caller must first prove the original Main/Input admission was not applied.
    pub async fn reject_unapplied(
        &self,
        id: &str,
        code: &str,
        message: &str,
    ) -> IvyResult<Document> {
        for _ in 0..16 {
            let current = self
                .store
                .read("chat-bridge/operation", Reference::Current(id))
                .await
                .map_err(|e| e.into_ivy())?;
            self.check(&current.value)?;
            if current.value.get("phase") == Some(&Json::from("failed")) {
                return Ok(current);
            }
            require(
                current.value.get("phase") == Some(&Json::from("accepted"))
                    && current.value.get("nativeCall") == Some(&Json::Null),
                "chat_operation_unresolved",
                "Only an unexecuted admission may be refused.",
            )?;
            let mut next = current.value.clone();
            next.insert("phase", "failed".into());
            next.insert(
                "error",
                Json::object([
                    ("code", code.into()),
                    ("detail", message.into()),
                    ("outcome", "not_executed".into()),
                ]),
            );
            next.insert("updatedAt", current.value.required("createdAt")?.clone());
            let run = async {
                let pin = self
                    .store
                    .write(
                        "chat-bridge/operation",
                        &next,
                        &mutation(
                            id,
                            &format!("refuse:{}:{}", current.pin.revision, hash_json(&next)?),
                        )?,
                        Destination::Update {
                            object_id: id,
                            expected_revision: current.pin.revision,
                        },
                    )
                    .await
                    .map_err(|e| e.into_ivy())?;
                self.store
                    .read("chat-bridge/operation", Reference::Pinned(&pin))
                    .await
                    .map_err(|e| e.into_ivy())
            }
            .await;
            match run {
                Ok(doc) => return Ok(doc),
                Err(error) if contention(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "The refusal needs original-operation recovery.",
        )
        .with_outcome(Outcome::Unknown))
    }
}
