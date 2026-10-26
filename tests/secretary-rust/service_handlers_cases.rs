#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::{JsString, checked_native_contract::CheckedNativeContracts, json::canonical};
use ivy_sdk::{clock::Clock, service::InvocationContext};
use ivy_secretary::{
    engine::SecretaryEngine,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    service_handlers::*,
    store::Store,
};
use std::cell::{Cell, RefCell};

struct FixedClock(i64);
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
fn failure(value: &IvyJson) -> IvyError {
    let outcome = match value
        .required("outcome")
        .unwrap()
        .as_str()
        .unwrap()
        .as_str()
    {
        "unknown" => Outcome::Unknown,
        "completed" => Outcome::Completed,
        _ => Outcome::NotExecuted,
    };
    IvyError::new(
        value.required("code").unwrap().as_str().unwrap(),
        value.required("message").unwrap().as_str().unwrap(),
    )
    .with_outcome(outcome)
}
fn outcome(value: ivy_contracts::IvyResult<IvyJson>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(error) => IvyJson::object([
            ("code", error.code.into()),
            ("message", error.message.into()),
            ("outcome", error.outcome.to_json()),
        ]),
    }
}
#[tokio::test]
async fn original_public_handler_generation_identity_dispatch_and_wake_order() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = reference_data::read(include_bytes!("service-handlers-cases.json.gz"));
    let corpus = parse(
        std::str::from_utf8(&output).unwrap(),
        8 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 194);
    let names = IvyJson::Array(
        PublicMethod::ALL
            .into_iter()
            .map(|m| IvyJson::from(format!("secretary.{}", m.name())))
            .collect(),
    );
    assert_eq!(&names, corpus.required("handlerNames").unwrap());
    let retained = CheckedNativeContracts::new(root);
    for row in rows {
        let input = row.required("input").unwrap();
        let method = PublicMethod::ALL
            .into_iter()
            .find(|m| input.get("method") == Some(&IvyJson::from(m.name())))
            .unwrap();
        let label = format!(
            "{}/{}",
            method.name(),
            input.required("label").unwrap().as_str().unwrap()
        );
        let c = input.required("context").unwrap();
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled.")
                .with_outcome(Outcome::Unknown),
            ..RequestOptions::default()
        };
        if c.get("cancelled") == Some(&true.into()) {
            cancellation.cancellation.cancel();
        }
        let context = InvocationContext::detached(c
                .required("caller")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(), c
                .get("operationId")
                .and_then(IvyJson::as_js_string)
                .cloned(),c.required("generation").unwrap().as_f64().unwrap() as u64,cancellation.cancellation.clone(),cancellation.cancel_error.clone());
        let call = admit(
            method,
            input.required("args").unwrap(),
            &context,
            input
                .get("engineGeneration")
                .and_then(IvyJson::as_f64)
                .map(|v| v as u64),
            || {
                if cancellation.cancellation.is_cancelled() {
                    Err(cancellation.cancel_error.clone())
                } else {
                    Ok(())
                }
            },
        );
        let calls = RefCell::new(Vec::new());
        let wakes = Cell::new(0);
        let mut binding_rpc = None;
        let result = match call {
            Err(error) => Err(error),
            Ok(call) => {
                if call.method == EngineMethod::Binding {
                    let owner_error = input.get("ownerError").filter(|v| !v.is_null());
                    let step = if let Some(error) = owner_error {
                        let a = error.as_array().unwrap();
                        IvyJson::object([
                            ("method", "serviceNodes.get".into()),
                            (
                                "error",
                                IvyJson::object([
                                    ("code", a[0].clone()),
                                    ("message", a[1].clone()),
                                    ("outcome", a[2].clone()),
                                ]),
                            ),
                        ])
                    } else {
                        IvyJson::object([
                            ("method", "serviceNodes.get".into()),
                            (
                                "value",
                                IvyJson::object([
                                    ("connected", true.into()),
                                    ("synced", true.into()),
                                    ("principalId", "secretary-principal".into()),
                                    ("hostId", "fixture-host".into()),
                                    ("serviceName", "secretary".into()),
                                ]),
                            ),
                        ])
                    };
                    let client = Arc::new(Scripted {
                        steps: Mutex::new([step].into_iter().collect()),
                        trace: Mutex::default(),
                    });
                    let journal =
                        Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
                    let store = Store {
                        client: client.clone(),
                        root: "00000000-0000-4000-8000-000000000001".into(),
                        journal: &journal,
                    };
                    let node = "fixture.secretary".into();
                    let host = "fixture-host".into();
                    let evidence = InboxEvidence {
                        store: &store,
                        settings: corpus.required("settings").unwrap(),
                        service_node_id: &node,
                        cancellation: RequestOptions::default(),
                    };
                    let clock =
                        FixedClock(corpus.required("wall").unwrap().as_f64().unwrap() as i64);
                    let tools = Arc::new(ToolClients::new(client.clone()));
                    let issues = RefCell::new(RecoveryScan::default());
                    let engine = SecretaryEngine {
                        evidence: &evidence,
                        clock: &clock,
                        operation_clock: &clock,
                        runtime_epoch: epoch_fixture(&"fixture-epoch".into()),
                        new_uuid: || "00000000-0000-4000-8000-000000000fff".into(),
                        tools: tools.as_ref(),
                        issues: &issues,
                    };
                    let handler = SecretaryHandlers {
                        engine: &engine,
                        generation: context.generation,
                        service_node_id: &node,
                        host_id: &host,
                        tools: tools.clone(),
                        retained: &retained,
                    };
                    let result = call
                        .complete(handler.execute(&call), || wakes.set(wakes.get() + 1))
                        .await;
                    assert!(
                        client.steps.lock().unwrap().is_empty(),
                        "binding owner lookup: {label}"
                    );
                    binding_rpc = Some(client.trace.lock().unwrap().clone());
                    result
                } else {
                    call.complete(
                        async {
                            let value = IvyJson::object([
                                ("method", call.method.name().into()),
                                ("arguments", IvyJson::Array(call.arguments.clone())),
                            ]);
                            calls.borrow_mut().push(value.clone());
                            if let Some(error) = input.get("error").filter(|v| !v.is_null()) {
                                Err(failure(error))
                            } else {
                                Ok(value)
                            }
                        },
                        || wakes.set(wakes.get() + 1),
                    )
                    .await
                }
            }
        };
        let expected = row.required("expected").unwrap();
        assert_eq!(
            canonical(&outcome(result), 8 * 1024 * 1024).unwrap(),
            canonical(expected.required("result").unwrap(), 8 * 1024 * 1024).unwrap(),
            "{label}"
        );
        assert_eq!(
            IvyJson::Number(wakes.get() as f64),
            expected.required("wakes").unwrap().clone(),
            "wakes in {label}"
        );
        if let Some(trace) = binding_rpc {
            // Authorization uses the real engine; the original's two direct
            // method spies correspond to one actual authenticated owner RPC.
            assert_eq!(trace.len(), 1, "{label}");
            assert_eq!(
                trace[0].required("method").unwrap(),
                &IvyJson::from("serviceNodes.get")
            );
            assert_eq!(
                trace[0].required("params").unwrap(),
                &IvyJson::object([("serviceNodeId", "fixture.secretary".into())])
            );
        } else {
            assert_eq!(
                canonical(&IvyJson::Array(calls.into_inner()), 8 * 1024 * 1024).unwrap(),
                canonical(expected.required("calls").unwrap(), 8 * 1024 * 1024).unwrap(),
                "delegates in {label}"
            );
        }
    }
}

#[tokio::test]
async fn sdk_handler_bridge_keeps_accepted_request_after_abandoned_waiter() {
    let (sender, mut queue) = tokio::sync::mpsc::unbounded_channel();
    let callbacks = registered_handlers(sender);
    assert_eq!(callbacks.len(), 16);
    let context = InvocationContext::detached("caller-\u{1f989}".into(),Some("original".into()),7,RequestOptions::default().cancellation,RequestOptions::default().cancel_error);
    let handler = callbacks.get(&JsString::from("secretary.capture")).unwrap();
    let mut pending = handler(
        IvyJson::object([("operationId", "original".into())]),
        context.clone(),
    );
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(pending.as_mut().poll(cx).is_pending()))
            .await
    );
    // Cancellation of the observation does not remove the admitted queue entry.
    drop(pending);
    let request = queue.recv().await.unwrap();
    assert_eq!(request.method, PublicMethod::Capture);
    assert_eq!(
        request.context.caller_principal_id,
        context.caller_principal_id
    );
    assert_eq!(request.context.operation_id, context.operation_id);
    assert_eq!(request.context.generation, 7);
    assert_eq!(
        request.arguments,
        IvyJson::object([("operationId", "original".into())])
    );
    assert!(request.reply.is_closed());
    assert!(request.reply.send(Ok(IvyJson::Null)).is_err());
    drop(queue);
    let error = callbacks
        .get(&JsString::from("secretary.followUpRead"))
        .unwrap()(IvyJson::Null, context)
    .await
    .unwrap_err();
    assert_eq!(error.code, "service_not_ready");
    assert_eq!(error.message, "Secretary is reconnecting.");
}
