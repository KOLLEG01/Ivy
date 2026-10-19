use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::{canonical, parse},
};
use ivy_secretary::media_schema::{MediaSchema, validate_media};
use ivy_secretary::media_spool::{MAXIMUM_BYTES, SpoolJournal, Validation};
use ivy_store::rusqlite::Connection;
use std::{
    io::{BufRead, Write},
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
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
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
struct NativeValidationUnavailable;
impl Validation for NativeValidationUnavailable {
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
fn memory() -> SpoolJournal<NativeValidationUnavailable> {
    SpoolJournal::from_owned_connection(
        Connection::open_in_memory().unwrap(),
        &fixture("owner"),
        NativeValidationUnavailable,
    )
    .unwrap()
}

#[test]
fn admission_reserves_capacity_and_failed_attempts_are_never_retried() {
    let rows = parse(&oracle("validation-cases", None, None), 16 * 1024 * 1024).unwrap();
    let names = [
        MediaSchema::Attachment,
        MediaSchema::Materialization,
        MediaSchema::DownloadEvidence,
        MediaSchema::Admission,
        MediaSchema::Intent,
        MediaSchema::Manifest,
        MediaSchema::ReadPlan,
        MediaSchema::ReadCall,
        MediaSchema::Coverage,
    ];
    for row in rows.as_array().unwrap() {
        let name = row.required("name").unwrap().as_str().unwrap();
        let selected = names.iter().find(|v| v.name() == name).unwrap();
        let input = row.required("input").unwrap().as_str().unwrap();
        let value = parse(&input, 2 * 1024 * 1024).unwrap();
        let actual = match validate_media(*selected, &value) {
            Ok(()) => IvyJson::Null,
            Err(e) => IvyJson::object([
                ("code", e.code.clone().into()),
                ("message", e.message.clone().into()),
                ("outcome", e.outcome.as_str().into()),
            ]),
        };
        assert_eq!(
            canonical(&actual, 4096).unwrap(),
            canonical(row.required("error").unwrap(), 4096).unwrap(),
            "{name}: {}",
            input.chars().take(200).collect::<String>()
        );
    }
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
    let result = SpoolJournal::from_owned_connection(spool.db, &wrong, NativeValidationUnavailable);
    assert_eq!(result.err().unwrap().code, "media_spool_owner_changed");
    for version in [0, 2] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE unrelated(x); PRAGMA user_version={version}"
        ))
        .unwrap();
        assert_eq!(
            SpoolJournal::from_owned_connection(db, &fixture("owner"), NativeValidationUnavailable)
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
                .to_str()
                .unwrap()
                .starts_with("secretary-spool-")
        );
        if let Err(error) = std::fs::remove_dir_all(actual) {
            if !std::thread::panicking() {
                panic!("Owned spool fixture cleanup: {error}");
            }
        }
    }
}
#[test]
fn node_and_rust_cross_open_acquired_and_interrupted_attempts() {
    let root =
        Scratch(std::env::temp_dir().join(format!("secretary-spool-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&root.0).unwrap();
    oracle("create", Some(&root.0), None);
    let mut spool =
        SpoolJournal::open(&root.0, &fixture("owner"), NativeValidationUnavailable).unwrap();
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

fn scratch() -> Scratch {
    let root =
        Scratch(std::env::temp_dir().join(format!("secretary-spool-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&root.0).unwrap();
    root
}
fn owned(root: &std::path::Path) -> IvyResult<SpoolJournal<NativeValidationUnavailable>> {
    SpoolJournal::open(root, &fixture("owner"), NativeValidationUnavailable)
}
#[test]
fn owned_spool_excludes_original_node_and_releases_every_failed_open() {
    let root = scratch();
    let spool = owned(&root.0).unwrap();
    let status = parse(&oracle("open-status", Some(&root.0), None), 1024).unwrap();
    assert_eq!(status.get("code"), Some(&"media_spool_owned".into()));
    assert_eq!(owned(&root.0).err().unwrap().code, "media_spool_owned");
    drop(spool);
    assert_eq!(
        parse(&oracle("open-status", Some(&root.0), None), 1024)
            .unwrap()
            .get("value"),
        Some(&"opened".into())
    );
    let wrong = IvyJson::object([("principalId", "other".into())]);
    assert_eq!(
        SpoolJournal::open(&root.0, &wrong, NativeValidationUnavailable)
            .err()
            .unwrap()
            .code,
        "media_spool_owner_changed"
    );
    let spool = owned(&root.0).unwrap();
    drop(spool);
    let db = Connection::open(root.0.join("media.sqlite")).unwrap();
    db.execute_batch("PRAGMA user_version=2").unwrap();
    drop(db);
    assert_eq!(
        owned(&root.0).err().unwrap().code,
        "media_spool_format_unsupported"
    );
    let db = Connection::open(root.0.join("media.sqlite")).unwrap();
    db.execute_batch("PRAGMA user_version=1").unwrap();
    drop(db);
    let spool = owned(&root.0).unwrap();
    drop(spool);
    let mut command = Command::new("node");
    command
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args(["tests/secretary-rust/spool-oracle.mjs", "hold"])
        .arg(&root.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "held");
    let blocked = owned(&root.0).err().unwrap().code;
    child.stdin.take().unwrap().write_all(b"close").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(blocked, "media_spool_owned");
    let spool = owned(&root.0).unwrap();
    drop(spool);
    println!(
        "owned-spool: actual Rust/Node exclusion in both directions; failed owner/format opens release the original transaction"
    );
}

#[test]
fn owned_spool_process_exit_fixture() {
    let Ok(root) = std::env::var("IVY_SECRETARY_SPOOL_CHILD_ROOT") else {
        return;
    };
    let input = parse(
        &std::env::var("IVY_SECRETARY_SPOOL_CHILD_INPUT").unwrap(),
        1024 * 1024,
    )
    .unwrap();
    let mut spool = SpoolJournal::open(
        std::path::Path::new(&root),
        input.required("owner").unwrap(),
        NativeValidationUnavailable,
    )
    .unwrap();
    spool
        .prepare(
            input.required("intent").unwrap(),
            input.required("materialization").unwrap(),
        )
        .unwrap();
    assert!(spool.begin_acquire(&"rust-process-cut".into()).unwrap());
    std::process::exit(42);
}
#[test]
fn owned_spool_process_exit_releases_lock_and_never_retries_interrupted_acquisition() {
    let root = scratch();
    let mut intent = fixture("intent");
    intent.insert("operationId", "rust-process-cut".into());
    let input = IvyJson::object([
        ("owner", fixture("owner")),
        ("intent", intent),
        ("materialization", fixture("materialization")),
    ]);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "owned_spool_process_exit_fixture", "--nocapture"])
        .env("IVY_SECRETARY_SPOOL_CHILD_ROOT", &root.0)
        .env(
            "IVY_SECRETARY_SPOOL_CHILD_INPUT",
            canonical(&input, 1024 * 1024).unwrap(),
        );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut spool = owned(&root.0).unwrap();
    let id = "rust-process-cut".into();
    let original = spool.get(&id).unwrap().unwrap();
    assert_eq!(original.phase, "failed");
    assert_eq!(
        original.error_code.as_deref(),
        Some("media_download_interrupted")
    );
    assert!(!spool.begin_acquire(&id).unwrap());
    assert_eq!(spool.get(&id).unwrap().unwrap(), original);
    drop(spool);
    let original = parse(
        &oracle("verify-rust-interrupted", Some(&root.0), None),
        1024 * 1024,
    )
    .unwrap();
    assert_eq!(original.get("phase"), Some(&"failed".into()));
    println!(
        "owned-spool: actual exit(42), OS-released owner transaction, original Node recovery, zero replacement acquisition"
    );
}

#[test]
fn owned_spool_rejects_linked_ancestors_and_substituted_direct_files() {
    let root = scratch();
    assert_eq!(
        owned(std::path::Path::new("relative-spool"))
            .err()
            .unwrap()
            .code,
        "media_spool_path_invalid"
    );
    assert_eq!(
        parse(
            &oracle("open-status", Some(&PathBuf::from("relative-spool")), None),
            1024
        )
        .unwrap()
        .get("code"),
        Some(&"media_spool_path_invalid".into())
    );
    let target = root.0.join("directory");
    std::fs::create_dir(&target).unwrap();
    let linked = root.0.join("linked");
    ivy_host_runtime::filesystem::directory_link(&target, &linked).unwrap();
    assert_eq!(owned(&linked).err().unwrap().code, "path_unsafe");
    assert_eq!(
        parse(&oracle("open-status", Some(&linked), None), 1024)
            .unwrap()
            .get("code"),
        Some(&"path_unsafe".into())
    );
    let below = linked.join("nested");
    assert_eq!(owned(&below).err().unwrap().code, "path_unsafe");
    assert_eq!(
        parse(&oracle("open-status", Some(&below), None), 1024)
            .unwrap()
            .get("code"),
        Some(&"path_unsafe".into())
    );
    for name in [
        "owner.sqlite",
        "media.sqlite",
        "media.sqlite-wal",
        "media.sqlite-shm",
    ] {
        for kind in ["directory", "hardlink"] {
            let local = root.0.join(format!("{name}-{kind}"));
            std::fs::create_dir(&local).unwrap();
            let path = local.join(name);
            let original = local.join("retained");
            std::fs::write(&original, b"unchanged original bytes").unwrap();
            if kind == "directory" {
                std::fs::create_dir(&path).unwrap();
            } else {
                std::fs::hard_link(&original, &path).unwrap();
            }
            assert_eq!(
                owned(&local).err().unwrap().code,
                "media_spool_path_invalid",
                "{name}/{kind}"
            );
            assert_eq!(
                parse(&oracle("open-status", Some(&local), None), 1024)
                    .unwrap()
                    .get("code"),
                Some(&"media_spool_path_invalid".into()),
                "{name}/{kind}"
            );
            assert_eq!(
                std::fs::read(&original).unwrap(),
                b"unchanged original bytes"
            );
        }
    }
    let spool = owned(&target).unwrap();
    assert!(
        !std::fs::metadata(target.join("owner.sqlite"))
            .unwrap()
            .permissions()
            .readonly()
    );
    assert!(
        !std::fs::metadata(target.join("media.sqlite"))
            .unwrap()
            .permissions()
            .readonly()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The existing root is not silently chmodded; newly created roots use0700.
        let private = root.0.join("new-private");
        let second = owned(&private).unwrap();
        assert_eq!(
            std::fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["owner.sqlite", "media.sqlite"] {
            assert_eq!(
                std::fs::metadata(private.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(second);
    }
    drop(spool);
    println!(
        "owned-spool: 11 original/Rust path refusals, actual directory-link ancestors and four DB paths x directory/hardlink substitutions; original targets unchanged"
    );
}
