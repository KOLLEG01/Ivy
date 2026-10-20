//! Explicit finite resource experiment, never a production DocumentWorker.
#[path = "document_resource_windows.rs"]
mod windows;
use ivy_contracts::hash::digest;
use ivy_host_runtime::process::{self, Command, LaunchContext};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    time::Instant,
};

async fn read(mut pipe: impl AsyncRead + Unpin, maximum: usize, start: Instant) -> (Vec<u8>, u64) {
    let mut bytes = vec![];
    (&mut pipe)
        .take((maximum + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .unwrap();
    assert!(
        bytes.len() <= maximum,
        "Private parser pipe exceeded its existing bound"
    );
    (bytes, start.elapsed().as_millis() as u64)
}
async fn measure(
    case: &Value,
    target: &str,
    corpus: &std::path::Path,
    root: &std::path::Path,
    output: &std::path::Path,
) -> Value {
    let name = case["name"].as_str().unwrap();
    let format = case["format"].as_str().unwrap();
    let budget = case["budget"].as_u64().unwrap().to_string();
    let bytes = std::fs::read(corpus.join(case["file"].as_str().unwrap())).unwrap();
    assert_eq!(bytes.len() as u64, case["byteLength"].as_u64().unwrap());
    assert_eq!(digest(&bytes), case["contentHash"].as_str().unwrap());
    let helper = PathBuf::from(std::env::var_os("IVY_SECRETARY_JOB_HELPER").unwrap());
    assert_eq!(
        digest(&std::fs::read(&helper).unwrap()),
        "sha256:c380c679cdbe22e883fa1450a9fba2c6c91d5344c756d62af3bc4e3e3df84b27"
    );
    let node = PathBuf::from(std::env::var_os("IVY_SECRETARY_RESOURCE_NODE").unwrap());
    let rust = PathBuf::from(std::env::var_os("IVY_SECRETARY_RESOURCE_RUST").unwrap());
    let pdf = PathBuf::from(std::env::var_os("IVY_SECRETARY_RESOURCE_PDF").unwrap());
    let (executable, args) = match target {
        "rust" => (
            rust,
            vec!["--document-worker".into(), format.into(), budget.into()],
        ),
        "original" => (
            node,
            vec![
                root.join("tests/secretary-rust/document-resource-original.mjs")
                    .into_os_string(),
                format.into(),
                budget.into(),
            ],
        ),
        "pdf" => (node, vec![pdf.into_os_string()]),
        _ => panic!("Unknown resource target"),
    };
    let start = Instant::now();
    let job_name = format!("Global\\Ivy.SecretaryResource.{}", uuid::Uuid::new_v4());
    let deadline = start + Duration::from_secs(30);
    let mut child = process::spawn(
        &Command {
            executable: "resource-parser".into(),
            args,
            cwd: Some(root.to_owned()),
            timeout_ms: 30000,
        },
        root,
        &BTreeMap::from([("resource-parser".into(), executable)]),
        LaunchContext {
            job_launcher: Some(helper),
            job_name: Some(job_name.clone()),
            raw_output: true,
            environment: BTreeMap::from([(
                "IVY_SECRETARY_ORACLE".into(),
                std::env::var_os("IVY_SECRETARY_ORACLE").unwrap(),
            )]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let launch_ms = start.elapsed().as_millis() as u64;
    let observer = windows::Observer::start(child.pid, &job_name);
    let observed_ms = start.elapsed().as_millis() as u64;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = tokio::spawn(read(child.stdout.take().unwrap(), 8 * 1024 * 1024, start));
    let mut stderr = tokio::spawn(read(child.stderr.take().unwrap(), 65536, start));
    let hold = case["holdEof"].as_bool().unwrap_or(false);
    let writer = tokio::spawn(async move {
        let error = stdin
            .write_all(&bytes)
            .await
            .err()
            .map(|e| e.kind().to_string());
        let written_ms = start.elapsed().as_millis() as u64;
        if hold {
            tokio::time::sleep_until(deadline + Duration::from_secs(2)).await;
        }
        let close = stdin.shutdown().await.err().map(|e| e.kind().to_string());
        (written_ms, start.elapsed().as_millis() as u64, error, close)
    });
    let completed = tokio::time::timeout_at(deadline, child.completion.wait()).await;
    let timed_out = completed.is_err();
    let status = match completed {
        Ok(value) => value,
        Err(_) => child.stop(Duration::ZERO).await.unwrap(),
    };
    // Completion never substitutes for private pipe EOF or kernel retirement.
    let (out, err) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(2), &mut stdout),
        tokio::time::timeout(Duration::from_secs(2), &mut stderr)
    );
    let (out, out_ms) = out.expect("Parser stdout EOF unavailable").unwrap();
    let (err, err_ms) = err.expect("Parser stderr EOF unavailable").unwrap();
    let stopped = process::owned_process_tree_stopped(&child).await.unwrap();
    let retired_ms = start.elapsed().as_millis() as u64;
    let memory = observer.finish();
    let input = writer.await.unwrap();
    let reply: Option<Value> = serde_json::from_slice(&out).ok();
    let observed = if timed_out {
        "owner_deadline"
    } else if let Some(code) = reply.as_ref().and_then(|v| v["code"].as_str()) {
        code
    } else if reply
        .as_ref()
        .is_some_and(|v| v["text"].is_string() || v["value"].is_object())
    {
        "accepted"
    } else {
        "invalid_private_reply"
    };
    let expected = case["expected"].as_str().unwrap();
    let row = json!({"case":name,"target":target,"format":format,"sourceBytes":case["byteLength"],"sourceHash":case["contentHash"],
        "budget":case["budget"],"expected":expected,"observed":observed,"expectedOutcomeMatches":observed==expected,
        "launchMs":launch_ms,"observerReadyMs":observed_ms,"stdinWriteMs":input.0,"stdinCloseMs":input.1,
        "stdinWriteError":input.2,"stdinCloseError":input.3,"stdoutEofMs":out_ms,"stderrEofMs":err_ms,
        "retiredMs":retired_ms,"ownerDeadlineMs":30000,"deadlineTriggered":timed_out,
        "status":{"exitCode":status.exit_code,"signal":status.signal,"errorCode":status.error_code},
        "stdoutBytes":out.len(),"stdoutHash":digest(&out),"stderrBytes":err.len(),"stderrHash":digest(&err),
        "retirementProven":stopped,"memory":memory});
    // Retain the exact observation before an assertion, including unavailable
    // counters or retirement. A failed observation must never become zero use.
    std::fs::write(
        output.join("latest-attempt.json"),
        serde_json::to_vec_pretty(&row).unwrap(),
    )
    .unwrap();
    std::fs::write(output.join(format!("{name}-{target}-reply.json")), &out).unwrap();
    assert!(stopped, "Owned parser tree retirement unproved");
    assert!(
        row["memory"]["processes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["jobMemberAtSetup"] == true)
            .all(|p| p["exited"] == true && p["successfulReads"].as_u64().unwrap() > 0)
    );
    assert!(
        row["memory"]["processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["jobMemberAtSetup"] == true)
    );
    assert!(
        status.error_code.is_none(),
        "Host process completion is uncertain"
    );
    assert!(
        hold || !timed_out,
        "Finite parser exceeded the preserved deadline"
    );
    assert!(
        hold || status.exit_code == Some(0),
        "Finite parser process failed before its private reply"
    );
    row
}

#[tokio::test]
#[ignore = "Explicit Windows resource experiment needs sealed parser/helper/Node/assets and a finite corpus."]
async fn finite_document_memory_and_retirement_baseline() {
    let root = std::env::current_dir().unwrap();
    let corpus = PathBuf::from(std::env::var_os("IVY_SECRETARY_RESOURCE_CORPUS").unwrap());
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(corpus.join("manifest.json")).unwrap()).unwrap();
    let output_directory =
        PathBuf::from(std::env::var_os("IVY_SECRETARY_RESOURCE_OUTPUT").unwrap());
    std::fs::create_dir(&output_directory).unwrap();
    let reuse_first = std::env::var_os("IVY_SECRETARY_RESOURCE_REUSE")
        .map(|path| {
            let rows = std::fs::read_to_string(path).unwrap();
            let rows: Vec<Value> = rows
                .lines()
                .map(|s| serde_json::from_str(s).unwrap())
                .collect();
            assert_eq!(rows.len(), 2);
            for (row, target) in rows.iter().zip(["rust", "original"]) {
                assert_eq!(row["case"], "html-original-inert");
                assert_eq!(row["target"], target);
                assert_eq!(row["sourceHash"], manifest["cases"][0]["contentHash"]);
                assert_eq!(row["retirementProven"], true);
                assert_eq!(row["observed"], "accepted");
            }
            true
        })
        .unwrap_or(false);
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_directory.join("observations.jsonl"))
        .unwrap();
    let mut count = 0;
    for case in manifest["cases"].as_array().unwrap() {
        // Both valid first-case measurements from run154 are reused. Only the
        // failed and unattempted observations need the corrected observer.
        if reuse_first && case["name"] == "html-original-inert" {
            continue;
        }
        for target in case["targets"].as_array().unwrap() {
            let row = measure(
                case,
                target.as_str().unwrap(),
                &corpus,
                &root,
                &output_directory,
            )
            .await;
            serde_json::to_writer(&mut output, &row).unwrap();
            output.write_all(b"\n").unwrap();
            output.flush().unwrap();
            println!(
                "resource {} {}: {} in {}ms; retirement {}",
                row["case"],
                row["target"],
                row["observed"],
                row["retiredMs"],
                row["retirementProven"]
            );
            count += 1;
        }
    }
    println!("Finite resource workloads observed: {count}; no OS cap or exhaustion attempted");
}
