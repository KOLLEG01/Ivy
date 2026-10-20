use super::*;
use std::{path::Path, sync::Condvar, time::Duration};
// Compile the exact production sequence, rather than a second create model.
#[path = "../../packages/host-runtime/rust/src/linux_host/durable_create.rs"]
mod durable_create;
use durable_create::{Files, Handle};
#[path = "host-linux-unix-close-cases.rs"]
mod unix_close_cases;

struct Gate {
    path: JsString,
    entered: Notify,
    released: Mutex<bool>,
    wake: Condvar,
}
impl Gate {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
    fn wait(&self, path: &JsString) {
        if path != &self.path {
            return;
        }
        self.entered.notify_one();
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.wake.wait(released).unwrap();
        }
    }
}
struct Release(Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
#[derive(Clone)]
struct Handles {
    host: SimHost,
    gate: Option<Arc<Gate>>,
}
struct Descriptor {
    files: Handles,
    path: JsString,
}
impl Files for Handles {
    type Handle = Descriptor;
    fn create(&self, path: &Path, mode: u32) -> io::Result<Descriptor> {
        let path = node_path::from_native(path);
        let mut state = self.host.state.lock().unwrap();
        state.event(IvyJson::object([
            ("op", "open".into()),
            ("path", IvyJson::String(path.clone())),
            ("flags", "wx".into()),
            ("mode", IvyJson::Number(mode as f64)),
        ]))?;
        if let Some(race) = state.model.get("createRace").cloned() {
            state.files.insert(path.clone(), race);
            return Err(fail("EEXIST"));
        }
        if state.files.get_key(&path).is_some() {
            return Err(fail("EEXIST"));
        }
        state.files.insert(
            path.clone(),
            IvyJson::object([
                ("data", "".into()),
                ("file", true.into()),
                ("symlink", false.into()),
                ("uid", IvyJson::Number(0.0)),
                ("gid", IvyJson::Number(0.0)),
                ("mode", IvyJson::Number(mode as f64)),
                ("size", IvyJson::Number(0.0)),
            ]),
        );
        Ok(Descriptor {
            files: self.clone(),
            path,
        })
    }
    fn directory(&self, path: &Path) -> io::Result<Descriptor> {
        let path = node_path::from_native(path);
        self.host.state.lock().unwrap().event(IvyJson::object([
            ("op", "open".into()),
            ("path", IvyJson::String(path.clone())),
            ("flags", "r".into()),
            ("mode", IvyJson::Null),
        ]))?;
        Ok(Descriptor {
            files: self.clone(),
            path,
        })
    }
}
impl Handle for Descriptor {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut state = self.files.host.state.lock().unwrap();
        let data = String::from_utf8_lossy(bytes).to_string();
        let mut event = path_event("write", &self.path);
        event.insert("data", data.clone().into());
        state.event(event)?;
        let mut file = state.files.get_key(&self.path).unwrap().clone();
        file.insert("data", data.into());
        file.insert("size", IvyJson::Number(bytes.len() as f64));
        state.files.insert(self.path.clone(), file);
        Ok(())
    }
    fn sync_all(&self) -> io::Result<()> {
        self.files
            .host
            .state
            .lock()
            .unwrap()
            .event(path_event("sync", &self.path))
    }
    fn close(self) -> io::Result<()> {
        self.files
            .host
            .state
            .lock()
            .unwrap()
            .event(path_event("close", &self.path))?;
        if let Some(gate) = &self.files.gate {
            gate.wait(&self.path);
        }
        Ok(())
    }
}
#[derive(Clone)]
struct CloseHost {
    handles: Handles,
    boundary_error: Arc<Mutex<Option<String>>>,
}
impl CloseHost {
    fn new(model: IvyJson, gate: Option<Arc<Gate>>) -> Self {
        Self {
            handles: Handles {
                host: SimHost::new(model),
                gate,
            },
            boundary_error: Default::default(),
        }
    }
    fn output(&self, result: IvyResult<IvyJson>) -> IvyJson {
        let raw = if result.is_err() {
            self.boundary_error.lock().unwrap().clone()
        } else {
            None
        };
        let mut output = self.handles.host.output(result);
        output.insert(
            "boundaryError",
            raw.map(IvyJson::from).unwrap_or(IvyJson::Null),
        );
        output
    }
}
impl LinuxHost for CloseHost {
    fn platform(&self) -> &str {
        self.handles.host.platform()
    }
    fn user_id(&self) -> Option<u32> {
        self.handles.host.user_id()
    }
    fn metadata(&self, path: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
        self.handles.host.metadata(path)
    }
    fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
        self.handles.host.read(path)
    }
    fn command(
        &self,
        file: &'static str,
        args: Vec<JsString>,
        timeout_ms: u32,
        maximum_buffer_bytes: usize,
    ) -> BoxFuture<'static, IvyResult<JsString>> {
        self.handles
            .host
            .command(file, args, timeout_ms, maximum_buffer_bytes)
    }
    fn create(
        &self,
        path: JsString,
        bytes: Vec<u8>,
        mode: u32,
        parent: JsString,
    ) -> BoxFuture<'static, io::Result<()>> {
        let handles = self.handles.clone();
        let boundary_error = self.boundary_error.clone();
        let task = tokio::task::spawn_blocking(move || {
            let result = durable_create::create(
                &handles,
                &node_path::to_native(&path),
                &bytes,
                mode,
                &node_path::to_native(&parent),
            );
            if let Err(error) = &result {
                *boundary_error.lock().unwrap() = Some(error.to_string());
            }
            result
        });
        Box::pin(async move { task.await.map_err(io::Error::other)? })
    }
}
fn retain(case: &str, input: IvyJson, actual: IvyJson, expected: IvyJson) {
    if let Some(path) = std::env::var_os("IVY_HOST_LINUX_CLOSE_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        let row = IvyJson::object([
            ("case", case.into()),
            ("input", input),
            ("actual", actual),
            ("expected", expected),
        ]);
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            json::stringify(&row, usize::MAX).unwrap()
        )
        .unwrap();
    }
}
fn fault(op: &str, at: usize, code: &str) -> IvyJson {
    IvyJson::object([
        ("op", op.into()),
        ("at", IvyJson::Number(at as f64)),
        ("code", code.into()),
    ])
}
#[tokio::test]
async fn production_creation_sequence_matches_original_close_errors_and_finally_precedence() {
    let fixture = Fixture::new();
    let cases = [
        ("success", vec![]),
        ("file-close-EIO", vec![fault("close", 1, "EIO")]),
        ("directory-close-EIO", vec![fault("close", 2, "EIO")]),
        ("file-close-EINTR-once", vec![fault("close", 1, "EINTR")]),
        (
            "directory-close-EINTR-once",
            vec![fault("close", 2, "EINTR")],
        ),
        (
            "write-failure-closes-file",
            vec![fault("write", 1, "EACCES")],
        ),
        (
            "file-sync-failure-closes-file",
            vec![fault("sync", 1, "EINVAL")],
        ),
        (
            "directory-sync-failure-closes-directory",
            vec![fault("sync", 2, "EACCES")],
        ),
        (
            "write-then-close-failure",
            vec![fault("write", 1, "EACCES"), fault("close", 1, "EIO")],
        ),
        (
            "file-sync-then-close-failure",
            vec![fault("sync", 1, "EINVAL"), fault("close", 1, "EIO")],
        ),
        (
            "directory-sync-then-close-failure",
            vec![fault("sync", 2, "EACCES"), fault("close", 2, "EIO")],
        ),
        (
            "write-then-close-distinct-error",
            vec![fault("write", 1, "EIO"), fault("close", 1, "EINVAL")],
        ),
        (
            "file-sync-then-close-distinct-error",
            vec![fault("sync", 1, "EIO"), fault("close", 1, "EACCES")],
        ),
        (
            "directory-sync-then-close-distinct-error",
            vec![fault("sync", 2, "EIO"), fault("close", 2, "EINVAL")],
        ),
        (
            "file-open-failure-no-close",
            vec![fault("open", 1, "EACCES")],
        ),
        (
            "directory-open-failure-after-file-close",
            vec![fault("open", 2, "EACCES")],
        ),
    ];
    for (case, faults) in cases {
        let mut model = model();
        model.insert("faults", IvyJson::Array(faults));
        let host = Arc::new(CloseHost::new(model.clone(), None));
        let mut request = input(&fixture, "resources", "ensureLinuxSlice", vec![scope()]);
        request.insert("model", model);
        request.insert("captureBoundaryError", true.into());
        let result = resources::ensure_linux_slice_with_host(scope(), host.clone())
            .await
            .map(|_| IvyJson::Null);
        let actual = host.output(result);
        let expected = oracle(&request);
        retain(case, request, actual.clone(), expected.clone());
        assert_eq!(
            json::canonical(&actual, usize::MAX).unwrap(),
            json::canonical(&expected, usize::MAX).unwrap(),
            "{case}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_waiter_preserves_both_close_boundaries_and_final_verification() {
    let fixture = Fixture::new();
    for path in [slice_path(&scope()), "/etc/systemd/system".into()] {
        let gate = Arc::new(Gate {
            path: path.clone().into(),
            entered: Notify::new(),
            released: Mutex::new(false),
            wake: Condvar::new(),
        });
        let _release = Release(gate.clone());
        let host = Arc::new(CloseHost::new(model(), Some(gate.clone())));
        let owned = host.clone();
        let observer =
            tokio::spawn(
                async move { resources::ensure_linux_slice_with_host(scope(), owned).await },
            );
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .unwrap();
        let before_drop = host.handles.host.state.lock().unwrap().trace.clone();
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        gate.release();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if host
                    .handles
                    .host
                    .state
                    .lock()
                    .unwrap()
                    .trace
                    .last()
                    .is_some_and(|event| event.get("op") == Some(&"read".into()))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let state = host.handles.host.state.lock().unwrap();
        let completed = IvyJson::object([
            ("trace", IvyJson::Array(state.trace.clone())),
            ("files", state.files.clone()),
        ]);
        drop(state);
        let mut request = input(&fixture, "resources", "ensureLinuxSlice", vec![scope()]);
        request.insert("model", model());
        request.insert("captureBoundaryError", true.into());
        let expected = oracle(&request);
        let expected_observations = IvyJson::object([
            ("trace", expected.get("trace").unwrap().clone()),
            ("files", expected.get("files").unwrap().clone()),
        ]);
        assert_eq!(
            json::canonical(&completed, usize::MAX).unwrap(),
            json::canonical(&expected_observations, usize::MAX).unwrap()
        );
        retain(
            &format!("native admitted owner after drop at {path}"),
            request,
            IvyJson::object([
                ("beforeDropTrace", IvyJson::Array(before_drop)),
                ("completedObservations", completed),
            ]),
            IvyJson::Null,
        );
    }
}

#[tokio::test]
async fn native_create_closes_its_actual_file_and_never_overwrites_it() {
    let fixture = Fixture::new();
    let path = fixture.0.join("owned");
    let host = NativeLinuxHost::new(fixture.0.clone());
    let result = host
        .create(
            node_path::from_native(&path),
            b"retained bytes".to_vec(),
            0o644,
            node_path::from_native(&fixture.0),
        )
        .await;
    #[cfg(unix)]
    result.as_ref().unwrap();
    // Windows may reject native directory open/sync; this does not qualify Linux
    // directory durability. In either case the created file must already close.
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options.open(&path).unwrap();
    drop(file);
    assert_eq!(fs::read(&path).unwrap(), b"retained bytes");
    let again = host
        .create(
            node_path::from_native(&path),
            b"replacement".to_vec(),
            0o644,
            node_path::from_native(&fixture.0),
        )
        .await;
    assert_eq!(again.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&path).unwrap(), b"retained bytes");
    retain(
        "native file close and exclusive create",
        IvyJson::Null,
        IvyJson::object([
            ("firstCreateSucceeded", result.is_ok().into()),
            ("fileClosed", true.into()),
            ("bytesUnchanged", true.into()),
        ]),
        IvyJson::Null,
    );
}
