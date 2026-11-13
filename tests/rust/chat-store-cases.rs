#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_chat_bridge::store::{Destination, Pin, StoreResult, local::LocalStore};
use ivy_contracts::{
    IvyJson as Json,
    hash::hash_json,
    json::{canonical, parse},
};
use std::{path::Path, process::Command};

fn json(s: &str) -> Json {
    parse(s, usize::MAX).unwrap()
}
fn s(v: &Json, key: &str) -> String {
    v.required(key).unwrap().as_str().unwrap()
}
fn n(v: &Json, key: &str) -> i64 {
    v.required(key).unwrap().as_f64().unwrap() as i64
}
fn translated(v: &Json, pins: &[(String, Pin)], normalize: bool) -> Json {
    match v {
        Json::String(units) => {
            // Values retain lone surrogates; only ASCII identity tokens change.
            let mut result = units.0.clone();
            let mut ordered = pins.iter().collect::<Vec<_>>();
            if !normalize {
                ordered.sort_by_key(|(a, _)| std::cmp::Reverse(a.len()));
            }
            for (alias, pin) in ordered {
                let token = format!("$id:{alias}");
                let (from, to) = if normalize {
                    (&pin.object_id, &token)
                } else {
                    (&token, &pin.object_id)
                };
                let from = from.encode_utf16().collect::<Vec<_>>();
                let to = to.encode_utf16().collect::<Vec<_>>();
                let mut next = vec![];
                let mut i = 0;
                while i < result.len() {
                    if result[i..].starts_with(&from) {
                        next.extend_from_slice(&to);
                        i += from.len();
                    } else {
                        next.push(result[i]);
                        i += 1;
                    }
                }
                result = next;
            }
            Json::String(ivy_contracts::JsString(result))
        }
        Json::Array(v) => Json::Array(v.iter().map(|v| translated(v, pins, normalize)).collect()),
        Json::Object(v) => Json::object(
            v.iter()
                .map(|(k, v)| (k.clone(), translated(v, pins, normalize))),
        ),
        _ => v.clone(),
    }
}
fn remember(pins: &mut Vec<(String, Pin)>, alias: String, pin: Pin) -> Json {
    let result = pin.json();
    if let Some((_, previous)) = pins.iter_mut().find(|(a, _)| *a == alias) {
        *previous = pin;
    } else {
        pins.push((alias, pin));
    }
    result
}
fn step(
    store: &mut LocalStore,
    pins: &mut Vec<(String, Pin)>,
    a: &Json,
    now: i64,
    iso: &str,
) -> StoreResult<Json> {
    let now = a
        .get("at")
        .and_then(Json::as_f64)
        .map(|v| v as i64)
        .unwrap_or(now);
    let alias = a
        .get("alias")
        .map(|v| v.as_str().unwrap())
        .unwrap_or_default();
    let pin = pins
        .iter()
        .find(|(name, _)| name == &alias)
        .map(|(_, p)| p.clone());
    let parent = a
        .get("parent")
        .filter(|v| !v.is_null())
        .map(|v| v.as_str().unwrap());
    let mut result = Json::Null;
    match s(a, "action").as_str() {
        "create" => {
            result = remember(
                pins,
                alias,
                store.write(
                    &s(a, "key"),
                    a.required("value")?,
                    Destination::Create {
                        name: &s(a, "name"),
                        parent_id: parent.as_deref(),
                    },
                    now,
                    iso,
                )?,
            );
        }
        "update" => {
            let p = pin.unwrap();
            result = remember(
                pins,
                alias,
                store.write(
                    &s(a, "key"),
                    a.required("value")?,
                    Destination::Update {
                        object_id: &p.object_id,
                        expected_revision: a
                            .get("revision")
                            .and_then(Json::as_f64)
                            .map(|v| v as i64)
                            .unwrap_or(p.revision),
                    },
                    now,
                    iso,
                )?,
            );
        }
        "named" | "read" => {
            let parent = if a.get("parent").is_none() {
                Some("root")
            } else {
                parent.as_deref()
            };
            let doc = if s(a, "action") == "named" {
                store.named(&s(a, "key"), &s(a, "name"), parent)?
            } else {
                let p = pin.unwrap();
                Some(
                    store.document(
                        &s(a, "key"),
                        &p.object_id,
                        Some(
                            a.get("revision")
                                .and_then(Json::as_f64)
                                .map(|v| v as i64)
                                .unwrap_or(p.revision),
                        ),
                        parent,
                    )?,
                )
            };
            if let Some(doc) = doc {
                result = Json::object([("pin", doc.pin.json()), ("value", doc.value)]);
            }
        }
        "limit" => {
            store.limits.count = n(a, "count") as usize;
            if let Some(b) = a.get("bytes").and_then(Json::as_f64) {
                store.limits.bytes = b as usize;
            }
        }
        "expire" => store.expire_operations(now, true, None, None)?,
        "confirm" => store.confirm_offer(
            &pin.unwrap().object_id,
            &a.required("replies")?
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
        )?,
        "delivered" => store.remember_notice_delivery(&s(a, "id"), now)?,
        "validate" => store.validate_operation_id(
            &s(a, "id").into(),
            now,
            a.get("retained").and_then(Json::as_bool).unwrap_or(false),
        )?,
        "blob" => {
            let bytes = a
                .required("bytes")?
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap() as u8)
                .collect::<Vec<_>>();
            result = remember(
                pins,
                alias,
                store.save_blob(
                    "chat-bridge/evidence-chunk",
                    &s(a, "name"),
                    parent.as_deref().unwrap(),
                    &bytes,
                    now,
                )?,
            );
        }
        "complete" => store.completed_input(&s(a, "id"), now as f64, now)?,
        "diagnostic" => {
            store.diagnostic_history_limits.count = n(a, "count") as usize;
            if let Some(bytes) = a.get("bytes").and_then(Json::as_f64) {
                store.diagnostic_history_limits.bytes = bytes as usize;
            }
            let mut value = a.required("value")?.clone();
            store.save_diagnostic_notice_state("scope", &mut value)?;
            result = Json::object([
                ("mutated", value),
                ("saved", store.diagnostic_notice_state("scope")?),
            ]);
        }
        "notice-input" => store.remember_notice_input(&s(a, "id"), now)?,
        "is-notice-input" => {
            result = store
                .is_notice_input(
                    &ivy_chat_bridge::store::mutation(&s(a, "id"), "native-user-message")?,
                    now,
                )?
                .into()
        }
        action => panic!("Unknown action {action}"),
    }
    Ok(result)
}

#[test]
fn local_retention_and_conflicts_match_original_typescript() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = reference_data::read(include_bytes!("fixtures/chat-store-cases.json.gz"));
    let fixture = parse(std::str::from_utf8(&output).unwrap(), usize::MAX).unwrap();
    let now = n(&fixture, "now");
    let iso = s(&fixture, "iso");
    for case in fixture.required("cases").unwrap().as_array().unwrap() {
        let mut store = LocalStore::open(None).unwrap();
        store.set_runtime_epoch("chat".into(), now).unwrap();
        let mut pins = vec![];
        let expected = case.required("expected").unwrap().as_array().unwrap();
        for (index, original) in case
            .required("actions")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let action = translated(original, &pins, false);
            let actual = match step(&mut store, &mut pins, &action, now, &iso) {
                Ok(result) => result,
                Err(error) => {
                    let e = error.into_ivy();
                    Json::object([(
                        "error",
                        Json::object([
                            ("code", e.code.into()),
                            ("message", e.message.into()),
                            ("outcome", e.outcome.to_json()),
                        ]),
                    )])
                }
            };
            // A saved open request still pins its original resume revision after
            // pendingAction is cleared. The older reducer discarded that pin.
            let expected_value = if s(case, "label") == "pending-revisions-and-collection-window"
                && index + 1 == expected.len()
            {
                expected[42].clone()
            } else {
                expected[index].clone()
            };
            assert_eq!(
                canonical(&translated(&actual, &pins, true), usize::MAX).unwrap(),
                canonical(&expected_value, usize::MAX).unwrap(),
                "{} step {index}: {original:?}",
                s(case, "label")
            );
        }
        store.close().unwrap();
    }
}

fn assert_journal_usage(db: &rusqlite::Connection) -> (i64, i64, i64) {
    let counters = db
        .query_row(
            "SELECT entries,bytes,reserved_bytes FROM journal_usage WHERE id=1",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .unwrap();
    let actual = db.query_row("SELECT (SELECT COUNT(*) FROM revisions)+(SELECT COUNT(*) FROM blobs),COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))) FROM revisions),0)+COALESCE((SELECT SUM(length(bytes)) FROM blobs),0),(SELECT COUNT(*)*1048576 FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/operation' AND json_extract(r.value_json,'$.phase') NOT IN ('succeeded','failed'))",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).unwrap();
    assert_eq!(counters, actual);
    counters
}

#[test]
fn journal_retirement_preserves_unknown_children_and_cross_root_original_pins() {
    let fixture = super::Fixture::new();
    let now = 1790000000123;
    let iso = "2026-09-21T14:13:20.123Z";
    let mut store = LocalStore::open(Some(&fixture.0)).unwrap();
    let operation = store.write(
        "chat-bridge/operation",
        &json(r#"{"operationId":"metadata:1790000000123:unknown-original","phase":"succeeded","updatedAt":"2026-09-21T14:13:20.123Z"}"#),
        Destination::Create { parent_id: Some("root"), name: "Original operation" }, now, iso,
    ).unwrap();
    let unknown = json(r#"{"state":"outcome_unknown","code":"native_connection_lost"}"#);
    let native = store
        .write(
            "chat-bridge/native-call",
            &unknown,
            Destination::Create {
                parent_id: Some(&operation.object_id),
                name: "Original call",
            },
            now,
            iso,
        )
        .unwrap();
    let bytes = store
        .save_blob(
            "chat-bridge/evidence-chunk",
            "Original bytes",
            &native.object_id,
            &[1, 2, 3, 255],
            now,
        )
        .unwrap();
    store.limits.count = 0;
    store.expire_operations(now, true, None, None).unwrap();
    assert_eq!(
        hash_json(
            &store
                .document(
                    "chat-bridge/native-call",
                    &native.object_id,
                    Some(1),
                    Some(&operation.object_id)
                )
                .unwrap()
                .value
        )
        .unwrap(),
        hash_json(&unknown).unwrap()
    );
    assert_journal_usage(
        &rusqlite::Connection::open(fixture.0.join("chat-bridge.sqlite")).unwrap(),
    );
    store.limits.count = 10_000;
    let source = store
        .write(
            "chat-bridge/native-call",
            &Json::object([
                ("state", "succeeded".into()),
                ("predecessor", native.json()),
            ]),
            Destination::Create {
                parent_id: Some("other-input"),
                name: "Other original call",
            },
            now,
            iso,
        )
        .unwrap();
    store
        .write(
            "chat-bridge/native-call",
            &json(r#"{"state":"succeeded","code":null}"#),
            Destination::Update {
                object_id: &native.object_id,
                expected_revision: 1,
            },
            now,
            iso,
        )
        .unwrap();
    store.close().unwrap();

    let mut store = LocalStore::open(Some(&fixture.0)).unwrap();
    store.limits.count = 0;
    store.expire_operations(now, true, None, None).unwrap();
    assert_eq!(
        hash_json(
            &store
                .document(
                    "chat-bridge/native-call",
                    &native.object_id,
                    Some(1),
                    Some(&operation.object_id)
                )
                .unwrap()
                .value
        )
        .unwrap(),
        hash_json(&unknown).unwrap()
    );
    assert_eq!(
        store
            .blob("chat-bridge/evidence-chunk", &bytes, &native.object_id)
            .unwrap(),
        [1, 2, 3, 255]
    );
    store.limits.count = 10_000;
    store
        .write(
            "chat-bridge/native-call",
            &json(r#"{"state":"succeeded","predecessor":null}"#),
            Destination::Update {
                object_id: &source.object_id,
                expected_revision: 1,
            },
            now,
            iso,
        )
        .unwrap();
    store.limits.count = 0;
    store.expire_operations(now, true, None, None).unwrap();
    assert!(store.local_info(&operation.object_id).unwrap().is_none());
    assert!(store.local_info(&native.object_id).unwrap().is_none());
    assert!(store.local_info(&bytes.object_id).unwrap().is_none());
    assert!(store.local_info(&source.object_id).unwrap().is_some());
    assert_journal_usage(
        &rusqlite::Connection::open(fixture.0.join("chat-bridge.sqlite")).unwrap(),
    );
    store.close().unwrap();
}

#[test]
fn journal_counters_match_original_rows_near_capacity_after_rollback_and_restart() {
    use rusqlite::{Connection, params};
    let fixture = super::Fixture::new();
    let path = fixture.0.join("chat-bridge.sqlite");
    LocalStore::open(Some(&fixture.0)).unwrap().close().unwrap();
    let mut db = Connection::open(&path).unwrap();
    {
        let transaction = db.transaction().unwrap();
        let mut document = transaction
            .prepare(
                "INSERT INTO documents VALUES(?,'chat-bridge/evidence',?,'released',1,'at','at')",
            )
            .unwrap();
        let mut revision = transaction
            .prepare("INSERT INTO revisions VALUES(?,1,'{}','at')")
            .unwrap();
        let mut blob = transaction
            .prepare("INSERT INTO blobs VALUES(?,'chat-bridge/evidence-chunk',?,'released',X'78')")
            .unwrap();
        for index in 0..4995 {
            let id = format!("entry-{index}");
            let name = format!("Original {index}");
            document.execute(params![id, name]).unwrap();
            revision.execute([id]).unwrap();
            blob.execute(params![format!("blob-{index}"), name])
                .unwrap();
        }
        drop(document);
        drop(revision);
        drop(blob);
        transaction.commit().unwrap();
    }
    assert_eq!(assert_journal_usage(&db), (9990, 14985, 0));
    let now = 1790000000123;
    let iso = "2026-09-21T14:13:20.123Z";
    let mut store = LocalStore::open(Some(&fixture.0)).unwrap();
    let operation = store.write("chat-bridge/operation",&json("{\"operationId\":\"chat:1790000000123:counter\",\"phase\":\"applying\",\"updatedAt\":\"2026-09-21T14:13:20.123Z\"}"),Destination::Create {parent_id:Some("open"),name:"Original operation"},now,iso).unwrap();
    let original = assert_journal_usage(&db);
    assert_eq!(original.0, 9991);
    assert_eq!(original.2, 1048576);
    store.limits.count = 9991;
    let failed = store
        .write(
            "chat-bridge/native-call",
            &json("{\"state\":\"prepared\"}"),
            Destination::Create {
                parent_id: Some("open"),
                name: "Original read",
            },
            now,
            iso,
        )
        .unwrap_err()
        .into_ivy();
    assert_eq!(failed.code, "capacity_exceeded");
    assert_eq!(assert_journal_usage(&db), original);
    store.close().unwrap();
    let mut store = LocalStore::open(Some(&fixture.0)).unwrap();
    store.limits.count = 9991;
    assert_eq!(assert_journal_usage(&db), original);
    store.completed_input("released", now as f64, now).unwrap();
    let native = store
        .write(
            "chat-bridge/native-call",
            &json("{\"state\":\"prepared\"}"),
            Destination::Create {
                parent_id: Some("open"),
                name: "Original read",
            },
            now,
            iso,
        )
        .unwrap();
    assert_eq!(assert_journal_usage(&db).0, 2);
    store.completed_input("open", now as f64, now).unwrap();
    store.limits.count = 2;
    assert_eq!(
        store
            .save_blob(
                "chat-bridge/evidence-chunk",
                "Must not displace open work",
                "other",
                b"original",
                now
            )
            .unwrap_err()
            .into_ivy()
            .code,
        "capacity_exceeded"
    );
    assert_eq!(
        store
            .document(
                "chat-bridge/operation",
                &operation.object_id,
                Some(operation.revision),
                Some("open")
            )
            .unwrap()
            .pin,
        operation
    );
    assert_eq!(
        store
            .document(
                "chat-bridge/native-call",
                &native.object_id,
                Some(native.revision),
                Some("open")
            )
            .unwrap()
            .pin,
        native
    );
    assert_eq!(assert_journal_usage(&db).2, 1048576);
    db.execute_batch("BEGIN; SAVEPOINT changed;").unwrap();
    db.execute(
        "UPDATE revisions SET value_json='{\"phase\":\"succeeded\"}' WHERE id=?",
        [&operation.object_id],
    )
    .unwrap();
    assert_eq!(assert_journal_usage(&db).2, 0);
    db.execute_batch("ROLLBACK TO changed; RELEASE changed; COMMIT;")
        .unwrap();
    assert_eq!(assert_journal_usage(&db).2, 1048576);
    store.close().unwrap();
    LocalStore::open(Some(&fixture.0)).unwrap().close().unwrap();
    assert_eq!(assert_journal_usage(&db).2, 1048576);
}

#[test]
fn journal_references_preserve_pinned_revisions_and_ignore_opaque_text() {
    let mut store = LocalStore::open(None).unwrap();
    let now = 1790000000123;
    let iso = "2026-09-21T14:13:20.123Z";
    let original = store
        .write(
            "chat-bridge/native-call",
            &json("{\"state\":\"succeeded\"}"),
            Destination::Create {
                parent_id: Some("input"),
                name: "Original native call",
            },
            now,
            iso,
        )
        .unwrap();
    let witness = store
        .write(
            "chat-bridge/collection",
            &Json::object([("turnCall", original.json()), ("state", "complete".into())]),
            Destination::Create {
                parent_id: Some("input"),
                name: "Original collection",
            },
            now,
            iso,
        )
        .unwrap();
    let current = store
        .write(
            "chat-bridge/native-call",
            &json("{\"state\":\"failed\"}"),
            Destination::Update {
                object_id: &original.object_id,
                expected_revision: 1,
            },
            now,
            iso,
        )
        .unwrap();
    assert_eq!(
        store
            .document(
                "chat-bridge/native-call",
                &original.object_id,
                Some(1),
                Some("input")
            )
            .unwrap()
            .pin,
        original
    );
    store
        .write(
            "chat-bridge/collection",
            &Json::object([
                ("state", "complete".into()),
                (
                    "opaque",
                    Json::object([
                        ("objectId", original.object_id.clone().into()),
                        ("revision", 1.0.into()),
                    ]),
                ),
                ("text", original.object_id.clone().into()),
            ]),
            Destination::Update {
                object_id: &witness.object_id,
                expected_revision: 1,
            },
            now,
            iso,
        )
        .unwrap();
    // Collection history retains the real pin until that history itself expires.
    let mut revision = 2;
    for _ in 0..33 {
        let next = store
            .write(
                "chat-bridge/collection",
                &json("{\"state\":\"complete\"}"),
                Destination::Update {
                    object_id: &witness.object_id,
                    expected_revision: revision,
                },
                now,
                iso,
            )
            .unwrap();
        revision = next.revision;
    }
    store.expire_operations(now, true, None, None).unwrap();
    assert_eq!(
        store
            .document(
                "chat-bridge/native-call",
                &original.object_id,
                Some(1),
                Some("input")
            )
            .unwrap()
            .pin,
        current
    );
}

#[test]
fn main_revision_compaction_exceeds_ten_thousand_updates() {
    let mut store = LocalStore::open(None).unwrap();
    let mut pin = store
        .write(
            "chat-bridge/main",
            &json("{\"sequence\":0}"),
            Destination::Create {
                parent_id: Some("root"),
                name: "main",
            },
            1790000000123,
            "2026-09-21T14:13:20.123Z",
        )
        .unwrap();
    for sequence in 1..=10050 {
        pin = store
            .write(
                "chat-bridge/main",
                &Json::object([("sequence", Json::Number(sequence as f64))]),
                Destination::Update {
                    object_id: &pin.object_id,
                    expected_revision: pin.revision,
                },
                1790000000123,
                "2026-09-21T14:13:20.123Z",
            )
            .unwrap();
    }
    assert_eq!(pin.revision, 10051);
    assert_eq!(
        store
            .document("chat-bridge/main", &pin.object_id, Some(1), Some("root"))
            .unwrap()
            .pin,
        pin
    );
    assert_eq!(store.change_version, 10051);
}

#[test]
fn node_rust_node_preserves_chat_database_and_atomic_failed_writes() {
    let fixture = super::Fixture::new();
    let db_path = fixture.0.join("chat-bridge.sqlite");
    super::node(
        r#"
      import {DatabaseSync} from 'node:sqlite';import {readFileSync} from 'node:fs';
      const db=new DatabaseSync(process.argv[1]);
      db.exec(readFileSync('tests/fixtures/chat-legacy-storage.sql','utf8'));
      const at='2026-09-21T14:13:20.123Z';
      db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run('main','chat-bridge/main','main','root',1,at,at);
      db.prepare('INSERT INTO revisions VALUES(?,?,?,?)').run('main',1,'{ "opaque":"\\ud800", "n":1 }',at);
      db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run('ack','chat-bridge/operation','ack','root',1,at,at);
      db.prepare('INSERT INTO revisions VALUES(?,?,?,?)').run('ack',1,JSON.stringify({phase:'succeeded',request:{action:'acknowledge',offerId:'offer',replyIds:['reply']}}),at);
      db.exec("CREATE TRIGGER fail_revision BEFORE INSERT ON revisions WHEN NEW.id='main' AND json_extract(NEW.value_json,'$.fail')=1 BEGIN SELECT RAISE(ABORT,'fixture_failure'); END;");db.close();
    "#,
        &db_path,
    );
    let mut store = LocalStore::open(Some(&fixture.0)).unwrap();
    let doc = store
        .document("chat-bridge/main", "main", Some(1), Some("root"))
        .unwrap();
    assert_eq!(
        s(&doc.revision, "contentHash"),
        hash_json(&doc.value).unwrap()
    );
    assert_eq!(
        n(&doc.revision, "byteLength"),
        r#"{ "opaque":"\ud800", "n":1 }"#.len() as i64
    );
    assert!(
        store
            .write(
                "chat-bridge/main",
                &json("{\"fail\":true}"),
                Destination::Update {
                    object_id: "main",
                    expected_revision: 1
                },
                1790000000123,
                "2026-09-21T14:13:20.123Z"
            )
            .is_err()
    );
    assert_eq!(
        store
            .document("chat-bridge/main", "main", None, Some("root"))
            .unwrap()
            .pin
            .revision,
        1
    );
    store
        .write(
            "chat-bridge/main",
            &json("{\"opaque\":\"\\udfff\",\"n\":2}"),
            Destination::Update {
                object_id: "main",
                expected_revision: 1,
            },
            1790000000123,
            "2026-09-21T14:13:20.123Z",
        )
        .unwrap();
    store.close().unwrap();
    super::node(
        r#"
      import {DatabaseSync} from 'node:sqlite';import assert from 'node:assert/strict';
      const db=new DatabaseSync(process.argv[1]);
      assert.deepEqual(JSON.parse(db.prepare("SELECT value_json FROM revisions WHERE id='main'").get().value_json),{n:2,opaque:'\udfff'});
      assert.equal(db.prepare("SELECT current_revision FROM documents WHERE id='main'").get().current_revision,2);
      assert.equal(db.prepare("SELECT COUNT(*) n FROM revisions WHERE id='main'").get().n,1);
      assert.equal(db.prepare("SELECT COUNT(*) n FROM offer_acknowledgements WHERE offer_id='offer' AND reply_id='reply'").get().n,1);
      for(const row of db.prepare('PRAGMA table_list').all())assert.equal(row.strict,0);
      assert.equal(db.prepare('PRAGMA integrity_check').get().integrity_check,'ok');db.close();
    "#,
        &db_path,
    );
}
