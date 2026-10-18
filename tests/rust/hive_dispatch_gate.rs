use ivy_contracts::{
    IvyError, IvyJson, Outcome,
    json::{parse, stringify},
};
use ivy_hive::admission::DispatchGate;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{runtime::Builder, sync::oneshot};

fn number(value: &Value, default: f64) -> f64 {
    match value.get("$number").and_then(Value::as_str) {
        Some("nan") => f64::NAN,
        Some("infinity") => f64::INFINITY,
        Some("negativeInfinity") => f64::NEG_INFINITY,
        Some("negativeZero") => -0.0,
        Some(other) => panic!("Unknown fixture number: {other}"),
        None => value.as_f64().unwrap_or(default),
    }
}
fn wire(value: &Value) -> IvyJson {
    parse(&value.to_string(), usize::MAX).unwrap()
}
fn error(value: &Value) -> IvyError {
    let message = match value["messageUnits"].as_array() {
        Some(units) => {
            ivy_contracts::JsString(units.iter().map(|v| v.as_u64().unwrap() as u16).collect())
        }
        None => value["message"].as_str().unwrap_or("Work failed.").into(),
    };
    let outcome = match value["outcome"].as_str().unwrap_or("unknown") {
        "not_executed" => Outcome::NotExecuted,
        "completed" => Outcome::Completed,
        "unknown" => Outcome::Unknown,
        other => panic!("Unknown fixture outcome: {other}"),
    };
    let result = IvyError::new_js(
        value["code"].as_str().unwrap_or("remote_error").into(),
        message,
    )
    .with_outcome(outcome);
    match value.get("details") {
        Some(details) => result.with_details(wire(details)),
        None => result,
    }
}
async fn flush() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}
fn compare(traces: Vec<Value>) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut oracle =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .current_dir(source)
            .arg("tests/fixtures/hive-dispatch-gate-oracle.mjs")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    oracle
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&traces).unwrap().as_bytes())
        .unwrap();
    let output = oracle.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    assert_eq!(expected.as_array().unwrap().len(), traces.len());
    for (trace, expected) in traces.iter().zip(expected.as_array().unwrap()) {
        let runtime = Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        runtime.block_on(async {
            let gate = DispatchGate::new(
                number(&trace["options"]["maximum"], 1024.0),
                number(&trace["options"]["waitMs"], 30_000.0),
            );
            let events = Arc::new(Mutex::new(Vec::<IvyJson>::new()));
            let mut controls = HashMap::<u64, oneshot::Sender<Result<IvyJson, IvyError>>>::new();
            let mut watchers = HashMap::new();
            let steps = trace["steps"].as_array().unwrap();
            let expected = expected.get("rows").unwrap().as_array().unwrap();
            assert_eq!(steps.len(), expected.len());
            for (index, step) in steps.iter().enumerate() {
                let id = step["id"].as_u64().unwrap_or(0);
                match step["op"].as_str().unwrap() {
                    "run" => {
                        let (input, reply) = oneshot::channel();
                        assert!(controls.insert(id, input).is_none(), "Reused job identity");
                        let work_events = events.clone();
                        let result = gate.run(move || async move {
                            work_events.lock().unwrap().push(IvyJson::Array(vec![
                                "start".into(),
                                IvyJson::Number(id as f64),
                            ]));
                            reply.await.expect("Fixture must complete accepted work")
                        });
                        let watcher_events = events.clone();
                        watchers.insert(
                            id,
                            tokio::spawn(async move {
                                let result = result.await.expect("Executor must remain alive");
                                let event = match result {
                                    Ok(value) => IvyJson::Array(vec![
                                        "ok".into(),
                                        IvyJson::Number(id as f64),
                                        value,
                                    ]),
                                    Err(error) => IvyJson::Array(vec![
                                        "error".into(),
                                        IvyJson::Number(id as f64),
                                        error.to_wire(),
                                    ]),
                                };
                                watcher_events.lock().unwrap().push(event);
                            }),
                        );
                        if step["immediate"] == true {
                            let _ = controls.remove(&id).unwrap().send(Ok(wire(&step["value"])));
                        }
                    }
                    "complete" => {
                        if let Some(input) = controls.remove(&id) {
                            let result = match step.get("error") {
                                Some(value) => Err(error(value)),
                                None => Ok(wire(&step["value"])),
                            };
                            let _ = input.send(result);
                        }
                    }
                    "drop" => watchers.get(&id).expect("Unknown result").abort(),
                    "advance" => {
                        tokio::time::advance(Duration::from_millis(step["ms"].as_u64().unwrap()))
                            .await
                    }
                    other => panic!("Unknown fixture operation: {other}"),
                }
                // A bulk deadline can wake all 1023 queued callers. Give each
                // expected ready event a bounded number of scheduler turns;
                // the paused clock cannot advance while this task yields.
                for _ in 0..32 + expected[index].as_array().unwrap().len() * 4 {
                    tokio::task::yield_now().await;
                }
                let actual = IvyJson::Array(std::mem::take(&mut *events.lock().unwrap()));
                // Shared wire conversion preserves lone UTF16 units and the
                // original JSON normalization of diagnostic nonfinite numbers.
                let actual = parse(&stringify(&actual, usize::MAX).unwrap(), usize::MAX).unwrap();
                let actual = actual.as_array().unwrap();
                let expected = expected[index].as_array().unwrap();
                assert_eq!(
                    actual.len(),
                    expected.len(),
                    "{} step {index}: {step}",
                    trace["name"]
                );
                for (event, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                    assert_eq!(
                        actual, expected,
                        "{} step {index}, event {event}: {step}",
                        trace["name"]
                    );
                }
            }
            // Resolve all accepted work before retiring this synthetic runtime.
            for (_, input) in controls {
                let _ = input.send(Ok(IvyJson::Null));
            }
            for _ in 0..steps.len() * 4 + 32 {
                tokio::task::yield_now().await;
            }
            for (_, watcher) in watchers {
                watcher.abort();
            }
        });
    }
}

#[test]
fn dispatch_fifo_expiry_result_errors_and_dropped_callers_match_original() {
    let mut default_steps = vec![json!({"op":"run","id":0})];
    default_steps.extend((1..=1024).map(|id| json!({"op":"run","id":id})));
    default_steps.extend([
        json!({"op":"advance","ms":29999}),
        json!({"op":"complete","id":0,"value":{"first":true}}),
        json!({"op":"advance","ms":1}),
        json!({"op":"run","id":1025}),
        json!({"op":"complete","id":1,"value":[1,2,3]}),
        json!({"op":"complete","id":1025,"value":"after expiry"}),
    ]);
    compare(vec![
        json!({"name":"default1024 slots/default30s queue-only expiry","steps":default_steps}),
        json!({"name":"FIFO,full,rejection,no later expired execution","options":{"maximum":4,"waitMs":10},"steps":[
            {"op":"run","id":0},{"op":"advance","ms":2},{"op":"run","id":1},
            {"op":"advance","ms":2},{"op":"run","id":2},{"op":"run","id":3},
            {"op":"run","id":4},{"op":"advance","ms":8},{"op":"run","id":5},
            {"op":"complete","id":0,"error":{"code":"provider_error","message":"failed","outcome":"completed","details":{"kept":true}}},
            {"op":"advance","ms":50},{"op":"complete","id":2,"value":"running is not timed out"},
            {"op":"run","id":6,"immediate":true,"value":42}
        ]}),
        json!({"name":"completed queued input, dropped active and waiting result","options":{"maximum":3,"waitMs":5},"steps":[
            {"op":"run","id":0},{"op":"drop","id":0},{"op":"run","id":1},
            {"op":"drop","id":1},{"op":"complete","id":1,"value":"already ready"},
            {"op":"run","id":2},{"op":"run","id":3},
            {"op":"complete","id":0,"value":"abandoned"},{"op":"complete","id":2,"error":{"messageUnits":[55296,120,56320],"outcome":"not_executed"}},
            {"op":"advance","ms":20},{"op":"run","id":4,"immediate":true,"value":{"reused":true}}
        ]}),
    ]);
}

#[test]
fn dispatch_numeric_slot_and_node_timer_boundaries_match_original() {
    let mut traces = Vec::new();
    for maximum in [
        json!(0),
        json!(-1),
        json!(1),
        json!(1.5),
        json!({"$number":"nan"}),
        json!({"$number":"infinity"}),
        json!({"$number":"negativeInfinity"}),
    ] {
        traces.push(json!({"name":format!("maximum {maximum}"),"options":{"maximum":maximum,"waitMs":4},"steps":[
            {"op":"run","id":0},{"op":"run","id":1},{"op":"run","id":2},
            {"op":"complete","id":0,"value":"first"},{"op":"advance","ms":4},
            {"op":"complete","id":1,"value":"second"},{"op":"run","id":3,"immediate":true,"value":true}
        ]}));
    }
    for wait in [
        json!(-1),
        json!(0),
        json!(0.5),
        json!(1.9),
        json!(4),
        json!(4.9),
        json!(2147483647_u64),
        json!(2147483648_u64),
        json!({"$number":"nan"}),
        json!({"$number":"infinity"}),
        json!({"$number":"negativeInfinity"}),
    ] {
        traces.push(
            json!({"name":format!("timer {wait}"),"options":{"waitMs":wait},"steps":[
                {"op":"run","id":0},{"op":"run","id":1},{"op":"advance","ms":0},
                {"op":"advance","ms":1},{"op":"advance","ms":2},{"op":"advance","ms":1},
                {"op":"complete","id":0,"value":"active"},{"op":"advance","ms":2147483647_u64},
                {"op":"complete","id":1,"value":"if started, keep running"}
            ]}),
        );
    }
    compare(traces);
}

#[test]
fn dispatch_seeded_interleaved_arrival_completion_and_deadline_traces_match_original() {
    let mut traces = Vec::new();
    for seed in [17_u64, 91, 12648430] {
        let mut random = seed;
        let mut next = 0;
        let mut steps = Vec::new();
        for _ in 0..600 {
            random = random
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            match random % 5 {
                0 | 1 => {
                    steps.push(json!({"op":"run","id":next,"immediate":random%13==0,"value":next}));
                    next += 1;
                }
                2 if next > 0 => {
                    steps.push(json!({"op":"complete","id":(random>>16)%next,"value":random>>32}))
                }
                3 if next > 0 => steps.push(json!({"op":"drop","id":(random>>16)%next})),
                _ => steps.push(json!({"op":"advance","ms":(random>>24)%5})),
            }
        }
        traces.push(json!({"name":format!("seed {seed}"),"options":{"maximum":7,"waitMs":11},"steps":steps}));
    }
    compare(traces);
}

#[test]
fn dispatch_is_eager_releases_after_result_and_retains_work_without_receivers() {
    Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(async {
            let gate = DispatchGate::new(1.0, 1.0);
            let first = gate.run(|| async { Ok(7) });
            // Slot admission happens before either future or caller is polled.
            let rejected = gate.run(|| async { Ok(9) });
            assert_eq!(rejected.await.unwrap().unwrap_err().code, "limit_exceeded");
            assert_eq!(first.await.unwrap().unwrap(), 7);
            assert_eq!(
                gate.run(|| async { Ok(10) })
                    .await
                    .unwrap()
                    .unwrap_err()
                    .code,
                "limit_exceeded"
            );
            flush().await;
            assert_eq!(
                gate.run(|| async { Ok("new result type") })
                    .await
                    .unwrap()
                    .unwrap(),
                "new result type"
            );
            flush().await;
            let (input, reply) = oneshot::channel();
            drop(gate.run(|| async move { Ok(reply.await.unwrap()) }));
            assert_eq!(
                gate.run(|| async { Ok(0) })
                    .await
                    .unwrap()
                    .unwrap_err()
                    .code,
                "limit_exceeded"
            );
            input.send(42).unwrap();
            flush().await;
            assert_eq!(gate.run(|| async { Ok(11) }).await.unwrap().unwrap(), 11);
        });
}

#[test]
fn dispatch_native_task_failure_drains_queue_and_executor_loss_is_distinct() {
    Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let gate = DispatchGate::new(2.0, 30_000.0);
            let failed = gate.run(|| async {
                panic!("synthetic native task failure");
                #[allow(unreachable_code)]
                Ok::<_, IvyError>(())
            });
            let next = gate.run(|| async { Ok(19) });
            assert!(failed.await.is_err());
            assert_eq!(next.await.unwrap().unwrap(), 19);
        });
    let runtime = Builder::new_current_thread().enable_time().build().unwrap();
    let (unpolled, gate) = runtime.block_on(async {
        let gate = DispatchGate::default();
        let result = gate.run(|| async {
            std::future::pending::<()>().await;
            Ok::<_, IvyError>(())
        });
        (result, gate)
    });
    drop(runtime);
    assert!(unpolled.blocking_recv().is_err());
    // The completion guard also exists before the task's first poll.
    Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            assert_eq!(gate.run(|| async { Ok(23) }).await.unwrap().unwrap(), 23);
        });
}

#[test]
fn dispatch_drops_expired_captures_and_retains_queue_after_gate_handles_are_dropped() {
    struct Capture(Arc<AtomicUsize>);
    impl Drop for Capture {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(async {
            let gate = DispatchGate::new(2.0, 2.0);
            let (finish, reply) = oneshot::channel();
            let active = gate.run(|| async move {
                reply.await.unwrap();
                Ok(())
            });
            let released = Arc::new(AtomicUsize::new(0));
            let capture = Capture(released.clone());
            let expired = gate.run(move || async move {
                drop(capture);
                panic!("Expired work must never start");
                #[allow(unreachable_code)]
                Ok(())
            });
            flush().await;
            tokio::time::advance(Duration::from_millis(2)).await;
            flush().await;
            assert_eq!(
                expired.await.unwrap().unwrap_err().code,
                "deadline_exceeded"
            );
            assert_eq!(released.load(Ordering::SeqCst), 1);
            let accepted = gate.run(|| async { Ok(37) });
            drop(gate);
            finish.send(()).unwrap();
            active.await.unwrap().unwrap();
            assert_eq!(accepted.await.unwrap().unwrap(), 37);
            assert_eq!(released.load(Ordering::SeqCst), 1);
        });
}

#[test]
fn dispatch_clones_allow_only_one_active_job_under_concurrent_submission() {
    Builder::new_multi_thread()
        .worker_threads(4)
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let gate = DispatchGate::new(64.0, 30_000.0);
            let active = Arc::new(AtomicUsize::new(0));
            let barrier = Arc::new(tokio::sync::Barrier::new(33));
            let mut callers = Vec::new();
            for id in 0..32 {
                let gate = gate.clone();
                let active = active.clone();
                let barrier = barrier.clone();
                callers.push(tokio::spawn(async move {
                    barrier.wait().await;
                    gate.run(move || async move {
                        assert_eq!(
                            active.fetch_add(1, Ordering::SeqCst),
                            0,
                            "Overlapping dispatch work"
                        );
                        tokio::task::yield_now().await;
                        assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                        Ok(id)
                    })
                    .await
                    .unwrap()
                    .unwrap()
                }));
            }
            barrier.wait().await;
            for (id, caller) in callers.into_iter().enumerate() {
                assert_eq!(caller.await.unwrap(), id);
            }
            assert_eq!(active.load(Ordering::SeqCst), 0);
        });
}
