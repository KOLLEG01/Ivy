// Include the exact implementation to inject a clock without adding test-only public APIs.
pub use ivy_sdk::{IvyError, IvyJson, IvyResult, JsString, clock, rpc};
mod implementation {
    include!("../../packages/sdk/rust/src/operation_ids.rs");
    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::rpc::RequestFuture;
        use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
        use std::time::Instant;

        struct FakeClock(AtomicI64);
        impl Clock for FakeClock {
            fn utc_millis(&self) -> i64 {
                self.0.load(Ordering::SeqCst)
            }
            fn monotonic(&self) -> Instant {
                Instant::now()
            }
        }
        struct Client {
            calls: AtomicUsize,
            completed: AtomicUsize,
            fail_first: bool,
        }
        impl RpcClient for Client {
            fn request<'a>(
                &'a self,
                method: &'a str,
                _: IvyJson,
                _: RequestOptions,
            ) -> RequestFuture<'a> {
                assert_eq!(method, "system.status");
                Box::pin(async move {
                    let call = self.calls.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    self.completed.fetch_add(1, Ordering::SeqCst);
                    if self.fail_first && call == 0 {
                        return Err(IvyError::new("offline", "offline"));
                    }
                    Ok(IvyJson::object([(
                        "runtimeEpoch",
                        if self.fail_first { "second" } else { "epoch" }.into(),
                    )]))
                })
            }
        }
        fn cache(clock: Arc<dyn Clock>) -> Arc<State> {
            Arc::new(State {
                entries: Mutex::new(Entries::default()),
                clock,
            })
        }
        fn client(fail_first: bool) -> Arc<Client> {
            Arc::new(Client {
                calls: AtomicUsize::new(0),
                completed: AtomicUsize::new(0),
                fail_first,
            })
        }

        #[tokio::test]
        async fn cache_lifetime_expiry_and_failures_match_typescript() {
            let clock = Arc::new(FakeClock(AtomicI64::new(1000)));
            let owner = cache(clock.clone());
            let client = client(false);
            let epochs = tokio::join!(owner.epoch(client.clone()), owner.epoch(client.clone()));
            let manual = owner
                .new_id(client.clone(), Some(JsString(vec![0xd800])))
                .await
                .unwrap();
            let first = owner
                .scoped(
                    client.clone(),
                    &IvyJson::object([("b", IvyJson::Number(2.0)), ("a", IvyJson::Number(1.0))]),
                )
                .unwrap()
                .await
                .unwrap();
            let key = IvyJson::object([("a", IvyJson::Number(1.0)), ("b", IvyJson::Number(2.0))]);
            let same = owner.scoped(client.clone(), &key).unwrap().await.unwrap();
            clock.0.store(1000 + 86_400_000 - 1, Ordering::SeqCst);
            let before = owner.scoped(client.clone(), &key).unwrap().await.unwrap();
            clock.0.fetch_add(1, Ordering::SeqCst);
            let after = owner.scoped(client.clone(), &key).unwrap().await.unwrap();
            clock.0.store(100_000_000, Ordering::SeqCst);
            owner
                .scoped(client.clone(), &"later".into())
                .unwrap()
                .await
                .unwrap();
            clock.0.store(0, Ordering::SeqCst);
            let earlier = owner
                .scoped(client.clone(), &"earlier".into())
                .unwrap()
                .await
                .unwrap();
            clock.0.store(86_400_001, Ordering::SeqCst);
            let retained = owner
                .scoped(client.clone(), &"earlier".into())
                .unwrap()
                .await
                .unwrap();
            let retry = self::client(true);
            let retry_owner = cache(clock.clone());
            let a = retry_owner.scoped(retry.clone(), &"scope".into()).unwrap();
            let b = retry_owner.scoped(retry.clone(), &"scope".into()).unwrap();
            let (a, b) = tokio::join!(a, b);
            assert!(a.is_err() && b.is_err());
            let recovered = retry_owner
                .scoped(retry.clone(), &"scope".into())
                .unwrap()
                .await
                .unwrap();
            let invalid = self::client(false);
            let invalid_code =
                match cache(clock).scoped(invalid.clone(), &"x".repeat(65_537).into()) {
                    Err(error) => error.code,
                    Ok(_) => panic!("unbounded scope"),
                };
            let actual = IvyJson::object([
                (
                    "epochs",
                    IvyJson::Array(vec![
                        IvyJson::String(epochs.0.unwrap()),
                        IvyJson::String(epochs.1.unwrap()),
                    ]),
                ),
                ("manual", IvyJson::String(manual)),
                ("same", (first == same).into()),
                ("before", (first == before).into()),
                ("expired", (first != after).into()),
                (
                    "issuedAt",
                    IvyJson::Number(
                        ivy_contracts::operation::parse_operation_id_js(&after)
                            .unwrap()
                            .issued_at_unix_ms as f64,
                    ),
                ),
                ("retainedBehindLater", (earlier == retained).into()),
                (
                    "calls",
                    IvyJson::Number(client.calls.load(Ordering::SeqCst) as f64),
                ),
                (
                    "retryCalls",
                    IvyJson::Number(retry.calls.load(Ordering::SeqCst) as f64),
                ),
                (
                    "failed",
                    IvyJson::Array(vec!["rejected".into(), "rejected".into()]),
                ),
                (
                    "recoveredEpoch",
                    IvyJson::String(
                        ivy_contracts::operation::parse_operation_id_js(&recovered)
                            .unwrap()
                            .runtime_epoch,
                    ),
                ),
                (
                    "invalidCalls",
                    IvyJson::Number(invalid.calls.load(Ordering::SeqCst) as f64),
                ),
                ("invalidCode", invalid_code.into()),
            ]);
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
            let output = std::process::Command::new("node")
                .args([
                    "--experimental-transform-types",
                    "--disable-warning=ExperimentalWarning",
                    "tests/fixtures/operation-ids-oracle.mjs",
                ])
                .current_dir(root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let expected =
                ivy_contracts::json::parse(std::str::from_utf8(&output.stdout).unwrap(), 65_536)
                    .unwrap();
            assert_eq!(
                canonical(&actual, 65_536).unwrap(),
                canonical(&expected, 65_536).unwrap()
            );
        }

        #[tokio::test]
        async fn public_helpers_share_by_client_and_do_not_retain_it() {
            use ivy_sdk::operation_ids::{new_operation_id, runtime_epoch, scoped_operation_id};

            let client = client(false);
            let weak = Arc::downgrade(&client);
            let first = scoped_operation_id(client.clone(), &"scope".into())
                .await
                .unwrap();
            let second = scoped_operation_id(client.clone(), &"scope".into())
                .await
                .unwrap();
            assert_eq!(first, second);
            assert_eq!(
                runtime_epoch(client.clone()).await.unwrap(),
                JsString::from("epoch")
            );
            assert_eq!(client.calls.load(Ordering::SeqCst), 1);
            assert_ne!(
                first,
                new_operation_id(client.clone(), Some("explicit".into()))
                    .await
                    .unwrap()
            );
            let other = self::client(false);
            assert_ne!(
                first,
                scoped_operation_id(other, &"scope".into()).await.unwrap()
            );
            drop(client);
            tokio::task::yield_now().await;
            assert!(weak.upgrade().is_none());
        }

        #[tokio::test]
        async fn dropping_all_waiters_does_not_cancel_the_shared_request() {
            let owner = cache(Arc::new(SystemClock));
            let client = client(false);
            drop(owner.scoped(client.clone(), &"scope".into()).unwrap());
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while client.completed.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("started request must complete even without any caller polling it");
            let epoch = owner.epoch(client.clone()).await.unwrap();
            assert_eq!(epoch, JsString::from("epoch"));
            assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[cfg(test)]
mod started_epoch {
    use futures_util::FutureExt;
    use ivy_sdk::{
        IvyError, IvyJson, IvyResult, JsString, Outcome,
        operation_ids::{RuntimeEpochWaiter, runtime_epoch, start_runtime_epoch},
        rpc::{RequestFuture, RequestOptions, RpcClient},
    };
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::sync::oneshot;

    enum Step {
        Ready(IvyResult<IvyJson>),
        Pending(oneshot::Receiver<IvyResult<IvyJson>>),
    }
    struct Client {
        steps: Mutex<VecDeque<Step>>,
        calls: Mutex<Vec<String>>,
        completed: AtomicUsize,
    }
    impl Client {
        fn new(steps: Vec<Step>) -> Arc<Self> {
            Arc::new(Self {
                steps: Mutex::new(steps.into()),
                calls: Mutex::new(Vec::new()),
                completed: AtomicUsize::new(0),
            })
        }
        fn trace(&self) -> IvyJson {
            IvyJson::Array(
                self.calls
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|method| method.clone().into())
                    .collect(),
            )
        }
    }
    impl RpcClient for Client {
        fn request<'a>(
            &'a self,
            method: &'a str,
            _: IvyJson,
            _: RequestOptions,
        ) -> RequestFuture<'a> {
            Box::pin(async move {
                // Record inside the future: constructing an unpolled request
                // must not satisfy the eager-start/initialization-order check.
                self.calls.lock().unwrap().push(method.to_owned());
                if method == "initialize" {
                    return Ok(true.into());
                }
                assert_eq!(method, "system.status");
                let step = self
                    .steps
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected epoch request");
                let result = match step {
                    Step::Ready(result) => result,
                    Step::Pending(receiver) => {
                        receiver.await.expect("controlled reply was dropped")
                    }
                };
                self.completed.fetch_add(1, Ordering::SeqCst);
                result
            })
        }
    }
    fn status(value: JsString) -> IvyResult<IvyJson> {
        Ok(IvyJson::object([("runtimeEpoch", IvyJson::String(value))]))
    }
    fn epoch() -> JsString {
        let mut value = JsString::from("epoch-");
        value.0.push(0xd800);
        value
    }
    fn failure(outcome: Outcome) -> IvyError {
        let mut message = vec![b'x' as u16; 2100];
        message.push(0xdfff);
        IvyError::new_js("service_unavailable".into(), JsString(message))
            .with_outcome(outcome)
            .with_details(IvyJson::object([
                ("text", IvyJson::String(JsString(vec![0xd800]))),
                (
                    "values",
                    IvyJson::Array(vec![IvyJson::Null, "e\u{301}".into()]),
                ),
            ]))
    }
    fn error(value: &IvyError) -> IvyJson {
        IvyJson::object([
            ("code", IvyJson::String(value.code_js())),
            ("message", IvyJson::String(value.message_js())),
            ("outcome", value.outcome.as_str().into()),
            ("details", value.details.clone().unwrap()),
        ])
    }
    async fn rejected(waiter: &RuntimeEpochWaiter, expected: &IvyError) -> IvyJson {
        let actual = waiter.get().await.unwrap_err();
        assert_eq!(&actual, expected);
        error(&actual)
    }
    fn compare(name: &str, actual: IvyJson) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let output = std::process::Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/runtime-epoch-oracle.mjs",
                name,
            ])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected =
            ivy_contracts::json::parse(std::str::from_utf8(&output.stdout).unwrap(), 65_536)
                .unwrap();
        assert_eq!(
            ivy_contracts::json::canonical(&actual, 65_536).unwrap(),
            ivy_contracts::json::canonical(&expected, 65_536).unwrap(),
            "{name}"
        );
    }

    #[tokio::test]
    async fn pending_handle_starts_before_independent_initialize_and_clones_share_it() {
        let (send, receive) = oneshot::channel();
        let client = Client::new(vec![Step::Pending(receive)]);
        let first = start_runtime_epoch(client.clone());
        let second = first.clone();
        let same = start_runtime_epoch(client.clone());
        let after_start = client.trace();
        assert_eq!(*client.calls.lock().unwrap(), ["system.status"]);
        client
            .request("initialize", IvyJson::Null, RequestOptions::default())
            .await
            .unwrap();
        let after_initialize = client.trace();
        let pending = first.get().now_or_never().is_none();
        assert!(pending);
        send.send(status(epoch())).unwrap();
        let values = vec![
            IvyJson::String(first.get().await.unwrap()),
            IvyJson::String(second.get().await.unwrap()),
        ];
        assert_eq!(same.get().await.unwrap(), epoch());
        let cached = start_runtime_epoch(client.clone()).get().await.unwrap();
        let legacy = runtime_epoch(client.clone()).await.unwrap();
        compare(
            "pending-success",
            IvyJson::object([
                ("afterStart", after_start),
                ("afterInitialize", after_initialize),
                ("pendingBeforeRelease", pending.into()),
                ("values", IvyJson::Array(values)),
                ("cached", IvyJson::String(cached)),
                ("legacy", IvyJson::String(legacy)),
                ("calls", client.trace()),
            ]),
        );
        let weak = Arc::downgrade(&client);
        drop(client);
        assert!(
            weak.upgrade().is_none(),
            "completed retained waiters must not keep the client alive"
        );
        assert_eq!(first.get().await.unwrap(), epoch());
    }

    #[tokio::test]
    async fn failed_handles_keep_full_original_errors_after_a_new_owner_retries() {
        for outcome in [Outcome::NotExecuted, Outcome::Unknown, Outcome::Completed] {
            let (send, receive) = oneshot::channel();
            let client = Client::new(vec![
                Step::Pending(receive),
                Step::Ready(status("second".into())),
            ]);
            let first = start_runtime_epoch(client.clone());
            let second = start_runtime_epoch(client.clone());
            let after_start = client.trace();
            let original = failure(outcome);
            send.send(Err(original.clone())).unwrap();
            let failures = vec![
                rejected(&first, &original).await,
                rejected(&second, &original).await,
            ];
            let recovered = start_runtime_epoch(client.clone()).get().await.unwrap();
            let retained = vec![
                rejected(&first, &original).await,
                rejected(&second, &original).await,
            ];
            compare(
                &format!("failure-{}", outcome.as_str()),
                IvyJson::object([
                    ("afterStart", after_start),
                    ("failures", IvyJson::Array(failures)),
                    ("recovered", IvyJson::String(recovered)),
                    ("retained", IvyJson::Array(retained)),
                    ("calls", client.trace()),
                ]),
            );
        }
    }

    #[tokio::test]
    async fn immediately_rejected_requests_remain_shared_until_asynchronous_settlement() {
        let original = failure(Outcome::Unknown);
        let client = Client::new(vec![
            Step::Ready(Err(original.clone())),
            Step::Ready(status("second".into())),
        ]);
        let first = start_runtime_epoch(client.clone());
        let second = start_runtime_epoch(client.clone());
        let after_start = client.trace();
        assert_eq!(*client.calls.lock().unwrap(), ["system.status"]);
        let failures = vec![
            rejected(&first, &original).await,
            rejected(&second, &original).await,
        ];
        let recovered = start_runtime_epoch(client.clone()).get().await.unwrap();
        let retained = rejected(&first, &original).await;
        compare(
            "ready-failure",
            IvyJson::object([
                ("afterStart", after_start),
                ("failures", IvyJson::Array(failures)),
                ("recovered", IvyJson::String(recovered)),
                ("retained", retained),
                ("calls", client.trace()),
            ]),
        );
    }

    #[tokio::test]
    async fn dropping_all_handles_keeps_the_driver_and_success_or_failure_cache_cleanup() {
        for failed in [false, true] {
            let (send, receive) = oneshot::channel();
            let client = Client::new(vec![
                Step::Pending(receive),
                Step::Ready(status("second".into())),
            ]);
            let waiter = start_runtime_epoch(client.clone());
            let after_start = client.trace();
            let abandoned = tokio::spawn({
                let waiter = waiter.clone();
                async move { waiter.get().await }
            });
            tokio::task::yield_now().await;
            abandoned.abort();
            let _ = abandoned.await;
            drop(waiter);
            send.send(if failed {
                Err(failure(Outcome::Unknown))
            } else {
                status(epoch())
            })
            .unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                while Arc::strong_count(&client) != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the independent driver must finish and release the request client");
            let completed_before = client.completed.load(Ordering::SeqCst);
            assert_eq!(completed_before, 1);
            let recovered = start_runtime_epoch(client.clone()).get().await.unwrap();
            compare(
                if failed {
                    "abandoned-failure"
                } else {
                    "abandoned-success"
                },
                IvyJson::object([
                    ("afterStart", after_start),
                    (
                        "completedBeforeRetry",
                        IvyJson::Number(completed_before as f64),
                    ),
                    ("recovered", IvyJson::String(recovered)),
                    ("calls", client.trace()),
                    (
                        "completed",
                        IvyJson::Number(client.completed.load(Ordering::SeqCst) as f64),
                    ),
                ]),
            );
        }
    }
}
