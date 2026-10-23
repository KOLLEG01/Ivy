use super::*;
use ivy_host_runtime::systemd::{
    NativeSystemdHost, SystemdAdapter, SystemdCommand, SystemdHost, UnitFile,
};
pub(super) struct Host {
    pub(super) inner: SimHost,
    native: Option<Arc<NativeSystemdHost>>,
}
impl Host {
    pub(super) fn new(model: IvyJson, native: Option<PathBuf>) -> Self {
        Self {
            inner: SimHost::new(model),
            native: native.map(|root| Arc::new(NativeSystemdHost::new(root))),
        }
    }
    fn adapter(self: &Arc<Self>) -> SystemdAdapter {
        if self.inner.state.lock().unwrap().model.get("customCommand") == Some(&true.into()) {
            let host = self.clone();
            let command: Arc<dyn SystemdCommand> = Arc::new(
                move |_config: IvyJson,
                      args: Vec<JsString>|
                      -> BoxFuture<'static, IvyResult<JsString>> {
                    let host = host.clone();
                    Box::pin(async move {
                        let mut state = host.inner.state.lock().unwrap();
                        state.trace.push(IvyJson::object([
                            ("op", "customCommand".into()),
                            (
                                "args",
                                IvyJson::Array(args.into_iter().map(IvyJson::String).collect()),
                            ),
                        ]));
                        let row = next_command(&mut state);
                        if let Some(code) = row.get("error") {
                            return Err(IvyError::new(utf(code), "Injected command failure."));
                        }
                        Ok(row.get("stdout").map(s).unwrap_or_default())
                    })
                },
            );
            SystemdAdapter::with_command(self.clone(), command)
        } else {
            SystemdAdapter::with_host(self.clone())
        }
    }
}
fn next_command(state: &mut State) -> IvyJson {
    let result = state
        .model
        .get("commands")
        .and_then(IvyJson::as_array)
        .and_then(|rows| rows.get(state.command_index))
        .cloned()
        .unwrap_or_else(empty);
    state.command_index += 1;
    result
}
struct File {
    path: JsString,
    host: SimHost,
}
impl UnitFile for File {
    fn write<'a>(&'a mut self, bytes: Vec<u8>) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            if self.host.pause.load(Ordering::SeqCst) {
                self.host.opened.notify_one();
                self.host.release.notified().await;
            }
            let mut state = self.host.state.lock().unwrap();
            let data = String::from_utf8_lossy(&bytes).to_string();
            let mut event = path_event("write", &self.path);
            event.insert("data", data.clone().into());
            state.event(event)?;
            let mut file = state.files.get_key(&self.path).unwrap().clone();
            file.insert("data", data.into());
            file.insert("size", IvyJson::Number(bytes.len() as f64));
            state.files.insert(self.path.clone(), file);
            Ok(())
        })
    }
    fn sync(&mut self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            self.host
                .state
                .lock()
                .unwrap()
                .event(path_event("sync", &self.path))
        })
    }
    fn close(self: Box<Self>) -> BoxFuture<'static, io::Result<()>> {
        Box::pin(async move {
            self.host
                .state
                .lock()
                .unwrap()
                .event(path_event("close", &self.path))
        })
    }
}
impl SystemdHost for Host {
    fn platform(&self) -> &str {
        LinuxHost::platform(&self.inner)
    }
    fn user_id(&self) -> Option<u32> {
        LinuxHost::user_id(&self.inner)
    }
    fn metadata(&self, path: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
        LinuxHost::metadata(&self.inner, path)
    }
    fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
        LinuxHost::read(&self.inner, path)
    }
    fn open(
        &self,
        path: JsString,
        exclusive_write: bool,
    ) -> BoxFuture<'static, io::Result<Box<dyn UnitFile>>> {
        let host = self.inner.clone();
        Box::pin(async move {
            let mut state = host.state.lock().unwrap();
            state.event(IvyJson::object([
                ("op", "open".into()),
                ("path", IvyJson::String(path.clone())),
                ("flags", if exclusive_write { "wx" } else { "r" }.into()),
                (
                    "mode",
                    if exclusive_write {
                        IvyJson::Number(420.0)
                    } else {
                        IvyJson::Null
                    },
                ),
            ]))?;
            if exclusive_write {
                if let Some(race) = state.model.get("createRace").cloned() {
                    state.files.insert(path.clone(), race);
                    return Err(fail("EEXIST"));
                }
                if state.files.get_key(&path).is_some() {
                    return Err(fail("EEXIST"));
                }
                state.files.insert(path.clone(),v(r#"{"data":"","file":true,"symlink":false,"uid":0,"gid":0,"mode":420,"size":0}"#));
            }
            drop(state);
            Ok(Box::new(File { path, host }) as Box<dyn UnitFile>)
        })
    }
    fn rename(&self, from: JsString, to: JsString) -> BoxFuture<'static, io::Result<()>> {
        let host = self.inner.clone();
        Box::pin(async move {
            let mut state = host.state.lock().unwrap();
            state.event(IvyJson::object([
                ("op", "rename".into()),
                ("from", IvyJson::String(from.clone())),
                ("to", IvyJson::String(to.clone())),
            ]))?;
            let file = state
                .files
                .get_key(&from)
                .cloned()
                .ok_or_else(|| fail("ENOENT"))?;
            state.files.insert(to, file);
            if let IvyJson::Object(files) = &mut state.files {
                files.retain(|(key, _)| key != &from);
            }
            Ok(())
        })
    }
    fn unlink(&self, path: JsString) -> BoxFuture<'static, io::Result<()>> {
        let host = self.inner.clone();
        Box::pin(async move {
            let mut state = host.state.lock().unwrap();
            state.event(path_event("unlink", &path))?;
            if state.files.get_key(&path).is_none() {
                return Err(fail("ENOENT"));
            }
            if let IvyJson::Object(files) = &mut state.files {
                files.retain(|(key, _)| key != &path);
            }
            Ok(())
        })
    }
    fn execute(
        &self,
        executable: JsString,
        args: Vec<JsString>,
    ) -> BoxFuture<'static, IvyResult<JsString>> {
        let host = self.inner.clone();
        let native = self.native.clone();
        Box::pin(async move {
            let row = {
                let mut state = host.state.lock().unwrap();
                state
                    .event(IvyJson::object([
                        ("op", "command".into()),
                        ("file", IvyJson::String(executable.clone())),
                        (
                            "args",
                            IvyJson::Array(args.iter().cloned().map(IvyJson::String).collect()),
                        ),
                        ("timeoutMs", IvyJson::Number(30_000.0)),
                        ("maxBuffer", IvyJson::Number(65_536.0)),
                    ]))
                    .map_err(|_| IvyError::internal())?;
                next_command(&mut state)
            };
            if let Some(native) = native {
                return native.execute(executable, args).await;
            }
            if let Some(code) = row.get("error") {
                let stderr = row.get("stderr").map(s).unwrap_or_default();
                let detail = &stderr.0[stderr.0.len().saturating_sub(1024)..];
                let mut message = JsString::from("Systemd command failed.").0;
                if !detail.is_empty() {
                    message.push(32);
                    message.extend_from_slice(detail);
                }
                return Err(IvyError::new_js("systemd_failed".into(), JsString(message))
                    .with_outcome(ivy_contracts::Outcome::Unknown)
                    .with_details(IvyJson::object([
                        (
                            "command",
                            IvyJson::String(
                                args.first().cloned().unwrap_or_else(|| "systemctl".into()),
                            ),
                        ),
                        ("exitCode", code.clone()),
                    ])));
            }
            Ok(row.get("stdout").map(s).unwrap_or_default())
        })
    }
    fn uuid(&self) -> String {
        self.inner
            .state
            .lock()
            .unwrap()
            .model
            .get("uuid")
            .map(utf)
            .unwrap_or_else(|| "11111111-2222-4333-8444-555555555555".into())
    }
    fn now_ms(&self) -> f64 {
        let mut state = self.inner.state.lock().unwrap();
        let clock = state
            .model
            .get("clock")
            .and_then(IvyJson::as_f64)
            .unwrap_or(0.0);
        state.trace.push(IvyJson::object([
            ("op", "now".into()),
            ("value", IvyJson::Number(clock)),
        ]));
        clock
    }
    fn sleep(&self, milliseconds: u64) -> BoxFuture<'static, ()> {
        let host = self.inner.clone();
        Box::pin(async move {
            let mut state = host.state.lock().unwrap();
            state.trace.push(IvyJson::object([
                ("op", "sleep".into()),
                ("milliseconds", IvyJson::Number(milliseconds as f64)),
            ]));
            let clock = state
                .model
                .get("clock")
                .and_then(IvyJson::as_f64)
                .unwrap_or(0.0);
            state
                .model
                .insert("clock", IvyJson::Number(clock + milliseconds as f64));
        })
    }
}
fn unit() -> JsString {
    "ivy-next-0123456789ab-fedcba987654.service".into()
}
fn configuration() -> IvyJson {
    v(r#"{"executables":{}}"#)
}
fn state(active: &str) -> IvyJson {
    IvyJson::object([(
        "stdout",
        format!("LoadState=loaded\nActiveState={active}\nSubState=fixture\nMainPID=42\n").into(),
    )])
}
fn scenario(fixture: &Fixture, op: &str, args: Vec<IvyJson>, model: IvyJson) -> IvyJson {
    let mut input = input(fixture, "systemdAdapter", op, args);
    input.insert("model", model);
    input
}
async fn run(input: &IvyJson, native: Option<PathBuf>) -> IvyJson {
    let host = Arc::new(Host::new(input.required("model").unwrap().clone(), native));
    let adapter = host.adapter();
    let args = input.required("args").unwrap().as_array().unwrap();
    let result: IvyResult<IvyJson> = async {
        Ok(match utf(input.required("operation").unwrap()).as_str() {
            "inspectUnit" => adapter
                .inspect_unit(args[0].clone(), s(&args[1]))
                .await?
                .json(),
            "readUnit" => adapter
                .read_unit(args[0].clone(), s(&args[1]))
                .await?
                .map(IvyJson::String)
                .unwrap_or(IvyJson::Null),
            "stopUnit" => adapter
                .stop_unit(args[0].clone(), s(&args[1]), args[2].as_f64().unwrap())
                .await?
                .json(),
            "replaceUnit" => {
                adapter
                    .replace_unit(args[0].clone(), s(&args[1]), s(&args[2]))
                    .await?;
                IvyJson::Null
            }
            "reload" => {
                adapter.reload(args[0].clone()).await?;
                IvyJson::Null
            }
            "startUnit" => adapter
                .start_unit(args[0].clone(), s(&args[1]))
                .await?
                .json(),
            _ => panic!("Unlisted operation"),
        })
    }
    .await;
    host.inner.output(result)
}
async fn check(name: &str, input: IvyJson) {
    compare(name, &input, run(&input, None).await);
}

#[tokio::test]
async fn systemd_adapter_preserves_original_stop_proof_polling_and_command_order() {
    let fixture = Fixture::new();
    for (name, commands, timeout) in [
        ("already-inactive", vec![state("inactive")], 1000.0),
        ("already-failed", vec![state("failed")], 0.0),
        (
            "stopped-immediately",
            vec![state("active"), empty(), state("inactive")],
            1000.0,
        ),
        (
            "stopped-after-two-polls",
            vec![
                state("active"),
                empty(),
                state("deactivating"),
                state("deactivating"),
                state("failed"),
            ],
            1000.0,
        ),
        (
            "zero-deadline-unknown",
            vec![state("active"), empty(), state("active")],
            0.0,
        ),
        (
            "poll-overshoot-stopped",
            vec![state("active"), empty(), state("active"), state("inactive")],
            1.0,
        ),
        (
            "poll-deadline-unknown",
            vec![state("active"), empty(), state("active"), state("active")],
            100.0,
        ),
    ] {
        check(
            name,
            scenario(
                &fixture,
                "stopUnit",
                vec![
                    configuration(),
                    IvyJson::String(unit()),
                    IvyJson::Number(timeout),
                ],
                IvyJson::object([("commands", IvyJson::Array(commands))]),
            ),
        )
        .await;
    }
    for timeout in [-1.0, 0.5, 120_001.0] {
        check(
            "invalid-stop-timeout",
            scenario(
                &fixture,
                "stopUnit",
                vec![
                    configuration(),
                    IvyJson::String(unit()),
                    IvyJson::Number(timeout),
                ],
                empty(),
            ),
        )
        .await;
    }
    for (op, commands) in [
        ("inspectUnit", vec![state("active")]),
        ("startUnit", vec![empty(), state("active")]),
        ("reload", vec![empty()]),
    ] {
        let args = if op == "reload" {
            vec![configuration()]
        } else {
            vec![configuration(), IvyJson::String(unit())]
        };
        check(
            op,
            scenario(
                &fixture,
                op,
                args,
                IvyJson::object([("commands", IvyJson::Array(commands))]),
            ),
        )
        .await;
    }
    for op in [
        "inspectUnit",
        "startUnit",
        "stopUnit",
        "readUnit",
        "replaceUnit",
    ] {
        let mut args = vec![configuration(), "ivy-next-OTHER.service".into()];
        if op == "stopUnit" {
            args.push(IvyJson::Number(0.0))
        }
        if op == "replaceUnit" {
            args.push("content".into())
        }
        check(
            "foreign-unit-refusal",
            scenario(&fixture, op, args, empty()),
        )
        .await;
    }
    check(
        "unexpected-load-state",
        scenario(
            &fixture,
            "inspectUnit",
            vec![configuration(), IvyJson::String(unit())],
            v(r#"{"commands":[{"stdout":"LoadState=masked\nActiveState=inactive\n"}]}"#),
        ),
    )
    .await;
    check(
        "injected-command-on-windows",
        scenario(
            &fixture,
            "stopUnit",
            vec![
                configuration(),
                IvyJson::String(unit()),
                IvyJson::Number(1000.0),
            ],
            IvyJson::object([
                ("platform", "win32".into()),
                ("customCommand", true.into()),
                (
                    "commands",
                    IvyJson::Array(vec![state("active"), empty(), state("inactive")]),
                ),
            ]),
        ),
    )
    .await;
    check(
        "custom-command-error-is-not-rewrapped",
        scenario(
            &fixture,
            "reload",
            vec![configuration()],
            v(r#"{"customCommand":true,"commands":[{"error":"injected_failure"}]}"#),
        ),
    )
    .await;
}

#[tokio::test]
async fn systemd_default_command_preserves_platform_path_limits_and_original_failure_fields() {
    let fixture = Fixture::new();
    for model in [v(r#"{"platform":"win32"}"#), v(r#"{"platform":"other"}"#)] {
        check(
            "native-command-platform-guard",
            scenario(&fixture, "reload", vec![configuration()], model),
        )
        .await;
    }
    for executable in ["relative/systemctl", "", "/path\u{0}bad", "/path\ncontrol"] {
        check(
            "systemctl-path-admission",
            scenario(
                &fixture,
                "reload",
                vec![IvyJson::object([(
                    "executables",
                    IvyJson::object([("systemctl", executable.into())]),
                )])],
                empty(),
            ),
        )
        .await;
    }
    for executable in [IvyJson::Null, "/configured/systemctl".into()] {
        check(
            "systemctl-nullish-default-and-override",
            scenario(
                &fixture,
                "reload",
                vec![IvyJson::object([(
                    "executables",
                    IvyJson::object([("systemctl", executable)]),
                )])],
                empty(),
            ),
        )
        .await;
    }
    for code in [
        "ENOENT".into(),
        "EACCES".into(),
        IvyJson::Number(7.0),
        "ERR_CHILD_PROCESS_STDIO_MAXBUFFER".into(),
        IvyJson::Null,
    ] {
        check(
            "original-command-error-code-and-tail",
            scenario(
                &fixture,
                "reload",
                vec![configuration()],
                IvyJson::object([(
                    "commands",
                    IvyJson::Array(vec![IvyJson::object([
                        ("error", code),
                        ("stderr", format!("prefix😀{}", "x".repeat(1023)).into()),
                    ])]),
                )]),
            ),
        )
        .await;
    }
}

#[tokio::test]
async fn systemd_definition_reads_admit_only_bounded_root_owned_regular_files() {
    let fixture = Fixture::new();
    let path = format!(
        "/etc/systemd/system/{}",
        String::from_utf16_lossy(&unit().0)
    );
    let files = [
        empty(),
        v(r#"{"data":"\uFEFFdirect unit\n","mode":420}"#),
        v(r#"{"data":"x","uid":12}"#),
        v(r#"{"data":"x","mode":438}"#),
        v(r#"{"file":false}"#),
        v(r#"{"symlink":true}"#),
        v(r#"{"size":65536,"data":"at-boundary"}"#),
        v(r#"{"size":65537}"#),
        v(r#"{"error":"EACCES"}"#),
        v(r#"{"readError":"ENOENT"}"#),
        v(r#"{"readError":"EIO"}"#),
    ];
    check(
        "missing-unit-definition",
        scenario(
            &fixture,
            "readUnit",
            vec![configuration(), IvyJson::String(unit())],
            empty(),
        ),
    )
    .await;
    for file in files {
        check(
            "unit-definition-admission",
            scenario(
                &fixture,
                "readUnit",
                vec![configuration(), IvyJson::String(unit())],
                IvyJson::object([("files", IvyJson::object([(path.clone(), file)]))]),
            ),
        )
        .await;
    }
}

#[tokio::test]
async fn systemd_replacement_keeps_original_write_close_rename_and_cleanup_boundaries() {
    let fixture = Fixture::new();
    let args = vec![configuration(), IvyJson::String(unit()), "unit\n😀".into()];
    check(
        "replacement-success",
        scenario(&fixture, "replaceUnit", args.clone(), empty()),
    )
    .await;
    for (op, at) in [
        ("open", 1),
        ("write", 1),
        ("sync", 1),
        ("close", 1),
        ("rename", 1),
        ("open", 2),
        ("sync", 2),
        ("close", 2),
    ] {
        check(
            "replacement-boundary-failure",
            scenario(
                &fixture,
                "replaceUnit",
                args.clone(),
                IvyJson::object([(
                    "faults",
                    IvyJson::Array(vec![IvyJson::object([
                        ("op", op.into()),
                        ("at", IvyJson::Number(at as f64)),
                        ("code", "EIO".into()),
                    ])]),
                )]),
            ),
        )
        .await;
    }
    for model in [
        v(r#"{"platform":"win32"}"#),
        v(r#"{"uid":42}"#),
        v(r#"{"createRace":{"data":"competitor"}}"#),
    ] {
        check(
            "replacement-root-and-exclusive-create",
            scenario(&fixture, "replaceUnit", args.clone(), model),
        )
        .await;
    }
    for content in [
        JsString::from(""),
        JsString::from("nul\0unit"),
        JsString::from("x".repeat(65_537)),
        JsString(vec![0xd800]),
        JsString::from("x".repeat(65_536)),
    ] {
        check(
            "replacement-content-admission-and-lossless-boundary",
            scenario(
                &fixture,
                "replaceUnit",
                vec![
                    configuration(),
                    IvyJson::String(unit()),
                    IvyJson::String(content),
                ],
                empty(),
            ),
        )
        .await;
    }
    check(
        "cleanup-unlink-failure-is-ignored",
        scenario(
            &fixture,
            "replaceUnit",
            args,
            v(r#"{"faults":[{"op":"rename","code":"EIO"},{"op":"unlink","code":"EACCES"}]}"#),
        ),
    )
    .await;
}

#[tokio::test]
async fn systemd_admitted_replacement_finishes_after_waiter_cancellation() {
    let host = Arc::new(Host::new(empty(), None));
    host.inner.pause.store(true, Ordering::SeqCst);
    let adapter = host.adapter();
    let waiter = tokio::spawn(async move {
        adapter
            .replace_unit(configuration(), unit(), "new unit".into())
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        host.inner.opened.notified(),
    )
    .await
    .unwrap();
    waiter.abort();
    host.inner.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let done = {
                let state = host.inner.state.lock().unwrap();
                state.trace.last().is_some_and(|event| {
                    event.get("op") == Some(&"close".into())
                        && event.get("path") == Some(&"/etc/systemd/system".into())
                })
            };
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await
        }
    })
    .await
    .unwrap();
    let result = host.inner.output(Ok(IvyJson::Null));
    append(
        "systemd-independent-cancelled-waiter",
        &result,
        &IvyJson::Null,
    );
    assert_eq!(
        result
            .required("files")
            .unwrap()
            .required(&format!(
                "/etc/systemd/system/{}",
                String::from_utf16_lossy(&unit().0)
            ))
            .unwrap()
            .required("data")
            .unwrap(),
        &IvyJson::from("new unit")
    );
}

#[tokio::test]
async fn systemd_native_file_boundary_writes_syncs_closes_and_refuses_reuse_in_disposable_root() {
    let fixture = Fixture::new();
    let host = NativeSystemdHost::new(fixture.0.clone());
    let path = node_path::from_native(&fixture.0.join("unit.tmp"));
    let mut file = host.open(path.clone(), true).await.unwrap();
    file.write("native😀".as_bytes().to_vec()).await.unwrap();
    file.sync().await.unwrap();
    file.close().await.unwrap();
    assert_eq!(
        fs::read(fixture.0.join("unit.tmp")).unwrap(),
        "native😀".as_bytes()
    );
    assert!(host.open(path.clone(), true).await.is_err());
    let destination = node_path::from_native(&fixture.0.join("unit.final"));
    host.rename(path, destination.clone()).await.unwrap();
    host.unlink(destination).await.unwrap();
    assert!(!fixture.0.join("unit.final").exists());
    append(
        "systemd-independent-native-file-effects",
        &true.into(),
        &IvyJson::Null,
    );
}

#[tokio::test]
async fn systemd_native_commands_keep_real_exit_spawn_buffer_and_utf16_error_tail() {
    let fixture = Fixture::new();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/host-systemd-child.rs");
    let binary = fixture
        .0
        .join(if cfg!(windows) { "state.exe" } else { "state" });
    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg(source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(directory) = std::env::var_os("IVY_HOST_SYSTEMD_NATIVE_DIRECTORY") {
        fs::create_dir_all(&directory).unwrap();
        fs::copy(
            &binary,
            PathBuf::from(directory).join(if cfg!(windows) {
                "systemd-child.exe"
            } else {
                "systemd-child"
            }),
        )
        .unwrap();
    }
    #[cfg(windows)]
    {
        fs::create_dir_all(fixture.0.join("dist/native")).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_ivy-job"),
            fixture.0.join("dist/native/ivy-job.exe"),
        )
        .unwrap();
    }
    for mode in ["state", "exit", "unicode", "overflow", "missing"] {
        let file = fixture.0.join(if cfg!(windows) {
            format!("{mode}.exe")
        } else {
            mode.into()
        });
        if mode != "missing" && mode != "state" {
            fs::copy(&binary, &file).unwrap();
        }
        #[cfg(not(windows))]
        let path = node_path::from_native(&file);
        #[cfg(windows)]
        let path = JsString::from(format!("//?/{}", file.to_string_lossy().replace('\\', "/")));
        let config = IvyJson::object([(
            "executables",
            IvyJson::object([("systemctl", IvyJson::String(path))]),
        )]);
        let op = if mode == "state" {
            "inspectUnit"
        } else {
            "reload"
        };
        let args = if op == "inspectUnit" {
            vec![config, IvyJson::String(unit())]
        } else {
            vec![config]
        };
        let input = scenario(&fixture, op, args, v(r#"{"nativeCommands":true}"#));
        let actual = run(&input, Some(fixture.0.clone())).await;
        compare(&format!("systemd-native-{mode}"), &input, actual);
    }
}
