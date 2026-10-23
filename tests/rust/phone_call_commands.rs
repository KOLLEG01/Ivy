use super::*;
use crate::{
    journal::{PhoneJournalLimits, PhoneJournalOptions, PhoneJournalOwner},
    service_test_support::{equal, fixture, observed, timed, until},
};
use ivy_contracts::json::{canonical, parse};
use std::{path::PathBuf, sync::OnceLock};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    task::JoinHandle,
};
fn object<const N: usize>(items: [(&str, IvyJson); N]) -> IvyJson {
    IvyJson::object(items)
}
fn id(n: u32) -> JsString {
    format!("60000000-0000-4000-8000-{n:012}").into()
}
fn cases() -> &'static IvyJson {
    static CASES: OnceLock<IvyJson> = OnceLock::new();
    CASES.get_or_init(|| fixture("calls"))
}
fn string(value: &IvyJson, key: &str) -> String {
    value.get(key).unwrap().as_str().unwrap()
}
fn resolve(value: &IvyJson) -> IvyJson {
    match value {
        IvyJson::String(s) if s == &JsString::from("$call") => id(3).into(),
        IvyJson::Array(items) => IvyJson::Array(items.iter().map(resolve).collect()),
        IvyJson::Object(items) => {
            IvyJson::object(items.iter().map(|(k, v)| (k.clone(), resolve(v))))
        }
        _ => value.clone(),
    }
}
fn incoming(call: &JsString) -> IvyJson {
    let context = object([
        ("sipCallId", "observed-wire-call".into()),
        ("fromUri", "sip:known@fixture".into()),
        ("peerAddress", "127.0.0.1".into()),
        ("peerPort", 5060.0.into()),
        ("transport", "udp".into()),
    ]);
    object([
        ("id", call.clone().into()),
        ("direction", "incoming".into()),
        ("state", "ringing".into()),
        ("sipCallId", "observed-wire-call".into()),
        ("error", IvyJson::Null),
        ("incoming", context),
    ])
}
fn normalize(value: &IvyJson, operations: &[JsString], key: &JsString) -> IvyJson {
    match value {
        IvyJson::String(s) if key == &"createdAt".into() || key == &"updatedAt".into() => {
            let time = s.to_utf8().unwrap();
            assert_eq!(time.len(), 24);
            assert!(chrono::DateTime::parse_from_rfc3339(&time).is_ok());
            "$time".into()
        }
        IvyJson::String(s) if operations.contains(s) => {
            format!("$op{}", operations.iter().position(|id| id == s).unwrap()).into()
        }
        IvyJson::Array(items) => IvyJson::Array(
            items
                .iter()
                .map(|v| normalize(v, operations, &"".into()))
                .collect(),
        ),
        IvyJson::Object(items) => IvyJson::object(
            items
                .iter()
                .map(|(k, v)| (k.clone(), normalize(v, operations, k))),
        ),
        _ => value.clone(),
    }
}
struct Fixture {
    root: PathBuf,
    journal: PhoneJournal,
    native: PhoneNativeClient,
    admission: PhoneAdmission,
    owners: BTreeMap<String, PhoneCallCommands>,
    requests: Arc<Mutex<Vec<IvyJson>>>,
    output: Option<DuplexStream>,
    reader: JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-phone-commands-{}", uuid::Uuid::new_v4()));
        let journal = PhoneJournal::open(PhoneJournalOptions {
            data_root: root.clone(),
            owner: PhoneJournalOwner {
                host_id: "fixture".into(),
                service_node_id: "phone".into(),
            },
            limits: PhoneJournalLimits::default(),
            max_concurrent_calls: 1,
        })
        .await
        .unwrap();
        journal.begin_epoch(id(1)).await.unwrap();
        let admission = PhoneAdmission::new(cases().get("policy").unwrap().clone()).unwrap();
        let (input, peer_input) = tokio::io::duplex(65536);
        let (output, read) = tokio::io::duplex(65536);
        let native =
            PhoneNativeClient::start(id(1), input, read, Arc::new(journal.clone())).unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let captured = requests.clone();
        let reader = tokio::spawn(async move {
            let mut input = BufReader::new(peer_input);
            loop {
                let mut line = String::new();
                if input.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                captured.lock().unwrap().push(parse(&line, 65536).unwrap());
            }
        });
        let owners = BTreeMap::from([(
            "base".into(),
            PhoneCallCommands::new(admission.clone(), journal.clone(), native.clone()),
        )]);
        Self {
            root,
            journal,
            native,
            admission,
            owners,
            requests,
            output: Some(output),
            reader,
        }
    }
    async fn fixed(&self, mut call: IvyJson, desired: JsString) -> IvyJson {
        let old = text(&call, "callId").unwrap();
        if old == desired {
            return call;
        }
        call.insert("callId", desired.clone().into());
        // Stabilize only the private fixture's effect-free admission, before any
        // command runs. Actual allocation/retirement and receipt SQL stay real.
        self.journal
            .fixture_db(move |db| {
                db.execute_batch("BEGIN IMMEDIATE").unwrap();
                db.execute(
                    "UPDATE calls SET call_id=?,value=? WHERE call_id=?",
                    ivy_store::rusqlite::params![
                        desired.to_utf8().unwrap(),
                        canonical(&call, 65536).unwrap(),
                        old.to_utf8().unwrap()
                    ],
                )
                .unwrap();
                db.execute(
                    "UPDATE active_calls SET call_id=? WHERE call_id=?",
                    ivy_store::rusqlite::params![
                        desired.to_utf8().unwrap(),
                        old.to_utf8().unwrap()
                    ],
                )
                .unwrap();
                db.execute_batch("COMMIT").unwrap();
                Ok(call)
            })
            .await
            .unwrap()
    }
    async fn seed(&self, desired: JsString, is_incoming: bool, operation: JsString) {
        let admission = if is_incoming {
            self.admission
                .incoming(
                    id(1).into(),
                    operation.into(),
                    "main".into(),
                    &incoming(&desired),
                )
                .unwrap()
        } else {
            self.admission
                .outgoing(&object([
                    ("epoch", id(1).into()),
                    ("operationId", operation.into()),
                    ("principalId", "main".into()),
                    ("recipientId", "personal".into()),
                ]))
                .unwrap()
        };
        self.fixed(self.journal.admit_call(admission).await.unwrap(), desired)
            .await;
    }
    async fn sent(&self, count: usize) {
        until(|| self.requests.lock().unwrap().len() >= count).await
    }
    async fn reply(&mut self, index: usize, result: IvyJson, error: IvyJson) {
        self.sent(index + 1).await;
        let request_id = self.requests.lock().unwrap()[index]
            .get("requestId")
            .unwrap()
            .clone();
        let reply = object([
            ("version", 1.0.into()),
            ("epoch", id(1).into()),
            ("requestId", request_id),
            ("ok", error.is_null().into()),
            ("result", resolve(&result)),
            ("error", error),
        ]);
        self.output
            .as_mut()
            .unwrap()
            .write_all((canonical(&reply, 65536).unwrap() + "\n").as_bytes())
            .await
            .unwrap();
    }
    async fn close(self) {
        self.native.close("phone_owner_stopping");
        drop(self.output);
        self.reader.abort();
        let _ = self.reader.await;
        self.journal.close().wait().await.unwrap();
        assert!(self.root.starts_with(std::env::temp_dir()));
        assert!(
            self.root
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("ivy-phone-commands-")
        );
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
fn command(owner: &PhoneCallCommands, step: &IvyJson) -> PhoneCallWork {
    let args = resolve(step.get("args").unwrap());
    let args = args.as_array().unwrap();
    let p = args[0].as_js_string().unwrap().clone();
    let call = args[1].as_js_string().unwrap().clone();
    let number = |n: usize| args[n].as_f64().unwrap() as u32;
    match string(step, "method").as_str() {
        "prepare" => owner.prepare(p, call),
        "hangup" => owner.hangup(p, call),
        "release" => owner.release(p, call),
        "claim" => owner.claim(p, call),
        "answer" => owner.answer(p, call, args.get(2) == Some(&true.into())),
        "dial" => owner.dial(
            p,
            call,
            args[2]
                .get("username")
                .and_then(IvyJson::as_js_string)
                .cloned(),
            args[2]
                .get("password")
                .and_then(IvyJson::as_js_string)
                .cloned(),
            number(3),
            args.get(4) == Some(&true.into()),
        ),
        "launchDesktop" => owner.launch_desktop(p, call, args[2].clone()),
        "feedback" => owner.feedback(p, call, number(2), args[3] == true.into()),
        "prepareRealtime" => owner.prepare_realtime(
            p,
            call,
            number(2),
            args.get(3).and_then(IvyJson::as_f64).map(|n| n as u32),
            args.get(4).and_then(IvyJson::as_f64).map(|n| n as u32),
        ),
        "stopRealtime" => owner.stop_realtime(p, call, number(2)),
        _ => panic!("Unknown command"),
    }
}
#[tokio::test]
async fn original_live_admission_pending_receipts_cleanup_and_generation_scopes_agree() {
    for row in cases().get("cases").unwrap().as_array().unwrap() {
        let mut f = Fixture::new().await;
        let mut pending = BTreeMap::<String, PhoneCallWork>::new();
        let mut observations = vec![];
        for step in row.get("steps").unwrap().as_array().unwrap() {
            let owner = f
                .owners
                .get(
                    &step
                        .get("owner")
                        .and_then(|s| s.as_str().ok())
                        .unwrap_or_else(|| "base".into()),
                )
                .unwrap()
                .clone();
            match string(step, "action").as_str() {
                "seed" => {
                    f.seed(
                        step.get("id")
                            .and_then(IvyJson::as_js_string)
                            .cloned()
                            .unwrap_or_else(|| id(3)),
                        step.get("incoming") == Some(&true.into()),
                        if step.get("id").is_some() {
                            id(5)
                        } else {
                            id(2)
                        },
                    )
                    .await
                }
                "owner" => {
                    let policy = if step.get("changed") == Some(&true.into()) {
                        PhoneAdmission::new(object([
                            ("recipients", IvyJson::Array(vec![])),
                            ("incoming", IvyJson::Array(vec![])),
                        ]))
                        .unwrap()
                    } else {
                        f.admission.clone()
                    };
                    f.owners.insert(
                        string(step, "name"),
                        PhoneCallCommands::new(policy, f.journal.clone(), f.native.clone()),
                    );
                }
                "invoke" => {
                    pending.insert(string(step, "name"), command(&owner, step));
                }
                "outgoing" => {
                    pending.insert(
                        string(step, "name"),
                        owner.admit_outgoing(step.get("args").unwrap().clone()),
                    );
                }
                "incoming" => {
                    pending.insert(
                        string(step, "name"),
                        owner.admit_incoming(step.get("args").unwrap().clone()),
                    );
                }
                "sent" => {
                    f.sent(step.get("count").unwrap().as_f64().unwrap() as usize)
                        .await
                }
                "reply" => {
                    f.reply(
                        step.get("index").unwrap().as_f64().unwrap() as usize,
                        step.get("result").unwrap().clone(),
                        step.get("error").unwrap().clone(),
                    )
                    .await
                }
                "wait" => {
                    let name = string(step, "name");
                    let result = timed(pending.get(&name).unwrap().wait()).await;
                    let result = if step.get("admission") == Some(&true.into()) {
                        match result {
                            Ok(call) => Ok(f.fixed(call, id(3)).await),
                            Err(error) => Err(error),
                        }
                    } else {
                        result
                    };
                    observations.push(object([
                        ("action", name.into()),
                        ("result", observed(result)),
                    ]));
                }
                "cancel" => observations.push(object([
                    ("action", "cancel".into()),
                    (
                        "result",
                        observed(
                            timed(owner.cancel_unprepared("admin".into(), id(3)).wait()).await,
                        ),
                    ),
                ])),
                "read" => observations.push(object([
                    ("action", "read".into()),
                    (
                        "result",
                        observed(
                            owner
                                .read(
                                    "admin".into(),
                                    id(3),
                                    step.get("method").unwrap().as_js_string().unwrap().clone(),
                                    0,
                                )
                                .await
                                .map(|v| v.unwrap_or(IvyJson::Null)),
                        ),
                    ),
                ])),
                "state" => observations.push(object([
                    ("action", "state".into()),
                    (
                        "result",
                        object([
                            (
                                "current",
                                IvyJson::Array(
                                    f.journal
                                        .current_calls()
                                        .await
                                        .unwrap()
                                        .iter()
                                        .map(|c| c.get("callId").unwrap().clone())
                                        .collect(),
                                ),
                            ),
                            ("status", f.journal.status().await.unwrap()),
                        ]),
                    ),
                ])),
                "retire" => f.journal.lose_epoch(id(1)).await.unwrap(),
                "epoch" => f
                    .journal
                    .begin_epoch(step.get("id").unwrap().as_js_string().unwrap().clone())
                    .await
                    .unwrap(),
                "close" => {
                    f.output.take();
                }
                _ => panic!("Unknown step"),
            }
        }
        let requests = f.requests.lock().unwrap().clone();
        let operations = requests
            .iter()
            .filter_map(|v| {
                v.get("operationId")
                    .and_then(IvyJson::as_js_string)
                    .cloned()
            })
            .collect::<Vec<_>>();
        let actual = normalize(
            &object([
                ("observations", IvyJson::Array(observations)),
                ("requests", IvyJson::Array(requests)),
            ]),
            &operations,
            &"".into(),
        );
        equal(&actual, row.get("expected").unwrap(), &string(row, "name"));
        f.close().await;
    }
}
#[tokio::test]
async fn admitted_prepare_and_release_settle_after_their_observers_disappear() {
    let mut f = Fixture::new().await;
    f.seed(id(3), false, id(2)).await;
    let calls = f.owners["base"].clone();
    drop(calls.prepare("main".into(), id(3)));
    f.reply(0, object([("callId", id(3).into())]), IvyJson::Null)
        .await;
    timed(calls.prepare("main".into(), id(3)).wait())
        .await
        .unwrap();
    drop(calls.release("main".into(), id(3)));
    f.reply(
        1,
        object([("callId", id(3).into()), ("released", true.into())]),
        IvyJson::Null,
    )
    .await;
    timed(async {
        loop {
            if f.journal.current_calls().await.unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(f.requests.lock().unwrap().len(), 2);
    f.close().await;
}
