use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::{canonical, parse},
};
use ivy_secretary::media_spool::{MAXIMUM_BYTES, SpoolJournal, Validation};
use ivy_store::rusqlite::Connection;
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::OnceLock,
};

fn oracle(mode: &str, root: Option<&PathBuf>, input: Option<&str>) -> String {
    let mut command = Command::new("node");
    command
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/secretary-rust/spool-oracle.mjs")
        .arg(mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = root {
        command.arg(root);
    }
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}
fn fixtures() -> &'static IvyJson {
    static FIXTURE: OnceLock<IvyJson> = OnceLock::new();
    FIXTURE.get_or_init(|| parse(&oracle("fixtures", None, None), 1_048_576).unwrap())
}
fn fixture(name: &str) -> IvyJson {
    fixtures().required(name).unwrap().clone()
}
struct OracleValidation;
impl Validation for OracleValidation {
    fn media(&self, name: &str, value: &IvyJson) -> IvyResult<()> {
        let request = IvyJson::object([("name", name.into()), ("value", value.clone())]);
        oracle("validate", None, Some(&canonical(&request, 1_048_576)?));
        Ok(())
    }
    fn operation(&self, _: &IvyJson) -> IvyResult<()> {
        Err(IvyError::new(
            "test_scope",
            "Native validation is outside these acquisition tests.",
        ))
    }
    fn progress(&self, _: &IvyJson, _: &IvyJson) -> IvyResult<()> {
        Err(IvyError::new(
            "test_scope",
            "Native validation is outside these acquisition tests.",
        ))
    }
}
fn memory() -> SpoolJournal<OracleValidation> {
    SpoolJournal::from_owned_connection(
        Connection::open_in_memory().unwrap(),
        &fixture("owner"),
        OracleValidation,
    )
    .unwrap()
}

#[test]
fn admission_reserves_capacity_and_failed_attempts_are_never_retried() {
    let mut spool = memory();
    let mut intent = fixture("intent");
    let materialization = fixture("materialization");
    for n in 0..15 {
        intent.insert("operationId", format!("reserved-{n}").into());
        spool.prepare(&intent, &materialization).unwrap();
    }
    assert!(spool.used_bytes().unwrap() > 15 * MAXIMUM_BYTES);
    assert_eq!(
        spool
            .prepare(&fixture("intent"), &materialization)
            .unwrap_err()
            .code,
        "media_spool_full"
    );
    let id = "reserved-0".into();
    assert!(spool.begin_acquire(&id).unwrap());
    assert!(!spool.begin_acquire(&id).unwrap());
    assert_eq!(spool.ensure_idle().unwrap_err().code, "media_spool_busy");
    spool.fail_acquire(&id, "media_download_cancelled").unwrap();
    spool.ensure_idle().unwrap();
    assert!(!spool.begin_acquire(&id).unwrap());
    assert_eq!(
        spool.payload(&id).unwrap_err().code,
        "media_spool_payload_unavailable"
    );
    spool.prepare(&fixture("intent"), &materialization).unwrap();
}

#[test]
fn format_gate_owner_identity_and_non_strict_schema_are_preserved() {
    let spool = memory();
    assert_eq!(
        spool
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        spool
            .db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        4
    );
    assert_eq!(
        spool
            .db
            .query_row(
                "SELECT sum(strict) FROM pragma_table_list WHERE schema='main'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let wrong = IvyJson::object([("principalId", "other".into())]);
    let result = SpoolJournal::from_owned_connection(spool.db, &wrong, OracleValidation);
    assert_eq!(result.err().unwrap().code, "media_spool_owner_changed");
    for version in [0, 2] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE unrelated(x); PRAGMA user_version={version}"
        ))
        .unwrap();
        assert_eq!(
            SpoolJournal::from_owned_connection(db, &fixture("owner"), OracleValidation)
                .err()
                .unwrap()
                .code,
            "media_spool_format_unsupported"
        );
    }
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn node_and_rust_cross_open_acquired_and_interrupted_attempts() {
    let root =
        Scratch(std::env::temp_dir().join(format!("secretary-spool-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&root.0).unwrap();
    oracle("create", Some(&root.0), None);
    let db = Connection::open(root.0.join("media.sqlite")).unwrap();
    let mut spool =
        SpoolJournal::from_owned_connection(db, &fixture("owner"), OracleValidation).unwrap();
    let original = fixture("intent")
        .required("operationId")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    assert_eq!(spool.payload(&original).unwrap().1, [0, 1, 254, 255]);
    assert_eq!(
        spool
            .get(&"interrupted".into())
            .unwrap()
            .unwrap()
            .error_code
            .as_deref(),
        Some("media_download_interrupted")
    );
    let mut intent = fixture("intent");
    intent.insert("operationId", "rust-original".into());
    spool.prepare(&intent, &fixture("materialization")).unwrap();
    let id = "rust-original".into();
    assert!(spool.begin_acquire(&id).unwrap());
    spool.fail_acquire(&id, "media_download_cancelled").unwrap();
    drop(spool);
    oracle("verify", Some(&root.0), None);
}
