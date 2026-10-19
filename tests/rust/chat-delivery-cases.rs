use super::{Client, Fixed, Replay, equal, field, flag, result, snapshot, step as main_step, text};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    cancellation::ChatCancellation,
    chat_main::ChatMain,
    collector::ChatCollector,
    delivery::ChatDelivery,
    diagnostic_notifications::DiagnosticMain,
    native_driver::{ChatNativeDriver, NativeOrigin},
    notice_input::NoticeInstructions,
    operations::ChatOperations,
    store::{ChatStore, Reference, mutation},
    worker::ChatWorker,
};
use ivy_contracts::{
    IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::parse,
    operation::derive_operation_id_js,
};
use ivy_sdk::{clock::Clock, rpc::RequestOptions, tools::ToolClients};
use std::{
    collections::VecDeque,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-delivery-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_DELIVERY_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
struct DrainClock {
    now: i64,
    base: Instant,
    elapsed_step: Mutex<(u64, u64)>,
}
impl DrainClock {
    fn set(&self, step: u64) {
        *self.elapsed_step.lock().unwrap() = (0, step);
    }
}
impl Clock for DrainClock {
    fn utc_millis(&self) -> i64 {
        self.now
    }
    fn monotonic(&self) -> Instant {
        let mut state = self.elapsed_step.lock().unwrap();
        let value = state.0;
        state.0 += state.1;
        self.base + Duration::from_millis(value)
    }
}
fn status(worker: &ChatWorker<'_>) -> Json {
    Json::object([
        ("idle", worker.idle.into()),
        ("runnable", worker.runnable.into()),
        (
            "turnId",
            worker
                .turn_id
                .clone()
                .map(Json::String)
                .unwrap_or(Json::Null),
        ),
    ])
}
async fn input(main: &ChatMain<'_>) -> IvyResult<ivy_chat_bridge::store::Document> {
    let main_doc = main.find().await?.unwrap();
    let ticket = &field(&main_doc.value, "queue").as_array().unwrap()[0];
    main.store()
        .read(
            "chat-bridge/input",
            Reference::Current(&text(ticket, "inputId")),
        )
        .await
        .map_err(|e| e.into_ivy())
}
async fn step(
    action: &Json,
    main: &ChatMain<'_>,
    delivery: &ChatDelivery<'_>,
    worker: &mut ChatWorker<'_>,
    client: &Client,
    notice: &NoticeInstructions,
    clock: &DrainClock,
) -> IvyResult<Json> {
    match text(action, "action").as_str() {
        "delivery" => {
            delivery.step().await?;
            Ok(Json::Null)
        }
        "worker" => {
            worker.step().await?;
            Ok(Json::Null)
        }
        "worker-status" => Ok(status(worker)),
        "head" => {
            let id = if let Some(selected) = action.get("input") {
                let selected = selected.as_str()?;
                if selected == "head" || selected == "tail" {
                    let m = main.find().await?.unwrap();
                    let queue = field(&m.value, "queue").as_array().unwrap();
                    (if selected == "tail" {
                        queue.last()
                    } else {
                        queue.first()
                    })
                    .map(|t| text(t, "inputId"))
                } else {
                    Some(selected)
                }
            } else {
                None
            };
            Ok(delivery
                .head(id.as_deref())
                .await?
                .map(|h| {
                    Json::object([
                        ("input", h.input.pin.json()),
                        ("binding", h.binding.pin.json()),
                        ("request", h.request),
                    ])
                })
                .unwrap_or(Json::Null))
        }
        "drain" => {
            clock.set(
                action
                    .get("millisPerRead")
                    .and_then(Json::as_f64)
                    .unwrap_or(0.0) as u64,
            );
            let maximum = action
                .get("continueSteps")
                .and_then(Json::as_f64)
                .unwrap_or(9_007_199_254_740_991.0) as u64;
            let mut checks = 0;
            worker
                .drain(|| {
                    checks += 1;
                    checks <= maximum
                })
                .await?;
            let mut value = status(worker);
            value.insert("continueChecks", Json::Number(checks as f64));
            Ok(value)
        }
        "restart" => Ok(Json::Null),
        "input" => Ok(input(main).await?.value),
        "pre-prepare-resume" => {
            let selected = input(main).await?;
            let current = main.find().await?.unwrap();
            let binding = main
                .store()
                .read(
                    "chat-bridge/binding",
                    Reference::Pinned(&super::object_pin(field(&current.value, "binding"))),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            let plan = main.native.resolve_plan()?;
            let origin = Json::object([("predecessor", Json::Null), ("preparedEpoch", Json::Null)]);
            let mut params = field(&plan, "threadResume").clone();
            params.insert(
                "threadId",
                field(field(&binding.value, "primary"), "nativeId").clone(),
            );
            let request = Json::object([
                ("nativeVersion", field(&plan, "nativeVersion").clone()),
                (
                    "catalogSourceHash",
                    field(&plan, "catalogSourceHash").clone(),
                ),
                ("method", "thread/resume".into()),
                ("params", params),
            ]);
            let id = derive_operation_id_js(
                field(&selected.value, "operationId")
                    .as_js_string()
                    .unwrap(),
                &format!("chat:native:resume:{}", hash_json(&origin)?).into(),
            )?;
            main.native
                .prepare(
                    &selected.pin.object_id,
                    &id,
                    &request,
                    NativeOrigin::default(),
                    false,
                )
                .await?;
            Ok(Json::Null)
        }
        "image-send" => {
            let bytes = STANDARD
                .decode(if flag(action, "valid") {
                    "iVBORw0KGgoAAA=="
                } else {
                    "VGhpcyBpcyBub3QgYSBQTkcgaW1hZ2Uu"
                })
                .unwrap();
            let current = main.find().await?.unwrap();
            let image = main
                .store()
                .client
                .request(
                    "objects.write",
                    Json::object([
                        (
                            "mutationId",
                            format!(
                                "chat:{}:synthetic-image",
                                field(oracle(), "now").as_f64().unwrap() as i64
                            )
                            .into(),
                        ),
                        ("contractVersion", "1.0.0".into()),
                        ("references", Json::object([] as [(&str, Json); 0])),
                        (
                            "create",
                            Json::object([
                                ("parentId", Json::Null),
                                (
                                    "ownerObjectId",
                                    field(field(&current.value, "conversation"), "objectId")
                                        .clone(),
                                ),
                                ("contractKey", "chat-bridge/image-png".into()),
                                ("name", "Synthetic original PNG".into()),
                            ]),
                        ),
                        (
                            "content",
                            Json::object([
                                ("encoding", "base64".into()),
                                ("value", STANDARD.encode(&bytes).into()),
                            ]),
                        ),
                    ]),
                    RequestOptions::default(),
                )
                .await?;
            main.queue()
                .send(
                    &"alice".into(),
                    &Json::object([
                        ("action", "send".into()),
                        ("operationId", super::operation("image-input")),
                        ("expectedBridge", main.admission().expected(&"alice".into())),
                        (
                            "channel",
                            field(main.admission().definition(), "channels")
                                .as_array()
                                .unwrap()[0]
                                .required("channel")?
                                .clone(),
                        ),
                        ("messageId", "image-input".into()),
                        ("expectedBinding", field(&current.value, "binding").clone()),
                        (
                            "payload",
                            Json::object([
                                ("text", "".into()),
                                (
                                    "images",
                                    Json::Array(vec![Json::object([
                                        (
                                            "object",
                                            Json::object([
                                                (
                                                    "objectId",
                                                    field(field(&image, "object"), "id").clone(),
                                                ),
                                                ("revision", Json::Number(1.0)),
                                            ]),
                                        ),
                                        ("contentHash", digest(&bytes).into()),
                                        ("byteLength", Json::Number(bytes.len() as f64)),
                                        ("mediaType", "image/png".into()),
                                        ("label", "Synthetic image".into()),
                                    ])]),
                                ),
                            ]),
                        ),
                    ]),
                )
                .await
        }
        "cancel-first" => {
            let selected = input(main).await?;
            ChatCancellation::new(main.operations, main, notice)
                .cancel(
                    &"alice".into(),
                    &Json::object([
                        ("action", "cancel".into()),
                        ("operationId", super::operation("delivery-cancel")),
                        ("expectedBridge", main.admission().expected(&"alice".into())),
                        ("inputId", selected.pin.object_id.into()),
                        (
                            "expectedRevision",
                            Json::Number(selected.pin.revision as f64),
                        ),
                        ("reason", "Original delivery cancellation.".into()),
                    ]),
                )
                .await
        }
        "diagnostic-find" => Ok(DiagnosticMain::find(main)
            .await?
            .map(super::document)
            .unwrap_or(Json::Null)),
        "diagnostic-ready" => {
            DiagnosticMain::ready(main).await?;
            Ok(Json::Null)
        }
        "diagnostic-notice" => {
            DiagnosticMain::notice(
                main,
                &"producer".into(),
                &Json::object([("operationId", super::operation("main-notice"))]),
            )
            .await
        }
        other => {
            let _ = other;
            main_step(action, main, client, notice).await
        }
    }
}

#[tokio::test]
async fn original_delivery_and_worker_use_real_main_native_and_collection() {
    let oracle = oracle();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let bytes = STANDARD.decode(text(oracle, "instructions")).unwrap();
    let instructions = parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX)
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let bytes = STANDARD.decode(text(oracle, "noticeInstructions")).unwrap();
    let notice = NoticeInstructions::from_json(
        &parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX).unwrap(),
    )
    .unwrap();
    let now = field(oracle, "now").as_f64().unwrap() as i64;
    let mut steps = 0;
    let mut errors = 0;
    for case in field(oracle, "cases").as_array().unwrap() {
        let seed = field(field(oracle, "seeds"), &text(case, "seed"));
        let path = std::env::temp_dir().join(format!("ivy-rust-delivery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("chat-bridge.sqlite"),
            STANDARD.decode(text(seed, "database")).unwrap(),
        )
        .unwrap();
        let client = Arc::new(Client {
            path: path.clone(),
            state: Mutex::new(Replay {
                pending: VecDeque::new(),
                mutations: vec![],
                records: field(seed, "records").clone(),
            }),
        });
        let tools = Arc::new(ToolClients::new(client.clone()));
        let definition = field(seed, "definition").clone();
        let actions = field(case, "steps").as_array().unwrap();
        let mut index = 0;
        while index < actions.len() {
            let store = ChatStore::with_clock(
                client.clone(),
                None,
                Some(path.clone()),
                Arc::new(Fixed(now)),
            )
            .await
            .unwrap();
            let admission = ChatAdmission::new(&definition).unwrap();
            let ops = ChatOperations::new(&store, &admission).unwrap();
            let native = ChatNativeDriver::new(
                &store,
                tools.clone(),
                &admission,
                &catalogs,
                "chat-provider".into(),
                &instructions,
            )
            .unwrap();
            let main = ChatMain::new(&ops, &native, &notice).unwrap();
            let evidence = native.evidence().unwrap();
            let delivery = ChatDelivery::new(&main);
            let collector = ChatCollector::new(&ops, &evidence, &delivery, &catalogs);
            let clock = Arc::new(DrainClock {
                now,
                base: Instant::now(),
                elapsed_step: Mutex::new((0, 0)),
            });
            let mut worker = ChatWorker::with_clock(&delivery, &collector, clock.clone()).unwrap();
            loop {
                if index >= actions.len() {
                    break;
                }
                let action = &actions[index];
                let expected = &field(case, "expected").as_array().unwrap()[index];
                client.begin(field(expected, "transport"));
                let value = if text(action, "action") == "reopen" {
                    Ok(Json::Null)
                } else {
                    step(
                        action,
                        &main,
                        &delivery,
                        &mut worker,
                        &client,
                        &notice,
                        &clock,
                    )
                    .await
                };
                let actual = snapshot(&path, &client, result(value));
                assert!(
                    client.state.lock().unwrap().pending.is_empty(),
                    "Missing delivery RPC {} step {index}",
                    text(case, "label")
                );
                let mut expected = expected.clone();
                expected.remove("transport");
                equal(
                    &actual,
                    &expected,
                    &format!("Delivery {} step {index}", text(case, "label")),
                );
                if field(&actual, "output").get("error").is_some() {
                    errors += 1;
                }
                steps += 1;
                index += 1;
                if index >= actions.len() || text(&actions[index], "action") == "reopen" {
                    break;
                }
            }
        }
        std::fs::remove_dir_all(path).unwrap();
    }
    assert!(field(oracle, "cases").as_array().unwrap().len() >= 35);
    assert!(steps >= 150);
    eprintln!(
        "Original delivery/worker: {} workflows, {steps} steps, {errors} expected errors; real Main/native/collection/cancellation/publication composition.",
        field(oracle, "cases").as_array().unwrap().len()
    );
}
