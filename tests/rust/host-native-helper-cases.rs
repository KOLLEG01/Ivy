use super::ServiceFixture;
use ivy_host_runtime::{
    paths,
    portable_native_files::{NativeHelperLayout as Layout, component_native_helper as select},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "win32-x64",
        ("windows", "aarch64") => "win32-arm64",
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        _ => panic!("unsupported native package target"),
    }
}

fn payload(root: &Path, target: bool, name: &str, bytes: &[u8]) -> PathBuf {
    let directory = root.join("dist/native");
    let file = if target {
        directory.join(platform()).join(name)
    } else {
        directory.join(name)
    };
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, bytes).unwrap();
    file
}

#[test]
fn explicit_layout_keeps_legacy_and_current_target_admission_separate() {
    let fixture = ServiceFixture::new();
    let root = paths::realpath(&fixture.0.join("artifact")).unwrap();
    let legacy = payload(&root, false, "ivy-job.exe", b"legacy");
    let target = payload(&root, true, "ivy-job.exe", b"current target");
    assert_eq!(
        select(&root, "ivy-job.exe", Layout::Legacy).unwrap(),
        legacy
    );
    assert_eq!(
        select(&root, "ivy-job.exe", Layout::Target).unwrap(),
        target
    );
    #[cfg(windows)]
    assert_eq!(
        select(
            &PathBuf::from(root.to_string_lossy().to_uppercase()),
            "ivy-job.exe",
            Layout::Target
        )
        .unwrap(),
        PathBuf::from(root.to_string_lossy().to_uppercase())
            .join("dist/native")
            .join(platform())
            .join("ivy-job.exe")
    );
    fs::remove_file(&target).unwrap();
    payload(&root, true, "unrelated-helper.exe", b"different executable");
    let other = root.join("dist/native/other-target/ivy-job.exe");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::write(other, b"wrong platform").unwrap();
    assert_eq!(
        select(&root, "ivy-job.exe", Layout::Target)
            .unwrap_err()
            .code,
        "artifact_missing"
    );
    assert_eq!(
        select(&root, "ivy-job.exe", Layout::Legacy).unwrap(),
        legacy
    );
    fs::remove_file(&legacy).unwrap();
    payload(&root, true, "ivy-job.exe", b"current target");
    assert_eq!(
        select(&root, "ivy-job.exe", Layout::Legacy)
            .unwrap_err()
            .code,
        "artifact_missing"
    );
    for name in [
        "",
        "../ivy-job.exe",
        "a/ivy-job.exe",
        "a\\ivy-job.exe",
        ".",
        "..",
        "/ivy-job.exe",
        "C:ivy-job.exe",
        "ivy-job.exe:stream",
    ] {
        assert_eq!(
            select(&root, name, Layout::Target).unwrap_err().code,
            "invalid_arguments",
            "{name}"
        );
    }
    assert_eq!(
        select(
            Path::new("relative-artifact"),
            "ivy-job.exe",
            Layout::Target
        )
        .unwrap_err()
        .code,
        "invalid_arguments"
    );
    assert_eq!(
        select(&root, &"x".repeat(129), Layout::Target)
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    fs::write(&target, b"").unwrap();
    assert_eq!(
        select(&root, "ivy-job.exe", Layout::Target)
            .unwrap_err()
            .code,
        "artifact_missing"
    );
    fs::remove_file(&target).unwrap();
    fs::create_dir(&target).unwrap();
    assert_eq!(
        select(&root, "ivy-job.exe", Layout::Target)
            .unwrap_err()
            .code,
        "artifact_missing"
    );
}

#[test]
fn helper_refuses_root_and_payload_directory_aliases_inside_or_outside_artifact() {
    for mode in ["root", "inside", "outside"] {
        let fixture = ServiceFixture::new();
        let root = paths::realpath(&fixture.0.join("artifact")).unwrap();
        payload(&root, true, "ivy-job.exe", b"selected helper");
        let (source, link, selected_root) = if mode == "root" {
            (
                root.clone(),
                fixture.0.join("root-alias"),
                fixture.0.join("root-alias"),
            )
        } else {
            let original = root.join("dist/native").join(platform());
            let source = if mode == "inside" {
                root.join("retained")
            } else {
                fixture.0.join("retained")
            };
            fs::rename(&original, &source).unwrap();
            (source, original, root.clone())
        };
        ivy_host_runtime::filesystem::directory_link(&source, &link).unwrap();
        let result = select(&selected_root, "ivy-job.exe", Layout::Target);
        // Remove only this test's link before fixture cleanup can follow it.
        #[cfg(windows)]
        fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        fs::remove_file(&link).unwrap();
        assert_eq!(result.unwrap_err().code, "artifact_missing", "{mode}");
    }
}

#[test]
fn helper_child() {
    if std::env::var_os("IVY_NATIVE_HELPER_CHILD").is_some() {
        println!("selected-current-target-owner");
    }
}

#[cfg(windows)]
#[tokio::test]
async fn selected_target_helper_uses_existing_job_owner_and_retirement_proof() {
    use ivy_host_runtime::process::{self, Command, LaunchContext};
    use std::{collections::BTreeMap, time::Duration};
    let fixture = ServiceFixture::new();
    let root = paths::realpath(&fixture.0.join("artifact")).unwrap();
    let target = root
        .join("dist/native")
        .join(platform())
        .join("ivy-job.exe");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_ivy-job"), &target).unwrap();
    // A wrong flat helper must not be opened or executed by this launch.
    payload(
        &root,
        false,
        "ivy-job.exe",
        b"never execute this flat helper",
    );
    let helper = select(&root, "ivy-job.exe", Layout::Target).unwrap();
    let command = Command {
        executable: "fixture".into(),
        args: vec![
            "--exact".into(),
            "native_helper_cases::helper_child".into(),
            "--nocapture".into(),
        ],
        cwd: None,
        timeout_ms: 10000,
    };
    let executables = BTreeMap::from([("fixture".into(), std::env::current_exe().unwrap())]);
    let mut owned = process::spawn(
        &command,
        &root,
        &executables,
        LaunchContext {
            environment: BTreeMap::from([("IVY_NATIVE_HELPER_CHILD".into(), "1".into())]),
            job_launcher: Some(helper),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    drop(owned.stdin.take());
    let result = tokio::time::timeout(Duration::from_secs(15), owned.completion.wait())
        .await
        .unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.error_code, None, "{result:?}");
    assert!(
        result.stdout.contains("selected-current-target-owner"),
        "{result:?}"
    );
    assert!(process::owned_process_tree_stopped(&owned).await.unwrap());
}
