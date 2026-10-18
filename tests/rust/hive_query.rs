use super::Root;
use ivy_contracts::{
    IvyJson, IvyResult,
    json::{canonical, parse, stringify},
};
use ivy_hive::{
    objects::{self, Location},
    paging::catalog_page,
    query::query_objects,
    store::Store,
};
use rusqlite::types::{Value as SqlValue, ValueRef};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

fn fixture(input: Value) -> Value {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tests/fixtures/hive-query-oracle.mjs"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn field(step: &Value, name: &str) -> IvyJson {
    match step.get(format!("{name}Json")) {
        Some(v) => parse(v.as_str().unwrap(), usize::MAX).unwrap(),
        None => IvyJson::from_serde(&step[name]).unwrap(),
    }
}
fn native(path: &Path, steps: &[Value], retained: bool) -> Value {
    let mut store = Store::open(path).unwrap();
    if !retained {
        store
            .set_metadata("cursor_secret", &"0123456789abcdef".repeat(4))
            .unwrap();
        drop(store);
        store = Store::open(path).unwrap();
    }
    let mut values: Vec<Option<IvyJson>> = vec![];
    let mut observations = vec![];
    for step in steps {
        if step["op"] == "reopen" {
            drop(store);
            store = Store::open(path).unwrap();
            values.push(None);
            observations.push(json!({"value":null,"error":null}));
            continue;
        }
        let result: IvyResult<Option<IvyJson>> = (|| {
            Ok(match step["op"].as_str().unwrap() {
                "sql" => {
                    store
                        .connection()
                        .execute_batch(step["sql"].as_str().unwrap())
                        .map_err(|e| store.query_error(e))?;
                    None
                }
                "cursor" => Some(
                    store
                        .cursor(&field(step, "identity"), &field(step, "boundary"))?
                        .into(),
                ),
                "readCursor" => {
                    let token = step
                        .get("from")
                        .map(|i| {
                            values[i.as_u64().unwrap() as usize]
                                .as_ref()
                                .unwrap()
                                .as_js_string()
                                .unwrap()
                                .clone()
                        })
                        .unwrap_or_else(|| step["cursor"].as_str().unwrap().into());
                    let value = store.read_cursor(&field(step, "identity"), &token)?;
                    Some(IvyJson::object([
                        ("present", value.is_some().into()),
                        ("value", value.unwrap_or(IvyJson::Null)),
                    ]))
                }
                "catalog" | "query" => {
                    let mut params = field(step, "params");
                    if let Some(index) = step["from"].as_u64() {
                        params.insert(
                            "cursor",
                            values[index as usize]
                                .as_ref()
                                .unwrap()
                                .get("nextCursor")
                                .unwrap()
                                .clone(),
                        );
                    }
                    if let Some(index) = step["tokenFrom"].as_u64() {
                        params.insert("cursor", values[index as usize].as_ref().unwrap().clone());
                    }
                    if step["op"] == "query" {
                        Some(
                            store.with_budget(Duration::from_secs(10), |s| {
                                query_objects(s, &params)
                            })?,
                        )
                    } else {
                        let mut records = field(step, "records").into_array().unwrap();
                        Some(catalog_page(
                            &store,
                            &mut records,
                            &field(step, "identity"),
                            |r| r.get("key").unwrap().as_js_string().unwrap().clone(),
                            params
                                .get("limit")
                                .and_then(IvyJson::as_f64)
                                .map(|v| v as usize),
                            params.get("cursor").and_then(IvyJson::as_js_string),
                        )?)
                    }
                }
                "read" | "stat" => {
                    let params = field(step, "params");
                    let location =
                        if let Some(id) = params.get("objectId").and_then(IvyJson::as_js_string) {
                            Location::Id(id)
                        } else {
                            Location::Path(params.get("path").unwrap().as_js_string().unwrap())
                        };
                    Some(if step["op"] == "stat" {
                        objects::stat(&store, location)?
                    } else {
                        objects::read(
                            &store,
                            location,
                            params
                                .get("revision")
                                .and_then(IvyJson::as_f64)
                                .map(|v| v as u64),
                        )?
                    })
                }
                "scalar" => Some(store.with_budget(
                    if step["overdue"] == true {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(10)
                    },
                    |s| {
                        let mut statement = s
                            .connection()
                            .prepare(step["sql"].as_str().unwrap())
                            .map_err(|e| s.query_error(e))?;
                        let names = statement
                            .column_names()
                            .iter()
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>();
                        let bindings = step["bindings"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .map(|v| match v {
                                        Value::Null => SqlValue::Null,
                                        Value::String(v) => SqlValue::Text(v.clone()),
                                        Value::Bool(v) => SqlValue::Integer(i64::from(*v)),
                                        _ => SqlValue::Real(v.as_f64().unwrap()),
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        let mut query = statement
                            .query(rusqlite::params_from_iter(bindings))
                            .map_err(|e| s.query_error(e))?;
                        let mut rows = vec![];
                        while let Some(row) = query.next().map_err(|e| s.query_error(e))? {
                            let mut pairs = vec![];
                            for (index, name) in names.iter().enumerate() {
                                let value =
                                    match row.get_ref(index).map_err(|e| s.query_error(e))? {
                                        ValueRef::Null => IvyJson::Null,
                                        ValueRef::Integer(v) => IvyJson::Number(v as f64),
                                        ValueRef::Real(v) => IvyJson::Number(v),
                                        ValueRef::Text(v) => {
                                            String::from_utf8_lossy(v).into_owned().into()
                                        }
                                        _ => panic!("Unexpected blob"),
                                    };
                                pairs.push((name.as_str(), value));
                            }
                            rows.push(IvyJson::object(pairs));
                        }
                        Ok(IvyJson::Array(rows))
                    },
                )?),
                _ => panic!("Unknown trace action"),
            })
        })();
        match result {
            Ok(value) => {
                observations.push(json!({"value":value.as_ref().map(|v|stringify(v,usize::MAX).unwrap()),"error":null}));
                values.push(value);
            }
            Err(error) => {
                observations.push(json!({"value":null,"error":serde_json::from_str::<Value>(&stringify(&error.to_wire(),1<<20).unwrap()).unwrap()}));
                values.push(None);
            }
        }
    }
    json!(observations)
}
fn normalized(value: &Value) -> Value {
    match value.as_str() {
        Some(text) => json!(canonical(&parse(text, usize::MAX).unwrap(), usize::MAX).unwrap()),
        None => value.clone(),
    }
}
fn compare(root: &Root, steps: &[Value], retained: bool) -> Value {
    let expected =
        fixture(json!({"path":root.0.join("reference.sqlite"),"steps":steps,"retained":retained}));
    let actual = native(&root.0.join("native.sqlite"), steps, retained);
    assert_eq!(actual.as_array().unwrap().len(), steps.len());
    for (index, (left, right)) in actual
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            left["error"], right["error"],
            "error step {index}: {}",
            steps[index]
        );
        if steps[index]["op"] == "sql" {
            assert!(
                right["error"].is_null(),
                "Fixture setup failed at step {index}"
            );
        }
        assert_eq!(
            normalized(&left["value"]),
            normalized(&right["value"]),
            "value step {index}: {}",
            steps[index]
        );
    }
    assert!(
        expected
            .as_array()
            .unwrap()
            .iter()
            .any(|r| !r["value"].is_null())
    );
    expected
}
fn result(rows: &Value, index: usize) -> Value {
    serde_json::from_str(
        rows[index]["value"]
            .as_str()
            .expect("Missing successful result"),
    )
    .unwrap()
}
fn quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "''"))
}
fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn seed(large: bool) -> String {
    let mut sql = String::from("BEGIN IMMEDIATE;");
    for (key, media, schema) in [
        (
            "fixture/value",
            "application/json",
            json!({"type":"object","properties":{"score":{"type":["number","null"]},"flag":{"type":"boolean"},"title":{"type":"string"},"tags":{"type":"array","items":{"type":"string"}},"obj":{"type":"object","properties":{"x":{"type":"integer"}}}}}),
        ),
        (
            "fixture/large",
            "application/fixture+json",
            json!({"type":"object","properties":{"text":{"type":"string"}}}),
        ),
        ("fixture/text", "text/plain", Value::Null),
        ("fixture/binary", "application/octet-stream", Value::Null),
    ] {
        sql.push_str(&format!(
            "INSERT INTO contract_families VALUES({},'agent',{},'retain');",
            quote(key),
            quote(media)
        ));
        for version in ["1.0.0", "2.0.0"] {
            let mut definition = json!({"key":key,"version":version,"mediaType":media,"owner":{"kind":"agent"},"retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}},"specMarkdown":"fixture"});
            if !schema.is_null() {
                definition["jsonSchema"] = schema.clone();
            }
            if key == "fixture/value" && version == "2.0.0" {
                definition["jsonSchema"]["properties"]["score"] = json!({"type":"string"});
            }
            sql.push_str(&format!(
                "INSERT INTO contracts VALUES({},{},{});",
                quote(key),
                quote(version),
                quote(&definition.to_string())
            ));
        }
    }
    for index in 0..40 {
        let id = format!("id{index:02}");
        let archived = index % 7 == 0;
        let parent = if index == 20 {
            "'id01'".to_string()
        } else {
            "NULL".to_string()
        };
        let version = if index % 5 == 0 { "2.0.0" } else { "1.0.0" };
        let score = if version == "2.0.0" {
            json!(format!("{index}"))
        } else if index % 6 == 0 {
            Value::Null
        } else {
            json!(index % 4)
        };
        let mut content = json!({"score":score,"flag":index%2==0,"title":format!("{}-{index}",if index%3==0{"Äbc"}else{"abc"}),"tags":["first","second"],"obj":{"x":index},"a/b":{"~key":index}});
        if index % 9 == 0 {
            content.as_object_mut().unwrap().remove("score");
        }
        insert(
            &mut sql,
            &id,
            "fixture/value",
            version,
            &parent,
            archived,
            "json",
            &content.to_string(),
            1,
        );
    }
    insert(
        &mut sql,
        "history",
        "fixture/value",
        "1.0.0",
        "NULL",
        false,
        "json",
        "{\"title\":\"current\"}",
        3,
    );
    sql.push_str("INSERT INTO revisions SELECT object_id,1,contract_key,contract_version,encoding,'{\"title\":\"old\"}',content_hash,15,created_at,references_json FROM revisions WHERE object_id='history' AND revision=3;");
    insert(
        &mut sql,
        "text",
        "fixture/text",
        "1.0.0",
        "NULL",
        false,
        "text",
        "plain é",
        1,
    );
    insert(
        &mut sql,
        "binary",
        "fixture/binary",
        "1.0.0",
        "NULL",
        false,
        "base64",
        "00ff10",
        1,
    );
    if large {
        for i in 0..7 {
            let size = if i == 6 { 1_045_000 } else { 600_000 };
            insert(
                &mut sql,
                &format!("big{i}"),
                "fixture/large",
                "1.0.0",
                "NULL",
                false,
                "json",
                &json!({"text":"🙂".repeat(size/4)}).to_string(),
                1,
            );
        }
    }
    sql.push_str("COMMIT;");
    sql
}
fn insert(
    sql: &mut String,
    id: &str,
    key: &str,
    version: &str,
    parent: &str,
    archived: bool,
    encoding: &str,
    content: &str,
    revision: u64,
) {
    let path = if parent == "'id01'" {
        format!("/id01/{id}")
    } else {
        format!("/{id}")
    };
    sql.push_str(&format!("INSERT INTO objects VALUES(NULL,{id},{parent},NULL,{id},{path},0,NULL,{depth},{key},{revision},{archived_at},{archive},'2026-01-01','2026-01-02'); INSERT INTO revisions VALUES({id},{revision},{key},{version},{encoding},{content},'sha256:fixture',{bytes},'2026-01-02','{{}}');",id=quote(id),path=quote(&path),depth=if parent=="NULL"{1}else{2},key=quote(key),archived_at=if archived{"'2026-01-01'"}else{"NULL"},archive=i32::from(archived),version=quote(version),encoding=quote(encoding),content=if encoding=="base64"{format!("X'{content}'")}else{quote(content)},bytes=if encoding=="base64"{content.len()/2}else{content.len()}));
}

#[test]
fn signed_cursor_bytes_errors_scope_and_restart_match_original_without_added_expiry() {
    let root = Root::new();
    let vectors = fixture(json!({"path":root.0.join("vectors.sqlite"),"mode":"vectors"}));
    assert!(vectors.as_array().unwrap().len() > 180);
    compare(&root, vectors.as_array().unwrap(), false);
}

#[test]
fn registry_catalog_pages_keep_utf8_keysets_and_actual_response_byte_limits() {
    let root = Root::new();
    let identity = json!({"method":"catalog","surface":"public"});
    let records = json!([{"key":"🙂"},{"key":"\u{e000}"},{"key":"abc"},{"key":"abc","duplicate":true},{"key":"é"},{"key":"z"}]);
    let mut steps =
        vec![json!({"op":"catalog","records":records,"identity":identity,"params":{"limit":2}})];
    steps.push(
        json!({"op":"catalog","records":records,"identity":identity,"params":{"limit":2},"from":0}),
    );
    steps.push(
        json!({"op":"catalog","records":records,"identity":identity,"params":{"limit":2},"from":1}),
    );
    steps.push(json!({"op":"catalog","records":records,"identity":identity,"params":{"limit":0}}));
    for boundary in [
        json!(null),
        json!(12),
        json!([]),
        json!({}),
        json!({"toString":1}),
    ] {
        let index = steps.len();
        steps.push(json!({"op":"cursor","identity":identity,"boundary":boundary}));
        steps.push(json!({"op":"catalog","records":records,"identity":identity,"params":{},"tokenFrom":index}));
    }
    steps.push(json!({"op":"catalog","recordsJson":"[{\"key\":\"\\ud800\"},{\"key\":\"�\"},{\"key\":\"🙂\"}]","identity":identity,"params":{}}));
    let big =
        json!([{"key":"a","data":"🙂".repeat(280_000)},{"key":"b","data":"é".repeat(550_000)}]);
    let index = steps.len();
    steps.push(json!({"op":"catalog","records":big,"identity":identity,"params":{"limit":50}}));
    steps.push(json!({"op":"catalog","records":big,"identity":identity,"params":{"limit":50},"from":index}));
    steps.push(json!({"op":"catalog","records":[{"key":"x","data":"x".repeat(2*1024*1024)}],"identity":identity,"params":{}}));
    let rows = compare(&root, &steps, false);
    assert_eq!(result(&rows, 0)["items"].as_array().unwrap().len(), 2);
    assert_eq!(result(&rows, 2)["items"][1]["key"], "🙂");
    assert_eq!(result(&rows, index)["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        rows[steps.len() - 1]["error"]["data"]["code"],
        "result_too_large"
    );
}

#[test]
fn typed_query_keysets_predicates_scalar_pointer_and_work_budget_match_original() {
    let root = Root::new();
    let mut steps = vec![sql(seed(false))];
    let fields = [
        "object.id",
        "object.parentId",
        "object.ownerObjectId",
        "object.name",
        "object.path",
        "object.position",
        "object.icon",
        "object.contractKey",
        "object.revision",
        "object.contractVersion",
        "object.archivedAt",
        "object.effectivelyArchived",
        "object.createdAt",
        "object.updatedAt",
        "object.hasContractChildren",
        "data:/score",
        "data:/flag",
        "data:/obj",
        "data:/missing",
        "data:/tags/0",
        "data:/tags/00",
        "data:/a~1b/~0key",
    ];
    steps.push(json!({"op":"query","params":{"contractKey":"fixture/value","select":fields,"includeArchived":true}}));
    for direction in ["asc", "desc"] {
        let params = json!({"contractKey":"fixture/value","select":["object.name","data:/score","data:/flag"],"orderBy":[{"field":"data:/score","direction":direction},{"field":"data:/flag","direction":"desc"}],"limit":7});
        let mut previous = None;
        for _ in 0..5 {
            let index = steps.len();
            let mut action = json!({"op":"query","params":params});
            if let Some(p) = previous {
                action["from"] = json!(p);
            }
            steps.push(action);
            previous = Some(index);
        }
    }
    for (op, value) in [
        ("eq", json!(0)),
        ("ne", json!(0)),
        ("gt", json!(1)),
        ("gte", json!(1)),
        ("lt", json!(2)),
        ("lte", json!(2)),
        ("eq", Value::Null),
        ("ne", Value::Null),
        ("eq", json!(false)),
        ("eq", json!("1")),
        ("in", json!([0, false, "1", null])),
    ] {
        steps.push(json!({"op":"query","params":{"contractKey":"fixture/value","where":{"op":op,"field":"data:/score","value":value},"select":["object.id","data:/score"]}}));
    }
    for predicate in [
        json!({"op":"isNull","field":"data:/score"}),
        json!({"op":"isNull","field":"data:/obj"}),
        json!({"op":"contains","field":"data:/title","value":"Ä"}),
        json!({"op":"not","arg":{"op":"eq","field":"data:/score","value":1}}),
        json!({"op":"and","args":[{"op":"gte","field":"data:/score","value":1},{"op":"or","args":[{"op":"eq","field":"data:/flag","value":false},{"op":"eq","field":"object.parentId","value":null}]}]}),
    ] {
        steps
            .push(json!({"op":"query","params":{"contractKey":"fixture/value","where":predicate}}));
    }
    for params in [
        json!({"contractKey":"missing"}),
        json!({"contractKey":"fixture/text","includeContent":true}),
        json!({"contractKey":"fixture/value","select":["object.unknown"]}),
        json!({"contractKey":"fixture/value","select":["data:/bad~2pointer"]}),
        json!({"contractKey":"fixture/value","select":["other"]}),
        json!({"contractKey":"fixture/value","contractVersions":["absent"]}),
        json!({"contractKey":"fixture/value","contractVersions":["1.0.0","2.0.0"],"where":{"op":"eq","field":"data:/score","value":1}}),
        json!({"contractKey":"fixture/value","contractVersions":["1.0.0"],"select":["data:/obj"]}),
        json!({"contractKey":"fixture/value","contractVersions":["1.0.0"],"select":["data:/tags/0","data:/obj/x"],"includeContent":true,"limit":2}),
    ] {
        steps.push(json!({"op":"query","params":params}));
    }
    let mut deep = json!({"op":"eq","field":"data:/score","value":1});
    for _ in 0..9 {
        deep = json!({"op":"not","arg":deep});
    }
    steps.push(json!({"op":"query","params":{"contractKey":"fixture/value","where":deep}}));
    steps.push(json!({"op":"query","params":{"contractKey":"fixture/value","where":{"op":"and","args":vec![json!({"op":"eq","field":"data:/score","value":1});64]}}}));
    for content in [
        json!({"a":null,"b":false,"c":0,"d":[],"e":{},"a/b":{"~k":"x"},"array":["x",null]})
            .to_string(),
        "null".into(),
        "not json".into(),
    ] {
        for pointer in [
            "/a",
            "/b",
            "/c",
            "/d",
            "/e",
            "/missing",
            "/a~1b/~0k",
            "/array/0",
            "/array/00",
            "/array/length",
        ] {
            steps.push(json!({"op":"scalar","sql":"SELECT ivy_type(?1,?2) AS type,ivy_value(?1,?2) AS value,ivy_is_null(?1,?2) AS is_null","bindings":[content,pointer]}));
        }
    }
    steps.push(json!({"op":"scalar","sql":"SELECT ivy_value('{}','/bad~2') AS value"}));
    for content in [
        "{\"s\":\"\\ud800\"}",
        "{\"s\":\"a\\u0000b\"}",
        "{\"s\":\"é🙂\"}",
    ] {
        steps.push(json!({"op":"scalar","sql":"SELECT ivy_type(?1,'/s') AS type,ivy_value(?1,'/s') AS value","bindings":[content]}));
    }
    let params = json!({"contractKey":"fixture/value","orderBy":[{"field":"data:/score","direction":"asc"}],"limit":2});
    let identity = json!({"method":"objects.query","contractKey":"fixture/value","orderBy":[{"field":"data:/score","direction":"asc"}],"limit":2});
    for boundary in [
        json!(null),
        json!(false),
        json!({"sort":[],"id":"id00"}),
        json!({"sort":[{"type":9,"value":null}],"id":12}),
        json!({"sort":[null],"id":"id00"}),
        json!({"sort":[{"type":9,"value":null}],"id":"id00"}),
    ] {
        let index = steps.len();
        steps.push(json!({"op":"cursor","identity":identity,"boundary":boundary}));
        steps.push(json!({"op":"query","params":params,"tokenFrom":index}));
    }
    let mismatch = steps.len();
    steps.push(json!({"op":"cursor","identity":{"method":"objects.query","contractKey":"fixture/value","limit":3},"boundary":{"sort":[],"id":"id00"}}));
    steps.push(json!({"op":"query","params":params,"tokenFrom":mismatch}));
    steps.push(json!({"op":"scalar","sql":"SELECT ivy_budget() AS budget","overdue":true}));
    steps.push(json!({"op":"scalar","sql":"SELECT ivy_budget() AS budget"}));
    steps.push(json!({"op":"reopen"}));
    steps.push(json!({"op":"query","params":{"contractKey":"fixture/value","limit":3}}));
    let rows = compare(&root, &steps, false);
    assert_eq!(result(&rows, 1)["items"].as_array().unwrap().len(), 41);
    assert_eq!(result(&rows, 2)["items"].as_array().unwrap().len(), 7);
    assert_eq!(
        rows[mismatch + 1]["error"]["data"]["code"],
        "invalid_cursor"
    );
    assert_eq!(
        result(&rows, steps.len() - 1)["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn query_byte_pages_and_immutable_json_text_binary_history_reads_match_original() {
    let root = Root::new();
    let mut steps = vec![sql(seed(true))];
    let params = json!({"contractKey":"fixture/large","where":{"op":"lt","field":"object.id","value":"big6"},"includeContent":true,"select":["data:/text"],"limit":50});
    let first = steps.len();
    steps.push(json!({"op":"query","params":params}));
    steps.push(json!({"op":"query","params":params,"from":first}));
    steps.push(json!({"op":"query","params":{"contractKey":"fixture/large","where":{"op":"eq","field":"object.id","value":"big6"},"includeContent":true,"select":["data:/text"]}}));
    for id in ["history", "text", "binary", "missing"] {
        steps.push(json!({"op":"read","params":{"objectId":id}}));
    }
    for revision in [1, 2, 3, 4] {
        steps.push(json!({"op":"read","params":{"objectId":"history","revision":revision}}));
    }
    steps.push(json!({"op":"stat","params":{"path":"/id01/id20"}}));
    steps.push(json!({"op":"read","params":{"path":"/text"}}));
    let rows = compare(&root, &steps, false);
    assert_eq!(result(&rows, first)["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        result(&rows, first + 1)["items"].as_array().unwrap().len(),
        1
    );
    assert_eq!(rows[first + 2]["error"]["data"]["code"], "result_too_large");
    assert_eq!(result(&rows, 6)["content"]["value"], "AP8Q");
}

#[test]
fn retained_format_four_copy_keeps_persisted_cursor_secret_and_readable_history() {
    let root = Root::new();
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/hive-current.sqlite.gz");
    let before = fs::read(&fixture_path).unwrap();
    let steps = fixture(
        json!({"path":root.0.join("reference.sqlite"),"mode":"retainedVectors","retained":true,"copyTo":[root.0.join("reference.sqlite"),root.0.join("native.sqlite")]}),
    );
    assert!(steps.as_array().unwrap().len() > 10);
    compare(&root, steps.as_array().unwrap(), true);
    assert_eq!(fs::read(fixture_path).unwrap(), before);
}
