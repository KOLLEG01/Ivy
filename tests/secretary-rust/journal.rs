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

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn typescript_and_rust_cross_open_the_same_journal() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let dir =
        Scratch(std::env::temp_dir().join(format!("secretary-rust-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&dir.0).unwrap();
    let run = |mode| {
        let output = Command::new("node")
            .current_dir(&root)
            .arg("tests/secretary-rust/journal-oracle.mjs")
            .arg(mode)
            .arg(&dir.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run("create");
    let journal = Journal::open(&dir.0.join("secretary.sqlite"), Limits::default()).unwrap();
    // Canonical writes retain their policy even though ordinary parsing is unbounded.
    let nested = |count| {
        let mut value = value(r#""\ud800""#);
        for _ in 0..count {
            value = IvyJson::Array(vec![value]);
        }
        IvyJson::object([("value", value)])
    };
    let wide = |count| IvyJson::object([("value", IvyJson::Array(vec![IvyJson::Null; count]))]);
    for (name, expected) in [("deep", nested(127)), ("wide", wide(399_998))] {
        let saved = journal.named("fixture", name, true).unwrap().unwrap();
        assert_eq!(saved.value, expected);
        journal.amend_technical("fixture", &saved.pin, &expected).unwrap();
    }
    for (name, rejected) in [("too-deep", nested(128)), ("too-wide", wide(399_999))] {
        assert_eq!(
            journal.create_technical("fixture", name, &rejected).unwrap_err().code,
            "limit_exceeded"
        );
        assert!(journal.named("fixture", name, true).unwrap().is_none());
    }
    let old = journal.named("fixture", "same", true).unwrap().unwrap();
    assert_eq!(
        old.value,
        // Both stores persist canonical JSON, so reopened properties are sorted.
        value(r#"{"nested":{"x":1},"phase":"running","text":"\ud800"}"#)
    );
    journal
        .amend_technical(
            "fixture",
            &old.pin,
            &value(r#"{"phase":"succeeded","text":"\udc00","nested":{"x":2}}"#),
        )
        .unwrap();
    journal
        .save_blob("fixture", "binary", &[0, 1, 254, 255])
        .unwrap();
    drop(journal);
    run("verify");
}
