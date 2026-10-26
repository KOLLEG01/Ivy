use ivy_contracts::{IvyJson, json::parse, operation::operation_id};
use ivy_secretary::journal::{Journal, Limits, Pin, Usage};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

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
        9
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
fn fallible_receipt_inspection_rolls_back_prior_cleanup_and_fences() {
    for technical in [false, true] {
        let mut journal = memory(Limits::default());
        let good = operation(10, "checkpoint", "succeeded");
        let mut bad = operation(11, "checkpoint", "succeeded");
        bad.insert("errorCode", value(r#"{"toString":null}"#));
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
        journal.limits.count = 0;
        let expected = ivy_store::receipts::has_unresolved_outcome(&bad).unwrap_err();
        let actual = journal.cleanup(0, 0, None, None).unwrap_err();
        assert_eq!(
            (actual.code, actual.message, actual.outcome, actual.details),
            (
                expected.code,
                expected.message,
                expected.outcome,
                expected.details
            )
        );
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
fn protected_cleanup_short_circuits_fallible_receipt_inspection() {
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

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
