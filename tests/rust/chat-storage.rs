use ivy_chat_bridge::whatsapp::auth::{AuthMutation, AuthStore};
use ivy_chat_bridge::whatsapp::journal_io::{JournalIo, StorageUnavailable};
use rusqlite::{Connection, types::Value};
#[path = "chat-auth-compat-cases.rs"]
mod auth_compat;
#[path = "chat-store-cases.rs"]
mod chat_store;
#[path = "chat-store-hive-cases.rs"]
mod chat_store_hive;
#[path = "chat-store-integration-cases.rs"]
mod chat_store_integration;
fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ivy-chat-auth-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn node(script: &str, path: &std::path::Path) {
    let output = std::process::Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args(["--input-type=module", "-e", script])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn opaque_auth_fields_are_preserved_and_key_batches_delete_exactly() {
    let mut store = AuthStore::open(":memory:").unwrap();
    let raw = r#"{ "unknown":"\ud800", "key":{"type":"Buffer","data":"AAH/"}, "counter":1 }"#;
    store.set("creds", "default", Some(raw)).unwrap();
    store
        .set_batch(&[
            AuthMutation {
                category: "session",
                id: "fixture",
                value: Some("{\"_sessions\":{}}"),
            },
            AuthMutation {
                category: "future-category",
                id: "opaque",
                value: Some(raw),
            },
        ])
        .unwrap();
    assert_eq!(store.get("creds", "default").unwrap().as_deref(), Some(raw));
    assert_eq!(
        store.get("future-category", "opaque").unwrap().as_deref(),
        Some(raw)
    );
    store
        .set_batch(&[AuthMutation {
            category: "session",
            id: "fixture",
            value: None,
        }])
        .unwrap();
    assert_eq!(store.get("session", "fixture").unwrap(), None);
    store.close().unwrap();
}

#[test]
fn node_rust_node_preserves_the_existing_non_strict_auth_database() {
    let fixture = Fixture::new();
    let path = fixture.0.join("auth.sqlite");
    node(
        r#"
      import { DatabaseSync } from 'node:sqlite'; import { readFileSync } from 'node:fs';
      const source = readFileSync('services/chat-bridge/src/whatsapp/auth.ts', 'utf8');
      const schema = source.match(/this\.db\.exec\(`([^`]+)`\)/)[1];
      const db = new DatabaseSync(process.argv[1]); db.exec(schema);
      const raw = '{ "unknown":"\\ud800", "key":{"type":"Buffer","data":"AAH/"}, "counter":1 }';
      const categories = ['creds','pre-key','session','sender-key','sender-key-memory','app-state-sync-key','app-state-sync-version','lid-mapping','device-list','identity-key','tctoken','future-category'];
      for (const category of categories) db.prepare('INSERT INTO auth VALUES(?,?,?)').run(category,'fixture',raw);
      db.exec("CREATE TRIGGER fail_fixture BEFORE INSERT ON auth WHEN NEW.id='reject' BEGIN SELECT RAISE(ABORT,'fixture_failure'); END;");
      db.close();
    "#,
        &path,
    );
    let mut store = AuthStore::open(&path).unwrap();
    let original = store.get("creds", "fixture").unwrap().unwrap();
    assert!(original.contains(r"\ud800"));
    let result = store.set_batch(&[
        AuthMutation {
            category: "session",
            id: "fixture",
            value: Some("{\"counter\":2}"),
        },
        AuthMutation {
            category: "session",
            id: "reject",
            value: Some("{}"),
        },
    ]);
    assert!(result.is_err());
    assert_eq!(
        store.get("session", "fixture").unwrap().as_ref(),
        Some(&original)
    );
    store
        .set(
            "session",
            "advanced",
            Some(r#"{"counter":2,"opaque":"\udfff"}"#),
        )
        .unwrap();
    store.close().unwrap();
    node(
        r#"
      import { DatabaseSync } from 'node:sqlite'; import assert from 'node:assert/strict';
      const db = new DatabaseSync(process.argv[1]);
      assert.equal(db.prepare("SELECT value FROM auth WHERE category='session' AND id='advanced'").get().value, '{"counter":2,"opaque":"\\udfff"}');
      const rows=db.prepare("SELECT value FROM auth WHERE id='fixture'").all();
      assert.equal(rows.length,12); assert(rows.every(row=>row.value===rows[0].value));
      assert.equal(db.prepare("PRAGMA table_list('auth')").get().strict,0);
      assert.equal(db.prepare('PRAGMA user_version').get().user_version,0);
      assert.equal(db.prepare('PRAGMA integrity_check').get().integrity_check,'ok');
      db.close();
    "#,
        &path,
    );
}

#[test]
fn journal_fence_reopens_original_rows_and_preserves_sequence_watermark() {
    let fixture = Fixture::new();
    let path = fixture.0.join("whatsapp.sqlite");
    let mut journal = JournalIo::open(&path).unwrap();
    journal
        .transaction(|journal| {
            journal.write(
                "INSERT INTO inbox(id,sender,jid,message,received) VALUES(?,?,?,?,?)",
                vec![
                    text("in"),
                    text("sender"),
                    text("fixture@invalid"),
                    text("{\"opaque\":\"\\ud800\"}"),
                    Value::Integer(1),
                ],
            )?;
            journal.write(
                "INSERT INTO retired_inbox VALUES(?,?,0)",
                vec![text("in"), text("fingerprint")],
            )
        })
        .unwrap();
    journal
        .write(
            "INSERT INTO outgoing(id,jid,text) VALUES(?,?,?)",
            vec![text("out"), text("fixture@invalid"), text("message")],
        )
        .unwrap();
    journal.flush().unwrap();
    let disk = Connection::open(&path).unwrap();
    assert_eq!(
        disk.query_row("SELECT COUNT(*) FROM inbox", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    disk.close().unwrap();
    journal.close().unwrap();
    node(
        r#"
      import { DatabaseSync } from 'node:sqlite'; import assert from 'node:assert/strict';
      const db=new DatabaseSync(process.argv[1]);
      assert.equal(db.prepare("PRAGMA table_list('inbox')").get().strict,0);
      assert.equal(db.prepare('SELECT fingerprint FROM retired_inbox').get().fingerprint,'fingerprint');
      db.exec("UPDATE sqlite_sequence SET seq=900 WHERE name='outgoing'"); db.close();
    "#,
        &path,
    );
    let mut journal = JournalIo::open(&path).unwrap();
    journal
        .write(
            "INSERT INTO outgoing(id,jid,text) VALUES(?,?,?)",
            vec![text("next"), text("fixture@invalid"), text("next message")],
        )
        .unwrap();
    assert_eq!(
        journal
            .connection()
            .query_row("SELECT sequence FROM outgoing WHERE id='next'", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        901
    );
    journal.close().unwrap();
    let disk = Connection::open(&path).unwrap();
    assert_eq!(
        disk.query_row("SELECT sequence FROM outgoing WHERE id='next'", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        901
    );
}

#[test]
fn writer_failure_is_sticky_and_fence_cannot_confirm_speculative_memory() {
    let fixture = Fixture::new();
    let path = fixture.0.join("whatsapp.sqlite");
    let mut journal = JournalIo::open(&path).unwrap();
    let disk = Connection::open(&path).unwrap();
    disk.execute_batch("CREATE TRIGGER reject_fixture BEFORE INSERT ON kv BEGIN SELECT RAISE(ABORT,'secret fixture must not escape'); END").unwrap();
    journal
        .write(
            "INSERT INTO kv VALUES(?,?)",
            vec![text("receipt"), text("true")],
        )
        .unwrap();
    assert_eq!(journal.flush(), Err(StorageUnavailable));
    assert_eq!(
        journal.write(
            "INSERT INTO kv VALUES(?,?)",
            vec![text("later"), text("true")]
        ),
        Err(StorageUnavailable)
    );
    assert_eq!(
        disk.query_row("SELECT COUNT(*) FROM kv", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(journal.close(), Err(StorageUnavailable));
}

#[test]
fn failed_memory_transaction_is_never_sent_to_disk() {
    let fixture = Fixture::new();
    let path = fixture.0.join("whatsapp.sqlite");
    let mut journal = JournalIo::open(&path).unwrap();
    let result: Result<(), StorageUnavailable> = journal.transaction(|journal| {
        journal.write(
            "INSERT INTO kv VALUES(?,?)",
            vec![text("partial"), text("true")],
        )?;
        Err(StorageUnavailable)
    });
    assert_eq!(result, Err(StorageUnavailable));
    journal.flush().unwrap();
    assert_eq!(
        journal
            .connection()
            .query_row("SELECT COUNT(*) FROM kv", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    journal.close().unwrap();
    let disk = Connection::open(&path).unwrap();
    assert_eq!(
        disk.query_row("SELECT COUNT(*) FROM kv", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
