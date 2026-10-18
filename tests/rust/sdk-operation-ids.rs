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
