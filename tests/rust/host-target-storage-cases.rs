use ivy_contracts::{IvyJson, JsString, hash, json};
use ivy_host_runtime::{
    IvyResult, accepted_configuration, filesystem, journal::HostJournal, node_path,
    service_storage, target,
};
use ivy_store::rusqlite::Connection;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};
static CORPUS: Mutex<()> = Mutex::new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-target-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let file = self.0.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, bytes).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn path(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn native(value: &IvyJson) -> PathBuf {
    node_path::filesystem_path(value.as_js_string().unwrap())
}
fn operation(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn source_root() -> PathBuf {
    std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        PathBuf::from,
    )
}
fn oracle(input: &IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-target-storage-oracle.mjs",
        ])
        .current_dir(source_root())
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
    if let Some(file) = std::env::var_os("IVY_HOST_TARGET_STORAGE_CORPUS") {
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
fn assert_pair(case: &str, actual: &IvyJson, expected: &IvyJson) {
    append(case, actual, expected);
    assert_eq!(
        json::canonical(actual, usize::MAX).unwrap(),
        json::canonical(expected, usize::MAX).unwrap(),
        "{case}"
    );
}
fn replace(text: &JsString, needle: &JsString, replacement: &JsString) -> JsString {
    let mut result = Vec::new();
    let mut at = 0;
    while at < text.0.len() {
        if text.0[at..].starts_with(&needle.0) {
            result.extend(&replacement.0);
            at += needle.0.len();
        } else {
            result.push(text.0[at]);
            at += 1;
        }
    }
    JsString(result)
}
fn normalize(value: &IvyJson, root: &Path, revisions: &[JsString], key: &str) -> IvyJson {
    if key == "requestedAt" {
        assert!(
            value
                .as_js_string()
                .unwrap()
                .to_utf8()
                .unwrap()
                .ends_with('Z')
        );
        return "<time>".into();
    }
    match value {
        IvyJson::String(text) => {
            let mut text = replace(text, &node_path::from_native(root), &"<root>".into());
            for revision in revisions {
                text = replace(&text, revision, &"<revision>".into());
            }
            IvyJson::String(text)
        }
        IvyJson::Array(values) => IvyJson::Array(
            values
                .iter()
                .map(|value| normalize(value, root, revisions, ""))
                .collect(),
        ),
        IvyJson::Object(values) => IvyJson::Object(
            values
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        normalize(value, root, revisions, &key.to_utf8().unwrap_or_default()),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn normalized(value: &IvyJson, root: &Path) -> IvyJson {
    let mut revisions = Vec::new();
    for row in value.required("written").unwrap().as_array().unwrap() {
        let revision = row
            .required("revision")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone();
        assert_eq!(
            uuid::Uuid::parse_str(&revision.to_utf8().unwrap())
                .unwrap()
                .get_version_num(),
            4
        );
        revisions.push(revision);
    }
    if let Some(result) = value.get("operation").and_then(|v| v.get("value")) {
        if let Some(revision) = result.get("revision").and_then(IvyJson::as_js_string) {
            assert_eq!(
                uuid::Uuid::parse_str(&revision.to_utf8().unwrap())
                    .unwrap()
                    .get_version_num(),
                4
            );
            revisions.push(revision.clone());
        }
    }
    normalize(value, root, &revisions, "")
}
fn observed(operation: IvyJson) -> IvyJson {
    IvyJson::object([
        ("operation", operation),
        ("written", IvyJson::Array(vec![])),
        ("accepted", IvyJson::Array(vec![])),
        ("selected", IvyJson::Null),
    ])
}
struct Inputs {
    root: PathBuf,
    host: IvyJson,
    candidate: IvyJson,
    manifest: IvyJson,
    home: JsString,
}
fn inputs(fixture: &Fixture, name: &str, component: &str) -> Inputs {
    let root = fixture.0.join(name);
    fs::create_dir_all(root.join("artifact/native")).unwrap();
    fs::write(
        root.join("artifact/native/runtime.exe"),
        b"selected native bytes",
    )
    .unwrap();
    let settings = if component == "phone-bridge" {
        IvyJson::object([
            (
                "native",
                IvyJson::object([
                    ("executable", "native/runtime.exe".into()),
                    (
                        "executableHash",
                        format!("sha256:{}", "0".repeat(64)).into(),
                    ),
                    ("retained", "native option".into()),
                ]),
            ),
            ("retained", IvyJson::String(JsString(vec![97, 0xd800, 98]))),
        ])
    } else {
        IvyJson::object([("retained", "new settings".into())])
    };
    let host = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", "fixture-host".into()),
        ("runtimeRoot", path(&root.join("runtime"))),
        ("artifactRoot", path(&root.join("artifacts"))),
        ("stagingRoot", path(&root.join("staging"))),
        ("publicBaseUrl", "http://127.0.0.1:39081/ivy".into()),
        (
            "executables",
            IvyJson::object([("node", path(&root.join("fixture-node.exe")))]),
        ),
        (
            "instances",
            IvyJson::Array(vec![IvyJson::object([
                ("instanceId", "logical-ü-😀".into()),
                ("serviceNodeId", "fixture-node".into()),
                ("componentId", component.into()),
                ("enabled", true.into()),
                ("engine", "process".into()),
                ("credential", "synthetic-current".into()),
                ("settings", settings),
            ])]),
        ),
    ]);
    let build = format!("sha256:{}", "a".repeat(64));
    let candidate = IvyJson::object([
        ("candidateId", build.clone().into()),
        ("componentId", component.into()),
        ("buildId", build.clone().into()),
        ("artifactRoot", path(&root.join("artifact"))),
        ("manifestPath", path(&root.join("component.json"))),
        ("createdAt", "2026-01-01T00:00:00.000Z".into()),
        (
            "platform",
            IvyJson::object([
                ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                ("arch", "x64".into()),
                ("node", "v24.18.0".into()),
            ]),
        ),
    ]);
    let command = IvyJson::object([
        ("executable", "node".into()),
        ("args", IvyJson::Array(vec!["dist/main.js".into()])),
        ("timeoutMs", IvyJson::Number(10000.0)),
    ]);
    let manifest = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("componentId", component.into()),
        ("kind", "service".into()),
        ("buildId", build.into()),
        ("version", "1.2.3".into()),
        ("connectsToHive", false.into()),
        (
            "requirements",
            IvyJson::object([
                ("node", ">=24.18.0 <25.0.0".into()),
                ("hiveProtocol", IvyJson::Null),
                ("contracts", IvyJson::Array(vec![])),
            ]),
        ),
        ("entrypoint", command.clone()),
        (
            "readiness",
            IvyJson::object([
                ("timeoutMs", IvyJson::Number(10000.0)),
                ("command", command),
            ]),
        ),
        (
            "shutdown",
            IvyJson::object([("timeoutMs", IvyJson::Number(5000.0))]),
        ),
        (
            "restart",
            IvyJson::object([
                ("policy", "never".into()),
                ("minimumDelayMs", IvyJson::Number(100.0)),
                ("maximumDelayMs", IvyJson::Number(1000.0)),
            ]),
        ),
    ]);
    Inputs {
        root,
        host,
        candidate,
        manifest,
        home: node_path::from_native(&fixture.0.join("explicit-home")),
    }
}
fn instance(host: &IvyJson) -> &IvyJson {
    &host.required("instances").unwrap().as_array().unwrap()[0]
}
fn set_instance(host: &mut IvyJson, key: &str, value: IvyJson) {
    let mut instance = instance(host).clone();
    instance.insert(key, value);
    host.insert("instances", IvyJson::Array(vec![instance]));
}
fn common(fixture: &Fixture, input: &Inputs, operation: &str) -> IvyJson {
    IvyJson::object([
        ("fixtureRoot", path(&fixture.0)),
        ("home", IvyJson::String(input.home.clone())),
        ("operation", operation.into()),
        ("host", input.host.clone()),
        (
            "instanceId",
            instance(&input.host)
                .required("instanceId")
                .unwrap()
                .clone(),
        ),
    ])
}
fn files(input: &Inputs, operation: IvyJson) -> IvyJson {
    let runtime = native(input.host.required("runtimeRoot").unwrap());
    let id = instance(&input.host)
        .required("instanceId")
        .unwrap()
        .as_js_string()
        .unwrap();
    let directory = node_path::filesystem_path(&node_path::join(
        node_path::Style::native(),
        &[
            node_path::from_native(&runtime),
            "instances".into(),
            id.clone(),
            "configurations".into(),
        ],
    ));
    let mut written = Vec::new();
    if directory.exists() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            written.push(IvyJson::object([
                ("revision", entry.file_name().to_str().unwrap().into()),
                (
                    "config",
                    json::parse(
                        &fs::read_to_string(entry.path().join("config.json")).unwrap(),
                        usize::MAX,
                    )
                    .unwrap(),
                ),
            ]));
        }
    }
    let mut accepted = Vec::new();
    let directory = runtime.join("accepted-configurations");
    if directory.is_dir() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            accepted.push(IvyJson::object([
                (
                    "revision",
                    entry
                        .file_name()
                        .to_str()
                        .unwrap()
                        .strip_suffix(".instance.json")
                        .unwrap()
                        .into(),
                ),
                (
                    "config",
                    json::parse(&fs::read_to_string(entry.path()).unwrap(), usize::MAX).unwrap(),
                ),
            ]));
        }
    }
    IvyJson::object([
        ("operation", operation),
        ("written", IvyJson::Array(written)),
        ("accepted", IvyJson::Array(accepted)),
        ("selected", IvyJson::Null),
    ])
}
async fn materialize(
    fixture: &Fixture,
    input: &Inputs,
    original: &Inputs,
    enabled: bool,
    basis: Option<IvyJson>,
    original_basis: Option<IvyJson>,
    case: &str,
) -> IvyJson {
    let journal = HostJournal::open(input.host.clone()).await.unwrap();
    journal
        .save_candidate(input.candidate.clone(), input.manifest.clone())
        .await
        .unwrap();
    let id = instance(&input.host)
        .required("instanceId")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let candidate = input
        .candidate
        .required("candidateId")
        .unwrap()
        .as_str()
        .unwrap();
    let result = target::materialize_target(
        journal.clone(),
        id.clone(),
        candidate,
        enabled,
        basis,
        input.home.clone(),
    )
    .await
    .map(|value| IvyJson::object([("target", value.target), ("config", value.config)]));
    let selected: i64 = Connection::open(
        native(input.host.required("runtimeRoot").unwrap()).join("deployments.sqlite"),
    )
    .unwrap()
    .query_row("SELECT COUNT(*) FROM runtime_targets", [], |row| row.get(0))
    .unwrap();
    assert_eq!(selected, 0);
    journal.close().await.unwrap();
    let actual = files(input, operation(result));
    let mut expected_input = common(fixture, original, "materialize");
    for (key, value) in [
        ("candidate", original.candidate.clone()),
        ("manifest", original.manifest.clone()),
        ("enabled", enabled.into()),
    ] {
        expected_input.insert(key, value);
    }
    if let Some(basis) = original_basis {
        expected_input.insert("basis", basis);
    }
    let expected = oracle(&expected_input);
    let actual = normalized(&actual, &input.root);
    let expected = normalized(&expected, &original.root);
    assert_pair(case, &actual, &expected);
    actual
}
#[tokio::test]
async fn immutable_targets_match_original_account_infrastructure_phone_settings_and_running_stopped_selection()
 {
    for (component, enabled) in [
        ("fixture", true),
        ("host-executor", false),
        ("service-manager", true),
        ("agent-manager", false),
        ("phone-bridge", true),
    ] {
        let fixture = Fixture::new();
        let actual = inputs(&fixture, "actual", component);
        let original = inputs(&fixture, "original", component);
        let result =
            materialize(&fixture, &actual, &original, enabled, None, None, component).await;
        assert_eq!(
            result.required("operation").unwrap().get("ok"),
            Some(&true.into())
        );
        assert_eq!(
            result
                .required("written")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            result.required("written").unwrap(),
            result.required("accepted").unwrap()
        );
    }
    for component in ["host-executor", "service-manager", "agent-manager"] {
        let fixture = Fixture::new();
        let mut actual = inputs(&fixture, "actual", component);
        let mut original = inputs(&fixture, "original", component);
        for input in [&mut actual, &mut original] {
            let settings = if component == "agent-manager" {
                IvyJson::object([
                    (
                        "appServer",
                        IvyJson::object([("mode", "claude-adapter".into())]),
                    ),
                    ("codexHome", path(&input.root.join("claude-home"))),
                ])
            } else {
                IvyJson::object([(
                    "hostConfigPath",
                    path(&input.root.join("explicit-host.json")),
                )])
            };
            set_instance(&mut input.host, "settings", settings);
        }
        let result = materialize(
            &fixture,
            &actual,
            &original,
            true,
            None,
            None,
            "explicit infrastructure paths / Claude defaults",
        )
        .await;
        assert_eq!(
            result.required("operation").unwrap().get("ok"),
            Some(&true.into())
        );
    }
    for component in ["host-executor", "service-manager"] {
        let fixture = Fixture::new();
        let mut actual = inputs(&fixture, "actual", component);
        let mut original = inputs(&fixture, "original", component);
        for input in [&mut actual, &mut original] {
            let config_path = node_path::join(
                node_path::Style::native(),
                &[
                    node_path::from_native(&input.root),
                    JsString(vec![0xd800, 46, 106, 115, 111, 110]),
                ],
            );
            input
                .host
                .insert("configPath", IvyJson::String(config_path));
        }
        let result = materialize(
            &fixture,
            &actual,
            &original,
            true,
            None,
            None,
            "infrastructure defaults preserve the raw UTF-16 configuration path",
        )
        .await;
        assert_eq!(
            result.required("operation").unwrap().get("ok"),
            Some(&true.into())
        );
    }
}
fn basis(input: &Inputs) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", input.host.required("hostId").unwrap().clone()),
        (
            "instanceId",
            instance(&input.host)
                .required("instanceId")
                .unwrap()
                .clone(),
        ),
        ("serviceNodeId", "accepted-old-node".into()),
        (
            "componentId",
            instance(&input.host)
                .required("componentId")
                .unwrap()
                .clone(),
        ),
        ("publicBaseUrl", "http://127.0.0.1:39082/previous".into()),
        ("dataRoot", path(&input.root.join("old-data"))),
        ("workRoot", path(&input.root.join("old-work"))),
        ("logsRoot", path(&input.root.join("old-logs"))),
        ("artifactRoot", path(&input.root.join("old-artifact"))),
        ("buildId", format!("sha256:{}", "b".repeat(64)).into()),
        ("version", "0.9.0".into()),
        ("credential", "synthetic-accepted-old".into()),
        (
            "settings",
            instance(&input.host).required("settings").unwrap().clone(),
        ),
    ])
}
#[tokio::test]
async fn target_ids_preserve_utf16_while_sqlite_and_filesystem_use_original_utf8_boundaries() {
    for id in [
        JsString(vec![114, 97, 119, 45, 0xd800]),
        JsString(vec![114, 97, 119, 45, 0xdfff]),
        JsString(vec![114, 97, 119, 0]),
    ] {
        let fixture = Fixture::new();
        let mut actual = inputs(&fixture, "actual", "fixture");
        let mut original = inputs(&fixture, "original", "fixture");
        for input in [&mut actual, &mut original] {
            set_instance(&mut input.host, "instanceId", IvyJson::String(id.clone()));
        }
        let observed = materialize(
            &fixture,
            &actual,
            &original,
            true,
            None,
            None,
            "UTF-16 ID: original SQL text / filesystem boundaries",
        )
        .await;
        if !id.0.contains(&0) {
            assert_eq!(
                observed.required("operation").unwrap().get("ok"),
                Some(&true.into())
            );
            let written = &observed.required("written").unwrap().as_array().unwrap()[0];
            assert_eq!(
                written.required("config").unwrap().get("instanceId"),
                Some(&IvyJson::String(id))
            );
        } else {
            assert_eq!(
                observed.required("operation").unwrap().get("ok"),
                Some(&false.into())
            );
        }
    }
}
#[tokio::test]
async fn rollback_targets_retain_the_complete_accepted_basis_and_rebind_only_selected_release_identity()
 {
    for component in ["fixture", "phone-bridge"] {
        let fixture = Fixture::new();
        let actual = inputs(&fixture, "actual", component);
        let original = inputs(&fixture, "original", component);
        let b = basis(&actual);
        let ob = basis(&original);
        let result = materialize(
            &fixture,
            &actual,
            &original,
            false,
            Some(b),
            Some(ob),
            "accepted rollback configuration",
        )
        .await;
        assert_eq!(
            result.required("operation").unwrap().get("ok"),
            Some(&true.into())
        );
        let config = result
            .required("operation")
            .unwrap()
            .required("value")
            .unwrap()
            .required("config")
            .unwrap();
        assert_eq!(
            config.get("credential"),
            Some(&"synthetic-accepted-old".into())
        );
        assert_eq!(
            config.get("serviceNodeId"),
            Some(&"accepted-old-node".into())
        );
        assert_eq!(config.get("version"), Some(&"1.2.3".into()));
    }
}
#[tokio::test]
async fn mismatched_basis_is_rejected_before_output_and_snapshot_failure_keeps_the_original_partial_file()
 {
    for kind in ["hostId", "instanceId", "componentId", "snapshot-file"] {
        let fixture = Fixture::new();
        let actual = inputs(&fixture, "actual", "fixture");
        let original = inputs(&fixture, "original", "fixture");
        let (mut b, mut ob) = (basis(&actual), basis(&original));
        let (b, ob) = if kind == "snapshot-file" {
            for input in [&actual, &original] {
                let root = native(input.host.required("runtimeRoot").unwrap());
                fs::create_dir_all(&root).unwrap();
                fs::write(root.join("accepted-configurations"), b"occupied").unwrap();
            }
            (None, None)
        } else {
            b.insert(kind, "wrong".into());
            ob.insert(kind, "wrong".into());
            (Some(b), Some(ob))
        };
        let result = materialize(&fixture, &actual, &original, true, b, ob, kind).await;
        assert_eq!(
            result.required("operation").unwrap().get("ok"),
            Some(&false.into())
        );
        assert_eq!(
            result
                .required("written")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            usize::from(kind == "snapshot-file")
        );
    }
}
#[tokio::test]
async fn native_settings_resolve_and_hash_only_the_selected_artifact_and_preserve_raw_utf16() {
    let fixture = Fixture::new();
    fixture.write("candidate/nested/runtime.exe", b"native bytes");
    fixture.write("outside/escape.exe", b"outside bytes");
    filesystem::directory_link(
        &fixture.0.join("outside"),
        &fixture.0.join("candidate/escape"),
    )
    .unwrap();
    for executable in [
        IvyJson::from("nested/runtime.exe"),
        "escape/escape.exe".into(),
        "../outside/escape.exe".into(),
        "nested".into(),
        "missing".into(),
        "".into(),
        path(&fixture.0.join("outside/escape.exe")),
        IvyJson::Null,
    ] {
        let settings = IvyJson::object([
            (
                "native",
                IvyJson::object([
                    ("executable", executable),
                    ("executableHash", "old hash".into()),
                    ("raw", IvyJson::String(JsString(vec![0xd800]))),
                ]),
            ),
            ("raw", IvyJson::String(JsString(vec![0xdfff]))),
        ]);
        let mut input = IvyJson::object([
            ("fixtureRoot", path(&fixture.0)),
            ("home", path(&fixture.0.join("home"))),
            ("operation", "settings".into()),
            ("component", "phone-bridge".into()),
            ("settings", settings.clone()),
            ("artifactRoot", path(&fixture.0.join("candidate"))),
        ]);
        let result = target::candidate_bound_settings(
            "phone-bridge".into(),
            settings.clone(),
            fixture.0.join("candidate"),
        )
        .await;
        if let Ok(value) = &result {
            assert_eq!(
                value.required("native").unwrap().get("executableHash"),
                Some(&hash::digest(b"native bytes").into())
            );
        }
        let actual = observed(operation(result));
        let expected = oracle(&input);
        assert_pair("release-owned phone native executable", &actual, &expected);
        input.insert("component", "fixture".into());
        let actual = observed(operation(
            target::candidate_bound_settings(
                "fixture".into(),
                settings.clone(),
                fixture.0.join("absent"),
            )
            .await,
        ));
        assert_pair(
            "other component settings unchanged",
            &actual,
            &oracle(&input),
        );
    }
    assert_eq!(
        fs::read(fixture.0.join("outside/escape.exe")).unwrap(),
        b"outside bytes"
    );
}
#[tokio::test]
async fn stopped_targets_replace_only_launch_nonce_desire_and_time() {
    let fixture = Fixture::new();
    let input_target = IvyJson::object([
        ("instanceId", "fixture".into()),
        ("candidateId", "retained candidate".into()),
        ("configPath", "retained config".into()),
        ("revision", "previous launch".into()),
        ("desired", "running".into()),
        ("requestedAt", "previous time".into()),
        ("retained", IvyJson::String(JsString(vec![0xd800]))),
    ]);
    let input = IvyJson::object([
        ("fixtureRoot", path(&fixture.0)),
        ("home", path(&fixture.0.join("home"))),
        ("operation", "stopped".into()),
        ("target", input_target.clone()),
    ]);
    let actual = observed(operation(Ok(target::stopped_target(&input_target))));
    assert_pair(
        "stopped target retains selection",
        &normalized(&actual, &fixture.0),
        &normalized(&oracle(&input), &fixture.0),
    );
    assert_eq!(
        input_target.get("revision"),
        Some(&"previous launch".into())
    );
}
#[tokio::test]
async fn admitted_materialization_finishes_both_files_after_its_waiter_is_dropped() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
        time::Duration,
    };
    let fixture = Fixture::new();
    let input = inputs(&fixture, "actual", "fixture");
    let journal = HostJournal::open(input.host.clone()).await.unwrap();
    journal
        .save_candidate(input.candidate.clone(), input.manifest.clone())
        .await
        .unwrap();
    let mut waiter = Box::pin(target::materialize_target(
        journal.clone(),
        instance(&input.host)
            .required("instanceId")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        input
            .candidate
            .required("candidateId")
            .unwrap()
            .as_str()
            .unwrap(),
        true,
        None,
        input.home.clone(),
    ));
    poll_fn(|cx| {
        assert!(matches!(waiter.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    drop(waiter);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let directory =
                native(input.host.required("runtimeRoot").unwrap()).join("accepted-configurations");
            let ready = fs::read_dir(directory)
                .ok()
                .and_then(|mut rows| rows.next())
                .and_then(Result::ok)
                .and_then(|row| fs::read_to_string(row.path()).ok())
                .and_then(|text| json::parse(&text, usize::MAX).ok())
                .is_some();
            if ready {
                let observed = files(&input, operation(Ok(IvyJson::Null)));
                assert_eq!(observed.get("written"), observed.get("accepted"));
                let revision = observed.required("accepted").unwrap().as_array().unwrap()[0]
                    .required("revision")
                    .unwrap()
                    .as_str()
                    .unwrap();
                let accepted = accepted_configuration::accepted_instance_configuration_path(
                    &input.host,
                    &revision,
                )
                .unwrap();
                assert_eq!(
                    accepted_configuration::read_accepted_instance_configuration(
                        &input.host,
                        &accepted
                    )
                    .unwrap(),
                    observed.required("written").unwrap().as_array().unwrap()[0]
                        .required("config")
                        .unwrap()
                        .clone()
                );
                append(
                    "cancelled waiter retains config and accepted snapshot",
                    &observed,
                    &IvyJson::Null,
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    journal.close().await.unwrap();
}
fn storage_plan(component: &str, contract: Option<IvyJson>) -> IvyJson {
    let mut value = IvyJson::object([("componentId", component.into())]);
    if let Some(contract) = contract {
        value.insert("storage", contract);
    }
    value
}
fn contract(min: f64, max: f64, write: f64) -> IvyJson {
    IvyJson::object([
        ("minReadableFormat", IvyJson::Number(min)),
        ("maxReadableFormat", IvyJson::Number(max)),
        ("writeFormat", IvyJson::Number(write)),
    ])
}
async fn compare_storage(
    fixture: &Fixture,
    input: &Inputs,
    plan: IvyJson,
    witness: Option<f64>,
    case: &str,
) -> IvyJson {
    let mut expected_input = common(fixture, input, "storage");
    expected_input.insert("plan", plan.clone());
    if let Some(witness) = witness {
        expected_input.insert("witness", IvyJson::Number(witness));
    }
    let result = service_storage::verify_service_storage(
        input.host.clone(),
        instance(&input.host)
            .required("instanceId")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        plan,
        witness,
    )
    .await;
    let actual = observed(operation(result.map(|()| IvyJson::Null)));
    assert_pair(case, &actual, &oracle(&expected_input));
    actual
}
fn db_path(input: &Inputs) -> PathBuf {
    let paths =
        ivy_host_runtime::layout::service_paths(&input.host, instance(&input.host)).unwrap();
    native(paths.required("data").unwrap()).join("phone-commands.sqlite")
}
#[tokio::test]
async fn service_storage_checks_missing_contract_before_existence_and_keeps_unrelated_journals_untouched()
 {
    let fixture = Fixture::new();
    let input = inputs(&fixture, "actual", "phone-bridge");
    for component in ["other", "phone-bridge", "constructor", "toString"] {
        for storage in [
            None,
            Some(IvyJson::Null),
            Some(false.into()),
            Some(contract(1.0, 3.0, 2.0)),
        ] {
            compare_storage(
                &fixture,
                &input,
                storage_plan(component, storage),
                None,
                "component table and missing contract order",
            )
            .await;
        }
    }
    assert!(!db_path(&input).exists());
}
#[tokio::test]
async fn service_storage_requires_the_exact_write_format_and_never_changes_the_database() {
    for format in [0, 1, 2, 3, -1] {
        let fixture = Fixture::new();
        let input = inputs(&fixture, "actual", "phone-bridge");
        let file = db_path(&input);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let db = Connection::open(&file).unwrap();
        db.execute_batch(&format!("PRAGMA user_version={format}"))
            .unwrap();
        drop(db);
        let before = fs::read(&file).unwrap();
        for storage in [
            contract(1.0, 3.0, 2.0),
            contract(2.0, 2.0, 2.0),
            contract(1.0, 4.0, 3.0),
            IvyJson::object([
                ("minReadableFormat", IvyJson::Number(1.0)),
                ("maxReadableFormat", IvyJson::Number(3.0)),
                ("writeFormat", "2".into()),
            ]),
        ] {
            compare_storage(
                &fixture,
                &input,
                storage_plan("phone-bridge", Some(storage)),
                None,
                "readable interval and exact numeric write format",
            )
            .await;
            assert_eq!(fs::read(&file).unwrap(), before);
        }
    }
}
#[tokio::test]
async fn locked_storage_uses_only_an_explicit_running_witness_and_corruption_does_not_use_it() {
    let fixture = Fixture::new();
    let input = inputs(&fixture, "actual", "phone-bridge");
    let file = db_path(&input);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let owner = Connection::open(&file).unwrap();
    owner
        .execute_batch("PRAGMA user_version=2;PRAGMA locking_mode=EXCLUSIVE;BEGIN EXCLUSIVE")
        .unwrap();
    for witness in [None, Some(2.0), Some(3.0)] {
        for storage in [contract(2.0, 2.0, 2.0), contract(3.0, 3.0, 3.0)] {
            compare_storage(
                &fixture,
                &input,
                storage_plan("phone-bridge", Some(storage)),
                witness,
                "real exclusive SQLite owner / explicit running format",
            )
            .await;
        }
    }
    owner.execute_batch("ROLLBACK").unwrap();
    drop(owner);
    let bytes = b"not a SQLite database";
    fs::write(&file, bytes).unwrap();
    for witness in [None, Some(2.0)] {
        let observed = compare_storage(
            &fixture,
            &input,
            storage_plan("phone-bridge", Some(contract(2.0, 2.0, 2.0))),
            witness,
            "corrupt data cannot use a running format witness",
        )
        .await;
        assert_eq!(
            observed.required("operation").unwrap().get("ok"),
            Some(&false.into())
        );
        assert_eq!(fs::read(&file).unwrap(), bytes);
    }
}
