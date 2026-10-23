use super::*;
use ivy_task_board::native_progress::NativeProgress;

#[test]
fn native_silence_tracks_activity_without_turning_stable_polls_into_progress() {
    let subject = NativeProgress::new("fixture-task-board".into(), 60_000.0);
    let run = id(200);
    subject.activity(&run, 0.0);
    assert!(subject.diagnostics(60_000.0).unwrap().is_empty());
    subject.observe(&run, &id(100), true, "initial".into(), 0.0);
    subject.observe(&run, &id(101), true, "initial".into(), 59_999.0);
    assert!(subject.diagnostics(59_999.0).unwrap().is_empty());
    assert_eq!(subject.diagnostics(120_999.9).unwrap(), vec![Json::object([
        ("code", "task_board_native_silent".into()),
        ("resource", Json::object([("objectId", Json::String(id(100)))])),
        ("source", "fixture-task-board".into()),
        ("severity", "warning".into()), ("status", "current".into()),
        ("firstObservedAt", "1970-01-01T00:01:00.000Z".into()),
        ("lastObservedAt", "1970-01-01T00:02:00.999Z".into()),
        ("message", "The native owner still reports running, but no progress has been observed for 2 minutes. Inspect the original native turn; its claim remains active.".into()),
    ])]);
    subject.activity(&run, 120_000.0);
    assert!(subject.diagnostics(179_999.0).unwrap().is_empty());
    assert_eq!(subject.diagnostics(180_000.0).unwrap().len(), 1);
    subject.observe(&run, &id(101), true, "changed".into(), 180_000.0);
    assert!(subject.diagnostics(239_999.0).unwrap().is_empty());
    assert_eq!(
        f(&subject.diagnostics(240_000.0).unwrap()[0], "resource"),
        &Json::object([("objectId", Json::String(id(101)))])
    );
    let error = subject.diagnostics(8.64e15 + 1.0).unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    subject.observe(&run, &id(101), false, "waiting".into(), 240_000.0);
    assert!(subject.diagnostics(300_000.0).unwrap().is_empty());
    subject.observe(&run, &id(101), true, "resumed".into(), 300_000.0);
    subject.forget(&run);
    assert!(subject.diagnostics(360_000.0).unwrap().is_empty());
}

#[test]
fn native_silence_bounds_retention_without_reordering_existing_runs() {
    let subject = NativeProgress::new("fixture-task-board".into(), 1.0);
    for n in 0..1024 {
        subject.observe(&id(n), &id(n + 2000), true, "initial".into(), 0.0);
    }
    // New activity/fingerprints do not turn the oldest entry into a new entry.
    subject.observe(&id(0), &id(2000), true, "changed".into(), 1.0);
    subject.activity(&id(0), 2.0);
    subject.observe(&id(1024), &id(3024), true, "initial".into(), 2.0);
    subject.activity(&id(0), 3.0);
    let diagnostics = subject.diagnostics(4.0).unwrap();
    assert_eq!(diagnostics.len(), 1024);
    assert_eq!(
        f(&diagnostics[0], "resource"),
        &Json::object([("objectId", Json::String(id(2001)))])
    );
    assert_eq!(
        f(diagnostics.last().unwrap(), "resource"),
        &Json::object([("objectId", Json::String(id(3024)))])
    );
    subject.forget(&id(1));
    subject.observe(&id(1025), &id(3025), true, "initial".into(), 4.0);
    let diagnostics = subject.diagnostics(5.0).unwrap();
    assert_eq!(diagnostics.len(), 1024);
    assert_eq!(
        f(&diagnostics[0], "resource"),
        &Json::object([("objectId", Json::String(id(2002)))])
    );
}
