use super::*;
use ivy_contracts::{
    checked_native_contract::CheckedNativeContracts, operation::parse_operation_id_js,
};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_driver::{AssignmentDriver, AssignmentTick},
    assignment_execution::AssignmentExecution,
    assignment_native::AssignmentNative,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
};
use std::{
    cell::RefCell,
    future::{Future, IntoFuture, poll_fn},
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
};
use tokio::sync::Notify;

#[derive(Default)]
struct Gate {
    seen: AtomicBool,
    released: AtomicBool,
    changed: Notify,
}
impl Gate {
    async fn wait(&self) {
        self.seen.store(true, Ordering::SeqCst);
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.released.load(Ordering::SeqCst) {
                break;
            }
            notified.await;
        }
    }
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
}
struct DriverClient {
    scripted: Scripted,
    owner: Gate,
    row: Gate,
    completion_write: AtomicBool,
    completion_read: AtomicBool,
}
impl RpcClient for DriverClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.scripted.trace.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params.clone()),
                (
                    "timeout",
                    IvyJson::Number(options.timeout.as_millis() as f64),
                ),
            ]));
            let step = self
                .scripted
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("Unexpected extra RPC");
            assert_eq!(step.get("method"), Some(&method.into()), "RPC order");
            match step
                .get("gate")
                .and_then(IvyJson::as_js_string)
                .map(|s| s.to_utf8().unwrap())
                .as_deref()
            {
                Some("owner") => self.owner.wait().await,
                Some("row") => self.row.wait().await,
                None => {}
                _ => panic!("Unknown gate"),
            }
            // An original async RPC always resumes its caller asynchronously.
            tokio::task::yield_now().await;
            if method == "objects.write"
                && params
                    .required("content")?
                    .required("value")?
                    .get("preflightOutput")
                    != Some(&"".into())
            {
                self.completion_write.store(true, Ordering::SeqCst);
            }
            if method == "objects.read" && self.completion_write.load(Ordering::SeqCst) {
                self.completion_read.store(true, Ordering::SeqCst);
            }
            if let Some(error) = step.get("error") {
                return Err(IvyError::new(
                    error.required("code")?.as_str()?,
                    error.required("message")?.as_str()?,
                ));
            }
            Ok(step.required("value")?.clone())
        })
    }
}
struct FixedClock(i64);
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
type Watched = Pin<Box<dyn Future<Output = IvyResult<()>>>>;
#[derive(Default)]
struct Observation {
    work: Option<Watched>,
    result: Option<IvyResult<()>>,
}
impl Observation {
    fn tick(tick: AssignmentTick) -> Self {
        Self {
            work: Some(tick.into_future()),
            result: None,
        }
    }
    async fn poll(&mut self) {
        poll_fn(|cx| {
            if let Some(work) = &mut self.work {
                if let Poll::Ready(result) = work.as_mut().poll(cx) {
                    self.result = Some(result);
                    self.work = None;
                }
            }
            Poll::Ready(())
        })
        .await;
    }
}
async fn boundary(
    mut ready: impl FnMut() -> bool,
    tick: &mut Observation,
    drain: &mut Observation,
) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            tick.poll().await;
            drain.poll().await;
            if ready() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Controlled work did not reach its boundary");
}
async fn settled(tick: &mut Observation, drain: &mut Observation) {
    for _ in 0..16 {
        tick.poll().await;
        drain.poll().await;
        tokio::task::yield_now().await;
    }
}
fn checkpoint(tick: &Observation, drain: &Observation) -> IvyJson {
    IvyJson::object([
        ("tickDone", tick.result.is_some().into()),
        ("drainDone", drain.result.is_some().into()),
    ])
}
fn outcome(result: IvyResult<()>) -> IvyJson {
    match result {
        Ok(()) => IvyJson::object([("value", IvyJson::Null)]),
        Err(error) => IvyJson::object([
            ("code", error.code.into()),
            ("message", error.message.into()),
            ("outcome", error.outcome.as_str().into()),
        ]),
    }
}
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn original_runner_coalescing_background_interleaving_and_drain() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let scratch =
        Scratch(std::env::temp_dir().join(format!("secretary-driver-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-driver-oracle.mjs")
        .arg(&scratch.0)
        .current_dir(&root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().expect("Original Node oracle required");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        8 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 8);
    for row in rows {
        let input = row.required("input").unwrap();
        let label = input.required("label").unwrap().as_str().unwrap();
        let process_case = input.get("processCase") == Some(&IvyJson::Bool(true));
        let pid = ivy_host_runtime::node_path::to_native(
            input.required("pid").unwrap().as_js_string().unwrap(),
        );
        let release = ivy_host_runtime::node_path::to_native(
            input.required("release").unwrap().as_js_string().unwrap(),
        );
        assert!(pid.starts_with(&scratch.0) && release.starts_with(&scratch.0));
        if pid.exists() {
            std::fs::remove_file(&pid).unwrap();
        }
        if release.exists() {
            std::fs::remove_file(&release).unwrap();
        }
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        let client = Arc::new(DriverClient {
            scripted: Scripted {
                steps: Mutex::new(
                    input
                        .required("steps")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .iter()
                        .cloned()
                        .collect(),
                ),
                trace: Mutex::default(),
            },
            owner: Gate::default(),
            row: Gate::default(),
            completion_write: AtomicBool::new(false),
            completion_read: AtomicBool::new(false),
        });
        let store = Store {
            client: client.clone(),
            root: corpus
                .required("rootId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            journal: &journal,
        };
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..RequestOptions::default()
        };
        let service_node_id = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: corpus.required("settings").unwrap(),
            service_node_id: &service_node_id,
            cancellation: cancellation.clone(),
        };
        let clock = FixedClock(corpus.required("time").unwrap().as_f64().unwrap() as i64);
        let retained = CheckedNativeContracts::new(root.clone());
        let execution = AssignmentExecution {
            native: AssignmentNative {
                evidence: &evidence,
                tools: Arc::new(ToolClients::new(client.clone())),
            },
            clock: &clock,
            retained: &retained,
            notification_policy: corpus
                .required("notificationPolicy")
                .unwrap()
                .as_js_string()
                .unwrap(),
        };
        let id = input
            .required("initial")
            .unwrap()
            .required("executionId")
            .unwrap()
            .as_str()
            .unwrap();
        let issues = RefCell::new(RecoveryScan {
            cursor: None,
            issues: vec![
                ("unrelated".into(), "keep".into()),
                (format!("execution:{id}"), "old".into()),
            ],
        });
        #[cfg(windows)]
        let helper = Some(
            std::env::var_os("CARGO_TARGET_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| root.join("target"))
                .join("debug/ivy-job.exe"),
        );
        #[cfg(not(windows))]
        let helper = None;
        let (handle, driver) =
            AssignmentDriver::new(&execution, 0.0, &issues, root.clone(), helper);
        let first = handle.tick();
        let coalesced = handle.tick();
        let third = handle.clone().tick();
        drop(first);
        drop(third);
        let scenario = async {
            let mut tick = Observation::tick(coalesced);
            let mut drain = Observation::default();
            boundary(
                || client.owner.seen.load(Ordering::SeqCst),
                &mut tick,
                &mut drain,
            )
            .await;
            client.owner.release();
            let mut checkpoints = Vec::new();
            if process_case {
                boundary(
                    || pid.is_file() && client.row.seen.load(Ordering::SeqCst),
                    &mut tick,
                    &mut drain,
                )
                .await;
                let pending_drain = handle.drain();
                drain.work = Some(Box::pin(async move {
                    pending_drain.await;
                    Ok(())
                }));
                settled(&mut tick, &mut drain).await;
                checkpoints.push(checkpoint(&tick, &drain));
                if input.get("midScan") == Some(&IvyJson::Bool(true)) {
                    std::fs::write(&release, "release").unwrap();
                    boundary(
                        || {
                            if input.get("writeFailure") == Some(&IvyJson::Bool(true)) {
                                client.completion_write.load(Ordering::SeqCst)
                            } else {
                                client.completion_read.load(Ordering::SeqCst)
                            }
                        },
                        &mut tick,
                        &mut drain,
                    )
                    .await;
                    settled(&mut tick, &mut drain).await;
                    checkpoints.push(checkpoint(&tick, &drain));
                    client.row.release();
                } else {
                    client.row.release();
                    while tick.result.is_none() {
                        tick.poll().await;
                        drain.poll().await;
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    settled(&mut tick, &mut drain).await;
                    checkpoints.push(checkpoint(&tick, &drain));
                    handle.tick().await.unwrap();
                    if input.get("cancel") == Some(&IvyJson::Bool(true)) {
                        cancellation.cancellation.cancel();
                    } else {
                        std::fs::write(&release, "release").unwrap();
                    }
                }
                while drain.result.is_none() {
                    tick.poll().await;
                    drain.poll().await;
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            } else {
                while tick.result.is_none() {
                    tick.poll().await;
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                handle.drain().await;
            }
            tick.poll().await;
            let mut results = vec![outcome(tick.result.take().unwrap())];
            if !process_case {
                results.push(outcome(handle.tick().await));
            }
            handle.close().await;
            (results, checkpoints)
        };
        let (_, (results, checkpoints)) =
            tokio::time::timeout(std::time::Duration::from_secs(20), async {
                tokio::join!(driver.run(), scenario)
            })
            .await
            .expect("Driver/drain budget");
        if process_case {
            let child_pid = std::fs::read_to_string(&pid).unwrap();
            let output = std::process::Command::new("node").args(["-e", "try{process.kill(Number(process.argv[1]),0);process.exitCode=1}catch(e){if(e.code!=='ESRCH')throw e}", &child_pid]).output().unwrap();
            assert!(
                output.status.success(),
                "Child still observed after drain: {label}"
            );
        }
        let mut trace = client.scripted.trace.lock().unwrap().clone();
        for call in &mut trace {
            if call.get("method") == Some(&"objects.write".into()) {
                let mut params = call.required("params").unwrap().clone();
                parse_operation_id_js(
                    params
                        .required("mutationId")
                        .unwrap()
                        .as_js_string()
                        .unwrap(),
                )
                .unwrap();
                params.insert("mutationId", "<scoped-operation>".into());
                call.insert("params", params);
            }
        }
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            ("checkpoints", IvyJson::Array(checkpoints)),
            ("trace", IvyJson::Array(trace)),
            (
                "issues",
                IvyJson::Array(
                    issues
                        .borrow()
                        .issues
                        .iter()
                        .map(|(id, code)| {
                            IvyJson::Array(vec![id.clone().into(), code.clone().into()])
                        })
                        .collect(),
                ),
            ),
        ]);
        assert!(
            client.scripted.steps.lock().unwrap().is_empty(),
            "Unconsumed RPC: {label}"
        );
        assert_eq!(&actual, row.required("expected").unwrap(), "case {label}");
    }
}
