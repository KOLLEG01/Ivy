use super::super::{
    Fixture, Gate, Live, Runtime as Collection, Time, array, compare, field, flag, merge, number,
    reference, result, snapshot, text,
};
use super::Client;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    cancel_worker::{CancellationGuard, CancellationNative, ChatCancelWorker},
    cancellation::ChatCancellation,
    collector::{ChatCollector, CollectionHead, CollectionRuntime},
    native_evidence::ChatNativeEvidence,
    notice_input::NoticeInstructions,
    notices::{ChatNotices, MainFuture, NoticeMain},
    operations::ChatOperations,
    publisher::PublicationFuture,
    queue::ChatQueue,
    store::{ChatStore, Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::parse,
    operation::derive_operation_id_js,
};
use ivy_sdk::clock::{Clock, date_to_iso};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock, atomic::AtomicI64},
};

fn oracle() -> &'static Json {
    static CORPUS: OnceLock<Json> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-cancel-worker-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_CANCEL_WORKER_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
async fn update(
    operations: &ChatOperations<'_>,
    key: &str,
    doc: Document,
    patch: &Json,
) -> IvyResult<Pin> {
    let mut value = doc.value.clone();
    merge(&mut value, patch);
    operations
        .store
        .write(
            key,
            &value,
            &mutation(&doc.pin.object_id, &format!("fixture:{}", doc.pin.revision))?,
            Destination::Update {
                object_id: &doc.pin.object_id,
                expected_revision: doc.pin.revision,
            },
        )
        .await
        .map_err(|e| e.into_ivy())
}
struct Runtime<'a> {
    collection: Collection<'a>,
    client: &'a Client,
    evidence: &'a ChatNativeEvidence<'a>,
    instructions: &'a NoticeInstructions,
    outcomes: &'a Mutex<VecDeque<Json>>,
}
impl Runtime<'_> {
    async fn current(&self, input_id: &str, pin: &Pin) -> IvyResult<Document> {
        self.collection
            .store
            .read_in(
                "chat-bridge/native-call",
                Reference::Current(&pin.object_id),
                Some(input_id),
            )
            .await
            .map_err(|e| e.into_ivy())
    }
    async fn dispatch<'a>(
        &'a self,
        kind: &'a str,
        input_id: &'a str,
        pin: &'a Pin,
        guard: &'a CancellationGuard<'a>,
    ) -> IvyResult<Document> {
        self.collection.event(Json::object([
            ("method", kind.into()),
            ("inputId", input_id.into()),
            ("pin", pin.json()),
        ]));
        let change = {
            let mut live = self.collection.live.lock().unwrap();
            let change = flag(&live.controls, "guardChange");
            if change {
                live.controls.insert("guardChange", false.into());
            }
            change
        };
        if change {
            self.client.patch(
                "original-input",
                &Json::object([("state", "preparing".into())]),
            );
        }
        guard().await?;
        if kind == "prevent"
            && self.collection.live.lock().unwrap().controls.get("prevent")
                == Some(&"unknown-error".into())
        {
            return Err(IvyError::new(
                "chat_native_prevention_unknown",
                "Original prevention is unavailable.",
            )
            .with_outcome(Outcome::Unknown));
        }
        let call = self.current(input_id, pin).await?;
        if call.value.get("state") != Some(&"prepared".into()) {
            return Ok(call);
        }
        let request = self.evidence.request(input_id, &call.value).await?;
        let operation = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected native cancellation outcome");
        for key in ["operationId", "requestHash", "method"] {
            assert_eq!(
                call.value.get(key),
                operation.get(key),
                "Original native identity differs: {key}"
            );
        }
        assert_eq!(
            request.get("params"),
            operation.get("params"),
            "Original native parameters differ"
        );
        let evidence = self
            .evidence
            .save_operation(input_id, &call.value, &operation)
            .await?;
        let pin = update(
            self.collection.operations,
            "chat-bridge/native-call",
            call,
            &Json::object([
                ("state", field(&operation, "phase").clone()),
                ("epoch", field(&operation, "epoch").clone()),
                ("evidence", evidence.json()),
                ("code", field(&operation, "code").clone()),
                ("updatedAt", field(&operation, "updatedAt").clone()),
            ]),
        )
        .await?;
        self.collection
            .store
            .read_in(
                "chat-bridge/native-call",
                Reference::Pinned(&pin),
                Some(input_id),
            )
            .await
            .map_err(|e| e.into_ivy())
    }
}
impl NoticeMain for Runtime<'_> {
    fn ensure_notice<'a>(&'a self, _: &'a JsString, _: &'a JsString) -> MainFuture<'a> {
        Box::pin(async { panic!("Unexpected binding creation") })
    }
    fn verify_owner(&self) -> MainFuture<'_> {
        CollectionRuntime::verify_owner(self)
    }
    fn recover_pending(&self) -> MainFuture<'_> {
        Box::pin(async move {
            self.collection
                .event(Json::object([("method", "recover".into())]));
            let queue = ChatQueue::new(self.collection.operations, None);
            let current = queue.main().await?;
            if current.value.get("pendingAction").is_none_or(Json::is_null) {
                return Ok(());
            }
            let operation = self
                .collection
                .store
                .read(
                    "chat-bridge/operation",
                    Reference::Pinned(&reference(field(&current.value, "pendingAction"))),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            match text(field(&operation.value, "request"), "action").as_str() {
                "cancel" => {
                    ChatCancellation::new(self.collection.operations, self, self.instructions)
                        .recover_pending()
                        .await
                }
                "notify" => {
                    ChatNotices::new(self.collection.operations, self, self.instructions)
                        .recover_pending()
                        .await
                }
                _ => queue.recover_pending().await,
            }
        })
    }
}
impl CollectionRuntime for Runtime<'_> {
    fn head<'a>(&'a self, id: Option<&'a str>) -> PublicationFuture<'a, Option<CollectionHead>> {
        self.collection.head(id)
    }
    fn call<'a>(&'a self, input_id: &'a str, pin: &'a Pin) -> PublicationFuture<'a, Document> {
        Box::pin(async move {
            self.collection.event(Json::object([
                ("method", "call".into()),
                ("inputId", input_id.into()),
                ("pin", pin.json()),
            ]));
            self.current(input_id, pin).await
        })
    }
    fn ready(&self) -> PublicationFuture<'_, Json> {
        self.collection.ready()
    }
    fn read(&self, request: Json) -> PublicationFuture<'_, Json> {
        self.collection.read(request)
    }
    fn verify_owner(&self) -> PublicationFuture<'_, ()> {
        Box::pin(async move {
            self.collection
                .event(Json::object([("method", "verify".into())]));
            if flag(
                &self.collection.live.lock().unwrap().controls,
                "ownerUnavailable",
            ) {
                Err(IvyError::new(
                    "fixture_owner_unavailable",
                    "Original owner is unavailable.",
                ))
            } else {
                Ok(())
            }
        })
    }
    fn recover_pending(&self) -> PublicationFuture<'_, ()> {
        NoticeMain::recover_pending(self)
    }
}
impl CancellationNative for Runtime<'_> {
    fn prepare<'a>(
        &'a self,
        input_id: &'a str,
        operation_id: &'a JsString,
        request: Json,
    ) -> PublicationFuture<'a, Document> {
        Box::pin(async move {
            self.collection.event(Json::object([
                ("method", "prepare".into()),
                ("inputId", input_id.into()),
                ("operationId", Json::String(operation_id.clone())),
                ("request", request.clone()),
            ]));
            let saved = self.evidence.save_request(input_id, &request).await?;
            let method = text(&request, "method");
            let key = match method.as_str() {
                "turn/start" => "turnStart",
                "thread/resume" => "threadResume",
                "turn/interrupt" => "turnInterrupt",
                _ => panic!("Unexpected worker method"),
            };
            let at = Json::String(date_to_iso(self.collection.clock.utc_millis() as f64)?);
            let definition = self.collection.operations.admission.definition();
            let value = Json::object([
                ("schemaVersion", 1.into()),
                ("operationId", Json::String(operation_id.clone())),
                ("serviceNodeId", "owner".into()),
                ("callerPrincipalId", "chat-service".into()),
                ("request", saved.json()),
                ("nativeVersion", field(&request, "nativeVersion").clone()),
                (
                    "catalogSourceHash",
                    field(&request, "catalogSourceHash").clone(),
                ),
                ("method", field(&request, "method").clone()),
                ("predecessor", Json::Null),
                ("preparedEpoch", Json::Null),
                (
                    "requestHash",
                    hash_json(&Json::object([
                        ("method", field(&request, "method").clone()),
                        ("params", field(&request, "params").clone()),
                    ]))?
                    .into(),
                ),
                (
                    "expectedDefinitionHash",
                    field(field(field(definition, "nativePlan"), "definitions"), key).clone(),
                ),
                ("createdAt", at.clone()),
                ("state", "prepared".into()),
                ("epoch", Json::Null),
                ("evidence", Json::Null),
                ("code", Json::Null),
                ("updatedAt", at),
            ]);
            let saved = self
                .collection
                .store
                .write(
                    "chat-bridge/native-call",
                    &value,
                    &mutation(input_id, "fixture-prepare:interrupt")?,
                    Destination::Create {
                        parent_id: Some(input_id),
                        name: "Fixture interrupt",
                    },
                )
                .await
                .map_err(|e| e.into_ivy())?;
            self.collection
                .store
                .read_in(
                    "chat-bridge/native-call",
                    Reference::Pinned(&saved),
                    Some(input_id),
                )
                .await
                .map_err(|e| e.into_ivy())
        })
    }
    fn prevent<'a>(
        &'a self,
        input_id: &'a str,
        pin: &'a Pin,
        guard: &'a CancellationGuard<'a>,
    ) -> PublicationFuture<'a, Document> {
        Box::pin(self.dispatch("prevent", input_id, pin, guard))
    }
    fn advance<'a>(
        &'a self,
        input_id: &'a str,
        pin: &'a Pin,
        guard: &'a CancellationGuard<'a>,
    ) -> PublicationFuture<'a, Document> {
        Box::pin(self.dispatch("advance", input_id, pin, guard))
    }
}
async fn action(
    s: &Json,
    worker: &ChatCancelWorker<'_>,
    collector: &ChatCollector<'_>,
    runtime: &Runtime<'_>,
) -> IvyResult<Json> {
    match text(s, "action").as_str() {
        "step" => worker.step().await?,
        "settle" => worker.settle().await?,
        "collect" => {
            return Ok(collector
                .step("original-input")
                .await?
                .map(|d| Json::object([("pin", d.pin.json()), ("value", d.value)]))
                .unwrap_or(Json::Null));
        }
        "control" => merge(
            &mut runtime.collection.live.lock().unwrap().controls,
            field(s, "value"),
        ),
        "patch-input" => runtime.client.patch("original-input", field(s, "value")),
        "patch-main" => {
            update(
                runtime.collection.operations,
                "chat-bridge/main",
                ChatQueue::new(runtime.collection.operations, None)
                    .main()
                    .await?,
                field(s, "value"),
            )
            .await?;
        }
        "patch-call" => {
            let input = runtime
                .collection
                .store
                .read("chat-bridge/input", Reference::Current("original-input"))
                .await
                .map_err(|e| e.into_ivy())?;
            let call = runtime
                .collection
                .store
                .read_in(
                    "chat-bridge/native-call",
                    Reference::Pinned(&reference(field(
                        field(&input.value, "nativeCalls"),
                        &text(s, "slot"),
                    ))),
                    Some("original-input"),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            update(
                runtime.collection.operations,
                "chat-bridge/native-call",
                call,
                field(s, "value"),
            )
            .await?;
        }
        "attach-interrupt" => {
            let input = runtime
                .collection
                .store
                .read("chat-bridge/input", Reference::Current("original-input"))
                .await
                .map_err(|e| e.into_ivy())?;
            let definition = runtime.collection.operations.admission.definition();
            let request = Json::object([
                (
                    "nativeVersion",
                    field(field(definition, "nativePlan"), "nativeVersion").clone(),
                ),
                (
                    "catalogSourceHash",
                    field(field(definition, "nativePlan"), "catalogSourceHash").clone(),
                ),
                ("method", "turn/interrupt".into()),
                (
                    "params",
                    Json::object([
                        ("threadId", "original-thread".into()),
                        ("turnId", "original-turn".into()),
                    ]),
                ),
            ]);
            let operation_id = derive_operation_id_js(
                field(&input.value, "operationId").as_js_string().unwrap(),
                &"fixture-interrupt".into(),
            )?;
            let call = runtime
                .prepare("original-input", &operation_id, request)
                .await?;
            let mut calls = field(&input.value, "nativeCalls").clone();
            calls.insert("interrupt", call.pin.json());
            runtime
                .client
                .patch("original-input", &Json::object([("nativeCalls", calls)]));
        }
        "reopen" => {}
        other => panic!("Unknown cancellation worker action {other}"),
    }
    Ok(Json::Null)
}
#[tokio::test]
async fn original_cancellation_prevention_interrupt_and_settlement() {
    let corpus = oracle();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let instructions: JsString =
        String::from_utf8(STANDARD.decode(text(corpus, "instructions")).unwrap())
            .unwrap()
            .into();
    let notices = NoticeInstructions::from_json(
        &parse(
            std::str::from_utf8(&STANDARD.decode(text(corpus, "noticeInstructions")).unwrap())
                .unwrap(),
            usize::MAX,
        )
        .unwrap(),
    )
    .unwrap();
    let mut steps = 0;
    let mut gates = 0;
    for case in array(corpus, "cases") {
        let bytes = STANDARD.decode(text(case, "database")).unwrap();
        assert_eq!(digest(&bytes), text(case, "databaseHash"));
        let path = Fixture::new(field(case, "database"));
        let client = Arc::new(Client::new(
            array(case, "records").to_vec(),
            Some(field(case, "history")),
        ));
        let clock = Arc::new(Time(AtomicI64::new(number(corpus, "now") as i64)));
        let live = Mutex::new(Live {
            controls: Json::object([("epoch", "original-epoch".into())]),
            events: vec![],
            observations: array(case, "observations").iter().cloned().collect(),
        });
        let outcomes = Mutex::new(array(case, "outcomes").iter().cloned().collect());
        let gate = Gate::new();
        let mut aliases = vec![];
        let mut mutations = HashMap::new();
        let mut last = 0;
        let mut index = 0;
        while index < array(case, "steps").len() {
            let store =
                ChatStore::with_clock(client.clone(), None, Some(path.0.clone()), clock.clone())
                    .await
                    .unwrap();
            let admission = ChatAdmission::new(field(case, "definition")).unwrap();
            let operations = ChatOperations::new(&store, &admission).unwrap();
            let evidence = ChatNativeEvidence::new(
                &store,
                field(case, "definition"),
                &catalogs,
                &gate,
                &instructions,
            )
            .unwrap();
            let runtime = Runtime {
                collection: Collection {
                    store: &store,
                    operations: &operations,
                    binding: reference(field(case, "binding")),
                    live: &live,
                    clock: &clock,
                },
                client: &client,
                evidence: &evidence,
                instructions: &notices,
                outcomes: &outcomes,
            };
            let collector = ChatCollector::new(&operations, &evidence, &runtime, &catalogs);
            let worker = ChatCancelWorker::new(&collector, &runtime, &runtime, &notices);
            loop {
                let s = &array(case, "steps")[index];
                let expected = &array(case, "expected")[index];
                gate.begin(array(expected, "gates"));
                gates += array(expected, "gates").len();
                let output = result(action(s, &worker, &collector, &runtime).await);
                let actual = snapshot(
                    &path,
                    &client.0,
                    &live,
                    &gate,
                    output,
                    &mut last,
                    &mut aliases,
                    &mut mutations,
                );
                compare(
                    &actual,
                    expected,
                    &format!("{} step {index} {}", text(case, "label"), text(s, "action")),
                    "",
                );
                index += 1;
                steps += 1;
                if index == array(case, "steps").len() || text(s, "action") == "reopen" {
                    break;
                }
            }
        }
        assert!(
            outcomes.lock().unwrap().is_empty(),
            "Unused original native outcomes"
        );
    }
    println!(
        "Original cancellation worker differential: {} workflows, {steps} steps, {gates} required shared boundary comparisons; native driver dispatch remains a mandatory synthetic boundary.",
        array(corpus, "cases").len()
    );
}
