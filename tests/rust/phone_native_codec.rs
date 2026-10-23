use super::*;
use ivy_phone_bridge::{
    media::EvsLibrary,
    native_codec::{CodecLibraryError, codec_test},
    native_codec_commands::NativeCodecCommands,
};
use serde_json::{Value, json};
use std::path::Path;

fn library() -> Result<Arc<EvsLibrary>, CodecLibraryError> {
    let directory =
        std::env::var_os("IVY_PHONE_EVS_DIR").expect("Authenticated ABI2 fixture required");
    // SAFETY: The qualifier pins this authored/reference DLL and dependencies
    // before/after execution; it never loads a deployment or arbitrary library.
    unsafe { EvsLibrary::load_packaged(&PathBuf::from(directory)) }
        .map_err(|_| CodecLibraryError::Failed)
}
fn save(name: &str, value: &Value) {
    if let Some(root) = std::env::var_os("IVY_PHONE_NATIVE_CODEC_TRACE") {
        std::fs::write(
            Path::new(&root).join(name),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
fn value(out: NativeOutcome) -> Value {
    match out.error_code() {
        Some(error) => json!({"error":error}),
        None => {
            json!({"result":serde_json::from_str::<Value>(out.result_json().unwrap()).unwrap()})
        }
    }
}
fn projection(value: &Value) -> Value {
    if !value["error"].is_null() {
        return value.clone();
    }
    let codecs: Vec<_> = value["result"]["codecs"].as_array().unwrap().iter().map(|row| json!({"name":row["name"],"passed":row["passed"],"frames":row["frames"],"error":row["error"]})).collect();
    json!({"result":{"passed":value["result"]["passed"],"codecs":codecs}})
}
fn bounds(value: &Value) {
    assert_eq!(value["passed"], true, "{value}");
    let codecs = value["codecs"].as_array().unwrap();
    assert_eq!(codecs.len(), 5);
    for (row, name) in codecs.iter().zip(["G722", "PCMA", "PCMU", "OPUS", "EVS"]) {
        assert_eq!(row["name"], name);
        assert_eq!(row["passed"], true);
        assert_eq!(row["frames"], 25);
        assert!((25..=25 * 8192).contains(&row["encodedBytes"].as_u64().unwrap()));
        assert!((23000..=24000).contains(&row["decodedSamples"].as_u64().unwrap()));
        assert!((0.1..=0.55).contains(&row["rms"].as_f64().unwrap()));
        assert!(row["error"].is_null());
    }
}
async fn scenario(stage: &str, extra: bool) -> Value {
    let (sip, media, _, config) = actual_owner(57000, 57998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let loads = Arc::new(AtomicUsize::new(0));
    let count = loads.clone();
    let codec = NativeCodecCommands::new(sip.clone(), remaining, move || {
        count.fetch_add(1, Ordering::SeqCst);
        library()
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
        codec.shutdown().await.unwrap();
    }
    if stage == "sip-stopping" {
        sip.close().wait().await.unwrap();
    }
    let out = value(
        codec
            .invoke(command(
                "codec.test",
                if extra { r#"{"extra":null}"# } else { "{}" },
                1,
                1,
            ))
            .await,
    );
    codec.shutdown().await.unwrap();
    sip.close().wait().await.unwrap();
    assert_eq!(
        media.starts.load(Ordering::SeqCst),
        usize::from(stage == "call")
    );
    assert!(endpoint.is_none_or(|e| std::net::UdpSocket::bind(e).is_ok()));
    json!({"name":format!("{stage}:{extra}"),"outcome":out,"loads":loads.load(Ordering::SeqCst)})
}
fn original() -> Value {
    let data: Value =
        serde_json::from_str(include_str!("../fixtures/phone-native-platform-cases.json")).unwrap();
    data["codec"]["expected"].clone()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_codec_admission_roundtrip_bounds_and_cache_agree() {
    let mut actual = Vec::new();
    for stage in ["unconfigured", "ready", "call", "closed", "sip-stopping"] {
        for extra in [false, true] {
            actual.push(scenario(stage, extra).await);
        }
    }
    save("rust.json", &json!(actual));
    let expected = original();
    let rows = expected["scenarios"].as_array().unwrap();
    assert_eq!(actual.len(), rows.len());
    let mut comparisons = Vec::new();
    for (a, b) in actual.iter().zip(rows) {
        assert_eq!(a["name"], b["name"]);
        let rust = projection(&a["outcome"]);
        let original = projection(&b["outcome"]);
        comparisons
            .push(json!({"name":a["name"],"rust":rust,"original":original,"equal":rust==original}));
        if a["outcome"]["error"].is_null() {
            bounds(&a["outcome"]["result"]);
            bounds(&b["outcome"]["result"]);
        }
        assert_eq!(a["loads"], usize::from(a["outcome"]["error"].is_null()));
    }
    let (sip, _, _, config) = actual_owner(57000, 57998);
    sip.configure(&configuration(&config), |_| async { Ok(()) })
        .unwrap()
        .wait()
        .await
        .unwrap();
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let loads = Arc::new(AtomicUsize::new(0));
    let count = loads.clone();
    let codec = NativeCodecCommands::new(sip.clone(), remaining, move || {
        count.fetch_add(1, Ordering::SeqCst);
        library()
    });
    let first = codec.invoke(command("codec.test", "{}", 1, 1)).await;
    let call = sip.prepare(CALL.into()).unwrap();
    assert_eq!(
        codec
            .invoke(command("codec.test", "{}", 2, 2))
            .await
            .error_code(),
        Some("runtime_not_ready")
    );
    sip.release(&call).unwrap().wait().await.unwrap();
    let cached = codec.invoke(command("codec.test", "{}", 3, 3)).await;
    assert_eq!(first.result_json(), cached.result_json());
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    codec.shutdown().await.unwrap();
    sip.close().wait().await.unwrap();
    assert_eq!(expected["cacheSameInstance"], true);
    assert_eq!(expected["cacheWhileCall"], "runtime_not_ready");
    let strict_metrics_equal = actual
        .iter()
        .zip(rows)
        .all(|(a, b)| a["outcome"] == b["outcome"]);
    save(
        "comparisons.json",
        &json!({"cases":10,"comparisons":comparisons,"rust":actual,"original":expected,"rustCacheLoads":1,"cacheBytesExact":true,"strictMetricsEqual":strict_metrics_equal,"projection":"Compare name/pass/frames/error and guard order; both independently satisfy original decodedSamples/RMS bounds. Full actual encodedBytes/decodedSamples/rms remain in raw replies. Production codec/resampler measurements are reported verbatim; numeric equality is observed separately from admission/roundtrip bounds, without postprocessing. CPU synthetic tone only; owned SIP loopback setup and inert audio factory; no device/audio capture or network peer."}),
    );
    assert!(comparisons.iter().all(|r| r["equal"] == true));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_readers_cannot_cancel_one_diagnostic_or_delay_shutdown() {
    let (sip, _, _, _) = actual_owner(57000, 57998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let loads = Arc::new(AtomicUsize::new(0));
    let count = loads.clone();
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
    let codec = NativeCodecCommands::new(sip.clone(), remaining.clone(), move || {
        count.fetch_add(1, Ordering::SeqCst);
        let mut released = worker.0.lock().unwrap();
        while !*released {
            released = worker.1.wait(released).unwrap();
        }
        library()
    });
    let original = codec.start(command("codec.test", "{}", 1, 1));
    wait(|| loads.load(Ordering::SeqCst) == 1).await;
    let second = codec.invoke(command("codec.test", "{}", 2, 2));
    drop(original);
    drop(second);
    // Keep one observer while close proves it does not join the blocking worker.
    let observer = codec.invoke(command("codec.test", "{}", 3, 3));
    timeout(Duration::from_secs(2), codec.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
    assert_eq!(
        codec
            .invoke(command("codec.test", "{}", 4, 4))
            .await
            .error_code(),
        Some("runtime_stopping")
    );
    assert_eq!(
        codec
            .invoke(command("codec.test", r#"{"extra":null}"#, 5, 5))
            .await
            .error_code(),
        Some("invalid_request")
    );
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    bounds(&value(timeout(Duration::from_secs(20), observer).await.unwrap())["result"]);
    codec.shutdown().await.unwrap();
    sip.close().wait().await.unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
    save(
        "retention.json",
        &json!({"loads":1,"droppedReaders":2,"preCloseObserverCompletes":true,"shutdownBeforeLibraryRelease":true,"remainingShutdowns":1,"newReadsStopped":true}),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn library_failure_classes_and_panicked_loader_remain_cached() {
    let mut rows = Vec::new();
    for (error, code) in [
        (CodecLibraryError::Missing, "codec_library_missing"),
        (
            CodecLibraryError::Incompatible,
            "codec_library_incompatible",
        ),
        (CodecLibraryError::Failed, "codec_roundtrip_failed"),
    ] {
        let row: Value =
            serde_json::from_str(&stringify(&codec_test(Err(error)), 65536).unwrap()).unwrap();
        assert_eq!(row["passed"], false);
        for r in &row["codecs"].as_array().unwrap()[..4] {
            assert_eq!(r["passed"], true);
            assert_eq!(r["frames"], 25);
        }
        assert_eq!(
            row["codecs"][4],
            json!({"name":"EVS","passed":false,"frames":0,"encodedBytes":0,"decodedSamples":0,"rms":0,"error":code})
        );
        rows.push(row);
    }
    let (sip, _, _, _) = actual_owner(57000, 57998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let loads = Arc::new(AtomicUsize::new(0));
    let count = loads.clone();
    let codec = NativeCodecCommands::new(sip.clone(), remaining.clone(), move || {
        count.fetch_add(1, Ordering::SeqCst);
        panic!("synthetic authenticated-loader failure")
    });
    for request in 1..=2 {
        assert_eq!(
            codec
                .invoke(command("codec.test", "{}", request, request))
                .await
                .error_code(),
            Some("operation_outcome_unknown")
        );
    }
    assert_eq!(
        codec
            .invoke(command("inventory", "{}", 3, 3))
            .await
            .result_json(),
        Some("{\"callerOwned\":true}")
    );
    assert_eq!(remaining.methods.lock().unwrap().len(), 1);
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    codec.shutdown().await.unwrap();
    sip.close().wait().await.unwrap();
    save(
        "failures.json",
        &json!({"injectedLoadClasses":rows,"panickedLoaderInvocations":1,"unknownRetained":true,"otherMethodsDelegate":true,"limitation":"Injected loader classes, not executable package authentication or OS loader classification qualification."}),
    );
}
