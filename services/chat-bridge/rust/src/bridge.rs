//! One Hive connection generation. Only the provider invocation supplies caller
//! identity, operation identity and cancellation; domain owners keep their gates.
use crate::{
    cancellation::ChatCancellation,
    chat_main::ChatMain,
    collector::ChatCollector,
    delivery::ChatDelivery,
    notices::ChatNotices,
    outbox::ChatOutbox,
    queries::ChatQueries,
    schema::catalog,
    store::{Pin, Reference},
    worker::ChatWorker,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, chat_schema::validate_chat, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::service::InvocationContext;

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

pub struct ChatBridge<'a> {
    pub main: &'a ChatMain<'a>,
    pub outbox: ChatOutbox<'a>,
    pub queries: ChatQueries<'a>,
    pub cancellation: ChatCancellation<'a>,
    pub worker: ChatWorker<'a>,
    /// Immutable generation of the connection that owns Main and its store.
    pub generation: u64,
    pub files: Option<&'a crate::files::ChatFiles>,
}
impl<'a> ChatBridge<'a> {
    pub fn new(
        delivery: &'a ChatDelivery<'a>,
        collector: &'a ChatCollector<'a>,
        generation: u64,
    ) -> IvyResult<Self> {
        let main = delivery.main;
        Ok(Self {
            main,
            outbox: ChatOutbox::new(main.operations),
            queries: ChatQueries::new(main.operations),
            cancellation: ChatCancellation::new(main.operations, main, main.notice_instructions()),
            worker: ChatWorker::new(delivery, collector)?,
            generation,
            files: None,
        })
    }
    pub async fn workspace(&self, caller: &JsString) -> IvyResult<Json> {
        let admission = self.main.admission();
        let expected = admission.expected(caller);
        self.main.native.verify_owner().await?;
        let main = self.main.find().await?;
        let binding_pin = main
            .as_ref()
            .and_then(|m| m.value.get("binding"))
            .filter(|v| !v.is_null());
        let binding = if let Some(value) = binding_pin {
            let pin = Pin {
                object_id: value.required("objectId")?.as_str()?,
                revision: value
                    .required("revision")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)? as i64,
            };
            Some(
                self.main
                    .store()
                    .read("chat-bridge/binding", Reference::Pinned(&pin))
                    .await
                    .map_err(|e| e.into_ivy())?,
            )
        } else {
            None
        };
        if let Some(binding) = &binding {
            let value = &binding.value;
            let definition = admission.definition();
            let primary = value.required("primary")?;
            require(
                value.get("workspaceId") == definition.get("workspaceId")
                    && value.get("definitionHash")
                        == Some(&Json::from(admission.definition_hash()))
                    && same(value.required("project")?, definition.required("project")?)?
                    && same(
                        value.required("nativePlan")?,
                        definition.required("nativePlan")?,
                    )?
                    && primary.get("serviceNodeId")
                        == definition.required("project")?.get("serviceNodeId")
                    && primary.get("namespace") == Some(&Json::from("codex"))
                    && primary.get("kind") == Some(&Json::from("thread")),
                "chat_binding_mismatch",
                "The selected Main must preserve its configured native project and owner.",
            )?;
        }
        // Native outages cannot hide saved history. Our own generation is
        // verified separately after observing the external native owner.
        let native_owner_ready = self.main.native.ready().await.is_ok();
        self.main.native.verify_owner().await?;
        let view = |document: crate::store::Document| {
            Json::object([("object", document.pin.json()), ("data", document.value)])
        };
        Ok(Json::object([
            ("expectedBridge", expected),
            (
                "project",
                admission.definition().required("project")?.clone(),
            ),
            ("main", main.map(view).unwrap_or(Json::Null)),
            ("binding", binding.map(view).unwrap_or(Json::Null)),
            ("channels", admission.channels_for(caller)),
            ("nativeOwnerReady", native_owner_ready.into()),
            ("observedAt", self.main.store().now_iso()?.into()),
        ]))
    }
    pub async fn invoke(
        &self,
        name: &str,
        args: &Json,
        context: &InvocationContext,
    ) -> IvyResult<Json> {
        context.throw_if_aborted()?;
        require(
            self.generation == context.generation,
            "stale_generation",
            "ChatBridge invocation belongs to another Hive generation.",
        )?;
        let caller = &context.caller_principal_id;
        // Reader/notice/either admission is identical in the original policy.
        self.main.admission().expected(caller);
        let operation = catalog().required("operations")?.required(name)?;
        let input = operation.required("input")?.as_str()?;
        validate_chat(
            input
                .strip_prefix("#/$defs/")
                .ok_or_else(IvyError::internal)?,
            args,
        )?;
        if operation.get("mutation") == Some(&Json::Bool(true)) {
            let operation_id = args.get("operationId").and_then(Json::as_js_string);
            require(
                operation_id.is_some() && operation_id == context.operation_id.as_ref(),
                "invalid_arguments",
                "The transport and Chat action must use one operation identity.",
            )?;
            let operation_id = operation_id.unwrap();
            let retained = self.main.operations.find(caller, operation_id).await?;
            let file_exists = self
                .files
                .map(|files| files.has(caller, operation_id))
                .transpose()?
                .unwrap_or(false);
            require(
                if name == "sendFile" {
                    retained.is_none()
                } else {
                    !file_exists
                },
                "chat_identity_conflict",
                "One ChatBridge operation identity cannot be reused for file and conversation actions.",
            )?;
            let retained_action = if name == "sendFile" {
                file_exists
            } else {
                retained.as_ref().is_some_and(|r| {
                    r.value.get("request").and_then(|r| r.get("action")) == Some(&Json::from(name))
                })
            };
            self.main
                .store()
                .validate_operation_id(operation_id, None, retained_action)
                .await
                .map_err(|e| e.into_ivy())?;
        }
        let result = match name {
            "sendFile" | "file" => {
                let files = self.files.ok_or_else(|| {
                    IvyError::new(
                        "service_not_ready",
                        "The WhatsApp file transport is unavailable.",
                    )
                })?;
                if name == "sendFile" {
                    files.send(self, caller, args).await?
                } else {
                    files.read(self, caller, args).await?
                }
            }
            "retryResult" => self.worker.collector.fresh().retry(caller, args).await?,
            "workspace" => self.workspace(caller).await?,
            "operation" => {
                self.main
                    .admission()
                    .authorize(caller, args.required("expectedBridge")?, None)?;
                let operation_id = args
                    .required("operationId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?;
                let operation = self.main.operations.find(caller, operation_id).await?;
                require(
                    operation.is_some(),
                    "not_found",
                    "This caller has no retained ChatBridge operation with that identity.",
                )?;
                operation.unwrap().value
            }
            "createMain" => self.main.create(caller, args).await?,
            "send" => self.main.queue().send(caller, args).await?,
            "cancel" => self.cancellation.cancel(caller, args).await?,
            "receive" => self.outbox.receive(caller, args).await?,
            "acknowledge" => self.outbox.acknowledge(caller, args).await?,
            "history" => self.outbox.history(caller, args).await?,
            "input" => self.queries.input(caller, args).await?,
            "notify" => {
                ChatNotices::new(
                    self.main.operations,
                    self.main,
                    self.main.notice_instructions(),
                )
                .notify(caller, args)
                .await?
            }
            "notice" => {
                ChatNotices::new(
                    self.main.operations,
                    self.main,
                    self.main.notice_instructions(),
                )
                .notice(caller, args)
                .await?
            }
            _ => return Err(IvyError::internal()),
        };
        context.throw_if_aborted()?;
        Ok(result)
    }
}
