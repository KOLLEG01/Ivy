//! Durable cancellation admission. The receipt confirms the retained intent;
//! native prevention, interruption and terminal settlement remain separate.
use crate::{
    admission::expected_bridge,
    notice_input::NoticeInstructions,
    notices::{ChatNotices, NoticeMain},
    operations::{ChatOperations, contention},
    queries::ChatQueries,
    queue::ChatQueue,
    store::{Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, chat_schema::validate_chat,
    hash::hash_json, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(v: &'a Json, key: &str) -> IvyResult<&'a Json> {
    v.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(v: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    field(v, key)?.as_js_string().ok_or_else(IvyError::internal)
}
fn pin(v: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: field(v, "objectId")?.as_str()?,
        revision: field(v, "revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    })
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn present(v: &Json, key: &str) -> bool {
    v.get(key).is_some_and(|value| !value.is_null())
}
fn tickets(v: &Json) -> IvyResult<&[Json]> {
    field(v, "queue")?.as_array().ok_or_else(IvyError::internal)
}
pub struct ChatCancellation<'a> {
    pub operations: &'a ChatOperations<'a>,
    main: &'a dyn NoticeMain,
    instructions: &'a NoticeInstructions,
}
impl<'a> ChatCancellation<'a> {
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
    fn queries(&self) -> ChatQueries<'_> {
        ChatQueries::new(self.operations)
    }
    async fn write_main(&self, current: &Document, next: &Json, scope: &str) -> IvyResult<()> {
        self.main.verify_owner().await?;
        self.operations
            .store
            .write(
                "chat-bridge/main",
                next,
                &mutation(
                    scope,
                    &format!("cancel-main:{}:{}", current.pin.revision, hash_json(next)?),
                )?,
                Destination::Update {
                    object_id: &current.pin.object_id,
                    expected_revision: current.pin.revision,
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        Ok(())
    }
    pub async fn cancel(&self, caller: &JsString, original: &Json) -> IvyResult<Json> {
        self.operations
            .admission
            .authorize(caller, expected_bridge(original)?, None)?;
        validate_chat("CancelRequest", original)?;
        let request = original.clone();
        let operation_id = string(&request, "operationId")?;
        let prior = self.operations.find(caller, operation_id).await?;
        if prior.is_none() {
            let selected = self
                .queries()
                .input(
                    caller,
                    &Json::object([
                        ("expectedBridge", field(&request, "expectedBridge")?.clone()),
                        ("inputId", field(&request, "inputId")?.clone()),
                    ]),
                )
                .await?;
            require(
                field(&selected, "object")?.get("revision") == request.get("expectedRevision"),
                "revision_conflict",
                "Cancellation requires the displayed input revision.",
            )?;
            let data = field(&selected, "data")?;
            require(
                !present(data, "finishedAt")
                    && !present(data, "result")
                    && !present(data, "cancellation"),
                "chat_cancel_unavailable",
                "Only an unfinished input without another cancellation can accept this intent.",
            )?;
            let current = self.queue().main().await?;
            require(
                tickets(&current.value)?
                    .iter()
                    .any(|t| t.get("inputId") == request.get("inputId")),
                "chat_cancel_unavailable",
                "The original input must still belong to the execution queue.",
            )?;
        }
        let operation = self.operations.begin(caller, &request).await?;
        for _ in 0..24 {
            let current = self.queue().main().await?;
            let latest = self.operations.find(caller, operation_id).await?;
            require(
                latest.is_some(),
                "chat_identity_conflict",
                "The original cancellation must remain discoverable.",
            )?;
            let latest = latest.unwrap();
            if current
                .value
                .get("pendingAction")
                .and_then(|p| p.get("objectId"))
                == Some(&operation.pin.object_id.clone().into())
            {
                self.recover_pending().await?;
                return Ok(self
                    .operations
                    .find(caller, operation_id)
                    .await?
                    .ok_or_else(IvyError::internal)?
                    .value);
            }
            if ["succeeded", "failed"]
                .iter()
                .any(|s| latest.value.get("phase") == Some(&Json::from(*s)))
            {
                return Ok(latest.value);
            }
            if present(&current.value, "pendingAction") {
                self.main.recover_pending().await?;
                continue;
            }
            require(
                latest.value.get("phase") == Some(&"accepted".into())
                    && latest.value.get("nativeCall") == Some(&Json::Null),
                "chat_operation_unresolved",
                "Only the original unapplied cancellation can claim Main.",
            )?;
            let mut next = current.value.clone();
            next.insert("pendingAction", operation.pin.json());
            next.insert("updatedAt", field(&operation.value, "createdAt")?.clone());
            if let Err(error) = self
                .write_main(&current, &next, &operation.pin.object_id)
                .await
            {
                if !contention(&error) {
                    return Err(error);
                }
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "Recover the original cancellation after concurrent Main actions.",
        )
        .with_outcome(Outcome::Unknown))
    }
    pub async fn recover_pending(&self) -> IvyResult<()> {
        for _ in 0..24 {
            let main = self.queue().main().await?;
            if !present(&main.value, "pendingAction") {
                return Ok(());
            }
            let accepted = self
                .operations
                .store
                .read(
                    "chat-bridge/operation",
                    Reference::Pinned(&pin(field(&main.value, "pendingAction")?)?),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            self.operations.check(&accepted.value)?;
            require(
                accepted.value.get("phase") == Some(&"accepted".into())
                    && field(&accepted.value, "request")?.get("action") == Some(&"cancel".into()),
                "chat_pending_action",
                "This Main claim must retain its original accepted cancellation.",
            )?;
            let request = field(&accepted.value, "request")?;
            let caller = string(&accepted.value, "callerPrincipalId")?;
            let scope = &accepted.pin.object_id;
            let operation = self
                .operations
                .store
                .read("chat-bridge/operation", Reference::Current(scope))
                .await
                .map_err(|e| e.into_ivy())?;
            self.operations.check(&operation.value)?;
            require(
                same(field(&operation.value, "request")?, request)?
                    && operation.value.get("createdAt") == accepted.value.get("createdAt"),
                "chat_identity_conflict",
                "Cancellation cannot change its original request and time.",
            )?;
            let result:IvyResult<()>=async {
                let selected=self.queries().input(caller,&Json::object([("expectedBridge",field(request,"expectedBridge")?.clone()),("inputId",field(request,"inputId")?.clone())])).await?;
                let data=field(&selected,"data")?;
                let cancellation=Json::object([("operationId",field(request,"operationId")?.clone()),("callerPrincipalId",Json::String(caller.clone())),("reason",field(request,"reason")?.clone()),("requestedAt",field(&accepted.value,"createdAt")?.clone())]);
                let applied=same(field(data,"cancellation")?,&cancellation)?;
                if operation.value.get("phase")==Some(&"failed".into()) || field(&selected,"object")?.get("revision")!=request.get("expectedRevision")&&!applied {
                    require(!applied,"chat_operation_unresolved","An applied original cancellation cannot be discarded as unexecuted.")?;
                    if operation.value.get("phase")!=Some(&"failed".into()) {self.operations.reject_unapplied(scope,"revision_conflict","The input changed before the original cancellation intent was saved.").await?;}
                    let mut next=main.value.clone();next.insert("pendingAction",Json::Null);next.insert("updatedAt",field(&accepted.value,"createdAt")?.clone());
                    return self.write_main(&main,&next,scope).await;
                }
                let input_id=field(request,"inputId")?.as_str()?;
                let base=self.operations.store.read("chat-bridge/input",Reference::Pinned(&Pin {object_id:input_id.clone(),revision:field(request,"expectedRevision")?.as_f64().ok_or_else(IvyError::internal)? as i64})).await.map_err(|e|e.into_ivy())?;
                self.operations.admission.authorize(caller,field(request,"expectedBridge")?,Some(field(field(&base.value,"identity")?,"channel")?))?;
                require(!present(&base.value,"finishedAt")&&!present(&base.value,"result")&&!present(&base.value,"cancellation")
                    &&same(field(&base.value,"identity")?,field(data,"identity")?)?&&same(field(&base.value,"binding")?,field(data,"binding")?)?,"chat_identity_conflict","Cancellation must preserve its original unfinished input and binding.")?;
                let mut intent=base.value.clone();intent.insert("state","cancel_requested".into());intent.insert("cancellation",cancellation);intent.insert("updatedAt",field(&accepted.value,"createdAt")?.clone());
                self.main.verify_owner().await?;
                let reference=self.operations.store.write("chat-bridge/input",&intent,&mutation(scope,"cancel-intent")?,Destination::Update {object_id:&input_id,expected_revision:field(request,"expectedRevision")?.as_f64().ok_or_else(IvyError::internal)? as i64}).await.map_err(|e|e.into_ivy())?;
                let saved=self.operations.store.read("chat-bridge/input",Reference::Pinned(&reference)).await.map_err(|e|e.into_ivy())?;
                self.operations.finish(scope,&Json::object([("action","cancel".into()),("operationId",field(request,"operationId")?.clone()),("input",Json::object([("object",saved.pin.json()),("data",saved.value)]))])).await?;
                let mut next=main.value.clone();next.insert("pendingAction",Json::Null);next.insert("updatedAt",field(&accepted.value,"createdAt")?.clone());
                let calls=field(&base.value,"nativeCalls")?.as_object().ok_or_else(IvyError::internal)?;
                if !present(&base.value,"turnId")&&calls.iter().all(|(_,value)|value==&Json::Null) {
                    let mut terminal=intent;terminal.insert("state","cancelled".into());terminal.insert("finishedAt",field(&accepted.value,"createdAt")?.clone());terminal.insert("error",Json::Null);
                    let cancelled=self.operations.store.write("chat-bridge/input",&terminal,&mutation(scope,"cancel-queued")?,Destination::Update {object_id:&input_id,expected_revision:saved.pin.revision}).await.map_err(|e|e.into_ivy())?;
                    let input=self.operations.store.read("chat-bridge/input",Reference::Pinned(&cancelled)).await.map_err(|e|e.into_ivy())?;
                    ChatNotices::new(self.operations,self.main,self.instructions).finish_undelivered(&input).await?;
                    next.insert("queue",Json::Array(tickets(&main.value)?.iter().filter(|t|t.get("inputId")!=request.get("inputId")).cloned().collect()));
                }
                self.write_main(&main,&next,scope).await
            }.await;
            match result {
                Ok(()) => return Ok(()),
                Err(error) if contention(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "Recover the original claimed cancellation after input contention.",
        )
        .with_outcome(Outcome::Unknown))
    }
    pub async fn recover(&self, input: &Json, input_id: &str) -> IvyResult<Json> {
        require(
            present(input, "cancellation"),
            "chat_cancel_unavailable",
            "The input must retain its original cancellation intent.",
        )?;
        let cancellation = field(input, "cancellation")?;
        let operation = self
            .operations
            .find(
                string(cancellation, "callerPrincipalId")?,
                string(cancellation, "operationId")?,
            )
            .await?;
        let matches = if let Some(operation) = &operation {
            let request = field(&operation.value, "request")?;
            request.get("action") == Some(&"cancel".into())
                && request.get("inputId") == Some(&input_id.into())
                && request.get("reason") == cancellation.get("reason")
                && operation.value.get("createdAt") == cancellation.get("requestedAt")
        } else {
            false
        };
        require(
            matches,
            "chat_identity_conflict",
            "Cancellation must preserve its exact original caller, request and reason.",
        )?;
        self.cancel(
            string(cancellation, "callerPrincipalId")?,
            field(&operation.unwrap().value, "request")?,
        )
        .await
    }
}
