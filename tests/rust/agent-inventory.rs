use ivy_agent_manager::{
    inventory::{
        InventoryRead, InventorySignal, NativeInventory, native_source_kinds,
        publish_native_projects, publish_native_snapshot, publish_native_threads,
    },
    native_journal::{Limits, NativeJournal, Owner},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::{digest, hash_json},
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::{
    codex_admission::Priority,
    codex_rpc::{Events, NativeRpc},
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::InvocationContext,
};
use std::{
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn text(value: &IvyJson, key: &str) -> String {
    field(value, key).as_str().unwrap()
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let result = Command::new("node")
            .arg("tests/fixtures/agent-inventory-oracle.mjs")
            .current_dir(root())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        parse(
            std::str::from_utf8(&result.stdout).unwrap(),
            4 * 1024 * 1024,
        )
        .unwrap()
    })
}
fn catalog_at(path: &str) -> IvyJson {
    parse(
        &std::fs::read_to_string(root().join(path)).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn catalog(row: &IvyJson) -> IvyJson {
    if let Some(value) = row.get("catalog") {
        return value.clone();
    }
    let mut value = catalog_at("specs/native/codex-0.154.0/catalog.json");
    if row.get("projectsSupported") == Some(&IvyJson::Bool(false)) {
        let mut methods = value
            .remove("clientRequests")
            .unwrap()
            .into_array()
            .unwrap();
        methods.retain(|v| v.get("method") != Some(&"project/list".into()));
        value.insert("clientRequests", IvyJson::Array(methods));
    }
    value
}
fn error(value: &IvyJson) -> IvyError {
    let mut error = IvyError::new_js(
        field(value, "code").as_js_string().unwrap().clone(),
        field(value, "message").as_js_string().unwrap().clone(),
    )
    .with_outcome(match text(value, "outcome").as_str() {
        "unknown" => Outcome::Unknown,
        "completed" => Outcome::Completed,
        _ => Outcome::NotExecuted,
    });
    if let Some(details) = value.get("details") {
        error = error.with_details(details.clone());
    }
    error
}
fn context() -> InvocationContext {
    InvocationContext::detached(
        "caller".into(),
        None,
        1,
        Default::default(),
        error(field(data(), "abortReason")),
    )
}
fn outcome(value: IvyResult<IvyJson>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("result", value)]),
        Err(error) => IvyJson::object([("error", error.to_wire())]),
    }
}
fn ordered(value: IvyJson) -> IvyJson {
    if value.as_object().is_some() {
        let mut values = value.into_object().unwrap();
        values.sort_by(|a, b| a.0.cmp(&b.0));
        IvyJson::Object(values.into_iter().map(|(k, v)| (k, ordered(v))).collect())
    } else if value.as_array().is_some() {
        IvyJson::Array(
            value
                .into_array()
                .unwrap()
                .into_iter()
                .map(ordered)
                .collect(),
        )
    } else {
        value
    }
}
fn same(actual: &IvyJson, expected: &IvyJson, label: &str) {
    assert_eq!(
        ordered(actual.clone()),
        ordered(expected.clone()),
        "{label}"
    );
}
fn capture(value: IvyJson) -> IvyResult<IvyJson> {
    let size = stringify(&value, MANAGEMENT_FRAME_BYTES)?.len();
    Ok(if size > 4096 {
        IvyJson::object([
            ("encodedBytes", IvyJson::Number(size as f64)),
            ("hash", hash_json(&value)?.into()),
        ])
    } else {
        IvyJson::object([("value", value)])
    })
}
fn materialize(value: &IvyJson) -> IvyJson {
    if let Some(values) = value.as_array() {
        return IvyJson::Array(values.iter().map(materialize).collect());
    }
    if let Some(values) = value.as_object() {
        if let Some(fill) = value.get("$fill") {
            return IvyJson::String(JsString(
                fill.as_js_string()
                    .unwrap()
                    .0
                    .repeat(field(value, "count").as_f64().unwrap() as usize),
            ));
        }
        if let Some(item) = value.get("$repeat") {
            return IvyJson::Array(
                (0..field(value, "count").as_f64().unwrap() as usize)
                    .map(|i| {
                        let mut item = materialize(item);
                        if value.get("ids") == Some(&IvyJson::Bool(true)) {
                            item.insert("id", format!("id-{i}").into());
                        }
                        item
                    })
                    .collect(),
            );
        }
        return IvyJson::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), materialize(v)))
                .collect(),
        );
    }
    value.clone()
}
struct Script {
    replies: Vec<IvyJson>,
    cursor: usize,
    calls: Vec<IvyJson>,
}
impl Script {
    fn new(row: &IvyJson) -> Self {
        Self {
            replies: field(row, "replies").as_array().unwrap().to_vec(),
            cursor: 0,
            calls: vec![],
        }
    }
    fn next(&mut self) -> IvyJson {
        let value = self
            .replies
            .get(self.cursor)
            .expect("unexpected extra request")
            .clone();
        self.cursor += 1;
        value
    }
}

#[test]
fn inventory_source_kinds_match_all_retained_catalogs_and_original_schema_resolution() {
    let rows = field(data(), "sourceKinds").as_array().unwrap();
    assert_eq!(rows.iter().filter(|v| v.get("path").is_some()).count(), 7);
    for row in rows {
        let catalog = if let Some(path) = row.get("path") {
            catalog_at(&path.as_str().unwrap())
        } else {
            field(row, "catalog").clone()
        };
        let result = native_source_kinds(&catalog).map(IvyJson::Array);
        same(
            &outcome(result),
            field(row, "expected"),
            &format!(
                "source kinds {}",
                stringify(row, MANAGEMENT_FRAME_BYTES).unwrap()
            ),
        );
    }
}

#[tokio::test]
async fn complete_native_inventory_matches_original_pages_ownership_deadlines_and_cache() {
    let rows = field(data(), "collections").as_array().unwrap();
    assert!(rows.len() >= 50);
    for row in rows {
        let script = Arc::new(Mutex::new(Script::new(row)));
        let now = Arc::new(AtomicU64::new(1_788_600_000_000));
        let context = context();
        let signal = InventorySignal::from_context(context.clone());
        let read: Arc<InventoryRead> = {
            let script = script.clone();
            let now = now.clone();
            let context = context.clone();
            Arc::new(move |method, params, options| {
                let script = script.clone();
                let now = now.clone();
                let context = context.clone();
                Box::pin(async move {
                    let step = {
                        let mut script = script.lock().unwrap();
                        let step = script.next();
                        assert_eq!(
                            method,
                            field(&step, "method").as_js_string().unwrap().clone()
                        );
                        assert!(matches!(options.priority, Priority::Background));
                        assert!(
                            options.request_id.is_none()
                                && options.before_send.is_none()
                                && options.before_resolve.is_none()
                        );
                        script.calls.push(IvyJson::object([
                            ("method", IvyJson::String(method)),
                            ("params", params),
                            ("detachOnTimeout", options.detach_on_timeout.into()),
                            ("budget", IvyJson::Number(options.timeout_ms as f64)),
                        ]));
                        step
                    };
                    now.fetch_add(
                        step.get("advanceMs")
                            .and_then(IvyJson::as_f64)
                            .unwrap_or(0.0) as u64,
                        Ordering::SeqCst,
                    );
                    if step.get("abort") == Some(&IvyJson::Bool(true)) {
                        context.signal.cancel();
                    }
                    if let Some(failure) = step.get("throw") {
                        return Err(error(failure));
                    }
                    if step.get("uncoded") == Some(&IvyJson::Bool(true)) {
                        return Err(IvyError::internal());
                    }
                    Ok(materialize(field(&step, "reply")))
                })
            })
        };
        let clock_now = now.clone();
        let clock_iso = now.clone();
        let inventory = NativeInventory::with_reader(
            catalog(row),
            field(data(), "epoch").as_js_string().unwrap().clone(),
            read,
            Arc::new(move || clock_now.load(Ordering::SeqCst) as f64),
            Arc::new(move || {
                chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                    clock_iso.load(Ordering::SeqCst) as i64,
                )
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                .into()
            }),
        )
        .unwrap();
        for (i, step) in field(row, "commands")
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let result = match text(step, "action").as_str() {
                "abort" => {
                    context.signal.cancel();
                    Ok(IvyJson::Null)
                }
                "currentProjects" => inventory.current_projects(
                    step.get("observedAt")
                        .and_then(IvyJson::as_js_string)
                        .cloned(),
                ),
                "collectThreads" => inventory.collect_threads(Some(&signal)).await,
                "collectProjects" => inventory.collect_projects(Some(&signal)).await,
                "collect" => inventory.collect(Some(&signal)).await,
                action => panic!("{action}"),
            }
            .and_then(capture);
            same(
                &outcome(result),
                &field(row, "expected").as_array().unwrap()[i],
                &format!("{} step {i}", text(row, "name")),
            );
        }
        let script = script.lock().unwrap();
        same(
            &IvyJson::Array(script.calls.clone()),
            field(row, "calls"),
            &format!("{} requests", text(row, "name")),
        );
        assert_eq!(
            script.cursor,
            field(row, "consumed").as_f64().unwrap() as usize
        );
    }
}

struct Publisher {
    script: Mutex<Script>,
    context: InvocationContext,
}
impl RpcClient for Publisher {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let step = self.script.lock().unwrap().next();
            assert_eq!(method, "inventory.sync");
            if step.get("abort") == Some(&IvyJson::Bool(true)) {
                self.context.signal.cancel();
            }
            self.script.lock().unwrap().calls.push(IvyJson::object([
                ("method", method.into()),
                ("params", params),
                (
                    "signalAbortedAfterHook",
                    options.cancellation.is_cancelled().into(),
                ),
            ]));
            if let Some(failure) = step.get("throw") {
                return Err(error(failure));
            }
            if step.get("uncoded") == Some(&IvyJson::Bool(true)) {
                return Err(IvyError::internal());
            }
            Ok(IvyJson::object::<&str>([]))
        })
    }
}
#[tokio::test]
async fn native_inventory_publication_matches_original_authority_and_uncertainty_guards() {
    let rows = field(data(), "publications").as_array().unwrap();
    assert!(rows.len() >= 25);
    for row in rows {
        let context = context();
        let signal = InventorySignal::from_context(context.clone());
        let connection = Publisher {
            script: Mutex::new(Script::new(row)),
            context: context.clone(),
        };
        let journal = Mutex::new(
            NativeJournal::new(
                Owner {
                    service_node_id: row
                        .get("ownerId")
                        .and_then(IvyJson::as_js_string)
                        .cloned()
                        .unwrap_or("owner".into()),
                    native_version: "fixture".into(),
                    native_executable_hash: digest("fixture").into(),
                },
                Limits {
                    max_operations: 100,
                    max_journal_bytes: 64 * 1024 * 1024,
                    max_pending_inputs: 10,
                },
            )
            .unwrap(),
        );
        let signal = if row.get("signal") == Some(&IvyJson::Bool(true)) {
            Some(&signal)
        } else {
            None
        };
        let snapshot = field(data(), "snapshot");
        for (i, step) in field(row, "commands")
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let result = match text(step, "action").as_str() {
                "abort" => {
                    context.signal.cancel();
                    Ok(IvyJson::Null)
                }
                "close" => {
                    journal.lock().unwrap().close();
                    Ok(IvyJson::Null)
                }
                "revision" => Ok(IvyJson::Number(journal.lock().unwrap().inventory_revision(
                    &text(step, "kind"),
                    field(step, "minimum").as_f64().unwrap(),
                ))),
                "threads" => publish_native_threads(&connection, &journal, snapshot, signal)
                    .await
                    .map(|_| IvyJson::Null),
                "projects" => publish_native_projects(&connection, &journal, snapshot, signal)
                    .await
                    .map(|_| IvyJson::Null),
                "snapshot" => publish_native_snapshot(&connection, &journal, snapshot, signal)
                    .await
                    .map(|_| IvyJson::Null),
                action => panic!("{action}"),
            };
            same(
                &outcome(result),
                &field(row, "expected").as_array().unwrap()[i],
                &format!("{} step {i}", text(row, "name")),
            );
        }
        let script = connection.script.lock().unwrap();
        same(
            &IvyJson::Array(script.calls.clone()),
            field(row, "calls"),
            &format!("{} publications", text(row, "name")),
        );
        assert_eq!(
            script.cursor,
            field(row, "consumed").as_f64().unwrap() as usize
        );
    }
}

#[tokio::test]
async fn inventory_native_rpc_adapter_uses_the_original_read_frames() {
    let catalog = catalog_at("specs/native/codex-0.154.0/catalog.json");
    let (input, sent) = tokio::io::duplex(65536);
    let (mut emit, output) = tokio::io::duplex(65536);
    let rpc = NativeRpc::start(&catalog, input, output, Events::default()).unwrap();
    let inventory = Arc::new(NativeInventory::new(catalog, "epoch".into(), rpc.clone()).unwrap());
    let task = {
        let inventory = inventory.clone();
        tokio::spawn(async move { inventory.collect(None).await })
    };
    let mut sent = BufReader::new(sent);
    for (i, method) in [
        "thread/loaded/list",
        "thread/list",
        "thread/list",
        "thread/loaded/list",
        "project/list",
    ]
    .into_iter()
    .enumerate()
    {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), sent.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let frame = parse(&line, MANAGEMENT_FRAME_BYTES).unwrap();
        assert_eq!(frame.get("method"), Some(&method.into()));
        let params = field(&frame, "params");
        assert_eq!(params.get("limit"), Some(&IvyJson::Number(100.0)));
        if method == "thread/list" {
            assert_eq!(params.get("archived"), Some(&IvyJson::Bool(i == 2)));
            assert_eq!(params.get("useStateDbOnly"), Some(&IvyJson::Bool(true)));
        }
        let reply = IvyJson::object([
            ("id", field(&frame, "id").clone()),
            (
                "result",
                IvyJson::object([
                    ("data", IvyJson::Array(vec![])),
                    ("nextCursor", IvyJson::Null),
                ]),
            ),
        ]);
        emit.write_all(
            format!("{}\n", stringify(&reply, MANAGEMENT_FRAME_BYTES).unwrap()).as_bytes(),
        )
        .await
        .unwrap();
    }
    let snapshot = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(field(&snapshot, "threads").as_array().unwrap().is_empty());
    assert!(
        field(field(&snapshot, "projects"), "projects")
            .as_array()
            .unwrap()
            .is_empty()
    );
    rpc.close("fixture_closed").await;
}
