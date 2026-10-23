use super::*;
use ivy_host_runtime::{
    docker::{DockerAdapter, DockerCommand, DockerHost, NativeDockerHost},
    process::ProcessResult,
};
use std::time::Duration;
struct Host {
    platform: String,
    paths: node_path::Context,
    model: IvyJson,
    state: Mutex<DockerState>,
    asleep: Notify,
    native: Option<Arc<NativeDockerHost>>,
    pause: AtomicBool,
    entered: Notify,
    release: Notify,
}
struct DockerState {
    trace: Vec<IvyJson>,
    directories: Vec<IvyJson>,
    index: usize,
    counts: BTreeMap<String, usize>,
}
impl Host {
    fn new(model: IvyJson, native: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            platform: model
                .get("platform")
                .map(utf)
                .unwrap_or_else(|| "linux".into()),
            paths: node_path::Context::current().unwrap(),
            model,
            state: Mutex::new(DockerState {
                trace: vec![],
                directories: vec![],
                index: 0,
                counts: BTreeMap::new(),
            }),
            asleep: Notify::new(),
            native: native.map(|root| Arc::new(NativeDockerHost::new(root).unwrap())),
            pause: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
        })
    }
    fn event(&self, event: IvyJson) -> IvyResult<()> {
        let mut state = self.state.lock().unwrap();
        let op = utf(event.required("op").unwrap());
        state.trace.push(event);
        let count = state.counts.entry(op.clone()).or_default();
        *count += 1;
        let at = *count;
        if self
            .model
            .get("faults")
            .and_then(IvyJson::as_array)
            .is_some_and(|rows| {
                rows.iter().any(|v| {
                    v.get("op") == Some(&IvyJson::from(op.as_str()))
                        && v.get("at").and_then(IvyJson::as_f64).unwrap_or(1.0) == at as f64
                })
            })
        {
            return Err(IvyError::internal());
        }
        Ok(())
    }
    fn next(&self) -> IvyResult<ProcessResult> {
        let mut state = self.state.lock().unwrap();
        let row = self
            .model
            .get("commands")
            .and_then(IvyJson::as_array)
            .and_then(|rows| rows.get(state.index))
            .cloned()
            .unwrap_or_else(empty);
        state.index += 1;
        if let Some(code) = row.get("error") {
            return Err(IvyError::new(utf(code), "Injected command failure."));
        }
        Ok(ProcessResult {
            exit_code: Some(row.get("exitCode").and_then(IvyJson::as_f64).unwrap_or(0.0) as i32),
            signal: None,
            error_code: None,
            stdout: row.get("stdout").map(utf).unwrap_or_default(),
            stderr: row.get("stderr").map(utf).unwrap_or_default(),
            truncated: row.get("truncated") == Some(&true.into()),
        })
    }
    fn adapter(self: &Arc<Self>) -> DockerAdapter {
        if self.model.get("customCommand") == Some(&false.into()) {
            return DockerAdapter::with_host(self.clone());
        }
        let host = self.clone();
        let command: Arc<dyn DockerCommand> = Arc::new(
            move |_config: IvyJson,
                  args: Vec<JsString>,
                  timeout: f64,
                  secrets: Vec<JsString>|
                  -> BoxFuture<'static, IvyResult<ProcessResult>> {
                let host = host.clone();
                Box::pin(async move {
                    host.event(IvyJson::object([
                        ("op", "command".into()),
                        ("args", strings(args)),
                        ("timeoutMs", IvyJson::Number(timeout)),
                        ("extraSecrets", strings(secrets)),
                    ]))?;
                    if host.pause.load(Ordering::SeqCst) {
                        host.entered.notify_one();
                        host.release.notified().await;
                    }
                    host.next()
                })
            },
        );
        DockerAdapter::with_command(self.clone(), command)
    }
    fn output(&self, result: IvyResult<IvyJson>) -> IvyJson {
        let state = self.state.lock().unwrap();
        IvyJson::object([
            ("operation", wire(result)),
            ("trace", IvyJson::Array(state.trace.clone())),
            ("directories", IvyJson::Array(state.directories.clone())),
        ])
    }
}
fn strings(values: Vec<JsString>) -> IvyJson {
    IvyJson::Array(values.into_iter().map(IvyJson::String).collect())
}
fn set(value: &mut IvyJson, path: &[&str], replacement: IvyJson) {
    if path.len() == 1 {
        value.insert(path[0], replacement);
    } else {
        let mut child = value.required(path[0]).unwrap().clone();
        set(&mut child, &path[1..], replacement);
        value.insert(path[0], child);
    }
}
impl DockerHost for Host {
    fn platform(&self) -> &str {
        &self.platform
    }
    fn paths(&self) -> &node_path::Context {
        &self.paths
    }
    fn mkdir(&self, path: JsString) -> BoxFuture<'static, IvyResult<()>> {
        let result = self.event(IvyJson::object([
            ("op", "mkdir".into()),
            ("path", IvyJson::String(path.clone())),
        ]));
        if result.is_ok() {
            self.state
                .lock()
                .unwrap()
                .directories
                .push(IvyJson::String(path.clone()));
        }
        if let Some(native) = &self.native {
            let native = native.clone();
            return Box::pin(async move {
                result?;
                native.mkdir(path).await
            });
        }
        Box::pin(async move { result })
    }
    fn configuration(&self, path: JsString) -> BoxFuture<'static, IvyResult<IvyJson>> {
        let event = self.event(IvyJson::object([
            ("op", "configuration".into()),
            ("path", IvyJson::String(path.clone())),
        ]));
        if let Some(native) = &self.native {
            let native = native.clone();
            return Box::pin(async move {
                event?;
                native.configuration(path).await
            });
        }
        let result = if self.model.get("configError") == Some(&true.into()) {
            Err(IvyError::internal())
        } else {
            Ok(self
                .model
                .get("configuration")
                .cloned()
                .unwrap_or_else(empty))
        };
        Box::pin(async move {
            event?;
            result
        })
    }
    fn verify_resources(
        &self,
        _host: IvyJson,
        instance: JsString,
        scope: IvyJson,
    ) -> BoxFuture<'static, IvyResult<()>> {
        let result = self.event(IvyJson::object([
            ("op", "resources".into()),
            ("instance", IvyJson::String(instance)),
            ("scope", scope),
        ]));
        Box::pin(async move { result })
    }
    fn execute(
        &self,
        config: IvyJson,
        args: Vec<JsString>,
        timeout: f64,
        redact: Vec<JsString>,
    ) -> BoxFuture<'static, IvyResult<ProcessResult>> {
        let event = self.event(IvyJson::object([
            ("op", "execute".into()),
            ("args", strings(args.clone())),
            ("timeoutMs", IvyJson::Number(timeout)),
            ("redact", strings(redact.clone())),
        ]));
        if let Some(native) = &self.native {
            let native = native.clone();
            return Box::pin(async move {
                event?;
                native.execute(config, args, timeout, redact).await
            });
        }
        let result = self.next();
        Box::pin(async move {
            event?;
            result
        })
    }
    fn sleep(&self) -> BoxFuture<'static, ()> {
        self.event(IvyJson::object([
            ("op", "sleep".into()),
            ("milliseconds", IvyJson::Number(1000.0)),
        ]))
        .unwrap();
        self.asleep.notify_one();
        if self.model.get("pauseObserver") == Some(&true.into()) {
            Box::pin(std::future::pending())
        } else {
            Box::pin(async {})
        }
    }
}
fn host() -> IvyJson {
    v(
        r#"{"hostId":"docker-host","runtimeRoot":"/fixture/runtime","executables":{"docker":"/fixture/docker"},"instances":[]}"#,
    )
}
fn instance() -> IvyJson {
    v(
        r#"{"instanceId":"hive","componentId":"hive","docker":{"imageRepository":"ivy/hive","ports":[]}}"#,
    )
}
fn hash(letter: char) -> IvyJson {
    format!("sha256:{}", letter.to_string().repeat(64)).into()
}
fn target() -> IvyJson {
    IvyJson::object([
        ("revision", hash('b')),
        ("configPath", "/fixture/config/instance.json".into()),
    ])
}
fn candidate() -> IvyJson {
    IvyJson::object([
        ("candidateId", hash('c')),
        ("dockerImage", hash('d')),
        ("buildId", hash('e')),
        ("artifactRoot", "/fixture/artifact".into()),
    ])
}
fn config() -> IvyJson {
    v(
        r#"{"dataRoot":"/fixture/data","artifactRoot":"/fixture/artifact","settings":{"listenHost":"0.0.0.0","listenPort":39999,"credentials":[],"backup":{"directory":"/fixture/backup","intervalHours":24,"retain":7}}}"#,
    )
}
fn manifest() -> IvyJson {
    v(
        r#"{"entrypoint":{"executable":"node","args":["dist/main.js"],"timeoutMs":5000},"restart":{"policy":"always"}}"#,
    )
}
fn id() -> String {
    "a".repeat(64)
}
fn row(stdout: impl Into<IvyJson>) -> IvyJson {
    IvyJson::object([("stdout", stdout.into())])
}
fn ls() -> IvyJson {
    row(format!("{}\n", id()))
}
fn raw(status: &str) -> IvyJson {
    let name = ivy_host_runtime::docker::container_name("docker-host".into(), "hive".into(), None)
        .unwrap();
    IvyJson::object([
        ("Id", id().into()),
        ("Name", format!("/{name}").into()),
        ("Image", hash('d')),
        (
            "Config",
            IvyJson::object([(
                "Labels",
                IvyJson::object([
                    ("dev.ivy.host", "docker-host".into()),
                    ("dev.ivy.instance", "hive".into()),
                    ("dev.ivy.candidate", hash('c')),
                    ("dev.ivy.launch", hash('b')),
                ]),
            )]),
        ),
        ("HostConfig", v(r#"{"CgroupParent":""}"#)),
        (
            "State",
            IvyJson::object([
                ("Status", status.into()),
                ("ExitCode", IvyJson::Number(0.0)),
                (
                    "StartedAt",
                    if status == "created" {
                        "0001-01-01T00:00:00Z".into()
                    } else {
                        "2026-01-01T00:00:00.000Z".into()
                    },
                ),
            ]),
        ),
    ])
}
fn inspected(status: &str) -> IvyJson {
    row(json::stringify(&raw(status), usize::MAX).unwrap())
}
fn model(commands: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([("commands", IvyJson::Array(commands))])
}
fn scenario(fixture: &Fixture, operation: &str, args: Vec<IvyJson>, mut model: IvyJson) -> IvyJson {
    if operation == "startContainer" {
        model.insert("pathStyle", "native".into());
    }
    IvyJson::object([
        (
            "fixtureRoot",
            IvyJson::String(node_path::from_native(&fixture.0)),
        ),
        ("operation", operation.into()),
        ("args", IvyJson::Array(args)),
        ("model", model),
    ])
}
fn fixture_path(path: &str) -> IvyJson {
    if cfg!(windows) {
        format!("C:{path}").into()
    } else {
        path.into()
    }
}
fn start_args() -> Vec<IvyJson> {
    let mut args = vec![
        host(),
        instance(),
        target(),
        config(),
        candidate(),
        manifest(),
        false.into(),
    ];
    args[2].insert("configPath", fixture_path("/fixture/config/instance.json"));
    args[3].insert("dataRoot", fixture_path("/fixture/data"));
    args[3].insert("artifactRoot", fixture_path("/fixture/artifact"));
    set(
        &mut args[3],
        &["settings", "backup", "directory"],
        fixture_path("/fixture/backup"),
    );
    args[4].insert("artifactRoot", fixture_path("/fixture/artifact"));
    args
}
fn result(value: &ProcessResult) -> IvyJson {
    IvyJson::object([
        (
            "exitCode",
            value
                .exit_code
                .map(|v| IvyJson::Number(v as f64))
                .unwrap_or(IvyJson::Null),
        ),
        (
            "signal",
            value
                .signal
                .clone()
                .map(IvyJson::from)
                .unwrap_or(IvyJson::Null),
        ),
        (
            "errorCode",
            value
                .error_code
                .clone()
                .map(IvyJson::from)
                .unwrap_or(IvyJson::Null),
        ),
        ("stdout", value.stdout.clone().into()),
        ("stderr", value.stderr.clone().into()),
        ("truncated", value.truncated.into()),
    ])
}
async fn run(input: &IvyJson, native: Option<PathBuf>) -> IvyJson {
    let host = Host::new(input.required("model").unwrap().clone(), native);
    let adapter = host.adapter();
    let args = input.required("args").unwrap().as_array().unwrap();
    let output: IvyResult<IvyJson> = async {
        Ok(match input.required("operation")?.as_str()?.as_str() {
            "containerName" => ivy_host_runtime::docker::container_name(
                s(&args[0]),
                s(&args[1]),
                args.get(2).cloned(),
            )?
            .into(),
            "dockerRestartPolicy" => {
                IvyJson::String(ivy_host_runtime::docker::docker_restart_policy(&args[0])?)
            }
            "inspectContainer" => adapter
                .inspect_container(args[0].clone(), s(&args[1]))
                .await?
                .unwrap_or(IvyJson::Null),
            "stopContainer" => adapter
                .stop_container(
                    args[0].clone(),
                    s(&args[1]),
                    args[2].as_f64().unwrap(),
                    args.get(3).map(s),
                )
                .await?
                .unwrap_or(IvyJson::Null),
            "verifyContainerImage" => {
                adapter
                    .verify_container_image(
                        args[0].clone(),
                        args[1].clone(),
                        args[2].clone(),
                        args[3].clone(),
                    )
                    .await?;
                IvyJson::Null
            }
            "startContainer" => {
                let owner = adapter
                    .start_container(
                        args[0].clone(),
                        args[1].clone(),
                        args[2].clone(),
                        args[3].clone(),
                        args[4].clone(),
                        args[5].clone(),
                        args[6] == true.into(),
                        args.get(7).cloned(),
                    )
                    .await?;
                if host.model.get("pauseObserver") == Some(&true.into()) {
                    host.asleep.notified().await;
                    let verification = if host.model.get("verifyOwner") == Some(&true.into()) {
                        match owner.verify().await {
                            Ok(()) => v(r#"{"verified":true}"#),
                            Err(error) => IvyJson::object([
                                ("verified", false.into()),
                                ("error", error.to_wire()),
                            ]),
                        }
                    } else {
                        IvyJson::Null
                    };
                    owner.dispose();
                    IvyJson::object([
                        ("verification", verification),
                        ("completion", result(&owner.completion.wait().await?)),
                    ])
                } else {
                    result(&owner.completion.wait().await?)
                }
            }
            _ => panic!("Unlisted Docker operation"),
        })
    }
    .await;
    host.output(output)
}
fn oracle(input: &IvyJson) -> IvyJson {
    let root = std::env::var_os("IVY_HOST_SOURCE_ROOT").unwrap();
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-docker-oracle.mjs",
        ])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn compare(name: &str, input: &IvyJson, actual: IvyJson) {
    let expected = oracle(input);
    if let Some(path) = std::env::var_os("IVY_HOST_DOCKER_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            json::stringify(
                &IvyJson::object([
                    ("case", name.into()),
                    ("input", input.clone()),
                    ("actual", actual.clone()),
                    ("expected", expected.clone())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_eq!(
        json::canonical(&actual, usize::MAX).unwrap(),
        json::canonical(&expected, usize::MAX).unwrap(),
        "{name}"
    );
}
async fn check(name: &str, input: IvyJson) {
    let actual = tokio::time::timeout(Duration::from_secs(10), run(&input, None))
        .await
        .expect(name);
    compare(name, &input, actual);
}
fn admission_refused(output: IvyJson, code: Option<&str>) {
    let operation = output.required("operation").unwrap();
    assert_eq!(operation.get("ok"), Some(&false.into()), "{output:?}");
    if let Some(code) = code {
        assert_eq!(
            operation
                .required("error")
                .unwrap()
                .required("data")
                .unwrap()
                .get("code"),
            Some(&code.into()),
            "{output:?}"
        );
    }
    assert_eq!(output.get("trace"), Some(&IvyJson::Array(vec![])));
    assert_eq!(output.get("directories"), Some(&IvyJson::Array(vec![])));
}

#[tokio::test]
async fn docker_inspection_and_stop_preserve_exact_identity_and_unknown_outcomes() {
    let fixture = Fixture::new();
    for (name, commands) in [
        ("absent", vec![empty()]),
        ("owned", vec![ls(), inspected("running")]),
        ("ambiguous", vec![row(format!("{}\n{}", id(), id()))]),
        ("invalid-id", vec![row("short")]),
        ("daemon-failure", vec![v(r#"{"error":"daemon_failed"}"#)]),
        ("null-inspection", vec![ls(), row("null")]),
        ("invalid-json-inspection", vec![ls(), row("{")]),
    ] {
        check(
            name,
            scenario(
                &fixture,
                "inspectContainer",
                vec![host(), "hive".into()],
                model(commands),
            ),
        )
        .await;
    }
    for key in ["Id", "Name", "Image", "State", "Config", "HostConfig"] {
        let mut value = raw("running");
        value.remove(key);
        check(
            key,
            scenario(
                &fixture,
                "inspectContainer",
                vec![host(), "hive".into()],
                model(vec![
                    ls(),
                    row(json::stringify(&value, usize::MAX).unwrap()),
                ]),
            ),
        )
        .await;
    }
    for (name, path, value) in [
        ("null-state", vec!["State"], IvyJson::Null),
        ("id-changed", vec!["Id"], "b".repeat(64).into()),
        (
            "exit-out-of-range",
            vec!["State", "ExitCode"],
            IvyJson::Number(256.0),
        ),
        (
            "candidate-null",
            vec!["Config", "Labels", "dev.ivy.candidate"],
            IvyJson::Null,
        ),
        ("null-labels", vec!["Config", "Labels"], IvyJson::Null),
        ("false-labels", vec!["Config", "Labels"], false.into()),
        (
            "array-labels",
            vec!["Config", "Labels"],
            IvyJson::Array(vec![]),
        ),
    ] {
        let mut raw = raw("running");
        set(&mut raw, &path, value);
        check(
            name,
            scenario(
                &fixture,
                "inspectContainer",
                vec![host(), "hive".into()],
                model(vec![ls(), row(json::stringify(&raw, usize::MAX).unwrap())]),
            ),
        )
        .await;
    }
    for status in [
        "created",
        "exited",
        "dead",
        "paused",
        "removing",
        "running",
        "restarting",
    ] {
        let mut commands = vec![ls(), inspected(status)];
        if ["running", "restarting"].contains(&status) {
            commands.extend([empty(), ls(), inspected("exited")]);
        }
        check(
            status,
            scenario(
                &fixture,
                "stopContainer",
                vec![host(), "hive".into(), IvyJson::Number(1001.0), id().into()],
                model(commands),
            ),
        )
        .await;
    }
    for (name, commands, expected) in [
        (
            "stop-lost-reply-proved",
            vec![
                ls(),
                inspected("running"),
                v(r#"{"error":"lost_reply"}"#),
                ls(),
                inspected("dead"),
            ],
            id(),
        ),
        (
            "stop-lost-reply-unknown",
            vec![
                ls(),
                inspected("running"),
                v(r#"{"error":"lost_reply"}"#),
                ls(),
                inspected("running"),
            ],
            id(),
        ),
        (
            "stop-not-observed",
            vec![ls(), inspected("running"), empty(), empty()],
            id(),
        ),
        (
            "stop-stale-id",
            vec![ls(), inspected("running")],
            "f".repeat(64),
        ),
    ] {
        check(
            name,
            scenario(
                &fixture,
                "stopContainer",
                vec![host(), "hive".into(), IvyJson::Number(0.0), expected.into()],
                model(commands),
            ),
        )
        .await;
    }
    let mut restored = host();
    restored.insert("runtimeRoot", "/fixture/restored-runtime".into());
    restored.insert("restoredFrom", v(r#"{"backupId":"backup-a"}"#));
    check(
        "restored-host-name",
        scenario(
            &fixture,
            "inspectContainer",
            vec![restored, "hive".into()],
            model(vec![empty()]),
        ),
    )
    .await;
    for malformed in [
        IvyJson::Null,
        false.into(),
        IvyJson::Number(0.0),
        "".into(),
        IvyJson::Array(vec![]),
        empty(),
    ] {
        let mut restored = host();
        restored.insert("restoredFrom", malformed);
        admission_refused(
            run(
                &scenario(
                    &fixture,
                    "inspectContainer",
                    vec![restored, "hive".into()],
                    empty(),
                ),
                None,
            )
            .await,
            None,
        );
    }
}

#[tokio::test]
async fn docker_image_and_command_admission_match_the_original() {
    let fixture = Fixture::new();
    for malformed in [
        IvyJson::Null,
        false.into(),
        true.into(),
        IvyJson::Number(0.0),
        IvyJson::Number(1.0),
        "".into(),
        IvyJson::Array(vec![]),
    ] {
        for operation in ["verifyContainerImage", "startContainer"] {
            for binding in ["docker", "dockerImage"] {
                let mut args = if operation == "startContainer" {
                    start_args()
                } else {
                    vec![host(), instance(), candidate(), manifest()]
                };
                if binding == "docker" {
                    args[1].insert(binding, malformed.clone());
                } else {
                    let at = if operation == "startContainer" { 4 } else { 2 };
                    args[at].insert(binding, malformed.clone());
                }
                admission_refused(
                    run(&scenario(&fixture, operation, args, empty()), None).await,
                    Some("unsupported_runtime"),
                );
            }
        }
        let mut invalid_host = host();
        set(&mut invalid_host, &["executables", "docker"], malformed);
        let mut data = empty();
        data.insert("customCommand", false.into());
        admission_refused(
            run(
                &scenario(
                    &fixture,
                    "inspectContainer",
                    vec![invalid_host, "hive".into()],
                    data,
                ),
                None,
            )
            .await,
            Some("unsupported_runtime"),
        );
    }
    for address in [
        "127.0.0.1",
        "::1",
        "fe80::1%eth0",
        "::ffff:192.0.2.1",
        "0.0.0.0",
        "127.00.0.1",
        "localhost",
        "[::1]",
        "fe80::1%bad_zone",
    ] {
        let mut entry = instance();
        set(
            &mut entry,
            &["docker", "ports"],
            v(&format!(
                r#"[{{"host":"{address}","port":80,"containerPort":80}}]"#
            )),
        );
        check(
            address,
            scenario(
                &fixture,
                "verifyContainerImage",
                vec![host(), entry, candidate(), manifest()],
                model(vec![row(hash('d'))]),
            ),
        )
        .await;
    }
    for repository in [
        "ivy/hive",
        "a-b.c_d/e9",
        "bad:tag",
        "Upper/Case",
        "a//b",
        "a..b",
        "a/",
        "-a",
    ] {
        let mut entry = instance();
        set(
            &mut entry,
            &["docker", "imageRepository"],
            repository.into(),
        );
        check(
            repository,
            scenario(
                &fixture,
                "verifyContainerImage",
                vec![host(), entry, candidate(), manifest()],
                model(vec![row(hash('d'))]),
            ),
        )
        .await;
    }
    check(
        "image-missing",
        scenario(
            &fixture,
            "verifyContainerImage",
            vec![host(), instance(), candidate(), manifest()],
            model(vec![row(hash('f'))]),
        ),
    )
    .await;
    for platform in ["linux", "win32"] {
        let mut data = model(vec![empty()]);
        data.insert("customCommand", false.into());
        data.insert("platform", platform.into());
        let mut host = host();
        host.insert("instances",v(r#"[{"credential":"owner-secret","componentId":"worker"},{"componentId":"hive","settings":{"credentials":[{"token":"runtime-secret"}]}}]"#));
        check(
            platform,
            scenario(
                &fixture,
                "inspectContainer",
                vec![host, "hive".into()],
                data,
            ),
        )
        .await;
    }
}

#[tokio::test]
async fn docker_creation_adoption_mounts_and_reconciliation_match_the_original() {
    let fixture = Fixture::new();
    for key in ["workRoot", "logsRoot"] {
        for malformed in [
            IvyJson::Null,
            false.into(),
            true.into(),
            IvyJson::Number(0.0),
            "".into(),
            IvyJson::Array(vec![]),
            empty(),
        ] {
            let mut args = start_args();
            args[3].insert(key, malformed);
            admission_refused(
                run(
                    &scenario(&fixture, "startContainer", args, model(vec![empty()])),
                    None,
                )
                .await,
                Some("invalid_arguments"),
            );
        }
    }
    for (name, commands) in [
        (
            "create-start-exit",
            vec![
                empty(),
                empty(),
                empty(),
                ls(),
                inspected("created"),
                empty(),
                ls(),
                inspected("exited"),
                ls(),
                inspected("exited"),
                row("bounded logs"),
            ],
        ),
        (
            "adopt-exited",
            vec![
                ls(),
                inspected("exited"),
                ls(),
                inspected("exited"),
                row("bounded logs"),
            ],
        ),
        (
            "create-failure-unproved",
            vec![empty(), empty(), v(r#"{"error":"lost_reply"}"#), empty()],
        ),
        ("created-missing", vec![empty(), empty(), empty(), empty()]),
    ] {
        check(
            name,
            scenario(&fixture, "startContainer", start_args(), model(commands)),
        )
        .await;
    }
    for path in [
        "/".into(),
        "relative".into(),
        fixture_path("/fixture/comma,path"),
        fixture_path("/fixture/control\npath"),
    ] {
        let mut args = start_args();
        args[3].insert("dataRoot", path);
        check(
            "invalid-mount",
            scenario(&fixture, "startContainer", args, model(vec![empty()])),
        )
        .await;
    }
    let mut args = start_args();
    args[3].insert("workRoot", fixture_path("/fixture/work"));
    args[3].insert("logsRoot", fixture_path("/fixture/logs"));
    set(
        &mut args[1],
        &["docker", "ports"],
        v(r#"[{"host":"::1","port":8080,"containerPort":39999}]"#),
    );
    check(
        "work-args-and-ipv6-port",
        scenario(
            &fixture,
            "startContainer",
            args,
            model(vec![empty(), empty(), empty(), empty()]),
        ),
    )
    .await;
    let mut mismatch = raw("running");
    set(
        &mut mismatch,
        &["Config", "Labels", "dev.ivy.launch"],
        hash('f'),
    );
    check(
        "different-live-launch",
        scenario(
            &fixture,
            "startContainer",
            start_args(),
            model(vec![
                ls(),
                row(json::stringify(&mismatch, usize::MAX).unwrap()),
            ]),
        ),
    )
    .await;
}

#[tokio::test]
async fn docker_observer_is_passive_and_disposal_never_stops_the_container() {
    let fixture = Fixture::new();
    for (name, observer, verification) in [
        (
            "running",
            vec![ls(), inspected("running")],
            vec![ls(), inspected("running")],
        ),
        (
            "daemon-failure",
            vec![v(r#"{"error":"daemon_failed"}"#)],
            vec![ls(), inspected("running")],
        ),
        (
            "paused",
            vec![ls(), inspected("paused")],
            vec![ls(), inspected("paused")],
        ),
    ] {
        let mut commands = vec![ls(), inspected("running")];
        commands.extend(observer);
        commands.extend(verification);
        let mut data = model(commands);
        data.insert("pauseObserver", true.into());
        data.insert("verifyOwner", true.into());
        check(
            name,
            scenario(&fixture, "startContainer", start_args(), data),
        )
        .await;
    }
    let data = model(vec![ls(), inspected("running"), empty()]);
    check(
        "identity-disappears",
        scenario(&fixture, "startContainer", start_args(), data),
    )
    .await;
}

#[tokio::test]
async fn docker_accepted_creation_survives_a_dropped_waiter() {
    let data = model(vec![empty(), empty(), empty(), empty()]);
    let host = Host::new(data, None);
    host.pause.store(true, Ordering::SeqCst);
    let adapter = host.adapter();
    let [
        host_config,
        instance,
        target,
        config,
        candidate,
        manifest,
        _,
    ]: [IvyJson; 7] = start_args().try_into().unwrap();
    let task = tokio::spawn(async move {
        adapter
            .start_container(
                host_config,
                instance,
                target,
                config,
                candidate,
                manifest,
                false,
                None,
            )
            .await
    });
    host.entered.notified().await;
    task.abort();
    let _ = task.await;
    host.pause.store(false, Ordering::SeqCst);
    host.release.notify_one();
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if host.state.lock().unwrap().index >= 4 {
            break;
        }
        assert!(tokio::time::Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let state = host.state.lock().unwrap();
    assert_eq!(state.index, 4);
    assert_eq!(state.directories.len(), 2);
}

#[tokio::test]
async fn docker_resources_reloaded_credentials_and_launch_recovery_match_the_original() {
    let fixture = Fixture::new();
    let budget = v(r#"{"memoryHighBytes":201326592,"memoryMaxBytes":268435456,"tasksMax":64}"#);
    let scope = resources::linux_resource_scope(
        &resources::installation_identity(&host()).unwrap(),
        &budget,
    )
    .unwrap();
    for driver in ["systemd 2", "cgroupfs 2", "systemd 1"] {
        let mut args = start_args();
        args.push(scope.clone());
        let mut value = raw("exited");
        set(
            &mut value,
            &["HostConfig", "CgroupParent"],
            scope.required("slice").unwrap().clone(),
        );
        let inspect = row(json::stringify(&value, usize::MAX).unwrap());
        check(
            driver,
            scenario(
                &fixture,
                "startContainer",
                args,
                model(vec![
                    row(driver),
                    ls(),
                    inspect.clone(),
                    ls(),
                    inspect,
                    row("logs"),
                ]),
            ),
        )
        .await;
    }
    for (name, logs, missing) in [
        ("credential-reload", row("private logs"), false),
        ("logs-unavailable", v(r#"{"error":"daemon_failed"}"#), false),
        ("configuration-unavailable", row("logs"), true),
    ] {
        let mut data = model(vec![
            ls(),
            inspected("exited"),
            ls(),
            inspected("exited"),
            logs,
        ]);
        data.insert(
            "configuration",
            v(r#"{"settings":{"credentials":[{"token":"rotated-secret"}]}}"#),
        );
        data.insert("configError", missing.into());
        check(
            name,
            scenario(&fixture, "startContainer", start_args(), data),
        )
        .await;
    }
    for (name, create, start, started) in [
        (
            "create-lost-reply-proved",
            v(r#"{"error":"lost_reply"}"#),
            empty(),
            true,
        ),
        (
            "start-lost-reply-proved",
            empty(),
            v(r#"{"error":"lost_reply"}"#),
            true,
        ),
        ("start-not-observed", empty(), empty(), false),
        (
            "start-lost-reply-unknown",
            empty(),
            v(r#"{"error":"lost_reply"}"#),
            false,
        ),
    ] {
        let observed = if started {
            inspected("exited")
        } else {
            inspected("created")
        };
        let commands = vec![
            empty(),
            empty(),
            create,
            ls(),
            inspected("created"),
            start,
            ls(),
            observed.clone(),
            ls(),
            observed,
            row("logs"),
        ];
        check(
            name,
            scenario(&fixture, "startContainer", start_args(), model(commands)),
        )
        .await;
    }
    for recovery in [
        None,
        Some(v(
            r#"{"runtimeRoot":"/restored/state","backupId":"backup-a"}"#,
        )),
        Some(v(r#"{"runtimeRoot":"/other/state","backupId":"backup-a"}"#)),
    ] {
        let mut args = vec![
            "host\u{1f680}".into(),
            IvyJson::String(JsString(vec![0xd800])),
        ];
        if let Some(recovery) = recovery {
            args.push(recovery);
        }
        check(
            "name-identity",
            scenario(&fixture, "containerName", args, empty()),
        )
        .await;
    }
    for policy in [
        empty(),
        v(r#"{"restart":{"policy":"never"}}"#),
        v(r#"{"restart":{"policy":"on-failure"}}"#),
        v(r#"{"restart":{"policy":"always"}}"#),
    ] {
        check(
            "restart-policy",
            scenario(&fixture, "dockerRestartPolicy", vec![policy], empty()),
        )
        .await;
    }
    for restart in [
        IvyJson::Null,
        false.into(),
        IvyJson::Number(0.0),
        "".into(),
        IvyJson::Array(vec![]),
        empty(),
        v(r#"{"policy":null}"#),
        v(r#"{"policy":false}"#),
        v(r#"{"policy":"on-failure:3"}"#),
    ] {
        let mut manifest = manifest();
        manifest.insert("restart", restart);
        assert_eq!(
            ivy_host_runtime::docker::docker_restart_policy(&manifest)
                .err()
                .unwrap()
                .code,
            "invalid_arguments"
        );
        let mut args = start_args();
        args[5] = manifest;
        admission_refused(
            run(&scenario(&fixture, "startContainer", args, empty()), None).await,
            Some("invalid_arguments"),
        );
    }
}

#[tokio::test]
async fn docker_native_boundary_uses_actual_process_files_and_bounded_secret_redaction() {
    let fixture = Fixture::new();
    let binary = fixture.0.join(if cfg!(windows) {
        "docker-command.exe"
    } else {
        "docker-command"
    });
    let compiler = Command::new("rustc")
        .arg("--edition=2024")
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../tests/fixtures/host-docker-child.rs"),
        )
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );
    #[cfg(windows)]
    {
        fs::create_dir_all(fixture.0.join("dist/native")).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_ivy-job"),
            fixture.0.join("dist/native/ivy-job.exe"),
        )
        .unwrap();
    }
    let mut arguments = start_args();
    arguments[0].insert(
        "runtimeRoot",
        IvyJson::String(node_path::from_native(&fixture.0)),
    );
    let mut executables =
        IvyJson::object([("docker", IvyJson::String(node_path::from_native(&binary)))]);
    executables.insert(JsString(vec![0xd800]), "/fixture/unused-alias".into());
    arguments[0].insert("executables", executables);
    arguments[0].insert(
        "instances",
        v(r#"[{"credential":"host-secret","componentId":"worker"}]"#),
    );
    let config_path = fixture.0.join("instance.json");
    fs::write(
        &config_path,
        r#"{"settings":{"credentials":[{"token":"rotated-secret"}]}}"#,
    )
    .unwrap();
    arguments[2].insert(
        "configPath",
        IvyJson::String(node_path::from_native(&config_path)),
    );
    fs::write(fixture.0.join("container-ls.stdout"), format!("{}\n", id())).unwrap();
    fs::write(
        fixture.0.join("container-inspect.stdout"),
        json::stringify(&raw("exited"), usize::MAX).unwrap(),
    )
    .unwrap();
    fs::write(
        fixture.0.join("container-logs.stdout"),
        format!("host-secret rotated-secret {}", "x".repeat(70000)),
    )
    .unwrap();
    fs::write(
        fixture.0.join("container-logs.stderr"),
        "rotated-secret host-secret",
    )
    .unwrap();
    fs::write(
        fixture.0.join("image-inspect.stdout"),
        format!("{}\n", utf(&hash('d'))),
    )
    .unwrap();
    let data = v(
        r#"{"customCommand":false,"nativeCommands":true,"nativeFiles":true,"pathStyle":"native"}"#,
    );
    let input = scenario(&fixture, "startContainer", arguments.clone(), data.clone());
    let actual = run(&input, Some(fixture.0.clone())).await;
    assert_eq!(
        actual.required("operation").unwrap().get("ok"),
        Some(&true.into())
    );
    let completed = actual
        .required("operation")
        .unwrap()
        .required("value")
        .unwrap();
    assert_eq!(completed.get("truncated"), Some(&true.into()));
    let stdout = utf(completed.required("stdout").unwrap());
    let stderr = utf(completed.required("stderr").unwrap());
    assert!(
        !stdout.contains("host-secret")
            && !stdout.contains("rotated-secret")
            && !stderr.contains("host-secret")
            && !stderr.contains("rotated-secret")
    );
    compare("actual-native-owner-files-logs", &input, actual);
    let input = scenario(
        &fixture,
        "verifyContainerImage",
        vec![arguments[0].clone(), instance(), candidate(), manifest()],
        data,
    );
    let actual = run(&input, Some(fixture.0.clone())).await;
    compare("actual-native-image-identity", &input, actual);
    let native = NativeDockerHost::new(fixture.0.clone()).unwrap();
    let image_args = || {
        vec![
            "--host".into(),
            "unix:///var/run/docker.sock".into(),
            "image".into(),
            "inspect".into(),
            "--format".into(),
            "{{.Id}}".into(),
            s(&hash('d')),
        ]
    };
    let image = native
        .execute(arguments[0].clone(), image_args(), 2_147_483_648.0, vec![])
        .await
        .unwrap();
    assert_eq!(image.exit_code, Some(0));
    assert_eq!(image.stdout, format!("{}\n", utf(&hash('d'))));
    let trace = fixture.0.join("native-commands.txt");
    let before = fs::read(&trace).unwrap();
    for timeout in [
        0.0,
        -1.0,
        0.5,
        5000.5,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        u64::MAX as f64,
    ] {
        let error = native
            .execute(arguments[0].clone(), image_args(), timeout, vec![])
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_arguments", "{timeout}");
    }
    for executable in [None, Some(false.into())] {
        let mut config = arguments[0].clone();
        let mut executables = empty();
        if let Some(executable) = executable {
            executables.insert("docker", executable);
        }
        config.insert("executables", executables);
        assert!(
            native
                .execute(config, image_args(), 5000.0, vec![])
                .await
                .is_err()
        );
    }
    assert_eq!(fs::read(&trace).unwrap(), before);
    if let Some(directory) = std::env::var_os("IVY_HOST_DOCKER_NATIVE_DIRECTORY") {
        fs::create_dir_all(&directory).unwrap();
        for (name, path) in [
            ("docker-command.exe", binary),
            ("native-commands.txt", fixture.0.join("native-commands.txt")),
        ] {
            fs::copy(path, PathBuf::from(&directory).join(name)).unwrap();
        }
    }
}
