use super::*;
use ivy_contracts::{
    IvyResult, JsString,
    hash::{digest, hash_json},
    json::canonical,
};
use ivy_secretary::{
    document::Format,
    media_document::{AcquiredInput, DocumentFuture, DocumentWorker, format_name},
    media_input::{Prepared, Source},
};
use std::{cell::RefCell, path::Path, process::Command};

fn oracle(script: &str, arguments: &[&str]) -> IvyJson {
    let mut command = Command::new("node");
    command
        .arg(script)
        .args(arguments)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap()
}
fn document(reply: &IvyJson, format: Format) -> IvyJson {
    let text = reply
        .get("text")
        .and_then(IvyJson::as_js_string)
        .cloned()
        .unwrap_or_else(|| "Inert projection".into());
    let repeat = reply.get("repeat").and_then(IvyJson::as_f64).unwrap_or(1.0) as usize;
    let mut value = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        (
            "format",
            reply
                .get("format")
                .cloned()
                .unwrap_or_else(|| format_name(format).into()),
        ),
        ("representation", "document_text".into()),
        (
            "sections",
            IvyJson::Array(vec![IvyJson::object([
                ("label", "Projection".into()),
                ("text", IvyJson::String(JsString(text.0.repeat(repeat)))),
            ])]),
        ),
        (
            "limitations",
            IvyJson::Array(vec![
                "visual_layout_not_interpreted".into(),
                "embedded_media_not_interpreted".into(),
                "external_resources_not_loaded".into(),
            ]),
        ),
    ]);
    if let Some(extra) = reply.get("document").and_then(IvyJson::as_object) {
        for (key, v) in extra {
            value.insert(key.clone(), v.clone());
        }
    }
    value
}
struct Worker {
    selected: RefCell<IvyJson>,
    trace: RefCell<Vec<IvyJson>>,
}
impl DocumentWorker for Worker {
    fn extract<'a>(&'a self, format: Format, bytes: &'a [u8], budget: f64) -> DocumentFuture<'a> {
        Box::pin(async move {
            let selected = self.selected.borrow().clone();
            self.trace.borrow_mut().push(IvyJson::object([
                ("format", format_name(format).into()),
                ("budget", IvyJson::Number(budget)),
                ("byteLength", IvyJson::Number(bytes.len() as f64)),
                ("contentHash", digest(bytes).into()),
            ]));
            tokio::task::yield_now().await;
            let reply = selected.required("reply")?;
            match reply.required("kind")?.as_str()?.as_str() {
                "unknown" => Err(IvyError::new(
                    "outcome_unknown",
                    "Original worker retirement is unproved.",
                )
                .with_outcome(Outcome::Unknown)),
                "error" | "exit" => Err(IvyError::new(
                    "fixture_worker_failed",
                    "Owned child failed after confirmed retirement.",
                )
                .with_outcome(Outcome::Completed)),
                "timeout" => Ok(IvyJson::object([("code", "media_document_timeout".into())])),
                "code" => Ok(IvyJson::object([("code", reply.required("code")?.clone())])),
                "raw" => Ok(reply.required("value")?.clone()),
                "pdfValue" => Ok(IvyJson::object([("value", document(reply, format))])),
                _ => Ok(IvyJson::object([(
                    "text",
                    format!(
                        "{}{}",
                        " ".repeat(
                            reply
                                .get("padding")
                                .and_then(IvyJson::as_f64)
                                .unwrap_or(0.0) as usize
                        ),
                        canonical(&document(reply, format), 16 * 1024 * 1024)?
                    )
                    .into(),
                )])),
            }
        })
    }
}
fn action_source(action: &IvyJson) -> (Source, Vec<u8>, f64) {
    let bytes = action
        .get("body")
        .unwrap_or(&"owned".into())
        .as_str()
        .unwrap()
        .into_bytes();
    let format = action
        .get("format")
        .and_then(|v| v.as_str().ok())
        .unwrap_or_else(|| "html".into());
    let mime = match format.as_str() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "text/html",
    };
    let source = Source {
        name: action
            .get("name")
            .and_then(IvyJson::as_js_string)
            .cloned()
            .unwrap_or_else(|| "inert".into()),
        media_type: Some(
            action
                .get("mediaType")
                .and_then(IvyJson::as_js_string)
                .cloned()
                .unwrap_or_else(|| mime.into()),
        ),
        byte_length: action
            .get("byteLength")
            .and_then(IvyJson::as_f64)
            .map(|n| n as usize)
            .unwrap_or(bytes.len()),
        content_hash: action
            .get("contentHash")
            .and_then(|v| v.as_str().ok())
            .unwrap_or_else(|| digest(&bytes)),
    };
    (
        source,
        bytes,
        action
            .get("budget")
            .and_then(IvyJson::as_f64)
            .unwrap_or(6291456.0),
    )
}
fn result(result: IvyResult<Prepared>) -> IvyJson {
    match result {
        Ok(value) => {
            let all = value.to_json();
            let mut brief = IvyJson::object([
                ("kind", all.required("kind").unwrap().clone()),
                ("source", all.required("source").unwrap().clone()),
                ("contentHash", hash_json(&all).unwrap().into()),
            ]);
            if let Some(code) = all.get("code") {
                brief.insert("code", code.clone());
            }
            IvyJson::object([("value", brief)])
        }
        Err(error) => IvyJson::object([
            ("code", IvyJson::String(error.code_js())),
            ("outcome", error.outcome.to_json()),
        ]),
    }
}
#[tokio::test]
async fn original_acquired_document_gates_reply_admission_and_cache_limits() {
    let corpus = oracle("tests/secretary-rust/media-document-oracle.mjs", &[]);
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 31);
    for row in rows {
        let input = row.required("input").unwrap();
        let label = input.required("label").unwrap().as_str().unwrap();
        let actions = input.required("actions").unwrap().as_array().unwrap();
        let worker = Worker {
            selected: RefCell::new(IvyJson::Null),
            trace: RefCell::new(vec![]),
        };
        let acquired = AcquiredInput::new(&worker, None);
        let mut values = vec![];
        if input.get("concurrent") == Some(&true.into()) {
            *worker.selected.borrow_mut() = actions[0].clone();
            let (s1, b1, n1) = action_source(&actions[0]);
            let (s2, b2, n2) = action_source(&actions[1]);
            let (a, b) = tokio::join!(
                acquired.prepare(&s1, &b1, n1),
                acquired.prepare(&s2, &b2, n2)
            );
            values.push(result(a));
            values.push(result(b));
        } else {
            for action in actions {
                *worker.selected.borrow_mut() = action.clone();
                let (source, bytes, budget) = action_source(action);
                values.push(result(acquired.prepare(&source, &bytes, budget).await));
            }
        }
        let actual = IvyJson::object([
            ("values", IvyJson::Array(values)),
            ("trace", IvyJson::Array(worker.trace.into_inner())),
        ]);
        assert_eq!(
            canonical(&actual, 32 * 1024 * 1024).unwrap(),
            canonical(row.required("expected").unwrap(), 32 * 1024 * 1024).unwrap(),
            "{label}"
        );
    }
    println!(
        "acquired document comparisons: {} scenarios / {} input actions",
        rows.len(),
        rows.iter()
            .map(|row| row
                .required("input")
                .unwrap()
                .required("actions")
                .unwrap()
                .as_array()
                .unwrap()
                .len())
            .sum::<usize>()
    );
}
#[tokio::test]
async fn approved_pdf_value_envelope_and_unknown_retirement_are_explicit() {
    let action = IvyJson::object([
        ("body", "pdf".into()),
        ("format", "pdf".into()),
        ("reply", IvyJson::object([("kind", "pdfValue".into())])),
    ]);
    let worker = Worker {
        selected: RefCell::new(action.clone()),
        trace: RefCell::new(vec![]),
    };
    let acquired = AcquiredInput::new(&worker, None);
    let (source, bytes, budget) = action_source(&action);
    assert!(matches!(
        acquired.prepare(&source, &bytes, budget).await.unwrap(),
        Prepared::Input { .. }
    ));
    let mut changed = action.clone();
    changed.insert("body", "other pdf".into());
    changed.insert("reply", IvyJson::object([("kind", "unknown".into())]));
    *worker.selected.borrow_mut() = changed.clone();
    let (source, bytes, budget) = action_source(&changed);
    let error = acquired.prepare(&source, &bytes, budget).await.unwrap_err();
    assert_eq!(error.outcome, Outcome::Unknown);
    changed.insert("reply", IvyJson::object([("kind", "pdfValue".into())]));
    *worker.selected.borrow_mut() = changed;
    assert!(matches!(
        acquired.prepare(&source, &bytes, budget).await.unwrap(),
        Prepared::Input { .. }
    ));
    assert_eq!(worker.trace.borrow().len(), 3);
}
#[test]
fn actual_private_worker_uses_raw_bytes_without_service_startup() {
    let result = oracle(
        "tests/secretary-rust/document-worker-oracle.mjs",
        &[env!("CARGO_BIN_EXE_ivy-secretary")],
    );
    assert_eq!(result.get("failures"), Some(&IvyJson::Array(vec![])));
    assert!(result.required("cases").unwrap().as_f64().unwrap() >= 15.0);
    println!(
        "private document child: {} exact original extraction/protocol cases; OS memory-cap and Host lifetime integration remain unqualified",
        result.required("cases").unwrap().as_f64().unwrap()
    );
}
