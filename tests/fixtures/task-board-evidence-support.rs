use serde_json::Value;
use std::path::Path;

#[path = "reference-data.rs"]
mod reference_data;

#[allow(dead_code)]
pub fn cases(mode: &str) -> Value {
    let bytes: &[u8] = match mode {
        "cases" => include_bytes!("task-board-evidence-cases.json.gz"),
        "decode" => include_bytes!("task-board-evidence-decode-cases.json.gz"),
        _ => panic!("Unknown evidence cases: {mode}"),
    };
    serde_json::from_slice(&reference_data::read(bytes)).unwrap()
}

#[allow(dead_code)]
pub fn original_blobs(path: &Path) -> Value {
    std::fs::write(
        path,
        reference_data::read(include_bytes!("task-board-original-blobs.sqlite.gz")),
    )
    .unwrap();
    serde_json::from_slice(&reference_data::read(include_bytes!(
        "task-board-original-blobs.json.gz"
    )))
    .unwrap()
}

#[allow(dead_code)]
pub fn original_gates(path: &Path) -> Value {
    std::fs::write(
        path,
        reference_data::read(include_bytes!("task-board-original-gates.sqlite.gz")),
    )
    .unwrap();
    serde_json::from_slice(&reference_data::read(include_bytes!(
        "task-board-original-gates.json.gz"
    )))
    .unwrap()
}
