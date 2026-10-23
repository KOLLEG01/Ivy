use super::{
    fixture::{ServiceFixture, host_configuration},
    parsed,
};
use ivy_contracts::IvyJson as J;
use ivy_host_runtime::{
    artifact, config, native_path, preparation_files,
    preparation_inputs::{self, PreparationProfile},
    source,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn target() -> String {
    format!(
        "{}-{}",
        if cfg!(windows) { "win32" } else { "linux" },
        if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        }
    )
}
fn write(root: &Path, relative: &str, bytes: &[u8]) {
    let file = root.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, bytes).unwrap();
}
fn declaration(files: &[&str]) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        (
            "targets",
            J::object([
                (
                    target(),
                    J::Array(files.iter().map(|path| J::from(*path)).collect()),
                ),
                (
                    if cfg!(windows) {
                        "linux-x64"
                    } else {
                        "win32-x64"
                    }
                    .to_owned(),
                    J::Array(vec![".local/other-target-missing".into()]),
                ),
            ]),
        ),
    ])
}
fn setup(f: &ServiceFixture) -> (PathBuf, J, PathBuf) {
    let checkout = f.0.join("checkout ü 空 😀; '");
    fs::create_dir(&checkout).unwrap();
    let git = std::env::var_os("IVY_TEST_GIT").unwrap_or_else(|| "git".into());
    let output = Command::new(&git)
        .args(["init", "--quiet"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    write(&checkout, ".gitignore", b".local/\n");
    write(
        &checkout,
        "package.json",
        br#"{"packageManager":"npm@11.0.0"}"#,
    );
    write(
        &checkout,
        "package-lock.json",
        br#"{"lockfileVersion":3,"packages":{}}"#,
    );
    let mut host = host_configuration(f);
    host.insert(
        "executables",
        J::object([("git", J::String(native_path::filesystem_name(&git)))]),
    );
    let staging = f.0.join("staging ü 空 😀; '");
    #[cfg(windows)]
    let staging = {
        use std::os::windows::ffi::OsStringExt;
        let mut name = staging.into_os_string();
        name.push(std::ffi::OsString::from_wide(&[0xd800]));
        PathBuf::from(name)
    };
    host.insert("stagingRoot", J::String(native_path::from_native(&staging)));
    let bootstrap = f.0.join("bootstrap");
    fs::create_dir_all(bootstrap.join("dist/native")).unwrap();
    #[cfg(windows)]
    fs::copy(
        std::env::var_os("IVY_BACKUP_JOB")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_ivy-job"))),
        bootstrap.join("dist/native/ivy-job.exe"),
    )
    .unwrap();
    (checkout, host, bootstrap)
}
async fn capture(
    root: &Path,
    host: &J,
    bootstrap: &Path,
    component: &str,
) -> ivy_host_runtime::IvyResult<J> {
    source::capture_component_source(
        native_path::from_native(root),
        host.clone(),
        bootstrap.to_owned(),
        component.into(),
    )
    .await
}
fn snapshot_root(snapshot: &J) -> PathBuf {
    native_path::to_native(
        snapshot
            .required("sourceRoot")
            .unwrap()
            .as_js_string()
            .unwrap(),
    )
}
async fn identity(root: PathBuf) -> preparation_inputs::PreparationIdentity {
    let (os, arch) = target()
        .split_once('-')
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .unwrap();
    preparation_inputs::preparation_identity_for_platform(
        root,
        "fixture".into(),
        "1.0.0".into(),
        "npm@11.0.0".into(),
        PreparationProfile {
            os: os.into(),
            arch: arch.into(),
            node: "v24.18.0".into(),
        },
        artifact::native_file_compare().unwrap(),
        Some(target().into()),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn component_capture_materializes_the_normal_native_build_source() {
    let f = ServiceFixture::new();
    let (_, host, bootstrap) = setup(&f);
    let checkout = std::env::var_os("IVY_HOST_SOURCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let declaration = parsed(
        &fs::read_to_string(checkout.join("services/task-board/build-inputs.json")).unwrap(),
    );
    let files = declaration
        .required("targets")
        .unwrap()
        .required(&target())
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(files.len(), 4);
    let snapshot = capture(&checkout, &host, &bootstrap, "task-board")
        .await
        .unwrap();
    let captured = snapshot_root(&snapshot);
    let output = std::env::var_os("IVY_TEST_CAPTURED_SOURCE_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| f.0.join("normal-build-source"));
    let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
    assert!(output.is_absolute() && !output.exists());
    assert!(
        fs::canonicalize(output.parent().unwrap())
            .unwrap()
            .starts_with(&temporary)
    );
    preparation_files::copy_source_tree(captured.clone(), output.clone())
        .await
        .unwrap();
    let mut hashes = Vec::new();
    for file in files {
        let relative = file.as_str().unwrap();
        let original = fs::read(checkout.join(&relative)).unwrap();
        assert_eq!(fs::read(captured.join(&relative)).unwrap(), original);
        assert_eq!(fs::read(output.join(&relative)).unwrap(), original);
        hashes.push(J::object([
            ("path", relative.into()),
            ("hash", ivy_contracts::hash::digest(&original).into()),
        ]));
    }
    for excluded in ["node_modules", ".git", "dist"] {
        assert!(!captured.join(excluded).exists());
        assert!(!output.join(excluded).exists());
    }
    for file in [
        "Cargo.toml",
        "Cargo.lock",
        "tools/build/build-deployment.mjs",
        "tools/build/build-inputs.mjs",
        "services/task-board/build-inputs.json",
    ] {
        assert_eq!(
            fs::read(output.join(file)).unwrap(),
            fs::read(checkout.join(file)).unwrap()
        );
    }
    let proof = J::object([
        ("output", J::String(native_path::from_native(&output))),
        ("runtimeInputs", J::Array(hashes)),
    ]);
    println!(
        "native-source-build={}",
        ivy_contracts::json::canonical(&proof, usize::MAX).unwrap()
    );
}

#[tokio::test]
async fn component_capture_selects_exact_runtime_bytes_and_invalidates_only_source_identity() {
    let f = ServiceFixture::new();
    let (checkout, host, bootstrap) = setup(&f);
    let runtime = format!(".local/native-runtime/{}/runtime.dll", target());
    write(&checkout, &runtime, b"first qualified bytes");
    write(&checkout, ".local/private/omit", b"not a build input");
    write(
        &checkout,
        ".local/native-runtime/other/omit",
        b"not selected",
    );
    config::atomic_json(
        checkout.join("services/fixture/build-inputs.json"),
        declaration(&[&runtime]),
    )
    .await
    .unwrap();
    // An unrelated component's missing runtime must not gate this capture.
    config::atomic_json(
        checkout.join("services/other/build-inputs.json"),
        declaration(&[".local/missing"]),
    )
    .await
    .unwrap();
    let first = capture(&checkout, &host, &bootstrap, "fixture")
        .await
        .unwrap();
    let first_root = snapshot_root(&first);
    assert_eq!(
        fs::read(first_root.join(&runtime)).unwrap(),
        b"first qualified bytes"
    );
    assert!(!first_root.join(".local/private").exists());
    assert!(!first_root.join(".local/native-runtime/other").exists());
    let copied = f.0.join("build-source");
    preparation_files::copy_source_tree(first_root.clone(), copied.clone())
        .await
        .unwrap();
    assert_eq!(
        fs::read(copied.join(&runtime)).unwrap(),
        b"first qualified bytes"
    );
    let first_identity = identity(first_root.clone()).await;
    write(&checkout, &runtime, b"new qualified bytes");
    let second = capture(&checkout, &host, &bootstrap, "fixture")
        .await
        .unwrap();
    let second_identity = identity(snapshot_root(&second)).await;
    assert_ne!(
        first_identity.source_identity,
        second_identity.source_identity
    );
    assert_ne!(first_identity.build_id, second_identity.build_id);
    assert_eq!(
        first_identity.dependency_key,
        second_identity.dependency_key
    );
    assert_eq!(
        first_identity.dependency_lock_hash,
        second_identity.dependency_lock_hash
    );
    assert_eq!(
        fs::read(first_root.join(&runtime)).unwrap(),
        b"first qualified bytes"
    );
    let unrelated = capture(&checkout, &host, &bootstrap, "without-declaration")
        .await
        .unwrap();
    assert!(!snapshot_root(&unrelated).join(".local").exists());
    let general = source::capture_source(native_path::from_native(&checkout), host, bootstrap)
        .await
        .unwrap();
    assert!(!snapshot_root(&general).join(".local").exists());
}

#[tokio::test]
async fn component_capture_refuses_missing_linked_and_malformed_inputs_before_snapshot_effects() {
    for kind in [
        "missing",
        "empty",
        "file-link",
        "parent-link",
        "declaration-link",
        "oversize",
        "declaration-oversize",
    ] {
        let f = ServiceFixture::new();
        let (checkout, host, bootstrap) = setup(&f);
        let runtime = ".local/runtime/value.dll";
        let declaration_path = checkout.join("services/fixture/build-inputs.json");
        config::atomic_json(&declaration_path, declaration(&[runtime]))
            .await
            .unwrap();
        if kind != "missing" {
            write(&checkout, runtime, b"runtime");
        }
        if kind == "empty" {
            fs::write(checkout.join(runtime), []).unwrap();
        }
        if kind == "oversize" {
            fs::OpenOptions::new()
                .write(true)
                .open(checkout.join(runtime))
                .unwrap()
                .set_len(64 * 1024 * 1024 + 1)
                .unwrap();
        }
        if kind == "declaration-oversize" {
            fs::write(&declaration_path, vec![b' '; 64 * 1024 + 1]).unwrap();
        }
        if ["file-link", "parent-link", "declaration-link"].contains(&kind) {
            let (from, to, directory) = if kind == "parent-link" {
                let from = f.0.join("runtime");
                fs::create_dir(&from).unwrap();
                fs::write(from.join("value.dll"), b"outside").unwrap();
                fs::remove_file(checkout.join(runtime)).unwrap();
                fs::remove_dir(checkout.join(".local/runtime")).unwrap();
                (from, checkout.join(".local/runtime"), true)
            } else {
                let to = if kind == "declaration-link" {
                    declaration_path.clone()
                } else {
                    checkout.join(runtime)
                };
                let from = f.0.join("linked-file");
                fs::rename(&to, &from).unwrap();
                (from, to, false)
            };
            #[cfg(windows)]
            if directory {
                std::os::windows::fs::symlink_dir(from, to).unwrap();
            } else {
                std::os::windows::fs::symlink_file(from, to).unwrap();
            }
            #[cfg(unix)]
            {
                let _ = directory;
                std::os::unix::fs::symlink(from, to).unwrap();
            }
        }
        assert!(
            capture(&checkout, &host, &bootstrap, "fixture")
                .await
                .is_err(),
            "{kind}"
        );
        assert!(
            !native_path::to_native(
                host.required("stagingRoot")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
            )
            .exists(),
            "{kind}"
        );
    }
    for value in [
        parsed(r#"{"schemaVersion":2,"targets":{}}"#),
        parsed(r#"{"schemaVersion":1,"targets":{},"extra":true}"#),
        parsed(r#"{"schemaVersion":1,"targets":{"unknown":[]}}"#),
        parsed(r#"{"schemaVersion":1,"targets":{"win32-x64":true}}"#),
    ] {
        let f = ServiceFixture::new();
        let (checkout, host, bootstrap) = setup(&f);
        config::atomic_json(checkout.join("services/fixture/build-inputs.json"), value)
            .await
            .unwrap();
        assert!(
            capture(&checkout, &host, &bootstrap, "fixture")
                .await
                .is_err()
        );
        assert!(
            !native_path::to_native(
                host.required("stagingRoot")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
            )
            .exists()
        );
    }
    for files in [
        vec!["../outside"],
        vec!["/absolute"],
        vec!["C:/absolute"],
        vec![".local/*.dll"],
        vec![".local//value"],
        vec![".local/./value"],
        vec![".local/value:stream"],
        vec![".local/value."],
        vec![".local/value", ".local/VALUE"],
    ] {
        let f = ServiceFixture::new();
        let (checkout, host, bootstrap) = setup(&f);
        config::atomic_json(
            checkout.join("services/fixture/build-inputs.json"),
            declaration(&files),
        )
        .await
        .unwrap();
        assert!(
            capture(&checkout, &host, &bootstrap, "fixture")
                .await
                .is_err()
        );
        assert!(
            !native_path::to_native(
                host.required("stagingRoot")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
            )
            .exists()
        );
    }
    for key in ["runtimeRoot", "artifactRoot", "stagingRoot", "configPath"] {
        for inside in [false, true] {
            if key == "configPath" && !inside {
                continue;
            }
            let f = ServiceFixture::new();
            let (checkout, mut host, bootstrap) = setup(&f);
            let private = if inside {
                checkout.join("private-state")
            } else {
                f.0.clone()
            };
            host.insert(key, J::String(native_path::from_native(&private)));
            let error =
                source::capture_source(native_path::from_native(&checkout), host, bootstrap)
                    .await
                    .unwrap_err();
            assert_eq!(error.code, "target_conflict", "{key}: inside={inside}");
            assert_eq!(
                fs::read(checkout.join("package.json")).unwrap(),
                br#"{"packageManager":"npm@11.0.0"}"#
            );
            assert!(!checkout.join("private-state").exists());
        }
    }
    // A supplied immutable snapshot is re-admitted before temporary build output.
    let f = ServiceFixture::new();
    let (checkout, host, bootstrap) = setup(&f);
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut plan =
        parsed(&fs::read_to_string(repository.join("services/host-executor/deploy.json")).unwrap());
    plan.insert("componentId", "fixture".into());
    let mut entrypoint = plan.required("entrypoint").unwrap().clone();
    entrypoint.insert("executable", "dist/native/ivy-fixture".into());
    entrypoint.insert("args", J::Array(Vec::new()));
    plan.insert("entrypoint", entrypoint);
    let mut readiness = plan.required("readiness").unwrap().clone();
    let mut command = readiness.required("command").unwrap().clone();
    command.insert("executable", "dist/native/ivy-health".into());
    command.insert("args", J::Array(Vec::new()));
    readiness.insert("command", command);
    plan.insert("readiness", readiness);
    config::atomic_json(checkout.join("services/fixture/deploy.json"), plan)
        .await
        .unwrap();
    config::atomic_json(
        checkout.join("services/fixture/build-inputs.json"),
        declaration(&[".local/missing"]),
    )
    .await
    .unwrap();
    let general = source::capture_source(
        native_path::from_native(&checkout),
        host.clone(),
        bootstrap.clone(),
    )
    .await
    .unwrap();
    let command_temp = native_path::to_native(
        host.required("stagingRoot")
            .unwrap()
            .as_js_string()
            .unwrap(),
    )
    .join("command-temp");
    let error = ivy_host_runtime::prepare::prepare_candidate(
        general,
        "fixture".into(),
        host,
        bootstrap,
        ivy_host_runtime::prepare::PreparationRuntime {
            node: None,
            compare: artifact::native_file_compare().unwrap(),
            http: ivy_host_runtime::portable_http::PortableHttp::new(
                reqwest::Client::builder().no_proxy(),
            )
            .unwrap(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "artifact_missing", "{error:?}");
    assert!(!command_temp.exists());
}
