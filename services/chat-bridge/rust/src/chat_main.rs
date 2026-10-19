//! Shared Main creation and replacement. Retain the original claim and native
//! attachment before I/O; publish its binding and receipt before releasing it.
use crate::{
    admission::{ChatAdmission, expected_bridge},
    cancellation::ChatCancellation,
    main_context::truthy,
    native_driver::{ChatNativeDriver, NativeOrigin, native_path},
    notice_input::NoticeInstructions,
    notices::{ChatNotices, MainFuture, NoticeMain},
    operations::{ChatOperations, contention},
    queue::{BindingFuture, ChatQueue, EnsureBinding, conversation_name, main_name},
    store::{ChatStore, Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, chat_schema::validate_chat,
    hash::hash_json, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
    operation::derive_operation_id_js,
};
use ivy_sdk::native_owner::{GuardFuture, NativeDispatchGuard};

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
fn pin(value: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: value.required("objectId")?.as_str()?,
        revision: value
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    })
}
fn present(value: &Json, key: &str) -> bool {
    value.get(key).is_some_and(truthy)
}
fn null<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn state(value: &Json, key: &str, choices: &[&str]) -> bool {
    choices
        .iter()
        .any(|choice| value.get(key) == Some(&Json::from(*choice)))
}
fn empty_queue(value: &Json) -> IvyResult<bool> {
    Ok(value
        .required("queue")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .is_empty())
}
fn binding_view(value: Document) -> Json {
    Json::object([("object", value.pin.json()), ("data", value.value)])
}
fn binding_name(id: &str) -> String {
    format!("Chat binding {id}")
}

pub struct ChatMain<'a> {
    pub operations: &'a ChatOperations<'a>,
    pub native: &'a ChatNativeDriver<'a>,
    instructions: &'a NoticeInstructions,
}
struct Claim {
    main: Document,
    operation: Document,
}
impl<'a> ChatMain<'a> {
    pub fn new(
        operations: &'a ChatOperations<'a>,
        native: &'a ChatNativeDriver<'a>,
        instructions: &'a NoticeInstructions,
    ) -> IvyResult<Self> {
        require(
            std::ptr::eq(operations.store, native.store)
                && same(
                    operations.admission.definition(),
                    native.admission().definition(),
                )?,
            "chat_scope_mismatch",
            "Main and native dispatch must share the same store and immutable configuration.",
        )?;
        Ok(Self {
            operations,
            native,
            instructions,
        })
    }
    pub fn store(&self) -> &ChatStore {
        self.operations.store
    }
    pub fn admission(&self) -> &ChatAdmission {
        self.operations.admission
    }
    pub fn queue(&self) -> ChatQueue<'_> {
        ChatQueue::new(self.operations, Some(self))
    }
    pub async fn find(&self) -> IvyResult<Option<Document>> {
        self.store()
            .named(
                "chat-bridge/main",
                &main_name(self.admission().definition())?,
            )
            .await
            .map_err(|e| e.into_ivy())?
            .map(|main| self.queue().check_main(main))
            .transpose()
    }
    async fn write_main(
        &self,
        main: &Document,
        next: &Json,
        scope: &str,
        step: &str,
    ) -> IvyResult<Document> {
        let saved = self
            .store()
            .write(
                "chat-bridge/main",
                next,
                &mutation(
                    scope,
                    &format!("{step}:{}:{}", main.pin.revision, hash_json(next)?),
                )?,
                Destination::Update {
                    object_id: &main.pin.object_id,
                    expected_revision: main.pin.revision,
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        self.store()
            .read("chat-bridge/main", Reference::Pinned(&saved))
            .await
            .map_err(|e| e.into_ivy())
    }
    fn check_available(&self, main: Option<&Document>, request: &Json) -> IvyResult<()> {
        let revision = main
            .map(|m| Json::Number(m.pin.revision as f64))
            .unwrap_or(Json::Null);
        require(
            request.get("expectedMainRevision") == Some(&revision),
            "revision_conflict",
            "Explicit Main creation requires the observed aggregate revision.",
        )?;
        require(
            !main.is_some_and(|m| present(&m.value, "pendingAction")),
            "chat_pending_action",
            "The original pending Main action must finish before replacement.",
        )?;
        require(
            match main {
                None => true,
                Some(main) => empty_queue(&main.value)?,
            },
            "chat_main_busy",
            "Main replacement requires an empty execution queue.",
        )
    }
    async fn resume_binding(&self, request: &Json) -> IvyResult<Option<Document>> {
        let Some(selected) = request.get("resumeMain").filter(|v| truthy(v)) else {
            return Ok(None);
        };
        let source = self
            .store()
            .read(
                "chat-bridge/main",
                Reference::Current(&selected.required("objectId")?.as_str()?),
            )
            .await
            .map_err(|e| e.into_ivy())?;
        let definition = self.admission().definition();
        require(
            same(&source.pin.json(), selected)?,
            "revision_conflict",
            "Main resumption requires the observed source revision.",
        )?;
        require(
            !present(&source.value, "pendingAction")
                && !present(&source.value, "publication")
                && empty_queue(&source.value)?,
            "chat_main_busy",
            "Main resumption requires an unoccupied source Main.",
        )?;
        let original = source.value.required("definition")?;
        require(
            source.value.get("definitionHash")
                != Some(&Json::from(self.admission().definition_hash()))
                && source.value.get("definitionHash") == Some(&hash_json(original)?.into())
                && original.get("principalId") == definition.get("principalId")
                && original.get("workspaceId") == definition.get("workspaceId")
                && original.get("rootObjectId") == definition.get("rootObjectId")
                && same(
                    original.required("project")?,
                    definition.required("project")?,
                )?,
            "chat_definition_mismatch",
            "Main resumption must retain the same principal, workspace and native project across configurations.",
        )?;
        require(
            present(&source.value, "binding"),
            "chat_binding_mismatch",
            "Main resumption requires an existing source binding.",
        )?;
        let binding = self
            .store()
            .read(
                "chat-bridge/binding",
                Reference::Pinned(&pin(source.value.required("binding")?)?),
            )
            .await
            .map_err(|e| e.into_ivy())?;
        let primary = binding.value.required("primary")?;
        require(
            binding.value.get("definitionHash") == source.value.get("definitionHash")
                && binding.value.get("workspaceId") == definition.get("workspaceId")
                && same(
                    binding.value.required("project")?,
                    definition.required("project")?,
                )?
                && same(
                    binding.value.required("nativePlan")?,
                    original.required("nativePlan")?,
                )?
                && primary.get("serviceNodeId")
                    == definition.required("project")?.get("serviceNodeId")
                && primary.get("namespace") == Some(&"codex".into())
                && primary.get("kind") == Some(&"thread".into()),
            "chat_binding_mismatch",
            "Main resumption must retain its original definition-owned native thread.",
        )?;
        Ok(Some(binding))
    }
    async fn conversation(&self, created_at: &Json) -> IvyResult<Pin> {
        let name = conversation_name(self.admission().definition())?;
        if let Some(existing) = self
            .store()
            .named("chat-bridge/conversation", &name)
            .await
            .map_err(|e| e.into_ivy())?
        {
            require(
                existing.value.get("workspaceId")
                    == self.admission().definition().get("workspaceId")
                    && existing.value.get("definitionHash")
                        == Some(&Json::from(self.admission().definition_hash())),
                "chat_definition_mismatch",
                "The retained conversation belongs to different ChatBridge settings.",
            )?;
            return Ok(existing.pin);
        }
        self.store()
            .write(
                "chat-bridge/conversation",
                &Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    (
                        "workspaceId",
                        self.admission()
                            .definition()
                            .required("workspaceId")?
                            .clone(),
                    ),
                    ("definitionHash", self.admission().definition_hash().into()),
                    ("createdAt", created_at.clone()),
                ]),
                &mutation(&name, "create")?,
                Destination::Create {
                    parent_id: self.store().root_object_id.as_deref(),
                    name: &name,
                },
            )
            .await
            .map_err(|e| e.into_ivy())
    }
    pub async fn create(&self, caller: &JsString, original: &Json) -> IvyResult<Json> {
        // Reader/notice/either retain identical admission under this contract.
        self.admission()
            .authorize(caller, expected_bridge(original)?, None)?;
        validate_chat("CreateMainRequest", original)?;
        let request = original.clone();
        self.native.verify_owner().await?;
        let operation_id = string(&request, "operationId")?;
        if self.operations.find(caller, operation_id).await?.is_none() {
            self.recover_pending().await?;
            self.check_available(self.find().await?.as_ref(), &request)?;
            self.resume_binding(&request).await?;
            self.native.verify_project().await?;
        }
        let accepted = self.operations.begin(caller, &request).await?;
        let scope = &accepted.pin.object_id;
        for _ in 0..32 {
            let current = self
                .operations
                .find(caller, operation_id)
                .await?
                .ok_or_else(|| {
                    IvyError::new(
                        "chat_identity_conflict",
                        "The original Main operation must remain discoverable.",
                    )
                })?;
            let main = self.find().await?;
            if main
                .as_ref()
                .and_then(|m| m.value.get("pendingAction"))
                .and_then(|p| p.get("objectId"))
                == Some(&scope.clone().into())
            {
                self.recover_pending().await?;
                return self
                    .operations
                    .find(caller, operation_id)
                    .await?
                    .map(|d| d.value)
                    .ok_or_else(IvyError::internal);
            }
            if state(&current.value, "phase", &["succeeded", "failed"]) {
                return Ok(current.value);
            }
            require(
                current.value.get("nativeCall") == Some(&Json::Null)
                    && state(&current.value, "phase", &["accepted"]),
                "chat_operation_unresolved",
                "An applied Main action cannot acquire a new claim.",
            )?;
            if let Err(error) = self.check_available(main.as_ref(), &request) {
                return Ok(self
                    .operations
                    .reject_unapplied(scope, &error.code, &error.message)
                    .await?
                    .value);
            }
            let claim = async {
                self.native.verify_owner().await?;
                if let Some(main) = main {
                    let mut next = main.value.clone();
                    next.insert("pendingAction", accepted.pin.json());
                    next.insert("updatedAt", accepted.value.required("createdAt")?.clone());
                    self.write_main(&main, &next, scope, "claim-main").await?;
                } else {
                    let definition = self.admission().definition();
                    let name = main_name(definition)?;
                    let conversation = self
                        .conversation(accepted.value.required("createdAt")?)
                        .await?;
                    self.store()
                        .write(
                            "chat-bridge/main",
                            &Json::object([
                                ("schemaVersion", Json::Number(1.0)),
                                ("definition", definition.clone()),
                                ("definitionHash", self.admission().definition_hash().into()),
                                ("conversation", conversation.json()),
                                ("binding", Json::Null),
                                ("pendingAction", accepted.pin.json()),
                                ("publication", Json::Null),
                                ("queue", Json::Array(vec![])),
                                ("nextSequence", Json::Number(1.0)),
                                ("updatedAt", accepted.value.required("createdAt")?.clone()),
                            ]),
                            &mutation(&name, "create")?,
                            Destination::Create {
                                parent_id: self.store().root_object_id.as_deref(),
                                name: &name,
                            },
                        )
                        .await
                        .map_err(|e| e.into_ivy())?;
                }
                Ok::<_, IvyError>(())
            }
            .await;
            if let Err(error) = claim {
                if !contention(&error) {
                    return Err(error);
                }
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "Main changed repeatedly; recover this original creation action.",
        )
        .with_outcome(Outcome::Unknown))
    }
    /// Select the current binding or idempotently create the first Main.
    pub async fn ensure(
        &self,
        caller: &JsString,
        parent_operation_id: &JsString,
    ) -> IvyResult<Json> {
        let expected = self.admission().expected(caller);
        let operation_id = derive_operation_id_js(
            parent_operation_id,
            &Json::object([
                ("action", "chat:auto-main".into()),
                ("caller", Json::String(caller.clone())),
            ]),
        )?;
        for _ in 0..8 {
            if let Some(prior) = self.operations.find(caller, &operation_id).await? {
                require(
                    prior.value.required("request")?.get("action") == Some(&"createMain".into()),
                    "chat_identity_conflict",
                    "Automatic Main creation retained another action.",
                )?;
                if state(&prior.value, "phase", &["succeeded"]) {
                    let outcome = null(&prior.value, "outcome");
                    require(
                        outcome.get("action") == Some(&"createMain".into()),
                        "chat_identity_conflict",
                        "Automatic Main creation needs its exact binding receipt.",
                    )?;
                    return Ok(outcome.required("binding")?.clone());
                }
                if state(&prior.value, "phase", &["failed"])
                    && null(&prior.value, "error").get("code") == Some(&"revision_conflict".into())
                {
                    let mut winner = self.find().await?;
                    if !winner
                        .as_ref()
                        .is_some_and(|m| present(&m.value, "binding"))
                    {
                        self.recover_pending().await?;
                        winner = self.find().await?;
                    }
                    require(
                        winner
                            .as_ref()
                            .is_some_and(|m| present(&m.value, "binding")),
                        "chat_contention",
                        "The concurrent automatic Main winner has not published its binding.",
                    )?;
                    let selected = pin(winner.unwrap().value.required("binding")?)?;
                    return Ok(binding_view(
                        self.store()
                            .read("chat-bridge/binding", Reference::Pinned(&selected))
                            .await
                            .map_err(|e| e.into_ivy())?,
                    ));
                }
                self.check_automatic_failure(&prior.value)?;
                self.recover_pending().await?;
                continue;
            }
            self.recover_pending().await?;
            let current = self.find().await?;
            if let Some(binding) = current
                .as_ref()
                .and_then(|m| m.value.get("binding"))
                .filter(|v| truthy(v))
            {
                return Ok(binding_view(
                    self.store()
                        .read("chat-bridge/binding", Reference::Pinned(&pin(binding)?))
                        .await
                        .map_err(|e| e.into_ivy())?,
                ));
            }
            let request = Json::object([
                ("action", "createMain".into()),
                ("operationId", Json::String(operation_id.clone())),
                ("expectedBridge", expected.clone()),
                (
                    "expectedMainRevision",
                    current
                        .map(|m| Json::Number(m.pin.revision as f64))
                        .unwrap_or(Json::Null),
                ),
                (
                    "reason",
                    "Automatically create Main for the first incoming message.".into(),
                ),
            ]);
            let created: IvyResult<Option<Json>> = async {
                let created = self.create(caller, &request).await?;
                if state(&created, "phase", &["succeeded"])
                    && null(&created, "outcome").get("action") == Some(&"createMain".into())
                {
                    return Ok(Some(
                        created.required("outcome")?.required("binding")?.clone(),
                    ));
                }
                self.check_automatic_failure(&created)?;
                Ok(None)
            }
            .await;
            match created {
                Ok(Some(binding)) => return Ok(binding),
                Ok(None) => {}
                Err(error)
                    if matches!(
                        error.code.as_str(),
                        "revision_conflict" | "chat_pending_action"
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "Automatic Main creation changed repeatedly; recover its original operation.",
        )
        .with_outcome(Outcome::Unknown))
    }
    fn check_automatic_failure(&self, operation: &Json) -> IvyResult<()> {
        if state(operation, "phase", &["failed"]) {
            let error = null(operation, "error");
            return Err(IvyError::new_js(
                error
                    .get("code")
                    .and_then(Json::as_js_string)
                    .cloned()
                    .unwrap_or_else(|| "chat_operation_failed".into()),
                error
                    .get("detail")
                    .and_then(Json::as_js_string)
                    .cloned()
                    .unwrap_or_else(|| "Automatic Main creation failed.".into()),
            ));
        }
        Ok(())
    }
    async fn claimed(&self, scope: &str, call_id: Option<&str>) -> IvyResult<Claim> {
        let main = self.queue().main().await?;
        require(
            main.value
                .get("pendingAction")
                .and_then(|p| p.get("objectId"))
                == Some(&scope.into())
                && empty_queue(&main.value)?,
            "revision_conflict",
            "Native Main creation requires its original claim and an empty queue.",
        )?;
        let original = self
            .store()
            .read(
                "chat-bridge/operation",
                Reference::Pinned(&pin(main.value.required("pendingAction")?)?),
            )
            .await
            .map_err(|e| e.into_ivy())?;
        let operation = self
            .store()
            .read("chat-bridge/operation", Reference::Current(scope))
            .await
            .map_err(|e| e.into_ivy())?;
        self.operations.check(&original.value)?;
        self.operations.check(&operation.value)?;
        require(
            original.value.required("request")?.get("action") == Some(&"createMain".into())
                && state(&original.value, "phase", &["accepted"])
                && original.value.get("nativeCall") == Some(&Json::Null)
                && original.value.get("callerPrincipalId")
                    == operation.value.get("callerPrincipalId")
                && original.value.get("operationId") == operation.value.get("operationId")
                && same(
                    original.value.required("request")?,
                    operation.value.required("request")?,
                )?
                && original.value.get("createdAt") == operation.value.get("createdAt"),
            "chat_identity_conflict",
            "The claimed Main action must preserve its original caller, request, timestamp and attached native call.",
        )?;
        require(
            call_id.is_none_or(|id| {
                state(&operation.value, "phase", &["applying"])
                    && operation
                        .value
                        .get("nativeCall")
                        .and_then(|p| p.get("objectId"))
                        == Some(&id.into())
            }),
            "revision_conflict",
            "The current action must still authorize this attached native call.",
        )?;
        Ok(Claim { main, operation })
    }
    async fn attach(&self, scope: &str, call: &Document) -> IvyResult<Document> {
        for _ in 0..16 {
            let operation = self.claimed(scope, None).await?.operation;
            let current = &operation.value;
            let attached = current.get("nativeCall").filter(|v| truthy(v));
            require(
                attached
                    .is_none_or(|p| p.get("objectId") == Some(&call.pin.object_id.clone().into())),
                "chat_native_mismatch",
                "Main creation cannot switch to a different native call.",
            )?;
            if attached.is_some_and(|p| {
                p.get("revision")
                    .and_then(Json::as_f64)
                    .is_some_and(|r| r >= call.pin.revision as f64)
            }) {
                return Ok(operation);
            }
            require(
                !state(current, "phase", &["succeeded", "failed"]),
                "chat_operation_unresolved",
                "A completed Main action cannot change its original native call.",
            )?;
            let error = if state(&call.value, "state", &["failed", "outcome_unknown"]) {
                Json::object([
                    ("code", call.value.required("code")?.clone()),
                    (
                        "detail",
                        format!(
                            "The original native Main creation {}.",
                            call.value.required("state")?.as_str()?
                        )
                        .into(),
                    ),
                    (
                        "outcome",
                        if state(&call.value, "state", &["failed"])
                            && call.value.get("epoch") == Some(&Json::Null)
                        {
                            "not_executed"
                        } else {
                            "unknown"
                        }
                        .into(),
                    ),
                ])
            } else {
                Json::Null
            };
            let mut next = current.clone();
            next.insert("nativeCall", call.pin.json());
            next.insert(
                "phase",
                if error.is_null() {
                    "applying".into()
                } else {
                    call.value.required("state")?.clone()
                },
            );
            next.insert("error", error);
            next.insert("updatedAt", call.value.required("updatedAt")?.clone());
            let saved = self
                .store()
                .write(
                    "chat-bridge/operation",
                    &next,
                    &mutation(
                        scope,
                        &format!("attach:{}:{}", operation.pin.revision, hash_json(&next)?),
                    )?,
                    Destination::Update {
                        object_id: scope,
                        expected_revision: operation.pin.revision,
                    },
                )
                .await
                .map_err(|e| e.into_ivy());
            match saved {
                Ok(pin) => {
                    return self
                        .store()
                        .read("chat-bridge/operation", Reference::Pinned(&pin))
                        .await
                        .map_err(|e| e.into_ivy());
                }
                Err(error) if contention(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "Recover the original Main native attachment.",
        )
        .with_outcome(Outcome::Unknown))
    }
    async fn predecessor(&self, main: &Document, operation: &Json) -> IvyResult<Json> {
        let request = operation.required("request")?;
        require(
            request.get("action") == Some(&"createMain".into()),
            "chat_identity_conflict",
            "A Main binding needs its original creation action.",
        )?;
        let revision = request.required("expectedMainRevision")?;
        if revision.is_null() {
            return Ok(Json::Null);
        }
        let selected = Pin {
            object_id: main.pin.object_id.clone(),
            revision: revision.as_f64().ok_or_else(IvyError::internal)? as i64,
        };
        let base = self
            .store()
            .read("chat-bridge/main", Reference::Pinned(&selected))
            .await
            .map_err(|e| e.into_ivy())?;
        require(
            base.value.get("definitionHash")
                == Some(&Json::from(self.admission().definition_hash()))
                && same(
                    base.value.required("definition")?,
                    self.admission().definition(),
                )?
                && !present(&base.value, "pendingAction")
                && empty_queue(&base.value)?
                && selected.revision < main.pin.revision,
            "chat_identity_conflict",
            "Main replacement must retain its exact unoccupied original aggregate revision.",
        )?;
        Ok(base.value.required("binding")?.clone())
    }
    pub async fn recover_pending(&self) -> IvyResult<()> {
        self.native.verify_owner().await?;
        for _ in 0..32 {
            let Some(found) = self.find().await? else {
                return Ok(());
            };
            let Some(pending) = found.value.get("pendingAction").filter(|v| truthy(v)) else {
                return Ok(());
            };
            let pending = self
                .store()
                .read("chat-bridge/operation", Reference::Pinned(&pin(pending)?))
                .await
                .map_err(|e| e.into_ivy())?;
            self.operations.check(&pending.value)?;
            match pending
                .value
                .required("request")?
                .required("action")?
                .as_str()?
                .as_str()
            {
                "send" => return self.queue().recover_pending().await,
                "cancel" => {
                    return ChatCancellation::new(self.operations, self, self.instructions)
                        .recover_pending()
                        .await;
                }
                "notify" => {
                    return ChatNotices::new(self.operations, self, self.instructions)
                        .recover_pending()
                        .await;
                }
                "createMain" => {}
                _ => {
                    return Err(IvyError::new(
                        "chat_pending_action",
                        "Another pending Main action needs its own recovery.",
                    ));
                }
            }
            let scope = &pending.pin.object_id;
            let result = self.recover_creation(scope).await;
            match result {
                Ok(Recovery::Complete) => return Ok(()),
                Ok(Recovery::Retry) => {}
                Err(error) if contention(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "Main publication changed repeatedly; recover its original action.",
        )
        .with_outcome(Outcome::Unknown))
    }
    async fn recover_creation(&self, scope: &str) -> IvyResult<Recovery> {
        let Claim {
            mut main,
            mut operation,
        } = self.claimed(scope, None).await?;
        if state(&operation.value, "phase", &["failed"]) {
            require(
                present(&operation.value, "nativeCall"),
                "chat_identity_conflict",
                "A failed claimed creation must retain its original native call.",
            )?;
            let failed = self
                .native
                .get(
                    scope,
                    Reference::Pinned(&pin(operation.value.required("nativeCall")?)?),
                )
                .await?;
            require(
                state(&failed.value, "state", &["failed"]),
                "chat_operation_unresolved",
                "Only an exact original native failure releases Main.",
            )?;
            self.release(
                &main,
                operation.value.required("updatedAt")?,
                scope,
                "release-failure",
            )
            .await?;
            return Ok(Recovery::Complete);
        }
        if state(&operation.value, "phase", &["succeeded"]) {
            let outcome = null(&operation.value, "outcome");
            let matches = if outcome.get("action") == Some(&"createMain".into()) {
                let binding = outcome.required("binding")?;
                same(main.value.required("binding")?, binding.required("object")?)?
                    && same(
                        &self
                            .store()
                            .read(
                                "chat-bridge/binding",
                                Reference::Pinned(&pin(binding.required("object")?)?),
                            )
                            .await
                            .map_err(|e| e.into_ivy())?
                            .value,
                        binding.required("data")?,
                    )?
            } else {
                false
            };
            require(
                matches,
                "chat_identity_conflict",
                "The completed Main action must retain its published binding and receipt.",
            )?;
            self.release(
                &main,
                operation.value.required("updatedAt")?,
                scope,
                "release-completed",
            )
            .await?;
            return Ok(Recovery::Complete);
        }
        require(
            operation.value.required("request")?.get("action") == Some(&"createMain".into()),
            "chat_identity_conflict",
            "Main creation must retain its original action.",
        )?;
        let replaced = self.predecessor(&main, &operation.value).await?;
        let resumed = self
            .resume_binding(operation.value.required("request")?)
            .await?;
        let previous = resumed
            .as_ref()
            .map(|d| d.pin.json())
            .unwrap_or_else(|| replaced.clone());
        let plan = self.native.resolve_plan()?;
        let method = if resumed.is_some() {
            "thread/resume"
        } else {
            "thread/start"
        };
        let params = if let Some(resumed) = &resumed {
            let mut params = plan.required("threadResume")?.clone();
            params.insert(
                "threadId",
                resumed
                    .value
                    .required("primary")?
                    .required("nativeId")?
                    .clone(),
            );
            params
        } else {
            plan.required("threadStart")?.clone()
        };
        let request = Json::object([
            ("nativeVersion", plan.required("nativeVersion")?.clone()),
            (
                "catalogSourceHash",
                plan.required("catalogSourceHash")?.clone(),
            ),
            ("method", method.into()),
            ("params", params),
        ]);
        let mut call = self
            .native
            .prepare(
                scope,
                &derive_operation_id_js(
                    string(&operation.value, "operationId")?,
                    &format!("chat:native:{method}").into(),
                )?,
                &request,
                NativeOrigin::default(),
                resumed.is_some(),
            )
            .await?;
        operation = self.attach(scope, &call).await?;
        if state(&operation.value, "phase", &["outcome_unknown"]) {
            return Ok(Recovery::Complete);
        }
        let call_id = call.pin.object_id.clone();
        let original_request = operation.value.required("request")?.clone();
        call = self
            .native
            .advance(
                scope,
                &call.pin,
                &MainGuard {
                    main: self,
                    scope,
                    call_id: &call_id,
                    request: &original_request,
                },
            )
            .await?;
        operation = self.attach(scope, &call).await?;
        if state(&call.value, "state", &["failed"]) {
            return Ok(Recovery::Retry);
        }
        if !state(&call.value, "state", &["succeeded"]) {
            return Ok(Recovery::Complete);
        }
        require(
            present(&call.value, "evidence"),
            "chat_native_mismatch",
            "A Main binding requires original native evidence.",
        )?;
        let receipt = self
            .native
            .evidence()?
            .operation(scope, &call.value, &pin(call.value.required("evidence")?)?)
            .await?;
        require(
            receipt
                .get("reply")
                .is_some_and(|r| truthy(r) && r.get("result").is_some()),
            "chat_native_mismatch",
            "Native Main creation needs its original successful reply.",
        )?;
        let thread = receipt
            .required("reply")?
            .required("result")?
            .required("thread")?;
        let matches = thread.get("id").and_then(Json::as_js_string).is_some()
            && thread.get("ephemeral") == Some(&Json::Bool(false))
            && thread.get("cwd").and_then(Json::as_js_string).is_some()
            && resumed.as_ref().is_none_or(|r| {
                thread.get("id") == r.value.get("primary").and_then(|p| p.get("nativeId"))
            })
            && native_path(string(thread, "cwd")?)?
                == native_path(string(plan.required("threadStart")?, "cwd")?)?;
        require(
            matches,
            "chat_project_mismatch",
            "The original native reply must identify a persistent Main in its configured cwd.",
        )?;
        let definition = self.admission().definition();
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("workspaceId", definition.required("workspaceId")?.clone()),
            ("definitionHash", self.admission().definition_hash().into()),
            ("project", definition.required("project")?.clone()),
            (
                "primary",
                Json::object([
                    (
                        "serviceNodeId",
                        definition
                            .required("project")?
                            .required("serviceNodeId")?
                            .clone(),
                    ),
                    ("namespace", "codex".into()),
                    ("kind", "thread".into()),
                    ("nativeId", thread.required("id")?.clone()),
                ]),
            ),
            ("nativePlan", definition.required("nativePlan")?.clone()),
            ("createdAt", operation.value.required("createdAt")?.clone()),
            (
                "operationId",
                operation.value.required("operationId")?.clone(),
            ),
            ("predecessor", previous),
        ]);
        let name = binding_name(scope);
        let binding = match self
            .store()
            .named("chat-bridge/binding", &name)
            .await
            .map_err(|e| e.into_ivy())?
        {
            Some(binding) => binding,
            None => {
                let saved = self
                    .store()
                    .write(
                        "chat-bridge/binding",
                        &value,
                        &mutation(scope, "publish-binding")?,
                        Destination::Create {
                            parent_id: self.store().root_object_id.as_deref(),
                            name: &name,
                        },
                    )
                    .await
                    .map_err(|e| e.into_ivy())?;
                self.store()
                    .read("chat-bridge/binding", Reference::Pinned(&saved))
                    .await
                    .map_err(|e| e.into_ivy())?
            }
        };
        require(
            binding.pin.revision == 1 && same(&binding.value, &value)?,
            "chat_identity_conflict",
            "The original Main binding must remain immutable.",
        )?;
        let claim = self.claimed(scope, None).await?;
        main = claim.main;
        operation = claim.operation;
        require(
            same(main.value.required("binding")?, &replaced)?
                || same(main.value.required("binding")?, &binding.pin.json())?,
            "chat_identity_conflict",
            "Pending Main creation cannot overwrite an unrelated binding.",
        )?;
        if !same(main.value.required("binding")?, &binding.pin.json())? {
            let mut next = main.value.clone();
            next.insert("binding", binding.pin.json());
            next.insert("updatedAt", operation.value.required("createdAt")?.clone());
            main = self.write_main(&main, &next, scope, "publish-main").await?;
        }
        self.operations
            .finish(
                scope,
                &Json::object([
                    ("action", "createMain".into()),
                    (
                        "operationId",
                        operation.value.required("operationId")?.clone(),
                    ),
                    ("binding", binding_view(binding)),
                ]),
            )
            .await?;
        self.release(
            &main,
            operation.value.required("createdAt")?,
            scope,
            "release-main",
        )
        .await?;
        Ok(Recovery::Complete)
    }
    async fn release(
        &self,
        main: &Document,
        updated_at: &Json,
        scope: &str,
        step: &str,
    ) -> IvyResult<()> {
        let mut next = main.value.clone();
        next.insert("pendingAction", Json::Null);
        next.insert("updatedAt", updated_at.clone());
        self.write_main(main, &next, scope, step).await?;
        Ok(())
    }
}
enum Recovery {
    Complete,
    Retry,
}
struct MainGuard<'a, 'b> {
    main: &'a ChatMain<'b>,
    scope: &'a str,
    call_id: &'a str,
    request: &'a Json,
}
impl NativeDispatchGuard for MainGuard<'_, '_> {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async move {
            self.main.native.verify_project().await?;
            self.main.claimed(self.scope, Some(self.call_id)).await?;
            self.main.resume_binding(self.request).await?;
            Ok(())
        })
    }
}
impl EnsureBinding for ChatMain<'_> {
    fn ensure_binding<'a>(
        &'a self,
        caller: &'a JsString,
        operation_id: &'a JsString,
    ) -> BindingFuture<'a> {
        Box::pin(async move {
            pin(self
                .ensure(caller, operation_id)
                .await?
                .required("object")?)
        })
    }
}
impl NoticeMain for ChatMain<'_> {
    fn ensure_notice<'a>(
        &'a self,
        caller: &'a JsString,
        operation_id: &'a JsString,
    ) -> MainFuture<'a> {
        Box::pin(async move {
            self.ensure(caller, operation_id).await?;
            Ok(())
        })
    }
    fn recover_pending(&self) -> MainFuture<'_> {
        Box::pin(ChatMain::recover_pending(self))
    }
    fn verify_owner(&self) -> MainFuture<'_> {
        Box::pin(self.native.verify_owner())
    }
}
