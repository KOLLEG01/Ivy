//! Reconcile cancellation against the original native calls and turn. A missing
//! or unknown owner outcome never authorizes replacement work or queue release.
use crate::{
    cancellation::ChatCancellation,
    collector::ChatCollector,
    notice_input::NoticeInstructions,
    notices::{ChatNotices, NoticeMain},
    operations::contention,
    publisher::PublicationFuture,
    queue::ChatQueue,
    store::{Destination, Document, Pin, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES, operation::derive_operation_id_js,
};

pub type CancellationGuard<'a> = dyn Fn() -> PublicationFuture<'a, ()> + Send + Sync + 'a;
/// The native driver must preserve its original durable request identity. It
/// invokes the supplied live queue guard before prevention or dispatch.
pub trait CancellationNative: Send + Sync {
    fn prepare<'a>(
        &'a self,
        input_id: &'a str,
        operation_id: &'a JsString,
        request: Json,
    ) -> PublicationFuture<'a, Document>;
    fn prevent<'a>(
        &'a self,
        input_id: &'a str,
        pin: &'a Pin,
        guard: &'a CancellationGuard<'a>,
    ) -> PublicationFuture<'a, Document>;
    fn advance<'a>(
        &'a self,
        input_id: &'a str,
        pin: &'a Pin,
        guard: &'a CancellationGuard<'a>,
    ) -> PublicationFuture<'a, Document>;
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a Json, key: &str) -> IvyResult<&'a Json> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn pin(value: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: field(value, "objectId")?.as_str()?,
        revision: field(value, "revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    })
}
fn present(value: &Json, key: &str) -> bool {
    value.get(key).is_some_and(|value| !value.is_null())
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn state(value: &Json, choices: &[&str]) -> bool {
    choices
        .iter()
        .any(|s| value.get("state") == Some(&Json::from(*s)))
}
pub struct ChatCancelWorker<'a> {
    collector: &'a ChatCollector<'a>,
    cancellation: ChatCancellation<'a>,
    native: &'a dyn CancellationNative,
    main: &'a dyn NoticeMain,
    instructions: &'a NoticeInstructions,
}
impl<'a> ChatCancelWorker<'a> {
    pub fn new(
        collector: &'a ChatCollector<'a>,
        native: &'a dyn CancellationNative,
        main: &'a dyn NoticeMain,
        instructions: &'a NoticeInstructions,
    ) -> Self {
        Self {
            collector,
            cancellation: ChatCancellation::new(collector.operations, main, instructions),
            native,
            main,
            instructions,
        }
    }
    async fn guard(&self, input: &Document) -> IvyResult<()> {
        let current = self
            .collector
            .runtime
            .head(Some(&input.pin.object_id))
            .await?;
        require(
            current.is_some_and(|head| {
                head.input.pin == input.pin
                    && present(&head.input.value, "cancellation")
                    && !present(&head.input.value, "finishedAt")
                    && !present(&head.input.value, "result")
            }),
            "revision_conflict",
            "Native cancellation requires its unchanged original queue head and retained intent.",
        )
    }
    async fn update(&self, input: &Document, patch: Json) -> IvyResult<()> {
        let mut next = input.value.clone();
        for (key, value) in patch.as_object().ok_or_else(IvyError::internal)? {
            next.insert(key.clone(), value.clone());
        }
        if same(&next, &input.value)? {
            return Ok(());
        }
        self.guard(input).await?;
        self.collector.runtime.verify_owner().await?;
        next.insert(
            "updatedAt",
            self.collector.operations.store.now_iso()?.into(),
        );
        self.collector
            .operations
            .store
            .write(
                "chat-bridge/input",
                &next,
                &mutation(
                    &input.pin.object_id,
                    &format!("cancel-state:{}:{}", input.pin.revision, hash_json(&next)?),
                )?,
                Destination::Update {
                    object_id: &input.pin.object_id,
                    expected_revision: input.pin.revision,
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        Ok(())
    }
    async fn attach(&self, input: &Document, slot: &str, call: Document) -> IvyResult<()> {
        let unknown = state(&call.value, &["outcome_unknown"]);
        let mut calls = field(&input.value, "nativeCalls")?.clone();
        calls.insert(slot, call.pin.json());
        let mut patch = Json::object([
            ("nativeCalls", calls),
            (
                "state",
                if unknown {
                    "outcome_unknown"
                } else {
                    "cancel_requested"
                }
                .into(),
            ),
            (
                "error",
                if unknown {
                    Json::object([
                        ("code", field(&call.value, "code")?.clone()),
                        ("outcome", "unknown".into()),
                        (
                            "detail",
                            "The original native cancellation call has an unknown outcome.".into(),
                        ),
                    ])
                } else {
                    Json::Null
                },
            ),
        ]);
        if slot == "turn" && state(&call.value, &["succeeded"]) {
            let original = self
                .collector
                .evidence
                .operation(
                    &input.pin.object_id,
                    &call.value,
                    &pin(field(&call.value, "evidence")?)?,
                )
                .await?;
            let reply = original.get("reply");
            require(
                reply.is_some_and(|value| !value.is_null())
                    && reply.unwrap().get("result").is_some(),
                "chat_native_mismatch",
                "Cancellation must reconcile the original delivered turn reply.",
            )?;
            let turn = field(field(reply.unwrap(), "result")?, "turn")?;
            require(
                turn.get("id").is_some_and(|id| id.as_js_string().is_some())
                    && present(&call.value, "epoch"),
                "chat_native_mismatch",
                "The original delivered turn must retain its ID and epoch.",
            )?;
            patch.insert("turnId", field(turn, "id")?.clone());
            patch.insert("epoch", field(&call.value, "epoch")?.clone());
        }
        if slot == "interrupt" && state(&call.value, &["failed"]) {
            patch.insert("error", Json::object([
                ("code", field(&call.value, "code")?.clone()), ("outcome", "unknown".into()),
                ("detail", "The original native interrupt failed; terminal turn evidence is still required.".into()),
            ]));
        }
        self.update(input, patch).await
    }
    pub async fn step(&self) -> IvyResult<()> {
        let mut selected: Option<Document> = None;
        let run = async {
            self.collector.runtime.recover_pending().await?;
            let Some(head) = self.collector.runtime.head(None).await? else {
                return Ok(());
            };
            selected = Some(head.input);
            let input = selected.as_ref().unwrap();
            if !present(&input.value, "cancellation")
                || present(&input.value, "finishedAt")
                || present(&input.value, "result")
            {
                return Ok(());
            }
            self.cancellation
                .recover(&input.value, &input.pin.object_id)
                .await?;
            if !present(&input.value, "turnId") {
                let mut calls: [Option<Document>; 2] = [None, None];
                for (index, slot) in ["turn", "resume"].iter().enumerate() {
                    let reference = field(&input.value, "nativeCalls")?.get(slot);
                    if !reference.is_some_and(|value| !value.is_null()) {
                        continue;
                    }
                    let reference = pin(reference.unwrap())?;
                    let attached = self
                        .collector
                        .runtime
                        .call(&input.pin.object_id, &reference)
                        .await?;
                    let request = self
                        .collector
                        .evidence
                        .request(&input.pin.object_id, &attached.value)
                        .await?;
                    let label = if *slot == "turn" {
                        "chat:native:turn/start".into()
                    } else {
                        format!(
                            "chat:native:resume:{}",
                            hash_json(&Json::object([
                                (
                                    "predecessor",
                                    field(&attached.value, "predecessor")?.clone()
                                ),
                                (
                                    "preparedEpoch",
                                    field(&attached.value, "preparedEpoch")?.clone()
                                ),
                            ]))?
                        )
                        .into()
                    };
                    let expected_id =
                        derive_operation_id_js(string(&input.value, "operationId")?, &label)?;
                    require(
                        attached.value.get("method")
                            == Some(&Json::from(if *slot == "turn" {
                                "turn/start"
                            } else {
                                "thread/resume"
                            }))
                            && attached.value.get("operationId")
                                == Some(&Json::String(expected_id))
                            && field(&request, "params")?.get("threadId")
                                == field(&head.binding.value, "primary")?.get("nativeId"),
                        "chat_native_mismatch",
                        "Prevention must use the original input's exact resume or turn call.",
                    )?;
                    let guard = || -> PublicationFuture<'_, ()> { Box::pin(self.guard(input)) };
                    let call = self
                        .native
                        .prevent(&input.pin.object_id, &reference, &guard)
                        .await?;
                    let attach = reference != call.pin
                        || *slot == "turn" && state(&call.value, &["succeeded"])
                        || state(&call.value, &["outcome_unknown"])
                            && !state(&input.value, &["outcome_unknown"]);
                    if attach {
                        self.attach(input, slot, call).await?;
                        return Ok(());
                    }
                    let terminal = state(&call.value, &["failed", "succeeded"]);
                    calls[index] = Some(call);
                    if !terminal {
                        return Ok(());
                    }
                }
                let failure = calls.iter().flatten().find(|call| {
                    state(&call.value, &["failed"])
                        && call.value.get("code") != Some(&"request_prevented".into())
                });
                self.update(input, Json::object([
                    ("state", if failure.is_some() { "failed" } else { "cancelled" }.into()),
                    ("finishedAt", self.collector.operations.store.now_iso()?.into()),
                    ("error", if let Some(call) = failure { Json::object([
                        ("code", field(&call.value, "code")?.clone()),
                        ("outcome", if call.value.get("epoch") == Some(&Json::Null) { "not_executed" } else { "unknown" }.into()),
                        ("detail", "The original native call failed before a turn was delivered.".into()),
                    ]) } else { Json::Null }),
                ])).await?;
                return Ok(());
            }
            if self
                .collector
                .observed_terminal(&input.pin.object_id)
                .await?
            {
                return Ok(());
            }
            let reference = field(&input.value, "nativeCalls")?.get("interrupt");
            let operation_id = derive_operation_id_js(
                string(field(&input.value, "cancellation")?, "operationId")?,
                &"chat:native:turn/interrupt".into(),
            )?;
            let params = Json::object([
                (
                    "threadId",
                    field(field(&head.binding.value, "primary")?, "nativeId")?.clone(),
                ),
                ("turnId", field(&input.value, "turnId")?.clone()),
            ]);
            if !reference.is_some_and(|value| !value.is_null()) {
                let plan = field(&head.binding.value, "nativePlan")?;
                let request = Json::object([
                    ("nativeVersion", field(plan, "nativeVersion")?.clone()),
                    (
                        "catalogSourceHash",
                        field(plan, "catalogSourceHash")?.clone(),
                    ),
                    ("method", "turn/interrupt".into()),
                    ("params", params),
                ]);
                let call = self
                    .native
                    .prepare(&input.pin.object_id, &operation_id, request)
                    .await?;
                self.attach(input, "interrupt", call).await?;
                return Ok(());
            }
            let reference = pin(reference.unwrap())?;
            let call = self
                .collector
                .runtime
                .call(&input.pin.object_id, &reference)
                .await?;
            let request = self
                .collector
                .evidence
                .request(&input.pin.object_id, &call.value)
                .await?;
            require(
                call.value.get("operationId") == Some(&Json::String(operation_id))
                    && call.value.get("method") == Some(&"turn/interrupt".into())
                    && same(field(&request, "params")?, &params)?,
                "chat_native_mismatch",
                "Cancellation must retain one interrupt of its exact original primary and turn.",
            )?;
            let guard = || -> PublicationFuture<'_, ()> { Box::pin(self.guard(input)) };
            self.attach(
                input,
                "interrupt",
                self.native
                    .advance(&input.pin.object_id, &reference, &guard)
                    .await?,
            )
            .await
        };
        let result: IvyResult<()> = run.await;
        match result {
            Err(error) if contention(&error) => Ok(()),
            Err(error) if selected.is_some() && error.code == "chat_native_prevention_unknown" => {
                self.update(
                    selected.as_ref().unwrap(),
                    Json::object([
                        ("state", "outcome_unknown".into()),
                        (
                            "error",
                            Json::object([
                                ("code", Json::String(error.code_js())),
                                ("outcome", "unknown".into()),
                                ("detail", Json::String(error.message_js())),
                            ]),
                        ),
                    ]),
                )
                .await
            }
            result => result,
        }
    }
    /// A terminal pre-turn input has no native Result. Every attached original
    /// call must be resolved before its unchanged ticket can leave the queue.
    pub async fn settle(&self) -> IvyResult<()> {
        let run = async {
            let Some(head) = self.collector.runtime.head(None).await? else {
                return Ok(());
            };
            let input = head.input;
            if !present(&input.value, "finishedAt")
                || !state(&input.value, &["failed", "cancelled"])
                || present(&input.value, "turnId")
                || present(&input.value, "result")
            {
                return Ok(());
            }
            if present(&input.value, "cancellation") {
                self.cancellation
                    .recover(&input.value, &input.pin.object_id)
                    .await?;
            }
            for slot in ["turn", "resume", "interrupt"] {
                let reference = field(&input.value, "nativeCalls")?.get(slot);
                if !reference.is_some_and(|value| !value.is_null()) {
                    continue;
                }
                let call = self
                    .collector
                    .runtime
                    .call(&input.pin.object_id, &pin(reference.unwrap())?)
                    .await?;
                require(
                    slot != "interrupt"
                        && call.value.get("method")
                            == Some(&Json::from(if slot == "turn" {
                                "turn/start"
                            } else {
                                "thread/resume"
                            })),
                    "chat_native_mismatch",
                    "An undelivered input cannot have another native call.",
                )?;
                if if slot == "turn" {
                    !state(&call.value, &["failed"])
                } else {
                    !state(&call.value, &["failed", "succeeded"])
                } {
                    return Ok(());
                }
            }
            let current = ChatQueue::new(self.collector.operations, None)
                .main()
                .await?;
            let latest = self
                .collector
                .runtime
                .head(Some(&input.pin.object_id))
                .await?;
            require(
                !present(&current.value, "pendingAction")
                    && !present(&current.value, "publication")
                    && field(&current.value, "queue")?
                        .as_array()
                        .and_then(|q| q.first())
                        .and_then(|t| t.get("inputId"))
                        == Some(&input.pin.object_id.clone().into())
                    && latest.is_some_and(|head| head.input.pin == input.pin),
                "revision_conflict",
                "Only the unchanged settled original input can release its ticket.",
            )?;
            self.collector.runtime.verify_owner().await?;
            ChatNotices::new(self.collector.operations, self.main, self.instructions)
                .finish_undelivered(&input)
                .await?;
            let mut next = current.value.clone();
            next.insert(
                "queue",
                Json::Array(
                    field(&current.value, "queue")?
                        .as_array()
                        .ok_or_else(IvyError::internal)?[1..]
                        .to_vec(),
                ),
            );
            next.insert("updatedAt", field(&input.value, "finishedAt")?.clone());
            self.collector
                .operations
                .store
                .write(
                    "chat-bridge/main",
                    &next,
                    &mutation(
                        &input.pin.object_id,
                        &format!("settle:{}:{}", current.pin.revision, hash_json(&next)?),
                    )?,
                    Destination::Update {
                        object_id: &current.pin.object_id,
                        expected_revision: current.pin.revision,
                    },
                )
                .await
                .map_err(|e| e.into_ivy())?;
            Ok(())
        };
        let result: IvyResult<()> = run.await;
        match result {
            Err(error) if contention(&error) => Ok(()),
            result => result,
        }
    }
}
