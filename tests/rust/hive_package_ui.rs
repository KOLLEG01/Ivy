use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_cli::publish_ui::read_ui_bundle;
use ivy_contracts::{
    IvyError, IvyJson, JsString, Outcome,
    json::{parse, stringify},
};
use ivy_hive::{
    package_ui::UiPackageInstaller,
    packages::{
        PackageInstallation, PackageOptions, PackageRequest, PackageStorage, PackageUiInstallation,
    },
    ui_files::UiFiles,
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}
fn encode(value: &IvyJson) -> String {
    stringify(value, usize::MAX).unwrap()
}
fn array(value: &IvyJson) -> &[IvyJson] {
    value.as_array().unwrap()
}
fn tree(root: &Path) -> IvyJson {
    fn walk(root: &Path, directory: &Path, rows: &mut Vec<(String, IvyJson)>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            let key = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let is_directory = entry.file_type().unwrap().is_dir();
            let mut record = IvyJson::object([
                ("path", key.clone().into()),
                (
                    "kind",
                    if is_directory { "directory" } else { "file" }.into(),
                ),
            ]);
            if !is_directory {
                record.insert("data", STANDARD.encode(fs::read(&path).unwrap()).into());
            }
            rows.push((key, record));
            if is_directory {
                walk(root, &path, rows);
            }
        }
    }
    let mut rows = vec![];
    walk(root, root, &mut rows);
    rows.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    IvyJson::Array(rows.into_iter().map(|(_, row)| row).collect())
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-hive-package-ui-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let path = fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            path.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-hive-package-ui-")
        );
        fs::remove_dir_all(path).unwrap();
    }
}
struct Worker {
    files: PathBuf,
    directory: PathBuf,
    config: IvyJson,
    trace: Mutex<Vec<IvyJson>>,
}
impl PackageStorage for Worker {
    fn request(&self, mut request: IvyJson) -> PackageRequest {
        let method = request
            .get("request")
            .and_then(|r| r.get("method"))
            .or_else(|| request.get("action"))
            .unwrap()
            .as_str()
            .unwrap();
        let mut trace = self.trace.lock().unwrap();
        if let Some(mut frame) = request.remove("request") {
            assert_eq!(
                uuid::Uuid::parse_str(&frame.required("id").unwrap().as_str().unwrap())
                    .unwrap()
                    .get_version_num(),
                4
            );
            frame.remove("id");
            request.insert("request", frame);
        }
        trace.push(IvyJson::object([
            ("request", request),
            ("files", tree(&self.files)),
        ]));
        let fail = self.config.get("fail").cloned().unwrap_or(IvyJson::Null);
        let count = trace
            .iter()
            .filter(|row| {
                row.get("request")
                    .and_then(|r| r.get("request"))
                    .and_then(|r| r.get("method"))
                    == Some(&"contracts.register".into())
            })
            .count();
        let result = if fail == IvyJson::from(method.as_str())
            || (fail == "second-contract".into() && method == "contracts.register" && count == 2)
        {
            Err(IvyError::new(
                "fixture_failure",
                format!("Fixture worker rejected {method}."),
            )
            .with_outcome(Outcome::Unknown)
            .with_details(IvyJson::object([("method", method.clone().into())])))
        } else if method == "ui.pointers" {
            if let Some(change) = self.config.get("change").filter(|v| !v.is_null()) {
                fs::write(
                    self.directory
                        .join(change.required("path").unwrap().as_str().unwrap()),
                    STANDARD
                        .decode(change.required("bytes").unwrap().as_str().unwrap())
                        .unwrap(),
                )
                .unwrap();
            }
            Ok(
                match self.config.get("current").cloned().unwrap_or(IvyJson::Null) {
                    IvyJson::Null => IvyJson::Array(vec![]),
                    value => IvyJson::Array(vec![IvyJson::object([("current_release_id", value)])]),
                },
            )
        } else {
            let value = if method == "system.status" {
                if self.config.get("invalidEpoch") == Some(&true.into()) {
                    json("{}")
                } else {
                    IvyJson::object([("runtimeEpoch", "fixture-package-epoch".into())])
                }
            } else {
                IvyJson::object([("fixture", true.into())])
            };
            Ok(IvyJson::object([("value", value)]))
        };
        Box::pin(std::future::ready(result))
    }
}
fn definition() -> IvyJson {
    json(
        r#"{"metadata":{"uiId":"fixture-ui","displayName":"Fixture","description":"Package composition 💡","iconKey":"ui"},"entryPath":"index.html","requirements":{"hiveProtocol":1,"contracts":[],"services":[]},"dataContracts":[]}"#,
    )
}
fn contract(key: &str, foreign: bool) -> IvyJson {
    let mut value = json(
        r#"{"key":"fixture/contract","version":"1.0.0","owner":{"kind":"agent"},"mediaType":"text/plain","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}},"specMarkdown":"Fixture contract."}"#,
    );
    value.insert("key", key.into());
    if foreign {
        value.insert(
            "owner",
            json(r#"{"kind":"service","serviceName":"fixture"}"#),
        );
    }
    value
}
fn node(input: &IvyJson) -> IvyJson {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .args([
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/hive-package-ui-oracle.mjs",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(encode(input).as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json(&String::from_utf8(output.stdout).unwrap())
}
fn record(value: &IvyJson) {
    if let Some(path) = std::env::var_os("IVY_HIVE_PACKAGE_UI_CORPUS") {
        let _guard = CORPUS.get_or_init(|| Mutex::new(())).lock().unwrap();
        let line = encode(value) + "\n";
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }
}
static CORPUS: OnceLock<Mutex<()>> = OnceLock::new();
async fn compare(mode: &str) {
    let root = Root::new();
    let mut selected = definition();
    let mut manifest = json(
        r#"{"kind":"app","componentId":"fixture-ui","app":{"appId":"fixture-ui","dist":"dist/ui","entryPath":"index.html"}}"#,
    );
    let mut config = json(
        r#"{"current":null,"fail":null,"change":null,"milliseconds":null,"invalidEpoch":false,"initialFiles":[]}"#,
    );
    if matches!(
        mode,
        "contracts" | "foreign-owner" | "fail-register" | "fail-second-register"
    ) {
        selected.insert(
            "dataContracts",
            IvyJson::Array(vec![
                contract("fixture/first", false),
                contract("fixture/second", mode == "foreign-owner"),
            ]),
        );
    }
    let native = root.0.join("native");
    let reference = root.0.join("reference");
    for artifact in [&native, &reference] {
        let directory = artifact.join("dist/ui");
        fs::create_dir_all(directory.join("sub")).unwrap();
        fs::write(directory.join("ivy-ui.json"), encode(&selected)).unwrap();
        for (path, bytes) in [
            ("index.html", "<h1>Fixture</h1>"),
            ("main.js", "\u{feff}fixture();"),
            ("data.json", " {\"z\":-0,\"a\":\"\\ud800\"} "),
            ("sub/é.txt", "é"),
        ] {
            fs::write(directory.join(path), bytes).unwrap();
        }
        match mode {
            "missing-definition" => fs::remove_file(directory.join("ivy-ui.json")).unwrap(),
            "malformed-definition" => fs::write(directory.join("ivy-ui.json"), "{").unwrap(),
            "invalid-definition" => fs::write(directory.join("ivy-ui.json"), "{}").unwrap(),
            "missing-entry" => fs::remove_file(directory.join("index.html")).unwrap(),
            "unsupported-extension" => fs::write(directory.join("hidden.exe"), "fixture").unwrap(),
            _ => {}
        }
    }
    let directory = native.join("dist/ui");
    let files = root.0.join("native-files");
    let source_files = root.0.join("reference-files");
    let bundle = read_ui_bundle(&directory, &selected).ok();
    let mut initial = vec![];
    if let Some(bundle) = &bundle {
        if matches!(
            mode,
            "same-release" | "same-release-incomplete" | "same-release-corrupt"
        ) {
            config.insert("current", bundle.release_id.clone().into());
        }
        if mode == "previous" {
            config.insert("current", "previous-release".into());
        }
        if matches!(mode, "same-release-incomplete" | "same-release-corrupt") {
            let path = format!("fixture-ui/{}/index.html", bundle.release_id);
            initial.push(IvyJson::object([
                ("path", path.clone().into()),
                (
                    "bytes",
                    STANDARD
                        .encode(if mode == "same-release-corrupt" {
                            b"changed".as_slice()
                        } else {
                            b"<h1>Fixture</h1>".as_slice()
                        })
                        .into(),
                ),
            ]));
            fs::create_dir_all(files.join(&path).parent().unwrap()).unwrap();
            fs::write(
                files.join(path),
                if mode == "same-release-corrupt" {
                    b"changed".as_slice()
                } else {
                    b"<h1>Fixture</h1>".as_slice()
                },
            )
            .unwrap();
        }
    }
    config.insert("initialFiles", IvyJson::Array(initial));
    let failure = match mode {
        "fail-pointers" => Some("ui.pointers"),
        "fail-epoch" => Some("system.status"),
        "fail-register" => Some("contracts.register"),
        "fail-second-register" => Some("second-contract"),
        "fail-deploy" => Some("uis.deploy"),
        _ => None,
    };
    if let Some(failure) = failure {
        config.insert("fail", failure.into());
    }
    if mode == "invalid-epoch" {
        config.insert("invalidEpoch", true.into());
    }
    let change = match mode {
        "changed-asset" => Some(("main.js", "changed();")),
        "equivalent-json" => Some(("data.json", "{\"a\":\"\\ud800\",\"z\":0}")),
        "changed-json" => Some(("data.json", "{\"x\":2}")),
        _ => None,
    };
    if let Some((path, bytes)) = change {
        config.insert(
            "change",
            IvyJson::object([
                ("path", path.into()),
                ("bytes", STANDARD.encode(bytes).into()),
            ]),
        );
    }
    if mode == "invalid-manifest" {
        manifest.insert("kind", "service".into());
    }
    if mode == "manifest-id" {
        manifest.insert("componentId", "other".into());
    }
    if matches!(mode, "definition-id" | "entry-path" | "nested-dist") {
        let mut app = manifest.remove("app").unwrap();
        match mode {
            "definition-id" => {
                app.insert("appId", "other".into());
                manifest.insert("componentId", "other".into());
            }
            "entry-path" => app.insert("entryPath", "other.html".into()),
            _ => app.insert("dist", "dist/./ui/../ui".into()),
        }
        manifest.insert("app", app);
    }
    let principal = if mode == "principal-unicode" {
        JsString(vec![112, 0xd800, 0x00e9, 0xd83d, 0xdca1])
    } else {
        "fixture-principal".into()
    };
    let worker = Arc::new(Worker {
        files: files.clone(),
        directory,
        config: config.clone(),
        trace: Mutex::new(vec![]),
    });
    let options = PackageOptions {
        install_ui: Some(Arc::new(UiPackageInstaller::new(
            worker.clone(),
            Arc::new(UiFiles::new(&files)),
        ))),
        ..PackageOptions::default()
    };
    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64;
    let future: PackageInstallation = options.install_ui.unwrap().install(PackageUiInstallation {
        manifest: manifest.clone(),
        artifact_root: native,
        publisher_principal_id: principal.clone(),
        build_id: format!("sha256:{}", "a".repeat(64)).into(),
    });
    let result = match future.await {
        Ok(()) => IvyJson::object([("ok", true.into())]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    };
    let trace = IvyJson::Array(worker.trace.lock().unwrap().clone());
    let mutation = array(&trace)
        .iter()
        .filter_map(|row| {
            row.get("request")?
                .get("request")?
                .get("params")?
                .get("mutationId")
        })
        .next()
        .cloned();
    if let Some(mutation) = mutation {
        let value = mutation.as_str().unwrap();
        let millis = value.split(':').nth(1).unwrap().parse::<f64>().unwrap();
        assert!(
            millis >= start
                && millis
                    <= SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as f64
        );
        config.insert("milliseconds", IvyJson::Number(millis));
    }
    config.insert("artifactRoot", reference.to_str().unwrap().into());
    config.insert("filesRoot", source_files.to_str().unwrap().into());
    config.insert("manifest", manifest);
    config.insert("principal", IvyJson::String(principal));
    config.insert("buildId", format!("sha256:{}", "a".repeat(64)).into());
    let expected = node(&config);
    let actual = IvyJson::object([
        ("result", result),
        ("trace", trace),
        ("files", tree(&files)),
    ]);
    record(&IvyJson::object([
        ("mode", mode.into()),
        ("native", actual.clone()),
        ("original", expected.clone()),
    ]));
    assert_eq!(encode(&actual), encode(&expected), "{mode}");
}

#[tokio::test]
async fn original_package_ui_success_and_existing_release_order() {
    for mode in [
        "publish",
        "previous",
        "same-release",
        "same-release-incomplete",
        "same-release-corrupt",
        "contracts",
        "nested-dist",
        "principal-unicode",
    ] {
        compare(mode).await;
    }
}
#[tokio::test]
async fn original_package_ui_worker_failure_order_and_outcomes() {
    for mode in [
        "fail-pointers",
        "fail-epoch",
        "fail-register",
        "fail-second-register",
        "fail-deploy",
        "foreign-owner",
        "invalid-epoch",
    ] {
        compare(mode).await;
    }
}
#[tokio::test]
async fn original_package_ui_changes_after_scan_preserve_private_files() {
    for mode in ["changed-asset", "equivalent-json", "changed-json"] {
        compare(mode).await;
    }
}
#[tokio::test]
async fn original_package_ui_manifest_and_bundle_admission_order() {
    for mode in [
        "invalid-manifest",
        "manifest-id",
        "definition-id",
        "entry-path",
        "missing-definition",
        "malformed-definition",
        "invalid-definition",
        "missing-entry",
        "unsupported-extension",
    ] {
        compare(mode).await;
    }
}
