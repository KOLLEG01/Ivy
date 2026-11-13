use ivy_contracts::{IvyJson, json::parse, operation::operation_id};
use ivy_secretary::journal::{Journal, Limits, Pin, Usage};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[path = "journal-memory.rs"]
mod retention_memory;

fn value(s: &str) -> IvyJson {
    parse(s, 1_048_576).unwrap()
}
fn id(n: u64) -> String {
    operation_id(
        "fixture-epoch",
        1_800_000_000_000 + n,
        &format!("nonce-{n}"),
    )
    .unwrap()
}
fn operation(n: u64, action: &str, phase: &str) -> IvyJson {
    IvyJson::object([
        ("operationId", id(n).into()),
        ("action", action.into()),
        ("phase", phase.into()),
        ("checkpointCaptures", IvyJson::Array(vec![])),
    ])
}
fn memory(limits: Limits) -> Journal {
    Journal::open(Path::new(":memory:"), limits).unwrap()
}

fn assert_usage(journal: &Journal) -> Usage {
    // Independent full-scan oracle includes retained revisions and the extra
    // reservation of accepted operations, using UTF-8 bytes rather than chars.
    let expected = journal.db.query_row("SELECT (SELECT COUNT(*) FROM revisions)+(SELECT COUNT(*) FROM technical_revisions)+(SELECT COUNT(*) FROM technical_blobs)+(SELECT COUNT(*) FROM documents WHERE key='secretary/operation' AND completed_at IS NULL), COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))) FROM revisions),0)+COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))) FROM technical_revisions),0)+COALESCE((SELECT SUM(length(bytes)) FROM technical_blobs),0)+COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))+512) FROM documents WHERE key='secretary/operation' AND completed_at IS NULL),0)", [], |row| Ok(Usage { count: row.get(0)?, bytes: row.get(1)? })).unwrap();
    assert_eq!(journal.usage().unwrap(), expected);
    expected
}

fn source_with_cursor(cursor: &str) -> IvyJson {
    let mut source = value(
        r#"{"schemaVersion":1,"definition":{"sourceId":"source","accountId":"account","kind":"email","producerPrincipalIds":["producer"],"allowedSenderIds":null,"ownSenderIds":[]},"definitionHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","cursor":null,"checkpointOperation":null,"updatedAt":null}"#,
    );
    source.insert(
        "definitionHash",
        ivy_contracts::hash::hash_json(source.required("definition").unwrap())
            .unwrap()
            .into(),
    );
    source.insert("cursor", cursor.into());
    ivy_secretary::schema::validate(ivy_secretary::schema::Schema::Source, &source).unwrap();
    source
}
fn pending_checkpoint(n: u64, source: &IvyJson, captures: Vec<IvyJson>) -> IvyJson {
    let mut accepted = value(
        r#"{"schemaVersion":1,"action":"checkpoint","operationId":"original","callerPrincipalId":"producer","requestHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","request":{"action":"checkpoint","operationId":"original","expectedScope":{"secretaryId":"secretary","rootObjectId":"root"},"sourceId":"source","previousCursor":null,"nextCursor":"next","captures":[]},"sourceId":"source","source":null,"createdAt":"2026-01-01T00:00:00.000Z","prepared":null,"existingEffect":null,"ignoredReason":null,"captureIdentity":null,"checkpointCaptures":[],"phase":"accepted","effect":null,"errorCode":null}"#,
    );
    accepted.insert("operationId", id(n).into());
    let request = accepted.get_mut("request").unwrap();
    request.insert("operationId", id(n).into());
    request.insert("previousCursor", source.required("cursor").unwrap().clone());
    let request_hash = ivy_contracts::hash::hash_json(request).unwrap();
    accepted.insert("requestHash", request_hash.into());
    accepted.insert("source", source.clone());
    accepted.insert("checkpointCaptures", IvyJson::Array(captures));
    ivy_secretary::schema::validate(ivy_secretary::schema::Schema::Accepted, &accepted).unwrap();
    accepted
}

#[test]
fn known_journal_edges_keep_checkpoint_receipts_and_ignore_opaque_cursors() {
    let mut journal = memory(Limits::default());
    let finished = journal
        .create_technical("workflow", "finished", &value(r#"{"phase":"succeeded"}"#))
        .unwrap();
    journal
        .complete_technical_workflow(&finished.pin.object_id, 0)
        .unwrap();
    let source = source_with_cursor(&finished.pin.object_id);
    journal
        .create_local("secretary/source", "source", &source, 0)
        .unwrap();
    let capture = journal
        .create_local(
            "secretary/operation",
            "capture",
            &operation(80, "capture", "succeeded"),
            1,
        )
        .unwrap();
    // A confirmed capture is normally eligible after its checkpoint acknowledges it.
    journal
        .db
        .execute(
            "INSERT INTO capture_acknowledgements VALUES(?)",
            [&capture.object_id],
        )
        .unwrap();
    let pending = pending_checkpoint(
        81,
        &source,
        vec![IvyJson::object([
            ("objectId", capture.object_id.clone().into()),
            ("revision", (capture.revision as f64).into()),
        ])],
    );
    let saved = journal
        .create_local("secretary/operation", "checkpoint", &pending, 2)
        .unwrap();
    journal.limits.count = 0;
    journal.cleanup(0, 0, None, None).unwrap();
    assert_eq!(
        journal
            .read("workflow", &finished.pin.object_id, None, true)
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        journal
            .read("secretary/operation", &capture.object_id, None, false)
            .unwrap()
            .pin,
        capture
    );
    assert!(
        ivy_contracts::json::canonical_equal(
            &journal
                .read("secretary/operation", &saved.object_id, None, false)
                .unwrap()
                .value,
            &pending,
            1_048_576
        )
        .unwrap()
    );
    assert!(
        ivy_contracts::json::canonical_equal(
            &journal
                .named("secretary/source", "source", false)
                .unwrap()
                .unwrap()
                .value,
            &source,
            1_048_576
        )
        .unwrap()
    );
    assert_eq!(journal.expired_before().unwrap(), 0);
}

#[test]
fn incoming_cursor_does_not_reserve_an_unrelated_local_workflow() {
    let mut journal = memory(Limits::default());
    let finished = journal
        .create_technical("workflow", "finished", &value(r#"{"phase":"succeeded"}"#))
        .unwrap();
    journal
        .complete_technical_workflow(&finished.pin.object_id, 0)
        .unwrap();
    let source = source_with_cursor(&finished.pin.object_id);
    journal.limits.count = 1;
    let saved = journal
        .create_local("secretary/source", "source", &source, 0)
        .unwrap();
    assert!(
        ivy_contracts::json::canonical_equal(
            &journal
                .read("secretary/source", &saved.object_id, None, false)
                .unwrap()
                .value,
            &source,
            1_048_576
        )
        .unwrap()
    );
    assert_eq!(
        journal
            .read("workflow", &finished.pin.object_id, None, true)
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(assert_usage(&journal).count, 1);
}

#[test]
fn user_content_cannot_merge_unrelated_native_work_windows_or_follow_ups_into_cleanup() {
    for key in [
        "secretary/public-operation",
        "secretary/assignment-native-call",
        "secretary/object-window",
        "secretary/channel-window",
        "secretary/follow-up",
    ] {
        let mut journal = memory(Limits::default());
        let finished = journal
            .create_technical("workflow", "finished", &value(r#"{"phase":"succeeded"}"#))
            .unwrap();
        journal
            .complete_technical_workflow(&finished.pin.object_id, 0)
            .unwrap();
        let text: IvyJson = finished.pin.object_id.clone().into();
        let data = match key {
            "secretary/public-operation" => IvyJson::object([
                ("operationId", id(90).into()),
                ("phase", "prepared".into()),
                ("requestHash", "retained-hash".into()),
                (
                    "write",
                    IvyJson::object([("value", IvyJson::object([("prompt", text)]))]),
                ),
            ]),
            "secretary/assignment-native-call" => IvyJson::object([
                ("operationId", id(90).into()),
                ("method", "turn/start".into()),
                ("params", IvyJson::object([("text", text)])),
            ]),
            "secretary/object-window" | "secretary/channel-window" => IvyJson::object([
                ("assignmentId", "assignment".into()),
                ("assignmentSnapshot", IvyJson::object([("prompt", text)])),
            ]),
            _ => IvyJson::object([
                ("phase", "running".into()),
                (
                    "request",
                    IvyJson::object([("itemId", "item".into()), ("question", text.clone())]),
                ),
                ("answer", text),
            ]),
        };
        let unrelated = journal.create_technical(key, "unrelated", &data).unwrap();
        let original = journal
            .read(key, &unrelated.pin.object_id, None, true)
            .unwrap();
        journal.limits.count = 0;
        journal.cleanup(0, 0, None, None).unwrap();
        assert_eq!(
            journal
                .read("workflow", &finished.pin.object_id, None, true)
                .unwrap_err()
                .code,
            "not_found",
            "{key}"
        );
        assert_eq!(
            journal
                .read(key, &unrelated.pin.object_id, None, true)
                .unwrap(),
            original,
            "{key}"
        );
    }
}

#[test]
fn usage_accounting_follows_history_reservations_and_transaction_rollback() {
    let journal = memory(Limits::default());
    let source = journal
        .create_local(
            "secretary/source",
            "source",
            &value(r#"{"text":"Grüße\ud800"}"#),
            0,
        )
        .unwrap();
    assert_usage(&journal);
    journal
        .amend_local(
            "secretary/source",
            &source,
            &value(r#"{"text":"größer"}"#),
            1,
        )
        .unwrap();
    let accepted = journal
        .create_local(
            "secretary/operation",
            "operation",
            &operation(1, "attach", "accepted"),
            1,
        )
        .unwrap();
    assert_eq!(assert_usage(&journal).count, 3);
    journal
        .amend_local(
            "secretary/operation",
            &accepted,
            &operation(1, "attach", "succeeded"),
            2,
        )
        .unwrap();
    assert_eq!(assert_usage(&journal).count, 3);
    let workflow = journal
        .create_technical("workflow", "workflow", &value(r#"{"text":"旧"}"#))
        .unwrap();
    let imported = journal
        .import_technical_revision("workflow", &workflow.pin, &value(r#"{"text":"neu"}"#))
        .unwrap();
    assert_eq!(assert_usage(&journal).count, 5);
    let amended = journal
        .amend_technical("workflow", &imported.pin, &IvyJson::Null)
        .unwrap();
    assert_eq!(assert_usage(&journal).count, 4);
    let before = assert_usage(&journal);
    assert!(
        journal
            .amend_technical("workflow", &workflow.pin, &IvyJson::Null)
            .is_err()
    );
    assert_eq!(assert_usage(&journal), before);

    let blob = journal.save_blob("blob", "blob", &[0, 255]).unwrap();
    assert_usage(&journal);
    journal.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    journal
        .db
        .execute(
            "UPDATE technical_blobs SET bytes=? WHERE id=?",
            ivy_store::rusqlite::params![vec![0_u8; 300], blob.object_id],
        )
        .unwrap();
    journal
        .db
        .execute(
            "UPDATE technical_revisions SET value_json=? WHERE id=?",
            ivy_store::rusqlite::params!["\"größer\"", amended.pin.object_id],
        )
        .unwrap();
    journal
        .db
        .execute(
            "UPDATE revisions SET value_json=? WHERE id=?",
            ivy_store::rusqlite::params!["null", source.object_id],
        )
        .unwrap();
    assert_usage(&journal);
    journal.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(assert_usage(&journal).bytes, before.bytes + 2);
    journal
        .delete_technical("workflow", &amended.pin.object_id)
        .unwrap();
    journal
        .db
        .execute("DELETE FROM technical_blobs WHERE id=?", [&blob.object_id])
        .unwrap();
    assert_usage(&journal);
    // Cleanup and expiry fencing use the same transactional accounting.
    journal
        .cleanup(0, journal.limits.count, None, None)
        .unwrap();
    assert_eq!(assert_usage(&journal).count, 1);
}

#[test]
fn usage_bootstrap_preserves_old_records_and_other_connections_stay_current() {
    let directory = std::env::temp_dir().join(format!("secretary-usage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("journal.sqlite");
    {
        let journal = Journal::open(&path, Limits::default()).unwrap();
        journal
            .create_local(
                "secretary/operation",
                "pending",
                &operation(2, "attach", "accepted"),
                0,
            )
            .unwrap();
        journal.save_blob("blob", "original", &[0, 255, 4]).unwrap();
        let workflow = journal
            .create_technical("workflow", "original", &value(r#"{"text":"旧"}"#))
            .unwrap();
        journal
            .import_technical_revision("workflow", &workflow.pin, &value(r#"{"text":"neu"}"#))
            .unwrap();
        let before = assert_usage(&journal);
        // Simulate a retained database without accounting metadata.
        let triggers = journal.db.prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'journal_usage_%'").unwrap().query_map([], |row| row.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
        for name in triggers {
            journal
                .db
                .execute_batch(&format!("DROP TRIGGER {name}"))
                .unwrap();
        }
        journal
            .db
            .execute_batch("DROP TABLE journal_usage")
            .unwrap();
        drop(journal);
        let first = Journal::open(&path, Limits::default()).unwrap();
        assert_eq!(assert_usage(&first), before);
        assert_eq!(
            first
                .read("workflow", &workflow.pin.object_id, Some(1), true)
                .unwrap(),
            workflow
        );
        let second = Journal::open(&path, Limits::default()).unwrap();
        let added = second
            .create_technical("workflow", "second", &IvyJson::Null)
            .unwrap();
        assert_eq!(
            assert_usage(&first),
            Usage {
                count: before.count + 1,
                bytes: before.bytes + 4
            }
        );
        first
            .delete_technical("workflow", &added.pin.object_id)
            .unwrap();
        assert_eq!(assert_usage(&second), before);
        assert!(
            second
                .db
                .execute("INSERT INTO journal_usage VALUES(2,0,0)", [])
                .is_err()
        );
    }
    assert_eq!(directory.parent(), Some(std::env::temp_dir().as_path()));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn window_queries_read_only_due_work_for_the_selected_assignment() {
    let journal = memory(Limits::default());
    let key = "secretary/object-window";
    journal.db.execute_batch("BEGIN").unwrap();
    // Unrelated and future bodies must never be decoded by this assignment's
    // flush. An unreadable retained revision makes accidental full reads fail.
    for n in 0..1000 {
        let object_id = format!("foreign-{n:04}");
        journal
            .db
            .execute(
                "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                ivy_store::rusqlite::params![
                    object_id,
                    key,
                    object_id,
                    1,
                    r#"{"assignmentId":"other","pending":{"dueAt":"2026-01-01T00:00:00.000Z"}}"#
                ],
            )
            .unwrap();
        journal
            .db
            .execute(
                "INSERT INTO technical_revisions VALUES(?,?,?)",
                ivy_store::rusqlite::params![object_id, 1, "{"],
            )
            .unwrap();
    }
    journal.db.execute_batch("COMMIT").unwrap();
    let due = journal
        .create_technical(
            key,
            "due",
            &value(r#"{"assignmentId":"selected","pending":{"dueAt":"2026-01-01T00:00:00.000Z"}}"#),
        )
        .unwrap();
    journal
        .create_technical(
            key,
            "future",
            &value(r#"{"assignmentId":"selected","pending":{"dueAt":"2026-01-02T00:00:00.000Z"}}"#),
        )
        .unwrap();
    journal
        .create_technical(
            key,
            "closed",
            &value(r#"{"assignmentId":"selected","pending":null}"#),
        )
        .unwrap();
    let now = 1_767_225_600_000;
    assert_eq!(
        journal.due_windows(key, &"selected".into(), now).unwrap(),
        vec![due.clone()]
    );
    assert!(
        journal
            .due_windows("secretary/channel-window", &"selected".into(), now)
            .unwrap()
            .is_empty()
    );
    assert!(
        journal
            .due_windows(key, &"selected".into(), now - 1)
            .unwrap()
            .is_empty()
    );
    // Keep current revision identity and body linked, including absence errors.
    journal
        .db
        .execute(
            "DELETE FROM technical_revisions WHERE id=?",
            [&due.pin.object_id],
        )
        .unwrap();
    assert_eq!(
        journal
            .due_windows(key, &"selected".into(), now)
            .unwrap_err()
            .code,
        "not_found"
    );
}

#[test]
fn follow_up_exists_preserves_phase_and_lossless_thread_identity() {
    let journal = memory(Limits::default());
    let thread = value(r#""thread-\ud800\u0000""#)
        .as_js_string()
        .unwrap()
        .clone();
    assert!(!journal.follow_up_busy(&thread, "self").unwrap());
    for (name, raw) in [
        (
            "self",
            r#"{"threadId":"thread-\ud800\u0000","phase":"running"}"#,
        ),
        (
            "done",
            r#"{"threadId":"thread-\ud800\u0000","phase":"succeeded"}"#,
        ),
        (
            "failed",
            r#"{"threadId":"thread-\ud800\u0000","phase":"failed"}"#,
        ),
        (
            "other",
            r#"{"threadId":"thread-\ud801\u0000","phase":"running"}"#,
        ),
    ] {
        journal
            .create_technical("secretary/follow-up", name, &value(raw))
            .unwrap();
    }
    assert!(!journal.follow_up_busy(&thread, "self").unwrap());
    for phase in [
        None,
        Some(IvyJson::Null),
        Some("prepared".into()),
        Some("outcome_unknown".into()),
    ] {
        let mut raw = IvyJson::object([("threadId", IvyJson::String(thread.clone()))]);
        if let Some(phase) = phase {
            raw.insert("phase", phase);
        }
        let saved = journal
            .create_technical("secretary/follow-up", "pending", &raw)
            .unwrap();
        assert!(journal.follow_up_busy(&thread, "self").unwrap());
        journal
            .delete_technical("secretary/follow-up", &saved.pin.object_id)
            .unwrap();
    }
}

#[test]
fn non_strict_core_and_version_are_preserved() {
    let journal = memory(Limits::default());
    assert_eq!(
        journal
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        journal
            .db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    assert_eq!(
        journal
            .db
            .query_row(
                "SELECT sum(strict) FROM pragma_table_list WHERE schema='main'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    journal.db.execute_batch("PRAGMA user_version=27").unwrap();
    // Core performs no spool-style format admission/migration.
    journal
        .db
        .execute_batch(include_str!(
            "../../services/secretary/rust/src/journal.sql"
        ))
        .unwrap();
    assert_eq!(
        journal
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        27
    );
}

#[test]
fn technical_cas_compacts_and_blobs_keep_original_bytes() {
    let journal = memory(Limits {
        count: 2,
        bytes: 1024,
    });
    let original = value(r#"{"phase":"running","text":"\ud800"}"#);
    let first = journal
        .create_technical("workflow", "name", &original)
        .unwrap();
    assert_eq!(
        journal
            .create_technical("workflow", "name", &IvyJson::Null)
            .unwrap(),
        first
    );
    let second = journal
        .amend_technical("workflow", &first.pin, &value(r#"{"phase":"succeeded"}"#))
        .unwrap();
    assert_eq!(journal.usage().unwrap().count, 1);
    assert_eq!(
        journal
            .read("workflow", &first.pin.object_id, Some(1), true)
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        journal
            .amend_technical("workflow", &first.pin, &original)
            .unwrap_err()
            .code,
        "revision_conflict"
    );
    assert_eq!(
        journal
            .read("workflow", &first.pin.object_id, None, true)
            .unwrap(),
        second
    );
    let pin = journal
        .save_blob("evidence", "bytes", &[0, 255, 4])
        .unwrap();
    assert_eq!(
        journal
            .save_blob("evidence", "bytes", &[0, 255, 4])
            .unwrap(),
        pin
    );
    assert_eq!(
        journal
            .save_blob("evidence", "bytes", &[0, 255, 5])
            .unwrap_err()
            .code,
        "secretary_identity_conflict"
    );
    assert_eq!(
        journal
            .blob(
                "evidence",
                "",
                Some(&Pin {
                    object_id: pin.object_id,
                    revision: 2
                })
            )
            .unwrap_err()
            .code,
        "secretary_evidence_conflict"
    );
}

#[test]
fn capture_reserve_acknowledgement_and_retired_identity_survive_cleanup() {
    let mut journal = memory(Limits {
        count: 4,
        bytes: 100_000,
    });
    let captured = journal
        .create_local(
            "secretary/operation",
            "capture",
            &operation(1, "capture", "accepted"),
            1,
        )
        .unwrap();
    assert_eq!(journal.usage().unwrap().count, 2);
    assert_eq!(
        journal
            .create_local(
                "secretary/operation",
                "second",
                &operation(2, "capture", "accepted"),
                1
            )
            .unwrap_err()
            .code,
        "capacity_exceeded"
    );
    journal
        .amend_local(
            "secretary/operation",
            &captured,
            &operation(1, "capture", "succeeded"),
            2,
        )
        .unwrap();
    journal.limits.count = 2;
    assert_eq!(
        journal
            .create_local("secretary/source", "source", &IvyJson::Null, 3)
            .unwrap_err()
            .code,
        "capacity_exceeded"
    );
    journal.limits.count = 4;
    let checkpoint = journal
        .create_local(
            "secretary/operation",
            "checkpoint",
            &operation(3, "checkpoint", "accepted"),
            4,
        )
        .unwrap();
    let mut completed = operation(3, "checkpoint", "succeeded");
    completed.insert(
        "checkpointCaptures",
        IvyJson::Array(vec![IvyJson::object([
            ("objectId", captured.object_id.clone().into()),
            ("revision", IvyJson::Number(2.0)),
        ])]),
    );
    journal
        .amend_local("secretary/operation", &checkpoint, &completed, 5)
        .unwrap();
    journal.limits.count = 3;
    journal
        .create_local("secretary/source", "source", &IvyJson::Null, 6)
        .unwrap();
    assert!(
        journal
            .named("secretary/operation", "capture", false)
            .unwrap()
            .is_none()
    );
    assert_eq!(journal.expired_before().unwrap(), 1_800_000_000_001);
    journal.limits.count = 10;
    assert_eq!(
        journal
            .create_local(
                "secretary/operation",
                "replay",
                &operation(1, "capture", "succeeded"),
                7
            )
            .unwrap_err()
            .code,
        "operation_expired"
    );
}

#[test]
fn unknown_and_reachable_technical_work_is_not_evicted_for_space() {
    let journal = memory(Limits {
        count: 2,
        bytes: 4096,
    });
    let child = journal
        .create_technical("child", "child", &value(r#"{"phase":"succeeded"}"#))
        .unwrap();
    let parent = journal
        .create_technical(
            "parent",
            "parent",
            &IvyJson::object([
                ("phase", "outcome_unknown".into()),
                ("child", child.pin.object_id.clone().into()),
            ]),
        )
        .unwrap();
    journal
        .complete_technical_workflow(&child.pin.object_id, 0)
        .unwrap();
    assert_eq!(
        journal
            .save_blob("evidence", "more", &[1])
            .unwrap_err()
            .code,
        "capacity_exceeded"
    );
    journal
        .amend_technical(
            "parent",
            &parent.pin,
            &IvyJson::object([
                ("phase", "succeeded".into()),
                ("child", child.pin.object_id.clone().into()),
            ]),
        )
        .unwrap();
    journal.save_blob("evidence", "more", &[1]).unwrap();
    assert!(journal.named("child", "child", true).unwrap().is_none());
    assert!(journal.named("parent", "parent", true).unwrap().is_none());
    assert_eq!(journal.usage().unwrap(), Usage { count: 1, bytes: 1 });
}

#[test]
fn current_checkpoint_completion_and_acknowledgements_commit_atomically() {
    let journal = memory(Limits::default());
    let accepted = journal
        .create_local(
            "secretary/operation",
            "checkpoint",
            &operation(8, "checkpoint", "accepted"),
            1,
        )
        .unwrap();
    let mut completed = operation(8, "checkpoint", "succeeded");
    completed.insert(
        "checkpointCaptures",
        IvyJson::Array(
            ["first", "refuse"]
                .into_iter()
                .map(|id| {
                    IvyJson::object([
                        ("objectId", IvyJson::from(id)),
                        ("revision", IvyJson::Number(2.0)),
                    ])
                })
                .collect(),
        ),
    );
    journal.db.execute_batch("CREATE TRIGGER refuse_checkpoint_ack BEFORE INSERT ON capture_acknowledgements WHEN NEW.capture_id='refuse' BEGIN SELECT RAISE(ABORT,'fixture acknowledgement refusal'); END;").unwrap();
    assert_eq!(
        journal
            .amend_local("secretary/operation", &accepted, &completed, 2)
            .unwrap_err()
            .code,
        "storage_unavailable"
    );
    assert_eq!(
        journal
            .read("secretary/operation", &accepted.object_id, None, false)
            .unwrap()
            .pin,
        accepted
    );
    assert_eq!(
        journal
            .db
            .query_row("SELECT COUNT(*) FROM capture_acknowledgements", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(
        journal
            .db
            .query_row(
                "SELECT COUNT(*) FROM revisions WHERE id=?",
                [&accepted.object_id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    journal
        .db
        .execute_batch("DROP TRIGGER refuse_checkpoint_ack")
        .unwrap();
    let saved = journal
        .amend_local("secretary/operation", &accepted, &completed, 2)
        .unwrap();
    assert_eq!(saved.revision, 2);
    assert_eq!(
        journal
            .db
            .query_row("SELECT COUNT(*) FROM capture_acknowledgements", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        2
    );
    assert_eq!(
        journal
            .amend_local("secretary/operation", &accepted, &completed, 3)
            .unwrap_err()
            .code,
        "revision_conflict"
    );
    assert_eq!(
        journal
            .read("secretary/operation", &saved.object_id, None, false)
            .unwrap()
            .pin,
        saved
    );
}

#[test]
fn storage_failure_rolls_back_prior_cleanup_and_fences() {
    for technical in [false, true] {
        let mut journal = memory(Limits::default());
        let good = operation(10, "checkpoint", "succeeded");
        let bad = operation(11, "checkpoint", "succeeded");
        let (first, second) = if technical {
            let first = journal
                .create_technical("workflow", "first", &good)
                .unwrap();
            let second = journal
                .create_technical("workflow", "second", &bad)
                .unwrap();
            journal
                .complete_technical_workflow(&first.pin.object_id, 1)
                .unwrap();
            journal
                .complete_technical_workflow(&second.pin.object_id, 2)
                .unwrap();
            (first.pin, second.pin)
        } else {
            (
                journal
                    .create_local("secretary/operation", "first", &good, 1)
                    .unwrap(),
                journal
                    .create_local("secretary/operation", "second", &bad, 2)
                    .unwrap(),
            )
        };
        let key = if technical {
            "workflow"
        } else {
            "secretary/operation"
        };
        let persisted_good = journal
            .read(key, &first.object_id, Some(first.revision), technical)
            .unwrap()
            .value;
        let persisted_bad = journal
            .read(key, &second.object_id, Some(second.revision), technical)
            .unwrap()
            .value;
        let before = journal.usage().unwrap();
        let table = if technical {
            "technical_documents"
        } else {
            "documents"
        };
        journal.db.execute_batch(&format!("CREATE TRIGGER refuse_second_cleanup BEFORE DELETE ON {table} WHEN OLD.name='second' BEGIN SELECT RAISE(ABORT,'fixture cleanup refusal'); END;")).unwrap();
        journal.limits.count = 0;
        let actual = journal.cleanup(0, 0, None, None).unwrap_err();
        assert_eq!(actual.code, "storage_unavailable");
        assert_eq!(
            journal
                .read(key, &first.object_id, Some(first.revision), technical)
                .unwrap()
                .value,
            persisted_good
        );
        assert_eq!(
            journal
                .read(key, &second.object_id, Some(second.revision), technical)
                .unwrap()
                .value,
            persisted_bad
        );
        assert_eq!(journal.usage().unwrap(), before);
        assert_eq!(journal.expired_before().unwrap(), 0);
        assert_eq!(
            journal
                .db
                .query_row("SELECT COUNT(*) FROM retired_operations", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        if technical {
            assert_eq!(
                journal
                    .db
                    .query_row("SELECT COUNT(*) FROM technical_cleanup", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                2
            );
        }
    }
}

#[test]
fn protected_cleanup_keeps_malformed_receipts_and_pending_contexts() {
    for technical in [false, true] {
        for incoming in [false, true] {
            let mut journal = memory(Limits::default());
            let mut bad = operation(12, "checkpoint", "succeeded");
            bad.insert("errorCode", value(r#"{"toString":null}"#));
            let key = if technical {
                "workflow"
            } else {
                "secretary/operation"
            };
            let pin = if technical {
                let saved = journal.create_technical(key, "protected", &bad).unwrap();
                journal
                    .complete_technical_workflow(&saved.pin.object_id, 1)
                    .unwrap();
                saved.pin
            } else {
                journal.create_local(key, "protected", &bad, 1).unwrap()
            };
            let persisted = journal
                .read(key, &pin.object_id, None, technical)
                .unwrap()
                .value;
            let reference = IvyJson::String(pin.object_id.clone().into());
            let before = journal.usage().unwrap();
            journal.limits.count = 0;
            journal
                .cleanup(
                    0,
                    0,
                    if incoming { None } else { Some(&pin.object_id) },
                    if incoming { Some(&reference) } else { None },
                )
                .unwrap();
            assert_eq!(
                journal
                    .read(key, &pin.object_id, None, technical)
                    .unwrap()
                    .value,
                persisted
            );
            assert_eq!(journal.usage().unwrap(), before);
            assert_eq!(journal.expired_before().unwrap(), 0);
        }
    }
    let mut journal = memory(Limits::default());
    let context = value(r#"{"threadId":null,"errorCode":{"toString":null}}"#);
    let saved = journal
        .create_technical("secretary/follow-up-context", "pending", &context)
        .unwrap();
    let persisted = journal
        .read(
            "secretary/follow-up-context",
            &saved.pin.object_id,
            None,
            true,
        )
        .unwrap()
        .value;
    journal
        .complete_technical_workflow(&saved.pin.object_id, 1)
        .unwrap();
    journal.limits.count = 0;
    journal.cleanup(0, 0, None, None).unwrap();
    assert_eq!(
        journal
            .read(
                "secretary/follow-up-context",
                &saved.pin.object_id,
                None,
                true
            )
            .unwrap()
            .value,
        persisted
    );
}

#[test]
fn malformed_receipt_codes_keep_original_records() {
    for technical in [false, true] {
        let mut journal = memory(Limits::default());
        let mut original = operation(13, "checkpoint", "succeeded");
        original.insert("errorCode", value(r#"{"toString":null}"#));
        assert!(ivy_store::receipts::has_unresolved_outcome(&original).unwrap());
        let key = if technical {
            "workflow"
        } else {
            "secretary/operation"
        };
        let pin = if technical {
            let saved = journal
                .create_technical(key, "unresolved", &original)
                .unwrap();
            journal
                .complete_technical_workflow(&saved.pin.object_id, 0)
                .unwrap();
            saved.pin
        } else {
            journal
                .create_local(key, "unresolved", &original, 0)
                .unwrap()
        };
        let persisted = journal
            .read(key, &pin.object_id, None, technical)
            .unwrap()
            .value;
        let before = journal.usage().unwrap();
        journal.limits.count = 0;
        journal.cleanup(0, 0, None, None).unwrap();
        assert_eq!(journal.usage().unwrap(), before);
        assert_eq!(
            journal
                .read(key, &pin.object_id, None, technical)
                .unwrap()
                .value,
            persisted
        );
        assert_eq!(journal.expired_before().unwrap(), 0);
    }
}

#[test]
fn later_technical_roots_ignore_domain_references_from_evicted_operations() {
    let mut journal = memory(Limits::default());
    let later = journal
        .create_technical("workflow", "later", &value(r#"{"phase":"succeeded"}"#))
        .unwrap();
    let first = journal
        .create_technical("workflow", "first", &value(r#"{"phase":"succeeded"}"#))
        .unwrap();
    let mut completed = operation(20, "checkpoint", "succeeded");
    completed.insert("target", later.pin.object_id.clone().into());
    journal
        .create_local("secretary/operation", "between", &completed, 1)
        .unwrap();
    journal
        .complete_technical_workflow(&first.pin.object_id, 0)
        .unwrap();
    journal
        .complete_technical_workflow(&later.pin.object_id, 2)
        .unwrap();
    journal.limits.count = 0;
    journal.cleanup(0, 0, None, None).unwrap();
    assert_eq!(journal.usage().unwrap().count, 0);
    assert_eq!(journal.expired_before().unwrap(), 1_800_000_000_020);
}

#[test]
fn follow_up_identity_projection_preserves_missing_null_and_utf16() {
    for (context, follow_up, retained) in [
        (
            r#"{"itemId":"\ud800","threadId":"thread"}"#,
            r#"{"phase":"running","request":{"itemId":"\ud800"}}"#,
            true,
        ),
        (
            r#"{"itemId":"\ud800","threadId":"thread"}"#,
            r#"{"phase":"running","request":{"itemId":"\ud801"}}"#,
            false,
        ),
        (
            r#"{"itemId":null,"threadId":"thread"}"#,
            r#"{"phase":"running","request":{}}"#,
            false,
        ),
        (
            r#"{"threadId":"thread"}"#,
            r#"{"phase":"running","request":{}}"#,
            true,
        ),
        (
            r#"{"itemId":null,"threadId":"thread"}"#,
            r#"{"phase":"running","request":{"itemId":null}}"#,
            true,
        ),
    ] {
        let mut journal = memory(Limits::default());
        let saved = journal
            .create_technical("secretary/follow-up-context", "context", &value(context))
            .unwrap();
        journal
            .create_technical("secretary/follow-up", "active", &value(follow_up))
            .unwrap();
        journal
            .complete_technical_workflow(&saved.pin.object_id, 0)
            .unwrap();
        journal.limits.count = 0;
        journal.cleanup(0, 0, None, None).unwrap();
        assert_eq!(
            journal
                .named("secretary/follow-up-context", "context", true)
                .unwrap()
                .is_some(),
            retained,
            "{context} / {follow_up}"
        );
        assert!(
            journal
                .named("secretary/follow-up", "active", true)
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn cleanup_removes_orphan_revisions_without_a_head_record() {
    let mut journal = memory(Limits::default());
    let saved = journal
        .create_technical("workflow", "orphan", &IvyJson::Null)
        .unwrap();
    journal
        .complete_technical_workflow(&saved.pin.object_id, 0)
        .unwrap();
    journal
        .db
        .execute(
            "DELETE FROM technical_documents WHERE id=?",
            [&saved.pin.object_id],
        )
        .unwrap();
    journal.limits.count = 0;
    journal.cleanup(0, 0, None, None).unwrap();
    assert_eq!(journal.usage().unwrap().count, 0);
    assert_eq!(
        journal
            .db
            .query_row("SELECT COUNT(*) FROM technical_cleanup", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn technical_heads_retain_exact_originals_and_compact_derived_duplicates_on_reopen() {
    use ivy_contracts::{JsString, hash::hash_json, json::canonical};
    let directory =
        Scratch(std::env::temp_dir().join(format!("secretary-heads-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("journal.sqlite");
    let mut original = value(
        r#"{"assignmentId":"a","threadId":"thread","phase":"running","request":{"itemId":"item","operationId":"op","prompt":"keep"},"pending":{"dueAt":"2030-01-01T00:00:00.000Z"},"contextRelease":{"state":"observing","original":"keep"}}"#,
    );
    original.insert(
        "answer",
        IvyJson::String(JsString(vec![0xd800, 0x78, 0xdc00])),
    );
    original.insert("body", "x".repeat(128 * 1024).into());
    let journal = Journal::open(&path, Limits::default()).unwrap();
    let first = journal
        .create_technical("secretary/follow-up", "owned", &original)
        .unwrap();
    let head: String = journal
        .db
        .query_row(
            "SELECT value_json FROM technical_documents WHERE id=?",
            [&first.pin.object_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(head.len() < 512);
    let head = value(&head);
    assert_eq!(head.get("threadId"), original.get("threadId"));
    assert_eq!(
        head.get("request").unwrap().get("itemId"),
        Some(&"item".into())
    );
    assert!(head.get("request").unwrap().get("prompt").is_none());
    assert!(head.get("answer").is_none());
    assert_eq!(
        journal.follow_up_busy(&"thread".into(), "other").unwrap(),
        true
    );
    assert_eq!(
        hash_json(
            &journal
                .read("secretary/follow-up", &first.pin.object_id, Some(1), true)
                .unwrap()
                .value
        )
        .unwrap(),
        hash_json(&original).unwrap()
    );
    let mut next = original.clone();
    next.insert("phase", "succeeded".into());
    let second = journal
        .import_technical_revision("secretary/follow-up", &first.pin, &next)
        .unwrap();
    assert_eq!(second.pin.revision, 2);
    assert_eq!(
        journal
            .amend_technical("secretary/follow-up", &first.pin, &next)
            .unwrap_err()
            .code,
        "revision_conflict"
    );
    let pending = operation(150, "attach", "accepted");
    journal
        .create_local("secretary/operation", "reserved", &pending, 0)
        .unwrap();
    let usage = assert_usage(&journal);
    // Emulate retained duplicated heads, without changing a single original byte.
    journal.db.execute("UPDATE technical_documents SET value_json=(SELECT r.value_json FROM technical_revisions r WHERE r.id=technical_documents.id AND r.revision=technical_documents.current_revision)", []).unwrap();
    drop(journal);
    let journal = Journal::open(&path, Limits::default()).unwrap();
    assert_eq!(assert_usage(&journal), usage);
    assert_eq!(
        hash_json(
            &journal
                .read("secretary/follow-up", &first.pin.object_id, Some(1), true)
                .unwrap()
                .value
        )
        .unwrap(),
        hash_json(&original).unwrap()
    );
    assert_eq!(
        hash_json(
            &journal
                .read("secretary/follow-up", &first.pin.object_id, None, true)
                .unwrap()
                .value
        )
        .unwrap(),
        hash_json(&next).unwrap()
    );
    let raw: String = journal
        .db
        .query_row(
            "SELECT value_json FROM technical_revisions WHERE id=? AND revision=1",
            [&first.pin.object_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        raw == canonical(&original, 1_048_576).unwrap(),
        "Original revision bytes changed"
    );
    let bytes: i64 = journal
        .db
        .query_row(
            "SELECT length(CAST(value_json AS BLOB)) FROM technical_documents WHERE id=?",
            [&first.pin.object_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(bytes < 512);
    assert!(!journal.follow_up_busy(&"thread".into(), "other").unwrap());
    drop(journal);
    assert_eq!(
        Journal::open(&path, Limits::default())
            .unwrap()
            .usage()
            .unwrap(),
        usage
    );
}

#[test]
fn technical_head_compaction_rolls_back_all_heads_when_a_write_fails() {
    let directory = Scratch(
        std::env::temp_dir().join(format!("secretary-head-rollback-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("journal.sqlite");
    let journal = Journal::open(&path, Limits::default()).unwrap();
    let original = value(r#"{"phase":"succeeded","body":"original"}"#);
    for name in ["first", "second"] {
        journal
            .create_technical("workflow", name, &original)
            .unwrap();
    }
    journal.db.execute("UPDATE technical_documents SET value_json=(SELECT r.value_json FROM technical_revisions r WHERE r.id=technical_documents.id AND r.revision=technical_documents.current_revision)", []).unwrap();
    let usage = journal.usage().unwrap();
    journal.db.execute_batch("CREATE TRIGGER refuse_head_compaction BEFORE UPDATE OF value_json ON technical_documents WHEN OLD.id=(SELECT MAX(id) FROM technical_documents) BEGIN SELECT RAISE(ABORT,'fixture head refusal'); END;").unwrap();
    drop(journal);
    assert!(Journal::open(&path, Limits::default()).is_err());
    let db = ivy_store::rusqlite::Connection::open(&path).unwrap();
    let matching: i64 = db.query_row("SELECT COUNT(*) FROM technical_documents d JOIN technical_revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.value_json=r.value_json", [], |r|r.get(0)).unwrap();
    assert_eq!(matching, 2);
    db.execute_batch("DROP TRIGGER refuse_head_compaction")
        .unwrap();
    drop(db);
    assert_eq!(
        Journal::open(&path, Limits::default())
            .unwrap()
            .usage()
            .unwrap(),
        usage
    );
}
