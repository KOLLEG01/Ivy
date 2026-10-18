use ivy_data_collector::policy::WORKER;
#[path = "data-collector-runner.inc.rs"]
mod runner;
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

#[test]
fn adapter_stays_byte_equal_to_the_existing_workload_capability() {
    let source = include_str!("../../services/data-collector/src/runner.ts").replace("\r\n", "\n");
    let template = source
        .split_once("const worker = `")
        .unwrap()
        .1
        .split_once("`;\n")
        .unwrap()
        .0;
    let expected = template.replace("${JSON.stringify(taskErrorCodes)}", r#"["authentication_required","interaction_required","configuration_invalid","provider_unavailable"]"#);
    assert_eq!(WORKER.replace("\r\n", "\n"), expected);
}

fn execute(script: &str, input: &str) -> (i32, Option<String>) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "ivy-collector-worker-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(path.clone());
    std::fs::write(path.join("task.mjs"), script).unwrap();
    std::fs::write(path.join("worker.mjs"), WORKER).unwrap();
    let mut child = Command::new("node")
        .arg("worker.mjs")
        .current_dir(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    (
        result.status.code().unwrap(),
        std::fs::read_to_string(path.join("result.json")).ok(),
    )
}
#[test]
fn retained_worker_preserves_default_export_context_and_explicit_null() {
    let (code, result) = execute(
        "export default async ({config,secrets,state,input,signal})=>({data:{config,secrets,state,input,aborted:signal.aborted},state:null,events:[]});",
        r#"{"config":{"key":1},"secrets":{"TOKEN":"synthetic"},"state":0,"input":false,"timeoutMs":5000}"#,
    );
    assert_eq!(code, 0);
    assert_eq!(
        result.as_deref(),
        Some(
            r#"{"data":{"config":{"key":1},"secrets":{"TOKEN":"synthetic"},"state":0,"input":false,"aborted":false},"state":null,"events":[]}"#
        )
    );
}
#[test]
fn worker_exit_codes_are_independent_of_stderr_and_output_file() {
    for (error, expected) in [
        ("authentication_required", 80),
        ("interaction_required", 81),
        ("configuration_invalid", 82),
        ("provider_unavailable", 83),
        ("unexpected_provider_text", 1),
    ] {
        let script = format!(
            "export default async ()=>{{throw Object.assign(new Error('synthetic private message'),{{code:'{error}'}})}};"
        );
        let (code, result) = execute(&script, r#"{"timeoutMs":5000}"#);
        assert_eq!(code, expected);
        assert!(result.is_none());
    }
    for script in [
        "export default async ()=>undefined;",
        "export default async ()=>({data:'a'.repeat(1000000)});",
    ] {
        let (code, result) = execute(script, r#"{"timeoutMs":5000}"#);
        assert_eq!(code, 79);
        assert!(result.is_none());
    }
}
