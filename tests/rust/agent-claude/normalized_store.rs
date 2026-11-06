use super::*;
use ivy_agent_manager::claude::store::{ItemDelta, ItemsView, PageRequest, TurnCursor};

fn open(root: &Root) -> SessionStore {
    SessionStore::with_clock(&root.0, &root.0.join("state.sqlite"), Arc::new(|| 1000.0)).unwrap()
}

#[test]
fn source_upgrade_is_atomic_and_keeps_invalid_bytes_for_repair() {
    for source in ["{", "[invalid]", "😀", "null", "{}", "1", r#""text""#] {
        let root = Root::new();
        original_run(
            &root,
            "",
            1_000_000,
            &[
                op("upsertThread", vec![thread("a", 100.0)]),
                op(
                    "upsertTurn",
                    vec![turn(
                        "good",
                        "a",
                        100.0.into(),
                        v(r#"[{"id":"saved","text":"keep"}]"#),
                    )],
                ),
                op("upsertTurn", vec![turn("bad", "a", 200.0.into(), v("[]"))]),
                op(
                    "sql",
                    vec![
                        format!(
                            "UPDATE turns SET items_json='{}' WHERE id='bad'",
                            source.replace('\'', "''")
                        )
                        .into(),
                    ],
                ),
            ],
        );
        let path = root.0.join("state.sqlite");
        let before = fs::read(&path).unwrap();
        let error = SessionStore::open(&root.0, &path).err().unwrap();
        assert!(!error.message.0.is_empty());
        assert_eq!(fs::read(&path).unwrap(), before);
        let db = Connection::open(&path).unwrap();
        let retained: String = db
            .query_row("SELECT items_json FROM turns WHERE id='bad'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(retained, source);
        assert!(
            query(
                &db,
                "SELECT name FROM sqlite_master WHERE name='turn_items'"
            )
            .as_array()
            .unwrap()
            .is_empty()
        );
    }
}

#[test]
fn streamed_text_is_durable_without_rewriting_other_items_or_turn_metadata() {
    let root = Root::new();
    let mut store = open(&root);
    store.upsert_thread(&thread("a", 100.0)).unwrap();
    store
        .upsert_turn(&turn(
            "t",
            "a",
            200.0.into(),
            v(r#"[
        {"id":"answer","type":"agentMessage","text":"prefix ","phase":null},
        {"id":"thinking","type":"reasoning","content":[""]},
        {"id":"command","type":"commandExecution","aggregatedOutput":null,"status":"inProgress"},
        {"id":"unrelated","type":"mcpToolCall"}
    ]"#),
        ))
        .unwrap();
    let db = Connection::open(root.0.join("state.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE writes(kind TEXT); CREATE TRIGGER record_turn AFTER UPDATE ON turns BEGIN INSERT INTO writes VALUES('turn'); END;
        CREATE TRIGGER record_item AFTER UPDATE ON turn_items BEGIN INSERT INTO writes VALUES('item'); END;
        UPDATE turn_items SET body_json='{' WHERE item_key='\"unrelated\"'; DELETE FROM writes;").unwrap();
    let delta = js(&v(r#""chunk 😀 \ud800\n""#)).clone();
    let mut expected = JsString::from("prefix ");
    for index in 0..300 {
        store
            .append_item_delta(&"t".into(), &"answer".into(), ItemDelta::Text, &delta)
            .unwrap();
        expected.0.extend_from_slice(&delta.0);
        if index == 137 {
            store.close().unwrap();
            store = open(&root);
        }
    }
    store
        .append_item_delta(
            &"t".into(),
            &"thinking".into(),
            ItemDelta::Reasoning,
            &delta,
        )
        .unwrap();
    store
        .append_item_delta(
            &"t".into(),
            &"command".into(),
            ItemDelta::CommandOutput,
            &delta,
        )
        .unwrap();
    assert!(store.turn_in_progress(&"t".into()).unwrap());
    assert_eq!(
        field(
            &store
                .get_item(&"t".into(), &"answer".into())
                .unwrap()
                .unwrap(),
            "text"
        ),
        &IvyJson::String(expected.clone())
    );
    assert_eq!(
        field(
            &store
                .get_item(&"t".into(), &"thinking".into())
                .unwrap()
                .unwrap(),
            "content"
        ),
        &IvyJson::Array(vec![IvyJson::String(delta.clone())])
    );
    assert_eq!(
        field(
            &store
                .get_item(&"t".into(), &"command".into())
                .unwrap()
                .unwrap(),
            "aggregatedOutput"
        ),
        &IvyJson::String(delta.clone())
    );
    assert_eq!(
        query(&db, "SELECT COUNT(*) AS count FROM writes"),
        v(r#"[{"count":0}]"#)
    );
    assert!(store.get_turn(&"t".into()).is_err());
    // A failed terminal-item write must keep every accepted stream chunk.
    db.execute_batch("CREATE TRIGGER refuse_item BEFORE UPDATE ON turn_items BEGIN SELECT RAISE(ABORT,'write denied'); END;").unwrap();
    assert!(
        store
            .update_item(&"t".into(), &"answer".into(), |mut item| {
                item.insert("phase", "final_answer".into());
                Ok(item)
            })
            .is_err()
    );
    assert_eq!(
        field(
            &store
                .get_item(&"t".into(), &"answer".into())
                .unwrap()
                .unwrap(),
            "text"
        ),
        &IvyJson::String(expected.clone())
    );
    db.execute_batch("DROP TRIGGER refuse_item").unwrap();
    store
        .update_item(&"t".into(), &"answer".into(), |mut item| {
            item.insert("phase", "final_answer".into());
            Ok(item)
        })
        .unwrap();
    assert_eq!(
        query(
            &db,
            "SELECT COUNT(*) AS count FROM item_deltas WHERE position=0"
        ),
        v(r#"[{"count":0}]"#)
    );
    store.close().unwrap();
    let store = open(&root);
    let answer = store
        .get_item(&"t".into(), &"answer".into())
        .unwrap()
        .unwrap();
    assert_eq!(field(&answer, "text"), &IvyJson::String(expected));
    assert_eq!(field(&answer, "phase"), &"final_answer".into());
}

#[test]
fn sql_pages_keep_ties_null_times_reverse_anchors_and_skip_unrequested_bodies() {
    let root = Root::new();
    let store = open(&root);
    store.upsert_thread(&thread("a", 100.0)).unwrap();
    store.upsert_thread(&thread("foreign", 100.0)).unwrap();
    let mut expected = Vec::new();
    for index in 0..220 {
        let items = IvyJson::Array(vec![
            IvyJson::object([
                ("id", format!("u{index}").into()),
                ("type", "userMessage".into()),
            ]),
            IvyJson::object([
                ("id", format!("c{index}").into()),
                ("type", "commandExecution".into()),
                ("aggregatedOutput", "x".repeat(8192).into()),
            ]),
            IvyJson::object([
                ("id", format!("a{index}").into()),
                ("type", "agentMessage".into()),
                ("text", "answer 😀".into()),
            ]),
        ]);
        expected.extend([
            format!("u{index}"),
            format!("c{index}"),
            format!("a{index}"),
        ]);
        store
            .upsert_turn(&turn(
                &format!("turn{index:03}"),
                "a",
                if index < 5 {
                    IvyJson::Null
                } else {
                    500.0.into()
                },
                items,
            ))
            .unwrap();
    }
    let db = Connection::open(root.0.join("state.sqlite")).unwrap();
    let original: String = db
        .query_row(
            "SELECT body_json FROM turn_items WHERE turn_id='turn219' AND position=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE turn_items SET body_json='{' WHERE turn_id='turn219' AND position=1",
        [],
    )
    .unwrap();
    let page = store
        .items_page(
            &"a".into(),
            None,
            PageRequest {
                offset: 0,
                limit: 3,
                descending: false,
            },
        )
        .unwrap();
    assert_eq!(page.data.len(), 3);
    assert!(page.has_more);
    for view in [ItemsView::Summary, ItemsView::NotLoaded] {
        let page = store.turns_page(&"a".into(), None, 1, true, view).unwrap();
        assert_eq!(field(&page.data[0], "id"), &"turn219".into());
        assert_eq!(
            field(&page.data[0], "items").as_array().unwrap().len(),
            if matches!(view, ItemsView::Summary) {
                2
            } else {
                0
            }
        );
    }
    assert!(
        store
            .turns_page(&"a".into(), None, 1, true, ItemsView::Full)
            .is_err()
    );
    assert_eq!(
        store
            .turns_page(&"a".into(), None, 1, false, ItemsView::Full)
            .unwrap()
            .data
            .len(),
        1
    );
    db.execute(
        "UPDATE turn_items SET body_json=? WHERE turn_id='turn219' AND position=1",
        [&original],
    )
    .unwrap();
    for descending in [false, true] {
        let mut actual = Vec::new();
        let mut cursor = None;
        loop {
            let page = store
                .turns_page(
                    &"a".into(),
                    cursor.as_ref(),
                    17,
                    descending,
                    ItemsView::NotLoaded,
                )
                .unwrap();
            let back = TurnCursor::decode(&page.backwards.as_ref().unwrap().encode()).unwrap();
            let reverse = store
                .turns_page(
                    &"a".into(),
                    Some(&back),
                    1,
                    !descending,
                    ItemsView::NotLoaded,
                )
                .unwrap();
            assert_eq!(field(&page.data[0], "id"), field(&reverse.data[0], "id"));
            actual.extend(page.data.iter().map(|t| field(t, "id").as_str().unwrap()));
            cursor = page.next.map(|c| TurnCursor::decode(&c.encode()).unwrap());
            if cursor.is_none() {
                break;
            }
        }
        let mut turns = (0..220).map(|i| format!("turn{i:03}")).collect::<Vec<_>>();
        if descending {
            turns.reverse();
        }
        assert_eq!(actual, turns);
        let mut actual = Vec::new();
        loop {
            let page = store
                .items_page(
                    &"a".into(),
                    None,
                    PageRequest {
                        offset: actual.len() as u64,
                        limit: 31,
                        descending,
                    },
                )
                .unwrap();
            actual.extend(
                page.data
                    .iter()
                    .map(|entry| field(field(entry, "item"), "id").as_str().unwrap()),
            );
            if !page.has_more {
                break;
            }
        }
        let mut ordered = expected.clone();
        if descending {
            ordered.reverse();
        }
        assert_eq!(actual, ordered);
    }
    let page = store
        .items_page(
            &"a".into(),
            Some(&"turn100".into()),
            PageRequest {
                offset: 1,
                limit: 1,
                descending: false,
            },
        )
        .unwrap();
    assert_eq!(field(field(&page.data[0], "item"), "id"), &"c100".into());
    assert!(
        store
            .items_page(
                &"foreign".into(),
                Some(&"turn100".into()),
                PageRequest {
                    offset: 0,
                    limit: 1,
                    descending: false
                }
            )
            .is_err()
    );
    assert!(
        store
            .items_page(
                &"a".into(),
                None,
                PageRequest {
                    offset: 661,
                    limit: 1,
                    descending: false
                }
            )
            .is_err()
    );
    assert!(
        !store
            .items_page(
                &"a".into(),
                None,
                PageRequest {
                    offset: 660,
                    limit: 1,
                    descending: false
                }
            )
            .unwrap()
            .has_more
    );
    assert!(TurnCursor::decode("after:0:null").is_err());
    assert!(TurnCursor::decode("after:1:NaN").is_err());
    store
        .append_item_delta(
            &"turn219".into(),
            &"a219".into(),
            ItemDelta::Text,
            &" delta".into(),
        )
        .unwrap();
    assert_eq!(store.delete_recent_turns(&"a".into(), 1.0).unwrap(), 1);
    assert_eq!(query(&db, "PRAGMA foreign_key_check"), v("[]"));
    assert_eq!(
        query(&db, "SELECT COUNT(*) AS count FROM item_deltas"),
        v(r#"[{"count":0}]"#)
    );
}

#[test]
fn native_errors_and_utf16_payloads_survive_atomic_replacements() {
    let root = Root::new();
    let store = open(&root);
    store.upsert_thread(&thread("a", 100.0)).unwrap();
    let mut missing = thread("missing-field", 300.0);
    missing.remove("forkedFromId");
    let error = store.upsert_thread(&missing).unwrap_err();
    assert_eq!(error.name, "invalid_arguments");
    assert_eq!(error.code, Some("invalid_arguments"));
    assert!(store.get_thread(&"missing-field".into()).unwrap().is_none());
    let error = store
        .list_threads(&ThreadList {
            cursor: Some(f64::NAN),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, Some("invalid_arguments"));
    let items =
        v(r#"[{"id":"\ud800","type":"agentMessage","text":"\udfff 😀","extra":{"nested":true}}]"#);
    store
        .upsert_turn(&turn("t", "a", 200.0.into(), items.clone()))
        .unwrap();
    let id = js(field(&items.as_array().unwrap()[0], "id"));
    assert_eq!(
        store.get_item(&"t".into(), id).unwrap().unwrap(),
        items.as_array().unwrap()[0]
    );
    for invalid in ["null", "{}", "1", r#""text""#] {
        let error = store
            .upsert_turn(&turn("t", "a", 200.0.into(), v(invalid)))
            .unwrap_err();
        assert_eq!(error.code, Some("invalid_arguments"));
        assert_eq!(
            field(&store.get_turn(&"t".into()).unwrap().unwrap(), "items"),
            &items
        );
    }
    let db = Connection::open(root.0.join("state.sqlite")).unwrap();
    db.execute_batch("UPDATE turn_items SET body_json='😀' WHERE turn_id='t'")
        .unwrap();
    let error = store.get_item(&"t".into(), id).unwrap_err();
    assert_eq!(error.name, "invalid_data");
    let retained: String = db
        .query_row(
            "SELECT body_json FROM turn_items WHERE turn_id='t'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, "😀");
    let section = store
        .create_section(&"s".into(), &" \u{feff}Section \n".into(), &IvyJson::Null)
        .unwrap();
    assert_eq!(field(&section, "name"), &"\u{feff}Section".into());
}
