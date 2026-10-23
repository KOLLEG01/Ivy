use ivy_contracts::{IvyJson, JsString, hash, json};
use ivy_host_runtime::{
    IvyError, Outcome,
    executor_lock::ExecutorLock,
    journal::HostJournal,
    node_path,
    node_runtime::NodeRuntime,
    portable_http::PortableHttp,
    preparations,
    prepare::{self, PreparationRuntime},
};
use std::{
    cmp::Ordering,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
static CORPUS: Mutex<()> = Mutex::new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-prepare-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn path(value: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(value))
}
fn native(value: &IvyJson) -> PathBuf {
    node_path::filesystem_path(value.as_js_string().unwrap())
}
fn root() -> PathBuf {
    std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        PathBuf::from,
    )
}
fn write(file: &Path, bytes: &[u8]) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, bytes).unwrap();
}
fn load(file: &Path) -> IvyJson {
    json::parse(&fs::read_to_string(file).unwrap(), usize::MAX).unwrap()
}
fn tools() -> (PathBuf, PathBuf, PathBuf) {
    let output = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let node = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let tar = std::env::var_os("IVY_HOST_SYSTEM_TAR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if cfg!(windows) {
                PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/tar.exe")
            } else {
                PathBuf::from("/usr/bin/tar")
            }
        });
    (
        node,
        tar,
        std::env::var_os("IVY_HOST_JOB_LAUNCHER")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_ivy-job"))),
    )
}
struct Inputs {
    root: PathBuf,
    host: IvyJson,
    snapshot: IvyJson,
    bootstrap: PathBuf,
}
fn inputs(parent: &Fixture, name: &str, mode: &str, tools: &(PathBuf, PathBuf, PathBuf)) -> Inputs {
    let root = parent.0.join(name);
    fs::create_dir(&root).unwrap();
    let staging = root.join("staging");
    let captured = staging.join("snapshot/source");
    let bootstrap = root.join("bootstrap");
    let snapshot = IvyJson::object([
        ("snapshotId", "fixed-snapshot".into()),
        ("sourceRoot", path(&captured)),
        ("originalRoot", path(&root.join("checkout"))),
        (
            "manifestPath",
            path(&staging.join("snapshot/snapshot.json")),
        ),
        ("capturedAt", "2026-01-01T00:00:00.000Z".into()),
    ]);
    let host = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", "fixture-host".into()),
        ("runtimeRoot", path(&root.join("runtime"))),
        ("artifactRoot", path(&root.join("artifacts"))),
        ("stagingRoot", path(&staging)),
        ("publicBaseUrl", "http://127.0.0.1:39081/ivy".into()),
        ("instances", IvyJson::Array(vec![])),
        (
            "executables",
            IvyJson::object([("node", path(&tools.0)), ("tar", path(&tools.1))]),
        ),
    ]);
    let command = IvyJson::object([
        ("executable", "node".into()),
        (
            "args",
            IvyJson::Array(vec!["tools/build.mjs".into(), mode.into()]),
        ),
        ("timeoutMs", IvyJson::Number(10000.0)),
    ]);
    let plan = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("componentId", "fixture".into()),
        ("kind", "app".into()),
        ("version", "1.2.3".into()),
        ("description", "Isolated app build fixture".into()),
        ("connectsToHive", false.into()),
        (
            "requirements",
            IvyJson::object([
                ("node", ">=24.18.0 <25.0.0".into()),
                ("hiveProtocol", IvyJson::Null),
                ("contracts", IvyJson::Array(vec![])),
            ]),
        ),
        ("prepare", IvyJson::Array(vec![command])),
        ("checks", IvyJson::Array(vec![])),
        (
            "app",
            IvyJson::object([
                ("appId", "fixture".into()),
                ("dist", "dist/apps/fixture".into()),
                ("entryPath", "index.html".into()),
            ]),
        ),
    ]);
    write(
        &captured.join("ui/fixture/deploy.json"),
        json::canonical(&plan, usize::MAX).unwrap().as_bytes(),
    );
    write(
        &captured.join("package.json"),
        br#"{"packageManager":"npm@1.2.3"}"#,
    );
    write(
        &captured.join("package-lock.json"),
        br#"{"lockfileVersion":3,"packages":{}}"#,
    );
    write(&captured.join("tools/build.mjs"),br#"import fs from 'node:fs';
if(process.argv[2]==='fail')process.exit(17);
if(process.argv[2]==='gate'){fs.writeFileSync('started','started');while(!fs.existsSync('release'))await new Promise(r=>setTimeout(r,10));}
fs.mkdirSync('dist/apps/fixture',{recursive:true});fs.writeFileSync('dist/apps/fixture/index.html','fixture output');fs.writeFileSync('dist/apps/fixture/ignored.js.map','must be omitted');
console.log('built '+process.env.IVY_DEPENDENCY_CACHE_HIT);
"#);
    write(
        &staging.join("snapshot/snapshot.json"),
        json::canonical(
            &IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("snapshot", snapshot.clone()),
            ]),
            usize::MAX,
        )
        .unwrap()
        .as_bytes(),
    );
    fs::create_dir_all(bootstrap.join("dist/native")).unwrap();
    fs::copy(&tools.2, bootstrap.join("dist/native/ivy-job.exe")).unwrap();
    Inputs {
        root,
        host,
        snapshot,
        bootstrap,
    }
}
fn oracle(input: IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-prepare-workflow-oracle.mjs",
        ])
        .current_dir(root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn runtime(node: &Path, inputs: &Inputs) -> PreparationRuntime {
    fn inventory(base: &Path, directory: &Path, files: &mut Vec<JsString>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                inventory(base, &entry.path(), files);
            } else {
                files.push(
                    entry
                        .path()
                        .strip_prefix(base)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/")
                        .into(),
                );
            }
        }
    }
    let source = native(inputs.snapshot.required("sourceRoot").unwrap());
    let mut files = Vec::new();
    inventory(&source, &source, &mut files);
    let input = IvyJson::object([
        ("operation", "order".into()),
        (
            "paths",
            IvyJson::Array(files.iter().cloned().map(IvyJson::String).collect()),
        ),
    ]);
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-preparation-inputs-oracle.mjs",
        ])
        .current_dir(root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let matrix = json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let matrix = matrix
        .required("operation")
        .unwrap()
        .required("value")
        .unwrap()
        .required("matrix")
        .unwrap()
        .clone();
    let compare = Arc::new(move |a: &JsString, b: &JsString| {
        let a = files
            .iter()
            .position(|p| a == p)
            .expect("unknown locale fixture key");
        let b = files
            .iter()
            .position(|p| b == p)
            .expect("unknown locale fixture key");
        match matrix.as_array().unwrap()[a].as_array().unwrap()[b]
            .as_f64()
            .unwrap() as i32
        {
            -1 => Ordering::Less,
            0 => Ordering::Equal,
            1 => Ordering::Greater,
            _ => unreachable!(),
        }
    });
    PreparationRuntime {
        node: Some(NodeRuntime {
            executable: node.into(),
            version: "24.18.0".into(),
        }),
        compare,
        imports: Some(Arc::new(|_, _| panic!("app must not invoke the service parser"))),
        http: PortableHttp::new(reqwest::Client::builder().no_proxy()).unwrap(),
    }
}
fn append(actual: &IvyJson, expected: &IvyJson, scope: &str) {
    if let Some(file) = std::env::var_os("IVY_HOST_PREPARE_WORKFLOW_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(
                &IvyJson::object([
                    ("actual", actual.clone()),
                    ("expected", expected.clone()),
                    ("scope", scope.into())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
}
async fn assert_cache_lease_released(staging: &Path) {
    let roots: Vec<_> = fs::read_dir(staging.join("dependency-cache-locks"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(roots.len(), 1);
    let lock = ExecutorLock::acquire(roots[0].clone(), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
}
const SERVICE_MAIN: &str = "import value from 'fixture-dep'; console.log(value);";
const SERVICE_HEALTH: &str = "process.exit(0);";
fn service_inputs(parent: &Fixture, name: &str, tools: &(PathBuf, PathBuf, PathBuf)) -> Inputs {
    let input = inputs(parent, name, "service", tools);
    let source = native(input.snapshot.required("sourceRoot").unwrap());
    let mut plan = load(&source.join("ui/fixture/deploy.json"));
    fs::remove_dir_all(source.join("ui")).unwrap();
    plan.insert("kind", "service".into());
    plan.remove("app");
    let command = |name: &str| {
        IvyJson::object([
            ("executable", "node".into()),
            ("args", IvyJson::Array(vec![name.into()])),
            ("timeoutMs", IvyJson::Number(10000.0)),
        ])
    };
    plan.insert("entrypoint", command("dist/main.js"));
    plan.insert(
        "readiness",
        IvyJson::object([
            ("timeoutMs", IvyJson::Number(10000.0)),
            ("command", command("dist/health.js")),
        ]),
    );
    plan.insert(
        "shutdown",
        IvyJson::object([("timeoutMs", IvyJson::Number(5000.0))]),
    );
    plan.insert(
        "restart",
        IvyJson::object([
            ("policy", "never".into()),
            ("minimumDelayMs", IvyJson::Number(100.0)),
            ("maximumDelayMs", IvyJson::Number(1000.0)),
        ]),
    );
    plan.insert(
        "prepare",
        IvyJson::Array(vec![
            IvyJson::object([
                ("executable", "node".into()),
                (
                    "args",
                    IvyJson::Array(vec!["tools/build/run-npm.mjs".into(), "ci".into()]),
                ),
                ("timeoutMs", IvyJson::Number(10000.0)),
            ]),
            command("tools/build.mjs"),
        ]),
    );
    write(
        &source.join("services/fixture/deploy.json"),
        json::canonical(&plan, usize::MAX).unwrap().as_bytes(),
    );
    write(&source.join("package-lock.json"),br#"{"lockfileVersion":3,"packages":{"node_modules/fixture-dep":{"version":"1.0.0"},"node_modules/development":{"version":"1.0.0","dev":true}}}"#);
    // A deterministic installer fixture exercises the original retain-after-ci
    // branch without installing packages or contacting a registry.
    write(&source.join("tools/build/run-npm.mjs"),br#"import fs from 'node:fs';
if(process.env.IVY_DEPENDENCY_CACHE_HIT==='1'){
 if(!fs.existsSync('node_modules/fixture-dep/index.js'))throw Error('Warm cache is unavailable');
}else{
 for(const name of ['fixture-dep','development']){fs.mkdirSync('node_modules/'+name,{recursive:true});fs.writeFileSync('node_modules/'+name+'/package.json',JSON.stringify({name,version:'1.0.0'}));fs.writeFileSync('node_modules/'+name+'/index.js','module.exports=1;');}
}
"#);
    let script = format!(
        "import fs from 'node:fs';fs.mkdirSync('dist',{{recursive:true}});fs.writeFileSync('dist/main.js',{});fs.writeFileSync('dist/health.js',{});fs.writeFileSync('dist/cache-hit.txt',process.env.IVY_DEPENDENCY_CACHE_HIT);fs.writeFileSync('dist/ignored.js.map','excluded');",
        json::stringify(&SERVICE_MAIN.into(), usize::MAX).unwrap(),
        json::stringify(&SERVICE_HEALTH.into(), usize::MAX).unwrap()
    );
    write(&source.join("tools/build.mjs"), script.as_bytes());
    input
}
fn service_runtime(node: &Path, input: &Inputs) -> PreparationRuntime {
    let mut runtime = runtime(node, input);
    // Exact fixture source bindings only. Parser compatibility is covered by
    // the original TypeScript AST corpus; production must supply that binding.
    runtime.imports = Some(Arc::new(|_, source| {
        if source == &JsString::from(SERVICE_MAIN) {
            Ok(vec!["fixture-dep".into()])
        } else {
            assert_eq!(source, &JsString::from(SERVICE_HEALTH));
            Ok(Vec::new())
        }
    }));
    runtime
}
fn advance_service_plan(input: &Inputs) {
    let source = native(input.snapshot.required("sourceRoot").unwrap());
    let file = source.join("services/fixture/deploy.json");
    let mut plan = load(&file);
    plan.insert("version", "1.2.4".into());
    write(
        &file,
        json::canonical(&plan, usize::MAX).unwrap().as_bytes(),
    );
}
#[tokio::test]
async fn native_activation_runtime_prepares_real_captured_service_and_registers_immutable_publication()
 {
    use ivy_host_runtime::executor::{
        ActivationRuntime, ExecutorOptions, NativeActivationRuntime, NativeOfflineHiveInspector,
        OperationContext,
    };
    use ivy_host_runtime::portable_native_files::NativeHelperLayout;
    let fixture = Fixture::new();
    let tools = tools();
    let mut input = service_inputs(&fixture, "native-runtime", &tools);
    input.host.insert(
        "instances",
        IvyJson::Array(vec![IvyJson::object([
            ("instanceId", "fixture".into()),
            ("serviceNodeId", "fixture".into()),
            ("componentId", "fixture".into()),
            ("engine", "process".into()),
            ("enabled", true.into()),
            ("settings", IvyJson::object(Vec::<(String, IvyJson)>::new())),
        ])]),
    );
    let journal = HostJournal::open(input.host.clone()).await.unwrap();
    let entry = journal
        .accept(
            IvyJson::object([
                ("action", "deploy".into()),
                ("instanceId", "fixture".into()),
                (
                    "source",
                    input.snapshot.required("originalRoot").unwrap().clone(),
                ),
                ("operationId", "native-prepare".into()),
            ]),
            Some(input.snapshot.clone()),
        )
        .await
        .unwrap();
    let context = OperationContext::open(
        &journal,
        &entry,
        ExecutorOptions {
            own_instance_id: Some("independent".into()),
            account_home: node_path::from_native(&fixture.0),
            build_id: None,
        },
        tokio_util::sync::CancellationToken::new(),
        Arc::new(ivy_sdk::clock::SystemClock),
    )
    .await
    .unwrap();
    let preparation = service_runtime(&tools.0, &input);
    let inspector = NativeOfflineHiveInspector::new(
        tools.2.clone(),
        NativeHelperLayout::Target,
    );
    let runtime = NativeActivationRuntime::new(
        input.bootstrap.clone(),
        preparation,
        tools.2.clone(),
        Arc::new(inspector),
    )
    .unwrap();
    let candidate = runtime
        .prepare(&context, &input.snapshot, "fixture")
        .await
        .unwrap();
    let artifact = native(candidate.required("artifactRoot").unwrap());
    assert_eq!(fs::read(artifact.join("dist/cache-hit.txt")).unwrap(), b"0");
    assert!(artifact.join("node_modules/fixture-dep/index.js").is_file());
    assert!(!artifact.join("node_modules/development").exists());
    assert!(!artifact.join("dist/ignored.js.map").exists());
    let id = candidate.required("candidateId").unwrap().as_str().unwrap();
    assert_eq!(
        json::canonical(&journal.candidate(&id).await.unwrap(), usize::MAX).unwrap(),
        json::canonical(&candidate, usize::MAX).unwrap()
    );
    assert_eq!(
        hash::digest(fs::read(native(candidate.required("archivePath").unwrap())).unwrap()),
        candidate.required("archiveHash").unwrap().as_str().unwrap()
    );
    let manifest = journal.manifest(&id).await.unwrap();
    assert_eq!(manifest.required("componentId").unwrap(), &"fixture".into());
    assert_eq!(
        manifest.required("buildId").unwrap(),
        candidate.required("buildId").unwrap()
    );
    assert_cache_lease_released(&native(input.host.required("stagingRoot").unwrap())).await;
    assert!(journal.target("fixture").await.unwrap().is_none());
    assert!(journal.installed("fixture").await.unwrap().is_none());
    runtime.dispose().await.unwrap();
    context.journal.close().await.unwrap();
    journal.close().await.unwrap();
}
#[tokio::test]
async fn service_preparation_uses_the_emitted_lock_closure_and_reuses_dependencies_for_a_new_build()
{
    let fixture = Fixture::new();
    let tools = tools();
    let actual = service_inputs(&fixture, "actual", &tools);
    let original = service_inputs(&fixture, "original", &tools);
    let mut first = None;
    for hit in [false, true] {
        let result = prepare::prepare_candidate(
            actual.snapshot.clone(),
            "fixture".into(),
            actual.host.clone(),
            actual.bootstrap.clone(),
            service_runtime(&tools.0, &actual),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        let candidate = result.as_ref().unwrap();
        let artifact = native(candidate.required("artifactRoot").unwrap());
        assert_eq!(
            fs::read(artifact.join("dist/cache-hit.txt")).unwrap(),
            if hit { b"1" } else { b"0" }
        );
        assert!(artifact.join("node_modules/fixture-dep/index.js").is_file());
        assert!(!artifact.join("node_modules/development").exists());
        assert!(!artifact.join("dist/ignored.js.map").exists());
        if let Some(prior) = &first {
            assert_ne!(candidate.get("buildId"), Some(prior));
        } else {
            first = candidate.get("buildId").cloned();
        }
        let observed = normalize(&summary(&actual, result, None).await, &actual.root, "");
        let expected = normalize(
            &oracle(IvyJson::object([
                ("fixtureRoot", path(&fixture.0)),
                ("root", path(&original.root)),
                ("host", original.host.clone()),
                ("snapshot", original.snapshot.clone()),
                ("bootstrap", path(&original.bootstrap)),
                ("reuse", false.into()),
            ])),
            &original.root,
            "",
        );
        append(
            &observed,
            &expected,
            if hit {
                "Full original service preparation with warm cache and a new source build identity"
            } else {
                "Full original service preparation with cache retain, emitted package closure and publication"
            },
        );
        assert_eq!(
            json::canonical(&observed, usize::MAX).unwrap(),
            json::canonical(&expected, usize::MAX).unwrap()
        );
        assert_cache_lease_released(&native(actual.host.required("stagingRoot").unwrap())).await;
        if !hit {
            advance_service_plan(&actual);
            advance_service_plan(&original);
        }
    }
}
#[tokio::test]
async fn parser_failure_keeps_raw_error_code_message_progress_and_replaces_only_error_details() {
    let fixture = Fixture::new();
    let tools = tools();
    let actual = service_inputs(&fixture, "actual", &tools);
    let code = JsString(vec![102, 105, 120, 116, 117, 114, 101, 45, 0xd800]);
    let message = JsString(vec![109, 101, 115, 115, 97, 103, 101, 45, 0xdfff]);
    let failure = IvyError::new_js(code.clone(), message.clone())
        .with_outcome(Outcome::Unknown)
        .with_details("old details".into());
    let mut runtime = service_runtime(&tools.0, &actual);
    runtime.imports = Some(Arc::new(move |_, _| Err(failure.clone())));
    let error = prepare::prepare_candidate(
        actual.snapshot.clone(),
        "fixture".into(),
        actual.host.clone(),
        actual.bootstrap.clone(),
        runtime,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code_js(), code);
    assert_eq!(error.message_js(), message);
    assert_eq!(error.outcome, Outcome::Unknown);
    assert_eq!(
        error.details,
        Some(IvyJson::object([(
            "snapshotId",
            actual.snapshot.required("snapshotId").unwrap().clone()
        )]))
    );
    let observed = summary(&actual, Err(error), None).await;
    assert_eq!(
        observed.required("record").unwrap().get("errorCode"),
        Some(&IvyJson::String(code.clone()))
    );
    assert_eq!(
        observed.required("record").unwrap().get("phase"),
        Some(&"unknown".into())
    );
    let progress_root =
        native(actual.host.required("runtimeRoot").unwrap()).join("preparation-progress");
    let records: Vec<_> = fs::read_dir(progress_root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(records.len(), 1);
    let mut step = JsString::from("failed: ").0;
    step.extend(code.0);
    assert_eq!(
        load(&records[0]).get("step"),
        Some(&IvyJson::String(JsString(step)))
    );
    assert_cache_lease_released(&native(actual.host.required("stagingRoot").unwrap())).await;
    append(
        &normalize(&observed, &actual.root, ""),
        &IvyJson::Null,
        "Injected mandatory-parser failure: exact UTF-16 error, failure record/progress, snapshot details and released cache owner; not an original-parser differential case",
    );
}
fn normalize(value: &IvyJson, base: &Path, key: &str) -> IvyJson {
    if [
        "createdAt",
        "startedAt",
        "updatedAt",
        "observedAt",
        "stepStartedAt",
    ]
    .contains(&key)
    {
        assert!(value.as_js_string().is_some());
        return "<time>".into();
    }
    if key == "archiveHash" {
        return "<checked-archive-hash>".into();
    }
    match value {
        IvyJson::String(s) => s
            .to_utf8()
            .map(|text| {
                IvyJson::from(text.replace(&base.to_string_lossy().to_string(), "<fixture>"))
            })
            .unwrap_or_else(|_| IvyJson::String(s.clone())),
        IvyJson::Array(items) => {
            IvyJson::Array(items.iter().map(|v| normalize(v, base, "")).collect())
        }
        IvyJson::Object(items) => IvyJson::Object(
            items
                .iter()
                .map(|(k, v)| (k.clone(), normalize(v, base, &k.to_utf8().unwrap())))
                .collect(),
        ),
        _ => value.clone(),
    }
}
async fn summary(
    inputs: &Inputs,
    result: ivy_host_runtime::IvyResult<IvyJson>,
    reuse: Option<IvyJson>,
) -> IvyJson {
    let mut candidate = IvyJson::Null;
    let operation = match result {
        Ok(value) => {
            candidate = value.clone();
            IvyJson::object([("ok", true.into()), ("value", value)])
        }
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    };
    let mut saved = IvyJson::Null;
    let mut manifest = IvyJson::Null;
    let mut record = IvyJson::Null;
    let mut archive_valid = IvyJson::Null;
    let mut files = Vec::new();
    fn tree(root: &Path, directory: &Path, files: &mut Vec<(String, IvyJson)>) {
        for e in fs::read_dir(directory).unwrap() {
            let e = e.unwrap();
            let file = e.path();
            if e.file_type().unwrap().is_dir() {
                tree(root, &file, files);
            } else {
                let bytes = fs::read(&file).unwrap();
                let local = file
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/");
                files.push((
                    local.clone(),
                    IvyJson::object([
                        ("path", local.into()),
                        ("bytes", IvyJson::Number(bytes.len() as f64)),
                        ("hash", hash::digest(&bytes).into()),
                    ]),
                ));
            }
        }
    }
    if !candidate.is_null() {
        let journal = HostJournal::open(inputs.host.clone()).await.unwrap();
        let id = candidate.required("candidateId").unwrap().as_str().unwrap();
        saved = journal.candidate(&id).await.unwrap();
        manifest = journal.manifest(&id).await.unwrap();
        journal.close().await.unwrap();
        let build = candidate.required("buildId").unwrap().as_str().unwrap();
        let staging = native(inputs.host.required("stagingRoot").unwrap());
        record = preparations::preparation_record(
            staging,
            &preparations::preparation_id_for_build(&build).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        let root = native(candidate.required("artifactRoot").unwrap());
        tree(&root, &root, &mut files);
        let bytes = fs::read(native(candidate.required("archivePath").unwrap())).unwrap();
        archive_valid = IvyJson::Bool(
            !bytes.is_empty() && candidate.get("archiveHash") == Some(&hash::digest(&bytes).into()),
        );
    } else {
        let staging = native(inputs.host.required("stagingRoot").unwrap());
        if let Ok(entries) = fs::read_dir(staging) {
            for e in entries {
                let e = e.unwrap();
                let name = e.file_name();
                if name.to_string_lossy().starts_with("build-")
                    && name.to_string_lossy().len() == 42
                {
                    record = load(&e.path().join("preparation.json"));
                }
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    IvyJson::object([
        ("operation", operation),
        ("candidate", candidate),
        ("record", record),
        ("manifest", manifest),
        ("saved", saved),
        ("reused", reuse.unwrap_or(IvyJson::Null)),
        ("archiveValid", archive_valid),
        (
            "files",
            IvyJson::Array(files.into_iter().map(|(_, row)| row).collect()),
        ),
    ])
}
#[tokio::test]
async fn complete_app_preparation_publishes_archives_registers_compacts_and_reuses_the_same_candidate()
 {
    let fixture = Fixture::new();
    let tools = tools();
    let actual = inputs(&fixture, "actual", "normal", &tools);
    let original = inputs(&fixture, "original", "normal", &tools);
    let runtime = runtime(&tools.0, &actual);
    let result = prepare::prepare_candidate(
        actual.snapshot.clone(),
        "fixture".into(),
        actual.host.clone(),
        actual.bootstrap.clone(),
        runtime.clone(),
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    let reused = prepare::prepare_candidate(
        actual.snapshot.clone(),
        "fixture".into(),
        actual.host.clone(),
        actual.bootstrap.clone(),
        runtime,
    )
    .await
    .unwrap();
    let observed = summary(&actual, result, Some(reused)).await;
    let expected = oracle(IvyJson::object([
        ("fixtureRoot", path(&fixture.0)),
        ("root", path(&original.root)),
        ("host", original.host.clone()),
        ("snapshot", original.snapshot.clone()),
        ("bootstrap", path(&original.bootstrap)),
        ("reuse", true.into()),
    ]));
    let observed = normalize(&observed, &actual.root, "");
    let expected = normalize(&expected, &original.root, "");
    append(
        &observed,
        &expected,
        "Full original app preparation, archive, journal, compaction and candidate reuse",
    );
    assert_eq!(
        json::canonical(&observed, usize::MAX).unwrap(),
        json::canonical(&expected, usize::MAX).unwrap()
    );
    assert_eq!(
        observed.required("record").unwrap().get("phase"),
        Some(&"compacted".into())
    );
    assert_eq!(observed.get("archiveValid"), Some(&true.into()));
}
#[tokio::test]
async fn failed_build_keeps_original_unknown_record_and_releases_its_build_and_cache_owners() {
    let fixture = Fixture::new();
    let tools = tools();
    let actual = inputs(&fixture, "actual", "fail", &tools);
    let original = inputs(&fixture, "original", "fail", &tools);
    let runtime = runtime(&tools.0, &actual);
    let result = prepare::prepare_candidate(
        actual.snapshot.clone(),
        "fixture".into(),
        actual.host.clone(),
        actual.bootstrap.clone(),
        runtime,
    )
    .await;
    assert!(result.is_err());
    let observed = summary(&actual, result, None).await;
    let expected = oracle(IvyJson::object([
        ("fixtureRoot", path(&fixture.0)),
        ("root", path(&original.root)),
        ("host", original.host.clone()),
        ("snapshot", original.snapshot.clone()),
        ("bootstrap", path(&original.bootstrap)),
        ("reuse", false.into()),
    ]));
    let normalized = normalize(&observed, &actual.root, "");
    let expected = normalize(&expected, &original.root, "");
    append(
        &normalized,
        &expected,
        "Full original failed command, unknown preparation record and terminal ownership cleanup",
    );
    assert_eq!(
        json::canonical(&normalized, usize::MAX).unwrap(),
        json::canonical(&expected, usize::MAX).unwrap()
    );
    assert_eq!(
        normalized.required("record").unwrap().get("phase"),
        Some(&"unknown".into())
    );
    let staging = native(actual.host.required("stagingRoot").unwrap());
    let build = observed
        .required("record")
        .unwrap()
        .required("buildId")
        .unwrap()
        .as_str()
        .unwrap();
    let lock = ExecutorLock::acquire(staging.join("build-locks").join(&build[7..]), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
    assert_cache_lease_released(&staging).await;
}
#[tokio::test]
async fn dropping_the_build_waiter_retains_publication_and_terminal_owner_cleanup() {
    let fixture = Fixture::new();
    let tools = tools();
    let actual = inputs(&fixture, "actual", "gate", &tools);
    let runtime = runtime(&tools.0, &actual);
    let snapshot = actual.snapshot.clone();
    let host = actual.host.clone();
    let bootstrap = actual.bootstrap.clone();
    let work = tokio::spawn(async move {
        prepare::prepare_candidate(snapshot, "fixture".into(), host, bootstrap, runtime).await
    });
    let staging = native(actual.host.required("stagingRoot").unwrap());
    let mutable = tokio::time::timeout(Duration::from_secs(20), async {
        'found: loop {
            if let Ok(entries) = fs::read_dir(&staging) {
                for e in entries {
                    let e = e.unwrap();
                    let root = e.path().join("source");
                    if root.join("started").is_file() {
                        break 'found root;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    work.abort();
    let _ = work.await;
    fs::write(mutable.join("release"), b"release").unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let record = load(&mutable.parent().unwrap().join("preparation.json"));
            if record.get("phase") == Some(&"compacted".into()) {
                let build = record.required("buildId").unwrap().as_str().unwrap();
                if let Ok(lock) =
                    ExecutorLock::acquire(staging.join("build-locks").join(&build[7..]), false)
                        .await
                {
                    lock.close().await.unwrap();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!mutable.exists());
}
