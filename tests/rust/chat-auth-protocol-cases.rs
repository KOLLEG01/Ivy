//! New NAB-3P authority/transaction/lifetime risks only; no live socket constructor.
#[path = "../../services/chat-bridge/rust/src/whatsapp/auth_prototype.rs"]
mod prototype;
use super::auth_node::{fixture, launch, lock_held, raw};
use ivy_chat_bridge::whatsapp::{
    auth_node::AuthLaunch,
    journal_io::JournalIo,
    journal_owner::Journal,
    messages::Translator,
    runtime::{RuntimeCallbacks, RuntimeParts, RuntimeResources, RuntimeSocket, WhatsAppRuntime},
};
use ivy_contracts::{IvyJson as Json, IvyResult, JsString, Outcome, json::parse};
use ivy_host_runtime::{
    exec_file::SpawnOptions,
    node_runtime::NodeRuntime,
    portable_native_files::NativeHelperLayout,
    process::{self, Command, LaunchContext},
};
use ivy_sdk::service::ServiceFuture;
use prototype::{
    Fault, ProtocolAccount, Reply,
    store::{Store, encode},
};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn read(path: impl AsRef<Path>) -> Json {
    parse(&fs::read_to_string(path).unwrap(), usize::MAX).unwrap()
}
fn buffer(bytes: &[u8]) -> Json {
    use base64::Engine;
    Json::object([
        ("type", "Buffer".into()),
        (
            "data",
            base64::engine::general_purpose::STANDARD
                .encode(bytes)
                .into(),
        ),
    ])
}
fn setup(root: &Path, mode: Option<&str>, fault: Option<&str>) -> AuthLaunch {
    // Reuse the parent's admitted artifact in the owner-process fixture. Never
    // recopy an executing Windows Job helper or mutate its immutable artifact.
    if std::env::var_os("IVY_CHAT_PROTOCOL_OWNER_ROOT").as_deref() == Some(root.as_os_str()) {
        assert!(mode.is_none() && fault.is_none());
        let environment = ["NODE_OPTIONS", "IVY_CHAT_PROTOCOL_GATE"]
            .into_iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (name.into(), value)))
            .collect();
        return AuthLaunch {
            node: NodeRuntime {
                executable: PathBuf::from(
                    std::env::var_os("IVY_CHAT_PROTOCOL_CHILD_NODE").unwrap(),
                ),
                version: std::env::var("IVY_CHAT_PROTOCOL_CHILD_NODE_VERSION").unwrap(),
            },
            artifact_root: PathBuf::from(
                std::env::var_os("IVY_CHAT_PROTOCOL_CHILD_ARTIFACT").unwrap(),
            ),
            helper_layout: NativeHelperLayout::Legacy,
            environment,
            deadline: Duration::from_secs(15),
        };
    }
    let mut options = launch(root, None);
    options.deadline = Duration::from_secs(15);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    fs::copy(
        source.join("services/chat-bridge/rust/node/protocol-auth.mjs"),
        options
            .artifact_root
            .join("services/chat-bridge/rust/node/protocol-auth.mjs"),
    )
    .unwrap();
    fs::create_dir_all(options.artifact_root.join("tests/rust/fixtures")).unwrap();
    fs::copy(
        source.join("tests/rust/fixtures/chat-auth-protocol-worker.mjs"),
        options
            .artifact_root
            .join("tests/rust/fixtures/chat-auth-protocol-worker.mjs"),
    )
    .unwrap();
    options.environment.insert(
        "IVY_CHAT_PROTOCOL_GATE".into(),
        root.join("protocol").into_os_string(),
    );
    if let Some(mode) = mode {
        options
            .environment
            .insert("IVY_CHAT_PROTOCOL_GATE_MODE".into(), mode.into());
    }
    if let Some(fault) = fault {
        options
            .environment
            .insert("IVY_CHAT_PROTOCOL_FAULT".into(), fault.into());
    }
    options
}
fn store(root: &Path) -> Store {
    Store::initialize_synthetic(
        &root.join("auth.sqlite"),
        &root.join("whatsapp-owner/executor-lock.sqlite"),
    )
    .unwrap()
}
fn reopen(root: &Path) -> IvyResult<Store> {
    Store::reopen_synthetic(
        &root.join("auth.sqlite"),
        &root.join("whatsapp-owner/executor-lock.sqlite"),
    )
}
fn receipt_rows(root: &Path) -> Json {
    let db = Connection::open(root.join("auth.sqlite")).unwrap();
    let mut query = db
        .prepare("SELECT key,value FROM chat_auth_receipt ORDER BY key")
        .unwrap();
    let rows = query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(|r| {
            let (k, v) = r.unwrap();
            Json::object([("key", k.into()), ("value", parse(&v, usize::MAX).unwrap())])
        })
        .collect();
    Json::Array(rows)
}
fn owner_row(root: &Path) -> Json {
    receipt_rows(root)
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r.get("key").and_then(Json::as_js_string) == Some(&JsString::from("owner")))
        .unwrap()
        .required("value")
        .unwrap()
        .clone()
}
async fn observed(root: &Path, name: &str) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !root.join(format!("protocol.{name}.observed")).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Actual child did not reach the fixture gate");
}
fn release(root: &Path, name: &str) {
    fs::write(root.join(format!("protocol.{name}.release")), "continue").unwrap();
}
fn capture(name: &str, value: Json) {
    if let Some(base) = std::env::var_os("IVY_CHAT_PROTOCOL_CAPTURE") {
        fs::write(
            format!("{}-{name}.json", Path::new(&base).display()),
            encode(&value),
        )
        .unwrap();
    }
}
fn table_count(root: &Path, table: &str) -> i64 {
    Connection::open(root.join("auth.sqlite"))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name=?",
            [table],
            |r| r.get(0),
        )
        .unwrap()
}
fn value(reply: Reply) -> Json {
    match reply {
        Reply::Value(v) => parse(&v, usize::MAX).unwrap(),
        Reply::LibraryError(name) => panic!("Unexpected categorical library result: {name}"),
    }
}
async fn close(owner: &ProtocolAccount) {
    owner.quiesce().await.unwrap();
    owner.retire().await.unwrap();
}
struct Socket(ProtocolAccount);
impl RuntimeSocket for Socket {
    fn bind(&self, _: RuntimeCallbacks) {}
    fn start(&self) {
        panic!("No live protocol socket");
    }
    fn state(&self) -> JsString {
        "stopped".into()
    }
    fn status(&self) -> Json {
        Json::Null
    }
    fn flush(&self) -> ServiceFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn read_receipts(&self) -> ServiceFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn set_busy(&self, _: Vec<JsString>) {}
    fn presence(&self) -> ServiceFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> ServiceFuture<()> {
        let proof = self.0.quiesce();
        Box::pin(async move { proof.await.map(|_| ()) })
    }
}
fn composed(owner: &ProtocolAccount, root: &Path) -> WhatsAppRuntime {
    let journal = Journal::new(JournalIo::open(root.join("runtime.sqlite")).unwrap());
    let checked = journal.clone();
    let retire = owner.clone();
    WhatsAppRuntime::new(RuntimeParts {
        config: Json::Null,
        host_id: "synthetic-host".into(),
        journal,
        socket: Arc::new(Socket(owner.clone())),
        media: Arc::new(super::account_auth::CloseOnly),
        factory: Arc::new(super::account_auth::CloseOnly),
        translator: Translator::default(),
        resources: RuntimeResources {
            status: Arc::new(|_| Box::pin(async { Ok(()) })),
            retire_account: Arc::new(move || {
                assert!(
                    checked.access(|_| Ok(())).is_err(),
                    "Journal must close between cached quiescence and store/lock release"
                );
                retire.retire()
            }),
        },
    })
}

#[test]
fn nested_original_transactions_interleave_and_retire_before_same_db_reopen() {
    runtime().block_on(async {
        let root = fixture("protocol-nested");
        let db = root.join("auth.sqlite");
        assert_eq!(table_count(&root, "chat_auth_receipt"), 0);
        let unchanged = raw(&db, "future-category", "opaque");
        let old_session = raw(&db, "session", "15555550101.1");
        let owner = ProtocolAccount::launch(store(&root), setup(&root, None, None))
            .await
            .unwrap();
        let runtime = composed(&owner, &root);
        let wire = read(root.join("wire.json"));
        let mut inbound = wire.required("current").unwrap().as_array().unwrap()[0].clone();
        use base64::Engine;
        inbound.insert(
            "ciphertext",
            buffer(
                &base64::engine::general_purpose::STANDARD
                    .decode(inbound.required("ciphertext").unwrap().as_str().unwrap())
                    .unwrap(),
            ),
        );
        inbound.insert("jid", "15555550101:1@s.whatsapp.net".into());
        let call = owner
            .start(
                "outbound",
                "nested",
                Json::object([
                    ("inbound", inbound),
                    (
                        "outbound",
                        Json::object([
                            ("jid", "15555550101:1@s.whatsapp.net".into()),
                            ("data", buffer(b"nested-reply")),
                        ]),
                    ),
                ]),
            )
            .await
            .unwrap();
        let id = call.id.clone();
        observed(&root, "nested-return").await;
        assert_eq!(
            raw(&db, "session", "15555550101.1"),
            old_session,
            "Nested returns cannot force the outer transaction commit"
        );
        assert!(raw(&db, "future-category", "nested-buffered").is_none());
        assert_eq!(
            owner.read(Some(&id)).await.unwrap().get("state"),
            Some(&Json::from("entered"))
        );
        let before = receipt_rows(&root);
        for method in ["decryptMessage", "encryptMessage"] {
            assert!(before.as_array().unwrap().iter().any(|r| {
                r.get("value").is_some_and(|v| {
                    v.get("method") == Some(&Json::from(method))
                        && v.get("state") == Some(&Json::from("completed"))
                })
            }));
        }
        let (_, event) = owner
            .call("inbound-crypto", "event", "independent-event".into())
            .await
            .unwrap();
        assert_eq!(
            value(event).get("event"),
            Some(&Json::from("inert-protocol"))
        );
        assert!(raw(&db, "future-category", "event").is_some());
        let dropped = runtime.close();
        drop(dropped);
        tokio::time::timeout(Duration::from_secs(3), async {
            while owner.read(None).await.unwrap().get("state") != Some(&Json::from("retiring")) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            owner
                .start("outbound", "batch", Json::Array(vec![]))
                .await
                .err()
                .unwrap()
                .outcome,
            Outcome::NotExecuted
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(40), runtime.close())
                .await
                .is_err()
        );
        assert!(lock_held(&root));
        release(&root, "nested-return");
        let result = value(call.result().await.unwrap());
        assert_eq!(result.get("decrypted"), Some(&buffer(b"current-0")));
        runtime.close().await.unwrap();
        runtime.close().await.unwrap();
        assert!(!lock_held(&root));
        assert_ne!(raw(&db, "session", "15555550101.1"), old_session);
        assert_eq!(raw(&db, "future-category", "opaque"), unchanged);
        let proof = owner.quiesce().await.unwrap().value();
        assert_eq!(proof.get("callbacksSettled"), Some(&Json::from(true)));
        let old_owner = owner_row(&root);
        assert_eq!(old_owner.get("state"), Some(&Json::from("quiesced")));
        fs::remove_file(root.join("runtime.sqlite")).unwrap();
        let next = ProtocolAccount::launch(reopen(&root).unwrap(), setup(&root, None, None))
            .await
            .unwrap();
        let (_, reply) = next
            .call(
                "repository",
                "invoke",
                Json::object([
                    ("target", "keys".into()),
                    ("method", "get".into()),
                    (
                        "arguments",
                        Json::Array(vec![
                            "future-category".into(),
                            Json::Array(vec!["nested-buffered".into()]),
                        ]),
                    ),
                ]),
            )
            .await
            .unwrap();
        assert!(value(reply).get("nested-buffered").is_some());
        close(&next).await;
        assert!(!lock_held(&root));
        let stats = owner.statistics();
        capture(
            "nested",
            Json::object([
                ("beforeOuterCommit", before),
                ("afterReopen", receipt_rows(&root)),
                ("proof", proof),
                ("sameEvolvedDatabase", true.into()),
                ("nestedReturnBeforeCommit", true.into()),
                ("independentEventCompleted", true.into()),
                ("droppedCloseObserver", true.into()),
                ("maximumFrameBytes", Json::Number(stats.maximum as f64)),
                ("liveSocket", false.into()),
            ]),
        );
    });
}

#[test]
fn commit_ack_gate_is_atomic_and_dropped_close_joins_the_original_work() {
    runtime().block_on(async {
        let root = fixture("protocol-ack");
        let owner = ProtocolAccount::launch(store(&root), setup(&root, Some("ack"), None))
            .await
            .unwrap();
        let entries = Json::Array(vec![
            Json::Array(vec![
                "future-category".into(),
                "large-protocol".into(),
                encode(&Json::from("x".repeat(300000))).into(),
            ]),
            Json::Array(vec!["future-category".into(), "ordered".into(), "1".into()]),
            Json::Array(vec!["future-category".into(), "ordered".into(), Json::Null]),
            Json::Array(vec!["future-category".into(), "ordered".into(), "2".into()]),
        ]);
        let call = owner.start("raw-store", "batch", entries).await.unwrap();
        let id = call.id.clone();
        observed(&root, "ack").await;
        assert_eq!(
            raw(&root.join("auth.sqlite"), "future-category", "ordered").as_deref(),
            Some("2")
        );
        let committed = receipt_rows(&root);
        assert!(
            committed
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r.get("key") == Some(&Json::from(format!("cb/{id}/1"))))
        );
        let observer = owner.quiesce();
        drop(observer);
        assert!(
            tokio::time::timeout(Duration::from_millis(40), owner.quiesce())
                .await
                .is_err()
        );
        assert!(lock_held(&root));
        release(&root, "ack");
        call.result().await.unwrap();
        owner.quiesce().await.unwrap();
        owner.retire().await.unwrap();
        owner.retire().await.unwrap();
        assert!(!lock_held(&root));
        let stats = owner.statistics();
        assert!(stats.maximum <= 65536 && stats.sent > 3 && stats.received > 3);
        capture(
            "ack",
            Json::object([
                ("committedBeforeAck", committed),
                ("finalReceipts", receipt_rows(&root)),
                ("maximumFrameBytes", Json::Number(stats.maximum as f64)),
                ("sameOwnerCleanup", true.into()),
            ]),
        );
    });
}

#[test]
fn complete_repository_surface_and_categorical_errors_keep_original_authority() {
    runtime().block_on(async {
        let root = fixture("protocol-surface");
        let owner = ProtocolAccount::launch(store(&root), setup(&root, None, None))
            .await
            .unwrap();
        let (_, surface) = owner
            .call("repository", "surface", Json::Null)
            .await
            .unwrap();
        let surface = value(surface);
        assert_eq!(
            surface
                .required("methods")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            14
        );
        let (_, error) = owner
            .call(
                "repository",
                "invoke",
                Json::object([
                    ("method", "encryptMessage".into()),
                    ("arguments", Json::Array(vec![Json::Null])),
                ]),
            )
            .await
            .unwrap();
        assert!(matches!(error,Reply::LibraryError(ref name)if name=="TypeError"));
        let cases = [
            (
                "repository",
                "validateSession",
                Json::Array(vec!["15555550101:1@s.whatsapp.net".into()]),
            ),
            (
                "repository",
                "getSessionInfo",
                Json::Array(vec!["15555550101:1@s.whatsapp.net".into()]),
            ),
            (
                "repository",
                "hasSenderKey",
                Json::Array(vec![Json::object([
                    ("group", "new-synthetic@g.us".into()),
                    ("meId", "15555550202:1@s.whatsapp.net".into()),
                ])]),
            ),
            (
                "repository",
                "deleteSession",
                Json::Array(vec![Json::Array(vec![
                    "15555550303:1@s.whatsapp.net".into(),
                ])]),
            ),
            (
                "repository",
                "migrateSession",
                Json::Array(vec!["15555550303@s.whatsapp.net".into(), "303@lid".into()]),
            ),
            (
                "lid",
                "storeLIDPNMappings",
                Json::Array(vec![Json::Array(vec![Json::object([
                    ("pn", "15555550303@s.whatsapp.net".into()),
                    ("lid", "303@lid".into()),
                ])])]),
            ),
            ("lid", "getPNForLID", Json::Array(vec!["303@lid".into()])),
        ];
        for (target, method, args) in cases {
            let (_, reply) = owner
                .call(
                    "repository",
                    "invoke",
                    Json::object([
                        ("target", target.into()),
                        ("method", method.into()),
                        ("arguments", args),
                    ]),
                )
                .await
                .unwrap();
            value(reply);
        }
        let (_, saved) = owner
            .call(
                "credential-save",
                "credentials",
                Json::object([("accountSyncCounter", Json::Number(9.0))]),
            )
            .await
            .unwrap();
        value(saved);
        close(&owner).await;
        capture(
            "surface",
            Json::object([
                ("surface", surface),
                ("receipts", receipt_rows(&root)),
                ("completeLibraryErrorPreserved", true.into()),
                ("noSocketConstruction", true.into()),
            ]),
        );
    });
}

#[test]
fn uncertain_boundaries_refuse_replay_and_receipt_trigger_failure_rolls_back_auth() {
    runtime().block_on(async{
 let mut captures=Vec::new();
 for fault in[Fault::Prepared,Fault::Entered,Fault::CommitBefore,Fault::CommitAfter,Fault::FinishBefore,Fault::FinishAfter,Fault::ResultBody]{
   let root=fixture(&format!("protocol-{fault:?}"));let owner=ProtocolAccount::launch(store(&root),setup(&root,None,None)).await.unwrap();owner.fault(fault).await;
   let call=owner.start("raw-store","batch",Json::Array(vec![Json::Array(vec!["future-category".into(),"fault".into(),"1".into()])])).await.unwrap();let id=call.id.clone();assert_eq!(call.result().await.err().unwrap().outcome,Outcome::Unknown);
   let receipt=owner.read(Some(&id)).await.unwrap();let committed=raw(&root.join("auth.sqlite"),"future-category","fault").is_some();assert_eq!(committed,matches!(fault,Fault::CommitAfter|Fault::FinishBefore|Fault::FinishAfter|Fault::ResultBody));
   let sent=owner.statistics().sent;assert!(owner.start("raw-store","batch",Json::Array(vec![])).await.is_err());assert_eq!(owner.statistics().sent,sent);assert!(owner.quiesce().await.is_err());assert!(owner.retire().await.is_err());assert!(lock_held(&root));
   captures.push(Json::object([("fault",format!("{fault:?}").into()),("operation",id.into()),("receipt",receipt),("committed",committed.into()),("bodyAvailable",false.into()),("epoch",owner_row(&root)),("replayRefused",true.into())]));
 }
 let root=fixture("protocol-atomic-rollback");let owner=ProtocolAccount::launch(store(&root),setup(&root,None,None)).await.unwrap();
 Connection::open(root.join("auth.sqlite")).unwrap().execute_batch("CREATE TRIGGER fail_protocol_receipt BEFORE INSERT ON chat_auth_receipt WHEN NEW.key LIKE 'cb/%' AND json_extract(NEW.value,'$.kind')='set' BEGIN SELECT RAISE(ABORT,'synthetic receipt failure'); END;").unwrap();
 let call=owner.start("raw-store","batch",Json::Array(vec![Json::Array(vec!["future-category".into(),"rollback-probe".into(),"1".into()])])).await.unwrap();let id=call.id.clone();assert!(call.result().await.is_err());assert!(raw(&root.join("auth.sqlite"),"future-category","rollback-probe").is_none());assert!(!receipt_rows(&root).as_array().unwrap().iter().any(|r|r.get("key")==Some(&Json::from(format!("cb/{id}/1")))));
 capture("uncertain",Json::object([("faults",Json::Array(captures)),("atomicAuthAndReceiptRollback",true.into()),("rollbackReceipts",receipt_rows(&root))]));
});
}

#[test]
fn stale_protocol_and_unproved_stop_or_quiescence_cannot_release_the_account() {
    runtime().block_on(async {
        let mut records = Vec::new();
        for fault in [Fault::ProofBefore, Fault::ProofAfter] {
            let root = fixture(&format!("protocol-{fault:?}"));
            let owner = ProtocolAccount::launch(store(&root), setup(&root, None, None))
                .await
                .unwrap();
            owner.fault(fault).await;
            assert!(owner.quiesce().await.is_err());
            assert!(owner.retire().await.is_err());
            assert!(lock_held(&root));
            let row = owner_row(&root);
            assert_eq!(
                row.get("state"),
                Some(&Json::from(if fault == Fault::ProofAfter {
                    "quiesced"
                } else {
                    "unknown"
                }))
            );
            records.push(row);
        }
        for method in ["corrupt", "unexpected-network", "child-exit"] {
            let root = fixture(&format!("protocol-{method}"));
            let owner = ProtocolAccount::launch(store(&root), setup(&root, None, None))
                .await
                .unwrap();
            assert!(owner.call("repository", method, Json::Null).await.is_err());
            assert!(owner.quiesce().await.is_err());
            assert!(lock_held(&root));
            records.push(owner_row(&root));
        }
        for invalid in ["token", "ordinal", "digest"] {
            let root = fixture(&format!("protocol-invalid-{invalid}"));
            let owner = ProtocolAccount::launch(store(&root), setup(&root, None, None))
                .await
                .unwrap();
            assert!(
                owner
                    .call("repository", "invalid-callback", invalid.into())
                    .await
                    .is_err()
            );
            assert!(owner.quiesce().await.is_err());
            assert!(lock_held(&root));
            records.push(owner_row(&root));
        }
        let root = fixture("protocol-stop-ack");
        let owner = ProtocolAccount::launch(store(&root), setup(&root, None, Some("stop-ack")))
            .await
            .unwrap();
        assert!(owner.quiesce().await.is_err());
        assert!(lock_held(&root));
        records.push(owner_row(&root));
        capture(
            "stop",
            Json::object([
                ("epochs", Json::Array(records)),
                ("forcedStopNotQuiescence", true.into()),
                ("unexpectedNetworkRefused", true.into()),
            ]),
        );
    });
}

#[test]
#[ignore = "Subprocess fixture; invoked by the bounded owner-death case"]
fn owner_process_fixture() {
    runtime().block_on(async {
        let root = PathBuf::from(std::env::var_os("IVY_CHAT_PROTOCOL_OWNER_ROOT").unwrap());
        let mode = std::env::var("IVY_CHAT_PROTOCOL_OWNER_MODE").unwrap();
        let owner = ProtocolAccount::launch(store(&root), setup(&root, None, None))
            .await
            .unwrap();
        owner
            .call(
                "raw-store",
                "batch",
                Json::Array(vec![Json::Array(vec![
                    "future-category".into(),
                    "owner-death".into(),
                    "1".into(),
                ])]),
            )
            .await
            .unwrap();
        if mode == "quiesced" {
            owner.quiesce().await.unwrap();
        }
        fs::write(
            root.join("owner-before-exit.json"),
            encode(&owner_row(&root)),
        )
        .unwrap();
        std::process::exit(0);
    });
}

#[test]
fn late_callback_after_stop_ack_is_rejected_before_storage() {
    runtime().block_on(async {
        let root = fixture("protocol-late-stop");
        let owner = ProtocolAccount::launch(store(&root), setup(&root, None, Some("late-stop")))
            .await
            .unwrap();
        assert!(owner.quiesce().await.is_err());
        assert!(owner.retire().await.is_err());
        assert!(
            raw(
                &root.join("auth.sqlite"),
                "future-category",
                "late-after-stop"
            )
            .is_none()
        );
        assert!(lock_held(&root));
        capture(
            "late-stop",
            Json::object([
                ("epoch", owner_row(&root)),
                ("receipts", receipt_rows(&root)),
                ("postAckWriteRefused", true.into()),
                ("fenceRetained", true.into()),
            ]),
        );
    });
}

#[test]
fn initialization_error_and_incomplete_owner_metadata_refuse_admission() {
    runtime().block_on(async {
        let root = fixture("protocol-invalid-initialization");
        let db =
            ivy_chat_bridge::whatsapp::auth::AuthStore::open(root.join("auth.sqlite")).unwrap();
        db.set("creds", "default", Some("not-json")).unwrap();
        db.close().unwrap();
        assert!(
            ProtocolAccount::launch(store(&root), setup(&root, None, None))
                .await
                .is_err()
        );
        assert!(lock_held(&root));
        let failed = receipt_rows(&root);
        assert!(failed.as_array().unwrap().iter().any(|r| {
            r.get("value").is_some_and(|v| {
                v.get("method") == Some(&Json::from("initialize"))
                    && v.get("outcome") == Some(&Json::from("library_error"))
            })
        }));
        let mut malformed = Vec::new();
        for field in ["version", "initOperation", "nextOperation"] {
            let root = fixture(&format!("protocol-incomplete-{field}"));
            let owner = ProtocolAccount::launch(store(&root), setup(&root, None, None))
                .await
                .unwrap();
            close(&owner).await;
            let db = Connection::open(root.join("auth.sqlite")).unwrap();
            db.execute(
                "UPDATE chat_auth_receipt SET value=json_remove(value,?) WHERE key='owner'",
                [format!("$.{field}")],
            )
            .unwrap();
            drop(db);
            assert!(reopen(&root).is_err());
            malformed.push(owner_row(&root));
        }
        capture(
            "readiness",
            Json::object([
                ("failedInitialization", failed),
                ("malformedOwners", Json::Array(malformed)),
                ("errorIsNotReady", true.into()),
                ("missingFieldsNotInitialized", true.into()),
            ]),
        );
    });
}

#[test]
fn actual_owner_death_and_missing_or_malformed_proof_are_not_fresh_initialization() {
    runtime().block_on(async{
 let mut results=Vec::new();
 for mode in["entered","quiesced"]{
   let root=fixture(&format!("protocol-owner-{mode}"));let options=setup(&root,None,None);let helper=SpawnOptions::for_component(&options.artifact_root,options.helper_layout).unwrap();let mut environment=options.environment;
   environment.insert("IVY_CHAT_PROTOCOL_CHILD_NODE".into(),options.node.executable.clone().into_os_string());environment.insert("IVY_CHAT_PROTOCOL_CHILD_NODE_VERSION".into(),options.node.version.clone().into());environment.insert("IVY_CHAT_PROTOCOL_CHILD_ARTIFACT".into(),options.artifact_root.clone().into_os_string());
   environment.insert("IVY_CHAT_PROTOCOL_OWNER_ROOT".into(),root.clone().into_os_string());environment.insert("IVY_CHAT_PROTOCOL_OWNER_MODE".into(),mode.into());
   let child=process::spawn(&Command{executable:"$fixture".into(),args:vec!["auth_protocol::owner_process_fixture".into(),"--exact".into(),"--ignored".into(),"--nocapture".into(),"--test-threads=1".into()],cwd:None,timeout_ms:0},&options.artifact_root,&BTreeMap::from([("$fixture".into(),std::env::current_exe().unwrap())]),LaunchContext{environment,job_launcher:helper.job_launcher,..Default::default()}).await.unwrap();
   let exit=tokio::time::timeout(Duration::from_secs(30),child.completion.wait()).await.unwrap();assert_eq!(exit.exit_code,Some(0),"{}",exit.stderr);assert!(exit.error_code.is_none());assert!(process::owned_process_tree_stopped(&child).await.unwrap());assert!(!lock_held(&root));
   if mode=="entered"{assert!(reopen(&root).is_err());assert!(Store::initialize_synthetic(&root.join("auth.sqlite"),&root.join("whatsapp-owner/executor-lock.sqlite")).is_err());}
   else{let next=ProtocolAccount::launch(reopen(&root).unwrap(),setup(&root,None,None)).await.unwrap();close(&next).await;}
   results.push(Json::object([("mode",mode.into()),("originalBeforeExit",read(root.join("owner-before-exit.json"))),("parentTreeStopped",true.into()),("postRecovery",owner_row(&root))]));
 }
 for corruption in["missing-format","malformed-owner","missing-proof"]{
   let root=fixture(corruption);let owner=ProtocolAccount::launch(store(&root),setup(&root,None,None)).await.unwrap();close(&owner).await;
   let db=Connection::open(root.join("auth.sqlite")).unwrap();match corruption{"missing-format"=>{db.execute("DELETE FROM chat_auth_receipt WHERE key='format'",[]).unwrap();},"malformed-owner"=>{db.execute("UPDATE chat_auth_receipt SET value='broken' WHERE key='owner'",[]).unwrap();},_=>{db.execute("UPDATE chat_auth_receipt SET value=json_set(value,'$.quiescence',null) WHERE key='owner'",[]).unwrap();}}drop(db);
   assert!(reopen(&root).is_err());assert!(Store::initialize_synthetic(&root.join("auth.sqlite"),&root.join("whatsapp-owner/executor-lock.sqlite")).is_err());
 }
 let root=fixture("protocol-missing-receipts");let before=raw(&root.join("auth.sqlite"),"creds","default");assert!(reopen(&root).is_err());assert_eq!(table_count(&root,"chat_auth_receipt"),0);assert_eq!(raw(&root.join("auth.sqlite"),"creds","default"),before);
 capture("owner-death",Json::object([("actualProcessDeaths",Json::Array(results)),("missingOrMalformedProofRefused",true.into()),("ordinaryAuthOpenUnchanged",true.into()),("oldStoreCloseNotInferred",true.into())]));
});
}
