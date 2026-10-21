//! Concrete generation/session/commands/listener assembly with a synthetic Hive
//! socket and explicit account/Host boundaries. No WhatsApp protocol is emulated.
use super::{Fixed, Peer, field, read_oracle, text};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    generation::{ChatGeneration, ChatGenerationConfig, settled},
    notice_input::NoticeInstructions,
    runtime::{ChatService, ChatTransport, WhatsAppServiceParts},
    whatsapp::{
        desktop::{DesktopHost, DesktopInvocation, WhatsAppDesktop},
        journal::Inbox,
        journal_io::JournalIo,
        journal_owner::Journal,
        media::{
            MediaDependencies, MediaFuture, MediaStream, TranscriptionRequest,
            TranscriptionResponse, WhatsAppMedia,
        },
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
    json::{canonical, parse, stringify},
};
use ivy_host_runtime::portable_native_files::NativeHelperLayout;
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions},
    service::{ConnectionOptions, ServiceConnection, ServiceFuture},
};
use std::{
    path::Path,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

fn oracle() -> &'static Json {
    static CORPUS: OnceLock<Json> = OnceLock::new();
    CORPUS.get_or_init(|| read_oracle("whatsapp-oracle", "IVY_CHAT_WA_RUNTIME_ORACLE_CAPTURE"))
}

// Compile the canonical Host module at the required filesystem boundary.
#[allow(dead_code)]
#[path = "../../packages/host-runtime/rust/src/node_path.rs"]
mod host_node_path;

#[derive(Default)]
struct MediaIo {
    calls: Mutex<Vec<Json>>,
}
struct MediaBytes(Option<Vec<u8>>, Arc<MediaIo>);
impl MediaStream for MediaBytes {
    fn next(&mut self) -> MediaFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.0.take()) })
    }
    fn destroy(&mut self) {
        self.1.calls.lock().unwrap().push("destroy".into());
    }
}
struct MediaResponse;
impl TranscriptionResponse for MediaResponse {
    fn ok(&self) -> bool {
        true
    }
    fn json(&mut self) -> RequestFuture<'_> {
        Box::pin(async { Ok(Json::object([("text", "  Fixture voice  ".into())])) })
    }
}
struct MediaAdapter(Arc<MediaIo>);
impl MediaDependencies for MediaAdapter {
    fn filesystem_path(&self, logical: &JsString) -> IvyResult<std::path::PathBuf> {
        host_node_path::node_filesystem_path(logical).map_err(|_| IvyError::internal())
    }
    fn download<'a>(
        &'a self,
        _: &'a Json,
        kind: &'static str,
        timeout: u64,
    ) -> MediaFuture<'a, Box<dyn MediaStream>> {
        Box::pin(async move {
            assert_eq!(timeout, 60000);
            self.0
                .calls
                .lock()
                .unwrap()
                .push(Json::object([("download", kind.into())]));
            Ok(Box::new(MediaBytes(
                Some(if kind == "image" {
                    vec![137, 80, 78, 71, 13, 10, 26, 10]
                } else {
                    b"OggS".to_vec()
                }),
                self.0.clone(),
            )) as Box<dyn MediaStream>)
        })
    }
    fn voice_wav<'a>(&'a self, bytes: &'a [u8], _: Translator) -> MediaFuture<'a, Vec<u8>> {
        Box::pin(async move {
            assert_eq!(bytes, b"OggS");
            self.0.calls.lock().unwrap().push("voice".into());
            ivy_chat_bridge::whatsapp::audio_processing::DecodedAudio {
                channels: &[&[0.0, 0.5, -0.5]],
                sample_rate: 16000.0,
            }
            .encode_mono_pcm16_wav(16000)
            .map_err(|_| IvyError::internal())
        })
    }
    fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> MediaFuture<'_, Box<dyn TranscriptionResponse>> {
        Box::pin(async move {
            assert_eq!(
                request.url,
                "https://speech.invalid/v1/audio/transcriptions"
            );
            assert_eq!(request.timeout_ms, 180000);
            assert!(request.redirect_error);
            assert_eq!(&request.wav[..4], b"RIFF");
            assert_eq!(request.model, JsString::from("fixture-model"));
            self.0.calls.lock().unwrap().push("transcribe".into());
            Ok(Box::new(MediaResponse) as Box<dyn TranscriptionResponse>)
        })
    }
}
struct DesktopIo {
    journal: std::path::PathBuf,
    calls: Mutex<Vec<Json>>,
}
impl DesktopHost for DesktopIo {
    fn platform(&self) -> &'static str {
        "win32"
    }
    fn run(&self, invocation: DesktopInvocation) -> ServiceFuture<()> {
        // Inspect the physical writer's commit before accepting the OS effect.
        let db = rusqlite::Connection::open(&self.journal).unwrap();
        let saved: String = db
            .query_row(
                "SELECT value FROM kv WHERE key='desktop:desktop-command'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            parse(&saved, 1000).unwrap(),
            Json::object([("attempted", true.into())])
        );
        assert_eq!(invocation.executable, "powershell.exe");
        assert_eq!(
            &invocation.arguments[..3],
            &[
                JsString::from("-NoProfile"),
                "-NonInteractive".into(),
                "-EncodedCommand".into()
            ]
        );
        assert_eq!(invocation.timeout_ms, 30000);
        assert!(invocation.windows_hide && invocation.ignored_stdio && !invocation.shell);
        self.calls.lock().unwrap().push(Json::object([
            ("executable", invocation.executable.into()),
            (
                "arguments",
                Json::Array(invocation.arguments.into_iter().map(Json::String).collect()),
            ),
            ("timeoutMs", Json::Number(invocation.timeout_ms as f64)),
            ("physicalAttemptCommitted", true.into()),
        ]));
        Box::pin(async { Ok(()) })
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
    binds: AtomicUsize,
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
    fn bind(&self, _: RuntimeCallbacks) {
        self.state.binds.fetch_add(1, Ordering::SeqCst);
    }
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
async fn component_desktop_assembly(
    root: &Path,
    journal: &Journal,
    config: Json,
    media: Arc<dyn RuntimeMedia>,
    generation: Arc<ChatGeneration>,
) -> Json {
    let artifact = root.join("admitted-component");
    std::fs::create_dir(&artifact).unwrap();
    let artifact = ivy_host_runtime::paths::realpath(&artifact).unwrap();
    let translator = Translator::new(Some(&JsString::from("de")));
    #[cfg(windows)]
    {
        // An unexpected cache miss must fail against an inert qualification
        // image, never reach the installed PowerShell/desktop capability.
        let command_bin = std::env::var_os("IVY_CHAT_DESKTOP_TEST_BIN")
            .expect("Supply the isolated inert powershell.exe command directory");
        // Cargo prepends DLL search directories to PATH for test binaries.
        // Check executable lookup, including cwd, before any valid command.
        let path = std::env::var_os("PATH").unwrap();
        let selected = std::iter::once(std::env::current_dir().unwrap())
            .chain(std::env::split_paths(&path))
            .map(|directory| directory.join("powershell.exe"))
            .find(|candidate| candidate.is_file())
            .map(|candidate| ivy_host_runtime::paths::realpath(&candidate).unwrap());
        assert_eq!(
            selected,
            Some(
                ivy_host_runtime::paths::realpath(
                    &std::path::PathBuf::from(command_bin).join("powershell.exe")
                )
                .unwrap()
            )
        );
        let helper = std::env::var_os("IVY_CHAT_DESKTOP_JOB_HELPER")
            .expect("Supply the matching source-pinned Host ivy-job qualification image");
        let native = artifact.join("dist/native");
        std::fs::create_dir_all(&native).unwrap();
        std::fs::copy(&helper, native.join("ivy-job.exe")).unwrap();
        assert_eq!(
            RuntimeAssembly::for_component(
                journal.clone(),
                config.clone(),
                media.clone(),
                artifact.clone(),
                NativeHelperLayout::Target,
                translator,
            )
            .err()
            .unwrap()
            .code,
            "artifact_missing",
            "Target composition must not select a legacy helper"
        );
        let target = native.join(if cfg!(target_arch = "x86_64") {
            "win32-x64"
        } else {
            "win32-arm64"
        });
        std::fs::create_dir(&target).unwrap();
        std::fs::copy(helper, target.join("ivy-job.exe")).unwrap();
    }
    let assembly = RuntimeAssembly::for_component(
        journal.clone(),
        config,
        media,
        artifact,
        NativeHelperLayout::Target,
        translator,
    )
    .unwrap();
    let attached = assembly.create(generation).unwrap();
    let mut row = Inbox {
        id: "desktop-command".into(),
        sender: "alice".into(),
        jid: "15555550201@s.whatsapp.net".into(),
        message: "{}".into(),
        state: "pending".into(),
        request: None,
        received: 1001,
    };
    // Reuse the physical journal result from the existing probe. Neither this
    // replay nor the attempted fence below may activate a real desktop command.
    let replay = attached.command(&row, &"/desktop ensure".into()).await;
    if cfg!(windows) {
        assert_eq!(
            replay.unwrap(),
            Some(
                Translator::default()
                    .translate("desktop.ensure", None)
                    .unwrap()
            )
        );
    } else {
        assert_eq!(replay.unwrap_err().code, "desktop_unavailable");
    }
    row.id = "component-invalid".into();
    let invalid = attached
        .command(&row, &"/desktop invalid".into())
        .await
        .unwrap_err();
    assert_eq!(invalid.code, "invalid_arguments");
    assert_eq!(
        invalid.message_js(),
        translator.translate("desktop.usage", None).unwrap()
    );
    assert!(journal.get("desktop:component-invalid").unwrap().is_null());
    row.id = "component-unknown".into();
    let attempted = Json::object([("attempted", true.into())]);
    journal
        .set("desktop:component-unknown", &attempted)
        .unwrap();
    journal.flush().await.unwrap();
    let unknown = attached
        .command(&row, &"/desktop restart".into())
        .await
        .unwrap_err();
    assert_eq!(
        unknown.code,
        if cfg!(windows) {
            "desktop_outcome_unknown"
        } else {
            "desktop_unavailable"
        }
    );
    assert_eq!(
        unknown.message_js(),
        translator
            .translate(
                if cfg!(windows) {
                    "desktop.unclear"
                } else {
                    "desktop.platform"
                },
                None
            )
            .unwrap()
    );
    assert_eq!(journal.get("desktop:component-unknown").unwrap(), attempted);
    Json::object([
        ("layout", "target".into()),
        ("missingTargetRejected", Json::Bool(cfg!(windows))),
        ("physicalResultReused", Json::Bool(cfg!(windows))),
        ("attemptFence", unknown.code.into()),
        ("localizedValidation", Json::String(invalid.message_js())),
        ("desktopActivation", false.into()),
    ])
}
async fn component_service_startup(
    root: &Path,
    peer: &mut Peer,
    row: &Json,
    generation: Arc<ChatGenerationConfig>,
    wa_config: Json,
) -> Json {
    peer.command(Json::object([
        ("action", "begin".into()),
        ("spec", row.clone()),
    ]))
    .await;
    let journal = Journal::new(JournalIo::open(root.join("service-whatsapp.sqlite")).unwrap());
    let media: Arc<dyn RuntimeMedia> = Arc::new(WhatsAppMedia::new(
        journal.clone(),
        wa_config.clone(),
        root.to_path_buf(),
        Arc::new(MediaAdapter(Arc::new(MediaIo::default()))),
        Translator::new(Some(&generation.language)),
    ));
    let account = Arc::new(Account::default());
    let (_release, close) = watch::channel(true);
    let parts = || {
        let status = account.clone();
        let auth = account.clone();
        let lock = account.clone();
        let owned_journal = journal.clone();
        WhatsAppServiceParts {
            config: wa_config.clone(),
            journal: journal.clone(),
            socket: Arc::new(Socket {
                state: account.clone(),
                close: close.clone(),
            }),
            media: media.clone(),
            resources: RuntimeResources {
                status: Arc::new(move |value| {
                    status.status.lock().unwrap().push(value);
                    Box::pin(async { Ok(()) })
                }),
                retire_account: Arc::new(move || {
                    let (auth, lock, journal) = (auth.clone(), lock.clone(), owned_journal.clone());
                    Box::pin(async move {
                        assert!(journal.access(|_| Ok(())).is_err());
                        auth.auth.fetch_add(1, Ordering::SeqCst);
                        auth.events.lock().unwrap().push("auth-closed".into());
                        lock.lock.fetch_add(1, Ordering::SeqCst);
                        lock.events.lock().unwrap().push("lock-closed".into());
                        Ok(())
                    })
                }),
            },
            admitted_artifact: root.join("admitted-component"),
            helper_layout: NativeHelperLayout::Target,
        }
    };
    let config = Json::object([
        ("componentId", "chat-bridge".into()),
        ("credential", "fixture-token".into()),
        ("serviceNodeId", "chat-provider".into()),
        ("hostId", "host".into()),
        ("version", "1.0.0".into()),
        ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
        ("publicBaseUrl", peer.url.clone().into()),
        (
            "settings",
            Json::object([
                ("pollMs", Json::Number(60000.0)),
                ("definition", generation.admission.definition().clone()),
                ("whatsappConfigPath", "synthetic-account-lifecycle".into()),
            ]),
        ),
    ]);
    let health = super::Probe::new("component-service-startup".into());
    let mut invalid = config.clone();
    invalid.insert("componentId", "other-component".into());
    let rejected =
        ChatService::start_with_whatsapp(&invalid, generation.clone(), health.health(), parts())
            .await
            .err()
            .unwrap();
    assert_eq!(rejected.code, "invalid_arguments");
    #[cfg(windows)]
    {
        let mut missing = parts();
        missing.admitted_artifact = root.join("service-missing-helper");
        std::fs::create_dir(&missing.admitted_artifact).unwrap();
        assert_eq!(
            ChatService::start_with_whatsapp(
                &config,
                generation.clone(),
                health.health(),
                missing,
            )
            .await
            .err()
            .unwrap()
            .code,
            "artifact_missing"
        );
    }
    assert_eq!(account.binds.load(Ordering::SeqCst), 0);
    assert!(account.events.lock().unwrap().is_empty());
    assert!(health.health.lock().unwrap().is_empty());
    assert!(
        field(
            &peer
                .command(Json::object([("action", "summary".into())]))
                .await,
            "handshakes"
        )
        .as_array()
        .unwrap()
        .is_empty()
    );
    let app = ChatService::start_with_whatsapp(&config, generation, health.health(), parts())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), app.service().wait_ready(None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.binds.load(Ordering::SeqCst), 1);
    assert_eq!(account.events.lock().unwrap()[0], Json::from("start"));
    assert_eq!(
        field(&health.health.lock().unwrap()[0], "details"),
        &Json::from("Connecting ChatBridge and verifying its exact workspace contracts.")
    );
    let first = app.runtime().unwrap();
    let first_lease = first.accept().unwrap();
    journal
        .set("service-shared-owner", &Json::Bool(true))
        .unwrap();
    peer.command(Json::object([("action", "disconnect".into())]))
        .await;
    super::until(
        || {
            app.service().ready()
                && app
                    .runtime()
                    .is_some_and(|g| g.generation() > first.generation())
        },
        "composed service reconnects its real WhatsApp attachment",
    )
    .await;
    assert!(first.accept().is_err());
    let retired = first.retire();
    assert!(
        retired.borrow().is_none(),
        "Accepted old store lease survives reconnect"
    );
    assert_eq!(
        journal.get("service-shared-owner").unwrap(),
        Json::Bool(true)
    );
    assert_eq!(account.binds.load(Ordering::SeqCst), 1);
    assert_eq!(account.closes.load(Ordering::SeqCst), 0);
    drop(first_lease);
    tokio::time::timeout(Duration::from_secs(30), settled(retired))
        .await
        .unwrap()
        .unwrap();
    let current = app.runtime().unwrap();
    let lease = current.accept().unwrap();
    drop(app.close());
    super::until(
        || account.auth.load(Ordering::SeqCst) == 1,
        "composed account close",
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), app.close())
            .await
            .is_err()
    );
    drop(lease);
    tokio::time::timeout(Duration::from_secs(30), app.close())
        .await
        .unwrap()
        .unwrap();
    assert!(current.accept().is_err());
    assert!(journal.access(|_| Ok(())).is_err());
    assert_eq!(account.closes.load(Ordering::SeqCst), 1);
    assert_eq!(account.auth.load(Ordering::SeqCst), 1);
    assert_eq!(account.lock.load(Ordering::SeqCst), 1);
    let report = peer
        .command(Json::object([("action", "summary".into())]))
        .await;
    assert!(field(&report, "mismatches").as_array().unwrap().is_empty());
    Json::object([
        ("admissionBeforeEffects", true.into()),
        (
            "runtimeBindings",
            Json::Number(account.binds.load(Ordering::SeqCst) as f64),
        ),
        ("firstGeneration", Json::Number(first.generation() as f64)),
        (
            "reconnectedGeneration",
            Json::Number(current.generation() as f64),
        ),
        ("sharedJournalRetained", true.into()),
        ("acceptedStoreLeaseRetained", true.into()),
        ("cancelledCloseRetained", true.into()),
        (
            "events",
            Json::Array(account.events.lock().unwrap().clone()),
        ),
        ("health", Json::Array(health.health.lock().unwrap().clone())),
        ("rpc", report),
        ("desktopActivation", false.into()),
    ])
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
    c.set_ready(true);
    let generation = ChatGeneration::with_clock(config.clone(), c.clone(), clock.clone())
        .await
        .unwrap();
    let journal = Journal::new(JournalIo::open(root.join("whatsapp-journal.sqlite")).unwrap());
    let wa_config=parse(r#"{"accountId":"account","botPhoneNumber":"+15555550200","senders":[{"phoneNumber":"+15555550201","principalId":"alice","channelId":"channel"}],"since":"2024-01-01T00:00:00Z","transcription":{"baseUrl":"https://speech.invalid/v1/","apiKey":"fixture-key","model":"fixture-model"}}"#,usize::MAX).unwrap();
    let media_io = Arc::new(MediaIo::default());
    let media = Arc::new(WhatsAppMedia::with_clock(
        journal.clone(),
        wa_config.clone(),
        root.clone(),
        Arc::new(MediaAdapter(media_io.clone())),
        Translator::new(None),
        clock.clone(),
    ));
    let desktop_io = Arc::new(DesktopIo {
        journal: root.join("whatsapp-journal.sqlite"),
        calls: Mutex::new(vec![]),
    });
    let factory = Arc::new(Factory {
        inner: RuntimeAssembly {
            journal: journal.clone(),
            config: wa_config.clone(),
            media: media.clone(),
            desktop: Arc::new(WhatsAppDesktop::new(
                journal.clone(),
                desktop_io.clone(),
                Translator::default(),
            )),
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
            media: media.clone(),
            factory: factory.clone(),
            translator: Translator::new(None),
            resources: RuntimeResources {
                status: Arc::new(move |value| {
                    status.status.lock().unwrap().push(value);
                    Box::pin(async { Ok(()) })
                }),
                retire_account: Arc::new(move || {
                    let (auth, lock, journal) = (auth.clone(), lock.clone(), auth_journal.clone());
                    Box::pin(async move {
                        assert!(journal.access(|_| Ok(())).is_err());
                        auth.auth.fetch_add(1, Ordering::SeqCst);
                        auth.events.lock().unwrap().push("auth-closed".into());
                        lock.lock.fetch_add(1, Ordering::SeqCst);
                        lock.events.lock().unwrap().push("lock-closed".into());
                        Ok(())
                    })
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
    journal
        .access(|j| {
            j.accept(
                "desktop-command",
                "alice",
                "15555550201@s.whatsapp.net",
                r#"{"message":{"conversation":"/desktop ensure"}}"#,
                1001,
            )
        })
        .unwrap();
    runtime.run(RuntimeWork::Control);
    quiet(&runtime).await;
    let desktop_result = Translator::default()
        .translate("desktop.ensure", None)
        .unwrap();
    let desktop_output = journal
        .access(|j| {
            assert!(j.pending()?.is_empty());
            Ok(j.outgoing()?)
        })
        .unwrap();
    assert_eq!(desktop_output.len(), 2);
    assert_eq!(desktop_output[1].text, desktop_result.to_utf8().unwrap());
    assert_eq!(
        journal.get("desktop:desktop-command").unwrap(),
        Json::object([
            ("attempted", true.into()),
            ("result", Json::String(desktop_result)),
        ])
    );
    assert_eq!(desktop_io.calls.lock().unwrap().len(), 1);
    let component_assembly = component_desktop_assembly(
        &root,
        &journal,
        factory.inner.config.clone(),
        media.clone(),
        generation.clone(),
    )
    .await;
    let mut payloads = Vec::new();
    {
        let lease = generation.accept().unwrap();
        for (id, content) in [
            (
                "b".repeat(64),
                Json::object([("conversation", "Actual generation text".into())]),
            ),
            (
                "c".repeat(64),
                parse(
                    r#"{"imageMessage":{"fileLength":8,"mimetype":"image/png"}}"#,
                    usize::MAX,
                )
                .unwrap(),
            ),
            (
                "d".repeat(64),
                parse(
                    r#"{"audioMessage":{"fileLength":1,"fileSha256":"AA==","seconds":1}}"#,
                    usize::MAX,
                )
                .unwrap(),
            ),
        ] {
            let row = Inbox {
                id: id.clone(),
                sender: "alice".into(),
                jid: "15555550201@s.whatsapp.net".into(),
                message: stringify(&Json::object([("message", content)]), usize::MAX).unwrap(),
                state: "pending".into(),
                request: None,
                received: 1000,
            };
            let payload = RuntimeMedia::payload(media.as_ref(), &row, &lease, true)
                .await
                .unwrap();
            assert_eq!(
                RuntimeMedia::payload(media.as_ref(), &row, &lease, true)
                    .await
                    .unwrap(),
                payload
            );
            assert_eq!(journal.get(&format!("payload:{id}")).unwrap(), payload);
            payloads.push(payload);
        }
    }
    assert_eq!(
        field(&payloads[0], "text"),
        &Json::from("Actual generation text")
    );
    assert_eq!(
        field(&payloads[1], "text"),
        &Json::String(
            Translator::new(None)
                .translate("media.image_caption", None)
                .unwrap()
        )
    );
    assert_eq!(
        field(&payloads[2], "text"),
        &Json::String(
            Translator::new(None)
                .translate(
                    "media.transcription_prefix",
                    Some(&Json::object([("transcript", "Fixture voice".into())]))
                )
                .unwrap()
        )
    );
    assert_eq!(
        std::fs::read(
            root.join("whatsapp-media")
                .join("c".repeat(64))
                .join("original")
        )
        .unwrap(),
        [137, 80, 78, 71, 13, 10, 26, 10]
    );
    assert_eq!(
        media_io.calls.lock().unwrap().len(),
        6,
        "Two downloads/destroys, one decoder and one transcription; cached payloads repeat none"
    );
    journal.flush().await.unwrap();
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
    let next = ChatGeneration::with_clock(config.clone(), c.clone(), clock.clone())
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
    // Reconciliation may have finished constructing an owner after transport
    // cleanup. With no accepted jobs, that owner must not pin another store.
    let late = ChatGeneration::with_clock(config.clone(), c.clone(), clock)
        .await
        .unwrap();
    ChatTransport::attach(&runtime, Some(late.clone())).unwrap();
    assert!(factory.owners.lock().unwrap()[2].upgrade().is_none());
    tokio::time::timeout(Duration::from_secs(30), settled(late.retire()))
        .await
        .unwrap()
        .unwrap();
    c.close();
    let report = peer
        .command(Json::object([("action", "summary".into())]))
        .await;
    assert!(field(&report, "mismatches").as_array().unwrap().is_empty());
    let service_startup =
        component_service_startup(&root, &mut peer, row, config, factory.inner.config.clone())
            .await;
    let actual = Json::object([
        ("generations", Json::Number(3.0)),
        ("failedReplacement", Json::String(rejected.code_js())),
        ("helpOutputs", Json::Number(1.0)),
        ("desktopOutputs", Json::Number(1.0)),
        ("componentDesktopAssembly", component_assembly),
        ("componentServiceStartup", service_startup),
        (
            "desktopCalls",
            Json::Array(desktop_io.calls.lock().unwrap().clone()),
        ),
        ("oldOwnerRetainedOnFailure", true.into()),
        ("detachReleasedStore", true.into()),
        ("cancelledCloseObserver", true.into()),
        ("closeReleasedStore", true.into()),
        ("lateAttachmentReleasedStore", true.into()),
        ("timersCleared", Json::Number(8.0)),
        ("mediaPayloads", Json::Array(payloads)),
        (
            "mediaCalls",
            Json::Array(media_io.calls.lock().unwrap().clone()),
        ),
        ("physicalTransportJournal", true.into()),
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
    assert_eq!(
        root.canonicalize().unwrap().parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn remote_image_sdk_publication_reuses_immutable_mutation_after_reply_loss() {
    let corpus = oracle();
    let mut peer = Peer::start().await;
    peer.command(Json::object([
        ("action", "begin".into()),
        (
            "spec",
            Json::object([
                ("label", "remote-media".into()),
                ("remoteMedia", true.into()),
            ]),
        ),
    ]))
    .await;
    let seed = peer
        .command(Json::object([
            ("action", "mediaSeed".into()),
            ("version", "0.154.0".into()),
        ]))
        .await;
    let root = std::env::temp_dir().join(format!("ivy-rust-wa-remote-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("chat-bridge.sqlite"),
        STANDARD.decode(text(&seed, "database")).unwrap(),
    )
    .unwrap();
    let config = Arc::new(ChatGenerationConfig {
        admission: ChatAdmission::new(field(&seed, "definition")).unwrap(),
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
    let c = ServiceConnection::connect(
        &peer.url,
        "fixture-token",
        ConnectionOptions::new("chat-provider".into()),
        RequestOptions::default().cancellation,
    )
    .await
    .unwrap();
    c.set_generation(7);
    c.set_ready(true);
    let generation = ChatGeneration::reconcile(config, c.clone()).await.unwrap();
    let journal = Journal::new(JournalIo::open(root.join("whatsapp.sqlite")).unwrap());
    let io = Arc::new(MediaIo::default());
    let media = WhatsAppMedia::new(
        journal.clone(),
        Json::object(Vec::<(String, Json)>::new()),
        root.clone(),
        Arc::new(MediaAdapter(io.clone())),
        Translator::default(),
    );
    let id = "e".repeat(64);
    let row=Inbox{id:id.clone(),sender:"alice".into(),jid:"15555550201@s.whatsapp.net".into(),message:r#"{"message":{"imageMessage":{"fileLength":8,"mimetype":"image/png","caption":"Remote caption"}}}"#.into(),state:"pending".into(),request:None,received:1000};
    let payload;
    let expected_owner;
    let expected_parent;
    {
        let lease = generation.accept().unwrap();
        let operations = lease.operations().unwrap();
        let native = lease.native().unwrap();
        let main = ivy_chat_bridge::chat_main::ChatMain::new(
            &operations,
            &native,
            &lease.config().notice_instructions,
        )
        .unwrap();
        expected_owner = field(
            &field(&main.queue().main().await.unwrap().value, "conversation"),
            "objectId",
        )
        .clone();
        expected_parent = lease
            .store()
            .root_object_id
            .clone()
            .map_or(Json::Null, Json::from);
        let lost = RuntimeMedia::payload(&media, &row, &lease, false)
            .await
            .unwrap_err();
        assert_eq!(lost.code, "fixture_image_reply_lost");
        assert_eq!(lost.outcome, ivy_contracts::Outcome::Unknown);
        assert!(journal.get(&format!("payload:{id}")).unwrap().is_null());
        payload = RuntimeMedia::payload(&media, &row, &lease, false)
            .await
            .unwrap();
        assert_eq!(
            RuntimeMedia::payload(&media, &row, &lease, false)
                .await
                .unwrap(),
            payload
        );
    }
    assert_eq!(field(&payload, "text"), &Json::from("Remote caption"));
    let image = &field(&payload, "images").as_array().unwrap()[0];
    let bytes = [137, 80, 78, 71, 13, 10, 26, 10];
    assert_eq!(
        field(image, "contentHash"),
        &Json::from(ivy_contracts::hash::digest(&bytes))
    );
    assert_eq!(field(image, "byteLength"), &Json::Number(8.0));
    assert_eq!(
        field(image, "object"),
        &Json::object([
            ("objectId", "remote-image".into()),
            ("revision", Json::Number(1.0))
        ])
    );
    assert_eq!(
        io.calls.lock().unwrap().len(),
        2,
        "One original download/destroy; reply recovery uses the saved bytes"
    );
    journal.flush().await.unwrap();
    journal.close().await.unwrap();
    let disk = Journal::new(JournalIo::open(root.join("whatsapp.sqlite")).unwrap());
    assert_eq!(disk.get(&format!("payload:{id}")).unwrap(), payload);
    disk.close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), settled(generation.retire()))
        .await
        .unwrap()
        .unwrap();
    c.close();
    let report = peer
        .command(Json::object([("action", "summary".into())]))
        .await;
    assert!(field(&report, "mismatches").as_array().unwrap().is_empty());
    let writes = field(&report, "calls")
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c.get("method") == Some(&Json::from("objects.write")))
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 2);
    assert_eq!(field(writes[0], "params"), field(writes[1], "params"));
    let request = field(writes[0], "params");
    let create = field(request, "create");
    assert_eq!(field(create, "ownerObjectId"), &expected_owner);
    assert_eq!(field(create, "parentId"), &expected_parent);
    assert_eq!(
        field(create, "contractKey"),
        &Json::from("chat-bridge/image-png")
    );
    assert_eq!(field(request, "contractVersion"), &Json::from("1.0.0"));
    assert_eq!(
        field(request, "content"),
        &Json::object([
            ("encoding", "base64".into()),
            ("value", STANDARD.encode(bytes).into())
        ])
    );
    assert!(!field(request, "mutationId").as_str().unwrap().is_empty());
    let capture = Json::object([
        ("originalSeed", seed),
        ("payload", payload),
        ("rpc", report),
        ("mediaCalls", Json::Array(io.calls.lock().unwrap().clone())),
        ("replyLossRetainedMutation", true.into()),
        ("physicalJournalReopen", true.into()),
    ]);
    if let Some(p) = std::env::var_os("IVY_CHAT_REMOTE_IMAGE_CAPTURE") {
        std::fs::write(p, canonical(&capture, usize::MAX).unwrap()).unwrap();
    }
    peer.finish().await;
    assert_eq!(
        root.canonicalize().unwrap().parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    std::fs::remove_dir_all(root).unwrap();
}
