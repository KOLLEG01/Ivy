//! Unbound candidate risks only. No actual socket constructor or account data.
#[path = "../../services/chat-bridge/rust/src/whatsapp/auth_candidate.rs"]
mod candidate;
#[path = "../../services/chat-bridge/rust/src/whatsapp/socket_candidate.rs"]
mod socket_candidate;
use super::auth_node::{fixture, raw};
use candidate::{CandidateAccount, CandidateBudget, Fault, Reply, store};
use ivy_contracts::{
    IvyJson as Json, Outcome,
    hash::digest,
    json::{canonical, parse},
};
use rusqlite::Connection;
use std::{fs, path::Path};
use store::{Grant, Store, TERMINAL_LIMIT, encode, text};

fn open(root: &Path) -> Store {
    Store::adopt_stopped_fixture(
        &root.join("auth.sqlite"),
        &root.join("whatsapp-owner/executor-lock.sqlite"),
    )
    .unwrap()
}
fn enter(store: &mut Store, kind: &str, method: &str, parent: Option<&Grant>) -> Grant {
    let g = store
        .prepare(
            kind,
            method,
            parent.map(|p| (p.id.as_str(), p.token.as_str())),
        )
        .unwrap();
    store.enter(&g.id).unwrap();
    g
}
fn finish(store: &mut Store, g: &Grant) {
    store
        .finish(&g.id, &g.token, "value", &digest(b"null"), None)
        .unwrap();
}
fn initialized(store: &mut Store) -> Grant {
    let init = enter(store, "init", "initialize", None);
    finish(store, &init);
    store.runtime_grant().unwrap()
}
fn count(root: &Path, pattern: &str) -> i64 {
    Connection::open(root.join("auth.sqlite"))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM chat_auth_receipt WHERE key LIKE ?",
            [pattern],
            |r| r.get(0),
        )
        .unwrap()
}
fn callback(store: &mut Store, g: &Grant, encoded: &str) -> Json {
    store
        .callback(
            &g.id,
            &g.token,
            1,
            "set",
            encoded,
            &digest(encoded.as_bytes()),
        )
        .unwrap()
}
fn proof(store: &Store, close: &Grant) -> Json {
    Json::object([
        ("epoch", store.epoch.as_str().into()),
        ("closeOperation", close.id.as_str().into()),
        ("stopAck", true.into()),
        ("treeStopped", true.into()),
        ("protocolDrained", true.into()),
        ("diagnosticsDrained", true.into()),
        ("rootsSettled", true.into()),
        ("callbacksSettled", true.into()),
        ("eventsSettled", true.into()),
        (
            "process",
            Json::object([("exitCode", Json::Number(0.0)), ("errorCode", Json::Null)]),
        ),
    ])
}
fn capture(name: &str, value: Json) {
    if let Some(p) = std::env::var_os("IVY_CHAT_CANDIDATE_CAPTURE") {
        fs::write(
            format!("{}-{name}.json", Path::new(&p).display()),
            encode(&value),
        )
        .unwrap();
    }
}
fn close_store(store: &mut Store) -> Json {
    let c = store.start_close().unwrap();
    let p = proof(store, &c);
    store.quiesce(&c, p.clone()).unwrap();
    store.retire().unwrap();
    p
}

#[test]
fn matched_result_body_is_required_before_root_compaction_credit_or_proof() {
    let root = fixture("candidate-result-body");
    let mut s = open(&root);
    let runtime = initialized(&mut s);
    let command = enter(&mut s, "outbound", "result-pending", Some(&runtime));
    s.hold_result(&command).unwrap();
    let child = enter(&mut s, "repository", "nested.result", Some(&command));
    s.hold_result(&child).unwrap();
    finish(&mut s, &child);
    assert!(
        s.finish(&command.id, &command.token, "value", &digest(b"null"), None)
            .is_err()
    );
    s.release_result(&child).unwrap();
    let write = enter(&mut s, "raw-store", "set", Some(&command));
    callback(
        &mut s,
        &write,
        "[[\"future-category\",\"body-pending\",\"1\"]]",
    );
    finish(&mut s, &write);
    finish(&mut s, &command);
    assert!(s.has_live(&command.id));
    assert_eq!(count(&root, "terminal/%"), 0);
    assert_eq!(count(&root, "cb/%"), 1);
    let close = s.start_close().unwrap();
    assert!(s.quiesce(&close, proof(&s, &close)).is_err());
    s.release_result(&command).unwrap();
    assert!(!s.has_live(&command.id));
    assert_eq!(count(&root, "cb/%"), 0);
    let p = proof(&s, &close);
    s.quiesce(&close, p.clone()).unwrap();
    s.retire().unwrap();
    capture(
        "body-pending",
        Json::object([
            (
                "proofScope",
                "SQL result/continuation gate; not native proof".into(),
            ),
            ("proof", p),
            ("root", root.to_string_lossy().as_ref().into()),
        ]),
    );
}

#[test]
fn nested_returns_pin_details_and_only_frozen_continuations_or_close_cleanup_drain() {
    let root = fixture("candidate-nested");
    let mut s = open(&root);
    let runtime = initialized(&mut s);
    let outer = enter(&mut s, "outbound", "outer.keys.transaction", Some(&runtime));
    let nested = enter(&mut s, "repository", "encryptMessage", Some(&outer));
    let write = enter(&mut s, "raw-store", "set", Some(&nested));
    let batch = "[[\"future-category\",\"candidate\",\"{ \\\"opaque\\\":\\\"\\\\udfff\\\" }\"],[\"future-category\",\"candidate\",\"2\"]]";
    callback(&mut s, &write, batch);
    finish(&mut s, &write);
    finish(&mut s, &nested);
    assert_eq!(
        s.receipt(&nested.id)
            .unwrap()
            .get("settled")
            .and_then(Json::as_bool),
        Some(false)
    );
    assert_eq!(count(&root, &format!("cb/{}/%", write.id)), 1);
    assert_eq!(count(&root, "terminal/%"), 0);
    let expected = digest(
        canonical(
            &Json::object([
                ("previous", digest(b"").into()),
                ("operationId", write.id.as_str().into()),
                ("ordinal", "1".into()),
                ("kind", "set".into()),
                ("requestDigest", digest(batch.as_bytes()).into()),
            ]),
            usize::MAX,
        )
        .unwrap()
        .as_bytes(),
    );
    assert_eq!(
        text(&s.receipt(&write.id).unwrap(), "callbacksDigest").unwrap(),
        expected
    );
    let commit = s.reserve(&outer, "transaction.commit").unwrap();
    let close = s.start_close().unwrap();
    assert_eq!(
        s.prepare(
            "background-prekey",
            "late-timer",
            Some((&runtime.id, &runtime.token))
        )
        .err()
        .unwrap()
        .outcome,
        Outcome::NotExecuted
    );
    assert!(
        s.prepare(
            "raw-store",
            "fresh-with-frozen-parent",
            Some((&outer.id, &outer.token))
        )
        .is_err()
    );
    assert!(s.activate_reserved(&commit, "wrong-purpose").is_err());
    s.activate_reserved(&commit, "transaction.commit").unwrap();
    callback(
        &mut s,
        &commit,
        "[[\"future-category\",\"outer-commit\",\"3\"]]",
    );
    finish(&mut s, &commit);
    assert!(s.quiesce(&close, proof(&s, &close)).is_err());
    finish(&mut s, &outer);
    assert_eq!(count(&root, "cb/%"), 0);
    assert_eq!(
        s.receipt(&nested.id)
            .unwrap()
            .get("settled")
            .and_then(Json::as_bool),
        Some(true)
    );
    let cleanup = s.token_index_cleanup(&close).unwrap();
    assert!(s.token_index_cleanup(&close).is_err());
    s.activate_reserved(&cleanup, "token-index.flush").unwrap();
    callback(
        &mut s,
        &cleanup,
        "[[\"tctoken\",\"index\",\"{\\\"close\\\":true}\"]]",
    );
    finish(&mut s, &cleanup);
    let p = proof(&s, &close);
    s.quiesce(&close, p.clone()).unwrap();
    s.retire().unwrap();
    assert_eq!(
        raw(&root.join("auth.sqlite"), "future-category", "candidate").as_deref(),
        Some("2")
    );
    capture(
        "store-nested",
        Json::object([
            (
                "proofScope",
                "SQL candidate unit seam; not a native child proof".into(),
            ),
            ("proof", p),
            ("nestedReceipt", s.receipt(&nested.id).unwrap_or(Json::Null)),
            ("root", root.to_string_lossy().as_ref().into()),
        ]),
    );
}

#[test]
fn terminal_4096_4097_boundary_pins_unresolved_and_malformed_index_rolls_back() {
    let root = fixture("candidate-retention");
    let mut s = open(&root);
    let runtime = initialized(&mut s);
    let pinned = enter(&mut s, "outbound", "still-live", Some(&runtime));
    let mut first = String::new();
    for n in 0..=TERMINAL_LIMIT {
        let g = enter(&mut s, "raw-store", "set", Some(&runtime));
        if n == 0 {
            first = g.id.clone();
            callback(
                &mut s,
                &g,
                "[[\"future-category\",\"retained-value\",\"1\"]]",
            );
        }
        finish(&mut s, &g);
        if n + 1 == TERMINAL_LIMIT {
            assert!(s.receipt(&first).is_ok());
            assert_eq!(count(&root, "terminal/%"), TERMINAL_LIMIT as i64);
        }
    }
    assert_eq!(count(&root, "terminal/%"), TERMINAL_LIMIT as i64);
    assert_eq!(s.receipt(&first).err().unwrap().outcome, Outcome::Unknown);
    assert_eq!(
        text(&s.receipt(&pinned.id).unwrap(), "state").unwrap(),
        "entered"
    );
    assert_eq!(
        raw(
            &root.join("auth.sqlite"),
            "future-category",
            "retained-value"
        )
        .as_deref(),
        Some("1")
    );
    finish(&mut s, &pinned);
    let db = Connection::open(root.join("auth.sqlite")).unwrap();
    let bad = format!("terminal/{}/00000000000000000000", s.epoch);
    db.execute(
        "INSERT INTO chat_auth_receipt(key,value) VALUES(?,?)",
        (&bad, "\"missing-original-id\""),
    )
    .unwrap();
    let g = enter(&mut s, "raw-store", "set", Some(&runtime));
    callback(
        &mut s,
        &g,
        "[[\"future-category\",\"before-compaction-error\",\"4\"]]",
    );
    assert!(
        s.finish(&g.id, &g.token, "value", &digest(b"null"), None)
            .is_err()
    );
    assert_eq!(
        text(&s.receipt(&g.id).unwrap(), "state").unwrap(),
        "entered"
    );
    assert_eq!(count(&root, &format!("cb/{}/%", g.id)), 1);
    assert_eq!(count(&root, &bad), 1);
    s.fence();
    assert!(
        s.prepare("raw-store", "replay", Some((&runtime.id, &runtime.token)))
            .is_err()
    );
    capture(
        "retention",
        Json::object([
            ("limit", Json::Number(TERMINAL_LIMIT as f64)),
            ("prunedId", first.into()),
            ("unknown", s.owner().unwrap()),
            ("compactionFailureReceipt", s.receipt(&g.id).unwrap()),
            ("root", root.to_string_lossy().as_ref().into()),
        ]),
    );
}

#[test]
fn clean_epoch_carries_exact_anchor_and_unknown_or_synthetic_markers_never_reinitialize() {
    let root = fixture("candidate-reopen");
    let mut a = open(&root);
    initialized(&mut a);
    let epoch = a.epoch.clone();
    let anchor = close_store(&mut a);
    drop(a);
    let mut b = Store::reopen_stopped_fixture(
        &root.join("auth.sqlite"),
        &root.join("whatsapp-owner/executor-lock.sqlite"),
    )
    .unwrap();
    assert_ne!(b.epoch, epoch);
    let db = Connection::open(root.join("auth.sqlite")).unwrap();
    let old: String = db
        .query_row(
            "SELECT value FROM chat_auth_receipt WHERE key=?",
            [format!("epoch/{epoch}")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        parse(&old, usize::MAX)
            .unwrap()
            .required("quiescence")
            .unwrap(),
        &anchor
    );
    initialized(&mut b);
    let second = b.epoch.clone();
    close_store(&mut b);
    drop(b);
    let mut c = Store::reopen_stopped_fixture(
        &root.join("auth.sqlite"),
        &root.join("whatsapp-owner/executor-lock.sqlite"),
    )
    .unwrap();
    assert_eq!(count(&root, "epoch/%"), 1);
    assert_eq!(count(&root, &format!("epoch/{epoch}")), 0);
    assert_eq!(count(&root, &format!("epoch/{second}")), 1);
    initialized(&mut c);
    close_store(&mut c);
    drop(c);
    let prior: String = db
        .query_row(
            "SELECT value FROM chat_auth_receipt WHERE key=?",
            [format!("epoch/{second}")],
            |r| r.get(0),
        )
        .unwrap();
    let mut prior = parse(&prior, usize::MAX).unwrap();
    prior.insert("state", "unknown".into());
    db.execute(
        "UPDATE chat_auth_receipt SET value=? WHERE key=?",
        (encode(&prior), format!("epoch/{second}")),
    )
    .unwrap();
    assert!(
        Store::reopen_stopped_fixture(
            &root.join("auth.sqlite"),
            &root.join("whatsapp-owner/executor-lock.sqlite")
        )
        .is_err()
    );
    assert_eq!(count(&root, &format!("epoch/{second}")), 1);
    let before = raw(&root.join("auth.sqlite"), "creds", "default");
    let owner: String = db
        .query_row(
            "SELECT value FROM chat_auth_receipt WHERE key='owner'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut owner = parse(&owner, usize::MAX).unwrap();
    owner.insert("state", "unknown".into());
    db.execute(
        "UPDATE chat_auth_receipt SET value=? WHERE key='owner'",
        [encode(&owner)],
    )
    .unwrap();
    assert!(
        Store::reopen_stopped_fixture(
            &root.join("auth.sqlite"),
            &root.join("whatsapp-owner/executor-lock.sqlite")
        )
        .is_err()
    );
    assert!(
        Store::adopt_stopped_fixture(
            &root.join("auth.sqlite"),
            &root.join("whatsapp-owner/executor-lock.sqlite")
        )
        .is_err()
    );
    assert_eq!(raw(&root.join("auth.sqlite"), "creds", "default"), before);
    let other = fixture("candidate-synthetic-marker");
    let db = Connection::open(other.join("auth.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE chat_auth_receipt(key TEXT NOT NULL PRIMARY KEY,value TEXT NOT NULL); INSERT INTO chat_auth_receipt VALUES('format','{\"version\":1,\"contract\":\"chat-auth-owner-v1\"}');").unwrap();
    assert!(
        Store::adopt_stopped_fixture(
            &other.join("auth.sqlite"),
            &other.join("whatsapp-owner/executor-lock.sqlite")
        )
        .is_err()
    );
    assert!(
        Store::reopen_stopped_fixture(
            &other.join("auth.sqlite"),
            &other.join("whatsapp-owner/executor-lock.sqlite")
        )
        .is_err()
    );
    capture(
        "reopen",
        Json::object([
            ("anchor", anchor),
            ("unknownPreserved", owner),
            ("root", root.to_string_lossy().as_ref().into()),
        ]),
    );
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn launch(
    root: &Path,
    mode: Option<&str>,
    fault: Option<&str>,
) -> ivy_chat_bridge::whatsapp::auth_node::AuthLaunch {
    let mut launch = super::auth_node::launch(root, None);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for path in [
        "services/chat-bridge/rust/node/protocol-candidate-auth.mjs",
        "tests/rust/fixtures/chat-auth-candidate-worker.mjs",
    ] {
        let target = launch.artifact_root.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(source.join(path), target).unwrap();
    }
    launch.environment.insert(
        "IVY_CHAT_CANDIDATE_GATE".into(),
        root.join("candidate").into_os_string(),
    );
    if let Some(mode) = mode {
        launch
            .environment
            .insert("IVY_CHAT_CANDIDATE_GATE_MODE".into(), mode.into());
    }
    if let Some(fault) = fault {
        launch
            .environment
            .insert("IVY_CHAT_CANDIDATE_FAULT".into(), fault.into());
    }
    launch
}
async fn account(root: &Path, mode: Option<&str>, fault: Option<&str>) -> CandidateAccount {
    CandidateAccount::initialize(
        open(root),
        launch(root, mode, fault),
        CandidateBudget::source_fixture(),
    )
    .ready()
    .await
    .unwrap()
}
fn read(path: impl AsRef<Path>) -> Json {
    parse(&fs::read_to_string(path).unwrap(), usize::MAX).unwrap()
}
fn rows(root: &Path) -> Json {
    let db = Connection::open(root.join("auth.sqlite")).unwrap();
    let mut q = db
        .prepare("SELECT key,value FROM chat_auth_receipt ORDER BY key")
        .unwrap();
    Json::Array(
        q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(|r| {
                let (k, v) = r.unwrap();
                Json::object([("key", k.into()), ("value", parse(&v, usize::MAX).unwrap())])
            })
            .collect(),
    )
}
fn owner_row(root: &Path) -> Json {
    rows(root)
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r.get("key") == Some(&Json::from("owner")))
        .unwrap()
        .required("value")
        .unwrap()
        .clone()
}
fn value(reply: Reply) -> Json {
    match reply {
        Reply::Value(v) => parse(&v, usize::MAX).unwrap(),
        Reply::LibraryError(e) => panic!("Unexpected original library error: {e}"),
    }
}
async fn observed(root: &Path, name: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !root.join(format!("candidate.{name}.observed")).exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Actual candidate child did not reach the declared gate");
}
fn release(root: &Path, name: &str) {
    fs::write(root.join(format!("candidate.{name}.release")), "continue").unwrap();
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
async fn stopped(account: &CandidateAccount) {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while account.cleanup_status().get("ownerWorkerRunning") == Some(&Json::from(true)) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Actual account worker remained active; cleanup is unavailable");
}

#[test]
fn actual_graph_nested_commit_cleanup_and_same_database_reopen() {
    runtime().block_on(async {
        let root = fixture("candidate-native-nested");
        let a = account(&root, None, None).await;
        let (_, surface) = a.call("repository", "surface", Json::Null).await.unwrap();
        let surface = value(surface);
        for flag in ["rawBeforeWrapper", "duplicateRefused", "oneCredsObject"] {
            assert_eq!(surface.get(flag), Some(&Json::from(true)));
        }
        let before = raw(&root.join("auth.sqlite"), "session", "15555550101.1");
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
        let call = a
            .start(
                "outbound",
                "nested",
                Json::object([
                    ("inbound", inbound),
                    (
                        "outbound",
                        Json::object([
                            ("jid", "15555550101:1@s.whatsapp.net".into()),
                            ("data", buffer(b"candidate-reply")),
                        ]),
                    ),
                ]),
            )
            .await
            .unwrap();
        observed(&root, "nested-return").await;
        assert_eq!(
            raw(&root.join("auth.sqlite"), "session", "15555550101.1"),
            before
        );
        assert!(
            raw(
                &root.join("auth.sqlite"),
                "future-category",
                "candidate-buffered"
            )
            .is_none()
        );
        let nested = rows(&root);
        assert!(
            nested
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r
                    .get("value")
                    .is_some_and(|v| v.get("method") == Some(&Json::from("encryptMessage"))
                        && v.get("state") == Some(&Json::from("completed"))
                        && v.get("settled") == Some(&Json::from(false))))
        );
        drop(a.quiesce());
        assert_eq!(
            a.start("outbound", "batch", Json::Array(vec![]))
                .await
                .err()
                .unwrap()
                .outcome,
            Outcome::NotExecuted
        );
        release(&root, "nested-return");
        let result = value(call.result().await.unwrap());
        assert_eq!(result.get("decrypted"), Some(&buffer(b"current-0")));
        let proof = a.quiesce().await.unwrap().value();
        a.retire().await.unwrap();
        stopped(&a).await;
        assert!(!super::auth_node::lock_held(&root));
        assert!(raw(&root.join("auth.sqlite"), "tctoken", "index").is_some());
        assert_ne!(
            raw(&root.join("auth.sqlite"), "session", "15555550101.1"),
            before
        );
        let next = Store::reopen_stopped_fixture(
            &root.join("auth.sqlite"),
            &root.join("whatsapp-owner/executor-lock.sqlite"),
        )
        .unwrap();
        let next = CandidateAccount::initialize(
            next,
            launch(&root, None, None),
            CandidateBudget::source_fixture(),
        )
        .ready()
        .await
        .unwrap();
        let (_, again) = next
            .call("repository", "surface", Json::Null)
            .await
            .unwrap();
        assert_eq!(value(again).get("oneCredsObject"), Some(&Json::from(true)));
        next.quiesce().await.unwrap();
        next.retire().await.unwrap();
        stopped(&next).await;
        capture(
            "native-nested",
            Json::object([
                ("surface", surface),
                ("beforeOuterCommit", nested),
                ("proof", proof),
                ("after", rows(&root)),
                ("root", root.to_string_lossy().as_ref().into()),
            ]),
        );
    });
}

#[test]
fn four_credits_allow_event_reentry_without_a_second_credit_and_refuse_queued_work() {
    runtime().block_on(async {
        let root = fixture("candidate-native-events");
        let a = account(&root, None, None).await;
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let proceed = std::sync::Arc::new(tokio::sync::Notify::new());
        let (selected, seen, wait) = (a.clone(), entered.clone(), proceed.clone());
        a.set_event_sink(std::sync::Arc::new(move |event| {
            let owner = selected.clone();
            let seen = seen.clone();
            let wait = wait.clone();
            Box::pin(async move {
                seen.notify_one();
                wait.notified().await;
                let (id, reply) = owner
                    .call_within(
                        event.operation,
                        "repository",
                        "invoke",
                        Json::object([
                            ("target", "lid".into()),
                            ("method", "getPNForLID".into()),
                            ("arguments", Json::Array(vec!["fixture@lid".into()])),
                        ]),
                    )
                    .await?;
                Ok(Json::object([
                    ("nestedOperation", id.into()),
                    ("lid", value(reply)),
                ]))
            })
        }));
        let mut calls = Vec::new();
        for _ in 0..3 {
            calls.push(a.start("outbound", "gate", Json::Null).await.unwrap());
        }
        let event = a
            .start(
                "inbound-crypto",
                "event",
                Json::object([("type", "inert.event".into())]),
            )
            .await
            .unwrap();
        entered.notified().await;
        let before = a.read(None).await.unwrap();
        let queued = a.start("outbound", "batch", Json::Array(vec![]));
        tokio::pin!(queued);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(40), &mut queued)
                .await
                .is_err()
        );
        assert_eq!(
            a.read(None).await.unwrap().get("nextOperation"),
            before.get("nextOperation")
        );
        drop(queued);
        proceed.notify_one();
        let event = value(event.result().await.unwrap());
        drop(a.quiesce());
        release(&root, "work");
        for call in calls {
            assert_eq!(
                value(call.result().await.unwrap()).get("done"),
                Some(&Json::from(true))
            );
        }
        let proof = a.quiesce().await.unwrap().value();
        a.retire().await.unwrap();
        stopped(&a).await;
        capture(
            "native-events",
            Json::object([
                ("event", event),
                ("proof", proof),
                ("queuedBeforeDispatch", before),
                ("root", root.to_string_lossy().as_ref().into()),
            ]),
        );
    });
}

#[test]
fn actual_pending_commit_ack_and_dropped_close_waiter_keep_original_cleanup() {
    runtime().block_on(async {
        let root = fixture("candidate-native-ack");
        let a = account(&root, Some("ack"), None).await;
        let call = a
            .start(
                "outbound",
                "batch",
                Json::Array(vec![Json::Array(vec![
                    "future-category".into(),
                    "ack-pending".into(),
                    "7".into(),
                ])]),
            )
            .await
            .unwrap();
        observed(&root, "ack").await;
        assert_eq!(
            raw(&root.join("auth.sqlite"), "future-category", "ack-pending").as_deref(),
            Some("7")
        );
        drop(a.quiesce());
        assert!(super::auth_node::lock_held(&root));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(40), a.quiesce())
                .await
                .is_err()
        );
        release(&root, "ack");
        call.result().await.unwrap();
        let proof = a.quiesce().await.unwrap().value();
        a.retire().await.unwrap();
        stopped(&a).await;
        assert_eq!(a.quiesce().await.unwrap().value(), proof);
        assert!(!super::auth_node::lock_held(&root));
        capture(
            "native-ack",
            Json::object([
                ("proof", proof),
                ("cleanup", a.cleanup_status()),
                ("root", root.to_string_lossy().as_ref().into()),
            ]),
        );
    });
}

#[test]
fn loss_after_terminal_compaction_and_unproved_stop_never_replay_or_release() {
    runtime().block_on(async {
        let root = fixture("candidate-native-retention-loss");
        let a = account(&root, None, None).await;
        a.fault(Fault::RetentionAfter).await;
        let call = a
            .start(
                "outbound",
                "batch",
                Json::Array(vec![Json::Array(vec![
                    "future-category".into(),
                    "retention-loss".into(),
                    "8".into(),
                ])]),
            )
            .await
            .unwrap();
        let id = call.id.clone();
        assert_eq!(call.result().await.err().unwrap().outcome, Outcome::Unknown);
        stopped(&a).await;
        assert_eq!(
            raw(
                &root.join("auth.sqlite"),
                "future-category",
                "retention-loss"
            )
            .as_deref(),
            Some("8")
        );
        assert!(super::auth_node::lock_held(&root));
        assert_eq!(owner_row(&root).get("state"), Some(&Json::from("unknown")));
        assert!(
            a.start("outbound", "batch", Json::Array(vec![]))
                .await
                .is_err()
        );
        let mut outcomes = vec![Json::object([
            ("originalOperation", id.into()),
            ("rows", rows(&root)),
            ("cleanup", a.cleanup_status()),
            ("root", root.to_string_lossy().as_ref().into()),
        ])];
        for fault in ["stop-ack", "late-stop"] {
            let root = fixture("candidate-native-stop-loss");
            let a = account(&root, None, Some(fault)).await;
            assert!(a.quiesce_settled().await.is_err());
            stopped(&a).await;
            assert!(a.retire().await.is_err());
            assert!(super::auth_node::lock_held(&root));
            assert!(
                raw(
                    &root.join("auth.sqlite"),
                    "future-category",
                    "late-after-stop"
                )
                .is_none()
            );
            outcomes.push(Json::object([
                ("fault", fault.into()),
                ("rows", rows(&root)),
                ("cleanup", a.cleanup_status()),
                ("root", root.to_string_lossy().as_ref().into()),
            ]));
        }
        capture("native-loss", Json::Array(outcomes));
    });
}

#[test]
fn absolute_initialization_and_cooperative_close_decisions_keep_actual_owner_state() {
    runtime().block_on(async {
        let root = fixture("candidate-native-init-deadline");
        let started = std::time::Instant::now();
        let attempt = CandidateAccount::initialize(
            open(&root),
            launch(&root, Some("ready"), None),
            CandidateBudget::source_fixture(),
        );
        observed(&root, "ready").await;
        assert_eq!(
            attempt.ready().await.err().unwrap().outcome,
            Outcome::Unknown
        );
        let initial_ms = started.elapsed().as_millis();
        let at_decision = attempt.cleanup_status();
        assert!(super::auth_node::lock_held(&root));
        assert!(attempt.ready().await.is_err());
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while attempt.cleanup_status().get("ownerWorkerRunning") == Some(&Json::from(true)) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let close_root = fixture("candidate-native-close-deadline");
        let a = account(&close_root, Some("cleanup"), None).await;
        let close_started = std::time::Instant::now();
        let close = a.quiesce();
        observed(&close_root, "cleanup").await;
        assert!(close.await.is_err());
        let close_ms = close_started.elapsed().as_millis();
        let closing_decision = a.cleanup_status();
        a.quiesce_settled().await.unwrap_err();
        stopped(&a).await;
        assert!(super::auth_node::lock_held(&close_root));
        assert!(a.retire().await.is_err());
        capture(
            "native-deadlines",
            Json::object([
                ("initialDecisionMs", Json::Number(initial_ms as f64)),
                ("initialAtDecision", at_decision),
                ("initialFinal", attempt.cleanup_status()),
                ("closeDecisionMs", Json::Number(close_ms as f64)),
                ("closeAtDecision", closing_decision),
                ("closeFinal", a.cleanup_status()),
                ("initialRoot", root.to_string_lossy().as_ref().into()),
                ("closeRoot", close_root.to_string_lossy().as_ref().into()),
            ]),
        );
    });
}

#[test]
fn rust_socket_policy_keeps_journal_identity_attempts_contact_and_r1_order() {
    runtime().block_on(async {
        use ivy_chat_bridge::whatsapp::{
            identity::identity,
            journal_io::JournalIo,
            journal_owner::Journal,
            messages::Translator,
            runtime::{
                RuntimeCallbacks, RuntimeParts, RuntimeResources, RuntimeSocket, WhatsAppRuntime,
            },
        };
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let root = fixture("candidate-native-policy");
        let a = account(&root, None, None).await;
        let journal = Journal::new(JournalIo::open(root.join("transport.sqlite")).unwrap());
        let config = Json::object([
            ("accountId", "fixture-account".into()),
            ("botPhoneNumber", "+15555550100".into()),
            ("since", "2000-01-01T00:00:00Z".into()),
            (
                "senders",
                Json::Array(vec![Json::object([
                    ("phoneNumber", "+15555550101".into()),
                    ("principalId", "fixture-user".into()),
                    ("channelId", "fixture-channel".into()),
                ])]),
            ),
        ]);
        let pairing = root.join("pairing-events.jsonl");
        let pairing_capture = pairing.clone();
        let socket = socket_candidate::CandidateSocket::for_inert_source_fixture(
            a.clone(),
            journal.clone(),
            config.clone(),
            "15555550100:2@s.whatsapp.net".into(),
            Arc::new(ivy_sdk::clock::SystemClock),
            Arc::new(move |value| {
                let path = pairing_capture.clone();
                Box::pin(async move {
                    use std::io::Write;
                    let mut f = fs::OpenOptions::new()
                        .append(true)
                        .create(true)
                        .open(path)
                        .unwrap();
                    writeln!(f, "{}", encode(&value)).unwrap();
                    Ok(())
                })
            }),
        );
        let contact = Arc::new(AtomicBool::new(false));
        let policy = contact.clone();
        socket.bind(RuntimeCallbacks {
            wake: Arc::new(|| {}),
            automatic_notifications: Arc::new(move || {
                let allowed = policy.load(Ordering::Acquire);
                Box::pin(async move { Ok(allowed) })
            }),
        });
        socket.connect().await.unwrap();
        assert_eq!(socket.state(), "connected".into());
        let now = ivy_sdk::clock::Clock::utc_millis(&ivy_sdk::clock::SystemClock);
        let message = Json::object([
            (
                "key",
                Json::object([
                    ("id", "original-inbound".into()),
                    ("remoteJid", "15555550101:4@s.whatsapp.net".into()),
                ]),
            ),
            ("messageTimestamp", Json::Number((now / 1000) as f64)),
            (
                "message",
                Json::object([("conversation", "inbound fixture".into())]),
            ),
        ]);
        let upsert = |message: Json| {
            Json::object([
                ("type", "messages.upsert".into()),
                ("eventType", "notify".into()),
                ("messages", Json::Array(vec![message])),
            ])
        };
        a.call("inbound-crypto", "event", upsert(message.clone()))
            .await
            .unwrap();
        let id = identity(&[
            "fixture-account".into(),
            "fixture-channel".into(),
            "original-inbound".into(),
        ])
        .unwrap();
        assert_eq!(journal.access(|j| j.pending()).unwrap().len(), 1);
        // An envelope change preserves the original content identity; a changed body does not.
        let mut envelope = message.clone();
        let mut key = envelope.required("key").unwrap().clone();
        key.insert("remoteJid", "fixture@lid".into());
        key.insert("remoteJidAlt", "15555550101@s.whatsapp.net".into());
        envelope.insert("key", key);
        a.call("inbound-crypto", "event", upsert(envelope.clone()))
            .await
            .unwrap();
        assert_eq!(journal.access(|j| j.pending()).unwrap().len(), 1);
        envelope.insert(
            "message",
            Json::object([("conversation", "changed body".into())]),
        );
        a.call("inbound-crypto", "event", upsert(envelope))
            .await
            .unwrap();
        assert_eq!(
            socket.status().get("errorCode"),
            Some(&Json::from("whatsui_identity_conflict"))
        );
        let mut old = message.clone();
        old.insert("messageTimestamp", Json::Number(1.));
        a.call("inbound-crypto", "event", upsert(old))
            .await
            .unwrap();
        assert_eq!(journal.access(|j| j.pending()).unwrap().len(), 1);
        let (manual, automatic) = journal
            .access(|j| {
                let manual = j.enqueue(
                    &"manual".into(),
                    "15555550101@s.whatsapp.net",
                    &"manual output".into(),
                    false,
                    now,
                )?;
                let automatic = j.enqueue(
                    &"automatic".into(),
                    "15555550101@s.whatsapp.net",
                    &"automatic output".into(),
                    true,
                    now,
                )?;
                Ok((manual[0].clone(), automatic[0].clone()))
            })
            .unwrap();
        let key = Json::object([
            ("id", manual.as_str().into()),
            ("remoteJid", "15555550101:7@c.us".into()),
        ]);
        assert_eq!(
            value(
                a.call("inbound-crypto", "get-message", key.clone())
                    .await
                    .unwrap()
                    .1
            )
            .get("conversation"),
            Some(&Json::from("manual output"))
        );
        let mut wrong = key.clone();
        wrong.insert("remoteJid", "15555550102@s.whatsapp.net".into());
        assert!(
            value(
                a.call("inbound-crypto", "get-message", wrong)
                    .await
                    .unwrap()
                    .1
            )
            .is_null()
        );
        socket.flush().await.unwrap();
        assert!(journal.access(|j| j.sent(&[manual.clone()])).unwrap());
        assert!(!journal.access(|j| j.sent(&[automatic.clone()])).unwrap());
        assert_eq!(
            journal
                .get(&format!("send-attempt:{manual}"))
                .unwrap()
                .get("count"),
            Some(&Json::Number(1.))
        );
        let receipt = rows(&root)
            .as_array()
            .unwrap()
            .iter()
            .find(|r| {
                r.get("value").is_some_and(|v| {
                    v.get("callerOperationId")
                        == Some(&Json::from(format!("send-attempt:{manual}:1")))
                })
            })
            .unwrap()
            .clone();
        contact.store(true, Ordering::Release);
        socket.flush().await.unwrap();
        assert!(journal.access(|j| j.sent(&[automatic.clone()])).unwrap());
        socket.read_receipts().await.unwrap();
        assert!(
            journal
                .get(&format!("read-receipt:{id}"))
                .unwrap()
                .is_null()
        );
        socket.set_busy(vec!["15555550101@s.whatsapp.net".into()]);
        socket.presence().await.unwrap();
        socket.set_busy(vec![]);
        socket.presence().await.unwrap();
        let policy_status = socket.status();
        assert_eq!(policy_status.get("connected"), Some(&Json::from(true)));
        // Compose through the existing R1 runtime. Its actual journal closes before retirement.
        let closed = journal.clone();
        let retire = a.clone();
        let marker = root.join("retire-policy");
        let mark = marker.clone();
        let runtime = WhatsAppRuntime::new(RuntimeParts {
            config,
            host_id: "fixture-host".into(),
            journal: journal.clone(),
            socket: Arc::new(socket.clone()),
            media: Arc::new(super::account_auth::CloseOnly),
            factory: Arc::new(super::account_auth::CloseOnly),
            translator: Translator::default(),
            resources: RuntimeResources {
                status: Arc::new(|_| Box::pin(async { Ok(()) })),
                retire_account: Arc::new(move || {
                    assert!(closed.access(|_| Ok(())).is_err());
                    assert!(!mark.exists());
                    fs::write(&mark, "one original retirement").unwrap();
                    retire.retire()
                }),
            },
        });
        drop(runtime.close());
        runtime.close().await.unwrap();
        stopped(&a).await;
        assert!(marker.exists());
        assert!(!super::auth_node::lock_held(&root));
        assert_eq!(socket.state(), "stopped".into());
        let trace = fs::read_to_string(root.join("candidate.trace")).unwrap();
        for expected in ["sendMessage", "readMessages", "composing", "paused"] {
            assert!(
                trace.contains(expected),
                "Missing original inert wire call {expected}"
            );
        }
        capture(
            "native-policy",
            Json::object([
                ("inertOnly", true.into()),
                ("statusBeforeClose", policy_status),
                ("attemptReceipt", receipt),
                (
                    "pairingCallback",
                    fs::read_to_string(pairing).unwrap().into(),
                ),
                ("trace", trace.into()),
                ("rows", rows(&root)),
                ("root", root.to_string_lossy().as_ref().into()),
            ]),
        );
    });
}

#[test]
fn missing_finite_context_during_close_fences_without_a_raw_write_or_replay() {
    runtime().block_on(async {
        let root = fixture("candidate-native-missing-context");
        let a = account(&root, None, None).await;
        let original = a
            .start("outbound", "missing-context", Json::Null)
            .await
            .unwrap();
        let id = original.id.clone();
        observed(&root, "missing-context").await;
        drop(a.quiesce());
        release(&root, "missing-context");
        assert_eq!(
            original.result().await.err().unwrap().outcome,
            Outcome::Unknown
        );
        assert!(a.quiesce_settled().await.is_err());
        stopped(&a).await;
        assert!(
            raw(
                &root.join("auth.sqlite"),
                "future-category",
                "missing-context"
            )
            .is_none()
        );
        assert!(super::auth_node::lock_held(&root));
        assert!(!a.accepting());
        capture(
            "native-missing-context",
            Json::object([
                ("originalOperation", id.into()),
                ("rows", rows(&root)),
                ("cleanup", a.cleanup_status()),
                ("root", root.to_string_lossy().as_ref().into()),
            ]),
        );
    });
}
