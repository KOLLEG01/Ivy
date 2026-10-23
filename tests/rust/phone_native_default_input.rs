use super::*;
use crate::{micro::MicroClientPins, native_config::NativeMicroValue};
use ivy_contracts::json::parse;
use std::{net::TcpListener, path::PathBuf, time::Instant};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ivy-phone-default-input-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-phone-default-input-")
        );
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}
fn micro(root: &Path, port: u16, mode: &str) -> Value {
    let folder = if mode == "missing-directory" {
        root.join("missing")
    } else {
        root.into()
    };
    if mode == "wrong-files" {
        for name in ["usbip.exe", "libusbip.dll", "resources.dll"] {
            std::fs::write(folder.join(name), "inert fixture bytes, never executable").unwrap();
        }
    }
    let hash = if mode == "wrong-hash" {
        "0".repeat(64)
    } else {
        MicroClientPins::release()
            .unwrap()
            .get("usbip.exe")
            .unwrap()
            .to_owned()
    };
    json!({"usbipExecutable":folder.join("usbip.exe"),"executableHash":format!("sha256:{hash}"),"port":port})
}
fn configuration(micro: Value) -> String {
    json!({"binding":{"address":"127.0.0.1","port":0,"transport":"udp","rtpPortRangeStart":57000,"rtpPortRangeEnd":57006},"codecs":{"preferences":["PCMA"]},"incomingPeers":[],"registration":null,"password":null,"micro":micro}).to_string()
}
fn assemble_default(other: Remaining) -> Arc<NativeSipCommands> {
    let media = Arc::new(Media(other.counts.clone()));
    Arc::new(NativeSipCommands::windows(media, Arc::new(other), |_| {
        panic!("No realtime call admitted")
    }))
}
async fn status(dispatcher: &NativeDispatcher, sequence: &mut usize) -> Value {
    *sequence += 1;
    project(
        "status",
        dispatcher
            .execute(request("status", "{}", 0, *sequence))
            .await,
    )
}
async fn settled(dispatcher: &NativeDispatcher, sequence: &mut usize, expected: &str) -> Value {
    let began = Instant::now();
    loop {
        let value = status(dispatcher, sequence).await;
        if value["micro"]["errorCode"] == expected {
            return value;
        }
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "Default input did not settle: {value}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn scenario(mode: &str) -> Value {
    let root = Directory::new();
    let port = port();
    let counts = Arc::new(Counts::default());
    let handler = assemble_default(remaining("quiet", counts.clone()));
    let dispatcher = NativeDispatcher::new(EPOCH.into(), 64, handler.clone()).unwrap();
    assert!(free(port));
    assert_eq!(counts.media.load(Ordering::SeqCst), 0);
    let mut held =
        (mode == "conflict-then-release").then(|| TcpListener::bind(("127.0.0.1", port)).unwrap());
    let params = configuration(if mode == "none" {
        Value::Null
    } else {
        micro(&root.0, port, mode)
    });
    let mut sequence = 0;
    let mut rows = vec![status(&dispatcher, &mut sequence).await];
    for (args, operation) in [(params.as_str(), 1), (params.as_str(), 1), ("{}", 2)] {
        sequence += 1;
        rows.push(project(
            "configure",
            dispatcher
                .execute(request("configure", args, operation, sequence))
                .await,
        ));
    }
    if held.is_some() {
        rows.push(settled(&dispatcher, &mut sequence, "micro_attachment_conflict").await);
        drop(held.take());
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
    rows.push(if mode == "none" {
        status(&dispatcher, &mut sequence).await
    } else {
        settled(
            &dispatcher,
            &mut sequence,
            if matches!(mode, "wrong-hash" | "wrong-files") {
                "micro_client_changed"
            } else {
                "micro_client_unavailable"
            },
        )
        .await
    });
    sequence += 1;
    rows.push(project(
        "heartbeat",
        dispatcher
            .execute(request("heartbeat", "{}", 0, sequence))
            .await,
    ));
    let occupied = !free(port);
    assert_eq!(occupied, mode != "none");
    let endpoint = handler.owner.observation().endpoint.unwrap();
    rows.push(close_result(handler.shutdown().await));
    rows.push(close_result(handler.shutdown().await));
    rows.push(status(&dispatcher, &mut sequence).await);
    assert_eq!(counts.media.load(Ordering::SeqCst), 0);
    assert_eq!(
        counts
            .trace
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.as_str() == "remaining")
            .count(),
        1
    );
    let released = free(port);
    assert!(released);
    json!({"name":mode,"rows":rows,"listenerOccupied":occupied,"listenerReleased":released,"bindingReleased":UdpSocket::bind(endpoint).is_ok(),"mediaFactories":0})
}
fn original(modes: &[&str]) -> Value {
    let data: Value =
        serde_json::from_str(include_str!("../fixtures/phone-native-platform-cases.json")).unwrap();
    assert_eq!(data["default"]["cases"], json!(modes));
    data["default"]["expected"].clone()
}
#[test]
fn default_factory_missing_client_conflict_retry_status_and_disposal_match_original() {
    let modes = [
        "none",
        "missing-directory",
        "missing-file",
        "wrong-hash",
        "wrong-files",
        "conflict-then-release",
    ];
    let actual = runtime().block_on(async {
        let mut out = Vec::new();
        for mode in modes {
            out.push(scenario(mode).await);
        }
        Value::Array(out)
    });
    trace("default-rust.json", &actual);
    let expected = original(&modes);
    let differences: Vec<_> = actual
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.as_array().unwrap())
        .filter(|(a, b)| a != b)
        .map(|(a, b)| json!({"rust":a,"original":b}))
        .collect();
    trace(
        "default-comparisons.json",
        &json!({"cases":modes.len(),"differences":differences,"rust":actual,"original":expected,"scope":"Unmodified default native/Desktop/Micro factory and no-call owners. Only isolated loopback listeners; release hashes never match fixture bytes, so no USB/IP client, driver, audio, UIA, input or call starts. Poll scheduling and ephemeral paths/ports are not compared."}),
    );
    assert_eq!(actual, expected);
}
#[test]
fn default_factory_is_lazy_and_final_cleanup_retains_the_real_listener() {
    let counts = Arc::new(Counts::default());
    let lazy = assemble_default(remaining("quiet", counts.clone()));
    assert!(!lazy.owner.observation().configured);
    assert_eq!(counts.media.load(Ordering::SeqCst), 0);
    drop(lazy);
    let root = Directory::new();
    let port = port();
    let raw = micro(&root.0, port, "missing-file").to_string();
    let settings = NativeMicroValue::read(&parse(&raw, 65536).unwrap(), &raw)
        .unwrap()
        .validate()
        .unwrap();
    let input = VoiceInputOwner::windows();
    let error = input.prepare_micro(settings).unwrap_err();
    assert!(error.is::<tokio::runtime::TryCurrentError>());
    assert!(free(port));
    let close = input.close().unwrap();
    assert!(close.result().unwrap().is_ok());
    assert!(close.same_work(&input.close().unwrap()));
    let values=runtime().block_on(async {
        let mut values=Vec::new();
        for mode in ["failed","pending"] {
            let root=Directory::new();let port=super::default_input::port();let counts=Arc::new(Counts::default());let other=remaining(mode,counts.clone());let entered=other.entered.clone();let release=other.release.clone();
            let handler=assemble_default(other);let dispatcher=NativeDispatcher::new(EPOCH.into(),64,handler.clone()).unwrap();let params=configuration(micro(&root.0,port,"missing-file"));
            let configured=dispatcher.execute(request("configure",&params,1,1)).await;assert!(configured.outcome().error_code().is_none());
            let mut sequence=1;settled(&dispatcher,&mut sequence,"micro_client_unavailable").await;assert!(!free(port));
            let waiter=handler.shutdown();drop(waiter);tokio::time::timeout(Duration::from_secs(3),entered.notified()).await.unwrap();
            if mode=="pending" {assert!(!free(port));assert!(tokio::time::timeout(Duration::from_millis(40),handler.shutdown()).await.is_err());release.notify_one();}
            let outcome=tokio::time::timeout(Duration::from_secs(3),handler.shutdown()).await.unwrap();assert_eq!(outcome.is_err(),mode=="failed");assert!(free(port));
            assert_eq!(handler.shutdown().await.is_err(),mode=="failed");assert_eq!(counts.trace.lock().unwrap().iter().filter(|s|s.as_str()=="remaining").count(),1);assert_eq!(counts.media.load(Ordering::SeqCst),0);
            values.push(json!({"mode":mode,"droppedObserverRetainedCleanup":true,"listenerReleased":true,"sameShutdownOutcome":true,"mediaFactories":0}));
        }
        values
    });
    trace(
        "default-controls.json",
        &json!({"constructorLazyOutsideRuntime":true,"missingRuntimeErrorPropagates":true,"noSocketOpenedBeforeRuntime":true,"emptyInputCloseMemoized":true,"shutdown":values,"scope":"Rust-only missing-runtime and injected remaining-layer failure/pending controls around the actual default Micro listener. No original OS-failure parity or live client/hardware effect."}),
    );
}
