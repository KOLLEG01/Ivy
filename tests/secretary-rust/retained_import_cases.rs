use super::*;
use base64::Engine;
use ivy_contracts::{JsString, checked_native_contract::CheckedNativeContracts, json::canonical};
use ivy_sdk::service::{ConnectionOptions, ServiceConnection};
use ivy_secretary::{
    import_state,
    journal::{Journal, Limits},
    retained_import::{ImportContext, step},
};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Attempt {
    state: IvyJson,
    current: bool,
    error: Option<String>,
    stages: Vec<IvyJson>,
    snapshot: IvyJson,
    receipts: IvyJson,
}
fn empty() -> IvyJson {
    IvyJson::Object(vec![])
}
fn read_state(journal: &Journal, key: &str) -> Option<IvyJson> {
    journal
        .db
        .query_row("SELECT value FROM journal_state WHERE key=?", [key], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .map(|v| parse(&v, 32 * 1024 * 1024).unwrap())
}
async fn connect(address: &str) -> ServiceConnection {
    ServiceConnection::connect(
        address,
        "fixture",
        ConnectionOptions::new("fixture.secretary".into()),
        RequestOptions::default().cancellation,
    )
    .await
    .unwrap()
}
async fn request(client: &ServiceConnection, method: &str, params: IvyJson) -> IvyJson {
    client
        .request(method, params, RequestOptions::default())
        .await
        .unwrap()
}
fn retained(before: &IvyJson, after: &IvyJson) {
    for row in before.required("schema").unwrap().as_array().unwrap() {
        assert!(
            after
                .required("schema")
                .unwrap()
                .as_array()
                .unwrap()
                .contains(row),
            "Original schema was lost: {row:?}"
        );
    }
    for table in before.required("tables").unwrap().as_array().unwrap() {
        let next = after
            .required("tables")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get("name") == table.get("name"))
            .unwrap();
        assert_eq!(table.get("columns"), next.get("columns"));
        let rows = next.required("rows").unwrap().as_array().unwrap();
        for row in table.required("rows").unwrap().as_array().unwrap() {
            if table.get("name") == Some(&"technical_documents".into()) {
                let original = row.as_array().unwrap();
                let current = rows
                    .iter()
                    .find(|r| r.as_array().unwrap()[3] == original[3])
                    .unwrap()
                    .as_array()
                    .unwrap();
                assert_eq!(
                    &original[..8],
                    &current[..8],
                    "Original local ID/key/name changed"
                );
            } else {
                assert!(
                    rows.contains(row),
                    "Original raw row was lost from {:?}: {row:?}",
                    table.get("name")
                );
            }
        }
    }
}
async fn attempt(
    path: &Path,
    data: &IvyJson,
    client: &ServiceConnection,
    cancellation: RequestOptions,
    abort_marker: bool,
    pause_uncertain: bool,
) -> Attempt {
    let node: JsString = "fixture.secretary".into();
    let settings = data.required("settings").unwrap();
    let identity = import_state::import_identity(settings, &node).unwrap();
    let operation = "fixture:1800000000000:whole-import".into();
    let prepared = import_state::prepare(
        path,
        Limits::default(),
        &identity,
        &operation,
        data.required("at").unwrap().as_f64().unwrap() as i64,
    )
    .unwrap();
    let mut state = prepared.state.unwrap();
    let source = import_state::open_source(
        &path.parent().unwrap().join(
            state
                .required("source")
                .unwrap()
                .required("manifest")
                .unwrap()
                .required("filename")
                .unwrap()
                .as_str()
                .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(
        canonical(
            &super::execution_import_cases::snapshot(&source),
            32 * 1024 * 1024
        )
        .unwrap(),
        canonical(data.required("before").unwrap(), 32 * 1024 * 1024).unwrap(),
        "The sealed source changed across restart"
    );
    if abort_marker {
        prepared.journal.db.execute_batch("CREATE TRIGGER abort_import_marker BEFORE INSERT ON journal_state WHEN NEW.key='secretary/current-format' BEGIN SELECT RAISE(ABORT,'fixture_marker_abort'); END;").unwrap();
    }
    let contracts = CheckedNativeContracts::new(path.parent().unwrap().join("contracts"));
    let ctx = ImportContext {
        journal: &prepared.journal,
        source: &source,
        settings,
        service_node_id: &node,
        client: Arc::new(client.clone()),
        retained: &contracts,
        cancellation,
        page_size: 1,
    };
    let mut stages = vec![];
    let mut error = None;
    for _ in 0..100 {
        let result = step(&ctx, &mut state).await;
        match result {
            Ok(progress) => {
                stages.push(IvyJson::object([
                    ("stage", progress.stage.into()),
                    ("advanced", progress.advanced.into()),
                    ("complete", progress.complete.into()),
                ]));
                // Retire this test process owner after Store recovered an
                // applied write from its value, before receipt reconciliation.
                if pause_uncertain && prepared.journal.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM journal_state WHERE key LIKE 'secretary/import/write/%' AND json_type(value,'$.response')='null')",
                    [], |r|r.get::<_,bool>(0),
                ).unwrap(){error=Some("fixture_interrupted".into());break;}
                if progress.complete || !progress.advanced {
                    break;
                }
            }
            Err(e) => {
                error = Some(e.code);
                break;
            }
        }
    }
    let saved = read_state(&prepared.journal, "secretary/import/run").unwrap();
    let current = import_state::current(&prepared.journal, &identity).unwrap();
    if error.is_none() {
        assert!(
            current,
            "Fixture must either complete or surface an explicit resumable failure"
        );
    }
    assert_eq!(
        prepared
            .journal
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        37
    );
    assert_eq!(
        prepared
            .journal
            .db
            .query_row(
                "SELECT COUNT(*) FROM capture_acknowledgements WHERE capture_id='retained-capture'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        if saved.get("stage") == Some(&"follow-up".into()) {
            0
        } else {
            1
        }
    );
    let snapshot = super::execution_import_cases::snapshot(&prepared.journal.db);
    retained(data.required("before").unwrap(), &snapshot);
    let rows = prepared
        .journal
        .db
        .prepare(
            "SELECT key,value FROM journal_state WHERE key LIKE 'secretary/import/%' ORDER BY key",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(|r| {
            let (k, v) = r.unwrap();
            IvyJson::object([
                ("key", k.into()),
                ("value", parse(&v, 32 * 1024 * 1024).unwrap()),
            ])
        })
        .collect();
    if abort_marker {
        prepared
            .journal
            .db
            .execute_batch("DROP TRIGGER abort_import_marker")
            .unwrap();
    }
    Attempt {
        state: saved,
        current,
        error,
        stages,
        snapshot,
        receipts: IvyJson::Array(rows),
    }
}
fn writes(state: &IvyJson, method: &str) -> Vec<IvyJson> {
    state
        .required("trace")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r.get("method") == Some(&method.into()))
        .map(|r| r.required("params").unwrap().clone())
        .collect()
}
fn assert_no_effects(state: &IvyJson) {
    for row in state.required("trace").unwrap().as_array().unwrap() {
        let method = row.required("method").unwrap().as_str().unwrap();
        if method == "tools.call" {
            assert_eq!(
                row.required("params").unwrap().get("qualifiedName"),
                Some(&"agent.status".into()),
                "No provider/native dispatch is permitted during import"
            );
        }
        assert!(!method.starts_with("fixture."));
    }
}
fn complete_state(data: &IvyJson, path: &Path, hive: &IvyJson) {
    let records = hive.required("records").unwrap().as_array().unwrap();
    let execution = records
        .iter()
        .map(|r| r.as_array().unwrap())
        .find(|r| r[1].get("key") == Some(&"secretary/execution".into()))
        .unwrap();
    let versions = execution[1]
        .required("versions")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        versions.len(),
        3,
        "Combined delivery conversion must create exactly one Hive revision"
    );
    let value = versions.last().unwrap().required("value").unwrap();
    assert_eq!(value.get("delivery"), data.get("main"));
    assert_eq!(value.get("voice"), data.get("voice"));
    let node = "fixture.secretary".into();
    let identity =
        import_state::import_identity(data.required("settings").unwrap(), &node).unwrap();
    let journal = import_state::runtime_journal(path, Limits::default(), &identity).unwrap();
    assert!(import_state::current(&journal, &identity).unwrap());
    for q in data.required("questions").unwrap().as_array().unwrap() {
        let id = q.required("id").unwrap().as_str().unwrap();
        let question = journal
            .read("secretary/follow-up", &id, None, true)
            .unwrap();
        assert_eq!(question.value.get("contextObjectId"), q.get("contextId"));
        let context = journal
            .read(
                "secretary/follow-up-context",
                &q.required("contextId").unwrap().as_str().unwrap(),
                None,
                true,
            )
            .unwrap();
        if q.get("pending") == Some(&true.into()) {
            assert_eq!(
                context.value.required("start").unwrap().get("operationId"),
                q.get("startId")
            );
        }
    }
    let trace = hive.required("trace").unwrap().as_array().unwrap();
    let ack = trace
        .iter()
        .position(|r| r.get("method") == Some(&"events.ack".into()))
        .unwrap();
    let retire = trace
        .iter()
        .position(|r| r.get("method") == Some(&"events.unsubscribe".into()))
        .unwrap();
    assert!(
        ack < retire,
        "Current stable page must be persisted and ACKed before obsolete unsubscribe"
    );
    let active = writes(hive, "events.unsubscribe")
        .into_iter()
        .filter(|p| p.get("name") == Some(&"obsolete-active".into()))
        .collect::<Vec<_>>();
    assert!(!active.is_empty());
    assert!(
        active
            .iter()
            .all(|p| p.get("mutationId") == active[0].get("mutationId")),
        "Retries may only use the original obsolete unsubscribe ID"
    );
}
#[tokio::test]
async fn bounded_whole_import_reopens_original_core_and_reconciles_original_hive_ids() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let directory = Directory(
        std::env::temp_dir().join(format!("secretary-whole-import-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&directory.0).unwrap();
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/retained-import-fixture.mjs")
        .arg(directory.0.join("original"))
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut process = Process(command.spawn().unwrap());
    let mut line = String::new();
    BufReader::new(process.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let data = parse(&line, 32 * 1024 * 1024).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.required("coreBase64").unwrap().as_str().unwrap())
        .unwrap();
    let address = format!(
        "http://127.0.0.1:{}",
        data.required("port").unwrap().as_f64().unwrap() as u16
    );
    let observer = connect(&address).await;
    for name in [
        "complete",
        "unknown-before-write",
        "unknown-applied-write",
        "unknown-unsubscribe",
        "expired-write",
        "foreign-root",
        "foreign-owner",
        "cancelled",
        "orphan",
        "marker-abort",
    ] {
        let mut options = empty();
        match name {
            "unknown-before-write" => {
                options.insert("fault", "write".into());
                options.insert("applied", false.into());
            }
            "unknown-applied-write" | "expired-write" => {
                options.insert("fault", "write".into());
                options.insert("applied", true.into());
            }
            "unknown-unsubscribe" => {
                options.insert("fault", "unsubscribe".into());
                options.insert("applied", true.into());
            }
            "foreign-root" => {
                options.insert("foreignQuery", true.into());
            }
            "foreign-owner" => {
                options.insert("ownerChanged", true.into());
            }
            _ => {}
        }
        request(&observer, "fixture.reset", options).await;
        let folder = directory.0.join(name);
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("secretary.sqlite");
        std::fs::write(&path, &bytes).unwrap();
        if name == "orphan" {
            let db = ivy_store::rusqlite::Connection::open(&path).unwrap();
            db.execute("INSERT INTO technical_documents VALUES('orphan','secretary/schedule-cursor','unavailable-assignment',1,'{}')",[]).unwrap();
            db.execute(
                "INSERT INTO technical_revisions VALUES('orphan',1,'{}')",
                [],
            )
            .unwrap();
        }
        let mut case_data = data.clone();
        if name == "orphan" {
            let db = ivy_store::rusqlite::Connection::open(&path).unwrap();
            case_data.insert("before", super::execution_import_cases::snapshot(&db));
        }
        let cancellation = RequestOptions::default();
        if name == "cancelled" {
            cancellation.cancellation.cancel();
        }
        let client = connect(&address).await;
        let first = attempt(
            &path,
            &case_data,
            &client,
            cancellation,
            name == "marker-abort",
            matches!(name, "unknown-applied-write" | "expired-write"),
        )
        .await;
        let before = request(&observer, "fixture.state", empty()).await;
        assert_no_effects(&before);
        if name == "complete" {
            assert!(first.error.is_none(), "{name}: {:?}", first.error);
            assert!(first.current);
            complete_state(&data, &path, &before);
        } else {
            assert!(
                !first.current,
                "Refused/uncertain conversion cannot admit normal runtime: {name}"
            );
            assert!(
                first.error.is_some(),
                "Expected an explicit refusal: {name}"
            );
        }
        let mut result = IvyJson::object([
            ("case", name.into()),
            (
                "firstError",
                first.error.clone().map_or(IvyJson::Null, IvyJson::from),
            ),
            ("firstState", first.state),
            ("firstStages", IvyJson::Array(first.stages)),
            ("firstReceipts", first.receipts),
        ]);
        if matches!(name, "foreign-root" | "foreign-owner" | "cancelled") {
            assert!(writes(&before, "objects.write").is_empty());
            assert!(writes(&before, "events.unsubscribe").is_empty());
        }
        if name == "foreign-root" || name == "orphan" {
            assert_eq!(first.error.as_deref(), Some("secretary_import_incomplete"));
        }
        if matches!(
            name,
            "unknown-before-write"
                | "unknown-applied-write"
                | "unknown-unsubscribe"
                | "expired-write"
                | "marker-abort"
        ) {
            client.close();
            request(
                &observer,
                "fixture.resume",
                IvyJson::object([("expire", (name == "expired-write").into())]),
            )
            .await;
            let restarted = connect(&address).await;
            let second = attempt(
                &path,
                &case_data,
                &restarted,
                RequestOptions::default(),
                false,
                false,
            )
            .await;
            let after = request(&observer, "fixture.state", empty()).await;
            assert_no_effects(&after);
            if name == "expired-write" {
                assert_eq!(second.error.as_deref(), Some("operation_expired"));
                assert!(!second.current);
                let node = "fixture.secretary".into();
                let identity =
                    import_state::import_identity(data.required("settings").unwrap(), &node)
                        .unwrap();
                assert_eq!(
                    import_state::runtime_journal(&path, Limits::default(), &identity)
                        .err()
                        .unwrap()
                        .code,
                    "secretary_import_required"
                );
            } else {
                assert!(second.error.is_none(), "{name}: {:?}", second.error);
                assert!(second.current);
                complete_state(&data, &path, &after);
            }
            let method = if name == "unknown-unsubscribe" {
                "events.unsubscribe"
            } else {
                "objects.write"
            };
            let original = writes(&before, method);
            let attempted = writes(&after, method);
            if name != "marker-abort" {
                // The fixture faults the first request; Store can recover it
                // and finish other writes in that same bounded page.
                let id = original.first().unwrap().required("mutationId").unwrap();
                let repeats = attempted
                    .iter()
                    .filter(|p| p.get("mutationId") == Some(id))
                    .count();
                assert_eq!(
                    repeats, 2,
                    "Original unknown request must be reconciled with its original ID despite a fresh epoch: {name}"
                );
            }
            result.insert(
                "resumedError",
                second.error.map_or(IvyJson::Null, IvyJson::from),
            );
            result.insert("resumedState", second.state);
            result.insert("resumedReceipts", second.receipts);
            result.insert("trace", after.required("trace").unwrap().clone());
            restarted.close();
        } else {
            client.close();
            result.insert("trace", before.required("trace").unwrap().clone());
        }
        // Include a compact raw retention count; all byte/type/history comparisons
        // above are assertions against the original Node database, not target JSON.
        result.insert(
            "originalTables",
            IvyJson::Number(
                first
                    .snapshot
                    .required("tables")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len() as f64,
            ),
        );
        println!("{}", canonical(&result, 32 * 1024 * 1024).unwrap());
    }
    observer.close();
    drop(process);
}
