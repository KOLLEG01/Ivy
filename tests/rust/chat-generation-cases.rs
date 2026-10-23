use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    generation::{ChatGeneration, ChatGenerationConfig, settled},
    notice_input::NoticeInstructions,
    store::ChatStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult,
    checked_native_contract::CheckedNativeContracts,
    json::{canonical, parse},
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestOptions, RpcClient},
    service::{ConnectionOptions, ServiceConnection},
};
use std::{
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command as AsyncCommand},
    sync::Notify,
};

fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    v.required(key).unwrap()
}
fn text(v: &Json, key: &str) -> String {
    field(v, key).as_str().unwrap()
}
fn flag(v: &Json, key: &str) -> bool {
    v.get(key).and_then(Json::as_bool) == Some(true)
}
fn equal(a: &Json, b: &Json, label: &str) {
    assert_eq!(
        canonical(a, usize::MAX).unwrap(),
        canonical(b, usize::MAX).unwrap(),
        "{label}"
    );
}
fn result<T>(v: IvyResult<T>) -> Json {
    match v {
        Ok(_) => Json::object([("value", Json::Null)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome_json()),
            ]),
        )]),
    }
}
fn oracle() -> &'static Json {
    static CORPUS: OnceLock<Json> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let out = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .args(["tests/rust/fixtures/chat-generation-peer.mjs", "oracle"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_GENERATION_ORACLE_CAPTURE") {
            std::fs::write(path, &out.stdout).unwrap();
        }
        parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap()
    })
}
struct Fixed(i64, Instant);
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> Instant {
        self.1
    }
}
struct Peer {
    child: Child,
    url: String,
}
impl Peer {
    async fn start(corpus: &Path) -> Self {
        let mut child = AsyncCommand::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-generation-peer.mjs")
            .arg("peer")
            .arg(corpus)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let line = tokio::time::timeout(
            Duration::from_secs(30),
            BufReader::new(child.stdout.take().unwrap())
                .lines()
                .next_line(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let value = parse(&line, usize::MAX).unwrap();
        Self {
            child,
            url: text(&value, "url"),
        }
    }
    async fn connect(
        &self,
        index: usize,
        generation: u64,
        after: usize,
        row: &Json,
        selected: Arc<Mutex<Option<Arc<ChatGeneration>>>>,
        held: bool,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    ) -> ServiceConnection {
        let mut options = ConnectionOptions::new("chat-provider".into());
        for tool in field(
            &field(row, "registry")
                .required("namespaces")
                .unwrap()
                .as_array()
                .unwrap()[0],
            "tools",
        )
        .as_array()
        .unwrap()
        {
            let name = text(tool, "name");
            let selected = selected.clone();
            let entered = entered.clone();
            let release = release.clone();
            options.handlers.insert(
                format!("chat.{name}").into(),
                Arc::new(move |args, context| {
                    let lease = selected
                        .lock()
                        .unwrap()
                        .as_ref()
                        .filter(|g| g.generation() == context.generation)
                        .ok_or_else(IvyError::internal)
                        .and_then(|g| g.accept());
                    let entered = entered.clone();
                    let release = release.clone();
                    let name = name.clone();
                    Box::pin(async move {
                        let lease = lease?;
                        let result = lease.invoke(&name, &args, &context).await?;
                        if held && name == "workspace" {
                            entered.notify_one();
                            release.notified().await;
                        }
                        // Keep accepted storage ownership through the final handler boundary.
                        drop(lease);
                        Ok(result)
                    })
                }),
            );
        }
        let c = ServiceConnection::connect(
            &self.url,
            "fixture-token",
            options,
            RequestOptions::default().cancellation,
        )
        .await
        .unwrap();
        c.request(
            "fixture.case",
            Json::object([
                ("index", Json::Number(index as f64)),
                ("generation", Json::Number(generation as f64)),
                ("after", Json::Number(after as f64)),
            ]),
            RequestOptions::default(),
        )
        .await
        .unwrap();
        c.set_generation(generation);
        c.set_registry(field(row, "registry")).unwrap();
        c.set_ready(true);
        c
    }
    async fn remaining(c: &ServiceConnection, expected: usize) {
        let r = c
            .request(
                "fixture.remaining",
                Json::object([] as [(&str, Json); 0]),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(field(&r, "remaining").as_f64(), Some(expected as f64));
        equal(
            field(&r, "mismatches"),
            &Json::Array(vec![]),
            "RPC errors cannot conceal a request mismatch",
        );
    }
    async fn finish(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
    }
}

fn database(path: &Path) -> Json {
    use rusqlite::types::ValueRef;
    let db = rusqlite::Connection::open(path).unwrap();
    let names: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    Json::Array(
        names
            .into_iter()
            .map(|name| {
                assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
                let mut statement = db.prepare(&format!("SELECT * FROM {name}")).unwrap();
                let columns = statement.column_count();
                let mut rows: Vec<_> = statement
                    .query_map([], |row| {
                        Ok(Json::Array(
                            (0..columns)
                                .map(|i| match row.get_ref(i).unwrap() {
                                    ValueRef::Null => Json::Null,
                                    ValueRef::Integer(v) => {
                                        Json::Array(vec!["integer".into(), v.to_string().into()])
                                    }
                                    ValueRef::Real(v) => {
                                        Json::Array(vec!["real".into(), v.to_string().into()])
                                    }
                                    ValueRef::Text(v) => Json::Array(vec![
                                        "text".into(),
                                        String::from_utf8_lossy(v).as_ref().into(),
                                    ]),
                                    ValueRef::Blob(v) => {
                                        Json::Array(vec!["blob".into(), STANDARD.encode(v).into()])
                                    }
                                })
                                .collect(),
                        ))
                    })
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                rows.sort_by_key(|v| canonical(v, usize::MAX).unwrap());
                Json::object([("table", name.into()), ("rows", Json::Array(rows))])
            })
            .collect(),
    )
}

#[tokio::test]
async fn actual_provider_generation_reconcile_and_retirement_match_original_main() {
    let root = std::env::temp_dir().join(format!("ivy-rust-generation-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    eprintln!("Generation fixture: {}", root.display());
    let corpus_path = root.join("original.json");
    std::fs::write(&corpus_path, canonical(oracle(), usize::MAX).unwrap()).unwrap();
    let peer = Peer::start(&corpus_path).await;
    let mut rpcs = 0;
    let mut successes = 0;
    let mut failures = 0;
    for (index, row) in field(oracle(), "cases")
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let label = text(row, "label");
        eprintln!("Generation case: {label}");
        let dir = root.join(index.to_string());
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("chat-bridge.sqlite");
        std::fs::write(&path, STANDARD.decode(text(row, "database")).unwrap()).unwrap();
        let config = Arc::new(ChatGenerationConfig {
            admission: ChatAdmission::new(field(row, "definition")).unwrap(),
            catalogs: CheckedNativeContracts::new(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
            ),
            instructions: parse(
                &String::from_utf8(STANDARD.decode(text(oracle(), "instructions")).unwrap())
                    .unwrap(),
                usize::MAX,
            )
            .unwrap()
            .into_js_string()
            .unwrap(),
            notice_instructions: NoticeInstructions::from_json(
                &parse(
                    &String::from_utf8(
                        STANDARD
                            .decode(text(oracle(), "noticeInstructions"))
                            .unwrap(),
                    )
                    .unwrap(),
                    usize::MAX,
                )
                .unwrap(),
            )
            .unwrap(),
            service_node_id: "chat-provider".into(),
            data_root: Some(dir.clone()),
            language: "en".into(),
        });
        let fixed = || {
            Arc::new(Fixed(
                field(oracle(), "now").as_f64().unwrap() as i64,
                Instant::now(),
            ))
        };
        let selected = Arc::new(Mutex::new(None));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let number = field(row, "generation").as_f64().unwrap() as u64;
        let mut connection = peer
            .connect(
                index,
                number,
                0,
                row,
                selected.clone(),
                flag(row, "retire"),
                entered.clone(),
                release.clone(),
            )
            .await;
        if flag(row, "preclosed") {
            connection.close();
        }
        let reconciled =
            ChatGeneration::with_clock(config.clone(), connection.clone(), fixed()).await;
        equal(
            &result(reconciled.as_ref().map(|_| ()).map_err(Clone::clone)),
            field(row, "reconciled"),
            &label,
        );
        if let Ok(generation) = reconciled {
            successes += 1;
            *selected.lock().unwrap() = Some(generation.clone());
            if flag(row, "invoke") {
                if flag(row, "retire") {
                    let old_connection = connection.clone();
                    let invocation = tokio::spawn(async move {
                        old_connection
                            .request(
                                "fixture.invoke",
                                Json::object([] as [(&str, Json); 0]),
                                RequestOptions::default(),
                            )
                            .await
                    });
                    tokio::time::timeout(Duration::from_secs(10), entered.notified())
                        .await
                        .unwrap();
                    Peer::remaining(&connection, 2).await;
                    let observer = generation.accept().unwrap();
                    connection.close();
                    let closing = generation.retire();
                    assert!(generation.accept().is_err());
                    assert!(closing.borrow().is_none());
                    let readable = observer
                        .store()
                        .named("chat-bridge/main", "retiring-generation")
                        .await
                        .map_err(|e| e.into_ivy());
                    equal(
                        &result(readable),
                        field(row, "readableWhileAccepted"),
                        "accepted storage remains open",
                    );
                    drop(observer);
                    assert!(
                        closing.borrow().is_none(),
                        "invocation alone must retain storage"
                    );
                    connection = peer
                        .connect(
                            index,
                            number + 1,
                            3,
                            row,
                            selected.clone(),
                            false,
                            Arc::new(Notify::new()),
                            Arc::new(Notify::new()),
                        )
                        .await;
                    let reopened =
                        ChatGeneration::with_clock(config, connection.clone(), fixed()).await;
                    equal(
                        &result(reopened.as_ref().map(|_| ()).map_err(Clone::clone)),
                        field(row, "reopened"),
                        "reopen with retiring readers",
                    );
                    let next = reopened.unwrap();
                    *selected.lock().unwrap() = Some(next.clone());
                    release.notify_one();
                    assert!(invocation.await.unwrap().is_err());
                    tokio::time::timeout(Duration::from_secs(10), settled(closing))
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(flag(row, "closedAfterAccepted"));
                    settled(generation.retire()).await.unwrap();
                    assert!(
                        next.accept()
                            .unwrap()
                            .store()
                            .named("chat-bridge/main", "new-generation")
                            .await
                            .unwrap()
                            .is_none()
                    );
                    Peer::remaining(&connection, 0).await;
                    connection.close();
                    settled(next.retire()).await.unwrap();
                } else {
                    let mut invoked = connection
                        .request(
                            "fixture.invoke",
                            Json::object([] as [(&str, Json); 0]),
                            RequestOptions::default(),
                        )
                        .await
                        .unwrap();
                    let mut expected = field(row, "invoked").clone();
                    invoked.remove("id");
                    expected.remove("id");
                    equal(
                        &invoked,
                        &expected,
                        "real provider context and native outage",
                    );
                    Peer::remaining(&connection, 0).await;
                    connection.close();
                    settled(generation.retire()).await.unwrap();
                }
            } else {
                Peer::remaining(&connection, 0).await;
                connection.close();
                settled(generation.retire()).await.unwrap();
            }
        } else {
            failures += 1;
            if !flag(row, "preclosed") {
                Peer::remaining(&connection, 0).await;
            }
            connection.close();
        }
        *selected.lock().unwrap() = None;
        let expected = dir.join("original-retained.sqlite");
        std::fs::write(
            &expected,
            STANDARD.decode(text(row, "retainedDatabase")).unwrap(),
        )
        .unwrap();
        equal(
            &database(&path),
            &database(&expected),
            "all retained SQLite rows",
        );
        // Reopening for a local read confirms failed reconciliation released its handle.
        let store = ChatStore::open(Arc::new(connection), None, Some(dir))
            .await
            .unwrap();
        store.close().await.unwrap();
        rpcs += field(row, "calls").as_array().unwrap().len();
    }
    peer.finish().await;
    eprintln!(
        "Generation: {successes} successful / {failures} refused original lifecycles; {rpcs} exact RPC comparisons; accepted handler retirement/reopen and all SQLite rows."
    );
    assert_eq!((successes, failures, rpcs), (6, 10, 37));
    let absolute = std::fs::canonicalize(&root).unwrap();
    let temporary = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    assert_eq!(absolute.parent(), Some(temporary.as_path()));
    assert!(
        absolute
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ivy-rust-generation-")
    );
    std::fs::remove_dir_all(absolute).unwrap();
}
