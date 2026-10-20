use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    builtin_schema::SchemaBundle,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    journal_store::{
        Contracts, JournalStore,
        durable_admission::{DurableAdmission, DurableContracts},
    },
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    operations::{Operations, Store},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    future::Future,
    path::PathBuf,
    pin::pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake},
};

fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn items(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn string<'a>(v: &'a Json, key: &str) -> &'a JsString {
    v.required(key).unwrap().as_js_string().unwrap()
}
fn same(a: &Json, b: &Json, label: &str) {
    assert_eq!(
        canonical(a, MANAGEMENT_FRAME_BYTES).unwrap(),
        canonical(b, MANAGEMENT_FRAME_BYTES).unwrap(),
        "{label}"
    );
}
fn fixture() -> Json {
    let path = std::env::var_os("IVY_TASK_BOARD_DURABLE_FIXTURE")
        .expect("Supply the qualified current durable fixture artifact");
    parse(
        &std::fs::read_to_string(path).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn bundle() -> SchemaBundle {
    let root = PathBuf::from(
        std::env::var_os("IVY_TASK_BOARD_CURRENT_CONTRACTS")
            .expect("Supply the actual complete canonical repository schema directory"),
    );
    let docs: Vec<&'static str> = [
        "task-board",
        "chat",
        "agent",
        "hive-wire",
        "hive-operations",
    ]
    .into_iter()
    .map(|name| {
        let text = std::fs::read_to_string(root.join(format!("{name}.schema.json"))).unwrap();
        &*Box::leak(text.into_boxed_str())
    })
    .collect();
    SchemaBundle::new(Box::leak(docs.into_boxed_slice())).unwrap()
}
fn error_value(e: IvyError) -> Json {
    Json::object([
        ("code", Json::String(e.code_js())),
        ("message", Json::String(e.message_js())),
        ("outcome", e.outcome.as_str().into()),
        ("details", e.details.unwrap_or(Json::Null)),
    ])
}
fn observed(result: IvyResult<()>) -> Json {
    match result {
        Ok(()) => Json::object([("valid", true.into()), ("error", Json::Null)]),
        Err(e) => Json::object([("valid", false.into()), ("error", error_value(e))]),
    }
}
struct Retained {
    schemas: SchemaBundle,
}
impl Contracts for Retained {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, _: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &"1.0.0".into())
    }
    fn validate_record(&self, key: &str, _: &JsString, value: &Json) -> IvyResult<()> {
        assert_ne!(
            key, "task-board/operation",
            "Current Operation must use the native durable binding"
        );
        assert_eq!(key, "task-board/history");
        assert!(self.schemas.matches(
            "https://ivy.invalid/schemas/task-board.schema.json#/$defs/History",
            value
        )?);
        Ok(())
    }
    fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
        panic!("Current durable definitions cannot fall through to a retained guard")
    }
}

#[test]
fn current_native_durable_guard_matches_qualified_owner() {
    let fixture = fixture();
    let current = DurableAdmission::new(bundle());
    let retained = Retained { schemas: bundle() };
    let contracts = DurableContracts {
        retained: &retained,
        current: &current,
    };
    let mut count = 0;
    for case in items(field(&fixture, "cases")) {
        let name = string(case, "name").to_utf8().unwrap();
        let definition = string(case, "definition").to_utf8().unwrap();
        let value = field(case, "value");
        let before = value.clone();
        let actual = observed(contracts.validate_definition(&definition, value));
        let mut expected = field(case, "expected").clone();
        if let Some(error) = expected.get("error").filter(|e| !e.is_null()).cloned() {
            let mut error = error;
            if error.get("details").is_none() {
                error.insert("details", Json::Null);
            }
            expected.insert("error", error);
        }
        same(&actual, &expected, &name);
        if definition == "Operation" {
            same(
                &observed(contracts.validate_record(
                    "task-board/operation",
                    &"1.0.0".into(),
                    value,
                )),
                &expected,
                &format!("{name} record binding"),
            );
        }
        same(value, &before, &format!("{name} unchanged input"));
        if let Some(hash) = case.get("valueHash") {
            assert_eq!(&Json::from(hash_json(value).unwrap()), hash);
        }
        count += 1;
    }
    assert_eq!(count, 72);
}

struct Probe {
    error: IvyError,
    calls: RefCell<Vec<Json>>,
}
impl Probe {
    fn record(&self, method: &str, value: Json) {
        self.calls
            .borrow_mut()
            .push(Json::Array(vec![method.into(), value]));
    }
}
impl Contracts for Probe {
    fn version(&self, key: &str) -> IvyResult<String> {
        self.record("version", key.into());
        Err(self.error.clone())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        self.record("local", key.into());
        Err(self.error.clone())
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        self.record(
            "readable",
            Json::Array(vec![key.into(), Json::String(version.clone())]),
        );
        Err(self.error.clone())
    }
    fn validate_record(&self, key: &str, version: &JsString, value: &Json) -> IvyResult<()> {
        self.record(
            "record",
            Json::Array(vec![
                key.into(),
                Json::String(version.clone()),
                value.clone(),
            ]),
        );
        Err(self.error.clone())
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        self.record("definition", Json::Array(vec![name.into(), value.clone()]));
        Err(self.error.clone())
    }
}
#[test]
fn durable_binding_preserves_retained_routes_versions_and_errors() {
    let current = DurableAdmission::new(bundle());
    let value = Json::object([("unchanged", Json::String(JsString(vec![0xd800])))]);
    let sentinel = IvyError::new_js("retained_unavailable".into(), JsString(vec![0xdfff]))
        .with_outcome(Outcome::Unknown)
        .with_details(value.clone());
    let probe = Probe {
        error: sentinel.clone(),
        calls: RefCell::default(),
    };
    let contracts = DurableContracts {
        retained: &probe,
        current: &current,
    };
    let version = JsString(vec![118, 45, 0xd800]);
    let failures = [
        contracts
            .version("task-board/operation")
            .map(|_| ())
            .unwrap_err(),
        contracts
            .local("task-board/operation")
            .map(|_| ())
            .unwrap_err(),
        contracts
            .readable_version("task-board/operation", &version)
            .map(|_| ())
            .unwrap_err(),
        contracts
            .validate_record("task-board/operation", &version, &value)
            .unwrap_err(),
        contracts
            .validate_record("task-board/history", &"1.0.0".into(), &value)
            .unwrap_err(),
        contracts
            .validate_definition("History", &value)
            .unwrap_err(),
    ];
    for error in failures {
        assert_eq!(error, sentinel);
    }
    assert_eq!(probe.calls.borrow().len(), 6);
}

struct Client {
    epoch: Json,
    calls: Mutex<Vec<Json>>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(Json::Array(vec![method.into(), params.clone()]));
            assert_eq!(
                method, "system.status",
                "No remote/native service effect is allowed"
            );
            same(
                &params,
                &Json::object([] as [(&str, Json); 0]),
                "original status request",
            );
            Ok(Json::object([("runtimeEpoch", self.epoch.clone())]))
        })
    }
}
struct NoWake;
impl Wake for NoWake {
    fn wake(self: Arc<Self>) {}
}
fn complete<T>(work: impl Future<Output = T>) -> T {
    let mut work = pin!(work);
    let waker = Arc::new(NoWake).into();
    for _ in 0..256 {
        if let Poll::Ready(v) = work.as_mut().poll(&mut Context::from_waker(&waker)) {
            return v;
        }
    }
    panic!("Finite durable journal fixture unexpectedly blocked")
}
fn revisions(db: &Connection) -> i64 {
    db.query_row("SELECT count(*) FROM revisions", [], |r| r.get(0))
        .unwrap()
}
fn refusal(fixture: &Json, label: &str, error: IvyError) {
    let source = items(field(field(fixture, "protocol"), "refusals"))
        .iter()
        .find(|r| field(r, "label") == &Json::from(label))
        .unwrap();
    let mut expected = source.clone();
    expected.remove("label");
    same(&error_value(error), &expected, label);
}

#[tokio::test(flavor = "current_thread")]
async fn current_admission_binds_local_journal_and_retained_replay() {
    let fixture = fixture();
    let protocol = field(&fixture, "protocol");
    let pending = field(protocol, "pending");
    let current = DurableAdmission::new(bundle());
    let retained = Retained { schemas: bundle() };
    let contracts = DurableContracts {
        retained: &retained,
        current: &current,
    };
    let rows = items(field(protocol, "accepted"));
    let mut identities = rows
        .iter()
        .map(|r| string(field(r, "pin"), "objectId").clone())
        .collect::<VecDeque<_>>();
    identities.push_back(
        string(
            field(field(pending, "entry"), "payload")
                .required("object")
                .unwrap(),
            "objectId",
        )
        .clone(),
    );
    identities.push_back(string(field(pending, "effect"), "objectId").clone());
    let identities = RefCell::new(identities);
    let clock = RefCell::new(String::new());
    let client = Arc::new(Client {
        epoch: field(protocol, "epoch").clone(),
        calls: Mutex::default(),
    });
    let path = PathBuf::from(
        std::env::var_os("IVY_TASK_BOARD_DURABLE_JOURNAL")
            .expect("Supply a new ignored native journal evidence path"),
    );
    assert!(!path.exists(), "Never overwrite an earlier journal attempt");
    let mut snapshots = Vec::new();
    macro_rules! bound {
        ($db:expr) => {
            Operations {
                store: JournalStore {
                    journal: LocalJournal {
                        connection: $db,
                        root_object_id: Some("root".into()),
                    },
                    client: client.clone(),
                    contracts: &contracts,
                    limits: HistoryLimits::default(),
                    now: || clock.borrow().clone(),
                    new_id: || {
                        identities
                            .borrow_mut()
                            .pop_front()
                            .unwrap_or_else(|| "unused-create-candidate".into())
                    },
                    cache: None,
                },
                now: || clock.borrow().clone(),
            }
        };
    }
    let id = string(field(field(pending, "row"), "pin"), "objectId");
    let outcome = field(field(pending, "completed"), "outcome");
    {
        let db = Connection::open(&path).unwrap();
        initialize(&db).unwrap();
        let ops = bound!(&db);
        for row in rows {
            let value = field(row, "value");
            *clock.borrow_mut() = string(value, "createdAt").to_utf8().unwrap();
            contracts
                .validate_definition("DurableRequest", field(value, "request"))
                .unwrap();
            let saved =
                complete(ops.begin(field(value, "request"), field(value, "actor"))).unwrap();
            same(&saved.value, value, "original accepted Operation");
            same(&saved.pin, field(row, "pin"), "original operation pin");
            snapshots.push(saved.value);
        }
        let value = field(pending, "accepted");
        let mut altered = field(value, "request").clone();
        altered.insert("archived", true.into());
        refusal(
            &fixture,
            "changed request same caller/id",
            complete(ops.begin(&altered, field(value, "actor"))).unwrap_err(),
        );
        *clock.borrow_mut() = string(field(pending, "prepared"), "updatedAt")
            .to_utf8()
            .unwrap();
        // Seed the exact retained preparation through real record admission.
        // Fresh staging necessarily allocates a new clock/nonce identity; replay
        // must instead consume the already persisted original mutation ID.
        let entry = field(pending, "entry").clone();
        let payload_destination = Json::object([(
            "create",
            Json::object([
                ("parentId", Json::String(id.clone())),
                ("name", field(field(&entry, "payload"), "label").clone()),
            ]),
        )]);
        let payload = complete(ops.store.write(
            "task-board/history",
            field(pending, "payload"),
            "seed-retained-payload",
            &payload_destination,
        ))
        .unwrap();
        same(
            &payload,
            field(field(&entry, "payload"), "object"),
            "retained payload pin",
        );
        let destination = Json::object([
            ("objectId", Json::String(id.clone())),
            ("expectedRevision", Json::Number(1.0)),
        ]);
        complete(ops.store.write(
            "task-board/operation",
            field(pending, "prepared"),
            "seed-retained-preparation",
            &destination,
        ))
        .unwrap();
        let saved = complete(ops.store.read(
            "task-board/operation",
            &Json::String(id.clone()),
            true,
        ))
        .unwrap();
        same(
            &saved.value,
            field(pending, "prepared"),
            "original prepared Operation",
        );
        snapshots.push(saved.value);
        refusal(
            &fixture,
            "unfinished completion without receipt",
            complete(ops.finish(id, outcome)).unwrap_err(),
        );
        let replacement = Json::object([(
            "create",
            Json::object([
                ("parentId", Json::String(id.clone())),
                ("name", "different".into()),
            ]),
        )]);
        refusal(
            &fixture,
            "stage destination replacement",
            complete(ops.stage(
                id,
                0.0,
                "task-board/history",
                &replacement,
                field(pending, "payload"),
            ))
            .unwrap_err(),
        );
        let effect = complete(ops.store.write_prepared(
            "task-board/history",
            field(pending, "payload"),
            string(&entry, "mutationId"),
            field(&entry, "destination"),
        ))
        .unwrap();
        same(
            &effect,
            field(pending, "effect"),
            "original receipt-less local create",
        );
        let invalid = items(field(&fixture, "cases"))
            .iter()
            .find(|c| field(c, "name") == &Json::from("unknown Operation field"))
            .unwrap();
        let before = revisions(&db);
        let destination = Json::object([
            ("objectId", Json::String(id.clone())),
            ("expectedRevision", Json::Number(2.0)),
        ]);
        let error = complete(ops.store.write(
            "task-board/operation",
            field(invalid, "value"),
            "malformed-record",
            &destination,
        ))
        .unwrap_err();
        assert_eq!(error.code, "invalid_arguments");
        assert_eq!(
            revisions(&db),
            before,
            "Admission must precede any journal mutation"
        );
    }
    let rpc_before = client.calls.lock().unwrap().len();
    {
        let db = Connection::open(&path).unwrap();
        initialize(&db).unwrap();
        let ops = bound!(&db);
        for row in rows {
            let source = field(row, "value");
            let latest = complete(
                ops.store
                    .read("task-board/operation", field(row, "pin"), true),
            )
            .unwrap();
            same(
                &latest.value,
                source,
                "retained immutable Operation after reopen",
            );
            let mut actor = field(source, "actor").clone();
            actor.insert("generation", Json::Number(2.0));
            actor.insert("serviceNodeId", "restarted-connection".into());
            let resumed = complete(ops.begin(field(source, "request"), &actor)).unwrap();
            same(
                field(&resumed.value, "actor"),
                field(source, "actor"),
                "original actor remains authoritative",
            );
        }
        *clock.borrow_mut() = string(field(pending, "publishing"), "updatedAt")
            .to_utf8()
            .unwrap();
        let before_docs: i64 = db
            .query_row("SELECT count(*) FROM documents", [], |r| r.get(0))
            .unwrap();
        let effect = complete(ops.apply(id, 0)).unwrap();
        same(
            &effect,
            field(pending, "effect"),
            "recovered original destination",
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            before_docs
        );
        let saved = complete(ops.store.read(
            "task-board/operation",
            &Json::String(id.clone()),
            true,
        ))
        .unwrap();
        same(
            &saved.value,
            field(pending, "publishing"),
            "original publishing receipt",
        );
        snapshots.push(saved.value);
        *clock.borrow_mut() = string(field(pending, "completed"), "updatedAt")
            .to_utf8()
            .unwrap();
        complete(ops.finish(id, outcome)).unwrap();
        let saved = complete(ops.store.read(
            "task-board/operation",
            &Json::String(id.clone()),
            true,
        ))
        .unwrap();
        same(
            &saved.value,
            field(pending, "completed"),
            "original completed Operation",
        );
        snapshots.push(saved.value);
        let before = revisions(&db);
        complete(ops.finish(id, outcome)).unwrap();
        assert_eq!(
            revisions(&db),
            before,
            "Completed replay must add no revision"
        );
        let mut wrong = outcome.clone();
        wrong.insert("operationId", "different-id".into());
        refusal(
            &fixture,
            "outcome identity change",
            complete(ops.finish(id, &wrong)).unwrap_err(),
        );
        let mut wrong = outcome.clone();
        wrong.insert(
            "task",
            Json::object([
                ("objectId", "different".into()),
                ("revision", Json::Number(1.0)),
            ]),
        );
        refusal(
            &fixture,
            "completed outcome replacement",
            complete(ops.finish(id, &wrong)).unwrap_err(),
        );
        assert_eq!(
            client.calls.lock().unwrap().len(),
            rpc_before,
            "Restart uses the persisted mutation identity"
        );
        assert_eq!(
            db.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
    let evidence = Json::object([
        ("snapshots", Json::Array(snapshots)),
        ("rpc", Json::Array(client.calls.lock().unwrap().clone())),
        ("protocolRefusals", Json::Number(5.0)),
        ("fixtureOnly", true.into()),
        ("nativeOrRemoteEffects", false.into()),
    ]);
    std::fs::write(
        path.with_extension("json"),
        canonical(&evidence, MANAGEMENT_FRAME_BYTES).unwrap(),
    )
    .unwrap();
}
