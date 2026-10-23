use super::*;
use crate::desktop::{
    AudioProcessOwnership, AudioProcessRecord, AudioProcessSnapshot, DesktopCaptureCorrelation,
    DesktopCaptureIdentity,
};

struct Processes {
    identity: AudioDesktopIdentity,
    foreign: bool,
}
impl AudioProcessSnapshot for Processes {
    fn read(&mut self, pid: i32) -> ActionResult<Option<AudioProcessRecord>> {
        Ok(Some(AudioProcessRecord {
            pid,
            parent_pid: if pid == self.identity.pid() {
                0
            } else {
                self.identity.pid()
            },
            session_id: if self.foreign && pid != self.identity.pid() {
                9
            } else {
                2
            },
            start_ticks: self.identity.creation_ticks(),
            image_path: Some(self.identity.image().into()),
        }))
    }
}
fn process(
    case: &Case,
    identity: &AudioDesktopIdentity,
    pid: i32,
) -> ActionResult<Option<AudioProcessOwnership>> {
    match case.failure {
        "process-argument" => return Err(DesktopActionError::Argument("fixture")),
        "process-error" => return Err(DesktopActionError::Unavailable),
        "process-null" => return Ok(None),
        _ => {}
    }
    AudioProcessOwnership::read(
        identity,
        pid,
        &|| Ok(case.state != "unavailable"),
        || {
            Ok(Some(Processes {
                identity: identity.clone(),
                foreign: case.state == "foreign",
            }))
        },
        &crate::media::image_equal,
    )
    .map(Some)
}
fn capture_owner(
    case: &Case,
    identity: &AudioDesktopIdentity,
    endpoint: &JsString,
) -> ActionResult<Option<DesktopCaptureCorrelation>> {
    match case.failure {
        "captureOwner-argument" => return Err(DesktopActionError::Argument("fixture")),
        "captureOwner-error" => return Err(DesktopActionError::Unavailable),
        "captureOwner-null" => return Ok(None),
        _ => {}
    }
    let state = match case.state {
        "none" => "none",
        "ambiguous" => "ambiguous",
        "unavailable" => "unavailable",
        _ => "matched",
    };
    Ok(Some(DesktopCaptureCorrelation {
        endpoint_id: endpoint.clone(),
        state,
        identity: if state == "matched" {
            Some(DesktopCaptureIdentity {
                instance_id: if case.image == "unpaired" {
                    JsString(vec![0xd800])
                } else if case.image.is_empty() {
                    "session".into()
                } else {
                    case.image.clone().into()
                },
                pid: case.pid,
                process_start_ticks: identity.creation_ticks(),
            })
        } else {
            None
        },
    }))
}
pub(super) fn wire(
    desktop: PhoneDesktopRuntime,
    case: &Case,
    trace: &Arc<Mutex<Vec<String>>>,
    sip: &NativeSipOperations,
) -> PhoneDesktopRuntime {
    let process_case = case.clone();
    let process_trace = trace.clone();
    let process_sip = sip.clone();
    let capture_case = case.clone();
    let capture_trace = trace.clone();
    let capture_sip = sip.clone();
    desktop
        .with_process(move |id, pid| {
            process_trace.lock().unwrap().push(format!(
                "process:{}:{}:{}:{}:{pid}",
                id.pid(),
                id.creation_ticks(),
                id.image().display(),
                id.application()
            ));
            if process_case.close_at == "process" {
                drop(process_sip.close());
            }
            process(&process_case, id, pid)
        })
        .with_capture_owner(move |id, endpoint| {
            capture_trace.lock().unwrap().push(format!(
                "captureOwner:{}:{}:{}:{}:{}",
                id.pid(),
                id.creation_ticks(),
                id.image().display(),
                id.application(),
                endpoint.to_utf8().unwrap()
            ));
            if capture_case.close_at == "captureOwner" {
                drop(capture_sip.close());
            }
            capture_owner(&capture_case, id, endpoint)
        })
}
fn params(method: &str, identity: &str, arg: &str) -> String {
    format!(
        r#"{{"desktop":{identity},"{}":{arg}}}"#,
        if method == "desktop.process" {
            "pid"
        } else {
            "endpointId"
        }
    )
}
fn valid(method: &str) -> String {
    params(
        method,
        IDENTITY,
        if method == "desktop.process" {
            "202"
        } else {
            r#""capture-fixture""#
        },
    )
}
#[tokio::test]
async fn ownership_validation_projection_and_uncancelled_reads_agree() {
    let mut rows = vec![];
    for (method, args, error) in [
        ("desktop.process", valid("desktop.process"), None),
        (
            "desktop.process",
            params("desktop.process", IDENTITY, "0"),
            Some("invalid_request"),
        ),
        (
            "desktop.process",
            params("desktop.process", IDENTITY, "202.0"),
            Some("operation_outcome_unknown"),
        ),
        (
            "desktop.process",
            params(
                "desktop.process",
                &IDENTITY.replace("\"pid\":101", "\"pid\":101.0"),
                "202",
            ),
            Some("invalid_request"),
        ),
        ("desktop.captureOwner", valid("desktop.captureOwner"), None),
        (
            "desktop.captureOwner",
            params("desktop.captureOwner", IDENTITY, "3"),
            Some("operation_outcome_unknown"),
        ),
        (
            "desktop.captureOwner",
            params("desktop.captureOwner", IDENTITY, "null"),
            Some("invalid_request"),
        ),
    ] {
        let c = Case::new(method, &args);
        let result = run_case(&c).await;
        assert_eq!(
            response(&result)
                .get("error")
                .unwrap()
                .as_str()
                .ok()
                .as_deref(),
            error
        );
        rows.push(result);
    }
    for method in ["desktop.process", "desktop.captureOwner"] {
        let mut c = Case::new(method, "{}");
        c.closed = true;
        let result = run_case(&c).await;
        assert_eq!(
            response(&result).get("error").unwrap().as_str().unwrap(),
            "runtime_stopping"
        );
        assert!(trace(&result).is_empty());
        rows.push(result);
        for failure in if method == "desktop.process" {
            vec!["process-null", "process-argument", "process-error"]
        } else {
            vec![
                "captureOwner-null",
                "captureOwner-argument",
                "captureOwner-error",
            ]
        } {
            let mut c = Case::new(method, &valid(method));
            c.failure = failure;
            let result = run_case(&c).await;
            let expected = if failure == "captureOwner-null" {
                None
            } else if failure.ends_with("argument") {
                Some("invalid_request")
            } else {
                Some("operation_outcome_unknown")
            };
            assert_eq!(
                response(&result)
                    .get("error")
                    .unwrap()
                    .as_str()
                    .ok()
                    .as_deref(),
                expected
            );
            rows.push(result);
        }
        let mut c = Case::new(method, &valid(method));
        c.params.extend([valid(method), valid(method)]);
        let result = run_case(&c).await;
        assert_eq!(trace(&result).len(), 3);
        rows.push(result);
        let sip = NativeSipOperations::new(Arc::new(NoMedia));
        let (enter_tx, enter_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let runtime =
            PhoneDesktopRuntime::with_application(|_| panic!("No application authorized"));
        let runtime = if method == "desktop.process" {
            runtime.with_process(move |_, pid| {
                enter_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                done_tx.send(()).unwrap();
                Ok(Some(AudioProcessOwnership {
                    pid,
                    state: crate::desktop::AudioProcessState::Unavailable,
                }))
            })
        } else {
            runtime.with_capture_owner(move |_, _| {
                enter_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                done_tx.send(()).unwrap();
                Ok(None)
            })
        };
        let layer = NativeDesktopCommands::with_runtime(
            sip.clone(),
            runtime,
            Arc::new(Remaining {
                closes: AtomicUsize::new(0),
                fail: false,
            }),
        );
        let work = layer.invoke(request(method, &valid(method), 1));
        enter_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(work);
        drop(sip.close());
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        layer.shutdown().await.unwrap();
    }
    save("process-command-ordinary.json", &IvyJson::Array(rows));
    save(
        "audio-process-native.json",
        &crate::desktop::verify_audio_process_native(),
    );
}
