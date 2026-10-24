use ivy_contracts::{IvyJson, core_schema::validate_host, json::parse};
use ivy_host_runtime::native_plan::materialize_native_plan;

fn source() -> IvyJson {
    let mut plan = parse(
        include_str!("../../services/agent-manager/deploy.json"),
        65536,
    )
    .unwrap();
    for key in ["entrypoint", "readiness"] {
        let mut command = IvyJson::object([
            (
                "executable",
                if key == "entrypoint" {
                    "dist/native/ivy-agent-manager".into()
                } else {
                    "dist/native/ivy-health".into()
                },
            ),
            (
                "args",
                IvyJson::Array(vec!["--config".into(), "{configPath}".into()]),
            ),
            ("timeoutMs", IvyJson::Number(5000.0)),
        ]);
        if key == "readiness" {
            command =
                IvyJson::object([("timeoutMs", IvyJson::Number(5000.0)), ("command", command)]);
        }
        plan.insert(key, command);
    }
    let mut requirements = plan.required("requirements").unwrap().clone();
    requirements.insert("os", IvyJson::Array(vec!["win32".into(), "linux".into()]));
    requirements.insert("arch", IvyJson::Array(vec!["x64".into(), "arm64".into()]));
    plan.insert("requirements", requirements);
    plan
}

#[test]
fn native_prepared_plan_matches_only_the_selected_artifact() {
    let original = source();
    for (os, arch) in [
        ("win32", "x64"),
        ("win32", "arm64"),
        ("linux", "x64"),
        ("linux", "arm64"),
    ] {
        let selected = materialize_native_plan(original.clone(), os, arch).unwrap();
        validate_host("BuildPlan", &selected).unwrap();
        let extension = if os == "win32" { ".exe" } else { "" };
        assert_eq!(
            selected
                .required("entrypoint")
                .unwrap()
                .required("executable")
                .unwrap(),
            &format!("dist/native/{os}-{arch}/ivy-agent-manager{extension}").into()
        );
        assert_eq!(
            selected
                .required("readiness")
                .unwrap()
                .required("command")
                .unwrap()
                .required("executable")
                .unwrap(),
            &format!("dist/native/{os}-{arch}/ivy-health{extension}").into()
        );
        assert_eq!(
            selected.required("requirements").unwrap().get("os"),
            Some(&IvyJson::Array(vec![os.into()]))
        );
        assert_eq!(
            selected.required("requirements").unwrap().get("arch"),
            Some(&IvyJson::Array(vec![arch.into()]))
        );
        assert_eq!(
            materialize_native_plan(selected.clone(), os, arch).unwrap(),
            selected
        );
        let other = if os == "win32" { "linux" } else { "win32" };
        assert!(materialize_native_plan(selected, other, arch).is_err());
    }
    assert_eq!(
        original.required("entrypoint").unwrap().get("executable"),
        Some(&"dist/native/ivy-agent-manager".into())
    );
}

#[test]
fn invalid_native_commands_fail_and_intentional_javascript_is_unchanged() {
    let mut node = source();
    let entry = IvyJson::object([
        ("executable", "node".into()),
        ("args", IvyJson::Array(vec!["worker.mjs".into()])),
        ("timeoutMs", IvyJson::Number(5000.0)),
    ]);
    node.insert("entrypoint", entry);
    assert_eq!(
        materialize_native_plan(node.clone(), "win32", "x64").unwrap(),
        node
    );
    let mut bad = source();
    bad.insert(
        "entrypoint",
        IvyJson::object([
            ("executable", "dist/native/../escape".into()),
            ("args", IvyJson::Array(vec![])),
        ]),
    );
    assert!(materialize_native_plan(bad, "win32", "x64").is_err());
    assert!(materialize_native_plan(source(), "darwin", "x64").is_err());
}

use ivy_contracts::{JsString, hash::hash_json};
use ivy_host_runtime::{
    artifact, candidate_identity, config, filesystem, native_path,
    node_runtime::NodeRuntime,
    preparation_inputs::{self, PreparationProfile},
    process, source,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-native-plan-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.0.parent() == fs::canonicalize(std::env::temp_dir()).ok().as_deref() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn os() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    }
}
fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}
fn platform() -> String {
    format!("{}-{}", os(), arch())
}
fn text(path: &Path) -> IvyJson {
    IvyJson::String(native_path::from_native(path))
}
fn assert_json(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        ivy_contracts::json::canonical(actual, usize::MAX).unwrap(),
        ivy_contracts::json::canonical(expected, usize::MAX).unwrap(),
    );
}
fn node(version: &str) -> NodeRuntime {
    NodeRuntime {
        executable: PathBuf::from("unused-node"),
        version: version.into(),
    }
}

#[tokio::test]
async fn source_capture_materializes_native_commands_without_rewriting_source() {
    let f = Fixture::new();
    let original = source();
    config::atomic_json(
        f.0.join("services/agent-manager/deploy.json"),
        original.clone(),
    )
    .await
    .unwrap();
    let selected = source::source_build_plan(f.0.clone(), "agent-manager")
        .await
        .unwrap();
    assert_json(
        &selected,
        &materialize_native_plan(original.clone(), os(), arch()).unwrap(),
    );
    assert_json(
        &config::json_file(f.0.join("services/agent-manager/deploy.json"), 65536)
            .await
            .unwrap(),
        &original,
    );
    config::atomic_json(f.0.join("ui/agent-manager/deploy.json"), original)
        .await
        .unwrap();
    assert_eq!(
        source::source_build_plan(f.0.clone(), "agent-manager")
            .await
            .unwrap_err()
            .code,
        "target_conflict"
    );
}

#[tokio::test]
async fn native_identity_separates_platforms_and_preserves_existing_recipe() {
    let f = Fixture::new();
    write(&f.0.join("package-lock.json"), b"{}");
    write(&f.0.join("source.rs"), b"fn main() {}");
    let mut builds = BTreeSet::new();
    let mut previous_source = None;
    for selected in ["win32-x64", "win32-arm64", "linux-x64", "linux-arm64"] {
        let identity = preparation_inputs::preparation_identity_for_platform(
            f.0.clone(),
            "agent-manager".into(),
            "1.0.0".into(),
            "npm@11.0.0".into(),
            PreparationProfile {
                os: os().into(),
                arch: arch().into(),
                node: "v24.18.0".into(),
            },
            Arc::new(|a, b| a.0.cmp(&b.0)),
            Some(selected.into()),
        )
        .await
        .unwrap();
        if let Some(previous) = &previous_source {
            assert_eq!(&identity.source_identity, previous);
        }
        previous_source = Some(identity.source_identity.clone());
        assert!(builds.insert(identity.build_id.clone()));
        assert_eq!(
            identity.build_id,
            hash_json(&IvyJson::object([
                ("schemaVersion", IvyJson::Number(3.0)),
                ("packageRecipe", IvyJson::Number(1.0)),
                ("componentId", "agent-manager".into()),
                ("version", "1.0.0".into()),
                ("sourceIdentity", identity.source_identity.into()),
                ("nativePlatform", selected.into())
            ]))
            .unwrap()
        );
    }
    let source_id = JsString::from(previous_source.unwrap());
    let legacy = candidate_identity::candidate_build_id(
        &"agent-manager".into(),
        &"1.0.0".into(),
        &source_id,
    )
    .unwrap();
    assert_eq!(
        legacy,
        hash_json(&IvyJson::object([
            ("schemaVersion", IvyJson::Number(3.0)),
            ("packageRecipe", IvyJson::Number(1.0)),
            ("componentId", "agent-manager".into()),
            ("version", "1.0.0".into()),
            ("sourceIdentity", IvyJson::String(source_id)),
        ]))
        .unwrap()
    );
    assert!(!builds.contains(&legacy));
}

async fn candidate_for(component: &str) -> (Fixture, IvyJson, IvyJson) {
    let f = Fixture::new();
    let root = f.0.join("artifact");
    let mut plan = materialize_native_plan(source(), os(), arch()).unwrap();
    plan.insert("componentId", component.into());
    let mut entrypoint = plan.required("entrypoint").unwrap().clone();
    entrypoint.insert(
        "executable",
        format!(
            "dist/native/{}/ivy-{component}{}",
            platform(),
            if cfg!(windows) { ".exe" } else { "" }
        )
        .into(),
    );
    plan.insert("entrypoint", entrypoint);
    let build = IvyJson::from(format!("sha256:{}", "a".repeat(64)));
    plan.insert("buildId", build.clone());
    let manifest = artifact::release_manifest(&plan).unwrap();
    let stamp = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("componentId", component.into()),
        ("version", manifest.required("version").unwrap().clone()),
        ("buildId", build.clone()),
    ]);
    config::atomic_json(root.join("dist/build-info.json"), stamp.clone())
        .await
        .unwrap();
    let directory = format!("dist/native/{}", platform());
    config::atomic_json(root.join(&directory).join("build-info.json"), stamp)
        .await
        .unwrap();
    for name in [
        format!("ivy-{component}{}", if cfg!(windows) { ".exe" } else { "" }),
        format!("ivy-health{}", if cfg!(windows) { ".exe" } else { "" }),
    ] {
        write(&root.join(&directory).join(name), b"native payload fixture");
    }
    for required in
        ivy_host_runtime::portable_native_files::portable_native_files_for_manifest(&manifest)
            .unwrap()
    {
        write(&root.join(required), b"required helper");
    }
    config::atomic_json(f.0.join("component.json"), manifest.clone())
        .await
        .unwrap();
    let candidate = IvyJson::object([
        ("candidateId", build.clone()),
        ("componentId", component.into()),
        ("buildId", build),
        ("artifactRoot", text(&root)),
        ("manifestPath", text(&f.0.join("component.json"))),
        ("createdAt", "2026-01-01T00:00:00.000Z".into()),
        (
            "platform",
            IvyJson::object([
                ("os", os().into()),
                ("arch", arch().into()),
                ("node", "24.18.0".into()),
            ]),
        ),
    ]);
    (f, candidate, manifest)
}

async fn candidate() -> (Fixture, IvyJson, IvyJson) {
    candidate_for("agent-manager").await
}

#[cfg(windows)]
#[tokio::test]
async fn native_phone_candidate_needs_its_job_helper_without_the_retired_input_dll() {
    let (f, candidate, manifest) = candidate_for("phone-bridge").await;
    let root = f.0.join("artifact");
    let directory = root.join(format!("dist/native/{}", platform()));
    assert!(!directory.join("ivy-phone-input.dll").exists());
    artifact::verify_native_candidate(candidate.clone(), f.0.clone())
        .await
        .unwrap();
    artifact::verify_launch_candidate_with_manifest(candidate.clone(), f.0.clone(), None, manifest)
        .await
        .unwrap();
    fs::remove_file(directory.join("ivy-job.exe")).unwrap();
    assert_eq!(
        artifact::verify_native_candidate(candidate, f.0.clone())
            .await
            .unwrap_err()
            .code,
        "artifact_missing"
    );
}

#[tokio::test]
async fn native_candidate_admits_direct_paths_without_node_and_runs_a_native_program() {
    let (f, candidate, manifest) = candidate().await;
    assert_json(
        &artifact::verify_native_candidate(candidate.clone(), f.0.clone())
            .await
            .unwrap(),
        &manifest,
    );
    // A mismatched/absent Node runtime cannot prevent direct native admission.
    artifact::verify_candidate(candidate.clone(), f.0.clone(), &node("0.0.0"))
        .await
        .unwrap();
    artifact::verify_launch_candidate_with_manifest(
        candidate.clone(),
        f.0.clone(),
        Some("dist/missing-node-argument.js".into()),
        manifest.clone(),
    )
    .await
    .unwrap();
    let root = f.0.join("artifact");
    let executable = manifest
        .required("entrypoint")
        .unwrap()
        .required("executable")
        .unwrap()
        .as_str()
        .unwrap();
    let actual = std::env::current_exe().unwrap();
    fs::copy(actual, root.join(&executable)).unwrap();
    let result = process::run_command(
        &process::Command {
            executable,
            args: vec!["--list".into()],
            cwd: None,
            timeout_ms: 10000,
        },
        &root,
        &BTreeMap::new(),
        process::LaunchContext {
            without_job: true,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert!(result.stdout.contains(
        "native_candidate_admits_direct_paths_without_node_and_runs_a_native_program: test"
    ));
    #[cfg(windows)]
    {
        let job = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ivy-job.exe");
        assert!(job.is_file(), "the selected runner must build ivy-job");
        let launcher = root.join(format!("dist/native/{}/ivy-job.exe", platform()));
        fs::copy(job, &launcher).unwrap();
        // Only the platform helper exists: the ordinary process default must
        // launch through it without Node or a caller-supplied helper override.
        assert!(!root.join("dist/native/ivy-job.exe").exists());
        let result = process::run_command(
            &process::Command {
                executable: manifest
                    .required("entrypoint")
                    .unwrap()
                    .required("executable")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                args: vec!["--list".into()],
                cwd: None,
                timeout_ms: 10000,
            },
            &root,
            &BTreeMap::new(),
            process::LaunchContext::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains(
            "native_candidate_admits_direct_paths_without_node_and_runs_a_native_program: test"
        ));
        // The same default Job owner must preserve explicitly requested shell
        // commands. Unnecessary quotes around switches change cmd's parsing.
        let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
        let executables = BTreeMap::from([("shell".into(), shell)]);
        for script in ["exit 0", "echo native-job-shell"] {
            let result = process::run_command(
                &process::Command {
                    executable: "shell".into(),
                    args: vec!["/d".into(), "/c".into(), script.into()],
                    cwd: None,
                    timeout_ms: 10000,
                },
                &root,
                &executables,
                process::LaunchContext::default(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.exit_code, Some(0), "{script}");
            if script.starts_with("echo") {
                assert!(result.stdout.contains("native-job-shell"));
            }
        }
        let failed = process::run_command(
            &process::Command {
                executable: "shell".into(),
                args: vec!["/d".into(), "/c".into(), "exit 7".into()],
                cwd: None,
                timeout_ms: 10000,
            },
            &root,
            &executables,
            process::LaunchContext::default(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(failed.code, "command_failed");
        assert_eq!(
            failed
                .details
                .as_ref()
                .and_then(|details| details.get("exitCode")),
            Some(&IvyJson::Number(7.0))
        );
        // run-npm starts a second Node from process.execPath. A verbatim
        // lpApplicationName made that real nested main-module load fail at C:.
        let source_root =
            fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")).unwrap();
        let node = fs::canonicalize(std::env::var_os("IVY_TEST_NODE").unwrap()).unwrap();
        let package = ivy_contracts::json::parse(
            &fs::read_to_string(source_root.join("package.json")).unwrap(),
            65536,
        )
        .unwrap();
        let package_manager = package
            .required("packageManager")
            .unwrap()
            .as_str()
            .unwrap();
        let expected = package_manager.strip_prefix("npm@").unwrap();
        for without_job in [true, false] {
            let result = process::run_command(
                &process::Command {
                    executable: "node".into(),
                    args: vec!["tools/build/run-npm.mjs".into(), "--version".into()],
                    cwd: None,
                    timeout_ms: 30000,
                },
                &source_root,
                &BTreeMap::from([("node".into(), node.clone())]),
                process::LaunchContext {
                    job_launcher: Some(PathBuf::from(env!("CARGO_BIN_EXE_ivy-job"))),
                    without_job,
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.stdout.trim(), expected, "without_job={without_job}");
        }
    }
}

#[tokio::test]
async fn native_candidate_rejects_missing_foreign_linked_and_mismatched_payloads() {
    for mode in [
        "entry",
        "health",
        "helper",
        "empty",
        "stamp",
        "build",
        "version",
        "foreign-platform",
        "requirements",
        "junction",
    ] {
        let (f, candidate, mut manifest) = candidate().await;
        let root = f.0.join("artifact");
        let main = artifact::launch_entrypoint(&manifest)
            .unwrap()
            .to_utf8()
            .unwrap();
        let directory = root.join(format!("dist/native/{}", platform()));
        match mode {
            "entry" => fs::remove_file(root.join(&main)).unwrap(),
            "health" => fs::remove_file(
                root.join(
                    manifest
                        .required("readiness")
                        .unwrap()
                        .required("command")
                        .unwrap()
                        .required("executable")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                ),
            )
            .unwrap(),
            "helper" => {
                if cfg!(windows) {
                    fs::remove_file(directory.join("ivy-job.exe")).unwrap();
                } else {
                    fs::remove_file(directory.join("build-info.json")).unwrap();
                }
                // A legacy helper must never satisfy a missing selected helper.
                write(&root.join("dist/native/ivy-job.exe"), b"legacy helper");
            }
            "empty" => fs::write(root.join(&main), b"").unwrap(),
            "stamp" => fs::remove_file(directory.join("build-info.json")).unwrap(),
            "build" | "version" => {
                let mut stamp = config::json_file(directory.join("build-info.json"), 65536)
                    .await
                    .unwrap();
                stamp.insert(
                    if mode == "build" {
                        "buildId"
                    } else {
                        "version"
                    },
                    if mode == "build" {
                        format!("sha256:{}", "b".repeat(64)).into()
                    } else {
                        "9.0.0".into()
                    },
                );
                config::atomic_json(directory.join("build-info.json"), stamp)
                    .await
                    .unwrap();
            }
            "foreign-platform" => {
                let mut command = manifest.required("entrypoint").unwrap().clone();
                command.insert(
                    "executable",
                    "dist/native/other-x64/ivy-agent-manager.exe".into(),
                );
                manifest.insert("entrypoint", command);
                write(
                    &root.join("dist/native/other-x64/ivy-agent-manager.exe"),
                    b"foreign",
                );
            }
            "requirements" => {
                let mut requirements = manifest.required("requirements").unwrap().clone();
                requirements.insert("os", IvyJson::Array(vec!["unavailable".into()]));
                manifest.insert("requirements", requirements);
            }
            "junction" => {
                let outside = f.0.join("outside");
                fs::rename(&directory, &outside).unwrap();
                filesystem::directory_link(&outside, &directory).unwrap();
            }
            _ => unreachable!(),
        }
        config::atomic_json(f.0.join("component.json"), manifest.clone())
            .await
            .unwrap();
        assert!(
            artifact::verify_native_candidate(candidate.clone(), f.0.clone())
                .await
                .is_err(),
            "{mode}"
        );
        if mode != "requirements" {
            assert!(
                artifact::verify_launch_candidate_with_manifest(
                    candidate,
                    f.0.clone(),
                    None,
                    manifest
                )
                .await
                .is_err(),
                "{mode} launch"
            );
        }
    }
}

#[tokio::test]
async fn intentional_javascript_keeps_its_entry_argument_and_runtime_requirement() {
    let (f, candidate, mut manifest) = candidate().await;
    let command = IvyJson::object([
        ("executable", "node".into()),
        ("args", IvyJson::Array(vec!["dist/worker.mjs".into()])),
        ("timeoutMs", IvyJson::Number(5000.0)),
    ]);
    manifest.insert("entrypoint", command.clone());
    let mut readiness = manifest.required("readiness").unwrap().clone();
    readiness.insert("command", command);
    manifest.insert("readiness", readiness);
    write(
        &f.0.join("artifact/dist/worker.mjs"),
        b"throw Error('preflight must not evaluate this file');",
    );
    write(
        &f.0.join("artifact/dist/native/ivy-job.exe"),
        b"legacy helper",
    );
    config::atomic_json(f.0.join("component.json"), manifest.clone())
        .await
        .unwrap();
    assert_eq!(
        artifact::launch_entrypoint(&manifest),
        Some(&JsString::from("dist/worker.mjs"))
    );
    assert!(
        artifact::verify_native_candidate(candidate.clone(), f.0.clone())
            .await
            .is_err()
    );
    assert!(
        artifact::verify_candidate(candidate.clone(), f.0.clone(), &node("0.0.0"))
            .await
            .is_err()
    );
    assert_json(
        &artifact::verify_candidate(candidate, f.0.clone(), &node("24.18.0"))
            .await
            .unwrap(),
        &manifest,
    );
}

#[tokio::test]
async fn native_archive_round_trip_checks_direct_commands_and_selected_helpers() {
    use ivy_host_runtime::package_archive;
    for mode in ["valid", "entry", "health", "helper", "javascript-health"] {
        let (f, candidate, mut manifest) = candidate().await;
        let root = f.0.join("artifact");
        match mode {
            "entry" | "health" => {
                let command = if mode == "entry" {
                    manifest.required("entrypoint").unwrap()
                } else {
                    manifest
                        .required("readiness")
                        .unwrap()
                        .required("command")
                        .unwrap()
                };
                fs::remove_file(
                    root.join(command.required("executable").unwrap().as_str().unwrap()),
                )
                .unwrap();
            }
            "helper" => {
                let files =
                    ivy_host_runtime::portable_native_files::portable_native_files_for_manifest(
                        &manifest,
                    )
                    .unwrap();
                if files.is_empty() {
                    continue;
                }
                fs::remove_file(root.join(&files[0])).unwrap();
                write(
                    &root.join("dist/native/ivy-job.exe"),
                    b"legacy helper cannot substitute",
                );
            }
            "javascript-health" => {
                let mut readiness = manifest.required("readiness").unwrap().clone();
                readiness.insert(
                    "command",
                    IvyJson::object([
                        ("executable", "node".into()),
                        (
                            "args",
                            IvyJson::Array(vec!["dist/missing-health.mjs".into()]),
                        ),
                        ("timeoutMs", IvyJson::Number(5000.0)),
                    ]),
                );
                manifest.insert("readiness", readiness);
                config::atomic_json(f.0.join("component.json"), manifest.clone())
                    .await
                    .unwrap();
            }
            _ => {}
        }
        let archive = package_archive::create_package_archive(candidate.clone(), BTreeMap::new())
            .await
            .unwrap();
        let extracted = f.0.join("received");
        let received = package_archive::validate_package_archive(
            archive.path,
            archive.hash,
            manifest.clone(),
            extracted.clone(),
            BTreeMap::new(),
        )
        .await;
        if mode == "valid" {
            received.unwrap();
            let mut admitted = candidate;
            admitted.insert("artifactRoot", text(&extracted.join("artifact")));
            admitted.insert("manifestPath", text(&extracted.join("component.json")));
            assert_json(
                &artifact::verify_native_candidate(admitted, extracted)
                    .await
                    .unwrap(),
                &manifest,
            );
        } else {
            assert!(received.is_err(), "{mode}");
        }
    }
}
