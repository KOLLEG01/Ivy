use super::*;
use ivy_host_runtime::{
    bootstrap_os::{BootstrapOs, SystemdBootstrapOs},
    node_runtime::NodeRuntime,
    systemd::{SystemdHost, UnitFile},
};

/// Keep observations asynchronous. Completion order among independent reads may
/// differ from Node's microtask queue; command and publication order stay exact.
struct Host(Arc<systemd_cases::Host>);
fn observed<T: Send + 'static>(future: BoxFuture<'static, T>) -> BoxFuture<'static, T> {
    Box::pin(async move {
        let result = future.await;
        tokio::task::yield_now().await;
        result
    })
}
impl SystemdHost for Host {
    fn platform(&self) -> &str {
        SystemdHost::platform(self.0.as_ref())
    }
    fn user_id(&self) -> Option<u32> {
        SystemdHost::user_id(self.0.as_ref())
    }
    fn metadata(&self, path: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
        observed(SystemdHost::metadata(self.0.as_ref(), path))
    }
    fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
        observed(SystemdHost::read(self.0.as_ref(), path))
    }
    fn open(
        &self,
        path: JsString,
        exclusive: bool,
    ) -> BoxFuture<'static, io::Result<Box<dyn UnitFile>>> {
        self.0.open(path, exclusive)
    }
    fn rename(&self, from: JsString, to: JsString) -> BoxFuture<'static, io::Result<()>> {
        self.0.rename(from, to)
    }
    fn unlink(&self, path: JsString) -> BoxFuture<'static, io::Result<()>> {
        self.0.unlink(path)
    }
    fn execute(
        &self,
        file: JsString,
        args: Vec<JsString>,
    ) -> BoxFuture<'static, IvyResult<JsString>> {
        self.0.execute(file, args)
    }
    fn uuid(&self) -> String {
        self.0.uuid()
    }
    fn now_ms(&self) -> f64 {
        self.0.now_ms()
    }
    fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
        self.0.sleep(ms)
    }
}
impl LinuxHost for Host {
    fn platform(&self) -> &str {
        LinuxHost::platform(&self.0.inner)
    }
    fn user_id(&self) -> Option<u32> {
        LinuxHost::user_id(&self.0.inner)
    }
    fn metadata(&self, path: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
        observed(LinuxHost::metadata(&self.0.inner, path))
    }
    fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
        observed(LinuxHost::read(&self.0.inner, path))
    }
    fn create(
        &self,
        path: JsString,
        bytes: Vec<u8>,
        mode: u32,
        parent: JsString,
    ) -> BoxFuture<'static, io::Result<()>> {
        self.0.inner.create(path, bytes, mode, parent)
    }
    fn command(
        &self,
        file: &'static str,
        args: Vec<JsString>,
        timeout: u32,
        max: usize,
    ) -> BoxFuture<'static, IvyResult<JsString>> {
        observed(self.0.inner.command(file, args, timeout, max))
    }
}
impl Host {
    fn new(model: IvyJson) -> Arc<Self> {
        Arc::new(Self(Arc::new(systemd_cases::Host::new(model, None))))
    }
    fn os(self: &Arc<Self>) -> SystemdBootstrapOs {
        SystemdBootstrapOs::with_hosts(
            NodeRuntime {
                executable: PathBuf::from("node"),
                version: "24.18.0".into(),
            },
            self.clone(),
            self.clone(),
        )
    }
    fn output(&self, result: IvyResult<IvyJson>) -> IvyJson {
        self.0.inner.output(result)
    }
}
fn scenario(fixture: &Fixture, op: &str, args: Vec<IvyJson>, model: IvyJson) -> IvyJson {
    let mut request = input(fixture, "systemdBootstrap", op, args);
    request.insert("model", model);
    request
}
async fn run(request: &IvyJson) -> IvyJson {
    let host = Host::new(request.required("model").unwrap().clone());
    let os = host.os();
    let args = request.required("args").unwrap().as_array().unwrap();
    let plans = |index: usize| args[index].as_array().unwrap().to_vec();
    let result: IvyResult<IvyJson> = async {
        Ok(match utf(request.required("operation").unwrap()).as_str() {
            "inspect" => IvyJson::Array(os.inspect(plans(0)).await?),
            "render" => os.render(args[0].clone(), args[1].clone()).await?,
            "check" => {
                os.check(args[0].clone()).await?;
                IvyJson::Null
            }
            "apply" => {
                os.apply(args[0].clone(), args[1].clone(), plans(2)).await?;
                IvyJson::Null
            }
            "prepare" => {
                os.prepare(args[0].clone()).unwrap().await?;
                IvyJson::Null
            }
            "reload" => {
                os.reload().await?;
                IvyJson::Null
            }
            "pause" => {
                os.pause(args[0].clone(), plans(1)).unwrap().await?;
                IvyJson::Null
            }
            "resume" => {
                os.resume(args[0].clone(), plans(1)).unwrap().await?;
                IvyJson::Null
            }
            _ => panic!("Unlisted bootstrap operation"),
        })
    }
    .await;
    host.output(result)
}
fn comparison(input: &IvyJson, output: &IvyJson) -> IvyJson {
    if input.get("parallelObservations") != Some(&true.into()) {
        return output.clone();
    }
    let mut output = output.clone();
    let mut trace = output
        .required("trace")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    let mut start = 0;
    while start < trace.len() {
        let op = trace[start].required("op").unwrap().clone();
        let mut end = start + 1;
        while end < trace.len() && trace[end].get("op") == Some(&op) {
            end += 1;
        }
        if ["stat", "read"]
            .iter()
            .any(|name| op == IvyJson::from(*name))
        {
            let paths: std::collections::BTreeSet<_> = trace[start..end]
                .iter()
                .map(|event| s(event.required("path").unwrap()))
                .collect();
            // Only distinct paths in an adjacent read-only run commute. Never
            // move a read across a stat, a command or any file mutation.
            if paths.len() == end - start {
                trace[start..end].sort_by_key(|event| s(event.required("path").unwrap()));
            }
        }
        start = end;
    }
    output.insert("trace", IvyJson::Array(trace));
    output
}
fn record(case: &str, input: &IvyJson, actual: &IvyJson, expected: &IvyJson) {
    if let Some(path) = std::env::var_os("IVY_HOST_SYSTEMD_BOOTSTRAP_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        let mut record = IvyJson::object([
            ("case", case.into()),
            ("input", input.clone()),
            ("actual", actual.clone()),
            ("expected", expected.clone()),
        ]);
        if input.get("parallelObservations") == Some(&true.into()) {
            record.insert("comparison", "Adjacent observations of distinct files commute; all command, mutation and phase order remains exact.".into());
            record.insert("comparedActual", comparison(input, actual));
            record.insert("comparedExpected", comparison(input, expected));
        }
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            json::stringify(&record, usize::MAX).unwrap()
        )
        .unwrap();
    }
}
async fn check(case: &str, input: IvyJson) -> IvyJson {
    let actual = run(&input).await;
    let expected = oracle(&input);
    record(case, &input, &actual, &expected);
    assert_eq!(
        json::canonical(&comparison(&input, &actual), usize::MAX).unwrap(),
        json::canonical(&comparison(&input, &expected), usize::MAX).unwrap(),
        "{case}"
    );
    actual
}
async fn check_parallel(case: &str, mut input: IvyJson) -> IvyJson {
    let model = input.required("model").unwrap();
    assert!(model.get("mutations").is_none() && model.get("faults").is_none());
    input.insert("parallelObservations", true.into());
    check(case, input).await
}
fn path(process: &IvyJson) -> String {
    format!(
        "/etc/systemd/system/{}.service",
        utf(process.required("name").unwrap())
    )
}
fn definition(plan: &IvyJson, process: &IvyJson) -> IvyJson {
    let content = bootstrap::linux_unit(plan, process, None).unwrap();
    IvyJson::object([
        (
            "hash",
            hash::digest(String::from_utf16_lossy(&content.0).as_bytes()).into(),
        ),
        ("content", IvyJson::String(content)),
    ])
}
fn owner(plan: &IvyJson, process: &IvyJson, enabled: bool, running: bool) -> IvyJson {
    let definition = definition(plan, process);
    let mut owner = process.clone();
    owner.insert(
        "candidateId",
        process
            .get("candidateId")
            .filter(|v| !v.is_null())
            .unwrap_or(plan.required("candidateId").unwrap())
            .clone(),
    );
    owner.insert(
        "definitionHash",
        definition.required("hash").unwrap().clone(),
    );
    owner.insert("definition", definition);
    owner.insert("enabled", enabled.into());
    owner.insert("running", running.into());
    owner
}
fn status(process: &IvyJson, enabled: bool, running: bool) -> IvyJson {
    IvyJson::object([("stdout", format!("ActiveState={}\nSubState={}\nUnitFileState={}\nLoadState=loaded\nFragmentPath={}\nDropInPaths=\nTransient=no\n",
        if running {"active"} else {"inactive"}, if running {"running"} else {"dead"}, if enabled {"enabled"} else {"disabled"}, path(process)).into())])
}
fn backend(plan: &IvyJson) -> IvyJson {
    let processes = plan.required("processes").unwrap().as_array().unwrap();
    IvyJson::object([
        (
            "files",
            IvyJson::object(processes.iter().map(|p| {
                (
                    path(p),
                    IvyJson::object([(
                        "data",
                        definition(plan, p).required("content").unwrap().clone(),
                    )]),
                )
            })),
        ),
        (
            "commands",
            IvyJson::Array(processes.iter().map(|p| status(p, true, false)).collect()),
        ),
    ])
}
fn snapshot(plan: &IvyJson, running: bool) -> IvyJson {
    IvyJson::object([
        ("hostId", plan.required("hostId").unwrap().clone()),
        (
            "installationId",
            plan.required("installationId").unwrap().clone(),
        ),
        (
            "owners",
            IvyJson::Array(
                plan.required("processes")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| owner(plan, p, true, running))
                    .collect(),
            ),
        ),
    ])
}
fn next_plan(old: &IvyJson) -> IvyJson {
    let mut next = old.clone();
    next.insert("candidateId", hash::digest(b"next candidate").into());
    next.insert("artifactRoot", "/fixture/next release".into());
    next
}
fn apply_args(old: &IvyJson, next: &IvyJson) -> Vec<IvyJson> {
    vec![
        owner(old, &processes(old), true, false),
        definition(next, &processes(next)),
        IvyJson::Array(vec![old.clone(), next.clone()]),
    ]
}
fn fault(op: &str, at: usize) -> IvyJson {
    IvyJson::object([
        ("op", op.into()),
        ("at", IvyJson::Number(at as f64)),
        ("code", "EIO".into()),
    ])
}
fn succeeded(output: &IvyJson) -> bool {
    output.required("operation").unwrap().get("ok") == Some(&true.into())
}

#[tokio::test]
async fn systemd_bootstrap_render_check_and_retained_candidate_identity_match_original() {
    let fixture = Fixture::new();
    let original = plan();
    for mode in ["ordinary", "process-entry", "raw-utf16"] {
        let mut next = next_plan(&original);
        if mode != "ordinary" {
            let mut process = processes(&next);
            process.insert(
                "entrypoint",
                v(r#"{"executable":"node","args":["dist/main.js","% $ \\ \" 😀"]}"#),
            );
            if mode == "raw-utf16" {
                process.insert(
                    "configPath",
                    IvyJson::String(JsString(vec![
                        47, 102, 105, 120, 116, 117, 114, 101, 47, 0xd800,
                    ])),
                );
            }
            next.insert("processes", IvyJson::Array(vec![process]));
        }
        let rendered = check(
            mode,
            scenario(
                &fixture,
                "render",
                vec![processes(&original), next],
                empty(),
            ),
        )
        .await;
        assert!(succeeded(&rendered));
        let definition = rendered
            .required("operation")
            .unwrap()
            .required("value")
            .unwrap()
            .clone();
        check(
            "retained definition check",
            scenario(&fixture, "check", vec![definition.clone()], empty()),
        )
        .await;
        let mut changed = definition;
        changed.insert("content", "changed".into());
        assert!(!succeeded(
            &check(
                "retained definition hash changed",
                scenario(&fixture, "check", vec![changed], empty())
            )
            .await
        ));
    }
    let mut next = original.clone();
    next.insert("processes", IvyJson::Array(vec![]));
    check(
        "next plan omits exact instance",
        scenario(
            &fixture,
            "render",
            vec![processes(&original), next],
            empty(),
        ),
    )
    .await;
    check(
        "no retained plans",
        scenario(&fixture, "inspect", vec![IvyJson::Array(vec![])], empty()),
    )
    .await;
    for mode in [
        "ordinary",
        "duplicate-plan",
        "other-retained-plan",
        "per-process-candidate",
    ] {
        let mut original = original.clone();
        if mode == "per-process-candidate" {
            let mut process = processes(&original);
            process.insert("candidateId", hash::digest(b"process candidate").into());
            original.insert("processes", IvyJson::Array(vec![process]));
        }
        let mut plans = vec![original.clone()];
        if mode == "duplicate-plan" {
            plans.push(original.clone());
        }
        if mode == "other-retained-plan" {
            let mut other = original.clone();
            other.insert("extra", true.into());
            assert_ne!(
                definition(&other, &processes(&other)),
                definition(&original, &processes(&original))
            );
            plans.push(other);
        }
        let output = check(
            mode,
            scenario(
                &fixture,
                "inspect",
                vec![IvyJson::Array(plans)],
                backend(&original),
            ),
        )
        .await;
        assert!(succeeded(&output));
    }
}

#[tokio::test]
async fn systemd_bootstrap_owner_refuses_unowned_file_bytes_metadata_and_loaded_state() {
    let fixture = Fixture::new();
    let original = plan();
    let process = processes(&original);
    let unit = path(&process);
    let bytes = definition(&original, &process)
        .required("content")
        .unwrap()
        .clone();
    for (name, metadata) in [
        ("ordinary", empty()),
        ("not regular", v(r#"{"file":false}"#)),
        ("symlink", v(r#"{"symlink":true}"#)),
        ("foreign uid", v(r#"{"uid":42}"#)),
        ("group writable", v(r#"{"mode":436}"#)),
        ("world writable", v(r#"{"mode":422}"#)),
        ("bounded", v(r#"{"size":65536}"#)),
        ("oversize", v(r#"{"size":65537}"#)),
        ("stat error", v(r#"{"error":"EACCES"}"#)),
        ("read error", v(r#"{"readError":"EIO"}"#)),
        ("invalid UTF8", v(r#"{"bytes":[255,254],"size":2}"#)),
        ("unknown definition", v(r#"{"data":"foreign unit"}"#)),
    ] {
        let mut file = IvyJson::object([("data", bytes.clone())]);
        for (key, value) in metadata.as_object().unwrap() {
            file.insert(key.clone(), value.clone());
        }
        let mut model = backend(&original);
        model.insert("files", IvyJson::object([(unit.clone(), file)]));
        let output = check(
            name,
            scenario(
                &fixture,
                "inspect",
                vec![IvyJson::Array(vec![original.clone()])],
                model,
            ),
        )
        .await;
        assert_eq!(succeeded(&output), ["ordinary", "bounded"].contains(&name));
    }
    for (key, value) in [
        ("FragmentPath", "/foreign"),
        ("DropInPaths", "/foreign.conf"),
        ("LoadState", "not-found"),
        ("Transient", "yes"),
        ("UnitFileState", "masked"),
        ("UnitFileState", "static"),
    ] {
        let mut model = backend(&original);
        let mut command = status(&process, true, false);
        command.insert(
            "stdout",
            format!(
                "{}\n{key}={value}",
                utf(command.required("stdout").unwrap())
            )
            .into(),
        );
        model.insert("commands", IvyJson::Array(vec![command]));
        assert!(!succeeded(
            &check(
                key,
                scenario(
                    &fixture,
                    "inspect",
                    vec![IvyJson::Array(vec![original.clone()])],
                    model
                )
            )
            .await
        ));
    }
    for (active, sub, enabled, running) in [
        ("inactive", "dead", true, false),
        ("failed", "failed", false, false),
        ("inactive", "failed", true, false),
        ("failed", "dead", true, false),
        ("active", "running", true, true),
        ("inactive", "running", true, true),
        ("activating", "dead", true, true),
        ("", "", true, true),
    ] {
        let mut command = status(&process, enabled, false);
        let text = utf(command.required("stdout").unwrap())
            .replace("ActiveState=inactive", &format!("ActiveState={active}"))
            .replace("SubState=dead", &format!("SubState={sub}"));
        command.insert(
            "stdout",
            format!("\u{feff}broken\nTransient=yes\n{text}\u{a0}").into(),
        );
        let mut model = backend(&original);
        model.insert("commands", IvyJson::Array(vec![command]));
        let output = check(
            "stopped conjunction and final duplicate property",
            scenario(
                &fixture,
                "inspect",
                vec![IvyJson::Array(vec![original.clone()])],
                model,
            ),
        )
        .await;
        assert_eq!(
            output
                .required("operation")
                .unwrap()
                .required("value")
                .unwrap()
                .as_array()
                .unwrap()[0]
                .get("running"),
            Some(&running.into())
        );
    }
    for name in [
        "foreign",
        "ivy-next-111111111111-222222222222\n",
        "ivy-next-111111111111-222222222222\r\n",
        "ivy-next-111111111111-222222222222\u{2028}",
    ] {
        let mut plan = original.clone();
        let mut process = process.clone();
        process.insert("name", name.into());
        plan.insert("processes", IvyJson::Array(vec![process]));
        check(
            "unit name rejects suffix",
            scenario(
                &fixture,
                "inspect",
                vec![IvyJson::Array(vec![plan])],
                empty(),
            ),
        )
        .await;
    }
    let mut model = backend(&original);
    model.insert("commands", v(r#"[{"error":"EIO"}]"#));
    check(
        "bounded owner command failure",
        scenario(
            &fixture,
            "inspect",
            vec![IvyJson::Array(vec![original])],
            model,
        ),
    )
    .await;
}

#[tokio::test]
async fn systemd_bootstrap_apply_preserves_durable_publish_cleanup_and_prior_or_desired_replay() {
    let fixture = Fixture::new();
    let old = plan();
    let next = next_plan(&old);
    let process = processes(&old);
    let args = apply_args(&old, &next);
    let mut base = backend(&old);
    base.insert(
        "commands",
        IvyJson::Array(vec![status(&process, true, false); 2]),
    );
    let output = check(
        "atomic replacement",
        scenario(&fixture, "apply", args.clone(), base.clone()),
    )
    .await;
    assert!(succeeded(&output));
    assert_eq!(
        output
            .required("files")
            .unwrap()
            .required(&path(&process))
            .unwrap()
            .get("data"),
        definition(&next, &process).get("content")
    );
    for (name, mut model) in [
        ("already desired", backend(&next)),
        ("requires root", base.clone()),
        ("running owner", base.clone()),
        ("exclusive temp", base.clone()),
    ] {
        if name == "requires root" {
            model.insert("uid", IvyJson::Number(42.0));
        }
        if name == "running owner" {
            model.insert(
                "commands",
                IvyJson::Array(vec![status(&process, true, true)]),
            );
        }
        if name == "exclusive temp" {
            model.insert("createRace", v(r#"{"data":"competitor"}"#));
        }
        let output = check(name, scenario(&fixture, "apply", args.clone(), model)).await;
        assert_eq!(succeeded(&output), name == "already desired");
    }
    let mut wrong = args.clone();
    wrong[0].insert("definitionHash", hash::digest(b"stale").into());
    check(
        "stale old hash",
        scenario(&fixture, "apply", wrong, base.clone()),
    )
    .await;
    let mut wrong = args.clone();
    wrong[1].insert("hash", hash::digest(b"tampered").into());
    check(
        "tampered desired",
        scenario(&fixture, "apply", wrong, base.clone()),
    )
    .await;
    for (op, at) in [
        ("stat", 1),
        ("read", 1),
        ("command", 1),
        ("open", 1),
        ("write", 1),
        ("sync", 1),
        ("close", 1),
        ("stat", 2),
        ("read", 2),
        ("command", 2),
        ("rename", 1),
        ("open", 2),
        ("sync", 2),
        ("close", 2),
        ("unlink", 1),
    ] {
        let mut model = base.clone();
        model.insert("faults", IvyJson::Array(vec![fault(op, at)]));
        check(
            &format!("apply failure {op} {at}"),
            scenario(&fixture, "apply", args.clone(), model),
        )
        .await;
    }
    let mut model = base.clone();
    model.insert(
        "faults",
        IvyJson::Array(vec![fault("rename", 1), fault("unlink", 1)]),
    );
    check(
        "cleanup failure overrides publication failure",
        scenario(&fixture, "apply", args.clone(), model),
    )
    .await;
    let mut model = base.clone();
    model.insert(
        "commands",
        IvyJson::Array(vec![
            status(&process, true, false),
            status(&process, true, true),
        ]),
    );
    check(
        "recheck owner started",
        scenario(&fixture, "apply", args.clone(), model),
    )
    .await;
    for (name, data) in [
        (
            "recheck desired from other writer",
            definition(&next, &process)
                .required("content")
                .unwrap()
                .clone(),
        ),
        ("recheck foreign definition", "foreign".into()),
    ] {
        let mut model = base.clone();
        model.insert(
            "mutations",
            IvyJson::Array(vec![IvyJson::object([
                ("op", "stat".into()),
                ("at", IvyJson::Number(2.0)),
                (
                    "files",
                    IvyJson::object([(path(&process), IvyJson::object([("data", data)]))]),
                ),
            ])]),
        );
        let output = check(name, scenario(&fixture, "apply", args.clone(), model)).await;
        assert!(!succeeded(&output));
        assert!(
            !output
                .required("trace")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e.get("op") == Some(&"rename".into()))
        );
    }
}

#[tokio::test]
async fn systemd_bootstrap_pause_resume_preserve_exact_identity_scheduling_and_running_selection() {
    let fixture = Fixture::new();
    let original = plan();
    let process = processes(&original);
    let plans = IvyJson::Array(vec![original.clone()]);
    let mut paired = original.clone();
    let mut second = process.clone();
    second.insert("instanceId", "second-owner".into());
    second.insert("name", "ivy-next-111111111111-333333333333".into());
    paired.insert("processes", IvyJson::Array(vec![process.clone(), second]));
    for running in [
        None,
        Some(IvyJson::Null),
        Some(IvyJson::Number(0.0)),
        Some(IvyJson::Number(1.0)),
        Some("".into()),
        Some("false".into()),
        Some(IvyJson::Array(vec![])),
        Some(empty()),
    ] {
        let mut state = snapshot(&paired, true);
        let mut owners = state
            .required("owners")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        match running {
            Some(value) => owners[1].insert("running", value),
            None => {
                owners[1].remove("running");
            }
        }
        state.insert("owners", IvyJson::Array(owners));
        let output = run(&scenario(
            &fixture,
            "resume",
            vec![state, IvyJson::Array(vec![paired.clone()])],
            backend(&paired),
        ))
        .await;
        assert!(!succeeded(&output));
        assert!(
            output
                .required("trace")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(output.get("files"), backend(&paired).get("files"));
    }
    for (name, mut state) in [
        ("foreign host", snapshot(&original, false)),
        ("foreign installation", snapshot(&original, false)),
        ("owner count", snapshot(&original, false)),
        ("candidate changed", snapshot(&original, false)),
        ("definition changed", snapshot(&original, false)),
    ] {
        match name {
            "foreign host" => state.insert("hostId", "foreign".into()),
            "foreign installation" => state.insert("installationId", "foreign".into()),
            "owner count" => state.insert("owners", IvyJson::Array(vec![])),
            _ => {
                let mut owner = owner(&original, &process, true, false);
                owner.insert(
                    if name == "candidate changed" {
                        "candidateId"
                    } else {
                        "definitionHash"
                    },
                    hash::digest(b"foreign").into(),
                );
                state.insert("owners", IvyJson::Array(vec![owner]));
            }
        }
        assert!(!succeeded(
            &check(
                name,
                scenario(
                    &fixture,
                    "pause",
                    vec![state, plans.clone()],
                    backend(&original)
                )
            )
            .await
        ));
    }
    for running in [false, true] {
        let mut model = backend(&original);
        let mut commands = vec![status(&process, true, running)];
        if running {
            commands.push(empty());
        }
        commands.push(status(&process, true, false));
        model.insert("commands", IvyJson::Array(commands));
        assert!(succeeded(
            &check(
                "pause exact observed state",
                scenario(
                    &fixture,
                    "pause",
                    vec![snapshot(&original, running), plans.clone()],
                    model
                )
            )
            .await
        ));
        let mut model = backend(&original);
        model.insert(
            "commands",
            IvyJson::Array(vec![status(&process, true, false), empty()]),
        );
        assert!(succeeded(
            &check(
                "resume only original running owners",
                scenario(
                    &fixture,
                    "resume",
                    vec![snapshot(&original, running), plans.clone()],
                    model
                )
            )
            .await
        ));
    }
    let mut model = backend(&original);
    model.insert(
        "commands",
        IvyJson::Array(vec![
            status(&process, true, true),
            empty(),
            status(&process, true, true),
        ]),
    );
    check(
        "pause did not stop",
        scenario(
            &fixture,
            "pause",
            vec![snapshot(&original, true), plans.clone()],
            model,
        ),
    )
    .await;
    let mut model = backend(&original);
    model.insert(
        "commands",
        IvyJson::Array(vec![status(&process, false, false)]),
    );
    check(
        "resume scheduling changed",
        scenario(
            &fixture,
            "resume",
            vec![snapshot(&original, true), plans.clone()],
            model,
        ),
    )
    .await;
    let mut state = snapshot(&original, true);
    state.insert("hostId", "ignored by resume".into());
    let mut original_owner = owner(&original, &process, true, true);
    original_owner.insert("definitionHash", hash::digest(b"old definition").into());
    state.insert("owners", IvyJson::Array(vec![original_owner]));
    let output = check(
        "resume compares scheduling rather than old definition",
        scenario(&fixture, "resume", vec![state, plans], backend(&original)),
    )
    .await;
    assert!(succeeded(&output));
}

#[tokio::test]
async fn systemd_bootstrap_observes_multiple_owners_and_pauses_executor_last() {
    let fixture = Fixture::new();
    let mut original = plan();
    let executor = processes(&original);
    let mut manager = executor.clone();
    manager.insert("instanceId", "renamed-manager".into());
    manager.insert("componentId", "service-manager".into());
    manager.insert("name", "ivy-next-111111111111-333333333333".into());
    original.insert(
        "processes",
        IvyJson::Array(vec![executor.clone(), manager.clone()]),
    );
    assert!(succeeded(
        &check_parallel(
            "parallel retained owners",
            scenario(
                &fixture,
                "inspect",
                vec![IvyJson::Array(vec![original.clone()])],
                backend(&original)
            )
        )
        .await
    ));
    let mut model = backend(&original);
    model.insert(
        "commands",
        IvyJson::Array(vec![
            status(&executor, true, true),
            status(&manager, true, true),
            empty(),
            empty(),
            status(&executor, true, false),
            status(&manager, true, false),
        ]),
    );
    let output = check_parallel(
        "service manager stops before executor",
        scenario(
            &fixture,
            "pause",
            vec![snapshot(&original, true), IvyJson::Array(vec![original])],
            model,
        ),
    )
    .await;
    assert!(succeeded(&output));
    let commands: Vec<_> = output
        .required("trace")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e.get("op") == Some(&"command".into())
                && e.get("args")
                    .and_then(IvyJson::as_array)
                    .and_then(|a| a.first())
                    == Some(&"stop".into())
        })
        .collect();
    assert_eq!(
        commands[0].required("args").unwrap().as_array().unwrap()[1],
        format!("{}.service", utf(manager.required("name").unwrap())).into()
    );
}

#[tokio::test]
async fn systemd_bootstrap_prepare_reuses_resource_ownership_and_bounded_reload() {
    let fixture = Fixture::new();
    let mut next = plan();
    let resource = scope();
    let slice = slice_path(&resource);
    assert!(succeeded(
        &check(
            "prepare no optional dependencies",
            scenario(&fixture, "prepare", vec![next.clone()], empty())
        )
        .await
    ));
    for value in [
        IvyJson::Null,
        false.into(),
        IvyJson::Number(0.0),
        "".into(),
        IvyJson::Array(vec![]),
        empty(),
    ] {
        let mut malformed = next.clone();
        malformed.insert("linuxResources", value.clone());
        let output = run(&scenario(&fixture, "prepare", vec![malformed], empty())).await;
        assert!(!succeeded(&output), "resource scope: {value:?}");
        assert!(
            output
                .required("trace")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    for key in ["user", "configPath"] {
        for value in [
            IvyJson::Null,
            false.into(),
            IvyJson::Number(0.0),
            IvyJson::Array(vec![]),
            empty(),
        ] {
            let mut malformed = next.clone();
            let mut process = processes(&malformed);
            process.insert(key, value.clone());
            malformed.insert("processes", IvyJson::Array(vec![process]));
            let output = run(&scenario(&fixture, "prepare", vec![malformed], empty())).await;
            assert!(!succeeded(&output), "{key}: {value:?}");
            assert!(
                output
                    .required("trace")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    let artifact = fixture.0.join("release");
    let executable = artifact
        .join("dist")
        .join("native")
        .join("ivy-agent-manager");
    let config_path = fixture.0.join("private/agent.json");
    let worker = fixture.0.join("tools/worker");
    let data = fixture.0.join("data");
    let projects = fixture.0.join("projects");
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::create_dir_all(worker.parent().unwrap()).unwrap();
    fs::write(&executable, b"native service fixture").unwrap();
    fs::write(&worker, b"native worker fixture").unwrap();
    let mut runtime = v(
        r#"{"schemaVersion":1,"hostId":"fixture","instanceId":"agent","serviceNodeId":"Fixture.agent-manager","componentId":"agent-manager","publicBaseUrl":"http://127.0.0.1","buildId":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","version":"1.0.0","settings":{}}"#,
    );
    let path = |path: &std::path::Path| IvyJson::String(node_path::from_native(path));
    runtime.insert("artifactRoot", path(&artifact));
    runtime.insert("dataRoot", path(&data));
    runtime.insert(
        "settings",
        IvyJson::object([
            ("nativeExecutable", path(&worker)),
            ("projectRoot", path(&projects)),
        ]),
    );
    let config_bytes = json::stringify(&runtime, usize::MAX).unwrap().into_bytes();
    fs::write(&config_path, &config_bytes).unwrap();
    fs::write(
        artifact.join("dist/build-info.json"),
        json::stringify(
            &IvyJson::object([
                ("buildId", runtime.required("buildId").unwrap().clone()),
                ("version", "1.0.0".into()),
            ]),
            usize::MAX,
        )
        .unwrap(),
    )
    .unwrap();
    let mut native = next.clone();
    native.remove("nodeExecutable");
    native.insert("artifactRoot", path(&artifact));
    let mut process = processes(&native);
    process.insert("componentId", "agent-manager".into());
    process.insert("user", "ivy-agent".into());
    process.insert("configPath", path(&config_path));
    process.insert(
        "entrypoint",
        v(r#"{"executable":"dist/native/ivy-agent-manager","args":[],"timeoutMs":5000}"#),
    );
    native.insert("processes", IvyJson::Array(vec![process]));
    let mut files = empty();
    for item in [
        &artifact,
        &executable,
        &config_path,
        &worker,
        &data,
        &projects,
    ] {
        files.insert(
            node_path::from_native(item),
            v(r#"{"data":"fixture","uid":1000,"gid":1000}"#),
        );
    }
    let identity_model = IvyJson::object([
        ("files", files.clone()),
        (
            "commands",
            v(r#"[{"stdout":"2001\n"},{"stdout":"3001\n"}]"#),
        ),
    ]);
    for node in [
        None,
        Some(NodeRuntime {
            executable: PathBuf::from("unused-node"),
            version: "0.0.0".into(),
        }),
    ] {
        let host = Host::new(identity_model.clone());
        let os = SystemdBootstrapOs::with_hosts(node, host.clone(), host.clone());
        let result = os
            .prepare(native.clone())
            .unwrap()
            .await
            .map(|_| IvyJson::Null);
        let output = host.output(result);
        assert!(
            succeeded(&output),
            "{}",
            json::stringify(&output, usize::MAX).unwrap()
        );
        assert_eq!(output.get("files"), Some(&files));
        let trace = output.required("trace").unwrap().as_array().unwrap();
        for args in [
            IvyJson::Array(vec![
                "-m".into(),
                "u:ivy-agent:r--".into(),
                path(&config_path),
            ]),
            IvyJson::Array(vec![
                "-m".into(),
                "u:ivy-agent:r-x".into(),
                path(&executable),
            ]),
            IvyJson::Array(vec!["-m".into(), "u:ivy-agent:r-x".into(), path(&worker)]),
            IvyJson::Array(vec![
                "-R".into(),
                "-m".into(),
                "u:ivy-agent:r-X".into(),
                path(&artifact),
            ]),
            IvyJson::Array(vec!["-R".into(), "ivy-agent:ivy-agent".into(), path(&data)]),
            IvyJson::Array(vec![
                "-R".into(),
                "-m".into(),
                "u:ivy-agent:rwX".into(),
                path(&projects),
            ]),
        ] {
            assert!(
                trace.iter().any(|event| event.get("args") == Some(&args)),
                "{args:?}"
            );
        }
        assert!(trace.iter().all(|event| {
            event.get("path") != Some(&"unused-node".into())
                && !event
                    .get("args")
                    .and_then(IvyJson::as_array)
                    .is_some_and(|args| args.contains(&"unused-node".into()))
        }));
        assert_eq!(fs::read(&config_path).unwrap(), config_bytes);
    }
    let node_path = fixture.0.join("tools/node");
    for mode in ["missing-node", "valid-node", "incompatible-node"] {
        let mut javascript = native.clone();
        let mut process = processes(&javascript);
        process.insert(
            "entrypoint",
            v(r#"{"executable":"node","args":["dist/main.js"],"timeoutMs":5000}"#),
        );
        javascript.insert("processes", IvyJson::Array(vec![process]));
        if mode != "missing-node" {
            javascript.insert("nodeExecutable", path(&node_path));
        }
        let mut model = identity_model.clone();
        let mut files = files.clone();
        files.insert(
            node_path::from_native(&node_path),
            v(r#"{"data":"Node fixture"}"#),
        );
        model.insert("files", files);
        let host = Host::new(model);
        let os = SystemdBootstrapOs::with_hosts(
            Some(NodeRuntime {
                executable: node_path.clone(),
                version: if mode == "incompatible-node" {
                    "0.0.0"
                } else {
                    "24.18.0"
                }
                .into(),
            }),
            host.clone(),
            host.clone(),
        );
        let result = os.prepare(javascript).unwrap().await.map(|_| IvyJson::Null);
        let output = host.output(result);
        assert_eq!(
            succeeded(&output),
            mode == "valid-node",
            "{mode}: {}",
            json::stringify(&output, usize::MAX).unwrap()
        );
        let trace = output.required("trace").unwrap().as_array().unwrap();
        if mode == "valid-node" {
            let args = IvyJson::Array(vec![
                "-m".into(),
                "u:ivy-agent:r-x".into(),
                path(&node_path),
            ]);
            assert!(trace.iter().any(|event| event.get("args") == Some(&args)));
        } else {
            assert!(trace.is_empty(), "{mode}");
        }
    }
    next.insert("linuxResources", resource.clone());
    let shown=IvyJson::object([("stdout",format!("FragmentPath={slice}\nDropInPaths=\nTransient=no\nLoadState=loaded\nMemoryHigh=201326592\nMemoryMax=268435456\nMemorySwapMax=0\nTasksMax=64\n").into())]);
    for mode in [
        "existing",
        "create",
        "changed bytes",
        "reload failure",
        "loaded override",
    ] {
        let mut model = empty();
        let mut file = slice_file(&resource);
        if mode == "changed bytes" {
            file.insert("data", "foreign".into());
        }
        if mode != "create" {
            model.insert("files", IvyJson::object([(slice.clone(), file)]));
        }
        let mut command = shown.clone();
        if mode == "loaded override" {
            command.insert(
                "stdout",
                format!(
                    "{}DropInPaths=/foreign.conf\n",
                    utf(command.required("stdout").unwrap())
                )
                .into(),
            );
        }
        model.insert(
            "commands",
            IvyJson::Array(vec![
                if mode == "reload failure" {
                    v(r#"{"error":"EIO"}"#)
                } else {
                    empty()
                },
                command,
            ]),
        );
        let output = check(
            mode,
            scenario(&fixture, "prepare", vec![next.clone()], model),
        )
        .await;
        assert_eq!(succeeded(&output), ["existing", "create"].contains(&mode));
    }
    for failure in [false, true] {
        check(
            "bounded reload",
            scenario(
                &fixture,
                "reload",
                vec![],
                if failure {
                    v(r#"{"commands":[{"error":"EIO"}]}"#)
                } else {
                    empty()
                },
            ),
        )
        .await;
    }
    let mut model = backend(&next);
    let mut files = model.required("files").unwrap().clone();
    files.insert(slice, slice_file(&resource));
    model.insert("files", files);
    model.insert(
        "commands",
        IvyJson::Array(vec![shown, status(&processes(&next), true, false)]),
    );
    assert!(succeeded(
        &check(
            "owner verifies retained resource before service",
            scenario(&fixture, "inspect", vec![IvyJson::Array(vec![next])], model)
        )
        .await
    ));
}

#[tokio::test]
async fn systemd_bootstrap_failed_owner_does_not_cancel_other_admitted_observations() {
    let mut original = plan();
    let mut invalid = processes(&original);
    invalid.insert("name", "foreign-unit".into());
    let mut valid = processes(&original);
    valid.insert("instanceId", "renamed-manager".into());
    valid.insert("componentId", "service-manager".into());
    valid.insert("name", "ivy-next-111111111111-333333333333".into());
    original.insert("processes", IvyJson::Array(vec![invalid, valid.clone()]));
    let mut model = backend(&original);
    model.insert(
        "commands",
        IvyJson::Array(vec![status(&valid, true, false)]),
    );
    let host = Host::new(model);
    let error = host.os().inspect(vec![original]).await.unwrap_err();
    assert_eq!(error.code, "target_conflict");
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if host
                .0
                .inner
                .state
                .lock()
                .unwrap()
                .trace
                .iter()
                .any(|event| event.get("op") == Some(&"command".into()))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let output = host.output(Err(error));
    record(
        "owner failure preserves admitted peer",
        &IvyJson::Null,
        &output,
        &IvyJson::Null,
    );
    assert!(
        output
            .required("trace")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event.get("op") == Some(&"read".into())
                && event.get("path") == Some(&path(&valid).into()))
    );
}

#[tokio::test]
async fn systemd_bootstrap_admitted_apply_finishes_after_observer_cancellation() {
    let old = plan();
    let next = next_plan(&old);
    let process = processes(&old);
    let mut model = backend(&old);
    model.insert(
        "commands",
        IvyJson::Array(vec![status(&process, true, false); 2]),
    );
    let host = Host::new(model);
    host.0.inner.pause.store(true, Ordering::SeqCst);
    let os = host.os();
    let args = apply_args(&old, &next);
    let waiter = tokio::spawn(async move {
        os.apply(
            args[0].clone(),
            args[1].clone(),
            args[2].as_array().unwrap().to_vec(),
        )
        .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        host.0.inner.opened.notified(),
    )
    .await
    .unwrap();
    waiter.abort();
    host.0.inner.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if host
                .0
                .inner
                .state
                .lock()
                .unwrap()
                .trace
                .last()
                .is_some_and(|e| e.get("op") == Some(&"unlink".into()))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let output = host.output(Ok(IvyJson::Null));
    record(
        "admitted apply outlives observer",
        &IvyJson::Null,
        &output,
        &IvyJson::Null,
    );
    assert_eq!(
        output
            .required("files")
            .unwrap()
            .required(&path(&process))
            .unwrap()
            .get("data"),
        definition(&next, &process).get("content")
    );
    assert_eq!(
        output.required("files").unwrap().as_object().unwrap().len(),
        1
    );
}
