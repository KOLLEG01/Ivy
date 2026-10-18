use super::*;
use ivy_contracts::json::canonical;
use ivy_secretary::registry::{secretary_registry, service_registry, triage_registry};

#[test]
fn original_registry_contracts_tools_and_main_read_scopes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = std::process::Command::new("node");
    command
        .arg("tests/secretary-rust/registry-oracle.mjs")
        .current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .expect("Original registry/main callback oracle required");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        8 * 1024 * 1024,
    )
    .unwrap();
    let compare = |actual: &IvyJson, expected: &IvyJson| {
        assert_eq!(
            canonical(actual, 8 * 1024 * 1024).unwrap(),
            canonical(expected, 8 * 1024 * 1024).unwrap()
        );
    };
    compare(
        &secretary_registry().unwrap(),
        corpus.required("baseline").unwrap(),
    );
    compare(&triage_registry(), corpus.required("dormant").unwrap());
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 6);
    for row in rows {
        compare(
            &service_registry(row.required("root").unwrap()).unwrap(),
            row.required("registry").unwrap(),
        );
    }
}
