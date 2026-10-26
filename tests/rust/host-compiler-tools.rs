//! Real filtered compiler execution, including the missing stddef.h regression.
#[path = "host-service-fixture.rs"]
#[allow(dead_code)]
mod fixture;
use fixture::ServiceFixture;
use ivy_contracts::IvyJson as J;
use ivy_host_runtime::{
    node_path,
    node_runtime::NodeRuntime,
    preparation_tools,
    process::{self, Command, LaunchContext},
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command as Child,
};

fn tool(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).expect(name))
}
fn helpers() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}
async fn tools(root: &Path) -> preparation_tools::PreparationTools {
    let executables = J::object(
        [
            ("cargo", tool("IVY_TEST_CARGO")),
            ("rustc", tool("IVY_TEST_RUSTC")),
            ("cl", tool("IVY_TEST_CL")),
        ]
        .map(|(key, value)| (key, J::String(node_path::from_native(&value)))),
    );
    preparation_tools::preparation_tools(executables, root.to_path_buf(), root.to_path_buf(), None)
        .await
        .unwrap()
}
async fn run(
    root: &Path,
    executable: PathBuf,
    args: Vec<OsString>,
    environment: BTreeMap<OsString, OsString>,
) -> String {
    let child = process::spawn(
        &Command {
            executable: "selected".into(),
            args,
            cwd: None,
            timeout_ms: 60000,
        },
        root,
        &BTreeMap::from([("selected".into(), executable)]),
        LaunchContext {
            environment,
            job_launcher: Some(helpers().join("ivy-job.exe")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), child.completion.wait())
        .await
        .unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert!(result.error_code.is_none(), "{result:?}");
    result.stdout.clone()
}
async fn probe() {
    let f = ServiceFixture::new();
    fs::create_dir_all(f.0.join("dist/native")).unwrap();
    fs::copy(
        helpers().join("ivy-job.exe"),
        f.0.join("dist/native/ivy-job.exe"),
    )
    .unwrap();
    let selected_node = f.0.join("selected node runtime/node.exe");
    fs::create_dir_all(selected_node.parent().unwrap()).unwrap();
    fs::copy(tool("IVY_TEST_NODE"), &selected_node).unwrap();
    fs::write(
        f.0.join("engine-requirements.js"),
        "process.stdout.write(process.execPath);",
    )
    .unwrap();
    let runtime = NodeRuntime {
        executable: tool("IVY_TEST_NODE"),
        version: "24.18.0".into(),
    };
    let absent = preparation_tools::preparation_tools(
        J::object([] as [(&str, J); 0]),
        f.0.clone(),
        f.0.clone(),
        None,
    )
    .await
    .unwrap();
    assert!(
        !absent
            .environment
            .contains_key(std::ffi::OsStr::new("PATH"))
    );
    for configured in [true, false] {
        let mappings = if configured {
            J::object([("node", J::String(node_path::from_native(&selected_node)))])
        } else {
            J::object([] as [(&str, J); 0])
        };
        let selected = preparation_tools::preparation_tools(
            mappings,
            f.0.clone(),
            f.0.clone(),
            Some(runtime.clone()),
        )
        .await
        .unwrap();
        let actual = run(
            &f.0,
            PathBuf::from(std::env::var_os("ComSpec").unwrap()),
            ["/d", "/s", "/c", "node ./engine-requirements.js"]
                .map(OsString::from)
                .to_vec(),
            selected.environment,
        )
        .await;
        let expected = if configured {
            &selected_node
        } else {
            &runtime.executable
        };
        assert_eq!(
            fs::canonicalize(actual.trim()).unwrap(),
            fs::canonicalize(expected).unwrap()
        );
    }
    let selected = tools(&f.0).await;
    for key in ["IVY_CARGO", "RUSTC", "CC", "CARGO_HOME", "INCLUDE", "LIB"] {
        assert!(
            selected
                .environment
                .get(std::ffi::OsStr::new(key))
                .is_some_and(|value| !value.is_empty()),
            "{key}"
        );
    }
    assert!(
        !process::runtime_environment(&selected.environment)
            .contains_key(std::ffi::OsStr::new("IVY_COMPILER_SECRET"))
    );
    let provided = Child::new(std::env::current_exe().unwrap())
        .arg("--provided")
        .env(
            "INCLUDE",
            selected
                .environment
                .get(std::ffi::OsStr::new("INCLUDE"))
                .unwrap(),
        )
        .env(
            "LIB",
            selected
                .environment
                .get(std::ffi::OsStr::new("LIB"))
                .unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        provided.status.success(),
        "{}",
        String::from_utf8_lossy(&provided.stderr)
    );
    let project = f.0.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='native_compiler_probe'\nversion='1.0.0'\nedition='2024'\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        project.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"native_compiler_probe\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    fs::write(project.join("build.rs"), r##"use std::{env,fs,path::PathBuf,process::Command};
fn main() {
    let out=PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let compiler=PathBuf::from(env::var_os("CC").unwrap());
    fs::write(out.join("answer.c"), "#include <stddef.h>\nint native_answer(void) { return sizeof(size_t) ? 42 : 0; }\n").unwrap();
    let compiled=Command::new(&compiler).args(["/nologo","/c"]).arg(out.join("answer.c")).arg(format!("/Fo{}",out.join("answer.obj").display())).output().unwrap();
    assert!(compiled.status.success(), "{compiled:?}");
    assert!(Command::new(compiler.parent().unwrap().join("lib.exe")).arg("/nologo").arg(format!("/OUT:{}",out.join("answer.lib").display())).arg(out.join("answer.obj")).status().unwrap().success());
    println!("cargo:rustc-link-search=native={}",out.display());
    println!("cargo:rustc-link-lib=static=answer");
}"##).unwrap();
    fs::write(project.join("src/main.rs"), "unsafe extern \"C\" {fn native_answer()->i32;} fn main(){assert_eq!(unsafe{native_answer()},42);println!(\"42\");}\n").unwrap();
    let mut environment = selected.environment;
    environment.insert(
        "CARGO_TARGET_DIR".into(),
        project.join("target").into_os_string(),
    );
    run(
        &project,
        tool("IVY_TEST_CARGO"),
        ["build", "--locked", "--offline", "--release"]
            .map(OsString::from)
            .to_vec(),
        environment.clone(),
    )
    .await;
    let answer = run(
        &project,
        project.join("target/release/native_compiler_probe.exe"),
        vec![],
        environment,
    )
    .await;
    assert_eq!(answer.trim(), "42");
}
#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--list") {
        println!("filtered_native_compiler_build: test");
        return;
    }
    if !cfg!(windows) {
        return;
    }
    if args.iter().any(|arg| arg == "--provided") {
        let f = ServiceFixture::new();
        fs::create_dir_all(f.0.join("dist/native")).unwrap();
        fs::copy(
            helpers().join("ivy-job.exe"),
            f.0.join("dist/native/ivy-job.exe"),
        )
        .unwrap();
        let selected = tools(&f.0).await;
        for key in ["INCLUDE", "LIB"] {
            assert_eq!(
                selected.environment.get(std::ffi::OsStr::new(key)),
                std::env::var_os(key).as_ref()
            );
        }
    } else if args.iter().any(|arg| arg == "--probe") {
        probe().await;
    } else {
        let result = Child::new(std::env::current_exe().unwrap())
            .arg("--probe")
            .env_remove("INCLUDE")
            .env_remove("LIB")
            .env_remove("WindowsSdkDir")
            .env_remove("WindowsSDKVersion")
            .env(
                "PATH",
                PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"),
            )
            .env("IVY_COMPILER_SECRET", "must-stay-private")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
