use futures_util::future::BoxFuture;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash, json};
use ivy_host_runtime::{
    bootstrap,
    linux_host::{FileMetadata, LinuxHost, NativeLinuxHost},
    linux_resources as resources, linux_service_identity as identity, node_path, systemd,
};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Notify;
#[path = "host-systemd-cases.rs"]
mod systemd_cases;
static CORPUS: Mutex<()> = Mutex::new(());
fn v(text: &str) -> IvyJson {
    json::parse(text, usize::MAX).unwrap()
}
fn empty() -> IvyJson {
    IvyJson::Object(vec![])
}
fn s(value: &IvyJson) -> JsString {
    value.as_js_string().unwrap().clone()
}
fn utf(value: &IvyJson) -> String {
    value.as_str().unwrap()
}
fn wire(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-linux-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn input(fixture: &Fixture, module: &str, operation: &str, args: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([
        (
            "fixtureRoot",
            IvyJson::String(node_path::from_native(&fixture.0)),
        ),
        ("module", module.into()),
        ("operation", operation.into()),
        ("args", IvyJson::Array(args)),
    ])
}
fn oracle(input: &IvyJson) -> IvyJson {
    let root = std::env::var_os("IVY_HOST_SOURCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-linux-platform-oracle.mjs",
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
fn append(case: &str, actual: &IvyJson, expected: &IvyJson) {
    if let Some(path) = std::env::var_os("IVY_HOST_LINUX_PLATFORM_CORPUS") {
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
                    ("case", case.into()),
                    ("actual", actual.clone()),
                    ("expected", expected.clone())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
}
fn compare(case: &str, input: &IvyJson, actual: IvyJson) {
    let expected = oracle(input);
    append(case, &actual, &expected);
    assert_eq!(
        json::canonical(&actual, usize::MAX).unwrap(),
        json::canonical(&expected, usize::MAX).unwrap(),
        "{case}"
    );
}
fn fail(code: &str) -> io::Error {
    io::Error::new(
        match code {
            "ENOENT" => io::ErrorKind::NotFound,
            "EEXIST" => io::ErrorKind::AlreadyExists,
            "EACCES" => io::ErrorKind::PermissionDenied,
            _ => io::ErrorKind::Other,
        },
        "Explicit simulated boundary failure",
    )
}
struct State {
    model: IvyJson,
    files: IvyJson,
    trace: Vec<IvyJson>,
    counts: BTreeMap<String, usize>,
    command_index: usize,
}
impl State {
    fn event(&mut self, event: IvyJson) -> io::Result<()> {
        let op = utf(event.required("op").unwrap());
        self.trace.push(event.clone());
        let count = self.counts.entry(op.clone()).or_default();
        *count += 1;
        let count = *count;
        if let Some(fault) = self
            .model
            .get("faults")
            .and_then(IvyJson::as_array)
            .and_then(|faults| {
                faults.iter().find(|fault| {
                    fault.get("op") == event.get("op")
                        && fault.get("at").and_then(IvyJson::as_f64).unwrap_or(1.0) == count as f64
                        && fault
                            .get("path")
                            .is_none_or(|path| Some(path) == event.get("path"))
                })
            })
        {
            return Err(fail(
                fault
                    .get("code")
                    .and_then(|value| value.as_str().ok())
                    .as_deref()
                    .unwrap_or("EIO"),
            ));
        }
        Ok(())
    }
}
#[derive(Clone)]
struct SimHost {
    state: Arc<Mutex<State>>,
    platform: String,
    uid: Option<u32>,
    pause: Arc<AtomicBool>,
    opened: Arc<Notify>,
    release: Arc<Notify>,
}
impl SimHost {
    fn new(model: IvyJson) -> Self {
        Self {
            platform: model
                .get("platform")
                .and_then(|value| value.as_str().ok())
                .unwrap_or_else(|| "linux".into()),
            uid: Some(model.get("uid").and_then(IvyJson::as_f64).unwrap_or(0.0) as u32),
            state: Arc::new(Mutex::new(State {
                files: model.get("files").cloned().unwrap_or_else(empty),
                model,
                trace: vec![],
                counts: BTreeMap::new(),
                command_index: 0,
            })),
            pause: Arc::new(AtomicBool::new(false)),
            opened: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }
    fn output(&self, result: IvyResult<IvyJson>) -> IvyJson {
        let state = self.state.lock().unwrap();
        IvyJson::object([
            ("operation", wire(result)),
            ("trace", IvyJson::Array(state.trace.clone())),
            ("files", state.files.clone()),
        ])
    }
}
fn path_event(op: &str, path: &JsString) -> IvyJson {
    IvyJson::object([("op", op.into()), ("path", IvyJson::String(path.clone()))])
}
fn file_bytes(file: &IvyJson) -> Vec<u8> {
    file.get("data")
        .and_then(IvyJson::as_js_string)
        .map(|value| String::from_utf16_lossy(&value.0).into_bytes())
        .unwrap_or_default()
}
impl LinuxHost for SimHost {
    fn platform(&self) -> &str {
        &self.platform
    }
    fn user_id(&self) -> Option<u32> {
        self.uid
    }
    fn metadata(&self, path: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
        let this = self.clone();
        Box::pin(async move {
            let mut state = this.state.lock().unwrap();
            state.event(path_event("stat", &path))?;
            let file = state.files.get_key(&path).ok_or_else(|| fail("ENOENT"))?;
            if let Some(error) = file.get("error") {
                return Err(fail(&utf(error)));
            }
            Ok(FileMetadata {
                is_file: file.get("file") != Some(&false.into()),
                is_symlink: file.get("symlink") == Some(&true.into()),
                uid: Some(file.get("uid").and_then(IvyJson::as_f64).unwrap_or(0.0) as u32),
                gid: Some(file.get("gid").and_then(IvyJson::as_f64).unwrap_or(0.0) as u32),
                mode: file.get("mode").and_then(IvyJson::as_f64).unwrap_or(420.0) as u32,
                size: file
                    .get("size")
                    .and_then(IvyJson::as_f64)
                    .map_or_else(|| file_bytes(file).len() as u64, |size| size as u64),
            })
        })
    }
    fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
        let this = self.clone();
        let result = (|| {
            let mut state = this.state.lock().unwrap();
            state.event(path_event("read", &path))?;
            let file = state.files.get_key(&path).ok_or_else(|| fail("ENOENT"))?;
            if let Some(error) = file.get("readError") {
                return Err(fail(&utf(error)));
            }
            Ok(file_bytes(file))
        })();
        Box::pin(std::future::ready(result))
    }
    fn create(
        &self,
        path: JsString,
        bytes: Vec<u8>,
        mode: u32,
        parent: JsString,
    ) -> BoxFuture<'static, io::Result<()>> {
        let this = self.clone();
        Box::pin(async move {
            {
                let mut state = this.state.lock().unwrap();
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
            }
            this.opened.notify_one();
            if this.pause.load(Ordering::SeqCst) {
                this.release.notified().await;
            }
            let mut state = this.state.lock().unwrap();
            let result = (|| -> io::Result<()> {
                let mut event = path_event("write", &path);
                event.insert("data", String::from_utf8_lossy(&bytes).to_string().into());
                state.event(event)?;
                let mut file = state.files.get_key(&path).unwrap().clone();
                file.insert("data", String::from_utf8_lossy(&bytes).to_string().into());
                file.insert("size", IvyJson::Number(bytes.len() as f64));
                state.files.insert(path.clone(), file);
                state.event(path_event("sync", &path))
            })();
            state.event(path_event("close", &path))?;
            result?;
            state.event(IvyJson::object([
                ("op", "open".into()),
                ("path", IvyJson::String(parent.clone())),
                ("flags", "r".into()),
                ("mode", IvyJson::Null),
            ]))?;
            let result = state.event(path_event("sync", &parent));
            state.event(path_event("close", &parent))?;
            result
        })
    }
    fn command(
        &self,
        file: &'static str,
        args: Vec<JsString>,
        timeout_ms: u32,
        maximum_buffer_bytes: usize,
    ) -> BoxFuture<'static, IvyResult<JsString>> {
        let this = self.clone();
        Box::pin(async move {
            let mut state = this.state.lock().unwrap();
            state
                .event(IvyJson::object([
                    ("op", "command".into()),
                    ("file", file.into()),
                    (
                        "args",
                        IvyJson::Array(args.into_iter().map(IvyJson::String).collect()),
                    ),
                    ("timeoutMs", IvyJson::Number(timeout_ms as f64)),
                    ("maxBuffer", IvyJson::Number(maximum_buffer_bytes as f64)),
                ]))
                .map_err(|_| IvyError::internal())?;
            let result = state
                .model
                .get("commands")
                .and_then(IvyJson::as_array)
                .and_then(|rows| rows.get(state.command_index))
                .cloned()
                .unwrap_or_else(empty);
            state.command_index += 1;
            if result.get("error").is_some() {
                return Err(IvyError::internal());
            }
            Ok(result.get("stdout").map(s).unwrap_or_default())
        })
    }
}
async fn execute(input: &IvyJson) -> IvyJson {
    let host = Arc::new(SimHost::new(
        input.get("model").cloned().unwrap_or_else(empty),
    ));
    let args = input.required("args").unwrap().as_array().unwrap();
    let module = utf(input.required("module").unwrap());
    let op = utf(input.required("operation").unwrap());
    let result: IvyResult<IvyJson> = async {
        Ok(match (module.as_str(), op.as_str()) {
            ("resources", "installationIdentity") => {
                resources::installation_identity(&args[0])?.into()
            }
            ("resources", "linuxResourceScope") => {
                resources::linux_resource_scope(&utf(&args[0]), &args[1])?
            }
            ("resources", "validateLinuxResourceScope") => {
                resources::validate_linux_resource_scope(
                    &args[0],
                    args.get(1).map(utf).as_deref(),
                )?;
                IvyJson::Null
            }
            ("resources", "linuxSlice") => resources::linux_slice(&args[0])?.into(),
            ("resources", "assertLinuxResourceMembership") => {
                resources::assert_linux_resource_membership(
                    &args[0],
                    &s(&args[1]),
                    &s(&args[2]),
                    &args[3],
                )?;
                IvyJson::Null
            }
            ("resources", "ensureLinuxSlice") => {
                resources::ensure_linux_slice_with_host(args[0].clone(), host.clone()).await?;
                IvyJson::Null
            }
            ("resources", "inspectLinuxSlice") => {
                resources::inspect_linux_slice_with_host(args[0].clone(), host.clone()).await?;
                IvyJson::Null
            }
            ("resources", "verifyLinuxResourceMembership") => {
                resources::verify_linux_resource_membership_with_host(
                    args[0].clone(),
                    s(&args[1]),
                    args[2].clone(),
                    host.clone(),
                )
                .await?;
                IvyJson::Null
            }
            ("identity", "linuxProcessUser") => {
                identity::linux_process_user(&s(&args[0]), &utf(&args[1]))
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null)
            }
            ("identity", "ensureLinuxProcessIdentity") => {
                identity::ensure_linux_process_identity_with_host(
                    args[0].clone(),
                    args[1].clone(),
                    host.clone(),
                )
                .await?;
                IvyJson::Null
            }
            ("bootstrap", "configurationBootstrapPlan") => bootstrap::configuration_bootstrap_plan(
                &args[0],
                &s(&args[1]),
                &args[2],
                args.get(3).filter(|value| !value.is_null()),
                args.get(4).and_then(IvyJson::as_array).unwrap_or(&[]),
            )?,
            ("bootstrap", "linuxUnit") => IvyJson::String(bootstrap::linux_unit(
                &args[0],
                &args[1],
                args.get(2).map(s),
            )?),
            ("bootstrap", "replaceableSystemdOwner") => bootstrap::replaceable_systemd_owner(
                &s(&args[0]),
                args.get(1) == Some(&true.into()),
            )
            .into(),
            ("systemd", "parseSystemdState") => systemd::parse_systemd_state(&s(&args[0])).json(),
            ("systemd", "systemdUnitName") => {
                systemd::systemd_unit_name(&args[0], &s(&args[1]))?.into()
            }
            ("systemd", "renderSystemdUnit") => {
                let value = systemd::render_systemd_unit(
                    &args[0],
                    &s(&args[1]),
                    &args[2],
                    &args[3],
                    &args[4],
                    args.get(5),
                )?;
                IvyJson::object([
                    ("unit", value.unit.into()),
                    ("content", IvyJson::String(value.content)),
                ])
            }
            _ => panic!("Unknown test operation"),
        })
    }
    .await;
    host.output(result)
}
async fn check(case: &str, input: IvyJson) -> IvyJson {
    let actual = execute(&input).await;
    compare(case, &input, actual.clone());
    actual
}
fn budget() -> IvyJson {
    v(r#"{"memoryHighBytes":201326592,"memoryMaxBytes":268435456,"tasksMax":64}"#)
}
fn config() -> IvyJson {
    v(
        r#"{"schemaVersion":1,"hostId":"linux-platform-fixture","runtimeRoot":"/fixture/state","artifactRoot":"/fixture/releases","stagingRoot":"/fixture/staging","publicBaseUrl":"http://127.0.0.1:39081/ivy","executables":{"node":"/usr/bin/node"},"instances":[{"instanceId":"renamed-executor","componentId":"host-executor","serviceNodeId":"executor","engine":"process","enabled":true,"settings":{}},{"instanceId":"renamed-manager","componentId":"service-manager","serviceNodeId":"manager","engine":"process","enabled":true,"settings":{}},{"instanceId":"agent","componentId":"agent-manager","serviceNodeId":"agent-node","engine":"process","enabled":true,"settings":{}}]}"#,
    )
}
fn candidate(component: &str, os: &str) -> IvyJson {
    let hash = hash::digest(component.as_bytes());
    IvyJson::object([
        ("candidateId", hash.clone().into()),
        ("componentId", component.into()),
        ("buildId", hash.into()),
        ("artifactRoot", "/fixture/artifact % $ ' \" \\name".into()),
        ("manifestPath", "/fixture/artifact/component.json".into()),
        ("createdAt", "2026-01-01T00:00:00.000Z".into()),
        (
            "platform",
            IvyJson::object([
                ("os", os.into()),
                ("arch", "x64".into()),
                ("node", "24.18.0".into()),
            ]),
        ),
    ])
}
fn manifest(component: &str) -> IvyJson {
    IvyJson::object([
        ("componentId", component.into()),
        (
            "entrypoint",
            v(
                r#"{"executable":"node","args":["dist/service/main.js","--literal","% $ \" \\ 😀"],"timeoutMs":30000}"#,
            ),
        ),
        (
            "restart",
            v(r#"{"policy":"always","minimumDelayMs":101,"maximumDelayMs":5000}"#),
        ),
    ])
}
fn plan() -> IvyJson {
    v(
        r#"{"schemaVersion":1,"os":"linux","hostId":"fixture","installationId":"ivy-next-111111111111","candidateId":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","artifactRoot":"/fixture/space % literal $directory","configPath":"/fixture/central config.json","configHash":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","runtimeRoot":"/fixture/state","nodeExecutable":"/usr/bin/node","processes":[{"instanceId":"renamed-executor","componentId":"host-executor","name":"ivy-next-111111111111-222222222222"}]}"#,
    )
}
fn processes(plan: &IvyJson) -> IvyJson {
    plan.required("processes").unwrap().as_array().unwrap()[0].clone()
}
fn scope() -> IvyJson {
    resources::linux_resource_scope("ivy-next-111111111111", &budget()).unwrap()
}
fn model() -> IvyJson {
    v(r#"{"platform":"linux","uid":0,"files":{}}"#)
}
fn slice_path(scope: &IvyJson) -> String {
    format!(
        "/etc/systemd/system/{}",
        utf(scope.required("slice").unwrap())
    )
}
fn slice_file(scope: &IvyJson) -> IvyJson {
    IvyJson::object([
        ("data", resources::linux_slice(scope).unwrap().into()),
        ("mode", IvyJson::Number(420.0)),
        ("uid", IvyJson::Number(0.0)),
    ])
}
#[tokio::test]
async fn immutable_linux_resource_identity_and_scope_match_the_original() {
    let fixture = Fixture::new();
    for mode in ["ordinary", "restored", "other-root", "raw"] {
        let mut config = config();
        if mode == "restored" || mode == "other-root" {
            config.insert("restoredFrom", v(r#"{"backupId":"backup-identity"}"#));
        }
        if mode == "other-root" {
            config.insert("runtimeRoot", "/fixture/restored".into());
        }
        if mode == "raw" {
            config.insert("hostId", IvyJson::String(JsString(vec![97, 0xd800])));
        }
        check(
            mode,
            input(&fixture, "resources", "installationIdentity", vec![config]),
        )
        .await;
    }
    for (key, value) in [
        ("memoryHighBytes", 0.0),
        ("memoryHighBytes", 268435456.0),
        ("memoryHighBytes", 268435457.0),
        ("tasksMax", 15.0),
        ("tasksMax", 65.0),
    ] {
        let mut value_budget = budget();
        value_budget.insert(key, IvyJson::Number(value));
        check(
            "budget boundaries",
            input(
                &fixture,
                "resources",
                "linuxResourceScope",
                vec!["ivy-next-111111111111".into(), value_budget],
            ),
        )
        .await;
    }
    for id in [
        "foreign",
        "ivy-next-111111111111\n",
        "ivy-next-ABCDEF012345",
        "ivy-next-111111111111",
    ] {
        check(
            "owned installation identity",
            input(
                &fixture,
                "resources",
                "linuxResourceScope",
                vec![id.into(), budget()],
            ),
        )
        .await;
    }
    for mode in [
        "valid",
        "reordered",
        "foreign",
        "changed-budget",
        "system",
        "newline",
    ] {
        let mut value = scope();
        match mode {
            "foreign" => {}
            "changed-budget" => value.insert(
                "budget",
                v(r#"{"memoryHighBytes":201326592,"memoryMaxBytes":268435456,"tasksMax":65}"#),
            ),
            "system" => value.insert("slice", "system.slice".into()),
            "newline" => value.insert(
                "slice",
                format!("{}\n", utf(value.required("slice").unwrap())).into(),
            ),
            "reordered" => {
                value =
                    json::parse(&json::canonical(&value, usize::MAX).unwrap(), usize::MAX).unwrap()
            }
            _ => {}
        }
        let mut args = vec![value.clone()];
        if mode == "foreign" {
            args.push("ivy-next-000000000000".into());
        }
        check(
            mode,
            input(&fixture, "resources", "validateLinuxResourceScope", args),
        )
        .await;
        check(
            mode,
            input(&fixture, "resources", "linuxSlice", vec![value]),
        )
        .await;
    }
}
#[tokio::test]
async fn cgroup_membership_and_effective_limits_are_exact_and_reads_are_joined_before_proc() {
    let fixture = Fixture::new();
    let scope = scope();
    let slice = utf(scope.required("slice").unwrap());
    let unit = "ivy-next-111111111111-222222222222.service";
    let controls = v(
        r#"{"memory.high":"201326592","memory.max":"268435456","memory.swap.max":"0","pids.max":"64"}"#,
    );
    for membership in [
        format!("0::/{slice}/{unit}\n"),
        format!("\u{feff}0::/{slice}/{unit}\u{a0}"),
        format!("0::/system.slice/{unit}"),
        format!("0::/{slice}/neighbor.service"),
        format!("0::/{slice}/{unit}\n1:memory:/legacy"),
    ] {
        check(
            "membership boundaries",
            input(
                &fixture,
                "resources",
                "assertLinuxResourceMembership",
                vec![
                    scope.clone(),
                    unit.into(),
                    membership.into(),
                    controls.clone(),
                ],
            ),
        )
        .await;
    }
    for key in ["memory.high", "memory.max", "memory.swap.max", "pids.max"] {
        for value in ["max", "", "1"] {
            let mut changed = controls.clone();
            changed.insert(key, value.into());
            check(
                "effective controller mismatch",
                input(
                    &fixture,
                    "resources",
                    "assertLinuxResourceMembership",
                    vec![
                        scope.clone(),
                        unit.into(),
                        format!("0::/{slice}/{unit}").into(),
                        changed,
                    ],
                ),
            )
            .await;
        }
    }
    for mode in ["valid", "missing", "changed", "foreign-scope", "platform"] {
        let config = config();
        let own_scope = resources::linux_resource_scope(
            &resources::installation_identity(&config).unwrap(),
            &budget(),
        )
        .unwrap();
        let local_slice = utf(own_scope.required("slice").unwrap());
        let local_unit = systemd::systemd_unit_name(&config, &"renamed-executor".into()).unwrap();
        let mut backend = model();
        let mut files = empty();
        for (key, value) in controls.as_object().unwrap() {
            if mode == "missing" && key == &JsString::from("memory.high") {
                continue;
            }
            files.insert(
                format!("/sys/fs/cgroup/{local_slice}/{}", key.to_utf8().unwrap()),
                IvyJson::object([("data", value.clone())]),
            );
        }
        files.insert(
            "/proc/self/cgroup",
            IvyJson::object([(
                "data",
                format!(
                    "0::/{local_slice}/{}",
                    if mode == "changed" {
                        "neighbor.service"
                    } else {
                        &local_unit
                    }
                )
                .into(),
            )]),
        );
        backend.insert("files", files);
        if mode == "platform" {
            backend.insert("platform", "win32".into());
        }
        let mut request = input(
            &fixture,
            "resources",
            "verifyLinuxResourceMembership",
            vec![
                config,
                "renamed-executor".into(),
                if mode == "foreign-scope" {
                    scope.clone()
                } else {
                    own_scope
                },
            ],
        );
        request.insert("model", backend);
        check(mode, request).await;
    }
}
#[tokio::test]
async fn bootstrap_selection_and_direct_units_preserve_raw_argv_and_retained_plan_order() {
    let fixture = Fixture::new();
    for os in ["linux", "win32"] {
        for supplied in [false, true] {
            let config = config();
            let candidate = candidate("host-executor", os);
            let components = IvyJson::Array(
                ["host-executor", "service-manager"]
                    .into_iter()
                    .map(|component| {
                        IvyJson::object([
                            ("candidate", candidate_for(component, os)),
                            ("manifest", manifest(component)),
                        ])
                    })
                    .collect(),
            );
            let args = vec![
                config,
                IvyJson::String(node_path::from_native(&fixture.0.join("host.json"))),
                candidate,
                if supplied { budget() } else { IvyJson::Null },
                components,
            ];
            check(
                "bootstrap outer-owner selection",
                input(&fixture, "bootstrap", "configurationBootstrapPlan", args),
            )
            .await;
        }
    }
    for mode in [
        "fallback-executor",
        "fallback-service",
        "explicit",
        "never",
        "resources",
        "reordered",
        "control",
        "directory-space",
        "directory-control",
        "directory-backslash",
        "raw",
        "own-config",
    ] {
        let mut plan = plan();
        let mut process = processes(&plan);
        match mode {
            "fallback-service" => {
                process.insert("componentId", "task-board".into());
            }
            "explicit" => {
                process.insert(
                    "entrypoint",
                    manifest("host-executor")
                        .required("entrypoint")
                        .unwrap()
                        .clone(),
                );
                process.insert(
                    "restart",
                    v(r#"{"policy":"on-failure","minimumDelayMs":1001}"#),
                );
            }
            "never" => {
                process.insert("restart", v(r#"{"policy":"never","minimumDelayMs":0}"#));
            }
            "resources" | "reordered" => {
                plan.insert("linuxResources", scope());
                if mode == "reordered" {
                    plan = json::parse(&json::canonical(&plan, usize::MAX).unwrap(), usize::MAX)
                        .unwrap();
                }
            }
            "control" => {
                process.insert(
                    "entrypoint",
                    v(r#"{"executable":"node","args":["line\nbreak"]}"#),
                );
            }
            "directory-space" => {
                plan.insert("artifactRoot", "/fixture/ends ".into());
            }
            "directory-control" => {
                plan.insert("artifactRoot", "/fixture/line\nbreak".into());
            }
            "directory-backslash" => {
                plan.insert("artifactRoot", "/fixture/ends\\".into());
            }
            "raw" => {
                process.insert("instanceId", IvyJson::String(JsString(vec![0xd800])));
                process.insert(
                    "entrypoint",
                    IvyJson::object([
                        ("executable", "node".into()),
                        (
                            "args",
                            IvyJson::Array(vec![IvyJson::String(JsString(vec![
                                97, 0xdfff, 37, 36, 92, 34,
                            ]))]),
                        ),
                    ]),
                );
            }
            "own-config" => {
                process.insert("configPath", "/fixture/private % config.json".into());
            }
            _ => {}
        }
        check(
            mode,
            input(&fixture, "bootstrap", "linuxUnit", vec![plan, process]),
        )
        .await;
    }
    let valid = bootstrap::linux_unit(&plan(), &processes(&plan()), None).unwrap();
    for content in [
        valid,
        "[Unit]\nDescription=IvyNext old\nguardian.js\nEnvironment=OLD=1\n".into(),
        "foreign guardian.js Environment=".into(),
    ] {
        for legacy in [false, true] {
            check(
                "legacy replacement authority",
                input(
                    &fixture,
                    "bootstrap",
                    "replaceableSystemdOwner",
                    vec![IvyJson::String(content.clone()), legacy.into()],
                ),
            )
            .await;
        }
    }
}
fn candidate_for(component: &str, os: &str) -> IvyJson {
    candidate(component, os)
}
#[tokio::test]
async fn systemd_state_and_dynamic_units_retain_exact_launch_identity() {
    let fixture = Fixture::new();
    for pid in [
        "42",
        "0",
        "-1",
        "1.5",
        "9007199254740991",
        "9007199254740992",
        "NaN",
        "Infinity",
        "0x2a",
        "\u{feff}42\u{a0}",
    ] {
        check("systemctl pid coercion",input(&fixture,"systemd","parseSystemdState",vec![format!("LoadState=loaded\r\nActiveState=active\r\nSubState=running\r\nMainPID={pid}\r\n").into()])).await;
    }
    for output in [
        "",
        "garbage",
        "=ignored\nLoadState=loaded\nMainPID=2\nMainPID=3\n",
        "LoadState=first\nLoadState=not-found\nActiveState=inactive\n",
    ] {
        check(
            "systemctl property boundaries",
            input(
                &fixture,
                "systemd",
                "parseSystemdState",
                vec![output.into()],
            ),
        )
        .await;
    }
    let mut config = config();
    let mut instances = config
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    instances[0].insert("componentId", "task-board".into());
    config.insert("instances", IvyJson::Array(instances));
    let candidate = candidate("task-board", "linux");
    let target = IvyJson::object([
        ("instanceId", "renamed-executor".into()),
        ("revision", "11111111-1111-4111-8111-111111111111".into()),
        (
            "candidateId",
            candidate.required("candidateId").unwrap().clone(),
        ),
        ("configPath", "/fixture/private % config.json".into()),
    ]);
    for mode in [
        "valid",
        "resources",
        "mismatched-id",
        "mismatched-candidate",
        "missing-entrypoint",
        "false-entrypoint",
        "null-resources",
        "false-resources",
        "agent",
    ] {
        let mut config = config.clone();
        let mut candidate = candidate.clone();
        let mut target = target.clone();
        let mut manifest = manifest("task-board");
        match mode {
            "mismatched-id" => target.insert("instanceId", "other".into()),
            "mismatched-candidate" => target.insert("candidateId", hash::digest(b"other").into()),
            "missing-entrypoint" => {
                manifest = IvyJson::object([("componentId", "task-board".into())])
            }
            "false-entrypoint" => manifest.insert("entrypoint", false.into()),
            "agent" => {
                let mut instances = config
                    .required("instances")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .to_vec();
                instances[0].insert("componentId", "agent-manager".into());
                config.insert("instances", IvyJson::Array(instances));
                candidate.insert("componentId", "agent-manager".into());
                manifest.insert("componentId", "agent-manager".into());
            }
            _ => {}
        }
        let mut args = vec![
            config.clone(),
            "renamed-executor".into(),
            target,
            candidate,
            manifest,
        ];
        if mode == "null-resources" {
            args.push(IvyJson::Null);
        }
        if mode == "false-resources" {
            args.push(false.into());
        }
        if mode == "resources" {
            args.push(
                resources::linux_resource_scope(
                    &resources::installation_identity(&config).unwrap(),
                    &budget(),
                )
                .unwrap(),
            );
        }
        check(mode, input(&fixture, "systemd", "renderSystemdUnit", args)).await;
    }
}
#[tokio::test]
async fn resource_file_creation_and_loaded_state_preserve_guards_races_and_error_effects() {
    let fixture = Fixture::new();
    let scope = scope();
    let path = slice_path(&scope);
    for mode in [
        "missing",
        "valid",
        "race",
        "race-conflict",
        "foreign",
        "writable",
        "link",
        "large",
        "content",
        "write-failure",
        "file-sync-failure",
        "directory-sync-failure",
        "not-admin",
        "wrong-platform",
    ] {
        let mut backend = model();
        let mut files = empty();
        let mut file = slice_file(&scope);
        match mode {
            "foreign" => file.insert("uid", IvyJson::Number(1.0)),
            "writable" => file.insert("mode", IvyJson::Number(0o666 as f64)),
            "link" => file.insert("symlink", true.into()),
            "large" => file.insert("size", IvyJson::Number(4097.0)),
            "content" | "race-conflict" => file.insert("data", "foreign bytes".into()),
            _ => {}
        }
        if ![
            "missing",
            "race",
            "race-conflict",
            "write-failure",
            "file-sync-failure",
            "directory-sync-failure",
        ]
        .contains(&mode)
        {
            files.insert(path.clone(), file.clone());
        }
        backend.insert("files", files);
        if mode.starts_with("race") {
            backend.insert("createRace", file);
        }
        if mode == "not-admin" {
            backend.insert("uid", IvyJson::Number(1.0));
        }
        if mode == "wrong-platform" {
            backend.insert("platform", "win32".into());
        }
        if mode.ends_with("failure") {
            backend.insert(
                "faults",
                IvyJson::Array(vec![IvyJson::object([
                    (
                        "op",
                        if mode == "write-failure" {
                            "write"
                        } else {
                            "sync"
                        }
                        .into(),
                    ),
                    (
                        "at",
                        IvyJson::Number(if mode == "directory-sync-failure" {
                            2.0
                        } else {
                            1.0
                        }),
                    ),
                    ("code", "EIO".into()),
                ])]),
            );
        }
        let mut request = input(
            &fixture,
            "resources",
            "ensureLinuxSlice",
            vec![scope.clone()],
        );
        request.insert("model", backend);
        check(mode, request).await;
    }
    let slice = utf(scope.required("slice").unwrap());
    let correct = format!(
        "FragmentPath={path}\nDropInPaths=\nTransient=no\nLoadState=loaded\nMemoryHigh=201326592\nMemoryMax=268435456\nMemorySwapMax=0\nTasksMax=64\n"
    );
    for mode in [
        "valid",
        "override",
        "changed-limit",
        "duplicate",
        "command-failure",
    ] {
        let mut backend = model();
        backend.insert(
            "files",
            IvyJson::object([(path.clone(), slice_file(&scope))]),
        );
        let output = match mode {
            "override" => correct.replace("DropInPaths=\n", "DropInPaths=/foreign.conf\n"),
            "changed-limit" => correct.replace("TasksMax=64", "TasksMax=max"),
            "duplicate" => format!("MemoryMax=wrong\n{correct}"),
            _ => correct.clone(),
        };
        backend.insert(
            "commands",
            IvyJson::Array(vec![if mode == "command-failure" {
                v(r#"{"error":"EIO"}"#)
            } else {
                IvyJson::object([("stdout", output.into())])
            }]),
        );
        let mut request = input(
            &fixture,
            "resources",
            "inspectLinuxSlice",
            vec![scope.clone()],
        );
        request.insert("model", backend);
        check(&format!("loaded slice {slice}: {mode}"), request).await;
    }
}
fn identity_fixture() -> (IvyJson, IvyJson, IvyJson) {
    let identity = v(
        r#"{"componentId":"agent-manager","user":"ivy-agent","configPath":"/fixture/private/config.json","artifactRoot":"/fixture/release","nodeExecutable":"/fixture/tools/node"}"#,
    );
    let runtime = v(
        r#"{"dataRoot":"/fixture/data","workRoot":"/fixture/work","logsRoot":"/fixture/logs","settings":{"nativeExecutable":"/fixture/tools/native","projectRoot":"/fixture/projects","internalProjectRoot":"/fixture/internal","projects":[{"path":"/fixture/projects"},{"path":"/fixture/extra"},null,[],{"path":false}]}}"#,
    );
    let mut backend = model();
    let mut files = empty();
    for path in [
        "/fixture/private/config.json",
        "/fixture/tools/node",
        "/fixture/tools/native",
        "/fixture/release",
        "/fixture/data",
        "/fixture/work",
        "/fixture/logs",
        "/fixture/projects",
        "/fixture/internal",
        "/fixture/extra",
    ] {
        files.insert(path, v(r#"{"data":"fixture","uid":1000,"gid":1000}"#));
    }
    backend.insert("files", files);
    backend.insert(
        "commands",
        v(r#"[{"stdout":"2001\n"},{"stdout":"3001\n"}]"#),
    );
    (identity, runtime, backend)
}
#[tokio::test]
async fn linux_identity_grants_match_original_order_deduplication_and_failure_boundaries() {
    let fixture = Fixture::new();
    for component in ["agent-manager", "host-executor"] {
        for platform in ["linux", "win32"] {
            check(
                "separate Linux user selection",
                input(
                    &fixture,
                    "identity",
                    "linuxProcessUser",
                    vec![component.into(), platform.into()],
                ),
            )
            .await;
        }
    }
    for mode in [
        "valid",
        "absent-user",
        "empty-user",
        "other-user",
        "other-component",
        "not-admin",
        "windows",
        "create-user",
        "uid-zero",
        "uid-nan",
        "uid-unsafe",
        "missing-config",
        "existing-owner",
        "duplicate-roots",
        "missing-project",
        "grant-failure",
    ] {
        let (mut identity, mut runtime, mut backend) = identity_fixture();
        match mode {
            "absent-user" => identity.insert("user", IvyJson::Null),
            "empty-user" => identity.insert("user", "".into()),
            "other-user" => identity.insert("user", "root".into()),
            "other-component" => identity.insert("componentId", "host-executor".into()),
            "not-admin" => backend.insert("uid", IvyJson::Number(1.0)),
            "windows" => backend.insert("platform", "win32".into()),
            "create-user" => backend.insert(
                "commands",
                v(r#"[{"error":"ENOENT"},{},{"stdout":"2001\n"},{"stdout":"3001\n"}]"#),
            ),
            "uid-zero" | "uid-nan" | "uid-unsafe" => {
                let uid = match mode {
                    "uid-zero" => "0",
                    "uid-nan" => "not-an-id",
                    _ => "9007199254740992",
                };
                backend.insert(
                    "commands",
                    IvyJson::Array(vec![
                        IvyJson::object([("stdout", uid.into())]),
                        IvyJson::object([("stdout", "3001".into())]),
                    ]),
                );
            }
            "missing-config" | "missing-project" => {
                let skip = if mode == "missing-config" {
                    "/fixture/private/config.json"
                } else {
                    "/fixture/projects"
                };
                backend.insert(
                    "files",
                    IvyJson::Object(
                        backend
                            .required("files")
                            .unwrap()
                            .as_object()
                            .unwrap()
                            .iter()
                            .filter(|(key, _)| key != &JsString::from(skip))
                            .cloned()
                            .collect(),
                    ),
                );
            }
            "existing-owner" => {
                let files = IvyJson::Object(
                    backend
                        .required("files")
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(key, value)| {
                            let mut file = value.clone();
                            file.insert("uid", IvyJson::Number(2001.0));
                            file.insert("gid", IvyJson::Number(3001.0));
                            (key.clone(), file)
                        })
                        .collect(),
                );
                backend.insert("files", files);
            }
            "duplicate-roots" => {
                runtime.insert("workRoot", "/fixture/data".into());
                runtime.insert("logsRoot", "/fixture/data".into());
            }
            "grant-failure" => {
                backend.insert("faults", v(r#"[{"op":"command","at":4,"code":"EACCES"}]"#))
            }
            _ => {}
        }
        let mut request = input(
            &fixture,
            "identity",
            "ensureLinuxProcessIdentity",
            vec![identity, runtime],
        );
        request.insert("model", backend);
        check(mode, request).await;
    }
}
#[tokio::test]
async fn native_metadata_matches_node_for_fixture_files_and_administration_stays_platform_guarded()
{
    let fixture = Fixture::new();
    let file = fixture.0.join("metadata.txt");
    fs::write(&file, b"fixture bytes").unwrap();
    let host = NativeLinuxHost::new(PathBuf::new());
    for readonly in [false, true] {
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(readonly);
        fs::set_permissions(&file, permissions).unwrap();
        let value = host.metadata(node_path::from_native(&file)).await.unwrap();
        let actual = IvyJson::object([
            (
                "operation",
                wire(Ok(IvyJson::object([
                    ("file", value.is_file.into()),
                    ("symlink", value.is_symlink.into()),
                    (
                        "uid",
                        value
                            .uid
                            .map(|value| IvyJson::Number(value as f64))
                            .unwrap_or(IvyJson::Null),
                    ),
                    (
                        "gid",
                        value
                            .gid
                            .map(|value| IvyJson::Number(value as f64))
                            .unwrap_or(IvyJson::Null),
                    ),
                    ("mode", IvyJson::Number(value.mode as f64)),
                    ("size", IvyJson::Number(value.size as f64)),
                ]))),
            ),
            ("trace", IvyJson::Array(vec![])),
            ("files", empty()),
        ]);
        compare(
            "native Node stat boundary",
            &input(
                &fixture,
                "nativeMetadata",
                "metadata",
                vec![IvyJson::String(node_path::from_native(&file))],
            ),
            actual,
        );
        assert_eq!(
            host.read(node_path::from_native(&file)).await.unwrap(),
            b"fixture bytes"
        );
    }
    #[cfg(windows)]
    {
        let result = resources::ensure_linux_slice(scope(), PathBuf::new()).await;
        assert_eq!(result.unwrap_err().code, "unsupported_runtime");
        let (identity, runtime, _) = identity_fixture();
        assert_eq!(
            identity::ensure_linux_process_identity(identity, runtime, PathBuf::new())
                .await
                .unwrap_err()
                .code,
            "unsupported_runtime"
        );
    }
    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(&file, permissions).unwrap();
}
#[tokio::test]
async fn admitted_slice_creation_survives_a_dropped_waiter_through_final_verification() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
        time::Duration,
    };
    let host = Arc::new(SimHost::new(model()));
    host.pause.store(true, Ordering::SeqCst);
    let mut future = Box::pin(resources::ensure_linux_slice_with_host(
        scope(),
        host.clone(),
    ));
    poll_fn(|cx| {
        assert!(matches!(future.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    drop(future);
    tokio::time::timeout(Duration::from_secs(5), host.opened.notified())
        .await
        .unwrap();
    host.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if host
                .state
                .lock()
                .unwrap()
                .trace
                .last()
                .is_some_and(|value| value.get("op") == Some(&"read".into()))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let state = host.state.lock().unwrap();
    assert_eq!(
        state.files.get(&slice_path(&scope())).unwrap().get("data"),
        Some(&resources::linux_slice(&scope()).unwrap().into())
    );
    append(
        "admitted resource owner finishes after waiter drop",
        &IvyJson::object([
            ("trace", IvyJson::Array(state.trace.clone())),
            ("files", state.files.clone()),
        ]),
        &IvyJson::Null,
    );
}
