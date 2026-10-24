use ivy_chat_bridge::whatsapp::{
    auth::{AuthAccount, AuthStore},
    auth_backend::NativeAuthStore,
    identity::identity,
    journal_io::JournalIo,
    journal_owner::Journal,
    message_wire::message_json,
    runtime::{RuntimeCallbacks, RuntimeSocket},
    socket::WhatsAppSocket,
};
use ivy_contracts::{
    IvyJson as Json, JsString,
    json::{parse, stringify},
};
use ivy_sdk::clock::Clock;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use wacore::{
    net::{
        DisconnectReason, HttpClient, HttpRequest, HttpResponse, Transport, TransportEvent,
        TransportFactory,
    },
    store::traits::{DeviceStore, LidPnMappingEntry, ProtocolStore},
    types::{
        events::{Connected, Disconnected, Event, InboundMessage, ServerAck},
        message::{MessageInfo, MessageSource},
    },
};
use waproto::whatsapp as wa;
use whatsapp_rust::anyhow;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-native-socket-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn oracle(mode: &str, path: &Path) {
    let output = Command::new("node")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args(["tests/rust/fixtures/chat-native-message-wire.mjs", mode])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn typed_native_message_encoding_retains_original_content_hashes_and_protocol_fields() {
    let fixture = Fixture::new();
    oracle("prepare", &fixture.0);
    let expected: Vec<String> =
        serde_json::from_slice(&std::fs::read(fixture.0.join("expected.json")).unwrap()).unwrap();
    for (i, expected) in expected.iter().enumerate() {
        let message = waproto::codec::message_decode(
            &std::fs::read(fixture.0.join(format!("wire-{i}.bin"))).unwrap(),
        )
        .unwrap();
        let encoded = stringify(&message_json(&message).unwrap(), usize::MAX).unwrap();
        assert_eq!(&encoded, expected, "Retained protobuf JSON sample {i}");
        assert_eq!(
            identity(&[encoded.into()]).unwrap(),
            identity(&[expected.clone().into()]).unwrap()
        );
        std::fs::write(
            fixture.0.join(format!("native-{i}.bin")),
            waproto::codec::message_to_vec(&message),
        )
        .unwrap();
    }
    oracle("verify", &fixture.0);
}

struct TestClock {
    now: AtomicI64,
    started: Instant,
}
impl TestClock {
    fn new() -> Self {
        Self {
            now: AtomicI64::new(1_800_000_000_000),
            started: Instant::now(),
        }
    }
}
impl Clock for TestClock {
    fn utc_millis(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
    fn monotonic(&self) -> Instant {
        self.started
    }
}
#[derive(Clone, Default)]
struct OfflineNetwork {
    calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl HttpClient for OfflineNetwork {
    async fn execute(&self, _: HttpRequest) -> anyhow::Result<HttpResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("Offline fixture"))
    }
}
#[async_trait::async_trait]
impl TransportFactory for OfflineNetwork {
    async fn create_transport(
        &self,
    ) -> anyhow::Result<(
        Arc<dyn Transport>,
        whatsapp_rust::async_channel::Receiver<TransportEvent>,
    )> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("Offline fixture"))
    }
}
fn config() -> Json {
    parse(r#"{"accountId":"synthetic-account","botPhoneNumber":"+15555550102","since":"2020-01-01T00:00:00.000Z","senders":[{"phoneNumber":"+15555550101","principalId":"synthetic-principal","channelId":"synthetic-channel"}]}"#, usize::MAX).unwrap()
}
async fn backend(path: &Path, number: &str) -> Arc<NativeAuthStore> {
    let backend = Arc::new(NativeAuthStore::new(
        AuthAccount::acquire(&path.join("auth.sqlite"), &path.join("owner")).unwrap(),
    ));
    backend.create().await.unwrap();
    let mut device = backend.load().await.unwrap().unwrap();
    device.pn = Some(format!("{number}:1@s.whatsapp.net").parse().unwrap());
    device.push_name = "Synthetic native socket".into();
    backend.save(&device).await.unwrap();
    backend
}
async fn socket(
    path: &Path,
) -> (
    WhatsAppSocket,
    Journal,
    Arc<NativeAuthStore>,
    Arc<TestClock>,
    OfflineNetwork,
    Arc<AtomicUsize>,
) {
    let journal = Journal::new(JournalIo::open(path.join("journal.sqlite")).unwrap());
    let backend = backend(path, "15555550102").await;
    let clock = Arc::new(TestClock::new());
    let network = OfflineNetwork::default();
    let socket = WhatsAppSocket::with_network(
        config(),
        journal.clone(),
        backend.clone(),
        path.to_owned(),
        clock.clone(),
        network.clone(),
        network.clone(),
    )
    .await
    .unwrap();
    let wakes = Arc::new(AtomicUsize::new(0));
    let count = wakes.clone();
    socket.bind(RuntimeCallbacks {
        wake: Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }),
        automatic_notifications: Arc::new(|| Box::pin(async { Ok(false) })),
        file_workspace: Arc::new(|_| Box::pin(async { Err(ivy_contracts::IvyError::new("service_not_ready", "No Main in this socket fixture.")) })),
    });
    (socket, journal, backend, clock, network, wakes)
}
fn incoming(id: &str, jid: &str, text: &str, now: i64) -> InboundMessage {
    let info = MessageInfo {
        id: id.into(),
        source: MessageSource {
            chat: jid.parse().unwrap(),
            sender: jid.parse().unwrap(),
            ..Default::default()
        },
        timestamp: wacore::chrono::DateTime::from_timestamp_millis(now).unwrap(),
        ..Default::default()
    };
    InboundMessage::builder()
        .info(Arc::new(info))
        .message(Arc::new(wa::Message {
            conversation: Some(text.into()),
            ..Default::default()
        }))
        .build()
}
fn count(journal: &Journal, table: &str) -> i64 {
    journal
        .access(|j| {
            Ok(j.connection()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap())
        })
        .unwrap()
}

fn file_workspace() -> Json {
    parse(r#"{"expectedBridge":{"principalId":"chat","callerPrincipalId":"synthetic-principal","workspaceId":"main","definitionHash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"binding":{"object":{"objectId":"binding","revision":1}},"channels":[{"channel":{"accountId":"synthetic-account","adapter":"whatsapp","channelId":"synthetic-channel"}}]}"#, usize::MAX).unwrap()
}
fn bind_files(socket: &WhatsAppSocket, workspace: Json) {
    socket.bind(RuntimeCallbacks {
        wake: Arc::new(|| {}),
        automatic_notifications: Arc::new(|| Box::pin(async { Ok(false) })),
        file_workspace: Arc::new(move |caller| {
            assert_eq!(caller, JsString::from("synthetic-principal"));
            let workspace = workspace.clone();
            Box::pin(async move { Ok(workspace) })
        }),
    });
}
fn queue_file(journal: &Journal, operation: &str, now: i64) -> String {
    use ivy_contracts::hash::{digest, hash_json};
    let caller: Json = "synthetic-principal".into();
    let key = format!("file:{}", identity(&[caller.clone(), operation.into()]).unwrap());
    let id = identity(&["file".into(), caller.clone(), operation.into()]).unwrap()[..32].to_ascii_uppercase();
    let cfg = config();
    let workspace = file_workspace();
    let saved = Json::object([
        ("caller", caller), ("expectedBridge", workspace.required("expectedBridge").unwrap().clone()),
        ("requestHash", digest(b"request").into()),
        ("transportHash", hash_json(&Json::object([
            ("account", cfg.required("accountId").unwrap().clone()),
            ("bot", cfg.required("botPhoneNumber").unwrap().clone()),
            ("sender", cfg.required("senders").unwrap().as_array().unwrap()[0].clone())
        ])).unwrap().into()),
        ("bytesBase64", STANDARD.encode(b"original bytes").into()), ("caption", Json::Null),
        ("delivery", Json::object([
            ("operationId", operation.into()), ("binding", workspace.required("binding").unwrap().required("object").unwrap().clone()),
            ("channel", workspace.required("channels").unwrap().as_array().unwrap()[0].required("channel").unwrap().clone()),
            ("file", Json::object([("filename", "note.txt".into()), ("mediaType", "text/plain".into()), ("byteLength", 14.0.into()), ("contentHash", digest(b"original bytes").into())])),
            ("kind", "document".into()), ("messageId", id.as_str().into()), ("state", "queued".into()), ("errorCode", Json::Null),
            ("createdAt", ivy_sdk::clock::date_to_iso(now as f64).unwrap().into()), ("updatedAt", ivy_sdk::clock::date_to_iso(now as f64).unwrap().into())
        ]))
    ]);
    journal.access(|j| {
        j.enqueue_attachment(&id, "15555550101@s.whatsapp.net", "", now)?;
        j.set(&key, &saved)?;
        j.set(&format!("out-file:{id}"), &key.into())
    }).unwrap();
    id
}

#[tokio::test(flavor = "current_thread")]
async fn file_dispatch_survives_cancel_without_resubmission_and_reconciles_recipient_ack() {
    use ivy_chat_bridge::{files::ChatFiles, whatsapp::journal::OutputState};
    let fixture = Fixture::new();
    let (socket, journal, _, clock, network, _) = socket(&fixture.0).await;
    bind_files(&socket, file_workspace());
    let id = queue_file(&journal, "file-original", clock.utc_millis());
    let files = ChatFiles::with_clock(journal.clone(), config(), clock.clone());
    assert_eq!(files.payload(&id).unwrap().bytes, b"original bytes");
    socket.observe(&Event::Connected(Connected::builder().build())).await.unwrap();
    if let Ok(result) = tokio::time::timeout(Duration::from_millis(50), socket.flush()).await { result.unwrap(); }
    let caller: JsString = "synthetic-principal".into();
    let operation: JsString = "file-original".into();
    let receipt = || files.read_operation(&caller, &operation);
    // A cancelled upload is uncertain. Repeated flushes must never upload again.
    for _ in 0..2 { socket.flush().await.unwrap(); }
    assert_eq!(receipt().await.unwrap().required("state").unwrap(), &Json::from("outcome_unknown"));
    let calls = network.calls.load(Ordering::SeqCst);
    socket.observe(&Event::ServerAck(ServerAck::builder().id(id.clone()).class("message".into()).from("15555550109@s.whatsapp.net".parse().unwrap()).build())).await.unwrap();
    assert_eq!(receipt().await.unwrap().required("state").unwrap(), &Json::from("outcome_unknown"));
    socket.observe(&Event::ServerAck(ServerAck::builder().id(id.clone()).class("message".into()).from("15555550101:8@s.whatsapp.net".parse().unwrap()).build())).await.unwrap();
    files.mark(&id, OutputState::Unknown, Some("late_observer")).unwrap();
    assert_eq!(receipt().await.unwrap().required("state").unwrap(), &Json::from("sent"));
    assert!(files.payload(&id).is_err());
    assert_eq!(network.calls.load(Ordering::SeqCst), calls);
    socket.close().await.unwrap();
    journal.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn file_dispatch_refuses_changed_binding_and_corrupt_bytes_before_network() {
    let fixture = Fixture::new();
    let (socket, journal, _, clock, network, _) = socket(&fixture.0).await;
    let changed = queue_file(&journal, "changed", clock.utc_millis());
    let mut workspace = file_workspace();
    workspace.insert("binding", parse(r#"{"object":{"objectId":"binding","revision":2}}"#, usize::MAX).unwrap());
    bind_files(&socket, workspace);
    socket.observe(&Event::Connected(Connected::builder().build())).await.unwrap();
    socket.flush().await.unwrap();
    let files = ivy_chat_bridge::files::ChatFiles::with_clock(journal.clone(), config(), clock.clone());
    let receipt = files.read_operation(&"synthetic-principal".into(), &"changed".into()).await.unwrap();
    assert_eq!(receipt.required("state").unwrap(), &Json::from("failed"));
    assert_eq!(receipt.required("errorCode").unwrap(), &Json::from("chat_file_target_changed"));
    assert!(files.payload(&changed).is_err());
    bind_files(&socket, file_workspace());
    let corrupt = queue_file(&journal, "corrupt", clock.utc_millis());
    let key = journal.get(&format!("out-file:{corrupt}")).unwrap().as_str().unwrap();
    let mut saved = journal.get(&key).unwrap(); saved.insert("bytesBase64", "Y29ycnVwdA==".into()); journal.set(&key, &saved).unwrap();
    socket.flush().await.unwrap();
    let receipt = files.read_operation(&"synthetic-principal".into(), &"corrupt".into()).await.unwrap();
    assert_eq!(receipt.required("errorCode").unwrap(), &Json::from("chat_file_integrity"));
    assert_eq!(network.calls.load(Ordering::SeqCst), 0);
    socket.close().await.unwrap();
    journal.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_socket_admits_durable_exact_senders_and_retained_duplicate_envelopes() {
    let fixture = Fixture::new();
    let (socket, journal, backend, clock, network, wakes) = socket(&fixture.0).await;
    let now = clock.utc_millis();
    let message = incoming("WIRE-1", "15555550101@s.whatsapp.net", "Original", now);
    let mut refused = incoming("GROUP", "group@g.us", "Group", now);
    Arc::make_mut(&mut refused.info).source.is_group = true;
    let mut own = incoming("OWN", "15555550101@s.whatsapp.net", "Own", now);
    Arc::make_mut(&mut own.info).source.is_from_me = true;
    socket
        .receive(&[
            refused,
            own,
            incoming("UNKNOWN", "15555550109@s.whatsapp.net", "Other", now),
            incoming(
                "STALE",
                "15555550101@s.whatsapp.net",
                "Old",
                now - 8 * 86_400_000,
            ),
            incoming(
                "FUTURE",
                "15555550101@s.whatsapp.net",
                "Future",
                now + 300_001,
            ),
        ])
        .await
        .unwrap();
    assert_eq!(count(&journal, "inbox"), 0);
    socket.receive(&[message.clone()]).await.unwrap();
    assert_eq!(count(&journal, "inbox"), 1);
    // Observe the actual disk while the memory view and its owner remain live.
    let disk = rusqlite::Connection::open(fixture.0.join("journal.sqlite")).unwrap();
    assert_eq!(
        disk.query_row("SELECT count(*) FROM inbox", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let id = identity(&[
        "synthetic-account".into(),
        "synthetic-channel".into(),
        "WIRE-1".into(),
    ])
    .unwrap();
    let original = journal
        .access(|j| Ok(j.pending()?.remove(0).message))
        .unwrap();
    backend
        .put_lid_mapping(&LidPnMappingEntry {
            lid: "100000000000001".into(),
            phone_number: "15555550101".into(),
            created_at: 1,
            updated_at: 1,
            learning_source: "fixture".into(),
        })
        .await
        .unwrap();
    let mut alias = incoming("WIRE-1", "100000000000001@lid", "Original", now);
    Arc::make_mut(&mut alias.info).source.sender_alt =
        Some("15555550101@s.whatsapp.net".parse().unwrap());
    socket.receive(&[alias.clone()]).await.unwrap();
    assert_eq!(
        journal
            .access(|j| Ok(j.pending()?.remove(0).message))
            .unwrap(),
        original
    );
    Arc::make_mut(&mut alias.message).conversation = Some("Changed".into());
    socket.receive(&[alias]).await.unwrap();
    assert_eq!(
        socket
            .status()
            .required("errorCode")
            .unwrap()
            .as_str()
            .unwrap(),
        "whatsui_identity_conflict"
    );
    assert_eq!(count(&journal, "inbox"), 1);
    let mut inconsistent = incoming("MISMATCH", "100000000000001@lid", "Refuse", now);
    Arc::make_mut(&mut inconsistent.info).source.sender_alt =
        Some("15555550109@s.whatsapp.net".parse().unwrap());
    socket.receive(&[inconsistent]).await.unwrap();
    assert_eq!(count(&journal, "inbox"), 1);
    journal.access(|j| j.finish(&id)).unwrap();
    socket.receive(&[message]).await.unwrap();
    assert_eq!(count(&journal, "inbox"), 1);
    assert!(wakes.load(Ordering::SeqCst) > 0);
    socket
        .observe(&Event::Connected(Connected::builder().build()))
        .await
        .unwrap();
    socket.read_receipts().await.unwrap();
    assert!(
        journal
            .get(&format!("read-receipt:{id}"))
            .unwrap()
            .as_object()
            .is_some()
    );
    assert_eq!(
        socket
            .status()
            .required("errorCode")
            .unwrap()
            .as_str()
            .unwrap(),
        "whatsapp_read_receipt_failed"
    );
    socket.presence().await.unwrap();
    assert_eq!(
        socket
            .status()
            .required("errorCode")
            .unwrap()
            .as_str()
            .unwrap(),
        "whatsapp_presence_failed"
    );
    assert!(
        socket
            .status()
            .required("lastOnlinePresenceAt")
            .unwrap()
            .is_null()
    );
    assert_eq!(network.calls.load(Ordering::SeqCst), 0);
    socket.close().await.unwrap();
    assert!(
        socket
            .receive(&[incoming("LATE", "15555550101@s.whatsapp.net", "Late", now)])
            .await
            .is_err()
    );
    assert!(backend.load().await.is_err());
    let next =
        AuthAccount::acquire(&fixture.0.join("auth.sqlite"), &fixture.0.join("owner")).unwrap();
    next.close().unwrap();
    journal.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_socket_retains_cancelled_attempts_until_a_correlated_receipt_and_reads_retained_retry_text()
 {
    let fixture = Fixture::new();
    let (socket, journal, backend, clock, network, _) = socket(&fixture.0).await;
    let now = clock.utc_millis();
    let (first, automatic) = journal
        .access(|j| {
            Ok((
                j.enqueue(
                    &"normal".into(),
                    "15555550101@s.whatsapp.net",
                    &"Text".into(),
                    false,
                    now,
                )?[0]
                    .clone(),
                j.enqueue(
                    &"automatic".into(),
                    "15555550101@s.whatsapp.net",
                    &"Notice".into(),
                    true,
                    now,
                )?[0]
                    .clone(),
            ))
        })
        .unwrap();
    socket
        .observe(&Event::Connected(Connected::builder().build()))
        .await
        .unwrap();
    // An unconnected native client waits for login. Cancelling the observer
    // must retain the exact attempt and message ID for the next retry.
    if let Ok(result) = tokio::time::timeout(Duration::from_millis(50), socket.flush()).await {
        result.unwrap();
    }
    assert_eq!(
        journal
            .get(&format!("send-attempt:{first}"))
            .unwrap()
            .required("count")
            .unwrap()
            .as_f64(),
        Some(1.0)
    );
    assert!(
        journal
            .get(&format!("send-attempt:{automatic}"))
            .unwrap()
            .is_null()
    );
    for step in [1, 2] {
        clock.now.store(now + step * 60_000, Ordering::SeqCst);
        if let Ok(result) = tokio::time::timeout(Duration::from_millis(50), socket.flush()).await {
            result.unwrap();
        }
    }
    clock.now.store(now + 180_000, Ordering::SeqCst);
    socket.flush().await.unwrap();
    let state = || {
        journal
            .access(|j| {
                Ok(j.connection()
                    .query_row("SELECT state FROM outgoing WHERE id=?", [&first], |r| {
                        r.get::<_, String>(0)
                    })
                    .unwrap())
            })
            .unwrap()
    };
    assert_eq!(state(), "unknown");
    assert_eq!(
        journal
            .get(&format!("send-attempt:{first}"))
            .unwrap()
            .required("count")
            .unwrap()
            .as_f64(),
        Some(3.0)
    );
    let wrong = ServerAck::builder()
        .id(first.clone())
        .class("message".into())
        .from("15555550109@s.whatsapp.net".parse().unwrap())
        .build();
    socket.observe(&Event::ServerAck(wrong)).await.unwrap();
    assert_eq!(state(), "unknown");
    let other_namespace = ServerAck::builder()
        .id(first.clone())
        .class("message".into())
        .from("15555550101@lid".parse().unwrap())
        .build();
    socket
        .observe(&Event::ServerAck(other_namespace))
        .await
        .unwrap();
    assert_eq!(state(), "unknown");
    let correct = ServerAck::builder()
        .id(first.clone())
        .class("message".into())
        .from("15555550101:8@s.whatsapp.net".parse().unwrap())
        .build();
    socket.observe(&Event::ServerAck(correct)).await.unwrap();
    assert_eq!(state(), "sent");
    let payload = backend
        .take_sent_message("15555550101@s.whatsapp.net", &first)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        waproto::codec::message_decode(&payload)
            .unwrap()
            .conversation
            .as_deref(),
        Some("Text")
    );
    assert!(
        backend
            .take_sent_message("15555550109@s.whatsapp.net", &first)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        backend
            .take_sent_message("15555550101@lid", &first)
            .await
            .unwrap()
            .is_none()
    );
    backend
        .put_lid_mapping(&LidPnMappingEntry {
            lid: "100000000000001".into(),
            phone_number: "15555550101".into(),
            created_at: 1,
            updated_at: 1,
            learning_source: "fixture".into(),
        })
        .await
        .unwrap();
    journal
        .access(|j| {
            j.mark(
                &first,
                ivy_chat_bridge::whatsapp::journal::OutputState::Unknown,
            )
        })
        .unwrap();
    let mapped = ServerAck::builder()
        .id(first.clone())
        .class("message".into())
        .from("100000000000001:8@lid".parse().unwrap())
        .build();
    socket.observe(&Event::ServerAck(mapped)).await.unwrap();
    assert_eq!(state(), "sent");
    let retry = backend
        .take_sent_message("100000000000001@lid", &first)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        waproto::codec::message_decode(&retry)
            .unwrap()
            .conversation
            .as_deref(),
        Some("Text")
    );
    socket
        .observe(&Event::Disconnected(
            Disconnected::builder()
                .reason(DisconnectReason::Unknown)
                .build(),
        ))
        .await
        .unwrap();
    assert_eq!(socket.state(), JsString::from("reconnecting"));
    // No transport was started. Cancelled send waits consumed only the journal
    // attempts above, without touching a real account or endpoint.
    assert_eq!(network.calls.load(Ordering::SeqCst), 0);
    socket.close().await.unwrap();
    socket.close().await.unwrap();
    journal.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_socket_starts_the_real_client_with_offline_network_and_closes_its_owner() {
    let fixture = Fixture::new();
    let (socket, journal, _, _, network, _) = socket(&fixture.0).await;
    assert_eq!(network.calls.load(Ordering::SeqCst), 0);
    socket.start();
    tokio::time::timeout(Duration::from_secs(5), async {
        while network.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), socket.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(socket.state(), JsString::from("stopped"));
    socket.start();
    let account =
        AuthAccount::acquire(&fixture.0.join("auth.sqlite"), &fixture.0.join("owner")).unwrap();
    account.close().unwrap();
    journal.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_socket_refuses_failed_inbound_commits_before_waking_main() {
    let fixture = Fixture::new();
    let (socket, journal, _, clock, _, wakes) = socket(&fixture.0).await;
    let disk = rusqlite::Connection::open(fixture.0.join("journal.sqlite")).unwrap();
    disk.execute_batch("CREATE TRIGGER fail_inbound BEFORE INSERT ON inbox BEGIN SELECT RAISE(ABORT,'injected journal failure'); END;").unwrap();
    let error = socket
        .receive(&[incoming(
            "FAIL",
            "15555550101@s.whatsapp.net",
            "Uncommitted",
            clock.utc_millis(),
        )])
        .await
        .unwrap_err();
    assert_eq!(error.code, "storage_unavailable");
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
    assert_eq!(
        disk.query_row("SELECT count(*) FROM inbox", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(socket.close().await.is_err());
    // Failed shutdown still retires the account; its lock cannot strand the
    // next owner after the journal has reported the failure.
    let account =
        AuthAccount::acquire(&fixture.0.join("auth.sqlite"), &fixture.0.join("owner")).unwrap();
    account.close().unwrap();
    assert!(journal.close().await.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn native_socket_refuses_a_different_persisted_account_before_constructing_the_client() {
    let fixture = Fixture::new();
    let backend = backend(&fixture.0, "15555550999").await;
    let before = AuthStore::open(fixture.0.join("auth.sqlite"))
        .unwrap()
        .get("creds", "default")
        .unwrap();
    let journal = Journal::new(JournalIo::open(fixture.0.join("journal.sqlite")).unwrap());
    let network = OfflineNetwork::default();
    let result = WhatsAppSocket::with_network(
        config(),
        journal.clone(),
        backend.clone(),
        fixture.0.clone(),
        Arc::new(TestClock::new()),
        network.clone(),
        network.clone(),
    )
    .await;
    assert!(matches!(result, Err(error) if error.code == "whatsapp_account_mismatch"));
    assert_eq!(network.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        AuthStore::open(fixture.0.join("auth.sqlite"))
            .unwrap()
            .get("creds", "default")
            .unwrap(),
        before
    );
    backend.close().unwrap();
    journal.close().await.unwrap();
}
