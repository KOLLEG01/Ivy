//! Retained user-contact delivery. A channel handoff never answers a question.
use crate::{
    inspection_journal::verify_owner,
    operations::{Document, Store, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, chat_schema::validate_chat,
    hash::hash_json, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    main_chat::{CHAT_INTERFACE_VERSION, main_chat, main_notification_checked},
    operation_ids::new_operation_id,
    rpc::{RequestOptions, RpcClient},
    tools::{ToolCallOptions, call_bound, discover},
};
use std::sync::Arc;

/// Bind the finalized current ticket/native-input/typed-request instructions.
/// No frozen prompt or readable-version policy is installed as a default.
pub trait DeliveryPolicies {
    fn now(&self) -> JsString;
    fn phone_target(&self) -> &Json;
    fn chat_readable_version(&self, version: &JsString) -> bool;
    fn handoff(&self, task: &Json, comment: &Json) -> IvyResult<JsString>;
    fn voice_prompt(&self, task: &Json, comment: &Json) -> IvyResult<JsString>;
}
#[allow(async_fn_in_trait)]
pub trait DeliveryEngine: DeliveryPolicies {
    fn client(&self) -> Arc<dyn RpcClient>;
    fn principal_id(&self) -> &JsString;
    async fn verify_owner(&self) -> IvyResult<()>;
    async fn new_chat_operation(&self) -> IvyResult<JsString>;
    fn new_phone_operation(&self) -> IvyResult<JsString>;
}
pub struct DeliveryRuntime<R, P> {
    pub client: Arc<R>,
    pub principal_id: JsString,
    pub service_node_id: JsString,
    pub policies: P,
}
impl<R, P: DeliveryPolicies> DeliveryPolicies for DeliveryRuntime<R, P> {
    fn now(&self) -> JsString {
        self.policies.now()
    }
    fn phone_target(&self) -> &Json {
        self.policies.phone_target()
    }
    fn chat_readable_version(&self, v: &JsString) -> bool {
        self.policies.chat_readable_version(v)
    }
    fn handoff(&self, t: &Json, c: &Json) -> IvyResult<JsString> {
        self.policies.handoff(t, c)
    }
    fn voice_prompt(&self, t: &Json, c: &Json) -> IvyResult<JsString> {
        self.policies.voice_prompt(t, c)
    }
}
impl<R: RpcClient + 'static, P: DeliveryPolicies> DeliveryEngine for DeliveryRuntime<R, P> {
    fn client(&self) -> Arc<dyn RpcClient> {
        self.client.clone()
    }
    fn principal_id(&self) -> &JsString {
        &self.principal_id
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        verify_owner(
            self.client.as_ref(),
            &self.principal_id,
            &self.service_node_id,
        )
        .await
    }
    async fn new_chat_operation(&self) -> IvyResult<JsString> {
        new_operation_id(self.client.clone(), None).await
    }
    fn new_phone_operation(&self) -> IvyResult<JsString> {
        Ok(uuid::Uuid::new_v4().to_string().into())
    }
}
pub struct Deliveries<'a, S, E> {
    pub store: &'a S,
    pub engine: &'a E,
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn eq(v: &Json, k: &str, t: &str) -> bool {
    f(v, k) == &Json::from(t)
}
fn array(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn require(b: bool, c: &str, m: &str) -> IvyResult<()> {
    if b { Ok(()) } else { Err(IvyError::new(c, m)) }
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn patch(v: &Json, fields: impl IntoIterator<Item = (&'static str, Json)>) -> Json {
    let mut v = v.clone();
    for (k, x) in fields {
        v.insert(k, x);
    }
    v
}
fn fixed(v: &Json) -> Json {
    Json::object(
        [
            "schemaVersion",
            "taskId",
            "commentId",
            "publication",
            "requestedRoute",
            "createdAt",
        ]
        .map(|k| (k, f(v, k).clone())),
    )
}
pub fn initial_delivery(
    phone_target: &Json,
    publication: &Json,
    task_id: &JsString,
    comment_id: &JsString,
    route: &JsString,
    at: &JsString,
) -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskId", Json::String(task_id.clone())),
        ("commentId", Json::String(comment_id.clone())),
        ("publication", publication.clone()),
        ("requestedRoute", Json::String(route.clone())),
        ("activeRoute", Json::String(route.clone())),
        ("state", "pending".into()),
        ("chatTarget", Json::Null),
        ("phoneTarget", phone_target.clone()),
        ("operationId", Json::Null),
        ("chatRequest", Json::Null),
        ("phoneRequest", Json::Null),
        ("phoneCallId", Json::Null),
        ("deliveryEvidence", Json::Null),
        ("code", Json::Null),
        ("createdAt", Json::String(at.clone())),
        ("updatedAt", Json::String(at.clone())),
    ])
}
impl<S: Store, E: DeliveryEngine> Deliveries<'_, S, E> {
    async fn comment(&self, v: &Json) -> IvyResult<(Json, Json)> {
        let publication = self
            .store
            .read(
                "task-board/operation",
                v.required("publication")?.required("objectId")?,
                true,
            )
            .await?;
        let mut written = None;
        for w in array(f(&publication.value, "writes")) {
            if eq(w, "contractKey", "task-board/task")
                && f(f(w, "outcome"), "objectId") == f(v, "taskId")
                && same(f(w, "outcome"), f(f(&publication.value, "outcome"), "task"))?
            {
                written = Some(w);
                break;
            }
        }
        require(
            eq(&publication.value, "phase", "succeeded") && written.is_some(),
            "task_board_delivery_mismatch",
            "Delivery requires its original completed Task publication.",
        )?;
        let task = self
            .store
            .read(
                "task-board/task",
                written.unwrap().required("payload")?.required("object")?,
                false,
            )
            .await?
            .value;
        let comment = array(f(&task, "comments"))
            .iter()
            .find(|c| f(c, "commentId") == f(v, "commentId"))
            .cloned();
        require(
            comment.is_some() && f(&task, "lastHistory").as_object().is_some(),
            "task_board_delivery_mismatch",
            "The routed Comment and its publication history are unavailable.",
        )?;
        Ok((task, comment.unwrap()))
    }
    async fn current(&self, id: &JsString) -> IvyResult<Option<Document>> {
        let current = self
            .store
            .read("task-board/delivery", &Json::String(id.clone()), true)
            .await?;
        let v = &current.value;
        if eq(v, "state", "confirmed") {
            return Ok(Some(current));
        }
        let publication = self
            .store
            .read(
                "task-board/operation",
                v.required("publication")?.required("objectId")?,
                true,
            )
            .await?;
        if !eq(&publication.value, "phase", "succeeded") {
            return Ok(None);
        }
        let original_pin = Json::object([
            ("objectId", Json::String(id.clone())),
            ("revision", Json::Number(1.0)),
        ]);
        let mut created = None;
        for w in array(f(&publication.value, "writes")) {
            if eq(w, "contractKey", "task-board/delivery") && same(f(w, "outcome"), &original_pin)?
            {
                created = Some(w);
                break;
            }
        }
        require(
            created.is_some(),
            "task_board_delivery_mismatch",
            "Delivery requires its original prepared publication.",
        )?;
        let created = created.unwrap();
        let first = self
            .store
            .read(
                "task-board/delivery",
                created.required("payload")?.required("object")?,
                false,
            )
            .await?
            .value;
        require(
            same(&fixed(v), &fixed(&first))?
                && eq(&first, "state", "pending")
                && [
                    "operationId",
                    "chatRequest",
                    "phoneRequest",
                    "phoneCallId",
                    "deliveryEvidence",
                ]
                .iter()
                .all(|k| f(&first, k) == &Json::Null)
                && f(f(&first, "publication"), "revision") == &Json::Number(1.0),
            "task_board_delivery_mismatch",
            "Delivery identity and original Comment route cannot change.",
        )?;
        require(
            f(f(f(&publication.value, "outcome"), "task"), "objectId") == f(&first, "taskId"),
            "task_board_delivery_mismatch",
            "Delivery requires its completed Task publication.",
        )?;
        let (task, comment) = self.comment(&first).await?;
        require(
            same(f(&comment, "delivery"), f(created, "outcome"))?
                && f(&task, "publication") == &Json::Null,
            "task_board_delivery_mismatch",
            "The completed Task must contain the exact delivered Comment.",
        )?;
        require(
            eq(v, "requestedRoute", "phone") || eq(v, "activeRoute", "chat"),
            "task_board_delivery_mismatch",
            "Chat delivery cannot change to Phone.",
        )?;
        if !f(v, "phoneRequest").is_null() {
            let p = v.required("phoneRequest")?;
            require(
                p.as_object().is_some()
                    && eq(v, "requestedRoute", "phone")
                    && f(v, "phoneTarget").as_object().is_some()
                    && f(p, "recipientId") == f(f(v, "phoneTarget"), "recipientId")
                    && eq(p, "route", "voice")
                    && f(p, "voicePrompt")
                        == &Json::String(self.engine.voice_prompt(&task, &comment)?),
                "task_board_delivery_mismatch",
                "Phone delivery must retain the exact Comment prompt and configured recipient.",
            )?;
        }
        if !f(v, "chatRequest").is_null() {
            let p = v.required("chatRequest")?;
            require(
                p.as_object().is_some()
                    && f(v, "chatTarget").as_object().is_some()
                    && same(
                        f(p, "expectedBridge"),
                        f(f(v, "chatTarget"), "expectedBridge"),
                    )?
                    && same(f(p, "channel"), f(f(v, "chatTarget"), "channel"))?
                    && same(f(p, "source"), f(&task, "lastHistory"))?
                    && f(p, "text") == &Json::String(self.engine.handoff(&task, &comment)?),
                "task_board_delivery_mismatch",
                "Chat delivery must retain the exact Comment handoff and configured Main channel.",
            )?;
        }
        require(
            f(v, "phoneCallId").is_null()
                || f(v, "phoneCallId")
                    .as_js_string()
                    .is_some_and(|id| !id.0.is_empty())
                    && !f(v, "phoneRequest").is_null(),
            "task_board_delivery_mismatch",
            "A Phone call identity requires its retained original request.",
        )?;
        let request = f(
            v,
            if eq(v, "activeRoute", "phone") {
                "phoneRequest"
            } else {
                "chatRequest"
            },
        );
        require(
            eq(v, "state", "pending") || v.get("operationId") == request.get("operationId"),
            "task_board_delivery_mismatch",
            "Active delivery state must retain its exact provider operation identity.",
        )?;
        Ok(Some(current))
    }
    async fn update(&self, current: &Document, next: Json) -> IvyResult<()> {
        if same(
            &patch(
                &current.value,
                [("updatedAt", next.required("updatedAt")?.clone())],
            ),
            &next,
        )? {
            return Ok(());
        }
        self.engine.verify_owner().await?;
        self.store
            .write(
                "task-board/delivery",
                &next,
                &mutation(
                    s(&current.pin, "objectId")?,
                    &format!(
                        "delivery:{}:{}",
                        current
                            .pin
                            .required("revision")?
                            .as_f64()
                            .ok_or_else(IvyError::internal)?,
                        hash_json(&next)?
                    ),
                )?,
                &Json::object([
                    ("objectId", current.pin.required("objectId")?.clone()),
                    (
                        "expectedRevision",
                        current.pin.required("revision")?.clone(),
                    ),
                ]),
            )
            .await?;
        Ok(())
    }
    async fn channel_call(
        &self,
        chat: bool,
        name: &str,
        target: &Json,
        args: Json,
        operation: Option<JsString>,
    ) -> IvyResult<Json> {
        self.engine.verify_owner().await?;
        let client = self.engine.client();
        let node = client
            .request(
                "serviceNodes.get",
                Json::object([("serviceNodeId", target.required("serviceNodeId")?.clone())]),
                RequestOptions::default(),
            )
            .await?;
        require(
            eq(
                &node,
                "serviceName",
                if chat { "chat-bridge" } else { "phone-bridge" },
            ) && (!chat
                || f(&node, "principalId") == f(f(target, "expectedBridge"), "principalId"))
                && ["connected", "synced", "ready"]
                    .iter()
                    .all(|k| f(&node, k).as_bool() == Some(true)),
            if chat {
                "task_board_chat_unavailable"
            } else {
                "task_board_phone_unavailable"
            },
            if chat {
                "The configured ChatBridge provider is not ready under its original principal."
            } else {
                "The configured PhoneBridge provider is not ready."
            },
        )?;
        let bound = discover(
            client.as_ref(),
            &format!("{}.{name}", if chat { "chat" } else { "phone" }).into(),
            &Json::object([
                ("serviceNodeId", target.required("serviceNodeId")?.clone()),
                (
                    "interfaceVersion",
                    if chat {
                        CHAT_INTERFACE_VERSION
                    } else {
                        "1.0.0"
                    }
                    .into(),
                ),
            ]),
        )
        .await?;
        call_bound(
            client.as_ref(),
            &bound,
            args,
            operation,
            ToolCallOptions::default(),
        )
        .await
    }
    async fn chat_evidence(&self, v: &Json, pin: &Json) -> IvyResult<Json> {
        let client = self.engine.client();
        let raw = client
            .request("objects.read", pin.clone(), RequestOptions::default())
            .await?;
        require(
            eq(f(&raw, "object"), "contractKey", "chat-bridge/reply")
                && f(&raw, "revision")
                    .get("contractVersion")
                    .and_then(Json::as_js_string)
                    .is_some_and(|v| self.engine.chat_readable_version(v))
                && eq(f(&raw, "content"), "encoding", "json"),
            "task_board_delivery_mismatch",
            "Delivery proof must be an exact saved ChatBridge reply revision.",
        )?;
        let reply = raw.required("content")?.required("value")?;
        validate_chat("Reply", reply)?;
        require(
            eq(f(reply, "origin"), "kind", "native") && f(v, "chatRequest").as_object().is_some(),
            "task_board_delivery_mismatch",
            "The service handoff must be written by current Main.",
        )?;
        let request = v.required("chatRequest")?;
        require(
            f(reply, "workspaceId") == f(f(request, "expectedBridge"), "workspaceId")
                && same(f(reply, "channel"), f(request, "channel"))?,
            "task_board_delivery_mismatch",
            "The Main reply must retain the original Chat identity.",
        )?;
        let input = client
            .request(
                "objects.read",
                Json::object([(
                    "objectId",
                    reply.required("origin")?.required("inputId")?.clone(),
                )]),
                RequestOptions::default(),
            )
            .await?;
        require(
            eq(f(&input, "object"), "contractKey", "chat-bridge/input")
                && f(&input, "revision")
                    .get("contractVersion")
                    .and_then(Json::as_js_string)
                    .is_some_and(|v| self.engine.chat_readable_version(v))
                && eq(f(&input, "content"), "encoding", "json"),
            "task_board_delivery_mismatch",
            "The Main reply must retain its hidden TaskBoard handoff.",
        )?;
        let handoff = input.required("content")?.required("value")?;
        validate_chat("Input", handoff)?;
        require(
            f(handoff, "definitionHash") == f(reply, "definitionHash")
                && f(handoff, "operationId") == f(request, "operationId")
                && f(f(handoff, "identity"), "senderPrincipalId")
                    == &Json::String(self.engine.principal_id().clone())
                && same(f(f(handoff, "identity"), "channel"), f(request, "channel"))?
                && same(f(handoff, "binding"), f(f(reply, "origin"), "binding"))?
                && same(f(handoff, "result"), f(f(reply, "origin"), "result"))?,
            "task_board_delivery_mismatch",
            "The Main reply must identify this exact TaskBoard handoff.",
        )?;
        Ok(Json::object([
            ("object", pin.clone()),
            ("data", reply.clone()),
        ]))
    }
    async fn chat(&self, current: &Document) -> IvyResult<()> {
        let v = &current.value;
        let target = if !f(v, "chatRequest").is_null() {
            f(v, "chatTarget").clone()
        } else {
            main_chat(self.engine.client().as_ref()).await?.to_json()
        };
        require(
            target.as_object().is_some(),
            "task_board_delivery_mismatch",
            "The original service handoff must retain its owner.",
        )?;
        if f(v, "chatRequest").is_null() {
            require(
                f(f(&target, "expectedBridge"), "callerPrincipalId")
                    == &Json::String(self.engine.principal_id().clone()),
                "task_board_delivery_mismatch",
                "Chat configuration must identify TaskBoard as producer.",
            )?;
            let (task, comment) = self.comment(v).await?;
            let operation = self.engine.new_chat_operation().await?;
            let request = Json::object([
                ("action", "notify".into()),
                ("operationId", Json::String(operation.clone())),
                ("expectedBridge", target.required("expectedBridge")?.clone()),
                ("channel", target.required("channel")?.clone()),
                ("source", task.required("lastHistory")?.clone()),
                ("text", Json::String(self.engine.handoff(&task, &comment)?)),
            ]);
            validate_chat("AdmittedNotifyRequest", &request)?;
            return self
                .update(
                    current,
                    patch(
                        v,
                        [
                            ("activeRoute", "chat".into()),
                            ("state", "dispatching".into()),
                            ("chatTarget", target),
                            ("operationId", Json::String(operation)),
                            ("chatRequest", request),
                            ("code", Json::Null),
                            ("updatedAt", Json::String(self.engine.now())),
                        ],
                    ),
                )
                .await;
        }
        let result: IvyResult<()> = async {
            let request = v.required("chatRequest")?;
            let operation = match self
                .channel_call(
                    true,
                    "operation",
                    &target,
                    Json::object([
                        (
                            "expectedBridge",
                            request.required("expectedBridge")?.clone(),
                        ),
                        ("operationId", request.required("operationId")?.clone()),
                    ]),
                    None,
                )
                .await
            {
                Err(e) if e.code == "not_found" => {
                    self.channel_call(
                        true,
                        "notify",
                        &target,
                        main_notification_checked(request)?,
                        Some(s(request, "operationId")?.clone()),
                    )
                    .await?
                }
                r => r?,
            };
            validate_chat("Operation", &operation)?;
            require(
                eq(f(&operation, "request"), "action", "notify")
                    && same(
                        &main_notification_checked(operation.required("request")?)?,
                        &main_notification_checked(request)?,
                    )?
                    && f(&operation, "callerPrincipalId")
                        == &Json::String(self.engine.principal_id().clone()),
                "task_board_delivery_unresolved",
                "Reconcile the original Main handoff identity.",
            )?;
            if !same(operation.required("request")?, request)? {
                let r = operation.required("request")?;
                return self
                    .update(
                        current,
                        patch(
                            v,
                            [
                                ("chatRequest", r.clone()),
                                (
                                    "chatTarget",
                                    Json::object([
                                        (
                                            "serviceNodeId",
                                            target.required("serviceNodeId")?.clone(),
                                        ),
                                        ("expectedBridge", r.required("expectedBridge")?.clone()),
                                        ("channel", r.required("channel")?.clone()),
                                    ]),
                                ),
                                ("state", "dispatching".into()),
                                ("code", "awaiting_main".into()),
                                ("updatedAt", Json::String(self.engine.now())),
                            ],
                        ),
                    )
                    .await;
            }
            let reply = if eq(&operation, "phase", "succeeded")
                && eq(f(&operation, "outcome"), "action", "notify")
            {
                let found = self
                    .channel_call(
                        true,
                        "notice",
                        &target,
                        Json::object([("operationId", request.required("operationId")?.clone())]),
                        None,
                    )
                    .await?;
                validate_chat("ReplyView", &found)?;
                Some(found)
            } else {
                require(
                    eq(&operation, "phase", "accepted") || eq(&operation, "phase", "applying"),
                    "task_board_delivery_unresolved",
                    "Main cannot complete the original handoff.",
                )?;
                if !eq(v, "code", "awaiting_main") || !eq(v, "state", "dispatching") {
                    self.update(
                        current,
                        patch(
                            v,
                            [
                                ("state", "dispatching".into()),
                                ("code", "awaiting_main".into()),
                                ("updatedAt", Json::String(self.engine.now())),
                            ],
                        ),
                    )
                    .await?;
                }
                return Ok(());
            };
            require(
                reply.is_some(),
                "task_board_delivery_unresolved",
                "The original Main handoff has no reply.",
            )?;
            let verified = self
                .chat_evidence(v, reply.as_ref().unwrap().required("object")?)
                .await?;
            let state = if eq(f(&verified, "data"), "state", "confirmed") {
                "confirmed"
            } else if eq(f(&verified, "data"), "state", "outcome_unknown") {
                "outcome_unknown"
            } else {
                "dispatching"
            };
            self.update(
                current,
                patch(
                    v,
                    [
                        ("state", state.into()),
                        (
                            "deliveryEvidence",
                            reply.unwrap().required("object")?.clone(),
                        ),
                        (
                            "code",
                            if state == "confirmed" {
                                Json::Null
                            } else {
                                "awaiting_display".into()
                            },
                        ),
                        ("updatedAt", Json::String(self.engine.now())),
                    ],
                ),
            )
            .await
        }
        .await;
        match result {
            Err(e) => self.uncertain(current, e).await,
            Ok(()) => Ok(()),
        }
    }
    async fn forwarded(&self, v: &Json, target: &Json, call_id: &Json) -> IvyResult<Option<Json>> {
        let request = v.required("phoneRequest")?;
        let operation = self
            .channel_call(
                false,
                "operation",
                target,
                Json::object([
                    ("callId", call_id.clone()),
                    ("operationId", request.required("operationId")?.clone()),
                ]),
                None,
            )
            .await?;
        if operation.is_null() {
            return Ok(None);
        }
        let intent = f(&operation, "intent");
        require(
            eq(intent, "method", "call.forwardVoice")
                && f(intent, "callId") == call_id
                && f(intent, "operationId") == f(request, "operationId")
                && f(intent, "prompt") == f(request, "voicePrompt"),
            "task_board_delivery_mismatch",
            "The forwarded prompt differs from the original Phone request.",
        )?;
        Ok(Some(operation))
    }
    async fn phone(&self, current: &Document) -> IvyResult<()> {
        let v = &current.value;
        let target = if f(v, "phoneTarget").is_null() {
            self.engine.phone_target()
        } else {
            v.required("phoneTarget")?
        };
        if target.is_null() {
            return self
                .fallback(current, "task_board_phone_unconfigured".into())
                .await;
        }
        if f(v, "phoneRequest").is_null() {
            let (task, comment) = self.comment(v).await?;
            let operation = self.engine.new_phone_operation()?;
            let request = Json::object([
                ("operationId", Json::String(operation.clone())),
                ("recipientId", target.required("recipientId")?.clone()),
                ("route", "voice".into()),
                (
                    "voicePrompt",
                    Json::String(self.engine.voice_prompt(&task, &comment)?),
                ),
            ]);
            return self
                .update(
                    current,
                    patch(
                        v,
                        [
                            ("phoneTarget", target.clone()),
                            ("state", "dispatching".into()),
                            ("operationId", Json::String(operation)),
                            ("phoneRequest", request),
                            ("code", Json::Null),
                            ("updatedAt", Json::String(self.engine.now())),
                        ],
                    ),
                )
                .await;
        }
        let request = v.required("phoneRequest")?;
        if f(v, "phoneCallId").is_null() {
            let result: IvyResult<()> = async {
                let result = self
                    .channel_call(
                        false,
                        "request",
                        target,
                        request.clone(),
                        Some(s(request, "operationId")?.clone()),
                    )
                    .await?;
                require(
                    f(&result, "callId").as_js_string().is_some()
                        && (f(&result, "operationId") == f(request, "operationId")
                            || self
                                .forwarded(v, target, result.required("callId")?)
                                .await?
                                .is_some()),
                    "task_board_delivery_mismatch",
                    "Phone admission changed its original identity.",
                )?;
                self.update(
                    current,
                    patch(
                        v,
                        [
                            ("phoneCallId", result.required("callId")?.clone()),
                            ("code", "awaiting_phone".into()),
                            ("updatedAt", Json::String(self.engine.now())),
                        ],
                    ),
                )
                .await
            }
            .await;
            return match result {
                Err(e) if e.outcome == Outcome::NotExecuted => {
                    self.fallback(current, e.code_js()).await
                }
                Err(e) => self.uncertain(current, e).await,
                Ok(()) => Ok(()),
            };
        }
        let call_id = v.required("phoneCallId")?;
        if let Some(forwarded) = self.forwarded(v, target, call_id).await? {
            if eq(&forwarded, "phase", "submitted") {
                return Ok(());
            }
            if !eq(&forwarded, "phase", "result")
                || f(f(&forwarded, "receipt"), "ok") != &true.into()
                || !eq(f(f(&forwarded, "receipt"), "result"), "state", "sent")
            {
                return self
                    .uncertain(
                        current,
                        IvyError::new(
                            "phone_prompt_outcome_unknown",
                            "The forwarded Voice prompt outcome is unknown.",
                        )
                        .with_outcome(Outcome::Unknown),
                    )
                    .await;
            }
            return self
                .update(
                    current,
                    patch(
                        v,
                        [
                            ("state", "confirmed".into()),
                            ("code", Json::Null),
                            ("updatedAt", Json::String(self.engine.now())),
                        ],
                    ),
                )
                .await;
        }
        let dial = self
            .channel_call(
                false,
                "operation",
                target,
                Json::object([("callId", call_id.clone()), ("method", "call.dial".into())]),
                None,
            )
            .await?;
        if dial.is_null() || eq(&dial, "phase", "submitted") {
            if !eq(v, "code", "awaiting_phone") {
                self.update(
                    current,
                    patch(
                        v,
                        [
                            ("code", "awaiting_phone".into()),
                            ("updatedAt", Json::String(self.engine.now())),
                        ],
                    ),
                )
                .await?;
            }
            return Ok(());
        }
        if eq(&dial, "phase", "outcome_unknown") {
            return self
                .uncertain(
                    current,
                    IvyError::new(
                        "phone_operation_outcome_unknown",
                        "The original phone connection outcome is unknown.",
                    )
                    .with_outcome(Outcome::Unknown),
                )
                .await;
        }
        if f(f(&dial, "receipt"), "ok") != &true.into()
            || !eq(f(f(&dial, "receipt"), "result"), "state", "connected")
        {
            return self.fallback(current, "phone_not_answered".into()).await;
        }
        let prompt = self
            .channel_call(
                false,
                "operation",
                target,
                Json::object([
                    ("callId", call_id.clone()),
                    ("method", "call.promptVoice".into()),
                    ("generation", Json::Number(0.0)),
                ]),
                None,
            )
            .await?;
        if prompt.is_null() || eq(&prompt, "phase", "submitted") {
            if !eq(v, "code", "awaiting_voice_prompt") {
                self.update(
                    current,
                    patch(
                        v,
                        [
                            ("code", "awaiting_voice_prompt".into()),
                            ("updatedAt", Json::String(self.engine.now())),
                        ],
                    ),
                )
                .await?;
            }
            return Ok(());
        }
        if eq(&prompt, "phase", "outcome_unknown") || f(f(&prompt, "receipt"), "ok") != &true.into()
        {
            return self
                .uncertain(
                    current,
                    IvyError::new(
                        "phone_prompt_outcome_unknown",
                        "The connected Voice prompt outcome is unknown.",
                    )
                    .with_outcome(Outcome::Unknown),
                )
                .await;
        }
        self.update(
            current,
            patch(
                v,
                [
                    ("state", "confirmed".into()),
                    ("code", Json::Null),
                    ("updatedAt", Json::String(self.engine.now())),
                ],
            ),
        )
        .await
    }
    async fn fallback(&self, current: &Document, code: JsString) -> IvyResult<()> {
        self.update(
            current,
            patch(
                &current.value,
                [
                    ("activeRoute", "chat".into()),
                    ("state", "pending".into()),
                    ("chatTarget", Json::Null),
                    ("operationId", Json::Null),
                    ("chatRequest", Json::Null),
                    ("deliveryEvidence", Json::Null),
                    ("code", Json::String(code)),
                    ("updatedAt", Json::String(self.engine.now())),
                ],
            ),
        )
        .await
    }
    async fn uncertain(&self, current: &Document, error: IvyError) -> IvyResult<()> {
        if ["revision_conflict", "mutation_conflict"].contains(&error.code.as_str()) {
            return Err(error);
        }
        if !eq(&current.value, "state", "outcome_unknown")
            || f(&current.value, "code") != &Json::String(error.code_js())
        {
            self.update(
                current,
                patch(
                    &current.value,
                    [
                        ("state", "outcome_unknown".into()),
                        ("code", Json::String(error.code_js())),
                        ("updatedAt", Json::String(self.engine.now())),
                    ],
                ),
            )
            .await?;
        }
        Ok(())
    }
    pub async fn step(&self, id: &JsString) -> IvyResult<()> {
        self.engine.verify_owner().await?;
        let Some(current) = self.current(id).await? else {
            return Ok(());
        };
        let v = &current.value;
        if eq(v, "state", "confirmed") {
            return Ok(());
        }
        if eq(v, "state", "outcome_unknown")
            && !(eq(v, "activeRoute", "phone") && !f(v, "phoneRequest").is_null()
                || eq(v, "activeRoute", "chat") && !f(v, "chatRequest").is_null())
        {
            return Ok(());
        }
        if eq(v, "activeRoute", "phone") {
            self.phone(&current).await
        } else {
            self.chat(&current).await
        }
    }
}
