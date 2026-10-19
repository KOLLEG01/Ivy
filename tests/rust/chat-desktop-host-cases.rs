use ivy_chat_bridge::whatsapp::{
    desktop::{DesktopHost, DesktopInvocation},
    desktop_host::NativeDesktopHost,
};
use ivy_contracts::{IvyJson as Json, JsString, json::parse};
use ivy_host_runtime::portable_native_files::NativeHelperLayout;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!("ivy-chat-desktop-host-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    ivy_host_runtime::paths::realpath(&path).unwrap()
}
fn host(root: &Path) -> NativeDesktopHost {
    #[cfg(windows)]
    {
        let helper = std::env::var_os("IVY_CHAT_DESKTOP_JOB_HELPER")
            .expect("Supply the matching, source-pinned Host ivy-job qualification image");
        let native = root.join("dist/native");
        fs::create_dir_all(&native).unwrap();
        fs::copy(&helper, native.join("ivy-job.exe")).unwrap();
        assert_eq!(
            NativeDesktopHost::for_component(root.to_path_buf(), NativeHelperLayout::Target)
                .err()
                .unwrap()
                .code,
            "artifact_missing"
        );
        let target = native.join(if cfg!(target_arch = "x86_64") {
            "win32-x64"
        } else {
            "win32-arm64"
        });
        fs::create_dir(&target).unwrap();
        fs::copy(helper, target.join("ivy-job.exe")).unwrap();
    }
    NativeDesktopHost::for_component(root.to_path_buf(), NativeHelperLayout::Target).unwrap()
}
fn invocation(receipt: &Path, mode: &str, timeout_ms: u64) -> DesktopInvocation {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/rust/fixtures/chat-desktop-host-child.mjs");
    DesktopInvocation {
        executable: "node",
        arguments: vec![
            ivy_host_runtime::node_path::from_native(&script),
            ivy_host_runtime::node_path::from_native(receipt),
            mode.into(),
            ivy_host_runtime::node_path::from_native(&receipt.with_extension("release")),
            "".into(),
            "spaces and \"quotes\"".into(),
            "trailing\\".into(),
            "€😀".into(),
        ],
        windows_hide: true,
        shell: false,
        ignored_stdio: true,
        timeout_ms,
    }
}
async fn read_receipt(file: &Path) -> Json {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = fs::read_to_string(file) {
                if let Ok(value) = parse(&text, usize::MAX) {
                    return value;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn assert_retired(receipt: &Json) {
    let pids =
        ["pid", "descendant"].map(|key| receipt.required(key).unwrap().as_f64().unwrap() as u32);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let observed = Command::new("node")
                .args(["-e", "process.exit(process.argv.slice(1).some(v=>{try{process.kill(Number(v),0);return true}catch(e){if(e.code==='ESRCH')return false;throw e}})?1:0)"])
                .args(pids.map(|pid|pid.to_string()))
                .output().unwrap();
            if observed.status.success() { break; }
            assert_eq!(observed.status.code(),Some(1),"{}",String::from_utf8_lossy(&observed.stderr));
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.unwrap();
}

#[tokio::test]
async fn ignored_io_lookup_arguments_and_terminal_failures() {
    let root = root();
    let host = host(&root);
    let file = root.join("probe.json");
    host.run(invocation(&file, "probe", 10000)).await.unwrap();
    let proof = read_receipt(&file).await;
    assert_eq!(
        proof.get("cwd"),
        Some(&Json::String(ivy_host_runtime::node_path::from_native(
            &std::env::current_dir().unwrap()
        )))
    );
    assert_eq!(proof.get("stdinBytes"), Some(&Json::Number(0.0)));
    assert_eq!(
        proof.get("args"),
        Some(&Json::Array(
            ["", "spaces and \"quotes\"", "trailing\\", "€😀"]
                .map(|s| Json::String(JsString::from(s)))
                .into()
        ))
    );
    for device in proof.required("devices").unwrap().as_array().unwrap() {
        assert_eq!(device.get("character"), Some(&Json::Bool(true)));
        assert_eq!(device.get("pipe"), Some(&Json::Bool(false)));
    }
    assert_eq!(
        host.run(invocation(&file, "exit", 10000))
            .await
            .unwrap_err()
            .code,
        "internal_error"
    );
    let mut missing = invocation(&file, "probe", 10000);
    missing.executable = "ivy-chat-desktop-intentionally-missing-executable";
    assert_eq!(host.run(missing).await.unwrap_err().code, "internal_error");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn deadline_retires_synthetic_tree() {
    let root = root();
    let host = host(&root);
    let file = root.join("deadline.json");
    assert_eq!(
        host.run(invocation(&file, "tree", 5000))
            .await
            .unwrap_err()
            .code,
        "internal_error"
    );
    let proof = read_receipt(&file).await;
    assert_retired(&proof).await;
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn dropped_observer_keeps_synthetic_completion_owned() {
    let root = root();
    let host = host(&root);
    let file = root.join("observer.json");
    // Drop before the join future is polled; the accepted native task still owns
    // launch, its deadline, completion and descendant retirement.
    drop(host.run(invocation(&file, "tree", 10000)));
    let proof = read_receipt(&file).await;
    fs::write(file.with_extension("release"), b"release").unwrap();
    assert_retired(&proof).await;
    fs::remove_dir_all(root).unwrap();
}
