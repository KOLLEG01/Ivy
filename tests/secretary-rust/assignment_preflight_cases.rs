use super::*;
use ivy_contracts::{
    checked_native_contract::CheckedNativeContracts, json::canonical,
    operation::parse_operation_id_js,
};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    assignment_execution::AssignmentExecution,
    assignment_native::AssignmentNative,
    assignment_preflight::{AssignmentPreflight, PreflightResult},
    inbox_evidence::InboxEvidence,
    journal::{Journal, Limits},
    store::Store,
};
use std::sync::atomic::{AtomicI64, Ordering};
struct PreflightClock(AtomicI64);
impl Clock for PreflightClock {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
#[tokio::test]
async fn original_preflight_admission_outputs_cancellation_and_timestamps() {
    compare_preflight(false).await;
}
#[tokio::test]
async fn original_preflight_with_the_paired_host_process_adapter() {
    compare_preflight(true).await;
}
struct ProcessRoot(std::path::PathBuf);
impl Drop for ProcessRoot {
    fn drop(&mut self) {
        let actual = std::fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(
                std::fs::canonicalize(std::env::temp_dir())
                    .unwrap()
                    .as_path()
            )
        );
        assert!(
            actual
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("secretary-preflight-")
        );
        let _ = std::fs::remove_dir_all(actual);
    }
}
async fn compare_preflight(real_process: bool) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let process_root = ProcessRoot(
        std::env::temp_dir().join(format!("secretary-preflight-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&process_root.0).unwrap();
    let artifact = process_root.0.join("artifact");
    std::fs::create_dir(&artifact).unwrap();
    #[cfg(windows)]
    if real_process {
        let platform = if cfg!(target_arch = "aarch64") {
            "win32-arm64"
        } else {
            "win32-x64"
        };
        let native = artifact.join("dist/native");
        let target = native.join(platform);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::copy(
            std::env::var_os("IVY_SECRETARY_JOB_HELPER")
                .expect("Qualified Host Job helper is required"),
            target.join("ivy-job.exe"),
        )
        .unwrap();
        std::fs::write(
            native.join("ivy-job.exe"),
            b"invalid legacy helper must never be selected",
        )
        .unwrap();
    }
    let job_launcher = if real_process {
        ivy_secretary::service::packaged_job_launcher(&artifact).unwrap()
    } else {
        None
    };
    #[cfg(windows)]
    if real_process {
        use std::os::windows::ffi::OsStringExt;
        let output = std::process::Command::new("node")
            .args(["-p", "process.execPath"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let executable =
            std::path::PathBuf::from(std::str::from_utf8(&output.stdout).unwrap().trim());
        for directory in ["isolated", "replacement", "paired"] {
            let path = process_root.0.join(directory);
            std::fs::create_dir(&path).unwrap();
            if directory == "paired" {
                std::fs::copy(&executable, path.join("exec-😀.exe")).unwrap();
            } else {
                // Only native Windows I/O can create these exact names. A JS fs
                // string would already select the replacement-character file.
                for unit in [0xd800, 0xdc00] {
                    let name = "exec-"
                        .encode_utf16()
                        .chain([unit])
                        .chain(".exe".encode_utf16())
                        .collect::<Vec<_>>();
                    std::fs::copy(&executable, path.join(std::ffi::OsString::from_wide(&name)))
                        .unwrap();
                }
                if directory == "replacement" {
                    std::fs::copy(&executable, path.join("exec-�.exe")).unwrap();
                }
            }
        }
    }
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/assignment-preflight-oracle.mjs")
        .current_dir(&root);
    if real_process {
        command.arg("--process").arg(&process_root.0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().expect("Original Node oracle required");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(
        rows.len(),
        if real_process {
            if cfg!(windows) { 26 } else { 21 }
        } else {
            45
        }
    );
    for (index, row) in rows.iter().enumerate() {
        let input = row.required("input").unwrap();
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        let client = Arc::new(Scripted {
            steps: Mutex::new(
                input
                    .required("steps")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            trace: Mutex::new(Vec::new()),
        });
        let store = Store {
            client: client.clone(),
            root: corpus
                .required("rootId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            journal: &journal,
        };
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..RequestOptions::default()
        };
        if input.get("cancelBefore") == Some(&IvyJson::Bool(true)) {
            cancellation.cancellation.cancel();
        }
        let service_node_id = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: corpus.required("settings").unwrap(),
            service_node_id: &service_node_id,
            cancellation: cancellation.clone(),
        };
        let clock = PreflightClock(AtomicI64::new(
            input.required("time").unwrap().as_f64().unwrap() as i64,
        ));
        let retained = CheckedNativeContracts::new(root.clone());
        let execution = AssignmentExecution {
            native: AssignmentNative {
                evidence: &evidence,
                tools: Arc::new(ToolClients::new(client.clone())),
            },
            clock: &clock,
            retained: &retained,
            notification_policy: corpus
                .required("notificationPolicy")
                .unwrap()
                .as_js_string()
                .unwrap(),
        };
        let preflight = AssignmentPreflight {
            execution: &execution,
        };
        let current = input.required("current").unwrap();
        let mut ran = false;
        let outcome = if real_process {
            let pid_file = input
                .get("pidFile")
                .and_then(IvyJson::as_js_string)
                .map(ivy_host_runtime::node_path::to_native);
            if let Some(file) = &pid_file {
                assert!(file.starts_with(&process_root.0));
                std::fs::remove_file(file).unwrap();
            }
            let abort = if input.get("cancelProcess") == Some(&IvyJson::Bool(true)) {
                let file = pid_file.clone().unwrap();
                let token = cancellation.cancellation.clone();
                Some(tokio::spawn(async move {
                    tokio::time::timeout(std::time::Duration::from_secs(5), async {
                        while !file.is_file() {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .expect("Child readiness before cancellation");
                    token.cancel();
                }))
            } else {
                None
            };
            let result = preflight
                .execute(current, &artifact, job_launcher.clone())
                .await;
            if let Some(abort) = abort {
                abort.await.unwrap();
            }
            if let Some(file) = pid_file {
                let pid = std::fs::read_to_string(file).unwrap();
                let output = std::process::Command::new("node")
                    .args(["-e", "try{process.kill(Number(process.argv[1]),0);process.exitCode=1}catch(e){if(e.code!=='ESRCH')throw e}", &pid])
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "Process still observed after return"
                );
            }
            result
        } else {
            match preflight.prepare(current).await {
                Err(error) => Err(error),
                Ok(None) => Ok(current.clone()),
                Ok(Some(prepared)) => {
                    ran = true;
                    clock.0.store(
                        input.required("finishedAt").unwrap().as_f64().unwrap() as i64,
                        Ordering::Relaxed,
                    );
                    if input.get("cancelProcess") == Some(&IvyJson::Bool(true)) {
                        cancellation.cancellation.cancel();
                    }
                    let result = if input.get("failed") == Some(&IvyJson::Bool(true)) {
                        let error = input.required("error").unwrap();
                        let outcome = match error
                            .required("outcome")
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .as_str()
                        {
                            "unknown" => Outcome::Unknown,
                            "completed" => Outcome::Completed,
                            _ => Outcome::NotExecuted,
                        };
                        PreflightResult::Failed {
                            error: IvyError::new(
                                error.required("code").unwrap().as_str().unwrap(),
                                error.required("message").unwrap().as_str().unwrap(),
                            )
                            .with_outcome(outcome),
                            is_ivy_error: input.get("ivyError") == Some(&IvyJson::Bool(true)),
                            stdout: input.required("stdout").unwrap().clone(),
                            stderr: input.required("stderr").unwrap().clone(),
                        }
                    } else {
                        let string = |key| {
                            input
                                .get(key)
                                .and_then(IvyJson::as_js_string)
                                .cloned()
                                .unwrap_or_else(|| "".into())
                        };
                        PreflightResult::Completed {
                            stdout: string("stdout"),
                            stderr: string("stderr"),
                        }
                    };
                    preflight.complete(&prepared, result).await
                }
            }
        };
        let result = match outcome {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(e) => IvyJson::object([
                ("code", e.code.into()),
                ("message", e.message.into()),
                ("outcome", e.outcome.to_json()),
            ]),
        };
        let mut trace = client.trace.lock().unwrap().clone();
        for call in &mut trace {
            if call.get("method") == Some(&"objects.write".into()) {
                let mut params = call.required("params").unwrap().clone();
                parse_operation_id_js(
                    params
                        .required("mutationId")
                        .unwrap()
                        .as_js_string()
                        .unwrap(),
                )
                .unwrap();
                params.insert("mutationId", "<scoped-operation>".into());
                call.insert("params", params);
            }
        }
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Case {index}: {:?}",
            input.get("label")
        );
        let mut actual = IvyJson::object([
            ("result", result),
            ("trace", IvyJson::Array(trace)),
            ("ran", IvyJson::Bool(ran)),
        ]);
        let mut expected = row.required("expected").unwrap().clone();
        if real_process {
            actual.remove("ran");
            expected.remove("ran");
        }
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(&expected, 32 * 1024 * 1024).unwrap(),
            "Case {index}: {:?}",
            input.get("label")
        );
    }
    if real_process {
        println!(
            "{} original preflight comparisons through the packaged helper; invalid legacy helper ignored, including timeout/cancellation retirement",
            rows.len()
        );
    }
}
