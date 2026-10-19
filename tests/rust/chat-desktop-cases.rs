use super::snapshot;
use ivy_chat_bridge::whatsapp::{
    commands::DesktopCommands,
    desktop::{DesktopHost, DesktopInvocation, WhatsAppDesktop},
    journal_io::JournalIo,
    journal_owner::Journal,
    messages::Translator,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{canonical, parse},
};
use ivy_sdk::service::ServiceFuture;
use std::{
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    v.required(k).unwrap()
}
fn capture(v: IvyResult<Json>) -> Json {
    match v {
        Ok(v) => Json::object([("value", v)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.as_str().into()),
            ]),
        )]),
    }
}
fn snap(j: &Journal) -> Json {
    j.access(|io| Ok(snapshot(io))).unwrap()
}
fn invocation(v: DesktopInvocation, j: &Journal) -> Json {
    Json::object([
        ("executable", v.executable.into()),
        (
            "arguments",
            Json::Array(v.arguments.into_iter().map(Json::String).collect()),
        ),
        (
            "options",
            Json::object([
                ("windowsHide", v.windows_hide.into()),
                (
                    "stdio",
                    if v.ignored_stdio {
                        "ignore".into()
                    } else {
                        Json::Null
                    },
                ),
                ("shell", v.shell.into()),
            ]),
        ),
        ("timeoutMs", Json::Number(v.timeout_ms as f64)),
        ("snapshot", snap(j)),
    ])
}
struct Host {
    spec: Json,
    journal: Journal,
    calls: Arc<Mutex<Vec<Json>>>,
}
impl DesktopHost for Host {
    fn platform(&self) -> &'static str {
        match self
            .spec
            .get("platform")
            .and_then(|v| v.as_str().ok())
            .as_deref()
        {
            Some("win32") => "win32",
            Some("darwin") => "darwin",
            _ => "linux",
        }
    }
    fn run(&self, v: DesktopInvocation) -> ServiceFuture<()> {
        self.calls
            .lock()
            .unwrap()
            .push(invocation(v, &self.journal));
        let fail = self
            .spec
            .get("event")
            .and_then(|v| v.as_str().ok())
            .is_some_and(|s| {
                matches!(
                    s.as_str(),
                    "nonzero" | "signal" | "spawn-error" | "timeout" | "error-then-exit"
                )
            });
        Box::pin(async move {
            if fail {
                Err(IvyError::internal())
            } else {
                Ok(())
            }
        })
    }
}
#[tokio::test]
async fn original_fixed_command_cache_and_attempts_match() {
    let out = Command::new("node")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-desktop-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    if let Some(p) = std::env::var_os("IVY_CHAT_DESKTOP_ORACLE_CAPTURE") {
        std::fs::write(p, &out.stdout).unwrap();
    }
    let corpus = parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap();
    let mut actual = vec![];
    for row in f(&corpus, "rows").as_array().unwrap() {
        let spec = f(row, "spec");
        let j = Journal::new(JournalIo::open(":memory:").unwrap());
        let key = spec
            .get("key")
            .and_then(|v| v.as_str().ok())
            .unwrap_or_else(|| "original-key".into());
        if let Some(v) = spec.get("stored") {
            j.set(&format!("desktop:{key}"), v).unwrap();
        }
        let calls = Arc::new(Mutex::new(vec![]));
        let host = Arc::new(Host {
            spec: spec.clone(),
            journal: j.clone(),
            calls: calls.clone(),
        });
        let desktop = WhatsAppDesktop::new(
            j.clone(),
            host,
            Translator::new(spec.get("language").and_then(Json::as_js_string)),
        );
        let mut steps = vec![];
        for action in f(spec, "actions").as_array().unwrap() {
            let result = capture(desktop.run(action.as_js_string().unwrap(), &key).await);
            steps.push(Json::object([("result", result), ("snapshot", snap(&j))]));
        }
        let result = Json::object([
            ("spec", spec.clone()),
            ("steps", Json::Array(steps)),
            ("calls", Json::Array(calls.lock().unwrap().clone())),
        ]);
        actual.push(result.clone());
        if let Some(p) = std::env::var_os("IVY_CHAT_DESKTOP_ACTUAL_CAPTURE") {
            std::fs::write(
                p,
                canonical(&Json::Array(actual.clone()), usize::MAX).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(
            canonical(&result, usize::MAX).unwrap(),
            canonical(row, usize::MAX).unwrap(),
            "{}",
            f(spec, "label").as_str().unwrap()
        );
        j.close().await.unwrap();
    }
    eprintln!("desktop_original_scenarios={}", actual.len());
}
struct HeldHost {
    path: std::path::PathBuf,
    entered: Arc<AtomicUsize>,
    release: watch::Receiver<bool>,
}
impl DesktopHost for HeldHost {
    fn platform(&self) -> &'static str {
        "win32"
    }
    fn run(&self, v: DesktopInvocation) -> ServiceFuture<()> {
        assert_eq!(v.timeout_ms, 30000);
        let db = rusqlite::Connection::open(&self.path).unwrap();
        let raw: String = db
            .query_row(
                "SELECT value FROM kv WHERE key='desktop:physical'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            parse(&raw, 1000).unwrap(),
            Json::object([("attempted", true.into())])
        );
        self.entered.fetch_add(1, Ordering::SeqCst);
        let mut gate = self.release.clone();
        Box::pin(async move {
            while !*gate.borrow_and_update() {
                gate.changed().await.unwrap();
            }
            Ok(())
        })
    }
}
#[tokio::test]
async fn durable_attempt_and_cancelled_observer_preserve_owned_completion() {
    let root = std::env::temp_dir().join(format!("ivy-rust-desktop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("journal.sqlite");
    let entered = Arc::new(AtomicUsize::new(0));
    let j = Journal::new(JournalIo::open(&file).unwrap());
    let (release, gate) = watch::channel(false);
    let desktop = WhatsAppDesktop::new(
        j.clone(),
        Arc::new(HeldHost {
            path: file.clone(),
            entered: entered.clone(),
            release: gate,
        }),
        Translator::default(),
    );
    let owner = desktop.clone();
    let waiting = tokio::spawn(async move { owner.run(&"ensure".into(), "physical").await });
    tokio::time::timeout(Duration::from_secs(10), async {
        while entered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let refused = desktop
        .run(&"restart".into(), "physical")
        .await
        .unwrap_err();
    assert_eq!(refused.code, "desktop_outcome_unknown");
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    release.send_replace(true);
    tokio::time::timeout(Duration::from_secs(10), async {
        while j.get("desktop:physical").unwrap().get("result").is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let cached = desktop.run(&"restart".into(), "physical").await.unwrap();
    assert_eq!(
        cached,
        Json::String(
            Translator::default()
                .translate("desktop.ensure", None)
                .unwrap()
        )
    );
    assert_eq!(entered.load(Ordering::SeqCst), 1);
    j.flush().await.unwrap();
    j.close().await.unwrap();
    let reopened = Journal::new(JournalIo::open(&file).unwrap());
    assert_eq!(
        reopened.get("desktop:physical").unwrap().get("result"),
        Some(&cached)
    );
    reopened.close().await.unwrap();
    // Drop the first observer before the owned task can even poll its flush.
    // The accepted attempt must still progress to launch and save its result.
    let early_file = root.join("early.sqlite");
    let early = Journal::new(JournalIo::open(&early_file).unwrap());
    let early_entered = Arc::new(AtomicUsize::new(0));
    let (early_release, early_gate) = watch::channel(false);
    let early_desktop = WhatsAppDesktop::new(
        early.clone(),
        Arc::new(HeldHost {
            path: early_file,
            entered: early_entered.clone(),
            release: early_gate,
        }),
        Translator::default(),
    );
    let action = JsString::from("ensure");
    let mut observer = early_desktop.run(&action, "physical");
    std::future::poll_fn(|cx| {
        assert!(observer.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(observer);
    assert_eq!(early_entered.load(Ordering::SeqCst), 0);
    tokio::time::timeout(Duration::from_secs(10), async {
        while early_entered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    early_release.send_replace(true);
    tokio::time::timeout(Duration::from_secs(10), async {
        while early
            .get("desktop:physical")
            .unwrap()
            .get("result")
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    early.close().await.unwrap();
    // A failed physical attempt commit refuses launch, even while its memory view
    // already contains the marker.
    let db = rusqlite::Connection::open(&file).unwrap();
    db.execute_batch("DELETE FROM kv; CREATE TRIGGER refuse_desktop BEFORE INSERT ON kv BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    drop(db);
    let failed = Journal::new(JournalIo::open(&file).unwrap());
    let host = Arc::new(Host {
        spec: Json::object([("platform", "win32".into())]),
        journal: failed.clone(),
        calls: Arc::new(Mutex::new(vec![])),
    });
    let desktop = WhatsAppDesktop::new(failed.clone(), host.clone(), Translator::default());
    let error = desktop.run(&"ensure".into(), "physical").await.unwrap_err();
    assert_eq!(error.code, "storage_unavailable");
    assert!(host.calls.lock().unwrap().is_empty());
    assert!(failed.close().await.is_err());
    let result = Json::object([
        ("physicalAttemptBeforeLaunch", true.into()),
        ("cancelledObserverRetainedResult", true.into()),
        ("cancelledBeforeFlushRetainedResult", true.into()),
        ("competingAttemptRefused", true.into()),
        ("cachedResultAfterReopen", cached),
        ("failedWriterRefusedLaunch", true.into()),
    ]);
    if let Some(p) = std::env::var_os("IVY_CHAT_DESKTOP_LIFETIME_CAPTURE") {
        std::fs::write(p, canonical(&result, usize::MAX).unwrap()).unwrap();
    }
    std::fs::remove_dir_all(&root).unwrap();
}
