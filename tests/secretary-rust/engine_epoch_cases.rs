#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use ivy_contracts::json::canonical;
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    engine::SecretaryEngine,
    inbox_evidence::{InboxEvidence, operation_name},
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::params;
use std::{
    cell::RefCell,
    future::Future,
    task::{Context, Poll, Waker},
};
use tokio::sync::oneshot;

enum EpochReply {
    Ready(ivy_contracts::IvyResult<IvyJson>),
    Pending(oneshot::Receiver<ivy_contracts::IvyResult<IvyJson>>),
}
struct EpochClient {
    trace: Mutex<Vec<IvyJson>>,
    epochs: Mutex<VecDeque<EpochReply>>,
    owner: IvyJson,
}
impl EpochClient {
    fn trace(&self) -> IvyJson {
        IvyJson::Array(self.trace.lock().unwrap().clone())
    }
}
impl RpcClient for EpochClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params),
            ]));
            if method == "system.status" {
                let reply = self
                    .epochs
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("Unexpected epoch retry");
                return match reply {
                    EpochReply::Ready(value) => value,
                    EpochReply::Pending(value) => value.await.unwrap(),
                };
            }
            // Match the original resolved RPC Promise's asynchronous settlement.
            tokio::task::yield_now().await;
            match method {
                "serviceNodes.get" => Ok(self.owner.clone()),
                "objects.stat" => Ok(IvyJson::object([("effectivelyArchived", false.into())])),
                _ => panic!("Unexpected RPC {method}"),
            }
        })
    }
}
struct FixedClock(i64);
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
fn failure(error: IvyError) -> IvyJson {
    IvyJson::object([
        ("code", error.code.into()),
        ("message", error.message.into()),
        ("outcome", error.outcome.to_json()),
    ])
}
fn poll_pending<T>(future: std::pin::Pin<&mut impl Future<Output = T>>) {
    assert!(matches!(
        future.poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
}
#[tokio::test]
async fn original_engine_constructor_fixed_epoch_and_independent_reads() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = reference_data::read(include_bytes!("engine-epoch-cases.json.gz"));
    let corpus = parse(
        std::str::from_utf8(&output).unwrap(),
        4 * 1024 * 1024,
    )
    .unwrap();
    let cases = corpus.required("cases").unwrap().as_array().unwrap();
    assert_eq!(cases.len(), 5);
    for (index, expected) in cases.iter().enumerate() {
        let (sender, receiver) = oneshot::channel();
        let replies = if index == 1 {
            VecDeque::from([
                EpochReply::Ready(Err(IvyError::new(
                    "fixture_epoch_failed",
                    "Original epoch failure.",
                )
                .with_outcome(Outcome::Unknown))),
                EpochReply::Ready(Ok(IvyJson::object([(
                    "runtimeEpoch",
                    "fixture-epoch".into(),
                )]))),
            ])
        } else {
            VecDeque::from([EpochReply::Pending(receiver)])
        };
        let client = Arc::new(EpochClient {
            trace: Mutex::default(),
            epochs: Mutex::new(replies),
            owner: corpus.required("owner").unwrap().clone(),
        });
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        let journal2 = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        let store = Store {
            client: client.clone(),
            root: corpus
                .required("rootId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            journal: &journal,
        };
        let store2 = Store {
            client: client.clone(),
            root: store.root.clone(),
            journal: &journal2,
        };
        let mut settings = corpus.required("config").unwrap().clone();
        if index == 3 {
            settings.insert(
                "sources",
                corpus
                    .required("settings")
                    .unwrap()
                    .required("sources")
                    .unwrap()
                    .clone(),
            );
        }
        let node = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: &settings,
            service_node_id: &node,
            cancellation: RequestOptions::default(),
        };
        let evidence2 = InboxEvidence {
            store: &store2,
            settings: &settings,
            service_node_id: &node,
            cancellation: RequestOptions::default(),
        };
        let clock = FixedClock(1767225600000);
        let tools = ToolClients::new(client.clone());
        let issues = RefCell::new(RecoveryScan::default());
        let engine = SecretaryEngine::new(
            &evidence,
            &clock,
            &clock,
            ivy_secretary::engine::random_uuid,
            &tools,
            &issues,
        );
        let id = corpus.required("id").unwrap().as_js_string().unwrap();
        let mut actual = IvyJson::object([("case", expected.required("case").unwrap().clone())]);
        match index {
            0 => {
                actual.insert("afterConstructor", client.trace());
                let mut initialize = Box::pin(engine.initialize());
                poll_pending(initialize.as_mut());
                actual.insert("afterInitializeCall", client.trace());
                initialize.await.unwrap();
                let mut epoch = Box::pin(engine.runtime_epoch.get());
                poll_pending(epoch.as_mut());
                actual.insert("initializedBeforeEpoch", true.into());
                sender
                    .send(Ok(IvyJson::object([(
                        "runtimeEpoch",
                        "fixture-epoch".into(),
                    )])))
                    .unwrap();
                epoch.await.unwrap();
            }
            1 => {
                actual.insert(
                    "firstResult",
                    failure(engine.runtime_epoch.get().await.unwrap_err()),
                );
                actual.insert(
                    "again",
                    failure(engine.validate_operation_id(id).await.unwrap_err()),
                );
                let second = SecretaryEngine::new(
                    &evidence2,
                    &clock,
                    &clock,
                    ivy_secretary::engine::random_uuid,
                    &tools,
                    &issues,
                );
                actual.insert(
                    "secondResult",
                    IvyJson::String(second.runtime_epoch.get().await.unwrap()),
                );
                actual.insert(
                    "failedStill",
                    failure(engine.validate_operation_id(id).await.unwrap_err()),
                );
            }
            2 => {
                let second = SecretaryEngine::new(
                    &evidence2,
                    &clock,
                    &clock,
                    ivy_secretary::engine::random_uuid,
                    &tools,
                    &issues,
                );
                let abandoned = engine.runtime_epoch.clone();
                drop(engine);
                drop(evidence);
                drop(store);
                drop(journal);
                sender
                    .send(Ok(IvyJson::object([(
                        "runtimeEpoch",
                        "fixture-epoch".into(),
                    )])))
                    .unwrap();
                assert_eq!(
                    second.runtime_epoch.get().await.unwrap(),
                    "fixture-epoch".into()
                );
                assert_eq!(abandoned.get().await.unwrap(), "fixture-epoch".into());
            }
            3 => {
                let encoded = canonical(corpus.required("accepted").unwrap(), 1_048_576).unwrap();
                let name = operation_name(
                    corpus.required("caller").unwrap(),
                    corpus.required("id").unwrap(),
                )
                .unwrap()
                .to_utf8()
                .unwrap();
                journal
                    .db
                    .execute(
                        "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                        params![
                            "retained",
                            "secretary/operation",
                            name,
                            1,
                            0,
                            Option::<i64>::None,
                            encoded
                        ],
                    )
                    .unwrap();
                journal
                    .db
                    .execute(
                        "INSERT INTO revisions VALUES(?,?,?)",
                        params!["retained", 1, encoded],
                    )
                    .unwrap();
                let mut action = Box::pin(engine.action(
                    corpus.required("caller").unwrap(),
                    corpus.required("request").unwrap(),
                ));
                poll_pending(action.as_mut());
                actual.insert("pendingBeforeEpoch", true.into());
                sender
                    .send(Err(IvyError::new(
                        "fixture_epoch_failed",
                        "Original epoch failure.",
                    )
                    .with_outcome(Outcome::Unknown)))
                    .unwrap();
                actual.insert("outcome", failure(action.await.unwrap_err()));
            }
            4 => {
                let saved = expected.required("saved").unwrap();
                let name = operation_name(
                    corpus.required("caller").unwrap(),
                    corpus.required("id").unwrap(),
                )
                .unwrap()
                .to_utf8()
                .unwrap();
                journal
                    .create_technical("secretary/public-operation", &name, saved)
                    .unwrap();
                actual.insert("saved", saved.clone());
                actual.insert(
                    "outcome",
                    engine
                        .operation_read(
                            corpus.required("caller").unwrap(),
                            corpus.required("scope").unwrap(),
                            corpus.required("id").unwrap(),
                        )
                        .await
                        .unwrap(),
                );
                let mut epoch = Box::pin(engine.runtime_epoch.get());
                poll_pending(epoch.as_mut());
                actual.insert("completedBeforeEpoch", true.into());
                sender
                    .send(Ok(IvyJson::object([(
                        "runtimeEpoch",
                        "fixture-epoch".into(),
                    )])))
                    .unwrap();
                epoch.await.unwrap();
            }
            _ => unreachable!(),
        }
        actual.insert("trace", client.trace());
        assert_eq!(
            canonical(&actual, 4 * 1024 * 1024).unwrap(),
            canonical(expected, 4 * 1024 * 1024).unwrap(),
            "Constructor scenario {index}"
        );
    }
}
