//! Durable service context for Main. Only Main's native result is user-visible.
use crate::{
    notice_input::{NoticeInstructions, admitted_notice_input},
    operations::{ChatOperations, contention},
    outbox::ChatOutbox,
    queue::{ChatQueue, input_name},
    store::{Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, chat_schema::validate_chat,
    hash::hash_json, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::main_chat::main_notification;
use std::{future::Future, pin::Pin as FuturePin};

pub type MainFuture<'a> = FuturePin<Box<dyn Future<Output = IvyResult<()>> + Send + 'a>>;
/// Main owns binding creation and recovery dispatch. Notice recovery itself
/// uses the existing operation journal and queue, without starting a native turn.
pub trait NoticeMain: Send + Sync {
    fn ensure_notice<'a>(
        &'a self,
        caller: &'a JsString,
        operation_id: &'a JsString,
    ) -> MainFuture<'a>;
    fn recover_pending(&self) -> MainFuture<'_>;
    fn verify_owner(&self) -> MainFuture<'_>;
}
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
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
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
fn optional<'a>(value: &'a Json, key: &str) -> &'a Json {
    static MISSING: Json = Json::Null;
    value.get(key).unwrap_or(&MISSING)
}
fn present(value: &Json) -> bool {
    crate::main_context::truthy(value)
}
fn safe_integer(n: f64) -> bool {
    n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0
}
fn capacity(main: &Json) -> IvyResult<bool> {
    Ok(array(main, "queue")?.len() < 64 && safe_integer(number(main, "nextSequence")? + 1.0))
}
fn pin(value: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: value.required("objectId")?.as_str()?,
        revision: number(value, "revision")? as i64,
    })
}
fn terminal(operation: &Json) -> bool {
    ["succeeded", "failed", "outcome_unknown"]
        .iter()
        .any(|v| operation.get("phase") == Some(&Json::from(*v)))
}
fn view(document: Document) -> Json {
    Json::object([("object", document.pin.json()), ("data", document.value)])
}

pub struct ChatNotices<'a> {
    pub operations: &'a ChatOperations<'a>,
    main: &'a dyn NoticeMain,
    instructions: &'a NoticeInstructions,
}
impl<'a> ChatNotices<'a> {
    pub fn new(
        operations: &'a ChatOperations<'a>,
        main: &'a dyn NoticeMain,
        instructions: &'a NoticeInstructions,
    ) -> Self {
        Self {
            operations,
            main,
            instructions,
        }
    }
    fn queue(&self) -> ChatQueue<'_> {
        ChatQueue::new(self.operations, None)
    }
    async fn input(&self, operation: &Document) -> IvyResult<Option<Document>> {
        let value = &operation.value;
        let request = value.required("request")?;
        require(
            request.get("action") == Some(&Json::from("notify")),
            "chat_identity_conflict",
            "A service handoff needs its original notice operation.",
        )?;
        let identity = Json::object([
            ("channel", request.required("channel")?.clone()),
            (
                "senderPrincipalId",
                value.required("callerPrincipalId")?.clone(),
            ),
            ("messageId", request.required("operationId")?.clone()),
        ]);
        let name = input_name(
            self.operations.admission.definition(),
            &identity,
            Some(&value.required("definitionHash")?.as_str()?),
        )?;
        let input = self
            .operations
            .store
            .named("chat-bridge/input", &name)
            .await
            .map_err(|e| e.into_ivy())?;
        if let Some(input) = &input {
            let admitted = admitted_notice_input(
                self.operations.admission,
                string(value, "callerPrincipalId")?,
                request,
                input.value.required("binding")?,
                self.instructions,
            )?;
            require(
                input.value.get("workspaceId") == value.get("workspaceId")
                    && input.value.get("definitionHash") == value.get("definitionHash")
                    && same(input.value.required("identity")?, &admitted.identity)?
                    && input.value.get("operationId") == value.get("operationId")
                    && input.value.get("requestHash") == Some(&Json::from(admitted.request_hash))
                    && same(input.value.required("payload")?, &admitted.payload)?,
                "chat_identity_conflict",
                "The retained service handoff differs from its original notice operation.",
            )?;
        }
        Ok(input)
    }
    pub async fn notify(&self, caller: &JsString, original: &Json) -> IvyResult<Json> {
        validate_chat("NotifyRequest", original)?;
        let previous = self
            .operations
            .find(caller, string(original, "operationId")?)
            .await?;
        if let Some(previous) = &previous {
            require(
                previous.value.required("request")?.get("action") == Some(&Json::from("notify"))
                    && same(
                        &main_notification(previous.value.required("request")?),
                        original,
                    )?,
                "mutation_conflict",
                "This service operation already names different notification content.",
            )?;
            if terminal(&previous.value) || self.input(previous).await?.is_some() {
                return Ok(previous.value.clone());
            }
        }
        let request = match &previous {
            Some(previous)
                if previous.value.required("request")?.get("action")
                    == Some(&Json::from("notify")) =>
            {
                previous.value.required("request")?.clone()
            }
            _ => {
                let mut request = original.clone();
                request.insert("expectedBridge", self.operations.admission.expected(caller));
                request.insert(
                    "channel",
                    array(self.operations.admission.definition(), "channels")?[0]
                        .required("channel")?
                        .clone(),
                );
                request
            }
        };
        self.main.verify_owner().await?;
        if self
            .operations
            .find(caller, string(&request, "operationId")?)
            .await?
            .is_none()
        {
            self.main
                .ensure_notice(caller, string(&request, "operationId")?)
                .await?;
            self.main.recover_pending().await?;
            let main = self.queue().main().await?;
            require(
                present(optional(&main.value, "binding")),
                "chat_main_missing",
                "The automatic Main creation did not publish a binding.",
            )?;
            require(
                capacity(&main.value)?,
                "chat_queue_full",
                "The Main execution queue cannot accept another service handoff.",
            )?;
            self.operations
                .store
                .require_available(&pin(request.required("source")?)?)
                .await
                .map_err(|e| e.into_ivy())?;
        }
        let accepted = self.operations.begin(caller, &request).await?;
        for _ in 0..32 {
            self.main.recover_pending().await?;
            let operation = self
                .operations
                .find(caller, string(&request, "operationId")?)
                .await?;
            require(
                operation.is_some(),
                "chat_identity_conflict",
                "The original notice operation must remain discoverable.",
            )?;
            let operation = operation.unwrap();
            if terminal(&operation.value) {
                return Ok(operation.value);
            }
            require(
                operation.value.get("phase") == Some(&Json::from("accepted")),
                "chat_operation_unresolved",
                "Recover the original service handoff.",
            )?;
            if self.input(&operation).await?.is_some() {
                return Ok(operation.value);
            }
            let main = self.queue().main().await?;
            if present(optional(&main.value, "pendingAction")) {
                continue;
            }
            let mut next = main.value.clone();
            next.insert("pendingAction", accepted.pin.json());
            next.insert("updatedAt", accepted.value.required("createdAt")?.clone());
            let run = async {
                self.main.verify_owner().await?;
                self.operations
                    .store
                    .write(
                        "chat-bridge/main",
                        &next,
                        &mutation(
                            &accepted.pin.object_id,
                            &format!("notice-claim:{}:{}", main.pin.revision, hash_json(&next)?),
                        )?,
                        Destination::Update {
                            object_id: &main.pin.object_id,
                            expected_revision: main.pin.revision,
                        },
                    )
                    .await
                    .map_err(|e| e.into_ivy())?;
                Ok::<_, IvyError>(())
            }
            .await;
            if let Err(error) = run {
                if !contention(&error) {
                    return Err(error);
                }
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "Recover the original service handoff after concurrent Main actions.",
        )
        .with_outcome(Outcome::Unknown))
    }
    pub async fn recover_pending(&self) -> IvyResult<()> {
        for _ in 0..32 {
            let mut main = self.queue().main().await?;
            if !present(optional(&main.value, "pendingAction")) {
                return Ok(());
            }
            let accepted = self
                .operations
                .store
                .read(
                    "chat-bridge/operation",
                    Reference::Pinned(&pin(main.value.required("pendingAction")?)?),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            self.operations.check(&accepted.value)?;
            let request = accepted.value.required("request")?;
            let scope = &accepted.pin.object_id;
            require(
                accepted.value.get("phase") == Some(&Json::from("accepted"))
                    && request.get("action") == Some(&Json::from("notify")),
                "chat_pending_action",
                "Service handoff recovery requires its original accepted Main claim.",
            )?;
            let mut input = self.input(&accepted).await?;
            let run = async {
                if input.is_none()
                    && (!present(optional(&main.value, "binding")) || !capacity(&main.value)?)
                {
                    let binding = present(optional(&main.value, "binding"));
                    self.operations
                        .reject_unapplied(
                            scope,
                            if binding {
                                "chat_queue_full"
                            } else {
                                "chat_main_missing"
                            },
                            if binding {
                                "The Main execution queue is full."
                            } else {
                                "No current Main binding can receive the service handoff."
                            },
                        )
                        .await?;
                    main = self.queue().main().await?;
                    let mut next = main.value.clone();
                    next.insert("pendingAction", Json::Null);
                    next.insert("updatedAt", accepted.value.required("createdAt")?.clone());
                    self.operations
                        .store
                        .write(
                            "chat-bridge/main",
                            &next,
                            &mutation(
                                scope,
                                &format!("notice-release-refusal:{}", main.pin.revision),
                            )?,
                            Destination::Update {
                                object_id: &main.pin.object_id,
                                expected_revision: main.pin.revision,
                            },
                        )
                        .await
                        .map_err(|e| e.into_ivy())?;
                    return Ok(());
                }
                require(
                    present(optional(&main.value, "binding")),
                    "chat_main_missing",
                    "A service handoff needs the current Main binding.",
                )?;
                let binding = main.value.required("binding")?.clone();
                let admitted = admitted_notice_input(
                    self.operations.admission,
                    string(&accepted.value, "callerPrincipalId")?,
                    request,
                    &binding,
                    self.instructions,
                )?;
                let mut ticket = None;
                for item in array(&main.value, "queue")? {
                    if same(item.required("identity")?, &admitted.identity)? {
                        ticket = Some(item);
                        break;
                    }
                }
                if let Some(ticket) = ticket {
                    let matches = if let Some(input) = &input {
                        ticket.get("inputId") == Some(&Json::from(input.pin.object_id.clone()))
                            && ticket.get("requestHash")
                                == Some(&Json::from(admitted.request_hash.clone()))
                            && same(
                                ticket.required("binding")?,
                                input.value.required("binding")?,
                            )?
                            && ticket.get("sequence") == input.value.get("sequence")
                            && ticket.get("admittedAt") == input.value.get("admittedAt")
                    } else {
                        false
                    };
                    require(
                        matches,
                        "chat_identity_conflict",
                        "The queued service handoff differs from its original input.",
                    )?;
                } else {
                    require(
                        capacity(&main.value)?,
                        "chat_queue_full",
                        "The Main execution queue cannot accept another service handoff.",
                    )?;
                    if input.is_none() {
                        let at = accepted.value.required("createdAt")?.clone();
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
                            ("binding", binding.clone()),
                            ("sequence", main.value.required("nextSequence")?.clone()),
                            ("payload", admitted.payload.clone()),
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
                            ("updatedAt", at),
                            ("finishedAt", Json::Null),
                        ]);
                        let name = input_name(
                            self.operations.admission.definition(),
                            &admitted.identity,
                            None,
                        )?;
                        let created = self
                            .operations
                            .store
                            .write(
                                "chat-bridge/input",
                                &value,
                                &mutation(scope, "publish-notice-input")?,
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
                                .read("chat-bridge/input", Reference::Pinned(&created))
                                .await
                                .map_err(|e| e.into_ivy())?,
                        );
                    }
                    let input = input.as_ref().unwrap();
                    require(
                        input.value.get("sequence") == main.value.get("nextSequence")
                            && input.value.get("operationId") == accepted.value.get("operationId"),
                        "chat_identity_conflict",
                        "An unfinished service handoff cannot consume another admission sequence.",
                    )?;
                    let mut next = main.value.clone();
                    next.insert(
                        "nextSequence",
                        Json::Number(number(&main.value, "nextSequence")? + 1.0),
                    );
                    next.insert("updatedAt", accepted.value.required("createdAt")?.clone());
                    let mut queue = array(&main.value, "queue")?.to_vec();
                    queue.push(Json::object([
                        ("inputId", input.pin.object_id.clone().into()),
                        ("identity", admitted.identity),
                        ("requestHash", admitted.request_hash.into()),
                        ("binding", binding),
                        ("sequence", input.value.required("sequence")?.clone()),
                        ("admittedAt", input.value.required("admittedAt")?.clone()),
                    ]));
                    next.insert("queue", Json::Array(queue));
                    let updated = self
                        .operations
                        .store
                        .write(
                            "chat-bridge/main",
                            &next,
                            &mutation(
                                scope,
                                &format!(
                                    "publish-notice-ticket:{}:{}",
                                    main.pin.revision,
                                    hash_json(&next)?
                                ),
                            )?,
                            Destination::Update {
                                object_id: &main.pin.object_id,
                                expected_revision: main.pin.revision,
                            },
                        )
                        .await
                        .map_err(|e| e.into_ivy())?;
                    main = self
                        .operations
                        .store
                        .read("chat-bridge/main", Reference::Pinned(&updated))
                        .await
                        .map_err(|e| e.into_ivy())?;
                }
                let mut next = main.value.clone();
                next.insert("pendingAction", Json::Null);
                next.insert("updatedAt", accepted.value.required("createdAt")?.clone());
                self.operations
                    .store
                    .write(
                        "chat-bridge/main",
                        &next,
                        &mutation(
                            scope,
                            &format!("notice-release-claim:{}", main.pin.revision),
                        )?,
                        Destination::Update {
                            object_id: &main.pin.object_id,
                            expected_revision: main.pin.revision,
                        },
                    )
                    .await
                    .map_err(|e| e.into_ivy())?;
                Ok::<_, IvyError>(())
            }
            .await;
            match run {
                Ok(()) => return Ok(()),
                Err(error) if contention(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "The original service handoff still needs queue recovery.",
        )
        .with_outcome(Outcome::Unknown))
    }
    pub async fn notice(&self, caller: &JsString, request: &Json) -> IvyResult<Json> {
        validate_chat("NoticeQuery", request)?;
        let operation = self
            .operations
            .find(caller, string(request, "operationId")?)
            .await?;
        require(
            operation
                .as_ref()
                .and_then(|o| o.value.get("request"))
                .and_then(|r| r.get("action"))
                == Some(&Json::from("notify")),
            "not_found",
            "This producer has no notice with this original identity.",
        )?;
        let operation = operation.unwrap();
        if operation.value.get("phase") == Some(&Json::from("failed")) {
            let error = optional(&operation.value, "error");
            return Err(IvyError::new_js(
                optional(error, "code")
                    .as_js_string()
                    .cloned()
                    .unwrap_or_else(|| "chat_operation_failed".into()),
                optional(error, "detail")
                    .as_js_string()
                    .cloned()
                    .unwrap_or_else(|| "The service handoff failed.".into()),
            ));
        }
        let outcome = optional(&operation.value, "outcome");
        require(
            operation.value.get("phase") == Some(&Json::from("succeeded"))
                && outcome.get("action") == Some(&Json::from("notify")),
            "chat_publication_pending",
            "Main has not finished the original service handoff.",
        )?;
        let id = outcome
            .required("reply")?
            .required("object")?
            .required("objectId")?
            .as_str()?;
        let reply = ChatOutbox::new(self.operations)
            .reply(
                Reference::Current(&id),
                operation.value.required("request")?.required("channel")?,
            )
            .await?;
        let input = self.input(&operation).await?;
        require(
            input.as_ref().is_some_and(|i| {
                reply.value.get("origin").and_then(|o| o.get("kind")) == Some(&Json::from("native"))
                    && reply.value.get("origin").and_then(|o| o.get("inputId"))
                        == Some(&Json::from(i.pin.object_id.clone()))
            }),
            "chat_identity_conflict",
            "Notice delivery must be Main's answer to this exact service handoff.",
        )?;
        Ok(view(reply))
    }
    pub async fn finish_undelivered(&self, input: &Document) -> IvyResult<()> {
        let value = &input.value;
        let operation = self
            .operations
            .find(
                string(value.required("identity")?, "senderPrincipalId")?,
                string(value, "operationId")?,
            )
            .await?;
        require(
            operation.is_some(),
            "chat_identity_conflict",
            "An undelivered input needs its original operation.",
        )?;
        let operation = operation.unwrap();
        if operation.value.required("request")?.get("action") != Some(&Json::from("notify")) {
            return Ok(());
        }
        let original = self.input(&operation).await?;
        require(
            original.as_ref().is_some_and(|o| o.pin == input.pin)
                && present(optional(value, "finishedAt"))
                && !present(optional(value, "turnId"))
                && !present(optional(value, "result"))
                && ["failed", "cancelled"]
                    .iter()
                    .any(|state| value.get("state") == Some(&Json::from(*state))),
            "chat_identity_conflict",
            "Only an unchanged terminal undelivered handoff can fail its producer receipt.",
        )?;
        let error = value
            .get("error")
            .filter(|e| !e.is_null())
            .cloned()
            .unwrap_or_else(|| {
                Json::object([
                    ("code", "chat_notice_cancelled".into()),
                    (
                        "detail",
                        "The service handoff was cancelled before its native turn.".into(),
                    ),
                    ("outcome", "not_executed".into()),
                ])
            });
        if operation.value.get("phase") == Some(&Json::from("failed")) {
            return require(
                same(operation.value.required("error")?, &error)?,
                "chat_identity_conflict",
                "The original handoff failure cannot change.",
            );
        }
        require(
            operation.value.get("phase") == Some(&Json::from("accepted")),
            "chat_operation_unresolved",
            "An undelivered handoff must retain its original accepted operation.",
        )?;
        let mut next = operation.value.clone();
        next.insert("phase", "failed".into());
        next.insert("error", error);
        next.insert("updatedAt", value.required("finishedAt")?.clone());
        self.operations
            .store
            .write(
                "chat-bridge/operation",
                &next,
                &mutation(
                    &operation.pin.object_id,
                    &format!("undelivered:{}", hash_json(&next)?),
                )?,
                Destination::Update {
                    object_id: &operation.pin.object_id,
                    expected_revision: operation.pin.revision,
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        Ok(())
    }
}
