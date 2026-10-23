use super::*;
use ivy_phone_bridge::native_inventory::{InventoryError, NativeInventoryCommands};
use serde_json::{Value, json};
use std::path::Path;
fn save(name: &str, value: &Value) {
    if let Some(root) = std::env::var_os("IVY_PHONE_NATIVE_INVENTORY_TRACE") {
        std::fs::write(
            Path::new(&root).join(name),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
fn outcome(v: NativeOutcome) -> Value {
    if let Some(error) = v.error_code() {
        json!({"error":error})
    } else {
        json!({"result":serde_json::from_str::<Value>(v.result_json().unwrap()).unwrap()})
    }
}
async fn scenario(stage: &str, extra: bool) -> Value {
    let (sip, media, _, config) = actual_owner(57000, 57998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let inventory = NativeInventoryCommands::with_reader(sip.clone(), remaining, || {
        Ok(IvyJson::object([("readOnlyFixture", true.into())]))
    });
    let mut endpoint = None;
    if stage != "unconfigured" {
        endpoint = Some(
            sip.configure(&configuration(&config), |_| async { Ok(()) })
                .unwrap()
                .wait()
                .await
                .unwrap(),
        );
    }
    if stage == "call" {
        sip.prepare(CALL.into()).unwrap();
    }
    if stage == "closed" {
        inventory.shutdown().await.unwrap();
    }
    if stage == "sip-stopping" {
        sip.close().wait().await.unwrap();
    }
    let v = outcome(
        inventory
            .start(command(
                "inventory",
                if extra { r#"{"extra":null}"# } else { "{}" },
                1,
                1,
            ))
            .await
            .await,
    );
    inventory.shutdown().await.unwrap();
    sip.close().wait().await.unwrap();
    assert_eq!(
        media.starts.load(Ordering::SeqCst),
        usize::from(stage == "call")
    );
    assert!(endpoint.is_none_or(|e| std::net::UdpSocket::bind(e).is_ok()));
    json!({"name":format!("{stage}:{extra}"),"outcome":v})
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inventory_read_only_admission_preserves_guard_order_and_reader_result() {
    let mut actual = Vec::new();
    for stage in ["unconfigured", "ready", "call", "closed", "sip-stopping"] {
        for extra in [false, true] {
            actual.push(scenario(stage, extra).await);
        }
    }
    save("inventory-rust.json", &json!(actual));
    assert_eq!(actual.len(), 10);
    for row in actual {
        let name = row["name"].as_str().unwrap();
        let expected = if name.ends_with(":true") {
            json!({"error":"invalid_request"})
        } else if name.starts_with("closed:") || name.starts_with("sip-stopping:") {
            json!({"error":"runtime_stopping"})
        } else {
            json!({"result":{"readOnlyFixture":true}})
        };
        assert_eq!(row["outcome"], expected, "{name}");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_inventory_read_retains_work_after_unpolled_start_and_shutdown() {
    let (sip, _, _, _) = actual_owner(57000, 57998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let started = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let reads = started.clone();
    let done = finished.clone();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker = gate.clone();
    struct Release(Arc<(Mutex<bool>, Condvar)>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.0.lock().unwrap() = true;
            self.0.1.notify_all();
        }
    }
    let _release = Release(gate.clone());
    let inventory =
        NativeInventoryCommands::with_reader(sip.clone(), remaining.clone(), move || {
            let n = reads.fetch_add(1, Ordering::SeqCst) + 1;
            let mut open = worker.0.lock().unwrap();
            while !*open {
                open = worker.1.wait(open).unwrap();
            }
            done.fetch_add(1, Ordering::SeqCst);
            Ok(IvyJson::object([("read", IvyJson::Number(n as f64))]))
        });
    let lost = inventory.start(command("inventory", "{}", 1, 1));
    wait(|| started.load(Ordering::SeqCst) == 1).await;
    drop(lost);
    let observer = inventory.invoke(command("inventory", "{}", 2, 2));
    wait(|| started.load(Ordering::SeqCst) == 2).await;
    timeout(Duration::from_secs(2), inventory.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
    assert_eq!(finished.load(Ordering::SeqCst), 0);
    assert_eq!(
        inventory
            .invoke(command("inventory", "{}", 3, 3))
            .await
            .error_code(),
        Some("runtime_stopping")
    );
    assert_eq!(
        inventory
            .invoke(command("inventory", r#"{"extra":null}"#, 4, 4))
            .await
            .error_code(),
        Some("invalid_request")
    );
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert_eq!(
        outcome(timeout(Duration::from_secs(2), observer).await.unwrap()),
        json!({"result":{"read":2}})
    );
    wait(|| finished.load(Ordering::SeqCst) == 2).await;
    inventory.shutdown().await.unwrap();
    sip.close().wait().await.unwrap();
    assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
    save(
        "inventory-retention.json",
        &json!({"reads":2,"notMemoized":true,"unpolledStartRetainsRead":true,"shutdownBeforeReadsFinish":true,"bothReadsFinish":true,"remainingCloses":1}),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inventory_failure_and_panic_are_unknown_and_next_read_is_independent() {
    let mut rows = Vec::new();
    for panic in [false, true] {
        let (sip, _, _, _) = actual_owner(57000, 57998);
        let remaining = Arc::new(Remaining::default());
        remaining.close_gate.release();
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let inventory =
            NativeInventoryCommands::with_reader(sip.clone(), remaining.clone(), move || {
                if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    if panic {
                        panic!("synthetic inventory enumeration panic")
                    }
                    return Err(InventoryError);
                }
                Ok(IvyJson::object([("sample", true.into())]))
            });
        assert_eq!(
            inventory
                .invoke(command("inventory", "{}", 1, 1))
                .await
                .error_code(),
            Some("operation_outcome_unknown")
        );
        assert_eq!(
            outcome(inventory.invoke(command("inventory", "{}", 2, 2)).await),
            json!({"result":{"sample":true}})
        );
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        let delegated = inventory
            .start(command("codec.test", "{}", 3, 3))
            .await
            .await;
        assert_eq!(delegated.result_json(), Some("{\"callerOwned\":true}"));
        assert_eq!(remaining.methods.lock().unwrap().len(), 1);
        inventory.shutdown().await.unwrap();
        sip.close().wait().await.unwrap();
        rows.push(json!({"panic":panic,"reads":2,"firstUnknownSecondNewResult":true,"remainingMethods":1}));
    }
    save("inventory-failures.json", &json!(rows));
}
