use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyJson, json::parse};
use ivy_secretary::media_input::{Source, prepare};
use std::process::Command;

fn oracle(script: &str) -> IvyJson {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = Command::new("node");
    command.arg(script).current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().expect("Node oracle must be available");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        4 * 1024 * 1024,
    )
    .unwrap()
}

#[test]
fn original_media_signature_encoding_integrity_and_exact_budget_differential() {
    let corpus = oracle("tests/secretary-rust/media-input-oracle.mjs");
    let cases = corpus.as_array().unwrap();
    assert!(
        cases.len() > 300,
        "complete signature/size boundary corpus required"
    );
    for (index, case) in cases.iter().enumerate() {
        let wire = case.required("source").unwrap();
        let source = Source {
            name: wire
                .required("name")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            media_type: wire.required("mediaType").unwrap().as_js_string().cloned(),
            byte_length: wire.required("byteLength").unwrap().as_f64().unwrap() as usize,
            content_hash: wire.required("contentHash").unwrap().as_str().unwrap(),
        };
        let bytes = STANDARD
            .decode(case.required("bytes").unwrap().as_str().unwrap())
            .unwrap();
        let budget = case.required("budget").unwrap().as_f64().unwrap();
        let actual = match prepare(&source, &bytes, budget) {
            Ok(value) => IvyJson::object([("value", value.to_json())]),
            Err(error) => IvyJson::object([("code", error.code.into())]),
        };
        assert_eq!(&actual, case.required("expected").unwrap(), "case {index}");
    }
}

#[test]
fn catalogue_identity_preserves_exact_observed_account_differential() {
    let corpus = oracle("tests/secretary-rust/source-identity-oracle.mjs");
    for case in corpus.as_array().unwrap() {
        let actual = match ivy_secretary::source_identity::catalogue_account_hash(
            case.required("value").unwrap(),
        ) {
            Ok(hash) => IvyJson::object([("hash", hash.into())]),
            Err(error) => IvyJson::object([("code", error.code.into())]),
        };
        assert_eq!(&actual, case.required("expected").unwrap());
    }
}

#[test]
fn original_triage_media_order_gaps_video_offsets_and_budget_differential() {
    use ivy_contracts::hash::{digest, hash_json};
    use ivy_secretary::{
        journal::Pin,
        media_input::Prepared,
        triage_media::{self, Attachment, Context, Representation},
    };
    use std::{
        future::{Future, ready},
        task::{Context as PollContext, Poll, Waker},
    };
    let corpus = oracle("tests/secretary-rust/triage-media-oracle.mjs");
    for case in corpus.as_array().unwrap() {
        let mode = case.required("mode").unwrap().as_str().unwrap();
        let item = Pin {
            object_id: "item".into(),
            revision: 1,
        };
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend([0; 24]);
        let mut ogg = b"OggS".to_vec();
        ogg.extend([0; 27]);
        let mut name: ivy_contracts::JsString = "photo".into();
        name.0.push(0xd800);
        name.0.extend(".png".encode_utf16());
        let files = [
            (name, "image/png", png),
            ("voice.ogg".into(), "audio/ogg", ogg),
            (
                "large.txt".into(),
                "text/plain",
                vec![b'x'; 6 * 1024 * 1024],
            ),
            ("after.txt".into(), "text/plain", b"complete".to_vec()),
        ];
        let context = Context {
            coverage: Pin {
                object_id: "coverage".into(),
                revision: 1,
            },
            coverage_contract: "secretary/media-coverage".into(),
            coverage_hash: format!("sha256:{}", "0".repeat(64)),
            item: item.clone(),
            source_id: "mail".into(),
            gap: (mode == "gap").then(|| "source_gap".into()),
            attachments: if matches!(mode.as_str(), "empty" | "gap") {
                vec![]
            } else {
                files
                    .iter()
                    .enumerate()
                    .map(|(i, (name, mime, _))| Attachment {
                        name: Some(name.clone()),
                        media_type: Some((*mime).into()),
                        manifest: (!matches!(mode.as_str(), "missing-manifest" | "missing-error"))
                            .then(|| Pin {
                                object_id: format!("manifest-{i}"),
                                revision: 1,
                            }),
                        error_code: (mode == "missing-manifest")
                            .then(|| "source_unavailable".into()),
                        representation: if i == 1 {
                            Representation::RenderedPreview
                        } else {
                            Representation::OriginalBytes
                        },
                    })
                    .collect()
            },
        };
        let selected = Pin {
            revision: if mode == "wrong-item" { 2 } else { 1 },
            ..item
        };
        let source_id = "mail".into();
        let mut budgets = Vec::new();
        let result = {
            let future = triage_media::assemble(&context, &selected, &source_id, |i, budget| {
                budgets.push(IvyJson::Array(vec![
                    IvyJson::Number(i as f64),
                    IvyJson::Number(budget as f64),
                ]));
                let (name, mime, bytes) = &files[i];
                let source = Source {
                    name: if mode == "wrong-source" {
                        "substituted".into()
                    } else {
                        name.clone()
                    },
                    media_type: Some((*mime).into()),
                    byte_length: bytes.len(),
                    content_hash: digest(bytes),
                };
                let value = if mode == "missing-input" {
                    Ok(None)
                } else if mode == "video" && i == 0 {
                    Ok(Some(Prepared::Video {
                        source,
                        inputs: ["frame 1", "frame 2"]
                            .map(|text| {
                                IvyJson::object([
                                    ("type", "text".into()),
                                    ("text", text.into()),
                                    ("text_elements", IvyJson::Array(vec![])),
                                ])
                            })
                            .into(),
                    }))
                } else {
                    prepare(&source, bytes, budget as f64).map(Some)
                };
                ready(value)
            });
            match std::pin::pin!(future).poll(&mut PollContext::from_waker(Waker::noop())) {
                Poll::Ready(value) => value,
                Poll::Pending => panic!("fixture readers are immediately ready"),
            }
        };
        let actual = match result {
            Ok(value) => IvyJson::object([
                ("media", value.media),
                (
                    "inputsHash",
                    hash_json(&IvyJson::Array(value.inputs.clone()))
                        .unwrap()
                        .into(),
                ),
                ("inputCount", IvyJson::Number(value.inputs.len() as f64)),
                ("budgets", IvyJson::Array(budgets)),
            ]),
            Err(error) => IvyJson::object([
                ("code", error.code.into()),
                ("budgets", IvyJson::Array(budgets)),
            ]),
        };
        assert_eq!(&actual, case.required("expected").unwrap(), "mode {mode}");
    }
}
