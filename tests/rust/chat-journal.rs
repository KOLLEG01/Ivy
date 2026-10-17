use ivy_chat_bridge::whatsapp::{
    journal::{HistoryLimits, OutputState},
    journal_io::JournalIo,
};
use ivy_contracts::{
    IvyJson as Json, IvyResult,
    json::{parse, stringify},
};
use rusqlite::types::Value;
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
fn apply(j: &mut JournalIo, a: &Json) -> IvyResult<Json> {
    match field(a, "op").as_str() {
        "accept" => j.accept(
            &field(a, "id"),
            "sender",
            "fixture@invalid",
            &field(a, "message"),
            number(a, "at"),
        )?,
        "set" => j.set(&field(a, "key"), a.required("value")?)?,
        "save" => j.save_request(&field(a, "id"), a.required("value")?)?,
        "rebind" => j.rebind_request(
            &field(a, "id"),
            a.required("previous")?,
            a.required("next")?,
        )?,
        "finish" => j.finish(&field(a, "id"))?,
        "enqueue" => {
            return Ok(Json::Array(
                j.enqueue(
                    a.required("key")?.as_js_string().unwrap(),
                    "fixture@invalid",
                    a.required("text")?.as_js_string().unwrap(),
                    a.get("automatic").and_then(Json::as_bool).unwrap_or(false),
                    1000,
                )?
                .into_iter()
                .map(Json::from)
                .collect(),
            ));
        }
        "mark" => {
            let state = match field(a, "state").as_str() {
                "sent" => OutputState::Sent,
                "unknown" => OutputState::Unknown,
                _ => OutputState::Sending,
            };
            for row in j.outgoing()? {
                if a.get("text").is_none() || field(a, "text") == row.text {
                    j.mark(&row.id, state)?;
                }
            }
        }
        "protect" => {
            let ids = Json::Array(
                j.outgoing()?
                    .into_iter()
                    .map(|r| Json::from(r.id))
                    .collect(),
            );
            let value = if a.get("cursor").and_then(Json::as_bool) == Some(true) {
                Json::object([("ids", ids), ("at", Json::Number(0.0))])
            } else {
                ids
            };
            j.set(&field(a, "key"), &value)?;
        }
        "prune" => j.prune(HistoryLimits {
            count: number(a, "count"),
            bytes: number(a, "bytes"),
        })?,
        "transaction" => j.try_transaction(|j| -> IvyResult<()> {
            for a in a.required("actions")?.as_array().unwrap() {
                apply(j, a)?;
            }
            Ok(())
        })?,
        "snapshot" => return Ok(snapshot(j)),
        _ => panic!("unknown test action"),
    }
    Ok(Json::Null)
}
fn differential(raw: &str) {
    let actions = parse(raw, usize::MAX).unwrap();
    let mut journal = JournalIo::open(":memory:").unwrap();
    let results = Json::Array(
        actions
            .as_array()
            .unwrap()
            .iter()
            .map(|a| match apply(&mut journal, a) {
                Ok(value) => Json::object([("value", value)]),
                Err(error) => Json::object([("error", error.code.into())]),
            })
            .collect(),
    );
    journal.close().unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child = Command::new("node")
        .current_dir(root)
        .arg("tests/rust/fixtures/chat-journal-oracle.mjs")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(raw.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    assert_eq!(
        results,
        expected,
        "Journal differs from original TypeScript; actual {}",
        stringify(&results, usize::MAX).unwrap()
    );
}

#[test]
fn original_request_identity_transactions_and_retired_inputs_match_typescript() {
    differential(
        r#"[
      {"op":"accept","id":"input","message":"raw","at":1},
      {"op":"accept","id":"input","message":"changed","at":1},
      {"op":"save","id":"input","value":{"z":1,"a":2}},
      {"op":"save","id":"input","value":{"ignored":true}},
      {"op":"rebind","id":"input","previous":{"a":2,"z":1},"next":{}},
      {"op":"rebind","id":"input","previous":{"z":1,"a":2},"next":{"binding":2}},
      {"op":"transaction","actions":[{"op":"set","key":"rollback","value":true},{"op":"accept","id":"input","message":"conflict","at":1}]},
      {"op":"set","key":"inbox-content:input","value":"fingerprint"},
      {"op":"finish","id":"input"},
      {"op":"prune","count":0,"bytes":0},
      {"op":"accept","id":"input","message":"raw","at":2},
      {"op":"accept","id":"input","message":"changed","at":2},
      {"op":"snapshot"}
    ]"#,
    );
}
#[test]
fn protected_output_partial_delivery_and_retired_replay_match_typescript() {
    differential(
        r#"[
      {"op":"enqueue","key":"old","text":"old","automatic":true},
      {"op":"mark","state":"sent"},
      {"op":"enqueue","key":"current","text":"current"},
      {"op":"protect","key":"message:binding:turn:item:sender","cursor":true},
      {"op":"mark","state":"sent"},
      {"op":"set","key":"listener:binding","value":{"lastTurn":"turn"}},
      {"op":"enqueue","key":"part1","text":"part1"},
      {"op":"enqueue","key":"part2","text":"part2"},
      {"op":"protect","key":"turn-delivered:binding:partial"},
      {"op":"mark","state":"sent","text":"part1"},
      {"op":"accept","id":"pending","message":"keep","at":0},
      {"op":"prune","count":0,"bytes":0},
      {"op":"enqueue","key":"old","text":"changed"},
      {"op":"enqueue","key":"old","text":"old"},
      {"op":"snapshot"},
      {"op":"mark","state":"sent","text":"part2"},
      {"op":"set","key":"listener:binding","value":{"lastTurn":"new"}},
      {"op":"prune","count":0,"bytes":0},
      {"op":"snapshot"}
    ]"#,
    );
}
#[test]
fn utf16_sql_binding_empty_output_and_pending_conflicts_match_typescript() {
    differential(
        r#"[
      {"op":"enqueue","key":"empty","text":""},
      {"op":"enqueue","key":"\ud800","text":"x\ud800y"},
      {"op":"enqueue","key":"\ud800","text":"x\ud800y"},
      {"op":"enqueue","key":"other","text":"ordinary"},
      {"op":"enqueue","key":"other","text":"changed"},
      {"op":"set","key":"opaque","value":{"2":2,"1":1,"a":"\ud800"}},
      {"op":"mark","state":"unknown"},
      {"op":"prune","count":0,"bytes":0},
      {"op":"snapshot"}
    ]"#,
    );
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
