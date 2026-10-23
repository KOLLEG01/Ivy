use super::*;
use ivy_contracts::{
    hash::digest,
    json::{canonical, parse},
};

const THREAD: &str = "10000000-0000-4000-8000-000000000001";
const TURN: &str = "10000000-0000-4000-8000-000000000002";
const ARCHIVE_THREAD: &str = "10000000-0000-4000-8000-000000000003";
const ARCHIVE_OPERATION: &str = "10000000-0000-4000-8000-000000000004";
const PRINCIPAL: &str = "prepared-phone-voice";
fn fingerprint() -> String {
    digest("phone-voice-journal-fixture")
}
fn value(text: &str) -> IvyJson {
    parse(text, 4 * 1024 * 1024).unwrap()
}

fn resolve_command_arguments(value: &IvyJson, call: &JsString) -> IvyJson {
    match value {
        IvyJson::String(text) if text == &JsString::from("$call") => call.clone().into(),
        IvyJson::Array(items) => IvyJson::Array(
            items
                .iter()
                .map(|v| resolve_command_arguments(v, call))
                .collect(),
        ),
        IvyJson::Object(items) => IvyJson::object(
            items
                .iter()
                .map(|(k, v)| (k.clone(), resolve_command_arguments(v, call))),
        ),
        _ => value.clone(),
    }
}
fn normalize_command_value(value: &IvyJson, allocated: &[JsString], key: &str) -> IvyJson {
    match value {
        IvyJson::String(text) => {
            if let Some(index) = allocated.iter().position(|id| id == text) {
                return format!("$allocated{index}").into();
            }
            let encoded = text.to_utf8().unwrap();
            if ["createdAt", "updatedAt", "checkedAt"].contains(&key) {
                assert_eq!(encoded.len(), 24);
                assert!(encoded.ends_with('Z'));
                assert_eq!(encoded.as_bytes()[19], b'.');
                assert!(chrono::DateTime::parse_from_rfc3339(&encoded).is_ok());
                return "$timestamp".into();
            }
            if (encoded.starts_with('{') || encoded.starts_with('['))
                && let Ok(decoded) = parse(&encoded, 4 * 1024 * 1024)
            {
                return normalize_command_value(&decoded, allocated, "");
            }
            value.clone()
        }
        IvyJson::Array(items) => IvyJson::Array(
            items
                .iter()
                .map(|v| normalize_command_value(v, allocated, ""))
                .collect(),
        ),
        IvyJson::Object(items) => IvyJson::object(items.iter().map(|(k, v)| {
            (
                k.clone(),
                normalize_command_value(v, allocated, &k.to_utf8().unwrap()),
            )
        })),
        _ => value.clone(),
    }
}
async fn command_step(
    journal: &PhoneJournal,
    action: &str,
    arguments: &IvyJson,
) -> IvyResult<IvyJson> {
    let args = arguments.as_array().unwrap();
    let text = |index: usize| args[index].as_js_string().unwrap().clone();
    let number = |index: usize| args[index].as_f64().unwrap() as u32;
    let optional = |index: usize| args.get(index).and_then(IvyJson::as_js_string).cloned();
    let option_json = |v: Option<IvyJson>| v.unwrap_or(IvyJson::Null);
    let option_text = |v: Option<JsString>| v.map_or(IvyJson::Null, Into::into);
    let empty = |v: ()| {
        let _ = v;
        IvyJson::Null
    };
    match action {
        "beginEpoch" => journal.begin_epoch(text(0)).await.map(empty),
        "loseEpoch" => journal.lose_epoch(text(0)).await.map(empty),
        "admitCall" => journal.admit_call(args[0].clone()).await,
        "call" => journal.call(text(0)).await.map(option_json),
        "callForOperation" => journal.call_for_operation(text(0)).await.map(option_json),
        "currentCalls" => journal.current_calls().await.map(IvyJson::Array),
        "history" => journal
            .history(optional(0), number(1))
            .await
            .map(IvyJson::Array),
        "target" => journal.target(text(0)).await.map(option_json),
        "retainTarget" => journal.retain_target(args[0].clone()).await,
        "discardUnpreparedCall" => journal.discard_unprepared_call(text(0)).await.map(empty),
        "releaseCall" => journal.release_call(text(0), text(1)).await.map(empty),
        "submit" => journal.submit(args[0].clone()).await.map(empty),
        "finish" => journal.finish(args[0].clone(), args[1].clone()).await,
        "operation" => journal.operation(text(0)).await.map(option_json),
        "callCommand" => journal
            .call_command(text(0), text(1), number(2))
            .await
            .map(option_json),
        "epoch" => journal.epoch().await.map(option_text),
        "epochOperations" => journal.epoch_operations().await.map(IvyJson::Number),
        "status" => journal.status().await,
        "archiveStatus" => journal.archive_status().await,
        "finishArchive" => {
            journal
                .finish_archive(args[0].clone(), args[1].clone())
                .await
        }
        "unresolvedArchive" => journal.unresolved_archive().await.map(IvyJson::Bool),
        "resolveArchive" => journal.resolve_archive(text(0)).await,
        "finishVoiceCreation" => {
            journal
                .finish_voice_creation(args[0].clone(), text(1))
                .await
        }
        "finishVoiceBinding" => {
            journal
                .finish_voice_binding(args[0].clone(), optional(1))
                .await
        }
        "finishVoiceStop" => journal.finish_voice_stop(args[0].clone()).await,
        "voiceTask" => journal
            .voice_task(text(0), number(1))
            .await
            .map(option_text),
        "createdVoiceTask" => journal
            .created_voice_task(text(0), number(1))
            .await
            .map(option_text),
        "finishVoiceSelection" => {
            journal
                .finish_voice_selection(args[0].clone(), text(1))
                .await
        }
        "voiceSelectionCommand" => journal
            .voice_selection_command(text(0), number(1))
            .await
            .map(option_json),
        "feedbackCommand" => journal
            .feedback_command(text(0), number(1))
            .await
            .map(option_json),
        "finishVoiceRestart" => {
            journal
                .finish_voice_restart(args[0].clone(), optional(1))
                .await
        }
        "finishSpeakerVoiceSelection" => {
            journal
                .finish_speaker_voice_selection(args[0].clone(), args[1].clone())
                .await
        }
        "selectedSpeakerVoice" => journal
            .selected_speaker_voice(text(0))
            .await
            .map(option_text),
        "fixtureMeta" => {
            let key = text(0).to_utf8().unwrap();
            let value = text(1).to_utf8().unwrap();
            journal
                .request(move |db| db.set_meta(&key, &value))
                .await
                .map(empty)
        }
        "fixtureSql" => {
            let sql = text(0).to_utf8().unwrap();
            journal
                .request(move |db| db.db.execute_batch(&sql).map_err(error))
                .await
                .map(empty)
        }
        _ => panic!("Unknown original command scenario action: {action}"),
    }
}
async fn command_snapshot(journal: &PhoneJournal) -> IvyJson {
    journal
        .request(|db| command_snapshot_db(&db.db))
        .await
        .unwrap()
}
fn command_snapshot_db(db: &Connection) -> IvyResult<IvyJson> {
    use ivy_store::rusqlite::types::ValueRef;
    let mut tables = Vec::new();
    for (name, sql) in [
        ("meta", "SELECT key,value FROM meta ORDER BY key"),
        ("epochs", "SELECT epoch FROM epochs ORDER BY epoch"),
        (
            "calls",
            "SELECT call_id,operation_id,value FROM calls ORDER BY rowid",
        ),
        (
            "targets",
            "SELECT call_id,value FROM targets ORDER BY rowid",
        ),
        ("active", "SELECT call_id FROM active_calls ORDER BY rowid"),
        (
            "commands",
            "SELECT operation_id,epoch,call_id,method,phase,value FROM commands ORDER BY rowid",
        ),
        (
            "archive",
            "SELECT kind,key,value FROM receipt_archive ORDER BY kind,key",
        ),
        (
            "history",
            "SELECT sequence,kind,key FROM receipt_archive_history ORDER BY sequence",
        ),
        (
            "retired",
            "SELECT kind,key,value_hash FROM receipt_archive_retired ORDER BY kind,key",
        ),
        ("usage", "SELECT records,bytes FROM receipt_archive_usage"),
    ] {
        let mut statement = db.prepare(sql).map_err(error)?;
        let rows = statement
            .query_map([], |row| {
                let mut values = Vec::new();
                for index in 0..row.as_ref().column_count() {
                    values.push(match row.get_ref(index)? {
                        ValueRef::Null => IvyJson::Null,
                        ValueRef::Integer(n) => IvyJson::Number(n as f64),
                        ValueRef::Text(s) => String::from_utf8(s.to_vec()).unwrap().into(),
                        other => panic!("Unexpected format-7 fixture column: {other:?}"),
                    });
                }
                Ok(IvyJson::Array(values))
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        tables.push((name, IvyJson::Array(rows)));
    }
    Ok(IvyJson::object(tables))
}

#[tokio::test]
async fn call_command_epoch_compaction_and_receipts_preserve_format7() {
    let originals = value(include_str!(
        "../fixtures/phone-journal-commands-cases.json"
    ));
    let cases = originals.as_array().unwrap();
    assert_eq!(cases.len(), 43);
    for scenario in cases {
        let name = scenario.get("name").unwrap().as_str().unwrap();
        let payload = scenario.get("payload").unwrap();
        let expected = scenario.get("expected").unwrap();
        let root = Root::new();
        let options = payload.get("options").unwrap();
        let mut configuration = root.options();
        for (key, target) in [
            ("maxOperations", &mut configuration.limits.max_operations),
            ("maxBytes", &mut configuration.limits.max_bytes),
            ("maxEpochs", &mut configuration.limits.max_epochs),
            (
                "maxConcurrentCalls",
                &mut configuration.max_concurrent_calls,
            ),
        ] {
            if let Some(number) = options.get(key).and_then(IvyJson::as_f64) {
                *target = number as usize;
            }
        }
        let journal = PhoneJournal::open(configuration).await.unwrap();
        let mut call: JsString = "10000000-0000-4000-8000-000000000005".into();
        let mut allocated = Vec::new();
        let mut observations = Vec::new();
        for (index, step) in payload
            .get("steps")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let action = step.get("action").unwrap().as_str().unwrap();
            let arguments = resolve_command_arguments(step.get("arguments").unwrap(), &call);
            let observed = match command_step(&journal, &action, &arguments).await {
                Ok(value) => {
                    assert_ne!(
                        step.get("storageFault"),
                        Some(&true.into()),
                        "{name}/{index}"
                    );
                    if action == "admitCall" {
                        call = value.get("callId").unwrap().as_js_string().unwrap().clone();
                        if arguments.as_array().unwrap()[0].get("direction")
                            == Some(&"outgoing".into())
                            && !allocated.contains(&call)
                        {
                            allocated.push(call.clone());
                        }
                    }
                    IvyJson::object([
                        ("ok", true.into()),
                        ("value", normalize_command_value(&value, &allocated, "")),
                    ])
                }
                Err(cause) if step.get("storageFault") == Some(&true.into()) => {
                    assert_eq!(cause.code, "storage_unavailable", "{name}/{index}");
                    assert_eq!(cause.outcome, Outcome::Unknown, "{name}/{index}");
                    IvyJson::object([
                        ("ok", false.into()),
                        ("storageFault", true.into()),
                        ("outcome", "unknown".into()),
                    ])
                }
                Err(cause) => IvyJson::object([
                    ("ok", false.into()),
                    ("code", cause.code.into()),
                    ("outcome", cause.outcome.to_json()),
                ]),
            };
            assert_eq!(
                canonical(&observed, 4 * 1024 * 1024).unwrap(),
                canonical(
                    &expected.get("observations").unwrap().as_array().unwrap()[index],
                    4 * 1024 * 1024
                )
                .unwrap(),
                "{name}/{index}/{action}"
            );
            observations.push(observed);
        }
        same(
            &IvyJson::Array(observations),
            expected.get("observations").unwrap(),
        );
        same(
            &normalize_command_value(&command_snapshot(&journal).await, &allocated, ""),
            expected.get("snapshot").unwrap(),
        );
        journal.close().wait().await.unwrap();
        // Independently read the persisted SQLite rows after the worker closes.
        if [
            "outgoing-original-admission-and-reopen",
            "released-call-compaction-keeps-identities",
            "created-bound-stopped-voice-keeps-original-evidence",
            "binding-sql-failure-rolls-back-all-writes",
        ]
        .contains(&name.as_str())
        {
            let readback = command_snapshot_db(
                &Connection::open(root.0.join("phone-commands.sqlite")).unwrap(),
            )
            .unwrap();
            same(
                &normalize_command_value(&readback, &allocated, ""),
                expected.get("snapshot").unwrap(),
            );
            let reopened = PhoneJournal::open(root.options()).await.unwrap();
            same(
                &normalize_command_value(&command_snapshot(&reopened).await, &allocated, ""),
                expected.get("snapshot").unwrap(),
            );
            reopened.close().wait().await.unwrap();
        }
    }
}
fn same(a: &IvyJson, b: &IvyJson) {
    assert_eq!(
        canonical(a, 4 * 1024 * 1024).unwrap(),
        canonical(b, 4 * 1024 * 1024).unwrap()
    );
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let parent = std::env::var_os("IVY_TEST_TEMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = parent.join(format!("ivy-phone-voice-journal-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn options(&self) -> PhoneJournalOptions {
        PhoneJournalOptions {
            data_root: self.0.clone(),
            owner: PhoneJournalOwner {
                host_id: "phone-fixture-host".into(),
                service_node_id: "phone-fixture-node".into(),
            },
            limits: PhoneJournalLimits::default(),
            max_concurrent_calls: 1,
        }
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("ivy-phone-voice-journal-")
        );
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn persisted_snapshot(db: &Connection) -> IvyJson {
    let mut meta = db
        .prepare("SELECT key,value FROM meta ORDER BY key")
        .unwrap();
    let meta = meta
        .query_map([], |row| {
            Ok(IvyJson::object([
                ("key", IvyJson::from(row.get::<_, String>(0)?)),
                ("value", IvyJson::from(row.get::<_, String>(1)?)),
            ]))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut schema=db.prepare("SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").unwrap();
    let schema = schema
        .query_map([], |row| {
            let sql = row
                .get::<_, String>(2)?
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>();
            Ok(IvyJson::object([
                ("type", IvyJson::from(row.get::<_, String>(0)?)),
                ("name", IvyJson::from(row.get::<_, String>(1)?)),
                ("sql", sql.into()),
            ]))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    IvyJson::object([
        (
            "format",
            IvyJson::from(
                db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap() as f64,
            ),
        ),
        ("meta", IvyJson::Array(meta)),
        ("schema", IvyJson::Array(schema)),
    ])
}
fn persisted_meta(root: &Root, key: &str) -> Option<String> {
    Connection::open(root.0.join("phone-commands.sqlite"))
        .unwrap()
        .query_row("SELECT value FROM meta WHERE key=?", [key], |row| {
            row.get(0)
        })
        .optional()
        .unwrap()
}
fn persisted_state(root: &Root, fp: Option<&str>) -> IvyJson {
    let fp = fp.map(str::to_owned).unwrap_or_else(fingerprint);
    let cache = persisted_meta(root, &format!("codexVoice:{PRINCIPAL}:{fp}"))
        .map(|s| value(&s))
        .unwrap_or(IvyJson::Null);
    let reasoning = persisted_meta(root, "voiceReasoning")
        .map(IvyJson::from)
        .unwrap_or(IvyJson::Null);
    let db = Connection::open(root.0.join("phone-commands.sqlite")).unwrap();
    IvyJson::object([
        ("cache", cache),
        ("reasoning", reasoning),
        ("snapshot", persisted_snapshot(&db)),
    ])
}
fn seed_format7(root: &Root) -> IvyJson {
    let db = Connection::open(root.0.join("phone-commands.sqlite")).unwrap();
    db.execute_batch(include_str!("../fixtures/phone-format7.sql"))
        .unwrap();
    IvyJson::object([("snapshot", persisted_snapshot(&db))])
}

fn cache(fingerprint: &str) -> IvyJson {
    IvyJson::object([
        ("fingerprint", fingerprint.into()),
        ("threadId", THREAD.into()),
        ("creationOperationId", IvyJson::Null),
    ])
}
async fn timed<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(12), future)
        .await
        .unwrap()
}

#[tokio::test]
async fn original_owner_lock_format_and_bounds_are_preserved() {
    let root = Root::new();
    let journal = PhoneJournal::open(root.options()).await.unwrap();
    assert_eq!(
        PhoneJournal::open(root.options()).await.err().unwrap().code,
        "phone_owner_already_running"
    );
    journal.close().wait().await.unwrap();
    let mut foreign = root.options();
    foreign.owner.host_id = "another-fixture-host".into();
    assert_eq!(
        PhoneJournal::open(foreign).await.err().unwrap().code,
        "target_conflict"
    );
    let raw = Connection::open(root.0.join("phone-commands.sqlite")).unwrap();
    raw.execute_batch("PRAGMA user_version=6;").unwrap();
    raw.close().unwrap();
    assert_eq!(
        PhoneJournal::open(root.options()).await.err().unwrap().code,
        "unsupported_storage"
    );
    let mut invalid = root.options();
    invalid.max_concurrent_calls = 33;
    assert_eq!(
        PhoneJournal::open(invalid).await.err().unwrap().code,
        "invalid_arguments"
    );
    let mut invalid = root.options();
    invalid.limits.max_bytes = OUTCOME_BYTES - 1;
    assert_eq!(
        PhoneJournal::open(invalid).await.err().unwrap().code,
        "invalid_arguments"
    );
    let mut invalid = root.options();
    invalid.data_root = PathBuf::from("relative-owner");
    assert_eq!(invalid.validate().err().unwrap().code, "invalid_arguments");
    #[cfg(windows)]
    {
        // A drive-relative root cannot select an absolute owned directory.
        let mut rooted = root.options();
        rooted.data_root = PathBuf::from(r"\owned");
        assert_eq!(rooted.validate().err().unwrap().code, "invalid_arguments");
    }
}

#[tokio::test]
async fn format7_fixture_reopens_and_preserves_historical_voice_evidence() {
    let root = Root::new();
    let seed = seed_format7(&root);
    let journal = PhoneJournal::open(root.options()).await.unwrap();
    assert_eq!(journal.data_root(), realpath(&root.0).unwrap());
    same(
        &journal
            .task_cache(PRINCIPAL, &fingerprint())
            .await
            .unwrap()
            .unwrap(),
        &cache(&fingerprint()),
    );
    assert_eq!(
        journal.remembered_voice_reasoning().await.unwrap(),
        Some("medium".into())
    );
    assert!(journal.used_codex_voice_task(THREAD.into()).await.unwrap());
    assert!(
        journal
            .used_codex_voice_task(ARCHIVE_THREAD.into())
            .await
            .unwrap()
    );
    assert!(!journal.used_codex_voice_task(TURN.into()).await.unwrap());
    let bindings = journal.codex_binding_page("", 64).await.unwrap();
    assert_eq!(bindings.len(), 2);
    assert_eq!(
        journal
            .codex_binding_page("10000000-0000-4000-8000-000000000005", 64)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        journal
            .codex_archive_page(&fingerprint(), "", 32)
            .await
            .unwrap()
            .len(),
        2
    );
    let archived = IvyJson::object([
        ("threadId", ARCHIVE_THREAD.into()),
        ("operationId", ARCHIVE_OPERATION.into()),
    ]);
    let mut wrong = archived.clone();
    wrong.insert("operationId", TURN.into());
    assert_eq!(
        journal
            .finish_codex_archive(&fingerprint(), wrong)
            .await
            .err()
            .unwrap()
            .code,
        "phone_voice_archive_changed"
    );
    journal
        .finish_codex_archive(&fingerprint(), archived.clone())
        .await
        .unwrap();
    journal
        .finish_codex_archive(&fingerprint(), archived)
        .await
        .unwrap();
    journal
        .remember_codex_task(&fingerprint(), ARCHIVE_THREAD.into())
        .await
        .unwrap();
    assert_eq!(
        journal
            .codex_archive_page(&fingerprint(), "", 32)
            .await
            .unwrap()
            .len(),
        1
    );
    journal
        .remember_voice_reasoning("high".into())
        .await
        .unwrap();
    let mut prepared = cache(&fingerprint());
    prepared.insert("preparationOperationId", TURN.into());
    prepared.insert("preparationTurnId", ARCHIVE_OPERATION.into());
    journal
        .retain_task(PRINCIPAL, prepared.clone())
        .await
        .unwrap();
    journal.close().wait().await.unwrap();
    let readback = persisted_state(&root, None);
    same(readback.get("cache").unwrap(), &prepared);
    assert_eq!(readback.get("reasoning"), Some(&"high".into()));
    assert_eq!(
        readback.get("snapshot").unwrap().get("format"),
        Some(&7.0.into())
    );
    same(
        seed.get("snapshot").unwrap().get("schema").unwrap(),
        readback.get("snapshot").unwrap().get("schema").unwrap(),
    );
    // Original historical deletion evidence is outside this maintenance path.
    let historical = |snapshot: &IvyJson| {
        snapshot
            .get("meta")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|row| {
                row.get("key")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .starts_with("voiceRetired:")
            })
            .unwrap()
            .clone()
    };
    same(
        &historical(seed.get("snapshot").unwrap()),
        &historical(readback.get("snapshot").unwrap()),
    );
    let blank = Root::new();
    PhoneJournal::open(blank.options())
        .await
        .unwrap()
        .close()
        .wait()
        .await
        .unwrap();
    let backwards = persisted_state(&blank, None);
    same(
        seed.get("snapshot").unwrap().get("schema").unwrap(),
        backwards.get("snapshot").unwrap().get("schema").unwrap(),
    );
}

#[tokio::test]
async fn cache_requires_typed_ownership_and_failed_retention_rolls_back() {
    let root = Root::new();
    let journal = PhoneJournal::open(root.options()).await.unwrap();
    let good = cache(&fingerprint());
    journal.retain_task(PRINCIPAL, good.clone()).await.unwrap();
    for (key, value) in [
        ("fingerprint", IvyJson::Array(vec![fingerprint().into()])),
        ("threadId", IvyJson::Null),
        ("creationOperationId", TURN.into()),
        ("preparationTurnId", false.into()),
        ("preparationOperationId", "not-a-task-id".into()),
    ] {
        let mut bad = good.clone();
        bad.insert(key, value);
        assert!(journal.retain_task(PRINCIPAL, bad).await.is_err(), "{key}");
        same(
            &journal
                .task_cache(PRINCIPAL, &fingerprint())
                .await
                .unwrap()
                .unwrap(),
            &good,
        );
    }
    journal
        .remember_voice_reasoning("high".into())
        .await
        .unwrap();
    assert_eq!(
        journal.remembered_voice_reasoning().await.unwrap(),
        Some("high".into())
    );
    assert!(journal.remember_voice_reasoning("".into()).await.is_err());
    assert!(
        journal
            .codex_archive_page("not-a-fingerprint", "", 32)
            .await
            .is_err()
    );
    let invalid_text = IvyJson::object([
        ("fingerprint", fingerprint().into()),
        ("partyKey", digest("fixture-party").into()),
        ("threadId", THREAD.into()),
        ("callId", ARCHIVE_THREAD.into()),
        ("generation", 0.0.into()),
        (
            "items",
            IvyJson::Array(vec![IvyJson::object([
                ("role", "user".into()),
                ("text", JsString(vec![0xd800]).into()),
            ])]),
        ),
    ]);
    assert_eq!(
        journal
            .retain_voice_conversation(invalid_text)
            .await
            .unwrap_err()
            .code,
        "phone_storage_invalid"
    );
    journal.close().wait().await.unwrap();
    let root = Root::new();
    let journal = PhoneJournal::open(root.options()).await.unwrap();
    journal.request(|db| db.db.execute_batch("CREATE TRIGGER fail_cache BEFORE INSERT ON meta WHEN NEW.key GLOB 'codexVoice:*' BEGIN SELECT RAISE(ABORT,'fixture-failure'); END;").map_err(error)).await.unwrap();
    assert!(
        journal
            .retain_task(PRINCIPAL, cache(&fingerprint()))
            .await
            .is_err()
    );
    assert!(
        journal
            .codex_archive_page(&fingerprint(), "", 32)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        journal
            .task_cache(PRINCIPAL, &fingerprint())
            .await
            .unwrap()
            .is_none()
    );
    journal.close().wait().await.unwrap();
}

#[tokio::test]
async fn admitted_work_survives_observer_loss_and_close_drains_without_readmission() {
    let root = Root::new();
    let journal = PhoneJournal::open(root.options()).await.unwrap();
    let (entered, running) = oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let first = journal.clone();
    let blocked = tokio::spawn(async move {
        first
            .request(move |_| {
                let _ = entered.send(());
                gate.recv().unwrap();
                Ok(())
            })
            .await
    });
    timed(running).await.unwrap();
    let next = journal.clone();
    let observer =
        tokio::spawn(async move { next.retain_task(PRINCIPAL, cache(&fingerprint())).await });
    timed(async {
        loop {
            if journal
                .0
                .admission
                .lock()
                .unwrap()
                .sender
                .as_ref()
                .unwrap()
                .capacity()
                < QUEUE_CAPACITY
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    observer.abort();
    let _ = observer.await;
    let close = journal.close();
    assert_eq!(
        journal
            .task_cache(PRINCIPAL, &fingerprint())
            .await
            .err()
            .unwrap()
            .code,
        "internal_error"
    );
    release.send(()).unwrap();
    timed(blocked).await.unwrap().unwrap();
    timed(close.wait()).await.unwrap();
    timed(journal.close().wait()).await.unwrap();
    let readback = persisted_state(&root, None);
    same(readback.get("cache").unwrap(), &cache(&fingerprint()));
}

#[tokio::test]
async fn preparation_persists_original_and_turn_on_real_worker_before_completion() {
    use crate::voice_rpc::{
        VoicePreparation,
        control_test_support::{Plan, fixture, hooks},
    };
    let root = Root::new();
    let journal = Arc::new(PhoneJournal::open(root.options()).await.unwrap());
    let (control, _, mut peers, _) = fixture(
        vec![Plan::default()],
        value(r#"{"cwd":"C:\\work"}"#),
        hooks(),
    );
    timed(control.open().wait()).await.unwrap();
    let fp = control.fingerprint().unwrap();
    journal.retain_task(PRINCIPAL, cache(&fp)).await.unwrap();
    let preparation = VoicePreparation::new(control.clone(), journal.clone());
    let work = preparation.prepare(THREAD.into());
    let mut peer = timed(peers.recv()).await.unwrap();
    let request = peer.request().await;
    assert_eq!(request.get("method"), Some(&"turn/start".into()));
    let original_cache = journal.task_cache(PRINCIPAL, &fp).await.unwrap().unwrap();
    assert_eq!(
        request.get("id"),
        original_cache.get("preparationOperationId")
    );
    peer.reply(
        &request,
        IvyJson::object([(
            "turn",
            IvyJson::object([("id", TURN.into()), ("status", "completed".into())]),
        )]),
    )
    .await;
    let read = peer.request().await;
    assert_eq!(read.get("method"), Some(&"thread/read".into()));
    let saved = journal.task_cache(PRINCIPAL, &fp).await.unwrap().unwrap();
    assert_eq!(saved.get("preparationTurnId"), Some(&TURN.into()));
    let ready = value(
        r#"{"thread":{"id":"10000000-0000-4000-8000-000000000001","cwd":"C:\\WORK","projectId":null,"ephemeral":false,"turns":[{"id":"10000000-0000-4000-8000-000000000002","status":"completed","items":[{"type":"mcpToolCall","tool":"ivy_instructions","server":"regular","status":"completed","result":{}},{"type":"mcpToolCall","tool":"phone_bridge_status","server":"regular","status":"completed","result":{}},{"type":"agentMessage","phase":"final_answer","text":"READY"}]}]}}"#,
    );
    peer.reply(&read, ready).await;
    timed(work.wait()).await.unwrap();
    control
        .close(Box::pin(async { Ok(()) }))
        .wait()
        .await
        .unwrap();
    journal.close().wait().await.unwrap();
    same(
        persisted_state(&root, Some(&fp)).get("cache").unwrap(),
        &saved,
    );
}

fn observed_digest(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([
            ("ok", true.into()),
            (
                "value",
                if value.is_null() {
                    IvyJson::Null
                } else {
                    digest(canonical(&value, 4 * 1024 * 1024).unwrap()).into()
                },
            ),
        ]),
        Err(error) => IvyJson::object([
            ("ok", false.into()),
            ("code", error.code.into()),
            ("message", error.message.into()),
            ("outcome", error.outcome.to_json()),
        ]),
    }
}

#[tokio::test]
async fn bounded_continuation_requires_the_original_positive_stop_and_survives_reopen() {
    let expected = value(include_str!(
        "../fixtures/phone-journal-continuation-cases.json"
    ));
    assert_eq!(expected.as_array().unwrap().len(), 30);
    for row in expected.as_array().unwrap() {
        let root = Root::new();
        let journal = PhoneJournal::open(root.options()).await.unwrap();
        let payload = row.get("payload").unwrap();
        let mut context = payload.get("context").unwrap().clone();
        if let Some(bytes) = row.get("textBytes").and_then(IvyJson::as_f64) {
            context.insert(
                "items",
                IvyJson::Array(vec![IvyJson::object([
                    ("role", "user".into()),
                    ("text", "a".repeat(bytes as usize).into()),
                ])]),
            );
        }
        let generation = context.get("generation").unwrap().clone();
        let action = payload.get("action").unwrap().as_str().unwrap();
        let placed = payload.get("placement").unwrap().as_str().unwrap();
        if let Some(operation) = payload.get("operation").filter(|value| !value.is_null()) {
            let operation = operation.clone();
            journal
                .request(move |db| {
                    let id = "10000000-0000-4000-8000-000000000006";
                    let call = "10000000-0000-4000-8000-000000000005";
                    let encoded = canonical(&operation, 4 * 1024 * 1024)?;
                    if placed == "archive" {
                        db.archive.retain("command", id, &encoded)?;
                        let mut key = vec![IvyJson::from(call), IvyJson::from("call.stopVoice")];
                        if generation != IvyJson::from(0.0) {
                            key.push(generation);
                        }
                        db.archive.retain(
                            "call-command",
                            &canonical(&IvyJson::Array(key), 4 * 1024 * 1024)?,
                            id,
                        )?;
                    } else {
                        db.db
                            .execute(
                                "INSERT INTO commands VALUES(?,?,?,?,?,?)",
                                params![
                                    id,
                                    "10000000-0000-4000-8000-000000000011",
                                    call,
                                    "call.stopVoice",
                                    operation.get("phase").unwrap().as_str()?,
                                    encoded
                                ],
                            )
                            .map_err(error)?;
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
        let initial = payload
            .get("initial")
            .filter(|value| !value.is_null())
            .cloned();
        let reading = action == "read";
        if initial.is_some() || reading {
            let retained = if reading {
                context.clone()
            } else {
                initial.unwrap()
            };
            journal
                .request(move |db| {
                    db.set_meta(
                        &format!("voiceConversation:{}", fingerprint()),
                        &canonical(&retained, 4 * 1024 * 1024)?,
                    )
                })
                .await
                .unwrap();
        }
        let result = match action.as_str() {
            "read" => journal
                .voice_conversation(&fingerprint())
                .await
                .map(|v| v.unwrap_or(IvyJson::Null)),
            "forget" => journal
                .forget_voice_conversation(&fingerprint())
                .await
                .map(|_| IvyJson::Null),
            "retain" => journal
                .retain_voice_conversation(context.clone())
                .await
                .map(|_| IvyJson::Null),
            _ => unreachable!(),
        };
        let result = observed_digest(result);
        let after = observed_digest(
            journal
                .voice_conversation(&fingerprint())
                .await
                .map(|v| v.unwrap_or(IvyJson::Null)),
        );
        let raw = journal
            .request(|db| db.meta(&format!("voiceConversation:{}", fingerprint())))
            .await
            .unwrap();
        let actual = IvyJson::object([
            ("result", result),
            ("after", after),
            (
                "rawHash",
                raw.map(|v| IvyJson::from(digest(v)))
                    .unwrap_or(IvyJson::Null),
            ),
        ]);
        same(&actual, row.get("expected").unwrap());
        journal.close().wait().await.unwrap();
        if row.get("name") == Some(&"confirmed-stop".into()) {
            let reopened = PhoneJournal::open(root.options()).await.unwrap();
            same(
                &reopened
                    .voice_conversation(&fingerprint())
                    .await
                    .unwrap()
                    .unwrap(),
                &context,
            );
            reopened.close().wait().await.unwrap();
            same(
                &value(
                    &persisted_meta(&root, &format!("voiceConversation:{}", fingerprint()))
                        .unwrap(),
                ),
                &context,
            );
        }
    }
}

#[tokio::test]
async fn retained_voice_evidence_matches_original_identity_generation_and_refusal() {
    let expected = value(include_str!(
        "../fixtures/phone-journal-evidence-cases.json"
    ));
    assert_eq!(expected.as_array().unwrap().len(), 25);
    for row in expected.as_array().unwrap() {
        let root = Root::new();
        let journal = PhoneJournal::open(root.options()).await.unwrap();
        let payload = row.get("payload").unwrap();
        let operation = payload.get("operation").unwrap().clone();
        let key = payload.get("key").unwrap().as_str().unwrap();
        let call = payload.get("callId").unwrap().as_str().unwrap();
        let method = payload.get("method").unwrap().as_str().unwrap();
        let generation = payload
            .get("requestedGeneration")
            .unwrap()
            .as_f64()
            .unwrap() as u32;
        let placed = payload.get("placement").unwrap().as_str().unwrap();
        let (key_copy, call_copy, method_copy) = (key.clone(), call.clone(), method.clone());
        journal
            .request(move |db| {
                let encoded = canonical(&operation, 4 * 1024 * 1024)?;
                match placed.as_str() {
                    "retired" => {
                        db.db
                            .execute(
                                "INSERT INTO receipt_archive_retired VALUES(?,?,?)",
                                params!["command", key_copy, digest(&encoded)],
                            )
                            .map_err(error)?;
                    }
                    "archive" => {
                        db.archive.retain("command", &key_copy, &encoded)?;
                        let mut lookup = vec![IvyJson::from(call_copy), IvyJson::from(method_copy)];
                        if let Some(generation) = operation
                            .get("intent")
                            .and_then(|intent| intent.get("voiceGeneration"))
                            .filter(|generation| **generation != IvyJson::from(0.0))
                        {
                            lookup.push(generation.clone());
                        }
                        db.archive.retain(
                            "call-command",
                            &canonical(&IvyJson::Array(lookup), 4 * 1024 * 1024)?,
                            &key_copy,
                        )?;
                    }
                    "active" => {
                        db.db
                            .execute(
                                "INSERT INTO commands VALUES(?,?,?,?,?,?)",
                                params![
                                    key_copy,
                                    "10000000-0000-4000-8000-000000000011",
                                    call_copy,
                                    method_copy,
                                    operation.get("phase").unwrap().as_str()?,
                                    encoded
                                ],
                            )
                            .map_err(error)?;
                    }
                    "none" => (),
                    _ => unreachable!(),
                }
                Ok(())
            })
            .await
            .unwrap();
        let result = match payload.get("action").unwrap().as_str().unwrap().as_str() {
            "operation" => journal
                .operation(key.into())
                .await
                .map(|v| v.unwrap_or(IvyJson::Null)),
            "command" => journal
                .call_command(call.into(), method.into(), generation)
                .await
                .map(|v| v.unwrap_or(IvyJson::Null)),
            "voice-task" => journal
                .voice_task(call.into(), generation)
                .await
                .map(|v| v.map(IvyJson::String).unwrap_or(IvyJson::Null)),
            "created" => journal
                .created_voice_task(call.into(), generation)
                .await
                .map(|v| v.map(IvyJson::String).unwrap_or(IvyJson::Null)),
            "latest" => journal.latest_voice_generation(call.into()).await.map(|v| {
                v.map(|n| IvyJson::Number(f64::from(n)))
                    .unwrap_or(IvyJson::Null)
            }),
            _ => unreachable!(),
        };
        same(&observed_digest(result), row.get("expected").unwrap());
        journal.close().wait().await.unwrap();
    }
}
