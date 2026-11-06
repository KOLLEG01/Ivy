use super::*;

fn turn(time: u64, phase: &str, thread: &str, id: Option<&str>, epoch: &str) -> IvyJson {
    let mut value = operation(time, phase);
    value.insert("method", "turn/start".into());
    value.insert("params", IvyJson::object([("threadId", thread.into())]));
    value.insert("epoch", epoch.into());
    if let Some(id) = id {
        value.insert(
            "reply",
            IvyJson::object([(
                "result",
                IvyJson::object([("turn", IvyJson::object([("id", id.into())]))]),
            )]),
        );
    }
    value
}
fn usage(f: &Fixture) -> (i64, i64, i64) {
    Connection::open(f.path())
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM suppressed_completions),
        (SELECT coalesce(sum(byte_length),0) FROM suppressed_completions),
        (SELECT count(*) FROM completion_receipts WHERE released=1)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

#[test]
fn early_completion_survives_restart_and_releases_only_its_exact_original_receipt() {
    let f = Fixture::new();
    let mut state = f.open(1, 64 * 1024 * 1024);
    let pending = turn(100, "dispatched", "thread", None, "native-epoch");
    let original = state
        .write_at("operation", "original", &pending, None, 100)
        .unwrap();
    state
        .suppress_completion("thread", "turn", "native-epoch")
        .unwrap();
    assert_eq!(
        state.read("operation", "original").unwrap(),
        Some(normalized(original.clone()))
    );
    assert_eq!(usage(&f).0, 1);
    state.close().unwrap();
    let mut state = f.open(1, 64 * 1024 * 1024);
    let done = turn(100, "succeeded", "thread", Some("turn"), "native-epoch");
    let saved = state
        .write_at("operation", "original", &done, Some(&original.pin), 101)
        .unwrap();
    assert_eq!(saved.value, done);
    assert_eq!(usage(&f), (0, 0, 1));
    assert_eq!(
        state.completion_caller("thread", "turn").unwrap(),
        Some("caller".into())
    );
    state
        .write_at("operation", "next", &operation(102, "failed"), None, 102)
        .unwrap();
    assert_eq!(state.completion_caller("thread", "turn").unwrap(), None);
}

#[test]
fn foreign_epoch_thread_and_different_turn_never_release_an_original_receipt() {
    for mismatch in ["epoch", "thread", "turn"] {
        let f = Fixture::new();
        let mut state = f.open(1, 64 * 1024 * 1024);
        let original = state
            .write_at(
                "operation",
                "original",
                &turn(100, "dispatched", "thread", None, "native-epoch"),
                None,
                100,
            )
            .unwrap();
        state
            .suppress_completion(
                if mismatch == "thread" {
                    "foreign"
                } else {
                    "thread"
                },
                if mismatch == "turn" {
                    "foreign"
                } else {
                    "turn"
                },
                if mismatch == "epoch" {
                    "foreign"
                } else {
                    "native-epoch"
                },
            )
            .unwrap();
        let done = turn(100, "succeeded", "thread", Some("turn"), "native-epoch");
        let saved = state
            .write_at("operation", "original", &done, Some(&original.pin), 101)
            .unwrap();
        assert_eq!(usage(&f), (0, 0, 0), "{mismatch}");
        assert_code(
            state.write_at("operation", "next", &operation(102, "failed"), None, 102),
            "native_journal_capacity",
        );
        assert_eq!(
            state.read("operation", "original").unwrap(),
            Some(normalized(saved.clone()))
        );
        state
            .suppress_completion("thread", "turn", "new-connection")
            .unwrap();
        assert_eq!(usage(&f), (0, 0, 1));
        assert_eq!(
            state.read("operation", "original").unwrap(),
            Some(normalized(saved))
        );
    }
}

#[test]
fn count_limits_preserve_older_proofs_and_unknown_operations() {
    let f = Fixture::new();
    let mut state = f.open(1000, 16 * 1024 * 1024 * 1024);
    let mut pins = Vec::new();
    for index in 0..129 {
        pins.push(
            state
                .write_at(
                    "operation",
                    &format!("key-{index}"),
                    &turn(
                        100 + index,
                        "dispatched",
                        &format!("thread-{index}"),
                        None,
                        "native-epoch",
                    ),
                    None,
                    (100 + index) as i64,
                )
                .unwrap()
                .pin,
        );
        for id in 0..10 {
            state
                .suppress_completion(
                    &format!("thread-{index}"),
                    &format!("turn-{id}"),
                    "native-epoch",
                )
                .unwrap();
        }
    }
    assert_eq!(usage(&f).0, 1024);
    assert!(usage(&f).1 <= 1024 * 1024);
    for index in [0_usize, 128] {
        let done = turn(
            100 + index as u64,
            "succeeded",
            &format!("thread-{index}"),
            Some("turn-0"),
            "native-epoch",
        );
        state
            .write_at(
                "operation",
                &format!("key-{index}"),
                &done,
                Some(&pins[index]),
                1000,
            )
            .unwrap();
    }
    assert_eq!(usage(&f).0, 1016);
    assert_eq!(usage(&f).2, 1);
    state
        .suppress_completion("thread-128", "turn-0", "native-epoch")
        .unwrap();
    assert_eq!(usage(&f).2, 2);
    let original = state
        .write_at(
            "operation",
            "unknown",
            &turn(2000, "dispatched", "unknown", None, "native-epoch"),
            None,
            2000,
        )
        .unwrap();
    state
        .suppress_completion("unknown", "observed", "native-epoch")
        .unwrap();
    let unknown = turn(2000, "outcome_unknown", "unknown", None, "native-epoch");
    let saved = state
        .write_at("operation", "unknown", &unknown, Some(&original.pin), 2001)
        .unwrap();
    let before = usage(&f);
    state.close().unwrap();
    let state = f.open(1000, 16 * 1024 * 1024 * 1024);
    assert_eq!(
        state.read("operation", "unknown").unwrap(),
        Some(normalized(saved))
    );
    assert_eq!(usage(&f), before);
}

#[test]
fn byte_limit_is_independent_of_row_count_and_unowned_events_create_no_rows() {
    let f = Fixture::new();
    let mut state = f.open(1000, 16 * 1024 * 1024 * 1024);
    for index in 0..100 {
        state
            .suppress_completion(&format!("foreign-{index}"), "turn", "native-epoch")
            .unwrap();
    }
    assert_eq!(usage(&f), (0, 0, 0));
    let id = "t".repeat(8192);
    for index in 0..65 {
        state
            .write_at(
                "operation",
                &format!("key-{index}"),
                &turn(
                    100 + index,
                    "dispatched",
                    &format!("thread-{index}"),
                    None,
                    "native-epoch",
                ),
                None,
                (100 + index) as i64,
            )
            .unwrap();
        state
            .suppress_completion(&format!("thread-{index}"), &id, "native-epoch")
            .unwrap();
    }
    let retained = usage(&f);
    assert!(retained.0 > 0 && retained.0 < 65);
    assert!(retained.1 <= 1024 * 1024);
    assert_eq!(retained.2, 0);
}
