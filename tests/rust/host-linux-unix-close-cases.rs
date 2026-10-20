use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

// Explicit Linux errno inputs to the cross-platform probe. Native Unix calls
// supply libc's own constants; no Windows errno interpretation is substituted.
const INTERRUPTED: i32 = 4;
const IN_PROGRESS: i32 = 115;
fn raw_code(code: &str) -> i32 {
    match code {
        "EINTR" => INTERRUPTED,
        "EINPROGRESS" => IN_PROGRESS,
        "EIO" => 5,
        "EBADF" => 9,
        "EACCES" => 13,
        "EINVAL" => 22,
        _ => panic!("Unlisted raw fixture error"),
    }
}
fn original(raw: i32) -> IvyJson {
    // The ordinary regression case uses the pinned contract. Qualification can
    // additionally compare every expectation with the compiled original C span.
    let error = match raw {
        INTERRUPTED | IN_PROGRESS => 0,
        other => other,
    };
    let expected = IvyJson::object([
        (
            "result",
            IvyJson::Number(if error == 0 { 0.0 } else { -1.0 }),
        ),
        ("error", IvyJson::Number(error as f64)),
        ("calls", IvyJson::Number(1.0)),
        ("fd", IvyJson::Number(42.0)),
    ]);
    let Some(executable) = std::env::var_os("IVY_UNIX_CLOSE_ORACLE") else {
        return expected;
    };
    let output = Command::new(executable)
        .args([raw.to_string(), "42".to_owned()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual = json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    assert_eq!(actual, expected, "Pinned original C adapter result");
    actual
}
#[test]
fn raw_adapter_normalizes_completion_and_calls_the_consumed_descriptor_once() {
    for raw in [
        0,
        INTERRUPTED,
        IN_PROGRESS,
        1,
        5,
        9,
        11,
        13,
        17,
        22,
        125,
        9999,
    ] {
        let expected = original(raw);
        let calls = AtomicUsize::new(0);
        let result = durable_create::unix_close_once(42, INTERRUPTED, IN_PROGRESS, |fd| {
            assert_eq!(fd, 42);
            calls.fetch_add(1, Ordering::SeqCst);
            if raw == 0 {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(raw))
            }
        });
        let actual = IvyJson::object([
            (
                "result",
                IvyJson::Number(if result.is_ok() { 0.0 } else { -1.0 }),
            ),
            (
                "error",
                IvyJson::Number(
                    result
                        .as_ref()
                        .err()
                        .and_then(io::Error::raw_os_error)
                        .unwrap_or(0) as f64,
                ),
            ),
            (
                "calls",
                IvyJson::Number(calls.load(Ordering::SeqCst) as f64),
            ),
            ("fd", IvyJson::Number(42.0)),
        ]);
        retain(
            &format!("raw libuv adapter {raw}"),
            IvyJson::object([
                ("raw", IvyJson::Number(raw as f64)),
                (
                    "libuvProbeUsed",
                    std::env::var_os("IVY_UNIX_CLOSE_ORACLE").is_some().into(),
                ),
            ]),
            actual.clone(),
            expected.clone(),
        );
        assert_eq!(actual, expected);
    }
    let error = durable_create::unix_close_once(42, INTERRUPTED, IN_PROGRESS, |_| {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "non-raw rejection",
        ))
    })
    .unwrap_err();
    assert_eq!(error.raw_os_error(), None);
    assert_eq!(error.to_string(), "non-raw rejection");
    retain(
        "opaque interrupted error stays rejected",
        IvyJson::Null,
        IvyJson::object([
            ("rawError", IvyJson::Null),
            ("message", error.to_string().into()),
        ]),
        IvyJson::Null,
    );
    #[cfg(target_os = "linux")]
    {
        assert_eq!(INTERRUPTED, libc::EINTR);
        assert_eq!(IN_PROGRESS, libc::EINPROGRESS);
    }
}

struct RawFiles(Handles);
struct RawHandle(Descriptor);
impl Files for RawFiles {
    type Handle = RawHandle;
    fn create(&self, p: &Path, mode: u32) -> io::Result<RawHandle> {
        self.0.create(p, mode).map(RawHandle)
    }
    fn directory(&self, p: &Path) -> io::Result<RawHandle> {
        self.0.directory(p).map(RawHandle)
    }
}
impl Handle for RawHandle {
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        self.0.write_all(b)
    }
    fn sync_all(&self) -> io::Result<()> {
        self.0.sync_all()
    }
    fn close(self) -> io::Result<()> {
        durable_create::unix_close_once(42, INTERRUPTED, IN_PROGRESS, |fd| {
            assert_eq!(fd, 42);
            self.0
                .files
                .host
                .state
                .lock()
                .unwrap()
                .event(path_event("close", &self.0.path))
                .map_err(|e| io::Error::from_raw_os_error(raw_code(&e.to_string())))
        })
    }
}
#[derive(Clone)]
struct RawHost {
    host: SimHost,
    error: Arc<Mutex<Option<(Option<i32>, String)>>>,
}
impl LinuxHost for RawHost {
    fn platform(&self) -> &str {
        self.host.platform()
    }
    fn user_id(&self) -> Option<u32> {
        self.host.user_id()
    }
    fn metadata(&self, p: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
        self.host.metadata(p)
    }
    fn read(&self, p: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
        self.host.read(p)
    }
    fn command(
        &self,
        f: &'static str,
        a: Vec<JsString>,
        t: u32,
        b: usize,
    ) -> BoxFuture<'static, IvyResult<JsString>> {
        self.host.command(f, a, t, b)
    }
    fn create(
        &self,
        p: JsString,
        b: Vec<u8>,
        mode: u32,
        parent: JsString,
    ) -> BoxFuture<'static, io::Result<()>> {
        let host = self.clone();
        let task = tokio::task::spawn_blocking(move || {
            let files = RawFiles(Handles {
                host: host.host,
                gate: None,
            });
            let result = durable_create::create(
                &files,
                &node_path::to_native(&p),
                &b,
                mode,
                &node_path::to_native(&parent),
            );
            if let Err(e) = &result {
                *host.error.lock().unwrap() = Some((e.raw_os_error(), e.to_string()));
            }
            result
        });
        Box::pin(async move { task.await.map_err(io::Error::other)? })
    }
}

#[tokio::test]
async fn raw_close_normalization_preserves_original_effects_and_earlier_errors() {
    let fixture = Fixture::new();
    for (case, faults, body_error, close_error) in [
        (
            "file interrupted",
            vec![fault("close", 1, "EINTR")],
            None,
            None,
        ),
        (
            "file in progress",
            vec![fault("close", 1, "EINPROGRESS")],
            None,
            None,
        ),
        (
            "directory interrupted",
            vec![fault("close", 2, "EINTR")],
            None,
            None,
        ),
        (
            "directory in progress",
            vec![fault("close", 2, "EINPROGRESS")],
            None,
            None,
        ),
        (
            "both completing",
            vec![fault("close", 1, "EINTR"), fault("close", 2, "EINPROGRESS")],
            None,
            None,
        ),
        (
            "write then interrupted close",
            vec![fault("write", 1, "EACCES"), fault("close", 1, "EINTR")],
            Some("EACCES"),
            None,
        ),
        (
            "write then in-progress close",
            vec![
                fault("write", 1, "EACCES"),
                fault("close", 1, "EINPROGRESS"),
            ],
            Some("EACCES"),
            None,
        ),
        (
            "file sync then completing close",
            vec![fault("sync", 1, "EINVAL"), fault("close", 1, "EINPROGRESS")],
            Some("EINVAL"),
            None,
        ),
        (
            "directory sync then completing close",
            vec![fault("sync", 2, "EACCES"), fault("close", 2, "EINTR")],
            Some("EACCES"),
            None,
        ),
        (
            "file EIO remains",
            vec![fault("close", 1, "EIO")],
            None,
            Some(5),
        ),
        (
            "write then EIO close wins",
            vec![fault("write", 1, "EACCES"), fault("close", 1, "EIO")],
            None,
            Some(5),
        ),
        (
            "directory sync then EIO close wins",
            vec![fault("sync", 2, "EINVAL"), fault("close", 2, "EIO")],
            None,
            Some(5),
        ),
    ] {
        let mut model = model();
        model.insert("faults", IvyJson::Array(faults.clone()));
        let host = Arc::new(RawHost {
            host: SimHost::new(model.clone()),
            error: Default::default(),
        });
        let result = resources::ensure_linux_slice_with_host(scope(), host.clone())
            .await
            .map(|_| IvyJson::Null);
        let actual = host.host.output(result);
        let mut promise_faults = Vec::new();
        let mut raw_results = Vec::new();
        for f in faults {
            if f.get("op") == Some(&"close".into()) {
                let result = original(raw_code(&f.required("code").unwrap().as_str().unwrap()));
                if result.get("result") == Some(&IvyJson::Number(-1.0)) {
                    promise_faults.push(f);
                }
                raw_results.push(result);
            } else {
                promise_faults.push(f);
            }
        }
        model.insert("faults", IvyJson::Array(promise_faults));
        let mut request = input(&fixture, "resources", "ensureLinuxSlice", vec![scope()]);
        request.insert("model", model);
        let expected = oracle(&request);
        assert_eq!(
            json::canonical(&actual, usize::MAX).unwrap(),
            json::canonical(&expected, usize::MAX).unwrap(),
            "{case}"
        );
        let observed = host.error.lock().unwrap().clone();
        if let Some(raw) = close_error {
            assert_eq!(observed.as_ref().unwrap().0, Some(raw));
        } else if let Some(body) = body_error {
            assert_eq!(observed, Some((None, body.to_string())));
        } else {
            assert_eq!(observed, None);
        }
        request.insert("rawCloseAdapterResults", IvyJson::Array(raw_results));
        retain(case, request, actual, expected);
    }
}
