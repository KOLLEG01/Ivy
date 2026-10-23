//! Durable input admission. Native execution acts separately on the saved head.
use crate::{
    operations::{ChatOperations, contention},
    store::{Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::{collections::HashSet, future::Future};

fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn js<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn number(value: &Json, key: &str) -> IvyResult<f64> {
    value.required(key)?.as_f64().ok_or_else(IvyError::internal)
}
fn array<'a>(value: &'a Json, key: &str) -> IvyResult<&'a [Json]> {
    value
        .required(key)?
        .as_array()
        .ok_or_else(IvyError::internal)
}
fn safe_integer(n: f64) -> bool {
    n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0
}
fn pin(value: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: value.required("objectId")?.as_str()?,
        revision: number(value, "revision")? as i64,
    })
}
fn definition_identity(definition: &Json) -> IvyResult<Json> {
    Ok(Json::object([
        ("principalId", definition.required("principalId")?.clone()),
        ("workspaceId", definition.required("workspaceId")?.clone()),
        ("definitionHash", hash_json(definition)?.into()),
    ]))
}
// A changed immutable definition owns new names; it cannot reinterpret old Main,
// conversation or input records as its own state.
pub fn main_name(definition: &Json) -> IvyResult<String> {
    Ok(format!(
        "Chat Main {}",
        &hash_json(&definition_identity(definition)?)?[7..]
    ))
}
pub fn conversation_name(definition: &Json) -> IvyResult<String> {
    Ok(format!(
        "Chat conversation {}",
        &hash_json(&definition_identity(definition)?)?[7..]
    ))
}
pub fn input_name(
    definition: &Json,
    identity: &Json,
    definition_hash: Option<&str>,
) -> IvyResult<String> {
    let hash = definition_hash
        .map(str::to_owned)
        .map(Ok)
        .unwrap_or_else(|| hash_json(definition))?;
    Ok(format!(
        "Chat input {}",
        &hash_json(&Json::object([
            ("principalId", definition.required("principalId")?.clone()),
            ("workspaceId", definition.required("workspaceId")?.clone()),
            ("definitionHash", hash.into()),
            ("identity", identity.clone()),
        ]))?[7..]
    ))
}

pub type BindingFuture<'a> = std::pin::Pin<Box<dyn Future<Output = IvyResult<Pin>> + Send + 'a>>;
/// Main supplies its durable binding creation/recovery path to message ingress.
pub trait EnsureBinding: Send + Sync {
    fn ensure_binding<'a>(
        &'a self,
        caller: &'a JsString,
        operation_id: &'a JsString,
    ) -> BindingFuture<'a>;
}

pub struct ChatQueue<'a> {
    pub operations: &'a ChatOperations<'a>,
    pub ensure_binding: Option<&'a dyn EnsureBinding>,
}
impl<'a> ChatQueue<'a> {
    pub fn new(
        operations: &'a ChatOperations<'a>,
        ensure_binding: Option<&'a dyn EnsureBinding>,
    ) -> Self {
        Self {
            operations,
            ensure_binding,
        }
    }
    pub async fn main(&self) -> IvyResult<Document> {
        let doc = self
            .operations
            .store
            .named(
                "chat-bridge/main",
                &main_name(self.operations.admission.definition())?,
            )
            .await
            .map_err(|e| e.into_ivy())?;
        require(
            doc.is_some(),
            "chat_main_missing",
            "No Main aggregate exists for this ChatBridge workspace.",
        )?;
        self.check_main(doc.unwrap())
    }
    pub fn check_main(&self, main: Document) -> IvyResult<Document> {
        let value = &main.value;
        require(
            value.get("definitionHash")
                == Some(&Json::from(self.operations.admission.definition_hash()))
                && same(
                    value.required("definition")?,
                    self.operations.admission.definition(),
                )?,
            "chat_definition_mismatch",
            "The stored Main belongs to a different ChatBridge configuration.",
        )?;
        let queue = array(value, "queue")?;
        let mut inputs = HashSet::new();
        let mut identities = HashSet::new();
        let mut ordered = true;
        for (index, ticket) in queue.iter().enumerate() {
            inputs.insert(js(ticket, "inputId")?.clone());
            identities.insert(hash_json(ticket.required("identity")?)?);
            ordered &= number(ticket, "sequence")? < number(value, "nextSequence")?
                && (index == 0
                    || number(ticket, "sequence")? > number(&queue[index - 1], "sequence")?);
        }
        require(
            inputs.len() == queue.len() && identities.len() == queue.len() && ordered,
            "chat_identity_conflict",
            "The saved Main queue contains duplicate or out-of-order identities.",
        )?;
        if let Some(publication) = value.get("publication").filter(|v| !(v).is_null()) {
            let first = number(publication, "firstSequence")?;
            let end = first + number(publication, "partCount")?;
            require(
                queue.first().and_then(|v| v.get("inputId")) == publication.get("inputId")
                    && number(publication, "publishedParts")? <= number(publication, "partCount")?
                    && safe_integer(end)
                    && end <= number(value, "nextSequence")?
                    && queue.iter().all(|ticket| {
                        ticket
                            .get("sequence")
                            .and_then(Json::as_f64)
                            .is_some_and(|n| n < first || n >= end)
                    }),
                "chat_identity_conflict",
                "Result publication must reserve its complete sequence range for the current queue head.",
            )?;
        }
        Ok(main)
    }
    async fn original_input(
        &self,
        identity: &Json,
        request_hash: &str,
    ) -> IvyResult<Option<Document>> {
        let admission = self.operations.admission;
        let input = self
            .operations
            .store
            .named(
                "chat-bridge/input",
                &input_name(admission.definition(), identity, None)?,
            )
            .await
            .map_err(|e| e.into_ivy())?;
        if let Some(input) = &input {
            require(
                input.value.get("workspaceId") == admission.definition().get("workspaceId")
                    && input.value.get("definitionHash")
                        == Some(&Json::from(admission.definition_hash()))
                    && same(input.value.required("identity")?, identity)?
                    && input.value.get("requestHash") == Some(&Json::from(request_hash)),
                "mutation_conflict",
                "The original message identity already names different input or a different Main binding.",
            )?;
        }
        Ok(input)
    }
    async fn binding(&self, selected: &Pin) -> IvyResult<()> {
        let binding = self
            .operations
            .store
            .read("chat-bridge/binding", Reference::Pinned(selected))
            .await
            .map_err(|e| e.into_ivy())?
            .value;
        let definition = self.operations.admission.definition();
        require(
            binding.get("workspaceId") == definition.get("workspaceId")
                && binding.get("definitionHash")
                    == Some(&Json::from(self.operations.admission.definition_hash()))
                && same(
                    binding.required("project")?,
                    definition.required("project")?,
                )?
                && same(
                    binding.required("nativePlan")?,
                    definition.required("nativePlan")?,
                )?
                && binding.required("primary")?.get("serviceNodeId")
                    == definition.required("project")?.get("serviceNodeId")
                && binding.required("primary")?.get("namespace") == Some(&Json::from("codex"))
                && binding.required("primary")?.get("kind") == Some(&Json::from("thread")),
            "chat_binding_mismatch",
            "Main must retain the exact configured native project, plan and owning thread.",
        )
    }
    async fn selected_binding(&self, caller: &JsString, request: &Json) -> IvyResult<Pin> {
        if let Some(value) = request.get("expectedBinding").filter(|v| !(v).is_null()) {
            return pin(value);
        }
        require(
            self.ensure_binding.is_some(),
            "chat_main_missing",
            "This message ingress cannot create its first Main.",
        )?;
        self.ensure_binding
            .unwrap()
            .ensure_binding(caller, js(request, "operationId")?)
            .await
    }
    async fn write_main(
        &self,
        current: &Document,
        value: &Json,
        scope: &str,
        step: &str,
    ) -> IvyResult<Document> {
        let pin = self
            .operations
            .store
            .write(
                "chat-bridge/main",
                value,
                &mutation(
                    scope,
                    &format!("{step}:{}:{}", current.pin.revision, hash_json(value)?),
                )?,
                Destination::Update {
                    object_id: &current.pin.object_id,
                    expected_revision: current.pin.revision,
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        self.operations
            .store
            .read("chat-bridge/main", Reference::Pinned(&pin))
            .await
            .map_err(|e| e.into_ivy())
    }
    async fn release(&self, main: &Document, accepted: &Document, step: &str) -> IvyResult<()> {
        let mut value = main.value.clone();
        value.insert("pendingAction", Json::Null);
        value.insert("updatedAt", accepted.value.required("createdAt")?.clone());
        self.write_main(main, &value, &accepted.pin.object_id, step)
            .await?;
        Ok(())
    }
    /// Recover the exact pending send, including writes whose reply was lost.
    pub async fn recover_pending(&self) -> IvyResult<()> {
        for _ in 0..32 {
            let mut main = self.main().await?;
            let Some(pending) = main.value.get("pendingAction").filter(|v| !(v).is_null()) else {
                return Ok(());
            };
            let accepted = self
                .operations
                .store
                .read("chat-bridge/operation", Reference::Pinned(&pin(pending)?))
                .await
                .map_err(|e| e.into_ivy())?;
            self.operations.check(&accepted.value)?;
            let request = accepted.value.required("request")?;
            require(
                request.get("action") == Some(&Json::from("send")),
                "chat_pending_action",
                "Another Main action needs its own recovery before new input.",
            )?;
            let caller = js(&accepted.value, "callerPrincipalId")?;
            let admitted = self.operations.admission.send(caller, request)?;
            let scope = &accepted.pin.object_id;
            let selected = self.selected_binding(caller, request).await?;
            let mut input = match self
                .original_input(&admitted.identity, &admitted.request_hash)
                .await
            {
                Ok(input) => input,
                Err(error) if error.code == "mutation_conflict" => {
                    let winner = self
                        .operations
                        .store
                        .named(
                            "chat-bridge/input",
                            &input_name(
                                self.operations.admission.definition(),
                                &admitted.identity,
                                None,
                            )?,
                        )
                        .await
                        .map_err(|e| e.into_ivy())?;
                    let valid = match winner {
                        Some(winner) => {
                            winner.value.get("operationId") != accepted.value.get("operationId")
                                && same(winner.value.required("identity")?, &admitted.identity)?
                        }
                        None => false,
                    };
                    require(
                        valid,
                        "chat_identity_conflict",
                        "An original input conflict needs explicit integrity recovery.",
                    )?;
                    let refused = async {
                        self.operations
                            .reject_unapplied(
                                scope,
                                "mutation_conflict",
                                "The original message identity names different accepted content.",
                            )
                            .await?;
                        self.release(&main, &accepted, "release-conflict").await
                    }
                    .await;
                    match refused {
                        Ok(()) => return Ok(()),
                        Err(e) if contention(&e) => continue,
                        Err(e) => return Err(e),
                    }
                }
                Err(error) => return Err(error),
            };
            let current = self
                .operations
                .find(caller, js(&accepted.value, "operationId")?)
                .await?;
            require(
                current.is_some(),
                "chat_identity_conflict",
                "The claimed operation must remain discoverable.",
            )?;
            let current = current.unwrap();
            if current.value.get("phase") == Some(&Json::from("succeeded")) {
                let valid = match &input {
                    Some(input) => match current.value.get("outcome").filter(|v| !v.is_null()) {
                        Some(outcome) => {
                            outcome.get("action") == Some(&Json::from("send"))
                                && same(outcome.required("input")?, &input_view(input))?
                        }
                        None => false,
                    },
                    None => false,
                };
                require(
                    valid,
                    "chat_identity_conflict",
                    "The completed admission must retain its exact original input receipt.",
                )?;
                match self.release(&main, &accepted, "release-completed").await {
                    Ok(()) => return Ok(()),
                    Err(e) if contention(&e) => continue,
                    Err(e) => return Err(e),
                }
            }
            let changed = !same(main.value.required("binding")?, &selected.json())?;
            let failed = current.value.get("phase") == Some(&Json::from("failed"));
            if input.is_none()
                && (failed
                    || changed
                    || array(&main.value, "queue")?.len() >= 64
                    || !safe_integer(number(&main.value, "nextSequence")? + 1.0))
            {
                let refused = async {
                    if !failed {
                        self.operations
                            .reject_unapplied(
                                scope,
                                if changed {
                                    "chat_binding_mismatch"
                                } else {
                                    "chat_queue_full"
                                },
                                if changed {
                                    "Main changed before admission."
                                } else {
                                    "The Main execution queue is full."
                                },
                            )
                            .await?;
                    } else {
                        require(
                            current.value.get("error").and_then(|v| v.get("outcome"))
                                == Some(&Json::from("not_executed")),
                            "chat_operation_unresolved",
                            "An uncertain failed action cannot release its claim.",
                        )?;
                    }
                    self.release(&main, &accepted, "release-refusal").await
                }
                .await;
                match refused {
                    Ok(()) => return Ok(()),
                    Err(e) if contention(&e) => continue,
                    Err(e) => return Err(e),
                }
            }
            // An alias consumes the original receipt even if its old ticket has
            // already been removed by the native worker.
            if let Some(original) = &input {
                if original.value.get("operationId") != accepted.value.get("operationId") {
                    let owner = self
                        .operations
                        .find(caller, js(&original.value, "operationId")?)
                        .await?;
                    let owner = owner.as_ref().map(|v| &v.value);
                    require(
                        owner.and_then(|v| v.get("phase")) == Some(&Json::from("succeeded"))
                            && owner
                                .and_then(|v| v.get("outcome"))
                                .and_then(|v| v.get("action"))
                                == Some(&Json::from("send"))
                            && owner
                                .and_then(|v| v.get("outcome"))
                                .and_then(|v| v.get("input"))
                                .and_then(|v| v.get("object"))
                                .and_then(|v| v.get("objectId"))
                                == Some(&Json::from(original.pin.object_id.clone())),
                        "chat_publication_pending",
                        "The original message admission must finish before an alias action.",
                    )?;
                    let aliased = async {
                        self.operations
                            .finish(scope, &send_outcome(&accepted.value, original)?)
                            .await?;
                        self.release(&main, &accepted, "release-alias").await
                    }
                    .await;
                    match aliased {
                        Ok(()) => return Ok(()),
                        Err(e) if contention(&e) => continue,
                        Err(e) => return Err(e),
                    }
                }
            }
            require(
                !changed,
                "chat_binding_mismatch",
                "A pending send cannot change its selected Main.",
            )?;
            self.binding(&selected).await?;
            let mut ticket = None;
            for value in array(&main.value, "queue")? {
                if same(value.required("identity")?, &admitted.identity)? {
                    ticket = Some(value.clone());
                    break;
                }
            }
            let publish = async {
                if let Some(ticket) = ticket {
                    let valid = match &input {
                        Some(input) => {
                            ticket.get("inputId") == Some(&Json::from(input.pin.object_id.clone()))
                                && ticket.get("requestHash")
                                    == Some(&Json::from(admitted.request_hash.clone()))
                                && same(
                                    ticket.required("binding")?,
                                    input.value.required("binding")?,
                                )?
                                && ticket.get("sequence") == input.value.get("sequence")
                                && ticket.get("admittedAt") == input.value.get("admittedAt")
                        }
                        None => false,
                    };
                    require(
                        valid,
                        "chat_identity_conflict",
                        "The queued ticket differs from its original input.",
                    )?;
                } else {
                    require(
                        array(&main.value, "queue")?.len() < 64
                            && safe_integer(number(&main.value, "nextSequence")? + 1.0),
                        "chat_queue_full",
                        "The Main execution queue cannot accept another input.",
                    )?;
                    if input.is_none() {
                        let at = accepted.value.required("createdAt")?;
                        let value = Json::object([
                            ("schemaVersion", Json::Number(1.0)),
                            (
                                "workspaceId",
                                self.operations
                                    .admission
                                    .definition()
                                    .required("workspaceId")?
                                    .clone(),
                            ),
                            (
                                "definitionHash",
                                self.operations.admission.definition_hash().into(),
                            ),
                            ("identity", admitted.identity.clone()),
                            (
                                "operationId",
                                accepted.value.required("operationId")?.clone(),
                            ),
                            ("requestHash", admitted.request_hash.clone().into()),
                            ("binding", selected.json()),
                            ("sequence", main.value.required("nextSequence")?.clone()),
                            ("payload", request.required("payload")?.clone()),
                            ("state", "queued".into()),
                            (
                                "nativeCalls",
                                Json::object([
                                    ("resume", Json::Null),
                                    ("turn", Json::Null),
                                    ("interrupt", Json::Null),
                                ]),
                            ),
                            ("turnId", Json::Null),
                            ("epoch", Json::Null),
                            ("result", Json::Null),
                            ("error", Json::Null),
                            ("cancellation", Json::Null),
                            ("admittedAt", at.clone()),
                            ("updatedAt", at.clone()),
                            ("finishedAt", Json::Null),
                        ]);
                        let name = input_name(
                            self.operations.admission.definition(),
                            &admitted.identity,
                            None,
                        )?;
                        let pin = self
                            .operations
                            .store
                            .write(
                                "chat-bridge/input",
                                &value,
                                &mutation(scope, "publish-input")?,
                                Destination::Create {
                                    parent_id: self.operations.store.root_object_id.as_deref(),
                                    name: &name,
                                },
                            )
                            .await
                            .map_err(|e| e.into_ivy())?;
                        input = Some(
                            self.operations
                                .store
                                .read("chat-bridge/input", Reference::Pinned(&pin))
                                .await
                                .map_err(|e| e.into_ivy())?,
                        );
                    }
                    let original = input.as_ref().unwrap();
                    require(
                        original.value.get("sequence") == main.value.get("nextSequence")
                            && original.value.get("operationId")
                                == accepted.value.get("operationId"),
                        "chat_identity_conflict",
                        "An unfinished input cannot consume another admission sequence.",
                    )?;
                    let mut next = main.value.clone();
                    next.insert(
                        "nextSequence",
                        Json::Number(number(&main.value, "nextSequence")? + 1.0),
                    );
                    next.insert("updatedAt", accepted.value.required("createdAt")?.clone());
                    let mut queue = array(&main.value, "queue")?.to_vec();
                    queue.push(Json::object([
                        ("inputId", original.pin.object_id.clone().into()),
                        ("identity", admitted.identity.clone()),
                        ("requestHash", admitted.request_hash.clone().into()),
                        ("binding", selected.json()),
                        ("sequence", original.value.required("sequence")?.clone()),
                        ("admittedAt", original.value.required("admittedAt")?.clone()),
                    ]));
                    next.insert("queue", Json::Array(queue));
                    main = self
                        .write_main(&main, &next, scope, "publish-ticket")
                        .await?;
                }
                require(
                    input.is_some(),
                    "chat_identity_conflict",
                    "A queued input must exist before completing admission.",
                )?;
                self.operations
                    .finish(
                        scope,
                        &send_outcome(&accepted.value, input.as_ref().unwrap())?,
                    )
                    .await?;
                // Retain the claim until both the queue and original receipt commit.
                self.release(&main, &accepted, "release-claim").await
            }
            .await;
            match publish {
                Ok(()) => return Ok(()),
                Err(e) if contention(&e) => (),
                Err(e) => return Err(e),
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "Main changed repeatedly. Recover its original pending action.",
        )
        .with_outcome(Outcome::Unknown))
    }
    pub async fn send(&self, caller: &JsString, original: &Json) -> IvyResult<Json> {
        let admitted = self.operations.admission.send(caller, original)?;
        let request = original.clone();
        // Known stale/full/missing Main is refused before retaining a new action.
        // A later race is refused durably under the Main claim.
        if self
            .operations
            .find(caller, js(&request, "operationId")?)
            .await?
            .is_none()
        {
            let selected = self.selected_binding(caller, &request).await?;
            self.recover_pending().await?;
            if self
                .original_input(&admitted.identity, &admitted.request_hash)
                .await?
                .is_none()
            {
                let main = self.main().await?;
                require(
                    same(main.value.required("binding")?, &selected.json())?,
                    "chat_binding_mismatch",
                    "The displayed Main binding has changed.",
                )?;
                require(
                    array(&main.value, "queue")?.len() < 64
                        && safe_integer(number(&main.value, "nextSequence")? + 1.0),
                    "chat_queue_full",
                    "The Main execution queue is full.",
                )?;
                self.binding(&selected).await?;
            }
        }
        let operation = self.operations.begin(caller, &request).await?;
        if terminal(&operation.value) {
            return Ok(operation.value);
        }
        require(
            operation.value.get("phase") == Some(&Json::from("accepted"))
                || operation.value.get("phase") == Some(&Json::from("applying")),
            "chat_operation_unresolved",
            "Recover the original action before accepting another effect.",
        )?;
        for _ in 0..32 {
            self.recover_pending().await?;
            if let Some(recovered) = self
                .operations
                .find(caller, js(&request, "operationId")?)
                .await?
            {
                if terminal(&recovered.value) {
                    return Ok(recovered.value);
                }
            }
            let main = self.main().await?;
            if main
                .value
                .get("pendingAction")
                .is_some_and(|value| !value.is_null())
            {
                continue;
            }
            let mut value = main.value.clone();
            value.insert("pendingAction", operation.pin.json());
            value.insert("updatedAt", operation.value.required("createdAt")?.clone());
            match self
                .write_main(&main, &value, &operation.pin.object_id, "claim")
                .await
            {
                Ok(_) => (),
                Err(e) if contention(&e) => (),
                Err(e) => return Err(e),
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "Main changed repeatedly. Recover the original input operation.",
        )
        .with_outcome(Outcome::Unknown))
    }
}
fn input_view(input: &Document) -> Json {
    Json::object([("object", input.pin.json()), ("data", input.value.clone())])
}
fn send_outcome(accepted: &Json, input: &Document) -> IvyResult<Json> {
    Ok(Json::object([
        ("action", "send".into()),
        ("operationId", accepted.required("operationId")?.clone()),
        ("input", input_view(input)),
    ]))
}
fn terminal(operation: &Json) -> bool {
    operation.get("phase") == Some(&Json::from("succeeded"))
        || operation.get("phase") == Some(&Json::from("failed"))
}
