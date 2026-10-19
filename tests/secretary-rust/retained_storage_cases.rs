use super::retained_import_cases as imported;
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    JsString,
    agent_schema::validate_agent,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::canonical,
};
use ivy_sdk::{native_evidence::validate_native_progress, service::ServiceConnection};
use ivy_secretary::{
    import_state,
    journal::{Journal, Limits},
    media_input::{self, Prepared, Source},
    media_spool::{SpoolJournal, Validation},
    retained_import::{ImportContext, step},
    triage_schema::{self, TriageSchema},
};
use ivy_store::rusqlite::{Connection, params};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture {
    _process: Process,
    directory: Directory,
    data: IvyJson,
    bytes: Vec<u8>,
    address: String,
}
fn fixture(storage: bool) -> Fixture {
    let directory = Directory(std::env::temp_dir().join(format!(
        "secretary-retained-storage-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&directory.0).unwrap();
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/retained-import-fixture.mjs")
        .arg(directory.0.join("original"));
    if storage {
        command.arg("storage");
    }
    command
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut process = Process(command.spawn().unwrap());
    let mut line = String::new();
    BufReader::new(process.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let data = parse(&line, 32 * 1024 * 1024).unwrap();
    let bytes = STANDARD
        .decode(data.required("coreBase64").unwrap().as_str().unwrap())
        .unwrap();
    let address = format!(
        "http://127.0.0.1:{}",
        data.required("port").unwrap().as_f64().unwrap() as u16
    );
    Fixture {
        _process: process,
        directory,
        data,
        bytes,
        address,
    }
}
struct NativeValidation;
fn same(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        canonical(actual, 32 * 1024 * 1024).unwrap(),
        canonical(expected, 32 * 1024 * 1024).unwrap()
    );
}
impl Validation for NativeValidation {
    fn operation(&self, value: &IvyJson) -> ivy_contracts::IvyResult<()> {
        validate_agent("Operation", value)
    }
    fn progress(&self, previous: &IvyJson, next: &IvyJson) -> ivy_contracts::IvyResult<()> {
        validate_native_progress(previous, next, None)
    }
}
fn check_spool(spool: &mut SpoolJournal<NativeValidation>, saved: &IvyJson) {
    let id = saved
        .required("intent")
        .unwrap()
        .required("operationId")
        .unwrap()
        .as_js_string()
        .unwrap();
    let (attempt, bytes) = spool.payload(id).unwrap();
    assert_eq!(
        bytes,
        STANDARD
            .decode(saved.required("payloadBase64").unwrap().as_str().unwrap())
            .unwrap()
    );
    same(&attempt.intent, saved.required("intent").unwrap());
    assert!(
        !spool.begin_acquire(id).unwrap(),
        "An acquired original is not another GET admission"
    );
    same(
        &spool.materialization(id).unwrap(),
        saved.required("materialization").unwrap(),
    );
    assert_eq!(
        spool
            .get(&"prepared-acquisition".into())
            .unwrap()
            .unwrap()
            .phase,
        "prepared"
    );
    assert_eq!(
        spool
            .get(&"interrupted-acquisition".into())
            .unwrap()
            .unwrap()
            .error_code
            .as_deref(),
        Some("media_download_interrupted")
    );
    assert!(
        !spool
            .begin_acquire(&"interrupted-acquisition".into())
            .unwrap()
    );
    assert!(!spool.begin_acquire(&"failed-acquisition".into()).unwrap());
    same(
        &spool
            .read_plan(saved.required("plan").unwrap().required("item").unwrap())
            .unwrap()
            .unwrap(),
        saved.required("plan").unwrap(),
    );
    let call = saved.required("call").unwrap();
    let native = spool
        .native_call(&hash_json(saved.required("plan").unwrap()).unwrap(), 0)
        .unwrap()
        .unwrap();
    same(&native.call, call);
    same(
        native.seen.as_ref().unwrap(),
        saved.required("operation").unwrap(),
    );
    assert!(native.observation.is_none());
    let evidence = attempt.evidence.unwrap();
    let source = Source {
        name: saved
            .required("materialization")
            .unwrap()
            .required("attachment")
            .unwrap()
            .required("name")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        media_type: Some("text/plain".into()),
        byte_length: bytes.len(),
        content_hash: evidence.required("contentHash").unwrap().as_str().unwrap(),
    };
    match media_input::prepare(&source, &bytes, 6.0 * 1024.0 * 1024.0).unwrap() {
        Prepared::Input { input, .. } => assert_eq!(
            input.get("text"),
            Some(&std::str::from_utf8(&bytes).unwrap().into())
        ),
        other => panic!("Original complete text must remain usable: {other:?}"),
    };
}
#[tokio::test]
async fn core_import_keeps_distinct_spool_bytes_native_receipts_and_dormant_work() {
    let fixture = fixture(true);
    let data = &fixture.data;
    let saved = data.required("storage").unwrap();
    let folder = fixture.directory.0.join("current");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("secretary.sqlite");
    std::fs::write(&path, &fixture.bytes).unwrap();
    let spool_folder = folder.join("outlook-media");
    std::fs::create_dir(&spool_folder).unwrap();
    for file in ["media.sqlite", "owner.sqlite"] {
        std::fs::copy(
            fixture
                .directory
                .0
                .join("original/outlook-media")
                .join(file),
            spool_folder.join(file),
        )
        .unwrap();
    }
    let media_path = spool_folder.join("media.sqlite");
    let original_bytes = std::fs::read(&media_path).unwrap();
    let client = imported::connect(&fixture.address).await;
    let result = imported::attempt(
        &path,
        data,
        &client,
        RequestOptions::default(),
        false,
        false,
        "",
    )
    .await;
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(result.current);
    let hive = imported::request(&client, "fixture.state", imported::empty()).await;
    imported::assert_no_effects(&hive);
    imported::complete_state(data, &path, &hive);
    assert_eq!(
        std::fs::read(&media_path).unwrap(),
        original_bytes,
        "Core conversion must not open, checkpoint or migrate the separate dormant spool"
    );
    let identity = import_state::import_identity(
        data.required("settings").unwrap(),
        &"fixture.secretary".into(),
    )
    .unwrap();
    let journal = import_state::runtime_journal(&path, Limits::default(), &identity).unwrap();
    for (key, id, name) in [
        ("secretary/triage-work", "retained-triage-work", "work"),
        (
            "secretary/triage-thread",
            "retained-triage-thread",
            "thread",
        ),
    ] {
        let row = journal.read(key, id, None, true).unwrap();
        same(&row.value, saved.required(name).unwrap());
        triage_schema::validate(
            if name == "work" {
                TriageSchema::Work
            } else {
                TriageSchema::Thread
            },
            &row.value,
        )
        .unwrap();
        assert_eq!(row.pin.revision, 3);
    }
    let params = journal
        .blob(
            "secretary/triage-native-parameters",
            "retained-triage-params",
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        params.1,
        STANDARD
            .decode(saved.required("nativeBase64").unwrap().as_str().unwrap())
            .unwrap()
    );
    assert_eq!(
        journal
            .blob("secretary/triage-read", "retained-triage-attachment", None)
            .unwrap()
            .unwrap()
            .1,
        [0, 255, 0, 216, 0]
    );
    assert_eq!(
        journal
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        37
    );
    drop(journal);
    // This leaf journal test supplies the original owner.sqlite exclusion. Runtime
    // directory/permission admission is a separate host integration qualification.
    let owner = Connection::open(spool_folder.join("owner.sqlite")).unwrap();
    owner
        .execute_batch("PRAGMA busy_timeout=0;BEGIN EXCLUSIVE")
        .unwrap();
    let mut spool = SpoolJournal::from_owned_connection(
        Connection::open(&media_path).unwrap(),
        saved.required("owner").unwrap(),
        NativeValidation,
    )
    .unwrap();
    check_spool(&mut spool, saved);
    let before = super::execution_import_cases::snapshot(&spool.db);
    let original = saved.required("before").unwrap();
    for table in original.required("tables").unwrap().as_array().unwrap() {
        if table.get("name") == Some(&"attempts".into()) {
            continue;
        }
        assert_eq!(
            before
                .required("tables")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t.get("name") == table.get("name")),
            Some(table)
        );
    }
    let attempt_rows = |snapshot: &IvyJson| {
        snapshot
            .required("tables")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get("name") == Some(&"attempts".into()))
            .unwrap()
            .required("rows")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec()
    };
    let interrupted_hex = "interrupted-acquisition"
        .as_bytes()
        .iter()
        .map(|v| format!("{v:02X}"))
        .collect::<String>();
    let original_rows = attempt_rows(original);
    let current_rows = attempt_rows(&before);
    assert_eq!(original_rows.len(), current_rows.len());
    for row in original_rows {
        if row.as_array().unwrap()[3] != IvyJson::from(interrupted_hex.clone()) {
            assert!(
                current_rows.contains(&row),
                "Only the interrupted acquisition may change during reopen"
            );
        }
    }
    assert_eq!(
        spool
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    spool.db.execute_batch("CREATE TRIGGER abort_media_retirement BEFORE DELETE ON read_plans BEGIN SELECT RAISE(ABORT,'fixture_retirement_abort');END;").unwrap();
    let rollback_before = super::execution_import_cases::snapshot(&spool.db);
    assert!(
        spool
            .archive_published(saved.required("publishedPlan").unwrap())
            .is_err()
    );
    assert_eq!(
        super::execution_import_cases::snapshot(&spool.db),
        rollback_before
    );
    spool
        .db
        .execute_batch("DROP TRIGGER abort_media_retirement")
        .unwrap();
    spool
        .archive_published(saved.required("publishedPlan").unwrap())
        .unwrap();
    assert!(
        spool
            .get(&"published-acquisition".into())
            .unwrap()
            .is_none()
    );
    assert!(
        spool
            .read_plan(
                saved
                    .required("publishedPlan")
                    .unwrap()
                    .required("item")
                    .unwrap()
            )
            .unwrap()
            .is_none()
    );
    assert!(
        spool
            .native_call(
                &hash_json(saved.required("publishedPlan").unwrap()).unwrap(),
                38
            )
            .unwrap()
            .is_none()
    );
    check_spool(&mut spool, saved);
    drop(spool);
    drop(owner);
    let owner = Connection::open(spool_folder.join("owner.sqlite")).unwrap();
    owner
        .execute_batch("PRAGMA busy_timeout=0;BEGIN EXCLUSIVE")
        .unwrap();
    let mut spool = SpoolJournal::from_owned_connection(
        Connection::open(&media_path).unwrap(),
        saved.required("owner").unwrap(),
        NativeValidation,
    )
    .unwrap();
    check_spool(&mut spool, saved);
    let call = saved.required("call").unwrap();
    let mut terminal = saved.required("operation").unwrap().clone();
    terminal.insert("phase", "succeeded".into());
    terminal.insert("epoch", "original-media-epoch".into());
    terminal.insert("requestId", IvyJson::Number(8.0));
    terminal.insert(
        "reply",
        IvyJson::object([("result", IvyJson::object([("retained", true.into())]))]),
    );
    let completed = spool.observe_native_call(call, &terminal).unwrap();
    assert_eq!(completed.observation.as_ref(), Some(&terminal));
    let repeated = spool.observe_native_call(call, &terminal).unwrap();
    same(
        repeated.observation.as_ref().unwrap(),
        completed.observation.as_ref().unwrap(),
    );
    let before_bad = super::execution_import_cases::snapshot(&spool.db);
    let mut changed = terminal.clone();
    changed.insert("operationId", "replacement-original-id".into());
    assert_eq!(
        spool.observe_native_call(call, &changed).unwrap_err().code,
        "media_native_intent_mismatch"
    );
    assert_eq!(
        super::execution_import_cases::snapshot(&spool.db),
        before_bad
    );
    spool.ensure_idle().unwrap();
    drop(spool);
    drop(owner);
    let metadata = folder.join("storage-fixture.json");
    std::fs::write(&metadata, canonical(saved, 32 * 1024 * 1024).unwrap()).unwrap();
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/retained-storage-fixture.mjs")
        .arg("verify")
        .arg(&spool_folder)
        .arg(&metadata)
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
    client.close();
    println!(
        "{}",
        canonical(
            &IvyJson::object([
                ("case", "core-separate-spool".into()),
                ("coreVersion", IvyJson::Number(37.0)),
                ("spoolVersion", IvyJson::Number(1.0)),
                ("originalGets", saved.required("gets").unwrap().clone()),
                ("coreImportLeftSpoolByteEqual", true.into()),
                ("rollback", true.into()),
                (
                    "originalNativeId",
                    call.required("operationId").unwrap().clone()
                ),
                ("originalNodeReopen", true.into())
            ]),
            4096
        )
        .unwrap()
    );
}

fn history_fingerprint(db: &Connection) -> (i64, i64, String) {
    let mut statement=db.prepare("SELECT id,revision,typeof(value_json),value_json FROM technical_revisions ORDER BY id,revision").unwrap();
    let mut rows = statement.query([]).unwrap();
    let mut pins = vec![];
    let mut count = 0;
    let mut bytes = 0;
    while let Some(row) = rows.next().unwrap() {
        let id: String = row.get(0).unwrap();
        let revision: i64 = row.get(1).unwrap();
        let kind: String = row.get(2).unwrap();
        let raw: String = row.get(3).unwrap();
        count += 1;
        bytes += raw.len() as i64;
        pins.push(IvyJson::Array(vec![
            id.into(),
            IvyJson::Number(revision as f64),
            kind.into(),
            digest(raw.as_bytes()).into(),
        ]));
    }
    (count, bytes, hash_json(&IvyJson::Array(pins)).unwrap())
}
fn seed_limit(path: &Path, byte_limit: bool) -> (i64, i64, String) {
    let journal = Journal::open(path, Limits::default()).unwrap();
    let usage = journal.usage().unwrap();
    let raw: String = journal
        .db
        .query_row(
            "SELECT value_json FROM technical_documents WHERE id='main'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let tx = journal.db.unchecked_transaction().unwrap();
    journal
        .db
        .execute(
            "DELETE FROM technical_revisions WHERE id='main' AND revision=3",
            [],
        )
        .unwrap();
    let mut revision = 3;
    if byte_limit {
        let mut remaining = Limits::default().bytes - usage.bytes;
        while remaining > 0 {
            let length = remaining.min(1024 * 1024);
            assert!(length >= 2);
            let value = format!("\"{}\"", "x".repeat(length as usize - 2));
            journal
                .db
                .execute(
                    "INSERT INTO technical_revisions VALUES('main',?,?)",
                    params![revision, value],
                )
                .unwrap();
            remaining -= length;
            revision += 1;
        }
    } else {
        for _ in usage.count..Limits::default().count {
            journal
                .db
                .execute(
                    "INSERT INTO technical_revisions VALUES('main',?,?)",
                    params![
                        revision,
                        "{\"retained\":\"\\ud800\",\"operationId\":\"original-history-id\"}"
                    ],
                )
                .unwrap();
            revision += 1;
        }
    }
    journal
        .db
        .execute(
            "INSERT INTO technical_revisions VALUES('main',?,?)",
            params![revision, raw],
        )
        .unwrap();
    journal
        .db
        .execute(
            "UPDATE technical_documents SET current_revision=? WHERE id='main'",
            [revision],
        )
        .unwrap();
    tx.commit().unwrap();
    let usage = journal.usage().unwrap();
    if byte_limit {
        assert_eq!(usage.bytes, 256 * 1024 * 1024);
    } else {
        assert_eq!(usage.count, 10_000);
    }
    history_fingerprint(&journal.db)
}
async fn scale_attempt(
    path: &Path,
    data: &IvyJson,
    client: &ServiceConnection,
    pause: bool,
    expected: &(i64, i64, String),
) -> (bool, IvyJson, usize) {
    let node: JsString = "fixture.secretary".into();
    let settings = data.required("settings").unwrap();
    let identity = import_state::import_identity(settings, &node).unwrap();
    let prepared = import_state::prepare(
        path,
        Limits::default(),
        &identity,
        &"fixture:1800000000000:whole-import".into(),
        data.required("at").unwrap().as_f64().unwrap() as i64,
    )
    .unwrap();
    let mut state = prepared.state.unwrap();
    let source = import_state::open_source(
        &path.parent().unwrap().join(
            state
                .required("source")
                .unwrap()
                .required("manifest")
                .unwrap()
                .required("filename")
                .unwrap()
                .as_str()
                .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(&history_fingerprint(&source), expected);
    let contracts = CheckedNativeContracts::new(path.parent().unwrap().join("contracts"));
    let ctx = ImportContext {
        journal: &prepared.journal,
        source: &source,
        settings,
        service_node_id: &node,
        client: Arc::new(client.clone()),
        retained: &contracts,
        cancellation: RequestOptions::default(),
        page_size: 64,
    };
    let mut steps = 0;
    for _ in 0..1000 {
        let progress = step(&ctx, &mut state).await.unwrap();
        steps += 1;
        if progress.complete {
            break;
        }
        assert!(progress.advanced);
        if pause && progress.stage == "history" && state.get("cursor").is_some_and(|v| !v.is_null())
        {
            break;
        }
    }
    assert_eq!(&history_fingerprint(&source), expected);
    let current = import_state::current(&prepared.journal, &identity).unwrap();
    assert_eq!(current, !pause);
    if current {
        let usage = prepared.journal.usage().unwrap();
        assert!(usage.count < 1000 && usage.bytes < 1024 * 1024);
        assert_eq!(
            prepared
                .journal
                .db
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            37
        );
        assert_eq!(
            prepared
                .journal
                .db
                .query_row(
                    "SELECT length(bytes) FROM technical_blobs WHERE id='retained-blob'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            5
        );
        let mapped: String = prepared
            .journal
            .db
            .query_row(
                "SELECT value FROM journal_state WHERE key LIKE 'secretary/import/map/%/main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let map = parse(&mapped, 1024 * 1024).unwrap();
        let source_count: i64 = source
            .query_row(
                "SELECT count(*) FROM technical_revisions WHERE id='main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let source_bytes:i64=source.query_row("SELECT sum(length(CAST(value_json AS BLOB))) FROM technical_revisions WHERE id='main'",[],|r|r.get(0)).unwrap();
        assert_eq!(
            map.required("history").unwrap().get("count"),
            Some(&IvyJson::Number(source_count as f64))
        );
        assert_eq!(
            map.required("history").unwrap().get("bytes"),
            Some(&IvyJson::Number(source_bytes as f64))
        );
    } else {
        assert!(prepared.journal.usage().unwrap().count < expected.0 + 20);
    }
    (current, state, steps)
}
#[tokio::test]
async fn default_count_and_byte_limits_archive_history_and_resume_without_a_quota_override() {
    let fixture = fixture(false);
    let observer = imported::connect(&fixture.address).await;
    for byte_limit in [false, true] {
        imported::request(&observer, "fixture.reset", imported::empty()).await;
        let folder = fixture.directory.0.join(if byte_limit {
            "default-bytes"
        } else {
            "default-count"
        });
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("secretary.sqlite");
        std::fs::write(&path, &fixture.bytes).unwrap();
        let expected = seed_limit(&path, byte_limit);
        let client = imported::connect(&fixture.address).await;
        let (_, _, first) = scale_attempt(&path, &fixture.data, &client, true, &expected).await;
        client.close();
        imported::request(&observer, "fixture.resume", imported::empty()).await;
        let restarted = imported::connect(&fixture.address).await;
        let (current, state, second) =
            scale_attempt(&path, &fixture.data, &restarted, false, &expected).await;
        assert!(current);
        let hive = imported::request(&observer, "fixture.state", imported::empty()).await;
        imported::assert_no_effects(&hive);
        imported::complete_state(&fixture.data, &path, &hive);
        restarted.close();
        println!(
            "{}",
            canonical(
                &IvyJson::object([
                    (
                        "case",
                        if byte_limit {
                            "default-256MiB"
                        } else {
                            "default-10000"
                        }
                        .into()
                    ),
                    ("historyRows", IvyJson::Number(expected.0 as f64)),
                    ("historyBytes", IvyJson::Number(expected.1 as f64)),
                    ("historyHash", expected.2.clone().into()),
                    ("ownerAttempts", IvyJson::Number(2.0)),
                    ("boundedSteps", IvyJson::Number((first + second) as f64)),
                    ("finalState", state)
                ]),
                1024 * 1024
            )
            .unwrap()
        );
    }
    observer.close();
}

#[tokio::test]
async fn a_full_source_head_set_retains_its_run_cursor_outside_the_record_receipt_count() {
    let fixture = fixture(false);
    let folder = fixture.directory.0.join("default-heads");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("secretary.sqlite");
    std::fs::write(&path, &fixture.bytes).unwrap();
    let journal = Journal::open(&path, Limits::default()).unwrap();
    let raw: String = journal
        .db
        .query_row(
            "SELECT value_json FROM technical_documents WHERE id='context-ready'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let tx = journal.db.unchecked_transaction().unwrap();
    journal.db.execute_batch("DELETE FROM documents;DELETE FROM revisions;DELETE FROM technical_documents;DELETE FROM technical_revisions;DELETE FROM technical_blobs;").unwrap();
    for n in 0..10_000 {
        let id = format!("head-{n:05}");
        journal
            .db
            .execute(
                "INSERT INTO technical_documents VALUES(?,'secretary/follow-up-context',?,1,?)",
                params![id, id, raw],
            )
            .unwrap();
        journal
            .db
            .execute(
                "INSERT INTO technical_revisions VALUES(?,1,?)",
                params![id, raw],
            )
            .unwrap();
    }
    tx.commit().unwrap();
    assert_eq!(journal.usage().unwrap().count, 10_000);
    drop(journal);
    let node: JsString = "fixture.secretary".into();
    let settings = fixture.data.required("settings").unwrap();
    let identity = import_state::import_identity(settings, &node).unwrap();
    let operation: JsString = "fixture:1800000000000:head-limit".into();
    let prepared = import_state::prepare(
        &path,
        Limits::default(),
        &identity,
        &operation,
        1_800_000_000_000,
    )
    .unwrap();
    let mut state = prepared.state.unwrap();
    let source = import_state::open_source(
        &folder.join(
            state
                .required("source")
                .unwrap()
                .required("manifest")
                .unwrap()
                .required("filename")
                .unwrap()
                .as_str()
                .unwrap(),
        ),
    )
    .unwrap();
    let contracts = CheckedNativeContracts::new(folder.join("contracts"));
    let client = imported::connect(&fixture.address).await;
    let ctx = ImportContext {
        journal: &prepared.journal,
        source: &source,
        settings,
        service_node_id: &node,
        client: Arc::new(client.clone()),
        retained: &contracts,
        cancellation: RequestOptions::default(),
        page_size: 64,
    };
    let mut pages = 0;
    while state.get("stage") == Some(&"history-heads".into()) {
        assert!(pages < 160);
        let progress = step(&ctx, &mut state)
            .await
            .expect("The one bounded run cursor must not consume an original record mapping slot");
        assert!(!progress.complete);
        pages += 1;
    }
    assert_eq!(state.get("stage"), Some(&"history".into()));
    assert!(!import_state::current(&prepared.journal, &identity).unwrap());
    assert_eq!(
        prepared
            .journal
            .db
            .query_row(
                "SELECT count(*) FROM journal_state WHERE key LIKE 'secretary/import/map/%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        10_000
    );
    assert_eq!(prepared.journal.usage().unwrap().count, 10_000);
    drop(ctx);
    drop(source);
    drop(prepared.journal);
    let resumed = import_state::prepare(
        &path,
        Limits::default(),
        &identity,
        &operation,
        1_800_000_000_001,
    )
    .unwrap();
    same(resumed.state.as_ref().unwrap(), &state);
    assert!(!import_state::current(&resumed.journal, &identity).unwrap());
    drop(resumed);
    client.close();
    println!(
        "{}",
        canonical(
            &IvyJson::object([
                ("case", "default-10000-head-mappings".into()),
                ("maps", IvyJson::Number(10_000.0)),
                ("pages", IvyJson::Number(pages as f64)),
                ("resumedStage", "history".into()),
                ("current", false.into())
            ]),
            4096
        )
        .unwrap()
    );
}
