use super::*;
use crate::desktop::{CaptureEndpoint, CaptureObservation, CaptureSession};

struct Endpoint {
    case: Case,
    trace: Arc<Mutex<Vec<String>>>,
    sip: NativeSipOperations,
    id: JsString,
    ids: usize,
    active: usize,
}
impl Endpoint {
    fn log(&self, text: &str) {
        self.trace.lock().unwrap().push(text.into());
    }
}
impl CaptureEndpoint for Endpoint {
    fn id(&mut self) -> ActionResult<JsString> {
        self.log("id");
        self.ids += 1;
        if self.case.failure == "id-error" {
            return Err(DesktopActionError::Unavailable);
        }
        Ok(
            if self.case.failure == "different-id"
                || self.case.failure == "changed-id" && self.ids == 2
            {
                "other".into()
            } else {
                self.id.clone()
            },
        )
    }
    fn is_active_capture(&mut self) -> ActionResult<bool> {
        self.log("active");
        self.active += 1;
        if self.case.failure == "active-error" {
            return Err(DesktopActionError::Unavailable);
        }
        Ok(self.case.state != "inactive-endpoint"
            && !(self.case.failure == "changed-active" && self.active == 2))
    }
    fn read_sessions(&mut self) -> ActionResult<Vec<CaptureSession>> {
        self.log("read");
        if self.case.close_at == "read" {
            drop(self.sip.close());
        }
        if matches!(
            self.case.failure,
            "read-error" | "read-null" | "null-session"
        ) {
            return Err(DesktopActionError::Unavailable);
        }
        if self.case.failure == "read-panic" {
            panic!("Inert capture provider panic");
        }
        let count = match self.case.state {
            "max" => 4096,
            "over" => 4097,
            "empty" => 0,
            _ => {
                if self.case.failure == "duplicate" {
                    2
                } else {
                    1
                }
            }
        };
        (0..count)
            .map(|i| {
                let name = if self.case.image == "unpaired" {
                    JsString(vec![0xd800])
                } else if !self.case.image.is_empty() {
                    self.case.image.clone().into()
                } else {
                    format!(
                        "session-{}",
                        if self.case.failure == "duplicate" {
                            0
                        } else {
                            i
                        }
                    )
                    .into()
                };
                CaptureSession::new(
                    name,
                    match self.case.state {
                        "inactive" => 0,
                        "expired" => 2,
                        "invalid-state" => 3,
                        _ => 1,
                    },
                    self.case.pid as u32,
                    match self.case.failure {
                        "process-error" => 0x80004005u32 as i32,
                        "multi-process" => 0x0889000d,
                        _ => 0,
                    },
                )
            })
            .collect()
    }
    fn close(&mut self) -> ActionResult<()> {
        self.log("dispose");
        if self.case.failure == "close-error" {
            Err(DesktopActionError::Unavailable)
        } else {
            Ok(())
        }
    }
}
pub(super) fn observe_case(
    case: &Case,
    trace: &Arc<Mutex<Vec<String>>>,
    sip: &NativeSipOperations,
    id: &JsString,
) -> ActionResult<CaptureObservation> {
    trace
        .lock()
        .unwrap()
        .push(format!("capture:{}", id.to_utf8().unwrap()));
    if case.close_at == "capture" {
        drop(sip.close());
    }
    match case.failure {
        "capture-null" | "capture-error" => return Err(DesktopActionError::Unavailable),
        "capture-argument" => return Err(DesktopActionError::Argument("fixture")),
        _ => {}
    }
    let result = CaptureObservation::read(id, |id| {
        trace.lock().unwrap().push("open".into());
        match case.failure {
            "open-error" => Err(DesktopActionError::Unavailable),
            "open-null" => Ok(None),
            _ => Ok(Some(Endpoint {
                case: case.clone(),
                trace: trace.clone(),
                sip: sip.clone(),
                id: id.clone(),
                ids: 0,
                active: 0,
            })),
        }
    })?;
    trace.lock().unwrap().push(format!(
        "inventory:{}:{}",
        if result.observed {
            "observed"
        } else {
            "unavailable"
        },
        result.sessions.len()
    ));
    Ok(result)
}
const PARAMS: &str = r#"{"endpointId":"capture-fixture"}"#;
pub(super) async fn ordinary() {
    let mut rows = vec![];
    for (params, closed, error) in [
        (PARAMS, false, None),
        ("{}", false, Some("invalid_request")),
        ("{}", true, Some("runtime_stopping")),
        (r#"{"endpointId":null}"#, false, Some("invalid_request")),
        (
            r#"{"endpointId":3}"#,
            false,
            Some("operation_outcome_unknown"),
        ),
        (
            r#"{"endpointId":"\ud800"}"#,
            false,
            Some("operation_outcome_unknown"),
        ),
    ] {
        let mut c = Case::new("desktop.capture", params);
        c.closed = closed;
        let actual = run_case(&c).await;
        assert_eq!(
            response(&actual)
                .get("error")
                .unwrap()
                .as_str()
                .ok()
                .as_deref(),
            error
        );
        assert_eq!(trace(&actual).is_empty(), error.is_some());
        rows.push(actual);
    }
    for failure in [
        "open-null",
        "read-null",
        "null-session",
        "duplicate",
        "changed-active",
        "changed-id",
        "close-error",
        "read-panic",
    ] {
        let mut c = Case::new("desktop.capture", PARAMS);
        c.failure = failure;
        let actual = run_case(&c).await;
        assert_eq!(
            response(&actual)
                .get("result")
                .unwrap()
                .get("state")
                .unwrap()
                .as_str()
                .unwrap(),
            "unavailable"
        );
        if failure != "open-null" {
            assert!(trace(&actual).contains(&"dispose".into()));
        }
        rows.push(actual);
    }
    for (state, observed) in [("max", true), ("over", false)] {
        let mut c = Case::new("desktop.capture", PARAMS);
        c.state = state;
        let actual = run_case(&c).await;
        assert!(trace(&actual).contains(&if observed {
            "inventory:observed:4096".into()
        } else {
            "inventory:unavailable:0".into()
        }));
        rows.push(actual);
    }
    for close_at in ["capture", "read"] {
        let mut c = Case::new("desktop.capture", PARAMS);
        c.close_at = close_at;
        let actual = run_case(&c).await;
        assert_eq!(response(&actual).get("ok").unwrap().as_bool(), Some(true));
        assert!(actual.get("closed").unwrap().as_bool().unwrap());
        rows.push(actual);
    }
    let sip = NativeSipOperations::new(Arc::new(NoMedia));
    let (enter_tx, enter_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let desktop = PhoneDesktopRuntime::with_application(|_| panic!("No application authorized"))
        .with_capture(move |id| {
            enter_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            done_tx.send(()).unwrap();
            Ok(CaptureObservation {
                endpoint_id: id.clone(),
                observed: false,
                sessions: vec![],
            })
        });
    let layer = NativeDesktopCommands::with_runtime(
        sip.clone(),
        desktop,
        Arc::new(Remaining {
            closes: AtomicUsize::new(0),
            fail: false,
        }),
    );
    let work = layer.invoke(request("desktop.capture", PARAMS, 1));
    enter_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    drop(work);
    drop(sip.close());
    release_tx.send(()).unwrap();
    done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    layer.shutdown().await.unwrap();
    save("capture-ordinary.json", &IvyJson::Array(rows));
    save(
        "capture-native.json",
        &crate::desktop::verify_capture_native(),
    );
}
