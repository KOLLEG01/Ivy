use ivy_chat_bridge::whatsapp::{
    journal::{HistoryLimits, OutputState},
    journal_io::JournalIo,
};
use ivy_contracts::{
    IvyJson as Json, IvyResult,
    json::{parse, stringify},
};
use rusqlite::types::Value;
#[path = "chat-command-cases.rs"]
mod commands;
#[path = "chat-desktop-cases.rs"]
mod desktop;
#[path = "chat-desktop-host-cases.rs"]
mod desktop_host;
#[path = "chat-listener-cases.rs"]
mod listener;
#[path = "chat-session-cases.rs"]
mod session;
#[path = "chat-wa-runtime-cases.rs"]
mod wa_runtime;
#[path = "chat-wa-socket-cases.rs"]
mod wa_socket;
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn field(a: &Json, key: &str) -> String {
    a.required(key).unwrap().as_str().unwrap()
}
fn number(a: &Json, key: &str) -> i64 {
    a.required(key).unwrap().as_f64().unwrap() as i64
}
fn snapshot(j: &JournalIo) -> Json {
    Json::object(
        [
            "kv",
            "inbox",
            "outgoing",
            "retired_inbox",
            "retired_outgoing",
        ]
        .map(|table| {
            let mut q = j
                .connection()
                .prepare(&format!(
                    "SELECT * FROM {table} ORDER BY {}",
                    if table == "kv" { "key" } else { "id" }
                ))
                .unwrap();
            let n = q.column_count();
            let rows = q
                .query_map([], |r| {
                    Ok(Json::Array(
                        (0..n)
                            .map(|i| match r.get::<_, Value>(i).unwrap() {
                                Value::Text(s) => s.into(),
                                Value::Integer(n) => Json::Number(n as f64),
                                Value::Real(n) => Json::Number(n),
                                Value::Null => Json::Null,
                                _ => panic!("unexpected blob"),
                            })
                            .collect(),
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (table, Json::Array(rows))
        }),
    )
}
#[test]
fn monotone_confirmation_and_tombstones_survive_restart() {
    let path = std::env::temp_dir().join(format!(
        "ivy-chat-journal-domain-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let fixture = Cleanup(path);
    let file = fixture.0.join("journal.sqlite");
    let key = "notice".into();
    let mut journal = JournalIo::open(&file).unwrap();
    let ids = journal
        .enqueue(&key, "fixture@invalid", &"original notice".into(), true, 1)
        .unwrap();
    assert_eq!(
        journal.notice_text(&key).unwrap().as_deref(),
        Some("original notice")
    );
    journal.mark(&ids[0], OutputState::Sent).unwrap();
    journal.mark(&ids[0], OutputState::Unknown).unwrap();
    assert!(journal.sent(&ids).unwrap());
    journal.prune(HistoryLimits { count: 0, bytes: 0 }).unwrap();
    journal.flush().unwrap();
    journal.close().unwrap();
    let mut reopened = JournalIo::open(&file).unwrap();
    assert!(reopened.has_output(&key).unwrap());
    assert!(reopened.sent(&ids).unwrap());
    assert_eq!(reopened.notice_text(&key).unwrap(), None);
    assert_eq!(
        reopened
            .enqueue(&key, "fixture@invalid", &"original notice".into(), false, 2)
            .unwrap(),
        ids
    );
    assert!(reopened.outgoing().unwrap().is_empty());
    reopened.close().unwrap();
}
