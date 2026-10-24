use super::*;
use crate::{
    journal::{PhoneJournalLimits, PhoneJournalOptions, PhoneJournalOwner},
    voice_rpc::control_test_support::{Plan, connector_fixture},
};
use ivy_contracts::json::parse;
use ivy_host_runtime::node_path;
use ivy_store::rusqlite::params;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, DuplexStream};

fn value(source: &str) -> IvyJson {
    parse(source, 4 * 1024 * 1024).unwrap()
}
fn object<const N: usize>(fields: [(&str, IvyJson); N]) -> IvyJson {
    IvyJson::object(fields)
}
fn id(n: u32) -> JsString {
    format!("30000000-0000-4000-8000-{n:012}").into()
}
fn equal(actual: &IvyJson, expected: &IvyJson, scope: &str) {
    assert_eq!(
        canonical(actual, 4 * 1024 * 1024).unwrap(),
        canonical(expected, 4 * 1024 * 1024).unwrap(),
        "{scope}"
    );
}
async fn timed<T>(work: impl Future<Output = T>) -> T {
    // Connection setup validates the complete native catalog in this fixture.
    tokio::time::timeout(Duration::from_secs(60), work)
        .await
        .unwrap()
}
async fn until(mut condition: impl FnMut() -> bool) {
    timed(async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let parent = std::env::var_os("IVY_TEST_TEMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let directory = parent.join(format!("ivy-phone-voice-journal-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn options(&self) -> PhoneJournalOptions {
        PhoneJournalOptions {
            data_root: self.0.clone(),
            owner: PhoneJournalOwner {
                host_id: "phone-fixture-host".into(),
                service_node_id: "phone-fixture-node".into(),
            },
            limits: PhoneJournalLimits::default(),
            max_concurrent_calls: 2,
        }
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("ivy-phone-voice-journal-")
        );
        if let Err(error) = fs::remove_dir_all(&self.0) {
            if !std::thread::panicking() {
                panic!("Fixture cleanup failed: {error}");
            }
        }
    }
}

fn observed<T: Into<IvyJson>>(action: &str, result: IvyResult<T>) -> IvyJson {
    match result {
        Ok(result) => object([
            ("action", action.into()),
            ("ok", true.into()),
            ("value", result.into()),
        ]),
        Err(error) => object([
            ("action", action.into()),
            ("ok", false.into()),
            ("code", error.code_js().into()),
            ("outcome", error.outcome.to_json()),
        ]),
    }
}
fn unit(action: &str, result: IvyResult<()>) -> IvyJson {
    observed(action, result.map(|()| IvyJson::Null))
}
fn selection() -> VoiceSelection {
    VoiceSelection {
        model: "fixture-model".into(),
        reasoning_effort: "high".into(),
    }
}
fn async_guard() -> VoiceAsyncGuard {
    Arc::new(|| Box::pin(async { Ok(()) }))
}
fn incoming(n: u32, epoch: &JsString) -> IvyJson {
    object([
        ("operationId", id(600 + n).into()),
        ("epoch", epoch.clone().into()),
        ("principalId", format!("voice-fixture-{n}").into()),
        ("policyHash", digest("fixture-policy").into()),
        ("direction", "incoming".into()),
        ("callId", id(700 + n).into()),
        ("recipientId", IvyJson::Null),
        ("destination", IvyJson::Null),
        (
            "incoming",
            object([
                ("sipCallId", format!("voice-offer-{n}").into()),
                ("fromUri", "sip:+49123456789@example.test".into()),
                ("peerAddress", "127.0.0.1".into()),
                ("peerPort", 5060.0.into()),
                ("transport", "udp".into()),
            ]),
        ),
    ])
}
fn thread(id: &JsString, cwd: &IvyJson, status: &str, turns: &[IvyJson]) -> IvyJson {
    object([
        ("id", id.clone().into()),
        ("cwd", cwd.clone()),
        ("projectId", IvyJson::Null),
        ("ephemeral", false.into()),
        ("status", object([("type", status.into())])),
        ("turns", IvyJson::Array(turns.to_vec())),
    ])
}
fn ready_turn(id: JsString, operation: IvyJson) -> IvyJson {
    object([
        ("id", id.into()),
        ("status", "completed".into()),
        (
            "items",
            IvyJson::Array(vec![
                object([("type", "userMessage".into()), ("clientId", operation)]),
                object([
                    ("type", "mcpToolCall".into()),
                    ("server", "fixture-ivy".into()),
                    ("tool", "ivy_instructions".into()),
                    ("status", "completed".into()),
                    ("error", IvyJson::Null),
                    ("result", object([])),
                ]),
                object([
                    ("type", "mcpToolCall".into()),
                    ("server", "fixture-ivy".into()),
                    ("tool", "phone_bridge_status".into()),
                    ("status", "completed".into()),
                    ("error", IvyJson::Null),
                    ("result", object([])),
                ]),
                object([
                    ("type", "agentMessage".into()),
                    ("phase", "final_answer".into()),
                    ("text", "READY".into()),
                ]),
            ]),
        ),
    ])
}
type Writer = Arc<tokio::sync::Mutex<DuplexStream>>;
async fn frame(writer: &Writer, frame: IvyJson) {
    writer
        .lock()
        .await
        .write_all(format!("{}\n", stringify(&frame, 4 * 1024 * 1024).unwrap()).as_bytes())
        .await
        .unwrap();
}
async fn commands(journal: &PhoneJournal) -> IvyJson {
    journal
        .fixture_db(|db| {
            let mut statement = db
                .prepare("SELECT value FROM commands ORDER BY rowid")
                .unwrap();
            Ok(IvyJson::Array(
                statement
                    .query_map([], |r| r.get::<_, String>(0))
                    .unwrap()
                    .map(|r| {
                        let command = value(&r.unwrap());
                        object([
                            (
                                "method",
                                command
                                    .get("intent")
                                    .unwrap()
                                    .get("method")
                                    .unwrap()
                                    .clone(),
                            ),
                            ("phase", command.get("phase").unwrap().clone()),
                            ("receipt", command.get("receipt").unwrap().clone()),
                        ])
                    })
                    .collect(),
            ))
        })
        .await
        .unwrap()
}
async fn seed_context(journal: &PhoneJournal, fingerprint: &str) {
    let intent = object([
        ("epoch", id(900).into()),
        ("operationId", id(500).into()),
        ("callId", id(701).into()),
        ("method", "call.stopVoice".into()),
        ("requestHash", digest("fixture-stop").into()),
        ("threadId", id(100).into()),
    ]);
    let operation = object([
        ("schemaVersion", 1.0.into()),
        ("intent", intent),
        ("phase", "result".into()),
        (
            "receipt",
            object([
                ("ok", true.into()),
                (
                    "result",
                    object([("threadId", id(100).into()), ("stopped", true.into())]),
                ),
                ("error", IvyJson::Null),
            ]),
        ),
        ("createdAt", "2026-08-25T10:00:00.000Z".into()),
        ("updatedAt", "2026-08-25T10:00:00.000Z".into()),
    ]);
    journal
        .fixture_db(move |db| {
            db.execute(
                "INSERT INTO commands VALUES(?,?,?,?,?,?)",
                params![
                    id(500).to_utf8().unwrap(),
                    id(900).to_utf8().unwrap(),
                    id(701).to_utf8().unwrap(),
                    "call.stopVoice",
                    "result",
                    canonical(&operation, 65536).unwrap()
                ],
            )
            .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    journal
        .retain_voice_conversation(object([
            ("fingerprint", fingerprint.into()),
            ("partyKey", digest("+49123456789").into()),
            ("threadId", id(100).into()),
            ("callId", id(701).into()),
            ("generation", 0.0.into()),
            (
                "items",
                IvyJson::Array(vec![object([
                    ("role", "user".into()),
                    ("text", "fixture earlier conversation".into()),
                ])]),
            ),
        ]))
        .await
        .unwrap();
}

// The real journal worker and real NativeRpc duplex transport are retained.
// Only the peer's native process, accounts and network effects are scripted.
#[tokio::test]
async fn reusable_and_retired_archive_queries_preserve_owned_scope() {
    let expected = value(include_str!(
        "../fixtures/phone-journal-voice-retirement-cases.json"
    ));
    let seed = expected.get("seed").unwrap();
    let root = Root::new();
    let journal = PhoneJournal::open(root.options()).await.unwrap();
    journal.begin_epoch(id(900)).await.unwrap();
    journal
        .admit_call(seed.get("call").unwrap().clone())
        .await
        .unwrap();
    {
        let saved_seed = seed.clone();
        journal
            .fixture_db(move |db| {
                let seed = &saved_seed;
                let old = seed.get("old").unwrap();
                let intent = old.get("intent").unwrap();
                db.execute(
                    "INSERT INTO commands VALUES(?,?,?,?,?,?)",
                    params![
                        text(intent, "operationId").unwrap().to_utf8().unwrap(),
                        text(intent, "epoch").unwrap().to_utf8().unwrap(),
                        text(intent, "callId").unwrap().to_utf8().unwrap(),
                        "call.bindVoice",
                        "result",
                        canonical(old, 65536).unwrap()
                    ],
                )
                .unwrap();
                let latest = seed.get("latest").unwrap();
                db.execute(
                    "INSERT INTO receipt_archive VALUES(?,?,?)",
                    params![
                        "command",
                        text(latest.get("intent").unwrap(), "operationId")
                            .unwrap()
                            .to_utf8()
                            .unwrap(),
                        canonical(latest, 65536).unwrap()
                    ],
                )
                .unwrap();
                db.execute(
                    "INSERT INTO receipt_archive VALUES(?,?,?)",
                    params![
                        "call-command",
                        canonical(
                            &IvyJson::Array(vec![
                                id(700).into(),
                                "call.bindVoice".into(),
                                1.0.into()
                            ]),
                            65536
                        )
                        .unwrap(),
                        id(502).to_utf8().unwrap()
                    ],
                )
                .unwrap();
                for retired in seed.get("retired").unwrap().as_array().unwrap() {
                    db.execute(
                        "INSERT INTO meta VALUES(?,?)",
                        params![
                            format!(
                                "voiceRetired:{}",
                                text(retired, "threadId").unwrap().to_utf8().unwrap()
                            ),
                            canonical(retired, 65536).unwrap()
                        ],
                    )
                    .unwrap();
                }
                Ok(())
            })
            .await
            .unwrap();
    }
    let mut observations = vec![
        observed(
            "reusable",
            journal
                .reusable_voice_task("voice-retirement".into())
                .await
                .map(|v| v.map_or(IvyJson::Null, Into::into)),
        ),
        observed(
            "missing-principal",
            journal
                .reusable_voice_task("no-call".into())
                .await
                .map(|v| v.map_or(IvyJson::Null, Into::into)),
        ),
        observed(
            "task",
            journal
                .retired_voice_task(id(10))
                .await
                .map(|v| v.unwrap_or(IvyJson::Null)),
        ),
        observed(
            "page",
            journal
                .retired_voice_page(1, "".into())
                .await
                .map(IvyJson::Array),
        ),
        observed(
            "page-after",
            journal
                .retired_voice_page(64, id(10))
                .await
                .map(IvyJson::Array),
        ),
        unit("begin", journal.begin_voice_archive(id(10), id(598)).await),
        observed(
            "after-begin",
            journal
                .retired_voice_task(id(10))
                .await
                .map(|v| v.unwrap_or(IvyJson::Null)),
        ),
        unit(
            "begin-again",
            journal.begin_voice_archive(id(10), id(597)).await,
        ),
        unit(
            "bad-operation",
            journal.begin_voice_archive(id(20), "bad".into()).await,
        ),
        observed(
            "bad-page",
            journal
                .retired_voice_page(0, "".into())
                .await
                .map(IvyJson::Array),
        ),
    ];
    let corrupt = seed.get("corrupt").unwrap().clone();
    journal
        .fixture_db(move |db| {
            db.execute(
                "INSERT INTO meta VALUES(?,?)",
                params![
                    format!(
                        "voiceRetired:{}",
                        text(&corrupt, "threadId").unwrap().to_utf8().unwrap()
                    ),
                    canonical(&corrupt, 65536).unwrap()
                ],
            )
            .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    observations.push(observed(
        "corrupt",
        journal
            .retired_voice_task(id(40))
            .await
            .map(|v| v.unwrap_or(IvyJson::Null)),
    ));
    equal(
        &IvyJson::Array(observations),
        expected.get("observations").unwrap(),
        "retirement",
    );
    journal.close().wait().await.unwrap();
}

#[tokio::test]
async fn task_sessions_recovery_reservations_and_archival_preserve_receipts() {
    let reference = value(include_str!(
        "../fixtures/phone-journal-voice-owner-cases.json"
    ));
    assert_eq!(reference.as_array().unwrap().len(), 13);
    for case in reference.as_array().unwrap() {
        let root = Root::new();
        let journal = PhoneJournal::open(root.options()).await.unwrap();
        let name = text(case, "name").unwrap().to_utf8().unwrap();
        let payload = case.get("payload").unwrap();
        let native = payload.get("settings").unwrap().clone();
        let fingerprint = text(payload, "fingerprint").unwrap().to_utf8().unwrap();
        journal.begin_epoch(id(900)).await.unwrap();
        if name == "pending-creation" {
            journal
                .retain_task(
                    "prepared-phone-voice",
                    object([
                        ("fingerprint", fingerprint.clone().into()),
                        ("threadId", IvyJson::Null),
                        ("creationOperationId", id(400).into()),
                    ]),
                )
                .await
                .unwrap();
        }
        if name.starts_with("continuation-") {
            seed_context(&journal, &fingerprint).await;
        }
        if [
            "idle-archive",
            "archive-lost-ack-observed",
            "archive-active-descendant",
        ]
        .contains(&name.as_str())
        {
            journal
                .remember_codex_task(&fingerprint, id(100))
                .await
                .unwrap();
            if name == "archive-lost-ack-observed" {
                journal
                    .begin_codex_archive(
                        &fingerprint,
                        object([("threadId", id(100).into()), ("operationId", IvyJson::Null)]),
                        id(401),
                    )
                    .await
                    .unwrap();
            }
        }
        let (connector, mut peers, schemas) = connector_fixture(vec![Plan::with_catalog(value(
            include_str!("../../specs/native/codex-0.159.2/catalog.json"),
        ))]);
        let errors = Arc::new(Mutex::new(Vec::<IvyJson>::new()));
        let reported = errors.clone();
        let voice = PhoneCodexVoice::new(
            VoiceControlSettings {
                native: native.clone(),
                artifact_root: "artifacts".into(),
                data_root: "owned-data".into(),
                credential: None,
                paths: node_path::Context {
                    style: node_path::Style::native(),
                    cwd: node_path::from_native(&root.0),
                },
            },
            journal.clone(),
            connector,
            schemas,
            Arc::new(move |code| reported.lock().unwrap().push(code.into())),
        )
        .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let answered = Arc::new(AtomicBool::new(false));
        let fail_stop = Arc::new(AtomicBool::new(false));
        let (writer_ready, mut writer) = tokio::sync::oneshot::channel();
        let peer_calls = calls.clone();
        let peer_answered = answered.clone();
        let peer_fail_stop = fail_stop.clone();
        let peer_name = name.clone();
        let peer_journal = journal.clone();
        let peer_fingerprint = fingerprint.clone();
        let cwd = native.get("cwd").unwrap().clone();
        let server = tokio::spawn(async move {
            let (mut reader, output) = peers.recv().await.unwrap().into_io();
            let writer = Arc::new(tokio::sync::Mutex::new(output));
            writer_ready
                .send(writer.clone())
                .unwrap_or_else(|_| panic!("Owner dropped"));
            let mut created = Vec::<JsString>::new();
            let mut turns = BTreeMap::<JsString, Vec<IvyJson>>::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let request = value(&line);
                if request.get("method").is_none() {
                    equal(request.get("id").unwrap(), &7.0.into(), "native answer id");
                    peer_answered.store(true, Ordering::SeqCst);
                    continue;
                }
                let method = text(&request, "method").unwrap().to_utf8().unwrap();
                let params = request.get("params").unwrap();
                let mut normalized = params.clone();
                if normalized.get("clientUserMessageId").is_some() {
                    normalized.insert("clientUserMessageId", "$preparation".into());
                }
                peer_calls.lock().unwrap().push(object([
                    ("method", method.clone().into()),
                    ("params", normalized),
                ]));
                if ["thread/start", "turn/start"].contains(&method.as_str()) {
                    let cache = peer_journal
                        .task_cache("prepared-phone-voice", &peer_fingerprint)
                        .await
                        .unwrap()
                        .unwrap();
                    equal(
                        cache
                            .get(if method == "thread/start" {
                                "creationOperationId"
                            } else {
                                "preparationOperationId"
                            })
                            .unwrap(),
                        request.get("id").unwrap(),
                        "durable original request id before dispatch",
                    );
                }
                let thread_id = params
                    .get("threadId")
                    .and_then(IvyJson::as_js_string)
                    .cloned()
                    .unwrap_or_default();
                let mut result = object([]);
                let mut error = None;
                match method.as_str() {
                    "thread/start" if peer_name == "rejected-creation-no-replay" => {
                        error = Some((-32603.0, "fixture creation rejected".to_owned()))
                    }
                    "thread/start" => {
                        let created_id = id(1 + created.len() as u32);
                        created.push(created_id.clone());
                        result = object([("thread", thread(&created_id, &cwd, "idle", &[]))]);
                    }
                    "thread/read" => {
                        result = object([(
                            "thread",
                            thread(
                                &thread_id,
                                &cwd,
                                "idle",
                                turns.get(&thread_id).map_or(&[], |v| v.as_slice()),
                            ),
                        )])
                    }
                    "thread/resume"
                        if peer_name == "continuation-missing" && thread_id == id(100) =>
                    {
                        error = Some((
                            -32600.0,
                            format!(
                                "no rollout found for thread id {}",
                                thread_id.to_utf8().unwrap()
                            ),
                        ))
                    }
                    "thread/resume" => {
                        result = object([(
                            "thread",
                            thread(
                                &thread_id,
                                &cwd,
                                "idle",
                                turns.get(&thread_id).map_or(&[], |v| v.as_slice()),
                            ),
                        )])
                    }
                    "turn/start" => {
                        let turn = ready_turn(
                            id(200 + created.iter().position(|v| v == &thread_id).unwrap() as u32),
                            params.get("clientUserMessageId").unwrap().clone(),
                        );
                        turns.insert(thread_id.clone(), vec![turn.clone()]);
                        result = object([("turn", turn)]);
                    }
                    "thread/realtime/listVoices" => {
                        result = value(
                            r#"{"voices":{"v1":["cove"],"defaultV1":"cove","v2":["marin"],"defaultV2":"marin"}}"#,
                        )
                    }
                    "config/read" => result = value(r#"{"config":{"realtime":{"voice":"cove"}}}"#),
                    "thread/realtime/start" => {
                        frame(
                            &writer,
                            object([
                                ("method", "thread/realtime/sdp".into()),
                                (
                                    "params",
                                    object([
                                        ("threadId", thread_id.into()),
                                        ("sdp", "v=0\r\nfixture-answer\r\n".into()),
                                    ]),
                                ),
                            ]),
                        )
                        .await
                    }
                    "thread/realtime/stop" if peer_fail_stop.load(Ordering::SeqCst) => {
                        error = Some((
                            -32603.0,
                            "fixture stop acknowledgement unavailable".to_owned(),
                        ))
                    }
                    "thread/list" => {
                        result = object([
                            (
                                "data",
                                IvyJson::Array(if params.get("archived") == Some(&true.into()) {
                                    vec![thread(&id(100), &cwd, "idle", &[])]
                                } else if peer_name == "archive-active-descendant" {
                                    vec![thread(&id(101), &cwd, "active", &[])]
                                } else {
                                    vec![]
                                }),
                            ),
                            ("nextCursor", IvyJson::Null),
                        ])
                    }
                    "thread/name/set"
                    | "thread/settings/update"
                    | "thread/unsubscribe"
                    | "thread/realtime/appendText"
                    | "thread/realtime/stop"
                    | "thread/archive" => (),
                    _ => panic!("Unexpected native method {method}"),
                }
                let reply = match error {
                    Some((code, message)) => object([(
                        "error",
                        object([("code", code.into()), ("message", message.into())]),
                    )]),
                    None => object([("result", result)]),
                };
                let mut reply = reply;
                reply.insert("id", request.get("id").unwrap().clone());
                frame(&writer, reply).await;
            }
        });
        let mut observations = Vec::new();
        if [
            "incoming-session",
            "lost-stop-ack",
            "connection-loss-clears-inputs",
            "overlapping-call-reservations",
        ]
        .contains(&name.as_str())
        {
            let call = journal.admit_call(incoming(0, &id(900))).await.unwrap();
            let call_id = text(&call, "callId").unwrap();
            let prepared = timed(voice.prepare(call.clone(), 0, selection()).wait()).await;
            observations.push(observed("prepare", prepared.clone()));
            let thread_id = prepared.unwrap();
            if name == "overlapping-call-reservations" {
                let second = journal.admit_call(incoming(1, &id(900))).await.unwrap();
                observations.push(observed(
                    "prepare-second",
                    timed(voice.prepare(second, 0, selection()).wait()).await,
                ));
                observations.push(unit(
                    "stop-without-session",
                    timed(voice.stop(call_id, None).wait()).await,
                ));
            } else {
                observations.push(observed(
                    "start",
                    timed(
                        voice
                            .start(
                                call.clone(),
                                0,
                                thread_id.clone(),
                                "fixture initial assignment".into(),
                                "fixture-offer-sdp".into(),
                                Arc::new(|| Ok(())),
                            )
                            .unwrap()
                            .wait(),
                    )
                    .await,
                ));
                observations.push(unit(
                    "connected",
                    timed(voice.connected(call_id.clone(), async_guard()).wait()).await,
                ));
                let emitter = timed(&mut writer).await.unwrap();
                frame(
                    &emitter,
                    object([
                        ("id", 7.0.into()),
                        ("method", "item/tool/requestUserInput".into()),
                        (
                            "params",
                            object([
                                ("threadId", thread_id.clone().into()),
                                ("turnId", id(300).into()),
                                ("itemId", "fixture-item".into()),
                                ("questions", IvyJson::Array(vec![])),
                            ]),
                        ),
                    ]),
                )
                .await;
                until(|| voice.inputs(&call_id).unwrap().len() == 1).await;
                let inputs = voice
                    .inputs(&call_id)
                    .unwrap()
                    .iter()
                    .map(|r| {
                        object([
                            ("id", r.get("id").unwrap().clone()),
                            ("method", r.get("method").unwrap().clone()),
                            (
                                "threadId",
                                r.get("params").unwrap().get("threadId").unwrap().clone(),
                            ),
                        ])
                    })
                    .collect();
                observations.push(observed("inputs", Ok(IvyJson::Array(inputs))));
                let input_token=text(&voice.inputs(&call_id).unwrap()[0],"inputToken").unwrap();
                assert!(
                    matches!(voice.answer(call_id.clone(), "7".into(), IvyJson::Null,input_token.clone()),
                    Err(error) if error.code == "phone_voice_input_missing"),
                    "A text request ID must not answer the numeric request 7"
                );
                if name == "connection-loss-clears-inputs" {
                    voice
                        .0
                        .rpc()
                        .unwrap()
                        .transport()
                        .close("fixture_connection_lost")
                        .await;
                    until(|| {
                        voice.failed(&call_id)
                            && errors
                                .lock()
                                .unwrap()
                                .contains(&"fixture_connection_lost".into())
                    })
                    .await;
                } else {
                    observations.push(unit(
                        "answer",
                        timed(
                            voice
                                .answer(
                                    call_id.clone(),
                                    7.0.into(),
                                    value(r#"{"result":{"answers":{}}}"#),
                                    input_token,
                                )
                                .unwrap()
                                .wait(),
                        )
                        .await,
                    ));
                    until(|| answered.load(Ordering::SeqCst)).await;
                    observations.push(unit(
                        "select",
                        timed(
                            voice
                                .select(
                                    call.clone(),
                                    thread_id.clone(),
                                    VoiceSelectionCommand::Sequence(1),
                                    VoiceSelection {
                                        model: "fixture-next-model".into(),
                                        reasoning_effort: "high".into(),
                                    },
                                    async_guard(),
                                )
                                .wait(),
                        )
                        .await,
                    ));
                    observations.push(unit(
                        "prompt",
                        timed(
                            voice
                                .prompt(
                                    call.clone(),
                                    thread_id.clone(),
                                    "fixture follow-up".into(),
                                    id(402),
                                    digest("fixture-forward").into(),
                                    async_guard(),
                                )
                                .wait(),
                        )
                        .await,
                    ));
                    for n in 0..130 {
                        frame(
                            &emitter,
                            object([
                                ("method", "thread/realtime/item/completed".into()),
                                (
                                    "params",
                                    object([
                                        ("threadId", thread_id.clone().into()),
                                        (
                                            "item",
                                            object([
                                                ("type", "transcriptSegment".into()),
                                                ("id", format!("transcript-{n}").into()),
                                                (
                                                    "role",
                                                    if n % 2 == 0 { "user" } else { "assistant" }
                                                        .into(),
                                                ),
                                                ("text", format!("line-{n}").into()),
                                            ]),
                                        ),
                                    ]),
                                ),
                            ]),
                        )
                        .await;
                    }
                    until(|| {
                        voice
                            .0
                            .session(&call_id)
                            .unwrap()
                            .lock()
                            .unwrap()
                            .items
                            .last()
                            .unwrap()
                            .get("text")
                            == Some(&"line-129".into())
                    })
                    .await;
                    fail_stop.store(name == "lost-stop-ack", Ordering::SeqCst);
                    observations.push(unit(
                        "stop",
                        timed(voice.stop(call_id.clone(), None).wait()).await,
                    ));
                    if name != "lost-stop-ack" {
                        observations.push(unit(
                            "stop-again",
                            timed(voice.stop(call_id.clone(), None).wait()).await,
                        ));
                    }
                }
                observations.push(observed(
                    "session-state",
                    Ok(object([
                        ("failed", voice.failed(&call_id).into()),
                        (
                            "speaker",
                            voice
                                .speaker_voice(&call_id)
                                .map_or(IvyJson::Null, Into::into),
                        ),
                        (
                            "inputs",
                            (voice.inputs(&call_id).unwrap().len() as f64).into(),
                        ),
                    ])),
                ));
                drop(emitter);
            }
        } else {
            observations.push(unit(
                "prewarm",
                timed(voice.prewarm("unused-fixture".into(), selection()).wait()).await,
            ));
            if [
                "warm-task",
                "unloaded-task",
                "pending-creation",
                "rejected-creation-no-replay",
            ]
            .contains(&name.as_str())
            {
                observations.push(unit(
                    "prewarm-again",
                    timed(voice.prewarm("unused-fixture".into(), selection()).wait()).await,
                ));
            }
            timed(voice.archive().wait()).await.unwrap();
        }
        let cache = journal
            .task_cache("prepared-phone-voice", &fingerprint)
            .await
            .unwrap()
            .map_or(IvyJson::Null, |v| {
                object([
                    ("threadId", v.get("threadId").unwrap().clone()),
                    (
                        "creationPending",
                        v.get("creationOperationId")
                            .and_then(IvyJson::as_js_string)
                            .is_some()
                            .into(),
                    ),
                    (
                        "prepared",
                        v.get("preparationTurnId")
                            .and_then(IvyJson::as_js_string)
                            .is_some()
                            .into(),
                    ),
                ])
            });
        let context = journal
            .voice_conversation(&fingerprint)
            .await
            .unwrap()
            .map_or(IvyJson::Null, |v| {
                object([
                    ("threadId", v.get("threadId").unwrap().clone()),
                    ("items", v.get("items").unwrap().clone()),
                ])
            });
        let actual = object([
            ("observations", IvyJson::Array(observations)),
            ("calls", IvyJson::Array(calls.lock().unwrap().clone())),
            ("errors", IvyJson::Array(errors.lock().unwrap().clone())),
            ("cache", cache),
            ("context", context),
            (
                "archive",
                IvyJson::Array(
                    journal
                        .codex_archive_page(&fingerprint, "", 16)
                        .await
                        .unwrap(),
                ),
            ),
            ("commands", commands(&journal).await),
        ]);
        equal(&actual, case.get("expected").unwrap(), &name);
        // Positive cleanup only after the saved comparison; uncertain stop is not replayed by this test.
        if name == "lost-stop-ack" {
            voice
                .0
                .rpc()
                .unwrap()
                .transport()
                .close("fixture_finished")
                .await;
        }
        if ["lost-stop-ack", "connection-loss-clears-inputs"].contains(&name.as_str()) {
            voice.0.state.lock().unwrap().sessions.clear();
        }
        let _ = timed(voice.close().wait()).await;
        drop(writer);
        timed(server).await.unwrap();
        journal.close().wait().await.unwrap();
    }
}
