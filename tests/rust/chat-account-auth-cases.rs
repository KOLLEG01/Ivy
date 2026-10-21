//! The runtime closes its real auth child; protocol/socket work stays synthetic.
use super::auth_node::{account, encode, fixture, launch, lock_held, raw};
use ivy_chat_bridge::{
    generation::{ChatGeneration, ChatGenerationLease},
    whatsapp::{
        auth_node::{AccountAuth, AuthMethod, AuthReply},
        journal::Inbox,
        journal_io::JournalIo,
        journal_owner::Journal,
        messages::Translator,
        runtime::{
            RuntimeAttached, RuntimeAttachmentFactory, RuntimeCallbacks, RuntimeMedia,
            RuntimeParts, RuntimeResources, RuntimeSocket, WhatsAppRuntime,
        },
    },
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome};
use ivy_sdk::{rpc::RequestFuture, service::ServiceFuture};
use std::{fs, path::Path, sync::Arc, time::Duration};

struct CloseOnly;
impl RuntimeSocket for CloseOnly {
    fn bind(&self, _: RuntimeCallbacks) {}
    fn start(&self) {
        panic!("No protocol socket in account composition");
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
        Box::pin(async { Ok(()) })
    }
}
impl RuntimeMedia for CloseOnly {
    fn command_text(&self, _: &Inbox) -> IvyResult<JsString> {
        Err(IvyError::internal())
    }
    fn content(&self, _: &Inbox) -> IvyResult<Json> {
        Err(IvyError::internal())
    }
    fn payload<'a>(
        &'a self,
        _: &'a Inbox,
        _: &'a ChatGenerationLease,
        _: bool,
    ) -> RequestFuture<'a> {
        Box::pin(async { Err(IvyError::internal()) })
    }
    fn prune(&self) -> ServiceFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
impl RuntimeAttachmentFactory for CloseOnly {
    fn create(&self, _: Arc<ChatGeneration>) -> IvyResult<Arc<dyn RuntimeAttached>> {
        Err(IvyError::internal())
    }
}

fn composed(owner: AccountAuth, journal: Journal, root: &Path) -> WhatsAppRuntime {
    let closed_journal = journal.clone();
    let marker = root.join("retirement-invoked");
    WhatsAppRuntime::new(RuntimeParts {
        config: Json::Null,
        host_id: "synthetic-host".into(),
        journal,
        socket: Arc::new(CloseOnly),
        media: Arc::new(CloseOnly),
        factory: Arc::new(CloseOnly),
        translator: Translator::default(),
        resources: RuntimeResources {
            status: Arc::new(|_| Box::pin(async { Ok(()) })),
            retire_account: Arc::new(move || {
                assert!(
                    closed_journal.access(|_| Ok(())).is_err(),
                    "Journal closes first"
                );
                assert!(!marker.exists(), "Runtime invokes account retirement once");
                fs::write(&marker, "retire").unwrap();
                owner.retire()
            }),
        },
    })
}

async fn pending_gate<T>(root: &Path, work: &mut ServiceFuture<T>) {
    tokio::time::timeout(Duration::from_secs(6), async {
        while !root.join("gate.observed").exists() {
            tokio::select! {
                _ = &mut *work => panic!("Completion preceded the actual child gate"),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
        }
    })
    .await
    .expect("Actual child did not reach the fixture gate");
    assert!(
        tokio::time::timeout(Duration::from_millis(40), &mut *work)
            .await
            .is_err()
    );
}
fn trace(root: &Path) -> Vec<Json> {
    fs::read_to_string(root.join("gate.trace"))
        .unwrap()
        .lines()
        .map(|line| ivy_contracts::json::parse(line, usize::MAX).unwrap())
        .collect()
}
fn requests(root: &Path, kind: &str) -> Vec<Json> {
    trace(root)
        .into_iter()
        .filter(|v| {
            v.get("event").and_then(Json::as_js_string) == Some(&JsString::from("request"))
                && v.get("kind").and_then(Json::as_js_string) == Some(&JsString::from(kind))
        })
        .collect()
}
fn capture(name: &str, result: Json) {
    if let Some(base) = std::env::var_os("IVY_CHAT_ACCOUNT_AUTH_CAPTURE") {
        fs::write(
            format!("{}-{name}.json", Path::new(&base).display()),
            encode(&result),
        )
        .unwrap();
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn composed_retirement_drains_entered_work_and_rejects_queued_or_unpolled_calls() {
    runtime().block_on(async {
        let root = fixture("composed-retirement");
        let owner = AccountAuth::launch(
            account(&root),
            31,
            "composed-init-original".into(),
            launch(&root, Some("retire-commit-gate")),
        )
        .await
        .unwrap();
        let journal = Journal::new(JournalIo::open(root.join("runtime.sqlite")).unwrap());
        let runtime = composed(owner.clone(), journal, &root);
        let mut entered = owner.call(
            "composed-entered-original".into(),
            AuthMethod::SetKeys,
            encode(&Json::object([(
                "future-category",
                Json::object([("entered", "committed".into())]),
            )])),
        );
        pending_gate(&root, &mut entered).await;
        assert_eq!(
            raw(&root.join("auth.sqlite"), "future-category", "entered").as_deref(),
            Some("\"committed\"")
        );
        let mut queued = owner.call(
            "composed-queued-original".into(),
            AuthMethod::SetKeys,
            encode(&Json::object([(
                "future-category",
                Json::object([("queued", true.into())]),
            )])),
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(40), &mut queued)
                .await
                .is_err()
        );
        let unpolled = owner.call(
            "composed-unpolled-original".into(),
            AuthMethod::Credentials,
            "null".into(),
        );
        // Invocation must close admission before this observer is ever polled.
        let never_polled = owner.retire();
        let error = owner
            .call(
                "composed-late-original".into(),
                AuthMethod::Credentials,
                "null".into(),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.outcome, Outcome::NotExecuted);
        drop(never_polled);
        // Both runtime and account observers may disappear; their owned cleanup continues.
        drop(runtime.close());
        let mut closed = runtime.close();
        assert!(
            tokio::time::timeout(Duration::from_millis(40), &mut closed)
                .await
                .is_err()
        );
        assert!(root.join("retirement-invoked").exists());
        assert!(lock_held(&root));
        assert!(
            ivy_chat_bridge::whatsapp::auth_node::AuthAccount::acquire(
                &root.join("auth.sqlite"),
                &root.join("whatsapp-owner/executor-lock.sqlite")
            )
            .is_err()
        );
        fs::write(root.join("gate.release"), "continue").unwrap();
        assert!(matches!(entered.await.unwrap(), AuthReply::Value(None)));
        assert_eq!(queued.await.err().unwrap().outcome, Outcome::NotExecuted);
        assert_eq!(unpolled.await.err().unwrap().outcome, Outcome::NotExecuted);
        closed.await.unwrap();
        runtime.close().await.unwrap();
        owner.retire().await.unwrap();
        owner.retire().await.unwrap();
        assert!(!lock_held(&root));
        assert!(raw(&root.join("auth.sqlite"), "future-category", "queued").is_none());
        let calls = requests(&root, "call");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("operationId").and_then(Json::as_js_string),
            Some(&"composed-entered-original".into())
        );
        assert_eq!(requests(&root, "init").len(), 1);
        assert_eq!(requests(&root, "stop").len(), 1);
        capture(
            "queued",
            Json::object([
                ("enteredCommitted", true.into()),
                ("queuedUnpolledLateNotExecuted", true.into()),
                ("droppedObserversDidNotCancel", true.into()),
                ("oneRetirement", true.into()),
                ("lockReleasedAfterCompletion", true.into()),
                ("trace", Json::Array(trace(&root))),
            ]),
        );
    });
}

#[test]
fn composed_retirement_waits_for_stop_ack_and_actual_child_exit() {
    runtime().block_on(async {
        let mut observations = Vec::new();
        for mode in ["retire-stop-gate", "retire-exit-gate"] {
            let root = fixture(mode);
            let owner = AccountAuth::launch(
                account(&root),
                32,
                "stop-init-original".into(),
                launch(&root, Some(mode)),
            )
            .await
            .unwrap();
            drop(owner.retire());
            let mut closed = owner.retire();
            pending_gate(&root, &mut closed).await;
            assert!(lock_held(&root));
            assert_eq!(
                owner
                    .call(
                        "stop-late-original".into(),
                        AuthMethod::Credentials,
                        "null".into()
                    )
                    .await
                    .err()
                    .unwrap()
                    .outcome,
                Outcome::NotExecuted
            );
            assert!(
                ivy_chat_bridge::whatsapp::auth_node::AuthAccount::acquire(
                    &root.join("auth.sqlite"),
                    &root.join("whatsapp-owner/executor-lock.sqlite")
                )
                .is_err()
            );
            fs::write(root.join("gate.release"), "continue").unwrap();
            closed.await.unwrap();
            owner.retire().await.unwrap();
            assert!(!lock_held(&root));
            assert_eq!(requests(&root, "stop").len(), 1);
            observations.push(Json::object([
                ("mode", mode.into()),
                ("pendingOwnershipHeld", true.into()),
                ("normalOwnershipReleased", true.into()),
                ("trace", Json::Array(trace(&root))),
            ]));
        }
        capture("stop-gates", Json::Array(observations));
    });
}

#[test]
fn composed_retirement_caches_unknown_without_replaying_entered_work() {
    runtime().block_on(async {
        let root = fixture("composed-stop-loss");
        let owner = AccountAuth::launch(account(&root), 33, "loss-init-original".into(), launch(&root, Some("retire-stop-loss"))).await.unwrap();
        // A complete original library error is a result, not a nonexecution error.
        assert!(matches!(owner.call("known-library-error-original".into(), AuthMethod::GetKeys, "null".into()).await.unwrap(), AuthReply::LibraryError(ref name) if name=="TypeError"));
        assert!(matches!(owner.call("known-next-original".into(), AuthMethod::Credentials, "null".into()).await.unwrap(), AuthReply::Value(Some(_))));
        let runtime = composed(owner.clone(), Journal::new(JournalIo::open(":memory:").unwrap()), &root);
        let error = runtime.close().await.err().unwrap();
        assert_eq!(error.outcome, Outcome::Unknown);
        for _ in 0..2 {
            let cached = runtime.close().await.err().unwrap();
            assert_eq!((cached.code, cached.message, cached.outcome), (error.code.clone(), error.message.clone(), error.outcome));
            let cached = owner.retire().await.err().unwrap();
            assert_eq!((cached.code, cached.message, cached.outcome), (error.code.clone(), error.message.clone(), error.outcome));
        }
        assert!(lock_held(&root));
        assert_eq!(requests(&root, "init").len(), 1);
        assert_eq!(requests(&root, "stop").len(), 1);

        let cancelled = fixture("composed-cancelled-call");
        let owner = AccountAuth::launch(account(&cancelled), 34, "cancel-init-original".into(), launch(&cancelled, Some("retire-commit-gate"))).await.unwrap();
        let mut entered = owner.call("cancel-entered-original".into(), AuthMethod::SetKeys,
            encode(&Json::object([("future-category", Json::object([("cancelled", "committed".into())]))])));
        pending_gate(&cancelled, &mut entered).await;
        drop(entered);
        fs::write(cancelled.join("gate.release"), "continue").unwrap();
        let error = owner.retire().await.err().unwrap();
        assert_eq!(error.outcome, Outcome::Unknown);
        assert_eq!(owner.retire().await.err().unwrap().outcome, Outcome::Unknown);
        assert!(lock_held(&cancelled));
        assert_eq!(requests(&cancelled, "call").len(), 1);
        assert_eq!(requests(&cancelled, "call")[0].get("operationId").and_then(Json::as_js_string), Some(&"cancel-entered-original".into()));
        for root in [&root, &cancelled] {
            assert!(ivy_chat_bridge::whatsapp::auth_node::AuthAccount::acquire(&root.join("auth.sqlite"),
                &root.join("whatsapp-owner/executor-lock.sqlite")).is_err(), "No second generation after Unknown");
        }
        capture("unknown", Json::object([("completeLibraryErrorPreserved", true.into()), ("stopResultCached", true.into()),
            ("droppedEnteredWorkUnknown", true.into()), ("noReplayOrSecondGeneration", true.into()),
            ("stopTrace", Json::Array(trace(&root))), ("cancelledTrace", Json::Array(trace(&cancelled)))]));
    });
}
