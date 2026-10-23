use super::*;
use crate::voice_rpc::control::tests::{Peer, Plan, fixture, hooks};
use ivy_contracts::json::{canonical, parse};
use std::sync::{OnceLock, Weak};

const THREAD: &str = "10000000-0000-4000-8000-000000000001";
const TURN: &str = "10000000-0000-4000-8000-000000000002";
const OLD: &str = "10000000-0000-4000-8000-000000000003";
fn value(text: &str) -> IvyJson {
    parse(text, 4 * 1024 * 1024).unwrap()
}
fn cases() -> IvyJson {
    value(include_str!(
        "../fixtures/phone-voice-preparation-cases.json"
    ))
}
fn scenarios() -> &'static IvyJson {
    static SAVED: OnceLock<IvyJson> = OnceLock::new();
    SAVED.get_or_init(|| {
        value(include_str!(
            "../fixtures/phone-voice-preparation-results.json"
        ))
    })
}

fn same(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        canonical(actual, 4 * 1024 * 1024).unwrap(),
        canonical(expected, 4 * 1024 * 1024).unwrap()
    );
}
fn observation(result: IvyResult<()>) -> IvyJson {
    match result {
        Ok(()) => IvyJson::object([("ok", true.into())]),
        Err(error) => IvyJson::object([
            ("ok", false.into()),
            ("code", error.code_js().into()),
            ("message", error.message_js().into()),
            ("outcome", error.outcome.to_json()),
        ]),
    }
}
async fn timed<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), future)
        .await
        .unwrap()
}

#[test]
fn readiness_requires_text_and_successful_regular_mcp_calls() {
    let mut turn = IvyJson::object([
        ("status", "completed".into()),
        ("items", cases().get("readyItems").unwrap().clone()),
    ]);
    validate_preparation(Some(&turn)).unwrap();
    let mut items = turn.get("items").unwrap().as_array().unwrap().to_vec();
    items
        .last_mut()
        .unwrap()
        .insert("text", " \nREADY\t".into());
    turn.insert("items", IvyJson::Array(items.clone()));
    validate_preparation(Some(&turn)).unwrap();
    for invalid in [
        IvyJson::Array(vec!["READY".into()]),
        true.into(),
        1.0.into(),
        IvyJson::Null,
        "NOT_READY".into(),
    ] {
        let mut bad = items.clone();
        bad.last_mut().unwrap().insert("text", invalid);
        turn.insert("items", IvyJson::Array(bad));
        assert_eq!(
            validate_preparation(Some(&turn)).unwrap_err().code,
            "phone_voice_not_ready"
        );
    }
    for (key, value) in [
        ("result", true.into()),
        ("error", "failed".into()),
        ("result", value(r#"{"isError":true}"#)),
        ("server", "ivy_dev".into()),
    ] {
        let mut bad = items.clone();
        bad[0].insert(key, value);
        turn.insert("items", IvyJson::Array(bad));
        assert!(validate_preparation(Some(&turn)).is_err());
    }
    turn.insert("items", IvyJson::Array(items));
    turn.insert("status", "inProgress".into());
    assert_eq!(
        validate_preparation(Some(&turn)).unwrap_err().code,
        "phone_voice_not_ready"
    );
}

// Instrument the REQUIRED journal boundary. This fixture is not a production
// cache, SQLite implementation or evidence of installed-storage compatibility.
struct Journal {
    cache: Mutex<Option<IvyJson>>,
    attempts: Mutex<Vec<IvyJson>>,
    fail: bool,
}
impl VoicePreparationJournal for Journal {
    fn task_cache<'a>(
        &'a self,
        key: &'a str,
        fingerprint: &'a str,
    ) -> VoiceJournalFuture<'a, Option<IvyJson>> {
        Box::pin(async move {
            assert_eq!(key, TASK_KEY);
            assert!(fingerprint.starts_with("sha256:"));
            Ok(self.cache.lock().unwrap().clone().map(|mut cache| {
                cache.insert("fingerprint", fingerprint.into());
                cache
            }))
        })
    }
    fn retain_task<'a>(&'a self, key: &'a str, cache: IvyJson) -> VoiceJournalFuture<'a, ()> {
        Box::pin(async move {
            assert_eq!(key, TASK_KEY);
            self.attempts.lock().unwrap().push(cache.clone());
            if self.fail {
                return Err(IvyError::new(
                    "fixture_retain_failed",
                    "Scripted durable retain failure.",
                ));
            }
            *self.cache.lock().unwrap() = Some(cache);
            Ok(())
        })
    }
}
fn journal(mode: &str) -> Arc<Journal> {
    let mut cache = IvyJson::object([
        ("fingerprint", "$fingerprint".into()),
        ("threadId", THREAD.into()),
        ("creationOperationId", IvyJson::Null),
    ]);
    if !["new", "persist-failure"].contains(&mode) {
        cache.insert("preparationOperationId", OLD.into());
        cache.insert("preparationTurnId", TURN.into());
        cache.insert(
            "preparationPromptHash",
            if mode == "changed" {
                format!("sha256:{}", "0".repeat(64))
            } else {
                digest(PROMPT)
            }
            .into(),
        );
    }
    Arc::new(Journal {
        cache: Mutex::new(if mode == "missing-cache" {
            None
        } else {
            Some(cache)
        }),
        attempts: Mutex::new(vec![]),
        fail: mode == "persist-failure",
    })
}
fn ready(journal: &Journal) -> IvyJson {
    let cache = journal.cache.lock().unwrap();
    let client = cache
        .as_ref()
        .and_then(|cache| cache.get("preparationOperationId"))
        .cloned()
        .unwrap_or(IvyJson::Null);
    let mut items = vec![IvyJson::object([
        ("type", "userMessage".into()),
        ("clientId", client),
    ])];
    items.extend(
        cases()
            .get("readyItems")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .cloned(),
    );
    IvyJson::object([
        ("id", TURN.into()),
        ("status", "completed".into()),
        ("items", IvyJson::Array(items)),
    ])
}
fn read_reply(journal: &Journal, turn: Option<IvyJson>) -> IvyJson {
    let _ = journal;
    IvyJson::object([(
        "thread",
        IvyJson::object([
            ("id", THREAD.into()),
            ("cwd", "C:\\WORK".into()),
            ("projectId", IvyJson::Null),
            ("ephemeral", false.into()),
            ("status", IvyJson::object([("type", "idle".into())])),
            ("turns", IvyJson::Array(turn.into_iter().collect())),
        ]),
    )])
}
fn normalize(value: &IvyJson, new_id: Option<&JsString>) -> IvyJson {
    match value {
        IvyJson::String(text) if Some(text) == new_id => "$new-operation".into(),
        IvyJson::Array(items) => {
            IvyJson::Array(items.iter().map(|value| normalize(value, new_id)).collect())
        }
        IvyJson::Object(items) => IvyJson::object(items.iter().map(|(key, value)| {
            (
                key.clone(),
                if *key == JsString::from("fingerprint") {
                    "$fingerprint".into()
                } else {
                    normalize(value, new_id)
                },
            )
        })),
        _ => value.clone(),
    }
}

#[tokio::test]
async fn preparation_recovery_and_durable_boundaries_over_native_rpc() {
    let mut actual = vec![];
    for expected in scenarios().as_array().unwrap() {
        let mode = expected.get("mode").unwrap().as_str().unwrap();
        let (control, _, mut peers, _) = fixture(
            vec![Plan::default()],
            value(r#"{"cwd":"C:\\work"}"#),
            hooks(),
        );
        timed(control.open().wait()).await.unwrap();
        let mut peer = timed(peers.recv()).await.unwrap();
        let journal = journal(&mode);
        let preparation = VoicePreparation::new(control.clone(), journal.clone());
        let work = preparation.prepare(THREAD.into());
        let mut calls = vec![];
        let mut reads = 0;
        for _ in expected.get("calls").unwrap().as_array().unwrap() {
            let request = peer.request().await;
            let method = request.get("method").unwrap().as_str().unwrap();
            calls.push(IvyJson::object([
                ("method", method.clone().into()),
                ("params", request.get("params").unwrap().clone()),
                (
                    "requestId",
                    if method == "turn/start" {
                        request.get("id").unwrap().clone()
                    } else {
                        IvyJson::Null
                    },
                ),
            ]));
            let response = match method.as_str() {
                "thread/read" => {
                    reads += 1;
                    let mut turn = ready(&journal);
                    if mode == "failed" {
                        turn.insert("status", "failed".into());
                    }
                    if mode == "bad-ready" {
                        let mut items = turn.get("items").unwrap().as_array().unwrap().to_vec();
                        items.last_mut().unwrap().insert("text", "NOT_READY".into());
                        turn.insert("items", IvyJson::Array(items));
                    }
                    if mode == "continuing" && reads == 1 {
                        turn.insert("status", "inProgress".into());
                    }
                    if mode == "new" || mode == "changed" {
                        if reads > usize::from(mode == "changed") {
                            assert_eq!(journal.attempts.lock().unwrap().len(), 2);
                        }
                    }
                    read_reply(&journal, if mode == "unknown" { None } else { Some(turn) })
                }
                "turn/start" => {
                    assert_eq!(journal.attempts.lock().unwrap().len(), 1);
                    assert_eq!(
                        request.get("id"),
                        journal
                            .cache
                            .lock()
                            .unwrap()
                            .as_ref()
                            .and_then(|cache| cache.get("preparationOperationId"))
                    );
                    IvyJson::object([(
                        "turn",
                        IvyJson::object([("id", TURN.into()), ("status", "completed".into())]),
                    )])
                }
                other => panic!("Unexpected preparation effect {other}"),
            };
            peer.reply(&request, response).await;
        }
        let result = observation(timed(work.wait()).await);
        let attempts = journal.attempts.lock().unwrap().clone();
        let new_id = attempts
            .first()
            .and_then(|cache| cache.get("preparationOperationId"))
            .and_then(IvyJson::as_js_string)
            .cloned();
        let row = IvyJson::object([
            ("mode", mode.into()),
            ("result", result),
            ("calls", IvyJson::Array(calls)),
            ("retainAttempts", IvyJson::Array(attempts)),
            (
                "cache",
                journal
                    .cache
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "pending",
                IvyJson::Number(preparation.0.state.lock().unwrap().pending.len() as f64),
            ),
        ]);
        actual.push(normalize(&row, new_id.as_ref()));
        timed(control.close(Box::pin(async { Ok(()) })).wait())
            .await
            .unwrap();
    }
    let actual = IvyJson::Array(actual);
    same(&actual, scenarios());
}

async fn linked_fixture(
    plans: usize,
) -> (
    VoiceControl,
    VoicePreparation,
    Arc<Journal>,
    tokio::sync::mpsc::UnboundedReceiver<Peer>,
) {
    let slot: Arc<Mutex<Option<Weak<Inner>>>> = Arc::new(Mutex::new(None));
    let mut callbacks = hooks();
    let notify = slot.clone();
    callbacks.events.on_notification = Arc::new(move |event| {
        let owner = notify.lock().unwrap().as_ref().and_then(Weak::upgrade);
        Box::pin(async move {
            if let Some(owner) = owner {
                VoicePreparation(owner).notification(&event);
            }
            Ok(())
        })
    });
    let lost = slot.clone();
    callbacks.events.on_close = Arc::new(move |code| {
        let owner = lost.lock().unwrap().as_ref().and_then(Weak::upgrade);
        Box::pin(async move {
            if let Some(owner) = owner {
                VoicePreparation(owner).connection_lost(&code);
            }
            Ok(())
        })
    });
    let (control, _, peers, _) = fixture(
        (0..plans).map(|_| Plan::default()).collect(),
        value(r#"{"cwd":"C:\\work"}"#),
        callbacks,
    );
    let journal = journal("new");
    let preparation = VoicePreparation::new(control.clone(), journal.clone());
    *slot.lock().unwrap() = Some(Arc::downgrade(&preparation.0));
    (control, preparation, journal, peers)
}

#[tokio::test]
async fn dropped_observers_keep_the_original_early_completion_and_acknowledgement() {
    let (control, preparation, journal, mut peers) = linked_fixture(1).await;
    drop(preparation.prepare(THREAD.into()));
    let observer = preparation.prepare(THREAD.into());
    let mut peer = timed(peers.recv()).await.unwrap();
    let start = peer.request().await;
    assert_eq!(start.get("method"), Some(&"turn/start".into()));
    peer.frame(IvyJson::object([
        ("method", "turn/completed".into()),
        (
            "params",
            IvyJson::object([("threadId", THREAD.into()), ("turn", ready(&journal))]),
        ),
    ]))
    .await;
    timed(async {
        loop {
            if preparation
                .0
                .state
                .lock()
                .unwrap()
                .pending
                .get(&JsString::from(THREAD))
                .is_some_and(|pending| pending.completed.is_some())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    peer.reply(
        &start,
        IvyJson::object([(
            "turn",
            IvyJson::object([("id", TURN.into()), ("status", "inProgress".into())]),
        )]),
    )
    .await;
    let read = peer.request().await;
    assert_eq!(read.get("method"), Some(&"thread/read".into()));
    assert_eq!(journal.attempts.lock().unwrap().len(), 2);
    peer.reply(&read, read_reply(&journal, Some(ready(&journal))))
        .await;
    timed(observer.wait()).await.unwrap();
    assert!(preparation.0.state.lock().unwrap().pending.is_empty());
    timed(control.close(Box::pin(async { Ok(()) })).wait())
        .await
        .unwrap();
}

#[tokio::test]
async fn lost_unacknowledged_preparation_is_reconciled_and_never_started_again() {
    let (control, preparation, journal, mut peers) = linked_fixture(2).await;
    let first = preparation.prepare(THREAD.into());
    let mut peer = timed(peers.recv()).await.unwrap();
    let start = peer.request().await;
    assert_eq!(start.get("method"), Some(&"turn/start".into()));
    control
        .current()
        .unwrap()
        .transport()
        .close("fixture_lost")
        .await;
    assert!(timed(first.wait()).await.is_err());
    assert!(preparation.0.state.lock().unwrap().pending.is_empty());
    let second = preparation.prepare(THREAD.into());
    let mut next = timed(peers.recv()).await.unwrap();
    let read = next.request().await;
    assert_eq!(read.get("method"), Some(&"thread/read".into()));
    next.reply(&read, read_reply(&journal, None)).await;
    assert_eq!(
        timed(second.wait()).await.err().unwrap().code,
        "phone_voice_prepare_unknown"
    );
    assert_eq!(journal.attempts.lock().unwrap().len(), 1);
    timed(control.close(Box::pin(async { Ok(()) })).wait())
        .await
        .unwrap();
}
