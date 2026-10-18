use super::{Root, compare, field, text, value};
use ivy_agent_manager::staged_files::{CanonicalBase64Decoder, StagedFiles};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    agent_schema::validate_agent,
    hash::digest,
    json::{parse, stringify},
};
use ivy_host_runtime::node_path;
use ivy_sdk::service::InvocationContext;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

fn oracle(input: IvyJson) -> IvyJson {
    const FRAME: usize = 32 * 1024 * 1024;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child = Command::new("node")
        .arg(root.join("tests/fixtures/agent-staged-files-oracle.mjs"))
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(&input, FRAME).unwrap().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    parse(std::str::from_utf8(&result.stdout).unwrap(), FRAME).unwrap()
}
struct CaseRoot {
    root: PathBuf,
    parent: PathBuf,
}
impl Drop for CaseRoot {
    fn drop(&mut self) {
        if self.root.exists() {
            oracle(IvyJson::object([
                ("action", "cleanup".into()),
                ("root", value(&self.root)),
                ("parent", value(&self.parent)),
            ]));
        }
    }
}
fn root(parent: &Path, name: String) -> CaseRoot {
    CaseRoot {
        root: parent.join(name),
        parent: parent.to_path_buf(),
    }
}

#[tokio::test]
async fn staged_file_storage_expiry_output_and_existing_paths_preserve_original_with_agreed_cap() {
    let parent = Root::new();
    let cases = oracle(IvyJson::object([("action", "catalog".into())]));
    let cases = cases.as_array().unwrap();
    assert_eq!(cases.len(), 22);
    for (index, row) in cases.iter().enumerate() {
        let original = root(&parent.path, format!("stage-original-{index}"));
        let expected = oracle(IvyJson::object([
            ("action", "run".into()),
            ("root", value(&original.root)),
            ("case", row.clone()),
        ]));
        let rust = root(&parent.path, format!("stage-rust-{index}"));
        let prepared = oracle(IvyJson::object([
            ("action", "prepare".into()),
            ("root", value(&rust.root)),
            ("case", row.clone()),
        ]));
        let request = field(&prepared, "input").clone();
        let codec = field(&prepared, "codec").clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let data = request
            .get("dataBase64")
            .and_then(IvyJson::as_js_string)
            .cloned();
        // This pair binding qualifies the real storage/expiry/handler domain.
        // Production must supply the separate shared Rust canonical codec.
        let decoder: Arc<CanonicalBase64Decoder> = Arc::new({
            let calls = calls.clone();
            move |encoded: &JsString| {
                calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(Some(encoded), data.as_ref());
                if codec.get("canonical") != Some(&IvyJson::Bool(true)) {
                    return None;
                }
                let length = field(&codec, "byteLength").as_f64().unwrap() as usize;
                Some(if let Some(fill) = codec.get("fill") {
                    vec![fill.as_f64().unwrap() as u8; length]
                } else {
                    field(&codec, "bytes")
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|byte| byte.as_f64().unwrap() as u8)
                        .collect()
                })
            }
        });
        let now = field(&prepared, "now").as_f64().unwrap();
        let owner = Arc::new(StagedFiles {
            root: text(&prepared, "staged"),
            decode: decoder,
            now: Arc::new(move || now),
        });
        let context = InvocationContext::detached(
            "caller".into(),
            None,
            1,
            Default::default(),
            IvyError::new("fixture_cancelled", "Caller cancelled."),
        );
        context.signal.cancel();
        let outcome = match (owner.handler())(request, context).await {
            Ok(result) => {
                validate_agent("StagedFile", &result).unwrap();
                let bytes =
                    std::fs::read(node_path::filesystem_path(&text(&result, "path"))).unwrap();
                assert_eq!(
                    field(&result, "byteLength").as_f64().unwrap() as usize,
                    bytes.len()
                );
                assert_eq!(field(&result, "contentHash"), &digest(&bytes).into());
                IvyJson::object([("result", result)])
            }
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        if text(row, "name") == "invalid-schema-before-codec".into() {
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        let legacy = field(&prepared, "legacy").clone();
        if !legacy.is_null() {
            assert_eq!(text(&legacy, "name").0.len(), 129);
            assert_eq!(
                std::fs::read(node_path::filesystem_path(&text(&legacy, "path"))).unwrap(),
                b"x"
            );
            let result = field(&outcome, "result");
            assert_eq!(text(result, "name").0.len(), 128);
            assert_ne!(result.get("path"), legacy.get("path"));
        }
        let snapshot = oracle(IvyJson::object([
            ("action", "snapshot".into()),
            ("root", value(&rust.root)),
        ]));
        let actual = oracle(IvyJson::object([
            ("action", "normalize".into()),
            ("root", value(&rust.root)),
            (
                "value",
                IvyJson::object([
                    ("outcome", outcome),
                    ("snapshot", snapshot),
                    ("legacy", legacy),
                ]),
            ),
        ]));
        let mut expected = expected;
        expected.remove("originalFailure");
        compare(&actual, &expected, &text(row, "name").to_utf8().unwrap());
    }
}
