use super::{Root, context, fixture_command};
use ivy_host_runtime::{IvyJson, paths, process};
use std::{collections::BTreeMap, fs, io::Write, path::Path, process::Command};

fn expected(executable: &Path, cwd: &Path) -> IvyJson {
    let result = Command::new("node").args(["-e", "const fs=require('node:fs');process.stdout.write(JSON.stringify({executable:fs.realpathSync(process.argv[1]),cwd:fs.realpathSync(process.argv[2])}))"])
        .arg(executable).arg(cwd).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    ivy_contracts::json::parse(&String::from_utf8(result.stdout).unwrap(), usize::MAX).unwrap()
}
fn compare(case: &str, actual: IvyJson, expected: IvyJson) {
    let canonical = |value| ivy_contracts::json::canonical(value, usize::MAX).unwrap();
    assert_eq!(canonical(&actual), canonical(&expected), "{case}");
    if let Some(file) = std::env::var_os("IVY_HOST_PROCESS_PATH_CORPUS") {
        writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(), "{}",
            canonical(&IvyJson::object([("case", case.into()), ("actual", actual), ("expected", expected)]))).unwrap();
    }
}

#[test]
fn resolved_process_executable_and_working_directory_use_original_node_spelling() {
    let root = Root::new(); let (mut command, executables) = fixture_command();
    fs::create_dir(root.0.join("nested")).unwrap(); command.cwd = Some("nested/..".into());
    let executable = std::fs::canonicalize(executables.get("fixture").unwrap()).unwrap();
    let mapped = BTreeMap::from([("fixture".into(), executable.clone())]);
    let extended_root = std::fs::canonicalize(&root.0).unwrap();
    let resolved = process::resolve_command(&command, &extended_root, &mapped).unwrap();
    let actual = IvyJson::object([("executable", resolved.executable.to_str().unwrap().into()), ("cwd", resolved.cwd.to_str().unwrap().into())]);
    compare("resolve", actual, expected(&executable, &root.0));
}

#[tokio::test]
async fn actual_children_keep_node_image_and_explicit_execution_cwd_spelling_with_and_without_jobs() {
    let root = Root::new(); let cwd = Root::new(); let (command, mut executables) = fixture_command();
    let executable = fs::canonicalize(executables.get("fixture").unwrap()).unwrap();
    executables.insert("fixture".into(), executable.clone());
    for without_job in [true, false] {
        let mut launch = context("path-spelling"); launch.without_job = without_job;
        launch.execution_cwd = Some(fs::canonicalize(&cwd.0).unwrap());
        #[cfg(windows)] { launch.job_launcher = Some(fs::canonicalize(launch.job_launcher.unwrap()).unwrap()); }
        let result = process::run_command(&command, &root.0, &executables, launch, None).await.unwrap();
        let value = result.stdout.split("path-spelling=").nth(1).unwrap().lines().next().unwrap();
        let actual = ivy_contracts::json::parse(value, usize::MAX).unwrap();
        compare(if without_job {"direct-child"} else {"job-child"}, actual, expected(&executable, &cwd.0));
        assert_eq!(paths::realpath(&cwd.0).unwrap(), Path::new(&expected(&executable, &cwd.0).required("cwd").unwrap().as_str().unwrap()));
    }
}
