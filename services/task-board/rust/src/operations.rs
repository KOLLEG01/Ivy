//! Retained prepare/apply/receipt protocol. Store adapters validate exact domain
//! schemas; this layer preserves original JSON values and destination identities.
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};

const OPERATION: &str = "task-board/operation";

#[derive(Debug, Clone)]
pub struct Document {
    pub pin: Json,
    pub value: Json,
    pub parent_id: Option<JsString>,
    pub content_hash: String,
}

/// The journal implementation owns CAS, schema validation, mutation receipts and
/// local/remote routing. No network effect may be inferred from a failed call.
#[allow(async_fn_in_trait)]
pub trait Store {
    fn root_object_id(&self) -> Option<JsString>;
    fn contract_version(&self, key: &str) -> IvyResult<String>;
    fn retired_operation(&self, caller: &JsString, id: &JsString) -> IvyResult<bool>;
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>>;
    fn complete_local_workflow(&self, id: &JsString, at: &JsString) -> IvyResult<()>;
    async fn named(
        &self,
        key: &str,
        name: &str,
        parent: Option<&JsString>,
    ) -> IvyResult<Option<Document>>;
    /// Address is either an object ID string or an exact object/revision pin.
    async fn read(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document>;
    async fn write(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json>;
    async fn prepared_mutation_id(&self, mutation: &str) -> IvyResult<JsString>;
    async fn write_prepared(
        &self,
        key: &str,
        value: &Json,
        mutation: &JsString,
        destination: &Json,
    ) -> IvyResult<Json>;
}

pub struct Operations<S, C> {
    pub store: S,
    /// Inject the owner's ISO UTC clock; called at the same persisted transitions as TS.
    pub now: C,
}

fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(|| IvyError::new("invalid_arguments", "Expected a string."))
}
fn phase(value: &Json) -> IvyResult<String> {
    value.required("phase")?.as_str()
}
fn writes(value: &Json) -> IvyResult<&[Json]> {
    value
        .required("writes")?
        .as_array()
        .ok_or_else(|| IvyError::new("invalid_arguments", "Expected an array."))
}
fn finished(value: &Json) -> IvyResult<bool> {
    Ok(matches!(phase(value)?.as_str(), "failed" | "succeeded"))
}
fn canonical_same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn nullable(value: Option<JsString>) -> Json {
    value.map(Json::String).unwrap_or(Json::Null)
}
fn recoverable_create(error: &IvyError) -> bool {
    matches!(
        error.code.as_str(),
        "revision_conflict" | "mutation_conflict"
    )
}
fn with(value: &Json, fields: impl IntoIterator<Item = (&'static str, Json)>) -> Json {
    let mut next = value.clone();
    for (key, value) in fields {
        next.insert(key, value);
    }
    next
}
pub fn identity_hash(caller: &JsString, id: &JsString) -> IvyResult<String> {
    Ok(hash_json(&Json::object([
        ("callerPrincipalId", Json::String(caller.clone())),
        ("operationId", Json::String(id.clone())),
    ]))?[7..]
        .to_owned())
}
pub fn mutation(scope: &JsString, step: &str) -> IvyResult<String> {
    Ok(format!(
        "task-board:{}",
        &hash_json(&Json::object([
            ("scope", Json::String(scope.clone())),
            ("step", step.into()),
        ]))?[7..]
    ))
}
fn operation_name(caller: &JsString, id: &JsString) -> IvyResult<String> {
    Ok(format!(
        "TaskBoard operation {}",
        identity_hash(caller, id)?
    ))
}

impl<S: Store, C: Fn() -> String> Operations<S, C> {
    pub async fn find(&self, caller: &JsString, id: &JsString) -> IvyResult<Option<Document>> {
        let found = self
            .store
            .named(OPERATION, &operation_name(caller, id)?, None)
            .await?;
        require(
            found.is_some() || !self.store.retired_operation(caller, id)?,
            "operation_expired",
            "The completed TaskBoard operation was evicted; its identity cannot be reused.",
        )?;
        if let Some(found) = &found {
            require(
                string(&found.value, "callerPrincipalId")? == caller
                    && string(&found.value, "operationId")? == id,
                "task_board_identity_conflict",
                "Operation identity does not match its original caller.",
            )?;
        }
        Ok(found)
    }

    pub async fn begin(&self, request: &Json, actor: &Json) -> IvyResult<Document> {
        let caller = string(actor, "principalId")?;
        let id = string(request, "operationId")?;
        let existing = self.find(caller, id).await?;
        let request_hash = hash_json(request)?;
        if let Some(existing) = existing {
            require(
                existing.value.required("requestHash")? == &Json::from(request_hash),
                "mutation_conflict",
                "The operation identity already names different TaskBoard arguments.",
            )?;
            return Ok(existing);
        }
        let now = (self.now)();
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("operationId", Json::String(id.clone())),
            ("callerPrincipalId", Json::String(caller.clone())),
            ("request", request.clone()),
            ("requestHash", request_hash.clone().into()),
            ("actor", actor.clone()),
            ("createdAt", now.clone().into()),
            ("updatedAt", now.into()),
            ("phase", "accepted".into()),
            ("writes", Json::Array(vec![])),
            ("outcome", Json::Null),
            ("error", Json::Null),
        ]);
        let destination = Json::object([(
            "create",
            Json::object([
                ("parentId", nullable(self.store.root_object_id())),
                ("name", operation_name(caller, id)?.into()),
            ]),
        )]);
        // The read is inside the original try boundary, including recovery from
        // an immutable create that succeeded before its follow-up read failed.
        let result = async {
            let pin = self
                .store
                .write(
                    OPERATION,
                    &value,
                    &mutation(&identity_hash(caller, id)?.into(), "accept")?,
                    &destination,
                )
                .await?;
            self.store.read(OPERATION, &pin, true).await
        }
        .await;
        match result {
            Ok(document) => Ok(document),
            Err(error) if recoverable_create(&error) => {
                let winner = self.find(caller, id).await?;
                require(
                    winner.as_ref().is_some_and(|w| {
                        w.value.get("requestHash") == Some(&Json::from(request_hash))
                    }),
                    "mutation_conflict",
                    "The winning Operation has different original arguments.",
                )?;
                Ok(winner.unwrap())
            }
            Err(error) => Err(error),
        }
    }

    async fn amend(
        &self,
        id: &JsString,
        mut transition: impl FnMut(&Json) -> IvyResult<Option<Json>>,
    ) -> IvyResult<Document> {
        for _ in 0..12 {
            let current = self
                .store
                .read(OPERATION, &Json::String(id.clone()), true)
                .await?;
            let Some(next) = transition(&current.value)? else {
                return Ok(current);
            };
            let updated = with(&next, [("updatedAt", (self.now)().into())]);
            let revision = current.pin.required("revision")?;
            let revision_number = revision
                .as_f64()
                .ok_or_else(|| IvyError::new("invalid_arguments", "Expected a revision."))?;
            let step = format!("amend:{revision_number}:{}", hash_json(&updated)?);
            let destination = Json::object([
                ("objectId", Json::String(id.clone())),
                ("expectedRevision", revision.clone()),
            ]);
            let result = async {
                let pin = self
                    .store
                    .write(OPERATION, &updated, &mutation(id, &step)?, &destination)
                    .await?;
                self.store.read(OPERATION, &pin, true).await
            }
            .await;
            match result {
                Ok(document) => return Ok(document),
                Err(error) if error.code == "revision_conflict" => {}
                Err(error) => return Err(error),
            }
        }
        Err(IvyError::new(
            "task_board_contention",
            "The operation changed repeatedly. Reconcile its saved stages before retrying.",
        )
        .with_outcome(Outcome::Unknown))
    }

    pub async fn prepare_native(&self, id: &JsString, proposed: &Json) -> IvyResult<Json> {
        let key = proposed.required("key")?;
        let saved = self
            .amend(id, |operation| {
                let mut calls = operation
                    .get("nativeCalls")
                    .and_then(Json::as_array)
                    .unwrap_or(&[])
                    .to_vec();
                if let Some(existing) = calls.iter().find(|call| call.get("key") == Some(key)) {
                    require(
                        canonical_same(&with(existing, [("observation", Json::Null)]), proposed)?,
                        "task_board_native_intent_mismatch",
                        "The native lifecycle stage already has another retained request.",
                    )?;
                    return Ok(None);
                }
                require(
                    !finished(operation)?,
                    "task_board_operation_failed",
                    "A finished action cannot prepare native work.",
                )?;
                calls.push(proposed.clone());
                Ok(Some(with(operation, [("nativeCalls", Json::Array(calls))])))
            })
            .await?;
        Ok(saved
            .value
            .required("nativeCalls")?
            .as_array()
            .ok_or_else(|| IvyError::new("invalid_arguments", "Expected an array."))?
            .iter()
            .find(|call| call.get("key") == Some(key))
            .unwrap()
            .clone())
    }

    pub async fn observe_native(
        &self,
        id: &JsString,
        key: &JsString,
        observation: &Json,
    ) -> IvyResult<Json> {
        let key = Json::String(key.clone());
        let saved = self
            .amend(id, |operation| {
                let mut calls = operation
                    .get("nativeCalls")
                    .and_then(Json::as_array)
                    .unwrap_or(&[])
                    .to_vec();
                let index = calls.iter().position(|call| call.get("key") == Some(&key));
                require(
                    index.is_some_and(|i| {
                        calls[i].get("operationId") == observation.get("operationId")
                    }),
                    "task_board_native_intent_mismatch",
                    "The observation must identify the retained native lifecycle call.",
                )?;
                let index = index.unwrap();
                if canonical_same(calls[index].required("observation")?, observation)? {
                    return Ok(None);
                }
                calls[index] = with(&calls[index], [("observation", observation.clone())]);
                Ok(Some(with(operation, [("nativeCalls", Json::Array(calls))])))
            })
            .await?;
        Ok(saved
            .value
            .required("nativeCalls")?
            .as_array()
            .ok_or_else(|| IvyError::new("invalid_arguments", "Expected an array."))?
            .iter()
            .find(|call| call.get("key") == Some(&key))
            .unwrap()
            .clone())
    }

    pub async fn archive_identity(&self, id: &JsString) -> IvyResult<JsString> {
        let current = self
            .store
            .read(OPERATION, &Json::String(id.clone()), true)
            .await?;
        if let Some(Json::String(identity)) = current.value.get("archiveMutationId") {
            if !identity.0.is_empty() {
                return Ok(identity.clone());
            }
        }
        let identity = self
            .store
            .prepared_mutation_id(&mutation(id, "archive-membership")?)
            .await?;
        let saved = self
            .amend(id, |operation| {
                if let Some(Json::String(value)) = operation.get("archiveMutationId") {
                    if !value.0.is_empty() {
                        return Ok(None);
                    }
                }
                Ok(Some(with(
                    operation,
                    [("archiveMutationId", Json::String(identity.clone()))],
                )))
            })
            .await?;
        Ok(string(&saved.value, "archiveMutationId")?.clone())
    }

    pub async fn stage(
        &self,
        id: &JsString,
        index: f64,
        key: &str,
        destination: &Json,
        proposed: &Json,
    ) -> IvyResult<Json> {
        require(
            index.is_finite() && index.fract() == 0.0 && (0.0..16.0).contains(&index),
            "task_board_stage_limit",
            "An operation supports at most 16 ordered prepared writes.",
        )?;
        let index = index as usize;
        let current = self
            .store
            .read(OPERATION, &Json::String(id.clone()), true)
            .await?;
        let agrees = |write: &Json| -> IvyResult<()> {
            require(
                write.required("contractKey")? == &Json::from(key)
                    && canonical_same(write.required("destination")?, destination)?,
                "task_board_stage_conflict",
                "This operation stage already names another contract or destination.",
            )
        };
        if let Some(existing) = writes(&current.value)?.get(index) {
            agrees(existing)?;
            return Ok(existing.clone());
        }
        require(
            !finished(&current.value)? && index == writes(&current.value)?.len(),
            "task_board_stage_conflict",
            "New writes must append to the unfinished operation in order.",
        )?;
        let hash = hash_json(proposed)?;
        let name = format!("Prepared {index} {}", &hash[7..]);
        let create = Json::object([(
            "create",
            Json::object([
                ("parentId", Json::String(id.clone())),
                ("name", name.clone().into()),
            ]),
        )]);
        let payload = match self
            .store
            .write(
                key,
                proposed,
                &mutation(id, &format!("payload:{index}:{hash}"))?,
                &create,
            )
            .await
        {
            Ok(pin) => pin,
            Err(error) if recoverable_create(&error) => {
                let found = self.store.named(key, &name, Some(id)).await?;
                require(
                    found.as_ref().is_some_and(|f| f.content_hash == hash),
                    "task_board_evidence_mismatch",
                    "Prepared payload identity has different saved bytes.",
                )?;
                found.unwrap().pin
            }
            Err(error) => return Err(error),
        };
        let entry = Json::object([
            (
                "mutationId",
                Json::String(
                    self.store
                        .prepared_mutation_id(&mutation(id, &format!("apply:{index}"))?)
                        .await?,
                ),
            ),
            ("contractKey", key.into()),
            ("contractVersion", self.store.contract_version(key)?.into()),
            (
                "payload",
                Json::object([
                    ("object", payload),
                    ("label", name.into()),
                    ("mediaType", "application/json".into()),
                    ("contentHash", hash.into()),
                ]),
            ),
            ("destination", destination.clone()),
            ("outcome", Json::Null),
        ]);
        let saved = self
            .amend(id, |operation| {
                if let Some(winner) = writes(operation)?.get(index) {
                    agrees(winner)?;
                    return Ok(None);
                }
                require(
                    !finished(operation)? && writes(operation)?.len() == index,
                    "task_board_stage_conflict",
                    "The operation cannot accept this new write.",
                )?;
                let mut next = writes(operation)?.to_vec();
                next.push(entry.clone());
                Ok(Some(with(
                    operation,
                    [
                        ("phase", "preparing".into()),
                        ("writes", Json::Array(next)),
                        ("error", Json::Null),
                    ],
                )))
            })
            .await?;
        Ok(writes(&saved.value)?[index].clone())
    }

    pub async fn apply(&self, id: &JsString, index: usize) -> IvyResult<Json> {
        let current = self
            .store
            .read(OPERATION, &Json::String(id.clone()), true)
            .await?;
        let entry = writes(&current.value)?.get(index);
        require(
            entry.is_some(),
            "task_board_stage_conflict",
            "No write has been prepared at this stage.",
        )?;
        let entry = entry.unwrap();
        if !entry.required("outcome")?.is_null() {
            return Ok(entry.required("outcome")?.clone());
        }
        require(
            !finished(&current.value)?,
            "task_board_stage_conflict",
            "A completed/failed operation cannot issue another prepared write.",
        )?;
        let reference = entry.required("payload")?;
        let key = entry.required("contractKey")?.as_str()?;
        let payload = self
            .store
            .read(&key, reference.required("object")?, false)
            .await?;
        require(
            payload.parent_id.as_ref() == Some(id)
                && reference.required("contentHash")? == &Json::from(payload.content_hash)
                && reference.required("mediaType")? == &Json::from("application/json"),
            "task_board_evidence_mismatch",
            "The staged payload must be owned by this operation and match its immutable content hash.",
        )?;
        let result = self
            .store
            .write_prepared(
                &key,
                &payload.value,
                string(entry, "mutationId")?,
                entry.required("destination")?,
            )
            .await?;
        self.amend(id, |operation| {
            let latest = writes(operation)?.get(index);
            require(
                match latest {
                    Some(latest) => canonical_same(
                        &with(latest, [("outcome", Json::Null)]),
                        &with(entry, [("outcome", Json::Null)]),
                    )?,
                    None => false,
                },
                "task_board_stage_conflict",
                "The applied stage no longer has its original saved intent.",
            )?;
            let latest = latest.unwrap();
            if !latest.required("outcome")?.is_null() {
                require(
                    canonical_same(latest.required("outcome")?, &result)?,
                    "task_board_evidence_mismatch",
                    "The retained write receipt differs from Hive replay.",
                )?;
                return Ok(None);
            }
            let mut next = writes(operation)?.to_vec();
            next[index] = with(latest, [("outcome", result.clone())]);
            Ok(Some(with(
                operation,
                [
                    ("phase", "publishing".into()),
                    ("writes", Json::Array(next)),
                    ("error", Json::Null),
                ],
            )))
        })
        .await?;
        Ok(result)
    }

    pub async fn finish(&self, id: &JsString, outcome: &Json) -> IvyResult<Json> {
        let done = self
            .amend(id, |operation| {
                require(
                    outcome.get("operationId") == operation.get("operationId"),
                    "task_board_identity_conflict",
                    "An outcome must retain its original operation ID.",
                )?;
                if phase(operation)? == "succeeded" {
                    require(
                        canonical_same(operation.required("outcome")?, outcome)?,
                        "mutation_conflict",
                        "The completed action has another original outcome.",
                    )?;
                    return Ok(None);
                }
                require(
                    phase(operation)? != "failed"
                        && writes(operation)?
                            .iter()
                            .all(|w| w.get("outcome").is_some_and(|o| !o.is_null())),
                    "task_board_publication_pending",
                    "All prepared writes must have durable receipts before completing the action.",
                )?;
                Ok(Some(with(
                    operation,
                    [
                        ("phase", "succeeded".into()),
                        ("outcome", outcome.clone()),
                        ("error", Json::Null),
                    ],
                )))
            })
            .await?;
        self.store
            .complete_local_workflow(id, string(&done.value, "updatedAt")?)?;
        Ok(done.value.required("outcome")?.clone())
    }

    pub async fn failure(
        &self,
        id: &JsString,
        error: &IvyError,
        needs_attention: bool,
    ) -> IvyResult<Document> {
        let failed = self
            .amend(id, |operation| {
                if finished(operation)? {
                    return Ok(None);
                }
                Ok(Some(with(
                    operation,
                    [
                        (
                            "phase",
                            if needs_attention {
                                "needs_attention"
                            } else {
                                "failed"
                            }
                            .into(),
                        ),
                        ("outcome", Json::Null),
                        (
                            "error",
                            Json::object([
                                ("code", Json::String(error.code_js())),
                                ("message", Json::String(error.message_js())),
                                ("execution", error.outcome.as_str().into()),
                            ]),
                        ),
                    ],
                )))
            })
            .await?;
        if phase(&failed.value)? == "failed" {
            let at = string(&failed.value, "updatedAt")?;
            self.store.complete_local_workflow(id, at)?;
            let request = failed.value.required("request")?;
            if failed.value.required("actor")?.required("source")? == &Json::from("scheduler")
                && matches!(
                    request.required("action")?.as_str()?.as_str(),
                    "start" | "continue"
                )
            {
                if let Some(intent) = request.get("intent") {
                    let intent = string(intent, "objectId")?;
                    if self.store.local_key(intent)?.as_deref() == Some("task-board/native-plan") {
                        self.store.complete_local_workflow(intent, at)?;
                    }
                }
            }
        }
        Ok(failed)
    }
}
