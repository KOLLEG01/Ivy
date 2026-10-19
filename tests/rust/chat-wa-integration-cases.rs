//! Concrete generation/session/commands/listener assembly with a synthetic Hive
//! socket and explicit account/Host boundaries. No WhatsApp protocol is emulated.
use super::{Fixed, Peer, field, oracle, text};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    generation::{ChatGeneration, ChatGenerationConfig, ChatGenerationLease, settled},
    notice_input::NoticeInstructions,
    runtime::ChatTransport,
    whatsapp::{
        commands::DesktopCommands,
        journal::Inbox,
        journal_io::JournalIo,
        journal_owner::Journal,
        messages::Translator,
        runtime::{
            RuntimeAssembly, RuntimeAttached, RuntimeAttachmentFactory, RuntimeCallbacks,
            RuntimeMedia, RuntimeParts, RuntimeResources, RuntimeSocket, RuntimeTimer,
            RuntimeTimers, RuntimeWork, WhatsAppRuntime,
        },
    },
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{canonical, parse},
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions},
    service::{ConnectionOptions, ServiceConnection, ServiceFuture},
};
use std::{
    path::Path,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

struct Media;
impl RuntimeMedia for Media {
    fn command_text(&self, row: &Inbox) -> IvyResult<JsString> {
        Ok(parse(&row.message, usize::MAX)?
            .required("message")?
            .required("conversation")?
            .as_js_string()
            .unwrap()
            .clone())
    }
    fn content(&self, _: &Inbox) -> IvyResult<Json> {
        panic!("This integration case admits commands only")
    }
    fn payload<'a>(
        &'a self,
        _: &'a Inbox,
        _: &'a ChatGenerationLease,
        _: bool,
    ) -> RequestFuture<'a> {
        Box::pin(async { panic!("No native input/media dispatch is authorized by this fixture") })
    }
    fn prune(&self) -> ServiceFuture<()> {
        Box::pin(async { panic!("No retention tick in this fixture") })
    }
}
struct Desktop;
impl DesktopCommands for Desktop {
    fn run<'a>(&'a self, _: &'a JsString, _: &'a str) -> RequestFuture<'a> {
        Box::pin(async { panic!("No desktop action in this fixture") })
    }
}
struct Factory {
    inner: RuntimeAssembly,
    refuse: AtomicBool,
    owners: Mutex<Vec<Weak<dyn RuntimeAttached>>>,
}
impl RuntimeAttachmentFactory for Factory {
    fn create(&self, g: Arc<ChatGeneration>) -> IvyResult<Arc<dyn RuntimeAttached>> {
        if self.refuse.swap(false, Ordering::SeqCst) {
            return Err(IvyError::new(
                "fixture_attachment",
                "Original constructor rejected attachment.",
            ));
        }
        let owner = self.inner.create(g)?;
        self.owners.lock().unwrap().push(Arc::downgrade(&owner));
        Ok(owner)
    }
}
#[derive(Default)]
struct Account {
    flushes: AtomicUsize,
    closes: AtomicUsize,
    close_entered: AtomicBool,
    auth: AtomicUsize,
    lock: AtomicUsize,
    status: Mutex<Vec<Json>>,
    events: Mutex<Vec<Json>>,
}
struct Socket {
    state: Arc<Account>,
    close: watch::Receiver<bool>,
}
impl RuntimeSocket for Socket {
    fn bind(&self, _: RuntimeCallbacks) {}
    fn start(&self) {
        self.state.events.lock().unwrap().push("start".into());
    }
    fn state(&self) -> JsString {
        "fixture-connected".into()
    }
    fn status(&self) -> Json {
        Json::object([
            ("state", "fixture-connected".into()),
            ("connected", true.into()),
            ("online", true.into()),
            ("privacy", true.into()),
            ("lastOnlinePresenceAt", Json::Null),
            ("errorCode", Json::Null),
            ("lastSendMs", Json::Null),
        ])
    }
    fn flush(&self) -> ServiceFuture<()> {
        self.state.flushes.fetch_add(1, Ordering::SeqCst);
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
        self.state.closes.fetch_add(1, Ordering::SeqCst);
        self.state.close_entered.store(true, Ordering::SeqCst);
        let mut gate = self.close.clone();
        let state = self.state.clone();
        Box::pin(async move {
            while !*gate.borrow_and_update() {
                gate.changed().await.unwrap();
            }
            state.events.lock().unwrap().push("socket-closed".into());
            Ok(())
        })
    }
}
struct Timer(Arc<AtomicUsize>);
impl RuntimeTimer for Timer {
    fn clear(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
struct Timers(Arc<AtomicUsize>);
impl RuntimeTimers for Timers {
    fn every(
        &self,
        _: &'static str,
        _: u64,
        _: Arc<dyn Fn() + Send + Sync>,
    ) -> Box<dyn RuntimeTimer> {
        Box::new(Timer(self.0.clone()))
    }
}
async fn quiet(runtime: &WhatsAppRuntime) {
    tokio::time::timeout(Duration::from_secs(30), runtime.settled())
        .await
        .unwrap();
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}
#[tokio::test]
async fn concrete_runtime_assembly_preserves_failed_attachment_and_generation_lifetime() {
    let corpus = oracle();
    let row = &field(corpus, "cases").as_array().unwrap()[0];
    let mut peer = Peer::start().await;
    peer.command(Json::object([
        ("action", "begin".into()),
        ("spec", row.clone()),
    ]))
    .await;
    let root = std::env::temp_dir().join(format!("ivy-rust-wa-assembly-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("chat-bridge.sqlite"),
        STANDARD.decode(text(row, "database")).unwrap(),
    )
    .unwrap();
    let config = Arc::new(ChatGenerationConfig {
        admission: ChatAdmission::new(field(row, "definition")).unwrap(),
        catalogs: ivy_contracts::checked_native_contract::CheckedNativeContracts::new(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        ),
        instructions: parse(
            &String::from_utf8(STANDARD.decode(text(corpus, "instructions")).unwrap()).unwrap(),
            usize::MAX,
        )
        .unwrap()
        .into_js_string()
        .unwrap(),
        notice_instructions: NoticeInstructions::from_json(
            &parse(
                &String::from_utf8(STANDARD.decode(text(corpus, "noticeInstructions")).unwrap())
                    .unwrap(),
                usize::MAX,
            )
            .unwrap(),
        )
        .unwrap(),
        service_node_id: "chat-provider".into(),
        data_root: Some(root.clone()),
        language: "en".into(),
    });
    let clock = Arc::new(Fixed(
        field(corpus, "now").as_f64().unwrap() as i64,
        Instant::now(),
    ));
    let c = ServiceConnection::connect(
        &peer.url,
        "fixture-token",
        ConnectionOptions::new("chat-provider".into()),
        RequestOptions::default().cancellation,
    )
    .await
    .unwrap();
    c.set_generation(7);
    c.set_registry(&ivy_chat_bridge::registry::chat_registry().unwrap())
        .unwrap();
    c.set_ready(true);
    let generation = ChatGeneration::with_clock(config.clone(), c.clone(), clock.clone())
        .await
        .unwrap();
    let journal = Journal::new(JournalIo::open(":memory:").unwrap());
    let wa_config=parse(r#"{"accountId":"account","botPhoneNumber":"+15555550200","senders":[{"phoneNumber":"+15555550201","principalId":"alice","channelId":"channel"}],"since":"2024-01-01T00:00:00Z"}"#,usize::MAX).unwrap();
    let media = Arc::new(Media);
    let factory = Arc::new(Factory {
        inner: RuntimeAssembly {
            journal: journal.clone(),
            config: wa_config.clone(),
            media: media.clone(),
            desktop: Arc::new(Desktop),
            translator: Translator::new(None),
        },
        refuse: AtomicBool::new(false),
        owners: Mutex::new(vec![]),
    });
    let state = Arc::new(Account::default());
    let (release, close) = watch::channel(false);
    let cleared = Arc::new(AtomicUsize::new(0));
    let status = state.clone();
    let auth = state.clone();
    let lock = state.clone();
    let auth_journal = journal.clone();
    let runtime = WhatsAppRuntime::with_dependencies(
        RuntimeParts {
            config: wa_config,
            host_id: "host".into(),
            journal: journal.clone(),
            socket: Arc::new(Socket {
                state: state.clone(),
                close,
            }),
            media,
            factory: factory.clone(),
            translator: Translator::new(None),
            resources: RuntimeResources {
                status: Arc::new(move |value| {
                    status.status.lock().unwrap().push(value);
                    Box::pin(async { Ok(()) })
                }),
                close_auth: Arc::new(move || {
                    assert!(auth_journal.access(|_| Ok(())).is_err());
                    auth.auth.fetch_add(1, Ordering::SeqCst);
                    auth.events.lock().unwrap().push("auth-closed".into());
                    Ok(())
                }),
                close_lock: Arc::new(move || {
                    lock.lock.fetch_add(1, Ordering::SeqCst);
                    lock.events.lock().unwrap().push("lock-closed".into());
                    Ok(())
                }),
            },
        },
        clock.clone(),
        Arc::new(Timers(cleared.clone())),
    );
    runtime.start();
    ChatTransport::attach(&runtime, Some(generation.clone())).unwrap();
    quiet(&runtime).await;
    runtime.status().await.unwrap();
    assert_eq!(
        field(state.status.lock().unwrap().last().unwrap(), "errorCode"),
        &Json::from("chat_main_missing")
    );
    let owner = factory.owners.lock().unwrap()[0].clone();
    assert!(owner.upgrade().is_some());
    journal
        .access(|j| {
            j.accept(
                "command",
                "alice",
                "15555550201@s.whatsapp.net",
                r#"{"message":{"conversation":"/help"}}"#,
                1000,
            )
        })
        .unwrap();
    runtime.run(RuntimeWork::Control);
    quiet(&runtime).await;
    let output = journal
        .access(|j| {
            assert!(j.pending()?.is_empty());
            Ok(j.outgoing()?)
        })
        .unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(
        output[0].text,
        Translator::new(None)
            .translate("help", None)
            .unwrap()
            .to_utf8()
            .unwrap()
    );
    factory.refuse.store(true, Ordering::SeqCst);
    let rejected = ChatTransport::attach(&runtime, Some(generation.clone())).unwrap_err();
    assert_eq!(rejected.code, "fixture_attachment");
    assert!(
        owner.upgrade().is_some(),
        "A failed replacement must retain the old owner"
    );
    let retired = generation.retire();
    assert!(retired.borrow().is_none());
    assert!(generation.accept().is_err());
    ChatTransport::attach(&runtime, None).unwrap();
    quiet(&runtime).await;
    tokio::time::timeout(Duration::from_secs(30), settled(retired))
        .await
        .unwrap()
        .unwrap();
    assert!(
        owner.upgrade().is_none(),
        "Detach must release the real session/listener generation lease"
    );
    let next = ChatGeneration::with_clock(config, c.clone(), clock)
        .await
        .unwrap();
    ChatTransport::attach(&runtime, Some(next.clone())).unwrap();
    quiet(&runtime).await;
    let next_owner = factory.owners.lock().unwrap()[1].clone();
    assert!(next_owner.upgrade().is_some());
    drop(runtime.close());
    super::until(
        || state.close_entered.load(Ordering::SeqCst),
        "native transport close entered",
    )
    .await;
    assert_eq!(cleared.load(Ordering::SeqCst), 8);
    assert_eq!(state.auth.load(Ordering::SeqCst), 0);
    let retired = next.retire();
    assert!(retired.borrow().is_none());
    assert!(journal.access(|_| Ok(())).is_ok());
    release.send_replace(true);
    runtime.close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), settled(retired))
        .await
        .unwrap()
        .unwrap();
    assert!(next_owner.upgrade().is_none());
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(state.auth.load(Ordering::SeqCst), 1);
    assert_eq!(state.lock.load(Ordering::SeqCst), 1);
    assert!(journal.access(|_| Ok(())).is_err());
    c.close();
    let report = peer
        .command(Json::object([("action", "summary".into())]))
        .await;
    assert!(field(&report, "mismatches").as_array().unwrap().is_empty());
    let actual = Json::object([
        ("generations", Json::Number(2.0)),
        ("failedReplacement", Json::String(rejected.code_js())),
        ("helpOutputs", Json::Number(1.0)),
        ("oldOwnerRetainedOnFailure", true.into()),
        ("detachReleasedStore", true.into()),
        ("cancelledCloseObserver", true.into()),
        ("closeReleasedStore", true.into()),
        ("timersCleared", Json::Number(8.0)),
        (
            "statuses",
            Json::Array(state.status.lock().unwrap().clone()),
        ),
        ("events", Json::Array(state.events.lock().unwrap().clone())),
        ("rpc", report),
    ]);
    if let Some(path) = std::env::var_os("IVY_CHAT_WA_INTEGRATION_CAPTURE") {
        std::fs::write(path, canonical(&actual, usize::MAX).unwrap()).unwrap();
    }
    peer.finish().await;
    std::fs::remove_dir_all(&root).unwrap();
}
