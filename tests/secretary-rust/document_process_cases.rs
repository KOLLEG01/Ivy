//! Actual capped consumer qualification; run explicitly with sealed native inputs.
use super::*;
use crate::{
    media_document::AcquiredInput,
    media_input::{Prepared, Source},
};
use ivy_contracts::{hash::digest, json::canonical};
use serde_json::Value;
use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "secretary-document-process-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
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
                .starts_with("secretary-document-process-")
        );
        std::fs::remove_dir_all(actual).unwrap();
    }
}
fn path(name: &str) -> PathBuf {
    std::env::var_os(name).expect(name).into()
}
fn worker(cancel: watch::Receiver<bool>, fixture: bool) -> HostDocumentWorker {
    let root = std::env::current_dir().unwrap();
    HostDocumentWorker::new(
        root.clone(),
        path("IVY_SECRETARY_DOCUMENT_BINARY"),
        path("IVY_SECRETARY_DOCUMENT_HELPER"),
        PdfAdapter {
            node: NodeRuntime {
                executable: path("IVY_SECRETARY_DOCUMENT_NODE"),
                version: "24.18.0".into(),
            },
            script: if fixture {
                root.join("tests/secretary-rust/document-process-child.mjs")
            } else {
                path("IVY_SECRETARY_DOCUMENT_PDF")
            },
        },
        cancel,
    )
}
fn control(mode: &str, ready: Option<&Path>) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"mode":mode,"ready":ready})).unwrap()
}
fn record(name: &str, value: Value) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let row = serde_json::json!({"case":name,"value":value});
    let number = NEXT.fetch_add(1, Ordering::SeqCst);
    std::fs::write(
        path("IVY_SECRETARY_DOCUMENT_RESULTS").join(format!("{number:03}-{name}.json")),
        serde_json::to_vec_pretty(&row).unwrap(),
    )
    .unwrap();
    println!("document process {name}: {}", value);
}

#[tokio::test]
#[ignore = "Explicit sealed Windows parser/helper/Node/PDF corpus required"]
async fn capped_corpus_matches_retained_rust_and_approved_pdf_replies() {
    let corpus = path("IVY_SECRETARY_DOCUMENT_CORPUS");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(corpus.join("manifest.json")).unwrap()).unwrap();
    let (_, cancel) = watch::channel(false);
    let driver = worker(cancel, false);
    let mut count = 0;
    for case in manifest["cases"].as_array().unwrap() {
        if case["holdEof"] == true {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let bytes = std::fs::read(corpus.join(case["file"].as_str().unwrap())).unwrap();
        assert_eq!(digest(&bytes), case["contentHash"].as_str().unwrap());
        let (format, target) = match case["format"].as_str().unwrap() {
            "html" => (Format::Html, "rust"),
            "docx" => (Format::Docx, "rust"),
            _ => (Format::Pdf, "pdf"),
        };
        let expected: Value = serde_json::from_slice(
            &std::fs::read(
                path("IVY_SECRETARY_DOCUMENT_REPLIES").join(format!("{name}-{target}-reply.json")),
            )
            .unwrap(),
        )
        .unwrap();
        let reply = driver
            .extract(format, &bytes, case["budget"].as_u64().unwrap() as f64)
            .await
            .unwrap();
        let actual = canonical(&reply, WORKER_REPLY_BYTES).unwrap();
        let expected =
            canonical(&IvyJson::from_serde(&expected).unwrap(), WORKER_REPLY_BYTES).unwrap();
        record(
            name,
            serde_json::json!({"hash":digest(actual.as_bytes()),"expectedHash":digest(expected.as_bytes()),"sourceHash":case["contentHash"],"capBytes":JOB_COMMIT_BYTES}),
        );
        assert_eq!(actual, expected, "Capped corpus reply {name}");
        count += 1;
    }
    assert_eq!(count, 21);
}

#[tokio::test]
#[ignore = "Explicit sealed Windows parser/helper/Node/PDF corpus required"]
async fn acquired_input_uses_real_worker_and_retains_its_cache() {
    let root = Root::new();
    let helper = root.0.join("ivy-job.exe");
    std::fs::copy(path("IVY_SECRETARY_DOCUMENT_HELPER"), &helper).unwrap();
    let (_, cancel) = watch::channel(false);
    let mut driver = worker(cancel, false);
    driver.job_launcher = helper.clone();
    let input = AcquiredInput::new(&driver, None);
    let bytes = b"<p>Complete inert source</p>";
    let source = Source {
        name: "doc".into(),
        media_type: Some("text/html".into()),
        byte_length: bytes.len(),
        content_hash: digest(bytes),
    };
    let first = input
        .prepare(&source, bytes, REQUEST_BYTES as f64)
        .await
        .unwrap();
    assert!(matches!(&first, Prepared::Input { .. }));
    std::fs::remove_file(helper).unwrap();
    let second = input
        .prepare(&source, bytes, REQUEST_BYTES as f64)
        .await
        .unwrap();
    assert_eq!(first, second);
    record(
        "acquired-cache",
        serde_json::json!({"sameProjectionWithoutHelper":true}),
    );
}

#[tokio::test]
#[ignore = "Explicit sealed Windows parser/helper/Node/PDF corpus required"]
async fn actual_single_and_aggregate_memory_notices_map_after_retirement_and_eof() {
    for mode in ["memory-single", "memory-aggregate"] {
        let (_, cancel) = watch::channel(false);
        let result = run(
            worker(cancel, true),
            Format::Pdf,
            control(mode, None),
            REQUEST_BYTES,
            Instant::now() + LIFETIME,
        )
        .await;
        record(
            mode,
            serde_json::json!({"failure":format!("{result:?}"),"capBytes":JOB_COMMIT_BYTES}),
        );
        assert_eq!(result, Err(Failure::Memory));
        assert_eq!(
            Failure::Memory.reply().unwrap_err().code,
            "media_document_parser_failed"
        );
    }
}

#[tokio::test]
#[ignore = "Explicit sealed Windows parser/helper/Node/PDF corpus required"]
async fn private_io_failures_and_inherited_pipes_retire_before_results() {
    for mode in [
        "invalid",
        "exit",
        "stdout-overflow",
        "stderr-overflow",
        "descendant",
    ] {
        let (_, cancel) = watch::channel(false);
        let result = run(
            worker(cancel, true),
            Format::Pdf,
            control(mode, None),
            REQUEST_BYTES,
            Instant::now() + LIFETIME,
        )
        .await;
        record(mode, serde_json::json!({"result":format!("{result:?}")}));
        if mode == "descendant" {
            assert_eq!(result.unwrap(), code("media_document_no_text"));
        } else {
            assert_eq!(result, Err(Failure::Parser));
        }
    }
}

#[tokio::test]
#[ignore = "Explicit sealed Windows parser/helper/Node/PDF corpus required"]
async fn original_thirty_second_deadline_keeps_its_code_after_retirement() {
    let (_, cancel) = watch::channel(false);
    let start = Instant::now();
    let result = worker(cancel, true)
        .extract(Format::Pdf, &control("hang", None), REQUEST_BYTES as f64)
        .await
        .unwrap();
    record(
        "deadline",
        serde_json::json!({"milliseconds":start.elapsed().as_millis(),"code":result.required("code").unwrap().as_str().unwrap()}),
    );
    assert_eq!(result, code("media_document_timeout"));
    assert!(start.elapsed() >= LIFETIME);
    assert!(start.elapsed() < LIFETIME + Duration::from_secs(5));
}

#[tokio::test]
#[ignore = "Explicit sealed Windows parser/helper/Node/PDF corpus required"]
async fn cancellation_before_and_after_admission_keeps_owned_retirement() {
    let (request, cancel) = watch::channel(false);
    let driver = worker(cancel, true);
    let root = Root::new();
    let ready = root.0.join("ready");
    let marker = ready.clone();
    let stop = tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.is_file() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        request.send(true).unwrap();
    });
    let result = run(
        driver,
        Format::Pdf,
        control("hang", Some(&ready)),
        REQUEST_BYTES,
        Instant::now() + LIFETIME,
    )
    .await;
    stop.await.unwrap();
    assert_eq!(result, Err(Failure::Cancelled));
    let (_, cancel) = watch::channel(true);
    let before = run(
        worker(cancel, true),
        Format::Pdf,
        control("ok", None),
        REQUEST_BYTES,
        Instant::now() + LIFETIME,
    )
    .await;
    assert_eq!(before, Err(Failure::Cancelled));
    record(
        "cancel",
        serde_json::json!({"beforeAndAfter":"Cancelled","retirementRequiredAfterAdmission":true}),
    );
}

#[tokio::test]
#[ignore = "Explicit sealed Windows parser/helper/Node/PDF corpus required"]
async fn old_helper_never_launches_a_capped_parser_unbounded() {
    let (_, cancel) = watch::channel(false);
    let mut driver = worker(cancel, true);
    driver.job_launcher = path("IVY_SECRETARY_PRE_MEMORY_HELPER");
    let root = Root::new();
    let marker = root.0.join("unexpected-parser");
    let result = run(
        driver,
        Format::Pdf,
        control("ok", Some(&marker)),
        REQUEST_BYTES,
        Instant::now() + LIFETIME,
    )
    .await;
    record(
        "old-helper",
        serde_json::json!({"result":format!("{result:?}"),"parserRan":marker.is_file()}),
    );
    assert_eq!(result, Err(Failure::Unknown));
    assert!(!marker.exists());
}
