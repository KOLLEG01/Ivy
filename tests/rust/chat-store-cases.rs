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
            assert_eq!(
                canonical(&translated(&actual, &pins, true), usize::MAX).unwrap(),
                canonical(&expected[index], usize::MAX).unwrap(),
                "{} step {index}: {original:?}",
                s(case, "label")
            );
        }
        store.close().unwrap();
    }
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
