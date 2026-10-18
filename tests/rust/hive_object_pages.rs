use super::{Root, compare, insert, json, result, seed, sql};
use serde_json::Value;

fn navigation_seed() -> String {
    let mut text = seed(false);
    text.push_str("BEGIN IMMEDIATE; UPDATE objects SET position=CAST(substr(id,3) AS INTEGER)%4 WHERE id GLOB 'id[0-9][0-9]';");
    for (id, parent, archived) in [
        ("child", "id01", false),
        ("grand", "child", false),
        ("hidden", "id01", true),
        ("hidden-child", "hidden", true),
        ("leaf", "child", false),
        ("empty-root", "id02", false),
        ("z", "id01", false),
        ("é", "id01", false),
        ("🙂", "id01", false),
        ("\u{e000}", "id01", false),
    ] {
        insert(
            &mut text,
            id,
            "fixture/value",
            "1.0.0",
            &format!("'{parent}'"),
            archived,
            "json",
            "{}",
            1,
        );
    }
    text.push_str("UPDATE objects SET path='/id01/child/grand',depth=3 WHERE id='grand'; UPDATE objects SET path='/id01/child/leaf',depth=3 WHERE id='leaf'; UPDATE objects SET path='/id01/hidden/hidden-child',depth=3,archived_at=NULL WHERE id='hidden-child'; UPDATE objects SET path='/id02/empty-root' WHERE id='empty-root';");
    text.push_str("INSERT INTO contract_families VALUES('fixture/owned','agent','application/json','owned'); INSERT INTO contracts VALUES('fixture/owned','1.0.0','{\"key\":\"fixture/owned\",\"version\":\"1.0.0\",\"owner\":{\"kind\":\"agent\"},\"mediaType\":\"application/json\",\"jsonSchema\":{\"type\":\"object\"},\"retention\":{\"objects\":{\"mode\":\"owned\"},\"revisions\":{\"mode\":\"all\"}},\"specMarkdown\":\"fixture\"}');");
    // Lifecycle ownership is immutable: seed it in the original insert.
    text.push_str("INSERT INTO objects VALUES(NULL,'owned-root',NULL,'id01','owned-root','/owned-root',0,NULL,1,'fixture/owned',1,NULL,0,'2026-01-01','2026-01-02'); INSERT INTO revisions VALUES('owned-root',1,'fixture/owned','1.0.0','json','{}','sha256:fixture',2,'2026-01-02','{}'); COMMIT;");
    text
}
fn action(op: &str, params: Value) -> Value {
    json!({"op":op,"params":params})
}
fn token(steps: &mut Vec<Value>, method: &str, params: &Value, boundary: Value) -> usize {
    let mut identity = params.clone();
    identity["method"] = json!(method);
    let index = steps.len();
    steps.push(json!({"op":"cursor","identity":identity,"boundary":boundary}));
    index
}

#[test]
fn sibling_pages_preserve_position_ties_scope_errors_and_changes_between_pages() {
    let root = Root::new();
    let mut steps = vec![sql(navigation_seed())];
    let root_params = json!({"parentId":null,"limit":7});
    let first = steps.len();
    steps.push(action("list", root_params.clone()));
    let mut previous = first;
    for _ in 0..5 {
        let index = steps.len();
        let mut page = action("list", root_params.clone());
        page["from"] = json!(previous);
        steps.push(page);
        previous = index;
    }
    let branch = json!({"parentId":"id01","limit":3});
    let children = steps.len();
    steps.push(action("list", branch.clone()));
    steps.push(json!({"op":"list","params":branch,"from":children}));
    steps.push(action(
        "list",
        json!({"parentId":"id01","includeArchived":true,"limit":200}),
    ));
    steps.push(action("list", json!({"parentId":"text","limit":50})));
    steps.push(action("list", json!({"parentId":"missing","cursor":"bad"})));
    steps.push(action("list", json!({"parentId":"id01","cursor":"bad"})));
    steps.push(json!({"op":"list","params":{"parentId":null,"limit":7,"includeArchived":false},"from":first}));
    steps.push(json!({"op":"list","params":{"parentId":"id01","limit":7},"from":first}));
    for boundary in [
        json!(null),
        json!(false),
        json!(0),
        json!([]),
        json!([0]),
        json!(["0", ""]),
        json!([0, 1]),
        json!([false, ""]),
        json!([0,"",{"ignored":true}]),
        json!([0, "\u{e000}"]),
    ] {
        let index = token(&mut steps, "objects.list", &root_params, boundary);
        steps.push(json!({"op":"list","params":root_params,"tokenFrom":index}));
    }
    let wrong = token(&mut steps, "objects.history", &root_params, json!([0, ""]));
    steps.push(json!({"op":"list","params":root_params,"tokenFrom":wrong}));
    // Mutate only a later leaf's navigation position, then consume the old keyset.
    steps.push(sql("UPDATE objects SET position=20 WHERE id='id39'"));
    steps.push(json!({"op":"list","params":root_params,"from":first}));
    steps.push(json!({"op":"list","params":{"parentId":null},"overdue":true}));
    steps.push(action("list", root_params));
    let rows = compare(&root, &steps, false);
    assert_eq!(result(&rows, first)["items"].as_array().unwrap().len(), 7);
    assert!(result(&rows, first)["nextCursor"].is_string());
    assert_eq!(
        result(&rows, children)["items"].as_array().unwrap().len(),
        3
    );
    assert_eq!(rows[wrong + 1]["error"]["data"]["code"], "invalid_cursor");
    assert_eq!(
        rows[steps.len() - 2]["error"]["data"]["code"],
        "limit_exceeded"
    );
    assert_eq!(
        result(&rows, steps.len() - 1)["items"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
}

#[test]
fn history_pages_preserve_number_coercion_pinned_references_and_retention_gaps() {
    let root = Root::new();
    let mut steps = vec![sql(navigation_seed())];
    steps.push(sql("BEGIN IMMEDIATE; INSERT INTO revisions SELECT object_id,4,contract_key,'2.0.0',encoding,'{}',content_hash,2,created_at,'{\"pinned\":{\"objectId\":\"id01\",\"revision\":1}}' FROM revisions WHERE object_id='history' AND revision=3; INSERT INTO revisions SELECT object_id,5,contract_key,'2.0.0',encoding,'{}',content_hash,2,created_at,'{}' FROM revisions WHERE object_id='history' AND revision=3; INSERT INTO revision_references VALUES('history',4,'pinned','id01',1); UPDATE objects SET current_revision=5 WHERE id='history'; COMMIT;"));
    let params = json!({"objectId":"history","limit":2});
    let first = steps.len();
    steps.push(action("history", params.clone()));
    steps.push(json!({"op":"history","params":params,"from":first}));
    let third = steps.len();
    steps.push(action("history", json!({"objectId":"history","limit":200})));
    steps.push(json!({"op":"history","params":{"objectId":"history","limit":3},"from":first}));
    steps.push(action(
        "history",
        json!({"objectId":"missing","cursor":"bad"}),
    ));
    for boundary in [
        json!(0),
        json!(null),
        json!(false),
        json!(true),
        json!(""),
        json!(" 0x4 "),
        json!("3.5"),
        json!("Infinity"),
        json!("-Infinity"),
        json!("NaN"),
        json!("not a number"),
        json!([]),
        json!([4]),
        json!([null]),
        json!([4, 3]),
        json!({}),
        json!({"valueOf":2}),
        json!({"toString":1}),
    ] {
        let index = token(&mut steps, "objects.history", &params, boundary);
        steps.push(json!({"op":"history","params":params,"tokenFrom":index}));
    }
    steps.push(sql("INSERT INTO revisions SELECT object_id,2,contract_key,contract_version,encoding,content,content_hash,byte_length,created_at,references_json FROM revisions WHERE object_id='history' AND revision=1;"));
    let complete = steps.len();
    steps.push(action("history", json!({"objectId":"history","limit":200})));
    steps.push(sql("DELETE FROM revision_references WHERE source_object_id='history' AND source_revision=4; DELETE FROM revisions WHERE object_id='history' AND revision=4;"));
    let gap = steps.len();
    steps.push(json!({"op":"history","params":params,"from":first}));
    steps.push(json!({"op":"history","params":params,"overdue":true}));
    steps.push(json!({"op":"reopen"}));
    steps.push(action("history", json!({"objectId":"history","limit":200})));
    let rows = compare(&root, &steps, false);
    assert_eq!(result(&rows, first)["items"][0]["revision"], 5);
    assert_eq!(
        result(&rows, first)["items"][1]["references"]["pinned"]["revision"],
        1
    );
    assert_eq!(result(&rows, third)["items"].as_array().unwrap().len(), 4);
    assert_eq!(result(&rows, third)["historyComplete"], false);
    assert_eq!(result(&rows, complete)["historyComplete"], true);
    assert_eq!(result(&rows, gap)["historyComplete"], false);
    assert_eq!(
        result(&rows, steps.len() - 1)["items"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn tree_pages_keep_breadth_path_order_archived_branches_and_truncation() {
    let root = Root::new();
    let mut steps = vec![sql(navigation_seed())];
    for root_id in [
        Value::Null,
        json!("id01"),
        json!("child"),
        json!("hidden"),
        json!("text"),
        json!("missing"),
    ] {
        for archived in [false, true] {
            for depth in [1, 2, 3] {
                steps.push(action(
                    "tree",
                    json!({"rootId":root_id,"includeArchived":archived,"depth":depth,"limit":200}),
                ));
            }
        }
    }
    let branch = steps.len();
    steps.push(action(
        "tree",
        json!({"rootId":"id01","depth":3,"limit":200}),
    ));
    steps.push(action("tree", json!({"rootId":"id01","depth":3,"limit":3})));
    steps.push(json!({"op":"tree","params":{"rootId":null},"overdue":true}));
    steps.push(action("tree", json!({"rootId":"text"})));
    let rows = compare(&root, &steps, false);
    assert_eq!(result(&rows, branch)["truncated"], false);
    assert_eq!(result(&rows, branch + 1)["truncated"], true);
    assert_eq!(rows[branch + 2]["error"]["data"]["code"], "limit_exceeded");
    assert_eq!(result(&rows, steps.len() - 1)["items"], json!([]));
    let items = result(&rows, branch)["items"].as_array().unwrap().clone();
    assert!(items.iter().any(|row| row["id"] == "grand"));
    assert!(!items.iter().any(|row| row["id"] == "hidden-child"
        || row["id"] == "text"
        || row["id"] == "owned-root"));
}

#[test]
fn navigation_metadata_pages_retain_original_row_limit_without_query_byte_cap() {
    let root = Root::new();
    let mut text = seed(false);
    text.push_str("BEGIN IMMEDIATE;");
    let mut path = String::new();
    let mut parent = String::from("NULL");
    for depth in 1..=60 {
        let id = format!("deep{depth:02}");
        let name = format!("level{depth:02}{}", "x".repeat(240));
        path.push('/');
        path.push_str(&name);
        insert(
            &mut text,
            &id,
            "fixture/value",
            "1.0.0",
            &parent,
            false,
            "json",
            "{}",
            1,
        );
        text.push_str(&format!(
            "UPDATE objects SET name='{name}',path='{path}',depth={depth} WHERE id='{id}';"
        ));
        parent = format!("'{id}'");
    }
    for index in 0..160 {
        let id = format!("wide{index:03}");
        insert(
            &mut text,
            &id,
            "fixture/value",
            "1.0.0",
            &parent,
            false,
            "json",
            "{}",
            1,
        );
        text.push_str(&format!(
            "UPDATE objects SET path='{path}/{id}',depth=61,position={index} WHERE id='{id}';"
        ));
    }
    text.push_str("COMMIT;");
    let steps = vec![
        sql(text),
        action("list", json!({"parentId":"deep60","limit":200})),
    ];
    let rows = compare(&root, &steps, false);
    assert_eq!(result(&rows, 1)["items"].as_array().unwrap().len(), 160);
    assert!(rows[1]["value"].as_str().unwrap().len() > 2 * 1024 * 1024);
    assert!(result(&rows, 1)["nextCursor"].is_null());
}
