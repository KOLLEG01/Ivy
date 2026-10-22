use super::*;
use ivy_hive::{
    browser_push_native::NativeCrypto,
    kernel::{StartupOptions, StartupPorts},
    worker_kernel::KernelWorker,
    worker_server,
};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::{
    future::poll_fn,
    io,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use tokio::{io::AsyncWrite, runtime::Builder, time::timeout};

fn locked(filename: &Path) -> bool {
    let db = Connection::open_with_flags(filename, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.busy_timeout(Duration::ZERO).unwrap();
    match db.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(_) => false,
        Err(rusqlite::Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) =>
        {
            true
        }
        Err(error) => panic!("Unexpected independent lock-probe failure: {error}"),
    }
}
#[derive(Default)]
struct Gate {
    bytes: Vec<u8>,
    publication: Option<Value>,
    release: bool,
    writer: Option<Waker>,
    observer: Option<Waker>,
}
struct HeldWriter {
    gate: Rc<RefCell<Gate>>,
    filename: PathBuf,
    root: PathBuf,
    fail: bool,
}
impl AsyncWrite for HeldWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.gate.borrow_mut().bytes.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut gate = self.gate.borrow_mut();
        if gate.publication.is_none() {
            let size = u32::from_be_bytes(gate.bytes[..4].try_into().unwrap()) as usize;
            assert_eq!(
                gate.bytes.len(),
                size + 4,
                "Only the startup frame is accepted"
            );
            let frame: Value = serde_json::from_slice(&gate.bytes[4..]).unwrap();
            gate.publication = Some(
                json!({"frame":frame,"databaseLocked":locked(&self.filename),"files":startup::files(&self.root)}),
            );
            if let Some(waker) = gate.observer.take() {
                waker.wake();
            }
        }
        if gate.release {
            Poll::Ready(if self.fail {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            })
        } else {
            gate.writer = Some(context.waker().clone());
            Poll::Pending
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
fn native(input: &Value, profile: &Value, fail: bool) -> Value {
    startup::prepare(input);
    let filename = PathBuf::from(input["filename"].as_str().unwrap());
    let root = PathBuf::from(input["uiRoot"].as_str().unwrap());
    let gate = Rc::new(RefCell::new(Gate::default()));
    let writer = HeldWriter {
        gate: gate.clone(),
        filename: filename.clone(),
        root: root.clone(),
        fail,
    };
    let options = StartupOptions {
        filename: ivy_host_runtime::node_path::from_native(&filename),
        ui_root: Some(input["uiRoot"].as_str().unwrap().into()),
        credentials: startup::credentials(input),
        kernel: Options {
            public_base_url: input["publicBaseUrl"].as_str().unwrap().into(),
            validate_messages: true,
            ..options()
        },
    };
    let ports = StartupPorts {
        runtime: Rc::new(registry::ObservedRuntime::new(profile)),
        query: Arc::new(registry::ObservedRuntime::new(profile)),
        clock: Rc::new(|| 100000.0),
        time_zones: Rc::new(|_: &JsString| panic!("No Intl admission in worker startup")),
        temporary_directory: Box::new(|| panic!("Explicit disk root needs no temporary directory")),
    };
    let raw_options = input.get("workerDataJson").and_then(Value::as_str);
    let bytes = raw_options.map_or_else(
        || serde_json::to_vec(input).unwrap(),
        |value| value.as_bytes().to_vec(),
    );
    let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&bytes);
    let mut projected_options = Value::Null;
    let server = worker_server::serve_retained(&frame[..], writer, |data| {
        let worker = if raw_options.is_some() {
            KernelWorker::open_worker_data(data, ports, Rc::new(NativeCrypto))
        } else {
            KernelWorker::open(options, ports, Rc::new(NativeCrypto))
        }?;
        if raw_options.is_some() {
            let kernel = worker.kernel().unwrap();
            let options = &kernel.options;
            projected_options = json!({"version":options.version.0,"buildId":options.build_id.0,"publicBaseUrl":options.public_base_url.0,"uiRoot":ivy_host_runtime::node_path::from_native(&kernel.parts.ui_files.root).0,"validateMessages":options.validate_messages,"resetBootstrapCredentialDigest":options.reset_bootstrap_credential_digest.as_ref().map(|v|&v.0)});
        }
        Ok::<_, ivy_hive::kernel::StartupFailure>(worker)
    });
    let observer = async {
        poll_fn(|context| {
            let mut gate = gate.borrow_mut();
            if let Some(publication) = gate.publication.as_ref() {
                // The entire frame is written, but flush is still pending. An
                // unrelated connection proves the owner's actual lock lifetime.
                assert_eq!(publication["databaseLocked"], locked(&filename));
                gate.release = true;
                if let Some(waker) = gate.writer.take() {
                    waker.wake();
                }
                Poll::Ready(())
            } else {
                gate.observer = Some(context.waker().clone());
                Poll::Pending
            }
        })
        .await;
    };
    let result = Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            timeout(Duration::from_secs(10), async {
                tokio::join!(server, observer).0
            })
            .await
            .unwrap()
        });
    let publication = gate.borrow().publication.clone().unwrap();
    let after_ready_error = result.is_err() && !fail && publication["frame"]["ready"] == true;
    assert_eq!(result.is_err(), after_ready_error || fail);
    if fail {
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }
    let database_locked = locked(&filename);
    let db = Connection::open_with_flags(&filename, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut output = json!({"publications":[publication],"afterReadyError":after_ready_error,"state":startup::state(&db),"files":startup::files(&root),"databaseLocked":database_locked});
    if raw_options.is_some() {
        output["options"] = projected_options;
    }
    output
}
fn original(input: &Value) -> Value {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tests/fixtures/hive-worker-startup.mjs"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn actual_worker_startup_retains_fatal_owner_and_reads_announcement_head_after_ready() {
    let root = Root::new();
    let profile = if let Some(path) = std::env::var_os("IVY_HIVE_STARTUP_PROFILE") {
        let prior: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        prior["original"]["registryRuntime"].clone()
    } else {
        oracle(&root.0.join("profile.sqlite"), &[op("constructor")])["registryRuntime"].clone()
    };
    let mut corpus = vec![];
    for name in [
        "ready",
        "credentials-close-before-fatal",
        "ui-fatal",
        "contract-fatal",
        "url-fatal",
        "push-head-fatal",
        "announcement-head-after-ready",
    ] {
        let base = root.0.join(name);
        fs::create_dir(&base).unwrap();
        let mut input = json!({"case":name,"filename":base.join("hive.sqlite"),"uiRoot":base.join("ui"),"publicBaseUrl":"https://example.invalid/base","credentials":[{"principalId":"alpha","digest":format!("sha256:{}","a".repeat(64))}]});
        if matches!(name, "credentials-close-before-fatal" | "ui-fatal") {
            input["rootFile"] = true.into();
        }
        if name == "credentials-close-before-fatal" {
            input["credentials"][0]["digest"] = "invalid".into();
        }
        if name == "contract-fatal" {
            input["seed"] = json!([{"op":"register","owner":{"kind":"hive"},"definition":{"key":"ivy/settings","version":"1.0.0","owner":{"kind":"hive"},"mediaType":"application/json","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"bounded","maximumCount":100}},"specMarkdown":"Different immutable contract.","jsonSchema":{"type":"object"}}}]);
        }
        if name == "url-fatal" {
            input["publicBaseUrl"] = "invalid".into();
        }
        if matches!(name, "push-head-fatal" | "announcement-head-after-ready") {
            input["seed"] = json!([{"op":"sql","sql":if name=="push-head-fatal" { "DROP TABLE events;" } else { "DROP TABLE events; INSERT INTO metadata VALUES('browser_push_cursor','7');" }}]);
        }
        let actual = native(&input, &profile, false);
        assert!(base.starts_with(&root.0));
        fs::remove_dir_all(&base).unwrap();
        fs::create_dir(&base).unwrap();
        let expected = original(&input);
        let fault = if name == "ui-fatal" {
            fs::remove_dir_all(&base).unwrap();
            fs::create_dir(&base).unwrap();
            native(&input, &profile, true)
        } else {
            Value::Null
        };
        corpus.push(
            json!({"input":input,"native":actual,"original":expected,"nativeFlushFailure":fault}),
        );
    }
    if let Some(path) = std::env::var_os("IVY_HIVE_WORKER_STARTUP_CORPUS") {
        fs::write(path, json!(corpus).to_string()).unwrap();
    }
    for case in &corpus {
        for key in [
            "publications",
            "afterReadyError",
            "state",
            "files",
            "databaseLocked",
        ] {
            assert_eq!(
                case["native"][key], case["original"][key],
                "{}: {key}",
                case["input"]["case"]
            );
            if !case["nativeFlushFailure"].is_null() {
                assert_eq!(
                    case["native"][key], case["nativeFlushFailure"][key],
                    "Failed Fatal flush: {key}"
                );
            }
        }
        assert_eq!(case["native"]["databaseLocked"], false);
        assert_eq!(
            case["native"]["publications"][0]["databaseLocked"],
            case["input"]["case"] != "credentials-close-before-fatal"
        );
    }
    assert_eq!(
        corpus[0]["native"]["publications"][0]["frame"],
        json!({"ready":true})
    );
    assert_eq!(
        corpus[6]["native"]["publications"][0]["frame"],
        json!({"ready":true})
    );
    assert_eq!(corpus[6]["native"]["afterReadyError"], true);
    for i in 1..6 {
        assert!(
            corpus[i]["native"]["publications"][0]["frame"]
                .get("fatal")
                .is_some()
        );
    }
}

#[test]
fn store_initialization_failure_releases_owner_before_fatal_without_ui_effects() {
    let root = Root::new();
    // These errors precede all runtime calls; any attempted primitive access
    // fails on its absent observation instead of supplying a test fallback.
    let profile = json!({"dates":[],"iso":[],"localeOrder":[]});
    let mut corpus = vec![];
    for name in ["malformed-metadata", "initial-schema-rollback"] {
        let base = root.0.join(name);
        fs::create_dir(&base).unwrap();
        let mut input = json!({"case":name,"filename":base.join("hive.sqlite"),"uiRoot":base.join("ui"),"publicBaseUrl":"https://example.invalid/base","credentials":[{"principalId":"alpha","digest":format!("sha256:{}","a".repeat(64))}]});
        if name == "malformed-metadata" {
            input["seed"] = json!([{"op":"sql","sql":"DROP TABLE metadata; CREATE TABLE metadata(key TEXT PRIMARY KEY,payload TEXT) STRICT;"}]);
        } else {
            // The original fresh-schema transaction creates metadata before
            // encountering this collision; neither implementation may retain it.
            input["rawSql"] = "CREATE TABLE principals(id INTEGER PRIMARY KEY) STRICT;".into();
        }
        let actual = native(&input, &profile, false);
        assert!(base.starts_with(&root.0));
        fs::remove_dir_all(&base).unwrap();
        fs::create_dir(&base).unwrap();
        let expected = original(&input);
        corpus.push(json!({"input":input,"native":actual,"original":expected}));
    }
    if let Some(path) = std::env::var_os("IVY_HIVE_WORKER_EARLY_FAILURE_CORPUS") {
        fs::write(path, json!(corpus).to_string()).unwrap();
    }
    for case in &corpus {
        for key in [
            "publications",
            "afterReadyError",
            "state",
            "files",
            "databaseLocked",
        ] {
            assert_eq!(
                case["native"][key], case["original"][key],
                "{}: {key}",
                case["input"]["case"]
            );
        }
        let publication = &case["native"]["publications"][0];
        assert_eq!(
            publication["frame"]["fatal"]["data"]["code"],
            "internal_error"
        );
        assert_eq!(publication["databaseLocked"], false);
        assert_eq!(publication["files"]["kind"], "absent");
        assert_eq!(case["native"]["databaseLocked"], false);
        assert_eq!(case["native"]["afterReadyError"], false);
        assert_eq!(case["original"]["portClosed"], true);
        assert_eq!(case["original"]["exception"], publication["frame"]["fatal"]);
    }
    assert!(
        corpus[1]["native"]["state"]["tables"]
            .get("metadata")
            .is_none()
    );
    assert_eq!(
        corpus[1]["native"]["state"]["tables"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn admitted_server_worker_frame_initializes_actual_owner_without_losing_utf16_or_defaults() {
    use ivy_hive::server_constructor::{Options as ServerOptions, Prepared};
    let root = Root::new();
    // Explicit qualification runs may reuse observations. Ordinary test
    // selection obtains them from the existing original-domain oracle.
    let profile = if let Some(path) = std::env::var_os("IVY_HIVE_STARTUP_PROFILE") {
        let prior: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        prior["original"]["registryRuntime"].clone()
    } else {
        let observed = oracle(&root.0.join("profile.sqlite"), &[op("constructor")]);
        if let Some(path) = std::env::var_os("IVY_HIVE_WORKER_PROFILE_CAPTURE") {
            fs::write(path, observed.to_string()).unwrap();
        }
        observed["registryRuntime"].clone()
    };
    let mut corpus = vec![];
    for name in [
        "omitted-defaults",
        "utf16-empty-reset",
        "reset-enabled",
        "null-optionals",
        "credential-fatal",
        "url-fatal",
    ] {
        let base = root.0.join(name);
        fs::create_dir(&base).unwrap();
        let filename = base.join("hive.sqlite");
        let mut data = IvyJson::object([
            (
                "filename",
                IvyJson::String(ivy_host_runtime::node_path::from_native(&filename)),
            ),
            ("publicBaseUrl", "https://example.invalid/base".into()),
            ("version", "1.2.3".into()),
            ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
            (
                "credentials",
                IvyJson::Array(vec![IvyJson::object([
                    ("principalId", "alpha".into()),
                    ("digest", format!("sha256:{}", "a".repeat(64)).into()),
                ])]),
            ),
            (
                "extraServerOption",
                IvyJson::object([("untouched", true.into())]),
            ),
        ]);
        if name == "utf16-empty-reset" {
            data.insert(
                "version",
                IvyJson::String(JsString(vec![0xd800, 0x2e, 0x31, 0xdc00])),
            );
            data.insert("validateMessages", false.into());
            data.insert("resetBootstrapCredentialDigest", "".into());
        }
        if name == "reset-enabled" {
            data.insert("validateMessages", true.into());
            data.insert(
                "resetBootstrapCredentialDigest",
                format!("sha256:{}", "a".repeat(64)).into(),
            );
        }
        if name == "null-optionals" {
            data.insert("validateMessages", IvyJson::Null);
            data.insert("resetBootstrapCredentialDigest", IvyJson::Null);
        }
        let prepared = Prepared::new(ServerOptions::from_json(data).unwrap()).unwrap();
        let mut data = prepared.worker_options().clone();
        assert_eq!(
            data.get("extraServerOption").unwrap().get("untouched"),
            Some(&IvyJson::Bool(true))
        );
        if name == "credential-fatal" {
            data.insert(
                "credentials",
                IvyJson::Array(vec![IvyJson::object([
                    ("principalId", "alpha".into()),
                    ("digest", "invalid".into()),
                ])]),
            );
        }
        if name == "url-fatal" {
            data.insert("publicBaseUrl", "invalid".into());
        }
        // Keep the raw wire as a string: serde_json cannot represent decoded
        // lone surrogates, and must not normalize those UTF-16 units.
        let input = json!({"case":name,"filename":filename,"uiRoot":prepared.ui_root().to_utf8().unwrap(),"publicBaseUrl":"https://example.invalid/base","credentials":[{"principalId":"alpha","digest":format!("sha256:{}","a".repeat(64))}],"workerDataJson":ivy_contracts::json::stringify(&data,ivy_hive::worker_protocol::MAX_FRAME_BYTES).unwrap()});
        let actual = native(&input, &profile, false);
        assert!(base.starts_with(&root.0));
        fs::remove_dir_all(&base).unwrap();
        fs::create_dir(&base).unwrap();
        let expected = original(&input);
        corpus.push(json!({"input":input,"native":actual,"original":expected}));
    }
    if let Some(path) = std::env::var_os("IVY_HIVE_WORKER_OPTIONS_CORPUS") {
        fs::write(path, json!(corpus).to_string()).unwrap();
    }
    for case in &corpus {
        for key in [
            "publications",
            "afterReadyError",
            "state",
            "files",
            "databaseLocked",
            "options",
        ] {
            assert_eq!(
                case["native"][key], case["original"][key],
                "{}: {key}",
                case["input"]["case"]
            );
        }
        assert_eq!(case["native"]["databaseLocked"], false);
        assert_eq!(
            case["native"]["publications"][0]["databaseLocked"],
            case["input"]["case"] != "credential-fatal"
        );
    }
    assert_eq!(corpus[0]["native"]["options"]["validateMessages"], false);
    assert_eq!(
        corpus[0]["native"]["options"]["resetBootstrapCredentialDigest"],
        Value::Null
    );
    assert_eq!(
        corpus[1]["native"]["options"]["version"],
        json!([0xd800, 0x2e, 0x31, 0xdc00])
    );
    assert_eq!(
        corpus[1]["native"]["options"]["resetBootstrapCredentialDigest"],
        json!([])
    );
    assert_eq!(corpus[2]["native"]["options"]["validateMessages"], true);
    assert!(
        corpus[2]["native"]["options"]["resetBootstrapCredentialDigest"]
            .as_array()
            .unwrap()
            .len()
            > 0
    );
    assert_eq!(corpus[3]["native"]["options"]["validateMessages"], false);
    assert_eq!(
        corpus[3]["native"]["options"]["resetBootstrapCredentialDigest"],
        Value::Null
    );
    for case in &corpus[..4] {
        assert_eq!(
            case["native"]["publications"][0]["frame"],
            json!({"ready":true})
        );
    }
    for case in &corpus[4..] {
        assert!(
            case["native"]["publications"][0]["frame"]
                .get("fatal")
                .is_some()
        );
    }
}
