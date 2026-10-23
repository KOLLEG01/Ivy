use super::*;
use ivy_phone_bridge::{
    desktop::VoiceInputOwner,
    media::EvsLibrary,
    micro::{
        MicroAttachmentStatus, MicroClose, MicroInputReceipt, MicroInputResult, MicroVoiceInput,
    },
    native_codec::CodecLibraryError,
};
use serde_json::{Value, json};
use std::path::Path;
#[path = "phone_native_composition_entry.rs"]
mod entry;
fn media_factory() -> Arc<CountedFactory> {
    Arc::new(CountedFactory {
        inner: NativeSipSessionFactory::new(
            Arc::new(Factory::routes(vec![])),
            None,
            Arc::new(Clock),
            |_| Arc::new(Events::default()),
        ),
        starts: AtomicUsize::new(0),
    })
}

fn save(name: &str, value: &Value) {
    if let Some(root) = std::env::var_os("IVY_PHONE_NATIVE_COMPOSITION_TRACE") {
        std::fs::write(
            Path::new(&root).join(name),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
fn outcome(value: NativeOutcome) -> Value {
    match value.error_code() {
        Some(code) => json!({"error":code}),
        None => {
            json!({"result":serde_json::from_str::<Value>(value.result_json().unwrap()).unwrap()})
        }
    }
}
fn project(method: &str, value: &Value) -> Value {
    if value.get("error").is_some() {
        return value.clone();
    }
    let v = &value["result"];
    match method {
        "status" | "heartbeat" => {
            json!({"result":{"configured":v["configured"],"call":if v["call"].is_null(){Value::Null}else{json!({"id":v["call"]["id"],"state":v["call"]["state"]})},"callIds":v["callIds"],"micro":v["micro"],"endpointPresent":v["endpoint"].is_string(),"mediaClosed":v["media"]["closed"],"mediaFailed":v["media"]["failed"]}})
        }
        "configure" => {
            json!({"result":{"configured":v["configured"],"endpointPresent":v["endpoint"].is_string()}})
        }
        "call.prepare" => {
            json!({"result":{"id":v["id"],"state":v["state"],"direction":v["direction"],"error":v["error"]}})
        }
        _ => value.clone(),
    }
}
fn steps() -> Vec<Value> {
    let id = json!({"callId":CALL});
    vec![
        json!({"method":"status","params":{}}),
        json!({"method":"heartbeat","params":{}}),
        json!({"method":"inventory","params":{}}),
        json!({"method":"inventory","params":{"extra":null}}),
        json!({"method":"codec.test","params":{"extra":null}}),
        json!({"method":"desktop.observe","params":{"extra":null}}),
        json!({"method":"call.prepare","params":id}),
        json!({"method":"configure","params":serde_json::from_str::<Value>(BASE).unwrap()}),
        json!({"method":"configure","params":{}}),
        json!({"method":"status","params":{}}),
        json!({"method":"call.prepare","params":id}),
        json!({"method":"status","params":id}),
        json!({"method":"codec.test","params":{}}),
        json!({"method":"inventory","params":{}}),
        json!({"method":"inventory","params":{"extra":null}}),
        json!({"method":"call.desktop.startVoice","params":{"callId":CALL}}),
        json!({"method":"call.release","params":id}),
        json!({"method":"status","params":{}}),
        json!({"method":"fixture.dispose","params":{}}),
        json!({"method":"status","params":{}}),
        json!({"method":"inventory","params":{}}),
        json!({"method":"inventory","params":{"extra":null}}),
        json!({"method":"codec.test","params":{}}),
        json!({"method":"codec.test","params":{"extra":null}}),
        json!({"method":"desktop.observe","params":{"extra":null}}),
        json!({"method":"desktop.launch","params":{}}),
    ]
}
fn original(steps: &[Value]) -> Value {
    let data: Value =
        serde_json::from_str(include_str!("../fixtures/phone-native-platform-cases.json")).unwrap();
    let expected = data["composition"]["expected"].clone();
    assert_eq!(steps.len(), expected.as_array().unwrap().len());
    expected
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_default_composition_shares_status_admission_and_release() {
    let media = media_factory();
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let loads = Arc::new(AtomicUsize::new(0));
    let count = loads.clone();
    let runtime =
        NativeSipCommands::windows(media.clone(), remaining.clone(), |_| Ok("null".into()))
            .with_diagnostics(
                move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    panic!("No admitted codec load in guard-only original corpus")
                },
                || Err(ivy_phone_bridge::native_inventory::InventoryError),
            );
    assert_eq!(loads.load(Ordering::SeqCst), 0);
    assert_eq!(media.starts.load(Ordering::SeqCst), 0);
    let steps = steps();
    let mut actual = Vec::new();
    let mut endpoint = None;
    for (i, step) in steps.iter().enumerate() {
        let method = step["method"].as_str().unwrap();
        let result = if method == "fixture.dispose" {
            runtime.shutdown().await.unwrap();
            json!({"result":{"disposed":true}})
        } else {
            outcome(
                runtime
                    .start(command(
                        method,
                        &step["params"].to_string(),
                        i as u64 + 1,
                        i as u64 + 1,
                    ))
                    .await
                    .await,
            )
        };
        if method == "configure" && result["result"]["endpoint"].is_string() {
            endpoint = Some(
                result["result"]["endpoint"]
                    .as_str()
                    .unwrap()
                    .parse::<std::net::SocketAddr>()
                    .unwrap(),
            );
        }
        if method == "call.prepare" && result["result"].is_object() {
            assert_eq!(result["result"]["id"], CALL);
            assert_eq!(result["result"]["state"], "prepared");
        }
        actual.push(result);
    }
    runtime.shutdown().await.unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 0);
    assert_eq!(media.starts.load(Ordering::SeqCst), 1);
    assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
    assert!(remaining.methods.lock().unwrap().is_empty());
    assert!(std::net::UdpSocket::bind(endpoint.unwrap()).is_ok());
    save("composition-rust.json", &json!(actual));
    let expected = original(&steps);
    let expected = expected.as_array().unwrap();
    assert_eq!(expected.len(), actual.len());
    let comparisons:Vec<_>=steps.iter().zip(actual.iter().zip(expected)).map(|(step,(actual,expected))|{
        let expected=if let Some(error)=expected.get("error"){json!({"error":error})}else{json!({"result":serde_json::from_str::<Value>(expected["json"].as_str().unwrap()).unwrap()})};
        let method=step["method"].as_str().unwrap();let a=project(method,actual);let b=project(method,&expected);
        json!({"method":method,"equal":a==b,"rust":a,"original":b})
    }).collect();
    save(
        "composition-comparisons.json",
        &json!({"cases":steps.len(),"comparisons":comparisons,"mediaFactories":1,"codecLoads":0,"remainingCloses":1,"scope":"Actual default Desktop/input/diagnostic composition with one SIP owner and owned loopback media. Status/configure omit ephemeral endpoint values and unselected call diagnostics; call.prepare compares id/state/direction/error. No live Desktop query, input, device capture/playback or authenticated realtime."}),
    );
    assert!(comparisons.iter().all(|v| v["equal"] == true));
}

#[derive(Default)]
struct Blocking {
    open: Mutex<bool>,
    wake: Condvar,
}
impl Blocking {
    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.wake.wait(open).unwrap();
        }
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.wake.notify_all();
    }
}
struct Unblock(Arc<Blocking>);
impl Drop for Unblock {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct InputDevice(Arc<AtomicUsize>);
impl MicroVoiceInput for InputDevice {
    fn status(&self) -> MicroInputResult<MicroAttachmentStatus> {
        Ok(MicroAttachmentStatus {
            state: "ready".into(),
            generation: Some(1),
            error_code: None,
        })
    }
    fn close(&self) -> MicroInputResult<MicroClose> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(MicroClose::completed(Ok(())))
    }
    fn send_microphone(
        &self,
        _: bool,
        _: i64,
        _: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputResult<MicroInputReceipt> {
        panic!("No input dispatch authorized")
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn composed_reads_retain_work_and_final_input_cleanup_after_observer_loss() {
    let mut rows = Vec::new();
    for failure in [false, true] {
        let (sip, media, _, _) = actual_owner(57000, 57998);
        let remaining = Arc::new(Remaining::default());
        remaining
            .close_mode
            .store(usize::from(failure), Ordering::SeqCst);
        let input_closes = Arc::new(AtomicUsize::new(0));
        let selected = input_closes.clone();
        let input =
            VoiceInputOwner::new(move |_| Ok(Some(Arc::new(InputDevice(selected.clone())))));
        let gate = Arc::new(Blocking::default());
        let _unblock = Unblock(gate.clone());
        let loads = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let loaded = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let (library_gate, inventory_gate) = (gate.clone(), gate.clone());
        let (loading, reading, library_done, inventory_done) =
            (loads.clone(), reads.clone(), loaded.clone(), read.clone());
        let runtime =
            NativeSipCommands::with_voice_input(sip.clone(), remaining.clone(), input, |_| {
                Ok("null".into())
            })
            .with_diagnostics(
                move || -> Result<Arc<EvsLibrary>, CodecLibraryError> {
                    loading.fetch_add(1, Ordering::SeqCst);
                    library_gate.wait();
                    library_done.fetch_add(1, Ordering::SeqCst);
                    Err(CodecLibraryError::Missing)
                },
                move || {
                    let n = reading.fetch_add(1, Ordering::SeqCst) + 1;
                    inventory_gate.wait();
                    inventory_done.fetch_add(1, Ordering::SeqCst);
                    Ok(IvyJson::object([("read", IvyJson::Number(n as f64))]))
                },
            );
        let mut config: Value = serde_json::from_str(BASE).unwrap();
        config["micro"] = json!({"usbipExecutable":"C:\\inert\\usbip.exe","executableHash":format!("sha256:{}","a".repeat(64)),"port":32409});
        let configured = runtime
            .start(command("configure", &config.to_string(), 1, 1))
            .await
            .await;
        let endpoint: std::net::SocketAddr = value(&configured)
            .get("endpoint")
            .unwrap()
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let diagnostic = runtime.start(command("codec.test", "{}", 2, 2));
        wait(|| loads.load(Ordering::SeqCst) == 1).await;
        drop(diagnostic);
        let lost = runtime.start(command("inventory", "{}", 3, 3));
        wait(|| reads.load(Ordering::SeqCst) == 1).await;
        drop(lost);
        let observed = runtime.invoke(command("inventory", "{}", 4, 4));
        wait(|| reads.load(Ordering::SeqCst) == 2).await;
        let prepared = runtime
            .start(command(
                "call.prepare",
                &json!({"callId":CALL}).to_string(),
                5,
                5,
            ))
            .await
            .await;
        assert!(prepared.error_code().is_none());
        assert_eq!(media.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            runtime
                .invoke(command("codec.test", "{}", 6, 6))
                .await
                .error_code(),
            Some("runtime_not_ready")
        );
        let stopped = runtime.shutdown();
        drop(stopped);
        wait(|| remaining.closes.load(Ordering::SeqCst) == 1).await;
        assert_eq!(input_closes.load(Ordering::SeqCst), 0);
        assert_eq!(read.load(Ordering::SeqCst), 0);
        assert_eq!(loaded.load(Ordering::SeqCst), 0);
        assert!(sip.observation().stopping);
        assert!(std::net::UdpSocket::bind(endpoint).is_ok());
        assert_eq!(
            runtime
                .invoke(command("codec.test", "{}", 7, 7))
                .await
                .error_code(),
            Some("runtime_stopping")
        );
        assert_eq!(
            runtime
                .invoke(command("inventory", "{}", 8, 8))
                .await
                .error_code(),
            Some("runtime_stopping")
        );
        assert_eq!(
            runtime
                .invoke(command("inventory", r#"{"extra":null}"#, 9, 9))
                .await
                .error_code(),
            Some("invalid_request")
        );
        remaining.close_gate.release();
        let result = timeout(Duration::from_secs(2), runtime.shutdown())
            .await
            .unwrap();
        assert_eq!(result.is_err(), failure);
        assert_eq!(input_closes.load(Ordering::SeqCst), 1);
        assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.shutdown().await.is_err(), failure);
        assert_eq!(input_closes.load(Ordering::SeqCst), 1);
        gate.release();
        assert_eq!(
            outcome(timeout(Duration::from_secs(2), observed).await.unwrap()),
            json!({"result":{"read":2}})
        );
        wait(|| read.load(Ordering::SeqCst) == 2 && loaded.load(Ordering::SeqCst) == 1).await;
        rows.push(json!({"remainingFailure":failure,"diagnosticsUnjoined":true,"reads":2,"libraryLoads":1,"inputCloses":1,"remainingCloses":1,"udpRetiredBeforePlatform":true,"activeCallSharesDiagnosticFence":true}));
    }
    save("composition-retention.json", &json!(rows));
}
