// Include the exact implementation to inject a clock without adding test-only public APIs.
pub use ivy_sdk::{IvyError, IvyJson, IvyResult, JsString, clock, rpc};
mod implementation {
    include!("../../packages/sdk/rust/src/operation_ids.rs");

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::rpc::RequestFuture;
        use ivy_contracts::operation::parse_operation_id_js;
        use ivy_sdk::{Outcome, operation_ids as sdk};
        use std::{
            sync::atomic::{AtomicI64, AtomicUsize, Ordering},
            time::{Duration, Instant},
        };
        use tokio::sync::oneshot;

        struct FakeClock(AtomicI64);
        impl Clock for FakeClock {
            fn utc_millis(&self) -> i64 {
                self.0.load(Ordering::SeqCst)
            }
            fn monotonic(&self) -> Instant {
                Instant::now()
            }
        }
        enum Step {
            Ready(IvyResult<IvyJson>),
            Pending(oneshot::Receiver<IvyResult<IvyJson>>),
        }
        struct Client {
            steps: Mutex<VecDeque<Step>>,
            calls: AtomicUsize,
            completed: AtomicUsize,
        }
        impl Client {
            fn new(steps: Vec<Step>) -> Arc<Self> {
                Arc::new(Self {
                    steps: Mutex::new(steps.into()),
                    calls: AtomicUsize::new(0),
                    completed: AtomicUsize::new(0),
                })
            }
            fn calls(&self) -> usize {
                self.calls.load(Ordering::SeqCst)
            }
        }
        impl RpcClient for Client {
            fn request<'a>(
                &'a self,
                method: &'a str,
                params: IvyJson,
                _: RequestOptions,
            ) -> RequestFuture<'a> {
                Box::pin(async move {
                    assert_eq!(method, "system.status");
                    assert_eq!(params, IvyJson::object(Vec::<(String, IvyJson)>::new()));
                    self.calls.fetch_add(1, Ordering::SeqCst);
                    let step = self
                        .steps
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("extra request");
                    let result = match step {
                        Step::Ready(result) => result,
                        Step::Pending(reply) => reply.await.expect("controlled reply was dropped"),
                    };
                    self.completed.fetch_add(1, Ordering::SeqCst);
                    result
                })
            }
        }
        fn status(epoch: impl Into<JsString>) -> IvyResult<IvyJson> {
            Ok(IvyJson::object([(
                "runtimeEpoch",
                IvyJson::String(epoch.into()),
            )]))
        }
        fn ready() -> Arc<Client> {
            Client::new(vec![Step::Ready(status("epoch"))])
        }
        fn cache(clock: Arc<dyn Clock>) -> Arc<State> {
            Arc::new(State {
                entries: Mutex::new(Entries::default()),
                clock,
            })
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
        async fn released(client: &Arc<Client>) {
            tokio::time::timeout(Duration::from_secs(1), async {
                while Arc::strong_count(client) != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the independent driver must release the completed client");
        }

        #[tokio::test]
        async fn malformed_status_is_rejected_without_coercion_and_retried() {
            let mut malformed = vec![
                IvyJson::Null,
                true.into(),
                IvyJson::Number(1.0),
                "epoch".into(),
                IvyJson::Array(vec![]),
                IvyJson::object(Vec::<(String, IvyJson)>::new()),
            ];
            malformed.extend(
                [
                    IvyJson::Null,
                    true.into(),
                    IvyJson::Number(1.0),
                    IvyJson::Array(vec!["epoch".into()]),
                    IvyJson::Array(vec!["epoch:part".into()]),
                    IvyJson::object([
                        ("length", IvyJson::Number(1.0)),
                        ("toString", IvyJson::Null),
                    ]),
                ]
                .into_iter()
                .map(|value| IvyJson::object([("runtimeEpoch", value)])),
            );
            for response in malformed {
                // Every entry point uses the same typed lookup and failure eviction.
                for entry in 0..3 {
                    let owner = cache(Arc::new(SystemClock));
                    let client = Client::new(vec![
                        Step::Ready(Ok(response.clone())),
                        Step::Ready(status("recovered")),
                    ]);
                    let first = match entry {
                        0 => owner.epoch(client.clone()).await,
                        1 => owner.new_id(client.clone(), Some("nonce".into())).await,
                        _ => owner.scoped(client.clone(), &"scope".into()).unwrap().await,
                    }
                    .unwrap_err();
                    assert_eq!(first.code, "invalid_frame", "{response:?}");
                    assert_eq!(
                        owner.epoch(client.clone()).await.unwrap(),
                        JsString::from("recovered")
                    );
                    assert_eq!(client.calls(), 2);
                }
            }
        }

        #[tokio::test]
        async fn pending_public_waiters_share_and_retain_the_lossless_success() {
            let (send, receive) = oneshot::channel();
            let client = Client::new(vec![Step::Pending(receive)]);
            let epoch = JsString(vec![b'e' as u16, 0xd800]);
            let first = sdk::start_runtime_epoch(client.clone());
            let cloned = first.clone();
            let second = sdk::start_runtime_epoch(client.clone());
            tokio::task::yield_now().await;
            assert_eq!(client.calls(), 1);
            assert!(first.get().now_or_never().is_none());
            send.send(status(epoch.clone())).unwrap();
            for waiter in [&first, &cloned, &second] {
                assert_eq!(waiter.get().await.unwrap(), epoch);
            }
            assert_eq!(sdk::runtime_epoch(client.clone()).await.unwrap(), epoch);
            let nonce = JsString(vec![0xdfff]);
            let id = sdk::new_operation_id(client.clone(), Some(nonce.clone()))
                .await
                .unwrap();
            let parsed = parse_operation_id_js(&id).unwrap();
            assert_eq!(parsed.runtime_epoch, epoch);
            assert_eq!(parsed.nonce, nonce);
            assert_eq!(client.calls(), 1);
            released(&client).await;
            let weak = Arc::downgrade(&client);
            drop(client);
            assert!(weak.upgrade().is_none());
            assert_eq!(first.get().await.unwrap(), epoch);
        }

        #[tokio::test]
        async fn failed_public_waiters_keep_original_errors_after_retry() {
            for outcome in [Outcome::NotExecuted, Outcome::Unknown, Outcome::Completed] {
                let (send, receive) = oneshot::channel();
                let client =
                    Client::new(vec![Step::Pending(receive), Step::Ready(status("second"))]);
                let first = sdk::start_runtime_epoch(client.clone());
                let second = sdk::start_runtime_epoch(client.clone());
                let original = failure(outcome);
                send.send(Err(original.clone())).unwrap();
                for waiter in [&first, &second] {
                    assert_eq!(waiter.get().await.unwrap_err(), original);
                }
                assert_eq!(
                    sdk::runtime_epoch(client.clone()).await.unwrap(),
                    JsString::from("second")
                );
                for waiter in [&first, &second] {
                    assert_eq!(
                        waiter.get().await.unwrap_err().to_wire(),
                        original.to_wire()
                    );
                }
                assert_eq!(client.calls(), 2);
                released(&client).await;
            }
        }

        #[tokio::test]
        async fn dropping_all_public_waiters_keeps_success_and_failure_cleanup() {
            for failed in [false, true] {
                let (send, receive) = oneshot::channel();
                let client =
                    Client::new(vec![Step::Pending(receive), Step::Ready(status("second"))]);
                let waiter = sdk::start_runtime_epoch(client.clone());
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
                    status("epoch")
                })
                .unwrap();
                released(&client).await;
                assert_eq!(client.completed.load(Ordering::SeqCst), 1);
                assert_eq!(
                    sdk::runtime_epoch(client.clone()).await.unwrap(),
                    JsString::from(if failed { "second" } else { "epoch" })
                );
                assert_eq!(client.calls(), if failed { 2 } else { 1 });
            }
        }

        #[tokio::test]
        async fn public_scope_caches_are_separate_and_do_not_retain_clients() {
            for client in [ready(), ready()] {
                let weak = Arc::downgrade(&client);
                let first = sdk::scoped_operation_id(client.clone(), &"scope".into())
                    .await
                    .unwrap();
                assert_eq!(
                    first,
                    sdk::scoped_operation_id(client.clone(), &"scope".into())
                        .await
                        .unwrap()
                );
                assert_eq!(
                    sdk::runtime_epoch(client.clone()).await.unwrap(),
                    JsString::from("epoch")
                );
                assert_ne!(
                    first,
                    sdk::new_operation_id(client.clone(), Some("explicit".into()))
                        .await
                        .unwrap()
                );
                assert_eq!(client.calls(), 1);
                released(&client).await;
                drop(client);
                assert!(weak.upgrade().is_none());
            }
        }

        #[tokio::test]
        async fn canonical_scopes_share_until_24_hour_expiry() {
            let clock = Arc::new(FakeClock(AtomicI64::new(1000)));
            let owner = cache(clock.clone());
            let client = ready();
            let first = owner
                .scoped(
                    client.clone(),
                    &IvyJson::object([("b", IvyJson::Number(2.0)), ("a", IvyJson::Number(1.0))]),
                )
                .unwrap();
            let key = IvyJson::object([("a", IvyJson::Number(1.0)), ("b", IvyJson::Number(2.0))]);
            let same = owner.scoped(client.clone(), &key).unwrap();
            let (first_id, same_id) = tokio::join!(first.clone(), same);
            let first_id = first_id.unwrap();
            assert_eq!(first_id, same_id.unwrap());
            clock.0.store(1000 + 86_400_000 - 1, Ordering::SeqCst);
            assert_eq!(
                first_id,
                owner.scoped(client.clone(), &key).unwrap().await.unwrap()
            );
            clock.0.fetch_add(1, Ordering::SeqCst);
            let expired = owner.scoped(client.clone(), &key).unwrap().await.unwrap();
            assert_ne!(first_id, expired);
            assert_eq!(
                parse_operation_id_js(&expired).unwrap().issued_at_unix_ms,
                1000 + 86_400_000
            );
            assert_eq!(
                first.await.unwrap(),
                first_id,
                "retained waiters keep their original ID"
            );
            assert_eq!(client.calls(), 1);

            // Scope retention remains insertion ordered if the wall clock moves backwards.
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
            assert_eq!(
                earlier,
                owner
                    .scoped(client, &"earlier".into())
                    .unwrap()
                    .await
                    .unwrap()
            );
        }

        #[tokio::test]
        async fn scoped_failures_are_shared_evicted_and_retained_by_existing_waiters() {
            let owner = cache(Arc::new(SystemClock));
            let original = failure(Outcome::Unknown);
            let client = Client::new(vec![
                Step::Ready(Err(original.clone())),
                Step::Ready(status("second")),
            ]);
            let first = owner.scoped(client.clone(), &"scope".into()).unwrap();
            let second = owner.scoped(client.clone(), &"scope".into()).unwrap();
            let (a, b) = tokio::join!(first.clone(), second);
            assert_eq!(a.unwrap_err(), original);
            assert_eq!(b.unwrap_err(), original);
            let recovered = owner
                .scoped(client.clone(), &"scope".into())
                .unwrap()
                .await
                .unwrap();
            assert_eq!(
                parse_operation_id_js(&recovered).unwrap().runtime_epoch,
                JsString::from("second")
            );
            assert_eq!(first.await.unwrap_err().to_wire(), original.to_wire());
            assert_eq!(client.calls(), 2);
        }

        #[tokio::test]
        async fn abandoned_scopes_complete_and_failed_scopes_can_retry() {
            for failed in [false, true] {
                let (send, receive) = oneshot::channel();
                let owner = cache(Arc::new(SystemClock));
                let client =
                    Client::new(vec![Step::Pending(receive), Step::Ready(status("second"))]);
                drop(owner.scoped(client.clone(), &"scope".into()).unwrap());
                send.send(if failed {
                    Err(failure(Outcome::Unknown))
                } else {
                    status("epoch")
                })
                .unwrap();
                released(&client).await;
                assert_eq!(client.completed.load(Ordering::SeqCst), 1);
                assert_eq!(
                    owner.entries.lock().unwrap().scopes.len(),
                    if failed { 0 } else { 1 }
                );
                let recovered = owner
                    .scoped(client.clone(), &"scope".into())
                    .unwrap()
                    .await
                    .unwrap();
                assert_eq!(
                    parse_operation_id_js(&recovered).unwrap().runtime_epoch,
                    JsString::from(if failed { "second" } else { "epoch" })
                );
                assert_eq!(client.calls(), if failed { 2 } else { 1 });
            }
        }

        #[tokio::test]
        async fn identity_and_scope_bounds_remain_enforced() {
            let clock = Arc::new(FakeClock(AtomicI64::new(0)));
            let owner = cache(clock.clone());
            let client = ready();
            for time in [0, 9_007_199_254_740_991] {
                clock.0.store(time, Ordering::SeqCst);
                let nonce = JsString(vec![0xdfff; 128]);
                let id = owner
                    .new_id(client.clone(), Some(nonce.clone()))
                    .await
                    .unwrap();
                let parsed = parse_operation_id_js(&id).unwrap();
                assert_eq!(parsed.issued_at_unix_ms, time as u64);
                assert_eq!(parsed.nonce, nonce);
            }
            clock.0.store(0, Ordering::SeqCst);
            for nonce in ["", ":", &"x".repeat(129)] {
                assert_eq!(
                    owner
                        .new_id(client.clone(), Some(nonce.into()))
                        .await
                        .unwrap_err()
                        .code,
                    "invalid_arguments"
                );
            }
            for time in [-1, 9_007_199_254_740_992] {
                clock.0.store(time, Ordering::SeqCst);
                assert_eq!(
                    owner
                        .scoped(client.clone(), &"scope".into())
                        .unwrap()
                        .await
                        .unwrap_err()
                        .code,
                    "invalid_arguments"
                );
            }
            clock.0.store(0, Ordering::SeqCst);
            assert!(
                owner
                    .scoped(client.clone(), &"scope".into())
                    .unwrap()
                    .await
                    .is_ok()
            );
            assert_eq!(
                client.calls(),
                1,
                "identity failures keep the successful epoch cache"
            );
            for epoch in ["", "epoch:part"] {
                let invalid = Client::new(vec![Step::Ready(status(epoch))]);
                assert_eq!(
                    cache(clock.clone())
                        .new_id(invalid, Some("nonce".into()))
                        .await
                        .unwrap_err()
                        .code,
                    "invalid_arguments"
                );
            }
            let untouched = ready();
            let error = match owner.scoped(untouched.clone(), &"x".repeat(65_537).into()) {
                Err(error) => error,
                Ok(_) => panic!("unbounded scope"),
            };
            assert_eq!(error.code, "content_too_large");
            assert_eq!(untouched.calls(), 0);
        }
    }
}
