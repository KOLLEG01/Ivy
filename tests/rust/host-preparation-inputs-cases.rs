use ivy_contracts::{JsString, json};
use ivy_host_runtime::{
    IvyJson, IvyResult,
    artifact::FileCompare,
    filesystem, node_path,
    preparation_inputs::{self as inputs, PreparationProfile},
};
use std::{
    cmp::Ordering,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
static CORPUS: Mutex<()> = Mutex::new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let temporary = std::env::temp_dir();
        let root = temporary.join(format!(
            "ivy-preparation-inputs-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(root.parent(), Some(temporary.as_path()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn package(&self, value: IvyJson) {
        self.write(
            "package.json",
            json::stringify(&value, usize::MAX).unwrap().as_bytes(),
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        #[cfg(windows)]
        fn writable(path: &Path) {
            if let Ok(m) = fs::symlink_metadata(path) {
                if m.file_type().is_symlink() {
                    return;
                }
                if m.is_dir() {
                    if let Ok(entries) = fs::read_dir(path) {
                        for entry in entries.flatten() {
                            writable(&entry.path());
                        }
                    }
                }
                if m.permissions().readonly() {
                    let mut p = m.permissions();
                    p.set_readonly(false);
                    let _ = fs::set_permissions(path, p);
                }
            }
        }
        #[cfg(windows)]
        writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn append(row: IvyJson) {
    if let Some(path) = std::env::var_os("IVY_HOST_PREPARATION_INPUTS_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
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
fn root() -> PathBuf {
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
fn wire(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn compare(input: IvyJson, result: IvyResult<IvyJson>) -> IvyJson {
    let actual = wire(result);
    let expected = oracle(&input);
    append(IvyJson::object([
        ("input", input),
        ("actual", actual.clone()),
        ("expected", expected.clone()),
    ]));
    assert_eq!(&actual, expected.required("operation").unwrap());
    actual
}
fn succeeds(value: &IvyJson) {
    assert_eq!(value.get("ok"), Some(&true.into()));
}
fn refuses(value: &IvyJson, code: &str) {
    assert_eq!(
        value
            .required("error")
            .unwrap()
            .required("data")
            .unwrap()
            .get("code"),
        Some(&code.into())
    );
}
fn input(operation: &str, fixture: &Fixture) -> IvyJson {
    IvyJson::object([
        ("operation", operation.into()),
        ("root", IvyJson::String(node_path::from_native(&fixture.0))),
    ])
}
fn manager(value: IvyJson) -> IvyJson {
    IvyJson::object([("packageManager", value)])
}
fn comparator() -> FileCompare {
    let paths: Vec<JsString> = vec![
        "package.json".into(),
        "package-lock.json".into(),
        "payload".into(),
    ];
    let input = IvyJson::object([
        ("operation", "order".into()),
        (
            "paths",
            IvyJson::Array(paths.iter().cloned().map(IvyJson::String).collect()),
        ),
    ]);
    let expected = oracle(&input);
    let rows = expected
        .required("operation")
        .unwrap()
        .required("value")
        .unwrap()
        .required("matrix")
        .unwrap();
    let matrix: Vec<Vec<_>> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap()
                .iter()
                .map(|n| match n.as_f64().unwrap() as i32 {
                    -1 => Ordering::Less,
                    0 => Ordering::Equal,
                    1 => Ordering::Greater,
                    _ => unreachable!(),
                })
                .collect()
        })
        .collect();
    append(IvyJson::object([("input", input), ("expected", expected)]));
    Arc::new(move |left, right| {
        matrix[paths
            .iter()
            .position(|p| p == left)
            .expect("unknown fixture key")][paths
            .iter()
            .position(|p| p == right)
            .expect("unknown fixture key")]
    })
}

#[test]
fn component_and_validated_plan_gates_keep_the_original_preflight_rules() {
    for component in [
        "sample",
        "a",
        "a0-",
        "",
        "Sample",
        "0bad",
        "a/b",
        "../sample",
        "ä",
        "a\n",
        "a_",
        "a b",
    ] {
        let input = IvyJson::object([
            ("operation", "component".into()),
            ("componentId", component.into()),
        ]);
        compare(
            input,
            inputs::validate_component_id(&component.into()).map(|_| IvyJson::Null),
        );
    }
    let mut plans = vec![
        IvyJson::object([
            ("kind", "native".into()),
            ("checks", IvyJson::Array(vec![])),
        ]),
        IvyJson::object([
            ("kind", "native".into()),
            ("checks", IvyJson::Array(vec!["check".into()])),
        ]),
    ];
    for (id, dist) in [
        ("sample", "dist/apps/sample"),
        ("other", "dist/apps/sample"),
        ("sample", "dist/apps/sample/../other"),
        ("sample", "dist/apps/sample/"),
        ("sample", "dist/apps/Sample"),
    ] {
        plans.push(IvyJson::object([
            ("kind", "app".into()),
            ("checks", IvyJson::Array(vec![])),
            (
                "app",
                IvyJson::object([("appId", id.into()), ("dist", dist.into())]),
            ),
        ]));
    }
    plans.push(IvyJson::object([
        ("kind", "app".into()),
        ("checks", IvyJson::Array(vec![])),
    ]));
    for plan in plans {
        let input = IvyJson::object([
            ("operation", "plan".into()),
            ("componentId", "sample".into()),
            ("plan", plan.clone()),
        ]);
        compare(
            input,
            inputs::validate_preparation_plan(&"sample".into(), &plan).map(|_| IvyJson::Null),
        );
    }
}

#[tokio::test]
async fn package_manager_accepts_only_pinned_string_values() {
    let values = vec![
        "npm@1.2.3".into(),
        "npm@01.002.3".into(),
        "npm@1.2".into(),
        "npm@1.2.3.4".into(),
        "NPM@1.2.3".into(),
        "npm@1.2.3\n".into(),
        "npm@1.2.3\r\n".into(),
        "npm@1.2.3 ".into(),
        "npm@１.2.3".into(),
        "npm@1..3".into(),
        "npm@1.2.-3".into(),
        "npm@1.2.3-beta".into(),
        IvyJson::Null,
        true.into(),
        IvyJson::Number(123.0),
        IvyJson::object(Vec::<(String, IvyJson)>::new()),
        IvyJson::object([("toString", "npm@1.2.3".into())]),
        IvyJson::Array(vec!["npm@1.2.3".into()]),
        IvyJson::Array(vec![IvyJson::Array(vec!["npm@1.2.3".into()])]),
        IvyJson::Array(vec!["npm@1.2.3".into(), IvyJson::Null]),
        IvyJson::Array(vec![]),
    ];
    for value in values {
        let fixture = Fixture::new();
        fixture.package(manager(value.clone()));
        let actual = inputs::preparation_package_manager(fixture.0.clone()).await;
        if matches!(value.as_str().as_deref(), Ok("npm@1.2.3" | "npm@01.002.3")) {
            assert_eq!(actual.unwrap(), value);
        } else {
            assert_eq!(actual.unwrap_err().code, "invalid_arguments");
        }
    }
    for value in [
        IvyJson::Null,
        IvyJson::Number(3.0),
        "npm@1.2.3".into(),
        IvyJson::Array(vec![]),
        IvyJson::object(Vec::<(String, IvyJson)>::new()),
    ] {
        let fixture = Fixture::new();
        fixture.package(value);
        compare(
            input("manager", &fixture),
            inputs::preparation_package_manager(fixture.0.clone()).await,
        );
    }
}

#[tokio::test]
async fn package_manager_reader_keeps_native_utf8_bounds_syntax_and_missing_distinct() {
    for kind in [
        "missing",
        "directory",
        "syntax",
        "bom",
        "unicode-unused",
        "invalid-utf8-unused",
        "invalid-utf8-version",
        "limit",
        "oversize",
    ] {
        let fixture = Fixture::new();
        match kind {
            "directory" => fs::create_dir(fixture.0.join("package.json")).unwrap(),
            "syntax" => fixture.write("package.json", b"{"),
            "bom" => fixture.write(
                "package.json",
                b"\xef\xbb\xbf{\"packageManager\":\"npm@1.2.3\"}",
            ),
            "unicode-unused" => fixture.package(IvyJson::object([
                ("packageManager", "npm@1.2.3".into()),
                ("unused", "ü 空 😀 �".into()),
            ])),
            "invalid-utf8-unused" => fixture.write(
                "package.json",
                b"{\"packageManager\":\"npm@1.2.3\",\"unused\":\"\xff\"}",
            ),
            "invalid-utf8-version" => {
                fixture.write("package.json", b"{\"packageManager\":\"npm@1.2.\xff\"}")
            }
            "limit" | "oversize" => {
                let head = "{\"packageManager\":\"npm@1.2.3\",\"padding\":\"";
                let bytes = 1024 * 1024 + usize::from(kind == "oversize");
                let body = format!("{head}{}\"}}", "x".repeat(bytes - head.len() - 2));
                assert_eq!(body.len(), bytes);
                fixture.write("package.json", body.as_bytes());
            }
            _ => (),
        }
        let result = inputs::preparation_package_manager(fixture.0.clone()).await;
        if kind == "limit" || kind == "unicode-unused" {
            assert_eq!(result.unwrap(), IvyJson::from("npm@1.2.3"));
        } else {
            assert_eq!(
                result.unwrap_err().code,
                if kind == "directory" || kind == "oversize" {
                    "invalid_arguments"
                } else {
                    "internal_error"
                },
            );
        }
    }
}

#[cfg(windows)]
#[tokio::test]
async fn package_manager_metadata_and_read_preserve_native_windows_filenames() {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let fixture = Fixture::new();
    fixture.write(
        "\u{fffd}-source/package.json",
        br#"{"packageManager":"npm@1.2.3"}"#,
    );
    let requested = fixture.0.join(OsString::from_wide(&[
        0xd800, 45, 115, 111, 117, 114, 99, 101,
    ]));
    assert_eq!(
        inputs::preparation_package_manager(requested.clone())
            .await
            .unwrap_err()
            .code,
        "internal_error"
    );
    fs::create_dir(&requested).unwrap();
    fs::create_dir(requested.join("package.json")).unwrap();
    assert_eq!(
        inputs::preparation_package_manager(requested.clone())
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    fs::remove_dir(requested.join("package.json")).unwrap();
    fs::write(
        requested.join("package.json"),
        br#"{"packageManager":"npm@9.8.7"}"#,
    )
    .unwrap();
    assert_eq!(
        inputs::preparation_package_manager(requested.clone())
            .await
            .unwrap(),
        IvyJson::from("npm@9.8.7")
    );
    fixture.write("\u{fffd}-source/package.json", &vec![b' '; 1024 * 1024 + 1]);
    assert_eq!(
        inputs::preparation_package_manager(requested.clone())
            .await
            .unwrap(),
        IvyJson::from("npm@9.8.7")
    );
    fs::write(requested.join("package.json"), vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert_eq!(
        inputs::preparation_package_manager(requested)
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
}

async fn identity(
    fixture: &Fixture,
    package_manager: IvyJson,
    profile: (&str, &str, &str),
    compare_paths: FileCompare,
) -> IvyJson {
    let mut input = input("identity", fixture);
    input.insert("componentId", "sample".into());
    input.insert("version", "1.2.3".into());
    input.insert("packageManager", package_manager.clone());
    input.insert(
        "profile",
        IvyJson::object([
            ("platform", profile.0.into()),
            ("arch", profile.1.into()),
            ("version", profile.2.into()),
        ]),
    );
    let value = inputs::preparation_identity(
        fixture.0.clone(),
        "sample".into(),
        "1.2.3".into(),
        package_manager,
        PreparationProfile {
            os: profile.0.into(),
            arch: profile.1.into(),
            node: profile.2.into(),
        },
        compare_paths,
    )
    .await
    .map(|v| v.json());
    compare(input, value)
}
#[tokio::test]
async fn preparation_identity_keeps_exact_runtime_coordinates_raw_managers_and_source_bytes() {
    let fixture = Fixture::new();
    fixture.write("package-lock.json", b"{\"lockfileVersion\":3}");
    fixture.write("payload", b"fixed source");
    let compare_paths = comparator();
    let mut results = Vec::new();
    for profile in [
        ("win32", "x64", "v24.18.0"),
        ("linux", "arm64", "v24.18.0"),
        ("win32", "x64", "24.18.0"),
        ("win32", "x64", "v24.19.0"),
    ] {
        fixture.package(manager("npm@1.2.3".into()));
        let package_manager = inputs::preparation_package_manager(fixture.0.clone())
            .await
            .unwrap();
        let result = identity(&fixture, package_manager, profile, compare_paths.clone()).await;
        succeeds(&result);
        results.push(result.required("value").unwrap().clone());
    }
    for value in &results[1..] {
        assert_eq!(
            value.get("sourceIdentity"),
            results[0].get("sourceIdentity")
        );
        assert_eq!(value.get("buildId"), results[0].get("buildId"));
        assert_ne!(value.get("dependencyKey"), results[0].get("dependencyKey"));
    }
    fixture.package(manager(IvyJson::Array(vec!["npm@1.2.3".into()])));
    let package_manager = inputs::preparation_package_manager(fixture.0.clone())
        .await
        .unwrap();
    let array = identity(
        &fixture,
        package_manager.clone(),
        ("win32", "x64", "v24.18.0"),
        compare_paths.clone(),
    )
    .await;
    assert_ne!(
        array.required("value").unwrap().get("dependencyKey"),
        results[0].get("dependencyKey")
    );
    fixture.write("package-lock.json", &vec![0xa9; 4 * 1024 * 1024 + 1]);
    succeeds(
        &identity(
            &fixture,
            package_manager,
            ("win32", "x64", "v24.18.0"),
            compare_paths,
        )
        .await,
    );
}

#[tokio::test]
async fn lock_hash_failure_precedes_inventory_refusal_and_existing_bytes_stay_untouched() {
    let fixture = Fixture::new();
    fixture.package(manager("npm@1.2.3".into()));
    let outside = fixture.0.with_extension("outside");
    assert_eq!(outside.parent(), fixture.0.parent());
    fs::create_dir(&outside).unwrap();
    struct Outside(PathBuf);
    impl Drop for Outside {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _outside = Outside(outside.clone());
    fs::write(outside.join("retained"), b"outside bytes").unwrap();
    filesystem::directory_link(&outside, &fixture.0.join("observed")).unwrap();
    let compare_paths = comparator();
    let package_manager = inputs::preparation_package_manager(fixture.0.clone())
        .await
        .unwrap();
    refuses(
        &identity(
            &fixture,
            package_manager.clone(),
            ("win32", "x64", "v24.18.0"),
            compare_paths.clone(),
        )
        .await,
        "internal_error",
    );
    fixture.write("package-lock.json", b"{}");
    refuses(
        &identity(
            &fixture,
            package_manager,
            ("win32", "x64", "v24.18.0"),
            compare_paths,
        )
        .await,
        "artifact_changed",
    );
    assert_eq!(
        fs::read(outside.join("retained")).unwrap(),
        b"outside bytes"
    );
}

#[test]
fn preparation_identity_survives_a_dropped_waiter_across_lock_and_inventory_phases() {
    use std::{
        future::Future,
        sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
        task::Poll,
        time::Duration,
    };
    let fixture = Fixture::new();
    fixture.package(manager("npm@1.2.3".into()));
    fixture.write("package-lock.json", b"{}");
    fixture.write("payload", b"same source bytes");
    let finished = Arc::new(AtomicBool::new(false));
    let flag = finished.clone();
    let compare_paths: FileCompare = Arc::new(move |left, right| {
        flag.store(true, AtomicOrdering::Release);
        left.0.cmp(&right.0)
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, hold) = std::sync::mpsc::channel();
        let gate = tokio::task::spawn_blocking(move || {
            let _ = entered.send(());
            hold.recv().unwrap();
        });
        waiting.await.unwrap();
        let mut operation = Box::pin(inputs::preparation_identity(
            fixture.0.clone(),
            "sample".into(),
            "1.2.3".into(),
            "npm@1.2.3".into(),
            PreparationProfile {
                os: "win32".into(),
                arch: "x64".into(),
                node: "v24.18.0".into(),
            },
            compare_paths,
        ));
        std::future::poll_fn(|context| match operation.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("work passed a held blocking worker"),
        })
        .await;
        drop(operation);
        release.send(()).unwrap();
        gate.await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !finished.load(AtomicOrdering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    assert_eq!(
        fs::read(fixture.0.join("payload")).unwrap(),
        b"same source bytes"
    );
    append(IvyJson::object([("scope","admitted lock hash/cache key/inventory/source identity work reaches final comparison after a dropped caller wait".into())]));
}
