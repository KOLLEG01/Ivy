use super::*;

#[derive(Default)]
struct ContainerModel {
    raw: Option<IvyJson>,
    trace: Vec<Vec<JsString>>,
    creates: usize,
}
#[derive(Clone, Default)]
struct Docker(Arc<Mutex<ContainerModel>>);
fn output(stdout: String) -> ProcessResult {
    ProcessResult {
        exit_code: Some(0),
        signal: None,
        error_code: None,
        stdout,
        stderr: String::new(),
        truncated: false,
    }
}
fn image() -> IvyJson {
    format!("sha256:{}", "b".repeat(64)).into()
}
impl Docker {
    fn adapter(&self, f: &Fixture) -> DockerAdapter {
        let model = self.clone();
        let command: Arc<dyn DockerCommand> = Arc::new(
            move |_: IvyJson,
                  args: Vec<JsString>,
                  _: f64,
                  _: Vec<JsString>|
                  -> BoxFuture<'static, IvyResult<ProcessResult>> {
                let model = model.clone();
                Box::pin(async move {
                    let mut s = model.0.lock().unwrap();
                    s.trace.push(args.clone());
                    let text = args
                        .iter()
                        .map(|a| a.to_utf8().unwrap())
                        .collect::<Vec<_>>();
                    let op = text.iter().take(2).map(String::as_str).collect::<Vec<_>>();
                    let stdout = match op.as_slice() {
                        ["container", "ls"] => s
                            .raw
                            .as_ref()
                            .map(|r| r.required("Id").unwrap().as_str().unwrap())
                            .unwrap_or_default(),
                        ["container", "inspect"] => {
                            json::stringify(s.raw.as_ref().expect("listed identity"), usize::MAX)
                                .unwrap()
                        }
                        ["image", "inspect"] => image().as_str().unwrap(),
                        ["image", "tag"] | ["container", "logs"] => String::new(),
                        ["container", "create"] => {
                            s.creates += 1;
                            let mut labels = IvyJson::object(Vec::<(String, IvyJson)>::new());
                            for pair in text.windows(2).filter(|p| p[0] == "--label") {
                                let (key, value) = pair[1].split_once('=').unwrap();
                                labels.insert(key, value.into());
                            }
                            let name =
                                text.windows(2).find(|p| p[0] == "--name").unwrap()[1].clone();
                            s.raw = Some(IvyJson::object([
                                ("Id", format!("{:064x}", s.creates).into()),
                                ("Name", format!("/{name}").into()),
                                ("Image", image()),
                                ("Config", IvyJson::object([("Labels", labels)])),
                                ("HostConfig", v(r#"{"CgroupParent":""}"#)),
                                (
                                    "State",
                                    v(
                                        r#"{"Status":"created","ExitCode":0,"StartedAt":"0001-01-01T00:00:00Z"}"#,
                                    ),
                                ),
                            ]));
                            String::new()
                        }
                        ["container", "start"] => {
                            s.raw.as_mut().unwrap().insert("State", v(r#"{"Status":"running","ExitCode":0,"StartedAt":"2026-10-19T09:00:00Z"}"#));
                            String::new()
                        }
                        ["container", "stop"] => {
                            nested(s.raw.as_mut().unwrap(), "State", "Status", "exited".into());
                            String::new()
                        }
                        ["container", "rm"] => {
                            s.raw = None;
                            String::new()
                        }
                        _ => panic!("Unexpected modeled Docker command {text:?}"),
                    };
                    Ok(output(stdout))
                })
            },
        );
        DockerAdapter::with_command(
            Arc::new(NativeDockerHost::new(f.root.0.clone()).unwrap()),
            command,
        )
    }
    fn calls(&self, operation: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|a| a.get(1) == Some(&JsString::from(operation)))
            .count()
    }
    fn seed(&self, f: &Fixture, target: &IvyJson) {
        self.0.lock().unwrap().raw = Some(IvyJson::object([
            ("Id", "c".repeat(64).into()),
            (
                "Name",
                format!(
                    "/{}",
                    ivy_host_runtime::docker::container_name(
                        f.host
                            .required("hostId")
                            .unwrap()
                            .as_js_string()
                            .unwrap()
                            .clone(),
                        "fixture".into(),
                        None
                    )
                    .unwrap()
                )
                .into(),
            ),
            ("Image", image()),
            (
                "Config",
                IvyJson::object([(
                    "Labels",
                    IvyJson::object([
                        ("dev.ivy.host", f.host.required("hostId").unwrap().clone()),
                        ("dev.ivy.instance", "fixture".into()),
                        ("dev.ivy.candidate", hash()),
                        (
                            "dev.ivy.launch",
                            target.required("revision").unwrap().clone(),
                        ),
                    ]),
                )]),
            ),
            ("HostConfig", v(r#"{"CgroupParent":""}"#)),
            (
                "State",
                v(r#"{"Status":"running","ExitCode":0,"StartedAt":"2026-10-19T09:00:00Z"}"#),
            ),
        ]));
    }
}
fn runtime(f: &Fixture, docker: &Docker) -> NativeActivationRuntime {
    NativeActivationRuntime::with_adapters(
        f.root.0.clone(),
        preparation(),
        PathBuf::from(env!("CARGO_BIN_EXE_ivy-job")),
        f.inspector.clone(),
        if cfg!(windows) { "win32" } else { "linux" }.into(),
        docker.adapter(f),
        ivy_host_runtime::systemd::SystemdAdapter::new(f.root.0.clone()),
    )
}
async fn fixture(endpoint: &str) -> Fixture {
    Fixture::new("hive", |host, candidate, manifest| {
        hive(host, manifest, Some(endpoint));
        candidate.insert("dockerImage", image());
        let mut instance = host.required("instances").unwrap().as_array().unwrap()[0].clone();
        instance.insert("engine", "docker".into());
        let port = url::Url::parse(endpoint).unwrap().port().unwrap();
        let settings = instance.required("settings").unwrap().clone();
        let mut settings = settings;
        nested(
            &mut settings,
            "backup",
            "directory",
            path(&native(host.required("runtimeRoot").unwrap()).join("backups")),
        );
        instance.insert("settings", settings);
        instance.insert(
            "docker",
            IvyJson::object([
                ("imageRepository", "fixture/hive".into()),
                (
                    "ports",
                    IvyJson::Array(vec![IvyJson::object([
                        ("host", "127.0.0.1".into()),
                        ("port", IvyJson::Number(port as f64)),
                        ("containerPort", IvyJson::Number(port as f64)),
                    ])]),
                ),
            ]),
        );
        host.insert("instances", IvyJson::Array(vec![instance]));
    })
    .await
}
fn rpc() -> Rpc {
    Rpc::new(
        IvyJson::object([("ready", true.into()), ("buildId", hash())]),
        v(r#"{"schemaVersion":1,"exists":false,"format":0,"contracts":[]}"#),
    )
}

#[tokio::test]
async fn native_docker_passive_observation_never_creates_starts_or_replaces() {
    for case in ["missing", "different-launch", "exact"] {
        let rpc = rpc();
        let f = fixture(&rpc.endpoint).await;
        let docker = Docker::default();
        let runtime = runtime(&f, &docker);
        let target = materialize(&f, true).await;
        write_health(&f, &target);
        if case != "missing" {
            docker.seed(&f, &target);
        }
        if case == "different-launch" {
            let mut s = docker.0.lock().unwrap();
            let mut config = s.raw.as_ref().unwrap().required("Config").unwrap().clone();
            nested(
                &mut config,
                "Labels",
                "dev.ivy.launch",
                "other-launch".into(),
            );
            s.raw.as_mut().unwrap().insert("Config", config);
        }
        runtime
            .reconcile(&f.context, Some(target), false)
            .await
            .unwrap();
        let observed = observation(&f).await;
        assert_eq!(
            observed.required("state").unwrap(),
            &match case {
                "missing" => "failed",
                "different-launch" => "unknown",
                _ => "ready",
            }
            .into(),
            "{case}"
        );
        for op in ["create", "start", "stop", "rm"] {
            assert_eq!(docker.calls(op), 0, "{case}:{op}");
        }
        assert!(!readiness_trace(&f).exists());
        runtime.dispose().await.unwrap();
        f.close().await;
    }
}

#[tokio::test]
async fn native_docker_explicit_replacement_retires_before_new_launch_and_stopped_target_fence() {
    let rpc = rpc();
    let f = fixture(&rpc.endpoint).await;
    prepare_readiness(&f);
    let docker = Docker::default();
    let runtime = runtime(&f, &docker);
    let first = materialize(&f, true).await;
    write_health(&f, &first);
    runtime
        .reconcile(&f.context, Some(first.clone()), true)
        .await
        .unwrap();
    assert_eq!(
        observation(&f).await.required("state").unwrap(),
        &"ready".into()
    );
    runtime
        .reconcile(&f.context, Some(first), false)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(readiness_trace(&f))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let next = materialize(&f, true).await;
    write_health(&f, &next);
    runtime
        .reconcile(&f.context, Some(next.clone()), false)
        .await
        .unwrap();
    assert_eq!(
        observation(&f).await.required("state").unwrap(),
        &"unknown".into()
    );
    assert_eq!(docker.calls("stop"), 0);
    assert_eq!(docker.calls("create"), 1);
    runtime
        .reconcile(&f.context, Some(next.clone()), true)
        .await
        .unwrap();
    assert_eq!(
        observation(&f).await.required("state").unwrap(),
        &"ready".into()
    );
    assert_eq!(docker.calls("stop"), 1);
    assert_eq!(docker.calls("create"), 2);
    assert_eq!(docker.calls("start"), 2);
    assert_eq!(
        fs::read_to_string(readiness_trace(&f))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let stopped = ivy_host_runtime::target::stopped_target(&next);
    runtime
        .reconcile(&f.context, Some(stopped.clone()), false)
        .await
        .unwrap();
    assert_eq!(
        observation(&f).await.required("targetRevision").unwrap(),
        stopped.required("revision").unwrap()
    );
    assert_eq!(
        observation(&f).await.required("state").unwrap(),
        &"stopped".into()
    );
    assert_eq!(docker.calls("stop"), 2);
    let before = docker.0.lock().unwrap().trace.len();
    runtime.dispose().await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(docker.calls("stop"), 2);
    assert!(docker.0.lock().unwrap().trace.len() >= before);
    f.close().await;
}

#[tokio::test]
async fn native_docker_hive_readiness_keeps_bounded_cold_start_and_stops_after_expiry() {
    let rpc = rpc();
    let mut f = fixture(&rpc.endpoint).await;
    let clock = Arc::new(RuntimeClock(std::sync::atomic::AtomicI64::new(
        f.context.clock.utc_millis(),
    )));
    f.context.clock = clock.clone();
    let docker = Docker::default();
    let runtime = runtime(&f, &docker);
    let target = materialize(&f, true).await;
    runtime
        .reconcile(&f.context, Some(target.clone()), true)
        .await
        .unwrap();
    assert_eq!(
        observation(&f).await.required("state").unwrap(),
        &"starting".into()
    );
    clock.advance(1001);
    runtime
        .reconcile(&f.context, Some(target.clone()), false)
        .await
        .unwrap();
    assert_eq!(docker.calls("stop"), 0);
    clock.advance(179_000);
    runtime
        .reconcile(&f.context, Some(target), false)
        .await
        .unwrap();
    assert_eq!(
        observation(&f).await.required("code").unwrap(),
        &"readiness_deadline".into()
    );
    assert_eq!(docker.calls("stop"), 1);
    assert_eq!(docker.calls("create"), 1);
    runtime.dispose().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn native_docker_changed_identity_completion_remains_unknown_without_replacement() {
    let rpc = rpc();
    let f = fixture(&rpc.endpoint).await;
    let docker = Docker::default();
    let runtime = runtime(&f, &docker);
    let target = materialize(&f, true).await;
    write_health(&f, &target);
    runtime
        .reconcile(&f.context, Some(target.clone()), true)
        .await
        .unwrap();
    docker
        .0
        .lock()
        .unwrap()
        .raw
        .as_mut()
        .unwrap()
        .insert("Image", format!("sha256:{}", "d".repeat(64)).into());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        if let Err(error) = runtime
            .reconcile(&f.context, Some(target.clone()), false)
            .await
        {
            assert_eq!(error.code, "outcome_unknown");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        observation(&f).await.required("state").unwrap(),
        &"unknown".into()
    );
    assert_eq!(docker.calls("create"), 1);
    assert_eq!(docker.calls("start"), 1);
    assert_eq!(docker.calls("stop"), 0);
    runtime.dispose().await.unwrap();
    f.close().await;
}
