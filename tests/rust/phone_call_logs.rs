use super::*;
use ivy_contracts::json::canonical;

fn id(n: u32) -> JsString {
    format!("70000000-0000-4000-8000-{n:012}").into()
}
fn status(state: u32, packets: u32, bytes: usize) -> IvyJson {
    IvyJson::object([
        (
            "call",
            IvyJson::object([
                ("id", id(1).into()),
                ("state", (state as f64).into()),
                ("text", "x".repeat(bytes).into()),
            ]),
        ),
        (
            "media",
            IvyJson::object([
                ("closed", false.into()),
                ("failed", false.into()),
                ("audio", IvyJson::object([("state", "connected".into())])),
                ("packets", (packets as f64).into()),
            ]),
        ),
    ])
}
#[test]
fn diagnostic_sampling_retention_and_byte_bounds_survive_reopen() {
    for scenario in ["sampling", "bounds", "per-call", "page-bytes", "global"] {
        let root = std::env::temp_dir().join(format!("ivy-phone-log-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let logs = PhoneCallLogs::open(&root).unwrap();
        let call = IvyJson::object([("callId", id(1).into())]);
        let expected = match scenario {
            "sampling" => {
                logs.record(
                    &call,
                    &IvyJson::object([("call", IvyJson::object([("id", id(2).into())]))]),
                )
                .unwrap();
                assert!(logs.read(&id(1), 64).unwrap().is_empty());
                logs.record(&call, &status(0, 0, 0)).unwrap();
                logs.record(&call, &status(0, 0, 0)).unwrap();
                logs.record(&call, &status(0, 1, 0)).unwrap();
                assert_eq!(logs.read(&id(1), 64).unwrap().len(), 1);
                logs.0.lock().unwrap().sampled.front_mut().unwrap().at -=
                    std::time::Duration::from_millis(5001);
                logs.record(&call, &status(0, 1, 0)).unwrap();
                logs.record(&call, &status(1, 2, 0)).unwrap();
                3
            }
            "bounds" => {
                for limit in [0, 65] {
                    assert_eq!(
                        logs.read(&id(1), limit).unwrap_err().code,
                        "invalid_arguments"
                    );
                }
                assert_eq!(
                    logs.record(&call, &status(0, 0, 32768)).unwrap_err().code,
                    "content_too_large"
                );
                0
            }
            "per-call" => {
                for state in 0..70 {
                    logs.record(&call, &status(state, 0, 0)).unwrap();
                }
                let page = logs.read(&id(1), 64).unwrap();
                assert_eq!(
                    page.first().unwrap().get("call").unwrap().get("state"),
                    Some(&69.0.into())
                );
                assert_eq!(
                    page.last().unwrap().get("call").unwrap().get("state"),
                    Some(&6.0.into())
                );
                64
            }
            "page-bytes" => {
                for state in 0..7 {
                    logs.record(&call, &status(state, 0, 28000)).unwrap();
                }
                4
            }
            "global" => {
                let store = logs.0.lock().unwrap();
                let db = store.db.as_ref().unwrap();
                db.execute_batch("BEGIN IMMEDIATE").unwrap();
                for _ in 0..4096 {
                    db.execute(
                        "INSERT INTO snapshots(call_id,at,value) VALUES (?,?,?)",
                        params![
                            id(2).to_utf8().unwrap(),
                            "2026-01-01T00:00:00.000Z",
                            "{\"call\":null,\"media\":null}"
                        ],
                    )
                    .unwrap();
                }
                db.execute_batch("COMMIT").unwrap();
                drop(store);
                logs.record(&call, &status(0, 0, 0)).unwrap();
                let store = logs.0.lock().unwrap();
                assert_eq!(
                    store
                        .db
                        .as_ref()
                        .unwrap()
                        .query_row("SELECT count(*) FROM snapshots", [], |r| r.get::<_, i64>(0))
                        .unwrap(),
                    4096
                );
                assert_eq!(store.sampled.len(), 1);
                1
            }
            _ => unreachable!(),
        };
        let before = logs.read(&id(1), 64).unwrap();
        assert_eq!(before.len(), expected, "{scenario}");
        for entry in &before {
            assert!(
                chrono::DateTime::parse_from_rfc3339(&entry.get("at").unwrap().as_str().unwrap())
                    .is_ok()
            );
        }
        logs.close().unwrap();
        let reopened = PhoneCallLogs::open(&root).unwrap();
        assert_eq!(reopened.read(&id(1), 64).unwrap(), before);
        reopened.close().unwrap();
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn rejected_diagnostic_insert_rolls_back_without_sampling_or_evicting() {
    let root = std::env::temp_dir().join(format!("ivy-phone-log-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let logs = PhoneCallLogs::open(&root).unwrap();
    let call = IvyJson::object([("callId", id(1).into())]);
    let status = IvyJson::object([
        (
            "call",
            IvyJson::object([("id", id(1).into()), ("state", 1.0.into())]),
        ),
        ("media", IvyJson::Null),
    ]);
    logs.record(&call, &status).unwrap();
    let before = canonical(&IvyJson::Array(logs.read(&id(1), 64).unwrap()), 65536).unwrap();
    logs.0.lock().unwrap().db.as_ref().unwrap().execute_batch("CREATE TRIGGER refuse_diagnostic BEFORE INSERT ON snapshots BEGIN SELECT RAISE(FAIL,'fixture diagnostic failure'); END;").unwrap();
    let mut changed = status.clone();
    changed.insert(
        "call",
        IvyJson::object([("id", id(1).into()), ("state", 2.0.into())]),
    );
    assert!(logs.record(&call, &changed).is_err());
    assert_eq!(
        canonical(&IvyJson::Array(logs.read(&id(1), 64).unwrap()), 65536).unwrap(),
        before
    );
    let store = logs.0.lock().unwrap();
    assert!(store.db.as_ref().unwrap().is_autocommit());
    assert!(store.sampled.front().unwrap().signature.contains("1"));
    drop(store);
    logs.close().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
