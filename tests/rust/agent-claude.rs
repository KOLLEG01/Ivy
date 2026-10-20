use ivy_agent_manager::claude::store::{SessionStore, StoreError, ThreadList, ThreadSort};
use ivy_contracts::{
    IvyJson, JsString,
    json::{parse, stringify},
};
use ivy_store::rusqlite::{Connection, types::ValueRef};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-claude-store-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        assert!(path.is_absolute());
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let valid = fs::symlink_metadata(&self.0).is_ok_and(|m| !m.file_type().is_symlink())
            && fs::canonicalize(&self.0).is_ok_and(|actual| {
                actual.file_name() == self.0.file_name()
                    && actual.parent() == fs::canonicalize(std::env::temp_dir()).ok().as_deref()
                    && actual
                        .file_name()
                        .is_some_and(|s| s.to_string_lossy().starts_with("ivy-claude-store-"))
            });
        if valid {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}
fn v(source: &str) -> IvyJson {
    parse(source, usize::MAX).unwrap()
}
static NULL: IvyJson = IvyJson::Null;
fn field<'a>(row: &'a IvyJson, key: &str) -> &'a IvyJson {
    row.get(key).unwrap_or(&NULL)
}
fn js(value: &IvyJson) -> &JsString {
    value.as_js_string().unwrap()
}
fn number(value: &IvyJson, default: f64) -> f64 {
    match value {
        IvyJson::Number(n) => *n,
        IvyJson::String(s) => s.trim().to_utf8().unwrap().parse().unwrap_or(f64::NAN),
        _ => default,
    }
}
fn strings(value: &IvyJson) -> Vec<JsString> {
    match value {
        IvyJson::String(s) => {
            if s.0.is_empty() {
                vec![]
            } else {
                vec![s.clone()]
            }
        }
        IvyJson::Array(a) => a.iter().map(|s| js(s).clone()).collect(),
        _ => vec![],
    }
}
fn optional(value: &IvyJson) -> Option<&JsString> {
    value.as_js_string()
}
fn json_option(value: Option<IvyJson>) -> IvyJson {
    value.unwrap_or(IvyJson::Null)
}
fn execute(store: &SessionStore, method: &str, args: &[IvyJson]) -> Result<IvyJson, StoreError> {
    let a = |i| args.get(i).unwrap_or(&NULL);
    match method {
        "upsertThread" => {
            store.upsert_thread(a(0))?;
            Ok(IvyJson::Null)
        }
        "getThread" => Ok(json_option(store.get_thread(js(a(0)))?)),
        "listThreads" => {
            let options = a(0);
            Ok(IvyJson::Array(
                store.list_threads(&ThreadList {
                    archived: field(options, "archived") == &IvyJson::Bool(true),
                    limit: options
                        .get("limit")
                        .filter(|v| !v.is_null())
                        .map(|v| number(v, 50.0)),
                    cursor: optional(field(options, "cursor"))
                        .filter(|s| !s.0.is_empty())
                        .map(|_| number(field(options, "cursor"), 0.0)),
                    is_pinned: field(options, "isPinned").as_bool(),
                    cwd: strings(field(options, "cwd")),
                    include_ephemeral: field(options, "includeEphemeral") == &IvyJson::Bool(true),
                    parent_thread_id: optional(field(options, "parentThreadId")).cloned(),
                    ancestor_thread_id: optional(field(options, "ancestorThreadId")).cloned(),
                    source_kinds: strings(field(options, "sourceKinds")),
                    section_id: options.get("sectionId").map(|s| optional(s).cloned()),
                    sort_key: match field(options, "sortKey").as_str().ok().as_deref() {
                        Some("updated_at") => Some(ThreadSort::Updated),
                        Some("recency_at") => Some(ThreadSort::Recency),
                        Some("section_position") => Some(ThreadSort::SectionPosition),
                        _ => None,
                    },
                    ascending: field(options, "sortDirection") == &"asc".into(),
                })?,
            ))
        }
        "listSections" => Ok(IvyJson::Array(
            store.list_sections(number(a(0), 100.0), number(a(1), 0.0))?,
        )),
        "getSection" => Ok(json_option(store.get_section(js(a(0)))?)),
        "createSection" => store.create_section(js(a(0)), js(a(1)), a(2)),
        "updateSection" => store.update_section(js(a(0)), js(a(1)), a(2)),
        "deleteSection" => {
            store.delete_section(js(a(0)))?;
            Ok(IvyJson::Null)
        }
        "moveThreadToSection" => {
            store.move_thread_to_section(js(a(0)), optional(a(1)), optional(a(2)))
        }
        "updateThreadStatus" => {
            store.update_thread_status(js(a(0)), a(1))?;
            Ok(IvyJson::Null)
        }
        "updateThreadName" => {
            store.update_thread_name(js(a(0)), optional(a(1)))?;
            Ok(IvyJson::Null)
        }
        "updateClaudeSessionId" => {
            store.update_claude_session_id(js(a(0)), optional(a(1)))?;
            Ok(IvyJson::Null)
        }
        "updateCodexSessionId" => {
            store.update_codex_session_id(js(a(0)), optional(a(1)))?;
            Ok(IvyJson::Null)
        }
        "setArchived" => {
            store.set_archived(js(a(0)), a(1) == &IvyJson::Bool(true))?;
            Ok(IvyJson::Null)
        }
        "upsertTurn" => {
            store.upsert_turn(a(0))?;
            Ok(IvyJson::Null)
        }
        "getTurn" => Ok(json_option(store.get_turn(js(a(0)))?)),
        "listTurns" => Ok(IvyJson::Array(store.list_turns(js(a(0)))?)),
        "deleteRecentTurns" => Ok(IvyJson::Number(
            store.delete_recent_turns(js(a(0)), number(a(1), 0.0))? as f64,
        )),
        "appendItem" => Ok(json_option(store.append_item(js(a(0)), a(1))?)),
        "updateItem" | "updateItemAndMoveToEnd" => {
            let update = |mut item: IvyJson| {
                for (key, value) in a(2).as_object().unwrap() {
                    item.insert(key.clone(), value.clone());
                }
                Ok(item)
            };
            Ok(json_option(if method == "updateItem" {
                store.update_item(js(a(0)), js(a(1)), update)?
            } else {
                store.update_item_and_move_to_end(js(a(0)), js(a(1)), update)?
            }))
        }
        "completeTurn" => Ok(json_option(store.complete_turn(
            js(a(0)),
            js(a(1)),
            a(2),
        )?)),
        "updateTurnDiff" => Ok(json_option(store.update_turn_diff(js(a(0)), js(a(1)))?)),
        "recoverStaleInProgressTurns" => {
            Ok(IvyJson::Number(store.recover_stale_in_progress_turns(
                optional(a(0)).unwrap_or(&"server restarted before completing turn".into()),
            )? as f64))
        }
        _ => panic!("unexpected fixture method {method}"),
    }
}
fn same(actual: &IvyJson, expected: &IvyJson, label: &str) {
    // JSON.stringify converts nonfinite numbers to null before crossing the
    // test boundary. Both source and Rust retain f64 until that boundary.
    let normalized = parse(&stringify(actual, usize::MAX).unwrap(), usize::MAX).unwrap();
    fn sorted(value: &IvyJson) -> IvyJson {
        match value {
            IvyJson::Object(pairs) => {
                let mut next = pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), sorted(v)))
                    .collect::<Vec<_>>();
                next.sort_by(|a, b| a.0.cmp(&b.0));
                IvyJson::Object(next)
            }
            IvyJson::Array(items) => IvyJson::Array(items.iter().map(sorted).collect()),
            _ => value.clone(),
        }
    }
    assert_eq!(
        stringify(&sorted(&normalized), usize::MAX).unwrap(),
        stringify(&sorted(expected), usize::MAX).unwrap(),
        "{label}"
    );
}
fn query(db: &Connection, sql: &str) -> IvyJson {
    let mut statement = db.prepare(sql).unwrap();
    let names = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let mut rows = statement.query([]).unwrap();
    let mut output = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let values = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let value = match row.get_ref(i).unwrap() {
                    ValueRef::Null => IvyJson::Null,
                    ValueRef::Integer(n) => {
                        if (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&n) {
                            IvyJson::Number(n as f64)
                        } else {
                            IvyJson::object([("integer", n.to_string().into())])
                        }
                    }
                    ValueRef::Real(n) => IvyJson::Number(n),
                    ValueRef::Text(s) => String::from_utf8_lossy(s).into_owned().into(),
                    ValueRef::Blob(s) => IvyJson::object(
                        s.iter()
                            .enumerate()
                            .map(|(i, b)| (i.to_string(), IvyJson::Number(f64::from(*b)))),
                    ),
                };
                (name.clone(), value)
            })
            .collect::<Vec<_>>();
        output.push(IvyJson::object(values));
    }
    IvyJson::Array(output)
}
fn original_run(original: &Root, seed: &str, clock: i64, ops: &[IvyJson]) -> IvyJson {
    let input = IvyJson::object([
        ("root", original.0.to_str().unwrap().into()),
        ("seedSql", seed.into()),
        ("clock", IvyJson::Number(clock as f64)),
        ("ops", IvyJson::Array(ops.to_vec())),
    ]);
    let mut child = Command::new("node")
        .args(["tests/fixtures/agent-claude-oracle.mjs"])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn scenario(seed: &str, clock: i64, ops: Vec<IvyJson>) {
    scenario_database(seed, clock, ops, None);
}
fn scenario_database(
    seed: &str,
    initial_clock: i64,
    ops: Vec<IvyJson>,
    copied: Option<&[u8]>,
) -> Vec<u8> {
    let original = Root::new();
    let rust = Root::new();
    if let Some(bytes) = copied {
        fs::write(original.0.join("state.sqlite"), bytes).unwrap();
        fs::write(rust.0.join("state.sqlite"), bytes).unwrap();
    }
    let expected = original_run(&original, seed, initial_clock, &ops);
    let path = rust.0.join("state.sqlite");
    if !seed.is_empty() {
        let db = Connection::open(&path).unwrap();
        db.execute_batch(seed).unwrap();
    }
    let time = Arc::new(AtomicI64::new(initial_clock));
    let captured = time.clone();
    let clock: Arc<dyn Fn() -> f64 + Send + Sync> =
        Arc::new(move || ((captured.load(Ordering::SeqCst) as f64) / 1000.0).floor());
    let mut store = SessionStore::with_clock(&rust.0, &path, clock.clone()).unwrap();
    for (index, op) in ops.iter().enumerate() {
        let method = field(op, "method").as_str().unwrap();
        let args = field(op, "args").as_array().unwrap();
        let result = match method.as_str() {
            "clock" => {
                time.store(args[0].as_f64().unwrap() as i64, Ordering::SeqCst);
                Ok(IvyJson::Null)
            }
            "reopen" => {
                store.close().unwrap();
                store = SessionStore::with_clock(&rust.0, &path, clock.clone()).unwrap();
                Ok(IvyJson::Null)
            }
            "sql" => {
                let db = Connection::open(&path).unwrap();
                db.execute_batch(&args[0].as_str().unwrap()).unwrap();
                Ok(IvyJson::Null)
            }
            _ => execute(&store, &method, args),
        };
        let actual = match result {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(e) => IvyJson::object([(
                "error",
                IvyJson::object([
                    ("name", e.name.into()),
                    ("message", e.message.into()),
                    ("code", e.code.map(IvyJson::from).unwrap_or(IvyJson::Null)),
                ]),
            )]),
        };
        same(
            &actual,
            &field(&expected, "outputs").as_array().unwrap()[index],
            &format!("step {index} {method}"),
        );
    }
    store.close().unwrap();
    let db = Connection::open(&path).unwrap();
    for table in ["threads", "turns", "thread_sections"] {
        same(
            &query(&db, &format!("SELECT rowid,* FROM {table} ORDER BY id")),
            field(field(&expected, "raw"), table),
            table,
        );
    }
    same(
        &query(
            &db,
            "SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
        ),
        field(field(&expected, "raw"), "schema"),
        "schema SQL bytes",
    );
    drop(db);
    let bytes = fs::read(&path).unwrap();
    if let Some(file) = std::env::var_os("IVY_CLAUDE_PROOF") {
        use ivy_contracts::hash::digest;
        let proof=IvyJson::object([
            ("test",std::thread::current().name().unwrap_or("unnamed").into()),("operations",IvyJson::Number(ops.len() as f64)),
            ("sourceIdentity",field(&expected,"sourceIdentity").clone()),("originalSQLiteVersion",field(&expected,"sqliteVersion").clone()),("rustSQLiteVersion",ivy_store::rusqlite::version().into()),
            ("outputsSha256",digest(stringify(field(&expected,"outputs"),usize::MAX).unwrap().as_bytes()).into()),("rawSha256",digest(stringify(field(&expected,"raw"),usize::MAX).unwrap().as_bytes()).into()),
            ("originalDatabaseSha256",digest(&fs::read(original.0.join("state.sqlite")).unwrap()).into()),("rustDatabaseSha256",digest(&bytes).into()),
            ("finding","All operation values/errors and final raw rows/schema SQL match the frozen pinned original".into()),
        ]);
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(file)
            .unwrap();
        writeln!(file, "{}", stringify(&proof, usize::MAX).unwrap()).unwrap();
    }
    bytes
}
fn op(method: &str, args: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([("method", method.into()), ("args", IvyJson::Array(args))])
}
fn thread(id: &str, created: f64) -> IvyJson {
    let mut value = v(
        r#"{"sessionId":"local-session","forkedFromId":null,"isPinned":false,"preview":"hello 😀","name":null,"archived":false,"cwd":"C:/fixture/work","model":"claude-test","reasoningEffort":null,"modelProvider":"anthropic","claudeSessionId":null,"source":"appServer","status":{"type":"idle"},"approvalPolicy":null,"sandboxMode":null,"permissionProfileId":null,"ephemeral":false,"threadSource":"user","agentRole":null,"agentNickname":null,"baseInstructions":null,"developerInstructions":null,"personality":null,"runtimeBackend":"claude","codexSessionId":null}"#,
    );
    value.insert("id", id.into());
    value.insert("createdAt", IvyJson::Number(created));
    value.insert("updatedAt", IvyJson::Number(created));
    value
}
fn turn(id: &str, thread: &str, started: IvyJson, items: IvyJson) -> IvyJson {
    IvyJson::object([
        ("id", id.into()),
        ("threadId", thread.into()),
        ("status", "inProgress".into()),
        ("startedAt", started),
        ("completedAt", IvyJson::Null),
        ("durationMs", IvyJson::Null),
        ("items", items),
        ("diff", "".into()),
        ("error", IvyJson::Null),
    ])
}

#[test]
fn sqlite_sessions_sections_and_restart_match_pinned_store() {
    let mut a = thread("a", 100.0);
    a.insert("isPinned", true.into());
    let b = thread("b", 100.0);
    let c = thread("c", 200.0);
    let mut replacement = a.clone();
    replacement.insert("createdAt", IvyJson::Number(999.0));
    replacement.insert("updatedAt", IvyJson::Number(300.0));
    replacement.insert("collaborationMode", "plan".into());
    replacement.insert("runtimeBackend", "codex".into());
    scenario(
        "",
        1_000_000,
        vec![
            op("listSections", vec![]),
            op("upsertThread", vec![a]),
            op("upsertThread", vec![b]),
            op("upsertThread", vec![c]),
            op("getThread", vec!["a".into()]),
            op("upsertThread", vec![replacement]),
            op("getThread", vec!["a".into()]),
            op("listThreads", vec![]),
            op(
                "listThreads",
                vec![v(r#"{"sortDirection":"asc","limit":2}"#)],
            ),
            op(
                "createSection",
                vec![
                    "s".into(),
                    " \u{feff}Section 😀 \n".into(),
                    v(r#"{"color":"blue","icon":"star","extra":1}"#),
                ],
            ),
            op("moveThreadToSection", vec!["a".into(), "s".into()]),
            op("clock", vec![IvyJson::Number(1_005_999.0)]),
            op(
                "moveThreadToSection",
                vec!["b".into(), "s".into(), "a".into()],
            ),
            op(
                "moveThreadToSection",
                vec!["c".into(), "s".into(), "missing".into()],
            ),
            op(
                "moveThreadToSection",
                vec!["a".into(), "s".into(), "b".into()],
            ),
            op(
                "listThreads",
                vec![v(
                    r#"{"sectionId":"s","sortKey":"section_position","sortDirection":"asc"}"#,
                )],
            ),
            op(
                "updateSection",
                vec![
                    "01984de2-8f74-7c91-a3b2-5c5e937cf318".into(),
                    "renamed".into(),
                    v(r#"{"icon":null,"color":4}"#),
                ],
            ),
            op(
                "updateSection",
                vec!["s".into(), " New ".into(), IvyJson::Null],
            ),
            op("getSection", vec!["s".into()]),
            op("listSections", vec![IvyJson::Number(1.0), "1".into()]),
            op("moveThreadToSection", vec!["a".into(), IvyJson::Null]),
            op("setArchived", vec!["b".into(), true.into()]),
            op("listThreads", vec![v(r#"{"archived":true}"#)]),
            op("deleteSection", vec!["s".into()]),
            op("reopen", vec![]),
            op("getThread", vec!["b".into()]),
            op("getThread", vec!["a".into()]),
            op("updateThreadName", vec!["a".into(), "renamed".into()]),
            op(
                "updateClaudeSessionId",
                vec!["a".into(), "claude-session".into()],
            ),
            op(
                "updateCodexSessionId",
                vec!["a".into(), "codex-session".into()],
            ),
            op(
                "updateThreadStatus",
                vec![
                    "a".into(),
                    v(r#"{"type":"active","activeFlags":["waitingOnApproval"]}"#),
                ],
            ),
            op("getThread", vec!["a".into()]),
            op("createSection", vec!["x".into(), " \t ".into()]),
            op("deleteSection", vec!["missing".into()]),
            op(
                "deleteSection",
                vec!["01984de2-8f74-7c91-a3b2-5c5e937cf318".into()],
            ),
            op("moveThreadToSection", vec!["missing".into(), IvyJson::Null]),
            op("moveThreadToSection", vec!["a".into(), "missing".into()]),
        ],
    );
}

#[test]
fn sqlite_topology_filters_cursor_ties_and_bounds_match_pinned_store() {
    let root = thread("root", 100.0);
    let mut child = thread("child", 110.0);
    child.insert("forkedFromId", "root".into());
    child.insert("threadSource", "subagent".into());
    child.insert("ephemeral", true.into());
    let mut grand = thread("grand", 120.0);
    grand.insert("forkedFromId", "child".into());
    grand.insert("threadSource", "memory_consolidation".into());
    grand.insert("ephemeral", true.into());
    let mut orphan = thread("orphan", 100.0);
    orphan.insert("threadSource", "subagent".into());
    orphan.insert("source", "cli".into());
    orphan.insert("cwd", "/unicode/😀".into());
    let mut archived = child.clone();
    archived.insert("id", "archived".into());
    archived.insert("archived", true.into());
    let mut unknown = thread("unknown", 120.0);
    unknown.insert("source", "unknown".into());
    unknown.insert("collaborationMode", "other".into());
    unknown.insert("runtimeBackend", "other".into());
    let mut ops = vec![];
    for value in [root, child, grand, orphan, archived, unknown] {
        ops.push(op("upsertThread", vec![value]));
    }
    for options in [
        r#"{}"#,
        r#"{"includeEphemeral":true}"#,
        r#"{"parentThreadId":"root"}"#,
        r#"{"ancestorThreadId":"root"}"#,
        r#"{"parentThreadId":"root","ancestorThreadId":"root"}"#,
        r#"{"ancestorThreadId":"missing"}"#,
        r#"{"sourceKinds":["subAgent"]}"#,
        r#"{"sourceKinds":["subAgentThreadSpawn"]}"#,
        r#"{"sourceKinds":["subAgentOther","subAgentCompact","subAgentReview"]}"#,
        r#"{"sourceKinds":["user","subAgentReview"]}"#,
        r#"{"sourceKinds":["memoryConsolidation"],"includeEphemeral":true}"#,
        r#"{"sourceKinds":["appServer","cli","exec","vscode","unknown"]}"#,
        r#"{"sourceKinds":["garbage"]}"#,
        r#"{"cwd":["C:/fixture/work","/unicode/😀"],"sortDirection":"asc"}"#,
        r#"{"cwd":"/unicode/😀"}"#,
        r#"{"cwd":[]}"#,
        r#"{"sectionId":null}"#,
        r#"{"limit":0}"#,
        r#"{"limit":-2}"#,
        r#"{"limit":400}"#,
        r#"{"limit":1.5}"#,
        r#"{"cursor":"100","sortDirection":"asc"}"#,
        r#"{"cursor":"120"}"#,
        r#"{"cursor":"not-a-number"}"#,
        r#"{"sortKey":"recency_at"}"#,
        r#"{"sortKey":"updated_at","sortDirection":"asc"}"#,
        r#"{"archived":true,"ancestorThreadId":"root"}"#,
    ] {
        ops.push(op("listThreads", vec![v(options)]));
    }
    ops.push(op("getThread", vec!["unknown".into()]));
    scenario("", 1_000_000, ops);
}

#[test]
fn sqlite_turn_item_updates_completion_and_deletion_match_pinned_store() {
    let first = turn(
        "first",
        "a",
        IvyJson::Number(500.0),
        v(
            r#"[{"id":"same","type":"agentMessage","text":"first"},{"id":"same","type":"agentMessage","text":"second"},{"id":"tail","type":"plan","text":"tail"}]"#,
        ),
    );
    let mut replacement = first.clone();
    replacement.insert("threadId", "b".into());
    replacement.insert("items", v("[]"));
    replacement.insert("apiDurationMs", IvyJson::Number(99.0));
    let mut missing = thread("missing-field", 300.0);
    missing.remove("forkedFromId");
    scenario(
        "",
        1_000_000,
        vec![
            op("upsertThread", vec![thread("a", 100.0)]),
            op("upsertThread", vec![thread("b", 100.0)]),
            op("upsertTurn", vec![first]),
            op(
                "appendItem",
                vec![
                    "first".into(),
                    v(
                        r#"{"id":"new","type":"agentMessage","text":"\ud800 \udfff 😀","nested":{"values":[1,null,false]}}"#,
                    ),
                ],
            ),
            op(
                "updateItem",
                vec![
                    "first".into(),
                    "same".into(),
                    v(r#"{"text":"both changed","extra":{"key":"value"}}"#),
                ],
            ),
            op(
                "updateItemAndMoveToEnd",
                vec![
                    "first".into(),
                    "same".into(),
                    v(r#"{"text":"first match moved"}"#),
                ],
            ),
            op("getTurn", vec!["first".into()]),
            op(
                "updateTurnDiff",
                vec!["first".into(), "diff 😀\n+line".into()],
            ),
            op("clock", vec![IvyJson::Number(400_000.0)]),
            op(
                "completeTurn",
                vec![
                    "first".into(),
                    "failed".into(),
                    v(r#"{"message":"error","nested":{"2":2,"1":1}}"#),
                ],
            ),
            op("getTurn", vec!["first".into()]),
            op("upsertTurn", vec![replacement]),
            op("getTurn", vec!["first".into()]),
            op("listTurns", vec!["a".into()]),
            op("listTurns", vec!["b".into()]),
            op(
                "upsertTurn",
                vec![turn("second", "a", IvyJson::Null, v("[]"))],
            ),
            op("completeTurn", vec!["second".into(), "completed".into()]),
            op(
                "upsertTurn",
                vec![turn("third", "a", IvyJson::Number(600.0), v("[]"))],
            ),
            op(
                "upsertTurn",
                vec![turn("foreign", "absent", IvyJson::Number(2.0), v("[]"))],
            ),
            op("upsertThread", vec![missing]),
            op("getThread", vec!["missing-field".into()]),
            op(
                "sql",
                vec![
                    r#"UPDATE turns SET items_json = '[ { "id": "x" } ]' WHERE id = 'third'"#
                        .into(),
                ],
            ),
            op(
                "updateItemAndMoveToEnd",
                vec!["third".into(), "absent".into(), v(r#"{"text":"unused"}"#)],
            ),
            op(
                "updateItem",
                vec!["third".into(), "absent".into(), v(r#"{"text":"unused"}"#)],
            ),
            op("appendItem", vec!["absent".into(), v("{}")]),
            op("updateItem", vec!["absent".into(), "x".into(), v("{}")]),
            op(
                "updateItemAndMoveToEnd",
                vec!["absent".into(), "x".into(), v("{}")],
            ),
            op("completeTurn", vec!["absent".into(), "completed".into()]),
            op("updateTurnDiff", vec!["absent".into(), "diff".into()]),
            op("deleteRecentTurns", vec!["a".into(), IvyJson::Number(0.0)]),
            op("deleteRecentTurns", vec!["a".into(), IvyJson::Number(-1.0)]),
            op("deleteRecentTurns", vec!["a".into(), IvyJson::Number(1.5)]),
            op("listTurns", vec!["a".into()]),
            op("deleteRecentTurns", vec!["a".into(), IvyJson::Number(2.0)]),
            op("listTurns", vec!["a".into()]),
            op("reopen", vec![]),
            op("getTurn", vec!["second".into()]),
        ],
    );
}

#[test]
fn sqlite_restart_reconciles_stale_items_and_preserves_partial_effects() {
    let mut a = thread("a", 100.0);
    a.insert(
        "status",
        v(r#"{"type":"active","activeFlags":["waitingOnApproval"]}"#),
    );
    let mut b = thread("b", 100.0);
    b.insert("status", v(r#"{"type":"active"}"#));
    let mut c = thread("c", 100.0);
    c.insert("status", v(r#"{"type":"active"}"#));
    let items = v(
        r#"[null,1,false,"raw",[],{"type":"subAgentActivity","kind":"started","status":"inProgress"},{"type":"subAgentActivity","kind":"interacted"},{"type":"subAgentActivity","kind":"completed"},{"type":"subAgentActivity","kind":"other","status":"inProgress"},{"id":"call","type":"collabAgentToolCall","status":"inProgress","agentsStates":{"2":{"status":"running","message":null,"extra":true},"1":{"status":"pendingInit","message":"keep"},"done":{"status":"completed"},"invalid":7,"array":[],"null":null}},{"id":"no-states","type":"collabAgentToolCall"},{"id":"null-states","type":"collabAgentToolCall","agentsStates":null},{"id":"array-states","type":"collabAgentToolCall","agentsStates":[]},{"type":"commandExecution","status":"inProgress"}]"#,
    );
    let mut terminal = turn("completed", "b", IvyJson::Number(100.0), items.clone());
    terminal.insert("status", "completed".into());
    let mut failed = turn("failed", "b", IvyJson::Number(100.0), items.clone());
    failed.insert("status", "failed".into());
    scenario("",1_000_000,vec![
        op("upsertThread",vec![a]),op("upsertThread",vec![b]),op("upsertThread",vec![c]),op("upsertTurn",vec![turn("stale","a",IvyJson::Number(300.0),items)]),
        op("upsertTurn",vec![terminal]),op("upsertTurn",vec![failed]),op("upsertTurn",vec![turn("bad-json","a",IvyJson::Null,v("[]"))]),
        op("sql",vec!["UPDATE turns SET items_json='{' WHERE id='bad-json'".into()]),op("reopen",vec![]),op("recoverStaleInProgressTurns",vec!["restart 😀".into()]),
        op("listTurns",vec!["a".into()]),op("listTurns",vec!["b".into()]),op("getThread",vec!["a".into()]),op("getThread",vec!["b".into()]),op("getThread",vec!["c".into()]),
        op("recoverStaleInProgressTurns",vec![]),
        op("sql",vec!["UPDATE turns SET items_json='{}' WHERE id='failed'; UPDATE turns SET items_json=' [ null, 1 ] ' WHERE id='completed'".into()]),op("recoverStaleInProgressTurns",vec![]),op("listTurns",vec!["b".into()]),op("recoverStaleInProgressTurns",vec![]),
    ]);
}

#[test]
fn sqlite_section_transactions_roll_back_but_turn_deletion_does_not() {
    scenario("",1_000_000,vec![
        op("upsertThread",vec![thread("a",100.0)]),op("upsertThread",vec![thread("b",100.0)]),op("upsertThread",vec![thread("c",100.0)]),op("createSection",vec!["s".into(),"Section".into()]),
        op("moveThreadToSection",vec!["a".into(),"s".into()]),op("moveThreadToSection",vec!["b".into(),"s".into()]),op("moveThreadToSection",vec!["c".into(),"s".into()]),
        op("sql",vec!["CREATE TRIGGER reject_position BEFORE UPDATE OF section_position ON threads WHEN NEW.id='b' BEGIN SELECT RAISE(ABORT,'position denied'); END".into()]),
        op("moveThreadToSection",vec!["c".into(),"s".into(),"a".into()]),op("listThreads",vec![v(r#"{"sectionId":"s","sortKey":"section_position","sortDirection":"asc"}"#)]),
        op("sql",vec!["DROP TRIGGER reject_position; CREATE TRIGGER reject_delete BEFORE DELETE ON thread_sections BEGIN SELECT RAISE(ABORT,'delete denied'); END".into()]),op("deleteSection",vec!["s".into()]),op("getThread",vec!["a".into()]),op("getThread",vec!["b".into()]),op("getSection",vec!["s".into()]),op("sql",vec!["DROP TRIGGER reject_delete".into()]),
        op("upsertTurn",vec![turn("older","a",IvyJson::Number(100.0),v("[]"))]),op("upsertTurn",vec![turn("newer","a",IvyJson::Number(200.0),v("[]"))]),
        op("sql",vec!["CREATE TRIGGER reject_turn_delete BEFORE DELETE ON turns WHEN OLD.id='older' BEGIN SELECT RAISE(ABORT,'older denied'); END".into()]),op("deleteRecentTurns",vec!["a".into(),IvyJson::Number(2.0)]),op("listTurns",vec!["a".into()]),op("sql",vec!["DROP TRIGGER reject_turn_delete".into()]),
    ]);
}

#[test]
fn sqlite_legacy_migrations_keep_pin_positions_enum_cleanup_and_appearance() {
    let legacy = r#"
CREATE TABLE threads (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL, forked_from_id TEXT,
 is_pinned INTEGER NOT NULL DEFAULT 0, preview TEXT NOT NULL, name TEXT,
 archived INTEGER NOT NULL DEFAULT 0, cwd TEXT NOT NULL, model TEXT NOT NULL,
 model_provider TEXT NOT NULL, claude_session_id TEXT, source TEXT NOT NULL,
 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, status_json TEXT NOT NULL,
 thread_source TEXT, agent_role TEXT, agent_nickname TEXT, base_instructions TEXT,
 developer_instructions TEXT
);
INSERT INTO threads VALUES('old','session',NULL,1,'legacy',NULL,0,'/fixture','model','anthropic',NULL,'app_server',100,200,'{"type":"idle"}','wrong','','','','');
INSERT INTO threads VALUES('plain','session',NULL,0,'legacy',NULL,0,'/fixture','model','anthropic',NULL,'cli',100,200,'{"type":"idle"}','subagent','role','name','base','developer');
"#;
    scenario(legacy,1_000_000,vec![op("getThread",vec!["old".into()]),op("getThread",vec!["plain".into()]),op("listSections",vec![]),op("reopen",vec![]),op("getThread",vec!["old".into()]),
        op("createSection",vec!["malformed".into(),"Legacy".into()]),op("sql",vec!["UPDATE thread_sections SET appearance_json='{' WHERE id='malformed'".into()]),op("getSection",vec!["malformed".into()]),
        op("sql",vec!["UPDATE thread_sections SET appearance_json='[]' WHERE id='malformed'".into()]),op("getSection",vec!["malformed".into()]),
        op("sql",vec![r#"UPDATE thread_sections SET appearance_json='{"color":5,"icon":"star","extra":true}' WHERE id='malformed'"#.into()]),op("getSection",vec!["malformed".into()]),
        op("sql",vec!["DELETE FROM thread_sections WHERE id='01984de2-8f74-7c91-a3b2-5c5e937cf318'".into()]),op("getSection",vec!["01984de2-8f74-7c91-a3b2-5c5e937cf318".into()]),op("reopen",vec![]),op("listSections",vec![]),
    ]);
    // A failed cleanup batch stops at that statement, as DatabaseSync.exec did;
    // later enum fixes must not be silently applied in a separate transaction.
    let interrupted = format!(
        "{legacy}\nCREATE TRIGGER reject_cleanup BEFORE UPDATE OF thread_source ON threads BEGIN SELECT RAISE(ABORT,'cleanup denied'); END;"
    );
    scenario(
        &interrupted,
        1_000_000,
        vec![
            op("getThread", vec!["old".into()]),
            op("getThread", vec!["plain".into()]),
            op("reopen", vec![]),
            op("getThread", vec!["old".into()]),
        ],
    );
}

#[test]
fn sqlite_numeric_boundaries_utf16_and_json_fields_match_pinned_store() {
    let mut text = thread("utf16", 100.0);
    text.insert("preview", v(r#""\ud800 text \udfff""#));
    text.insert("status", v(r#"{"type":"idle","value":"\ud800"}"#));
    let mut ops = vec![
        op("upsertThread", vec![text]),
        op("getThread", vec!["utf16".into()]),
        op(
            "upsertTurn",
            vec![turn("t", "utf16", IvyJson::Number(100.0), v("[]"))],
        ),
    ];
    for scalar in [
        "0x20000000000003",
        "0x20000000000003001",
        "0b100000000000000000000000000000000000000000000000000011",
        "0o400000000000000003",
        "  .5 ",
        "-0",
        "Infinity",
        "not-a-number",
        "1e309",
        "\u{feff}25\u{feff}",
    ] {
        ops.push(op(
            "sql",
            vec![format!("UPDATE turns SET started_at='{scalar}' WHERE id='t'").into()],
        ));
        ops.push(op("getTurn", vec!["t".into()]));
        ops.push(op("completeTurn", vec!["t".into(), "completed".into()]));
    }
    ops.extend([
        op(
            "sql",
            vec!["UPDATE threads SET section_position=9007199254740992 WHERE id='utf16'".into()],
        ),
        op("getThread", vec!["utf16".into()]),
        op("listThreads", vec![]),
        op(
            "sql",
            vec!["UPDATE threads SET section_position=-9007199254740992 WHERE id='utf16'".into()],
        ),
        op("getThread", vec!["utf16".into()]),
        op(
            "sql",
            vec!["UPDATE threads SET section_position=9007199254740991 WHERE id='utf16'".into()],
        ),
        op("getThread", vec!["utf16".into()]),
    ]);
    scenario("", 1_000_000, ops);
}

#[test]
fn sqlite_original_database_round_trips_through_rust_and_original_again() {
    let original = Root::new();
    let initial = original_run(
        &original,
        "",
        1_000_000,
        &[
            op("upsertThread", vec![thread("copied", 100.0)]),
            op(
                "upsertTurn",
                vec![turn(
                    "t",
                    "copied",
                    IvyJson::Number(200.0),
                    v(r#"[{"id":"a","type":"agentMessage","text":"original"}]"#),
                )],
            ),
            op(
                "updateClaudeSessionId",
                vec!["copied".into(), "original-sdk-session".into()],
            ),
        ],
    );
    assert!(
        field(&initial, "outputs")
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.get("error").is_none())
    );
    let original_bytes = fs::read(original.0.join("state.sqlite")).unwrap();
    let rust_bytes = scenario_database(
        "",
        1_005_000,
        vec![
            op("getThread", vec!["copied".into()]),
            op("listTurns", vec!["copied".into()]),
            op(
                "appendItem",
                vec![
                    "t".into(),
                    v(r#"{"id":"b","type":"agentMessage","text":"Rust write"}"#),
                ],
            ),
            op("completeTurn", vec!["t".into(), "completed".into()]),
            op(
                "updateThreadStatus",
                vec!["copied".into(), v(r#"{"type":"idle"}"#)],
            ),
            op(
                "updateThreadName",
                vec!["copied".into(), "Rust name".into()],
            ),
        ],
        Some(&original_bytes),
    );
    let reopened = Root::new();
    fs::write(reopened.0.join("state.sqlite"), rust_bytes).unwrap();
    let readback = original_run(
        &reopened,
        "",
        1_010_000,
        &[
            op("getThread", vec!["copied".into()]),
            op("getTurn", vec!["t".into()]),
            op("recoverStaleInProgressTurns", vec![]),
        ],
    );
    let values = field(&readback, "outputs").as_array().unwrap();
    assert_eq!(
        field(field(&values[0], "value"), "name"),
        &"Rust name".into()
    );
    assert_eq!(
        field(field(&values[0], "value"), "claudeSessionId"),
        &"original-sdk-session".into()
    );
    assert_eq!(
        field(field(&values[1], "value"), "status"),
        &"completed".into()
    );
    assert_eq!(
        field(field(&values[1], "value"), "items")
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(field(&values[2], "value"), &IvyJson::Number(0.0));
}

#[test]
fn sqlite_corrupt_json_reports_typed_failure_and_preserves_invalid_bytes() {
    let original = Root::new();
    let expected = original_run(
        &original,
        "",
        1_000_000,
        &[
            op("upsertThread", vec![thread("a", 100.0)]),
            op(
                "upsertTurn",
                vec![turn("t", "a", IvyJson::Number(200.0), v("[]"))],
            ),
            op(
                "sql",
                vec![
                    "UPDATE threads SET status_json='{'; UPDATE turns SET items_json='[invalid]'"
                        .into(),
                ],
            ),
            op("getThread", vec!["a".into()]),
            op("getTurn", vec!["t".into()]),
        ],
    );
    let rust = Root::new();
    let path = rust.0.join("state.sqlite");
    fs::copy(original.0.join("state.sqlite"), &path).unwrap();
    let store = SessionStore::with_clock(&rust.0, &path, Arc::new(|| 1000.0)).unwrap();
    let actual = [
        store.get_thread(&"a".into()).unwrap_err(),
        store.get_turn(&"t".into()).unwrap_err(),
    ];
    let mut findings = Vec::new();
    for (index, error) in actual.iter().enumerate() {
        let source = field(
            &field(&expected, "outputs").as_array().unwrap()[index + 3],
            "error",
        );
        assert_eq!(error.name, "SyntaxError");
        assert_eq!(&IvyJson::from(error.name), field(source, "name"));
        findings.push(IvyJson::object([
            ("original", source.clone()),
            (
                "rust",
                IvyJson::object([
                    ("name", error.name.into()),
                    ("message", error.message.clone().into()),
                ]),
            ),
            (
                "messageParity",
                (field(source, "message") == &IvyJson::from(error.message.clone())).into(),
            ),
        ]));
    }
    store.close().unwrap();
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        query(&db, "SELECT status_json FROM threads"),
        v(r#"[{"status_json":"{"}]"#)
    );
    assert_eq!(
        query(&db, "SELECT items_json FROM turns"),
        v(r#"[{"items_json":"[invalid]"}]"#)
    );
    if let Some(file) = std::env::var_os("IVY_CLAUDE_PROOF") {
        let proof=IvyJson::object([("test","sqlite_corrupt_json_reports_typed_failure_and_preserves_invalid_bytes".into()),("qualification","Typed failure and stored invalid bytes only; exact parser diagnostics are measured separately".into()),("findings",IvyJson::Array(findings))]);
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(file)
            .unwrap();
        writeln!(file, "{}", stringify(&proof, usize::MAX).unwrap()).unwrap();
    }
}

#[test]
fn sqlite_invalid_item_containers_keep_original_type_errors() {
    let mut ops = vec![
        op("upsertThread", vec![thread("a", 100.0)]),
        op(
            "upsertTurn",
            vec![turn("t", "a", IvyJson::Number(100.0), v("[]"))],
        ),
    ];
    for json in ["null", "{}", "1", r#""text""#, "[null]"] {
        ops.push(op(
            "sql",
            vec![format!("UPDATE turns SET items_json='{json}' WHERE id='t'").into()],
        ));
        ops.extend([
            op("appendItem", vec!["t".into(), v(r#"{"id":"next"}"#)]),
            op("updateItem", vec!["t".into(), "id".into(), v("{}")]),
            op(
                "updateItemAndMoveToEnd",
                vec!["t".into(), "id".into(), v("{}")],
            ),
        ]);
    }
    scenario("", 1_000_000, ops);
}
