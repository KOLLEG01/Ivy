use super::*;
use base64::Engine;
use ivy_contracts::json::canonical;
use ivy_secretary::{
    import_state::{self, runtime_journal},
    journal::Limits,
};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn directory() -> Directory {
    let p = Directory(
        std::env::temp_dir().join(format!("secretary-admission-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&p.0).unwrap();
    p
}
fn identity() -> IvyJson {
    IvyJson::object([
        (
            "scope",
            IvyJson::object([
                ("rootObjectId", "fixture-root".into()),
                ("secretaryId", "fixture".into()),
            ]),
        ),
        ("principalId", "fixture-principal".into()),
        ("serviceNodeId", "fixture.secretary".into()),
    ])
}
fn command(script: &str) -> Command {
    let mut c = Command::new("node");
    c.arg(script)
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    c
}
#[test]
fn retained_admission_requires_sealed_backup_original_identity_and_completed_import() {
    let mut oracle = command("tests/secretary-rust/journal-import-oracle.mjs");
    let output = oracle.output().unwrap();
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
    let row = &corpus.required("rows").unwrap().as_array().unwrap()[0];
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(
            row.required("input")
                .unwrap()
                .required("coreBase64")
                .unwrap()
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let directory = directory();
    let path = directory.0.join("secretary.sqlite");
    std::fs::write(&path, &bytes).unwrap();
    let original = identity();
    assert_eq!(
        runtime_journal(&path, Limits::default(), &original)
            .err()
            .unwrap()
            .code,
        "secretary_import_required"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "Refused normal startup cannot rewrite retained data"
    );
    let operation: ivy_contracts::JsString = "fixture:1800000000000:import".into();
    let prepared = import_state::prepare(
        &path,
        Limits::default(),
        &original,
        &operation,
        1800000000000,
    )
    .unwrap();
    let state = prepared.state.as_ref().unwrap();
    assert_eq!(state.get("stage"), Some(&"history-heads".into()));
    assert!(!import_state::current(&prepared.journal, &original).unwrap());
    let name = state
        .required("source")
        .unwrap()
        .required("manifest")
        .unwrap()
        .required("filename")
        .unwrap()
        .as_str()
        .unwrap();
    let backup = directory.0.join(&name);
    let source = import_state::open_source(&backup).unwrap();
    assert_eq!(
        canonical(
            &super::execution_import_cases::snapshot(&source),
            32 * 1024 * 1024
        )
        .unwrap(),
        canonical(
            row.required("expected")
                .unwrap()
                .required("before")
                .unwrap(),
            32 * 1024 * 1024
        )
        .unwrap(),
        "Every original schema/type/history/blob must be sealed before conversion"
    );
    assert_eq!(
        source
            .execute("DELETE FROM documents", [])
            .unwrap_err()
            .sqlite_error_code(),
        Some(ivy_store::rusqlite::ErrorCode::ReadOnly)
    );
    drop(source);
    drop(prepared);
    assert_eq!(
        import_state::prepare(
            &path,
            Limits::default(),
            &original,
            &"fixture:1800000000000:other".into(),
            1800000000001
        )
        .err()
        .unwrap()
        .code,
        "secretary_import_identity_conflict"
    );
    let mut foreign = original.clone();
    foreign.insert("principalId", "foreign".into());
    assert_eq!(
        import_state::prepare(
            &path,
            Limits::default(),
            &foreign,
            &operation,
            1800000000001
        )
        .err()
        .unwrap()
        .code,
        "secretary_import_identity_conflict"
    );
    let resumed = import_state::prepare(
        &path,
        Limits::default(),
        &original,
        &operation,
        1800000000001,
    )
    .unwrap();
    assert_eq!(
        resumed.state.as_ref().unwrap().get("at"),
        Some(&IvyJson::Number(1800000000000.0))
    );
    drop(resumed);
    let mut changed = std::fs::read(&backup).unwrap();
    changed[100] ^= 1;
    std::fs::write(&backup, &changed).unwrap();
    assert_eq!(
        import_state::prepare(
            &path,
            Limits::default(),
            &original,
            &operation,
            1800000000001
        )
        .err()
        .unwrap()
        .code,
        "secretary_import_backup_changed"
    );
    assert_eq!(std::fs::read(&backup).unwrap(), changed);
    assert_eq!(
        runtime_journal(&path, Limits::default(), &original)
            .err()
            .unwrap()
            .code,
        "secretary_import_required"
    );
    let fresh = directory.0.join("fresh.sqlite");
    let admitted = runtime_journal(&fresh, Limits::default(), &original).unwrap();
    assert!(import_state::current(&admitted, &original).unwrap());
    drop(admitted);
    assert!(runtime_journal(&fresh, Limits::default(), &original).is_ok());
    assert_eq!(
        runtime_journal(&fresh, Limits::default(), &foreign)
            .err()
            .unwrap()
            .code,
        "secretary_import_identity_conflict"
    );
}
#[test]
fn original_open_wal_owner_blocks_import_until_its_connection_is_closed() {
    let directory = directory();
    let path = directory.0.join("secretary.sqlite");
    let mut holder = command("tests/secretary-rust/import-owner-holder.mjs");
    holder
        .arg(&directory.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut process = holder.spawn().unwrap();
    let mut reader = BufReader::new(process.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "ready");
    let result = import_state::prepare(
        &path,
        Limits::default(),
        &identity(),
        &"fixture:1800000000000:owner".into(),
        1800000000000,
    );
    let refused = result.err().map(|error| error.code);
    process.stdin.take().unwrap().write_all(b"close\n").unwrap();
    assert!(process.wait().unwrap().success());
    assert_eq!(
        refused.as_deref(),
        Some("storage_unavailable"),
        "An idle original WAL connection must exclude the new importer before any backup or conversion"
    );
    assert_eq!(
        std::fs::read_dir(&directory.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e
                .file_name()
                .to_string_lossy()
                .starts_with("secretary-import-"))
            .count(),
        0
    );
    let prepared = import_state::prepare(
        &path,
        Limits::default(),
        &identity(),
        &"fixture:1800000000000:owner".into(),
        1800000000000,
    )
    .unwrap();
    assert!(prepared.state.is_some());
    drop(prepared);
}
