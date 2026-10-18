use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    builtin_schema::SchemaBundle,
    checked_native_contract::CheckedNativeContracts,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    journal_store::{Contracts, JournalStore},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    native_evidence::NativeEvidence,
    native_intent::NativeIntent,
    native_plan::{read_plan, resolved_plan},
};
use std::{
    cell::RefCell,
    collections::HashSet,
    path::Path,
    process::Command,
    sync::{Arc, OnceLock},
};

fn text(value: &Json, key: &str) -> JsString {
    value.required(key).unwrap().as_js_string().unwrap().clone()
}
fn validate(name: &str, value: &Json) -> IvyResult<()> {
    // Exact frozen schema composition in this fixture, not production wiring for
    // the owner-finalized current TaskBoard contract/overlay.
    static BUNDLE: OnceLock<SchemaBundle> = OnceLock::new();
    let bundle = BUNDLE.get_or_init(|| {
        SchemaBundle::new(&[
            include_str!("../../specs/schemas/task-board.schema.json"),
            include_str!("../../specs/schemas/chat.schema.json"),
            include_str!("../../specs/schemas/agent.schema.json"),
            include_str!("../../specs/schemas/hive-wire.schema.json"),
        ])
        .unwrap()
    });
    canonical(value, MANAGEMENT_FRAME_BYTES)?;
    if bundle.matches(
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{name}"),
        value,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Value does not match its TaskBoard workflow contract.",
        ))
    }
}
struct Policy {
    trace: RefCell<Vec<Json>>,
    allowed_records: HashSet<String>,
}
impl Contracts for Policy {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        Ok(matches!(
            key,
            "task-board/native-plan"
                | "task-board/native-call"
                | "task-board/native-evidence"
                | "task-board/run"
        ))
    }
    fn readable_version(&self, _: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &JsString::from("1.0.0"))
    }
    fn validate_record(&self, key: &str, _: &JsString, value: &Json) -> IvyResult<()> {
        match key {
            "task-board/native-plan" => self.validate_definition("NativePlan", value),
            "task-board/native-evidence" => self.validate_definition("NativeEvidence", value),
            "task-board/run" | "task-board/native-call"
                if self.allowed_records.contains(&hash_json(value)?) =>
            {
                Ok(())
            }
            _ => Err(IvyError::new(
                "invalid_arguments",
                "Fixture record not admitted.",
            )),
        }
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        self.trace
            .borrow_mut()
            .push(Json::Array(vec!["validate".into(), name.into()]));
        validate(name, value)
    }
}
struct NoRpc;
impl RpcClient for NoRpc {
    fn request<'a>(&'a self, method: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        panic!("Inline native consumer unexpectedly made RPC: {method}")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn inline_native_consumers_match_original_catalogs_and_real_journal_admission() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-native-intent-oracle.mjs",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let groups = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let catalogs = CheckedNativeContracts::new(root);
    let mut count = 0;
    for group in groups.as_array().unwrap() {
        let base = group.required("base").unwrap();
        for row in group.required("rows").unwrap().as_array().unwrap() {
            count += 1;
            let input = row.required("input").unwrap();
            let database = Connection::open_in_memory().unwrap();
            local_journal::initialize(&database).unwrap();
            let root_id = input
                .get("root")
                .map(|value| value.as_js_string().cloned())
                .unwrap_or_else(|| Some("root".into()));
            let journal = LocalJournal {
                connection: &database,
                root_object_id: root_id.clone(),
            };
            let plan = input.get("plan").unwrap_or(base.required("plan").unwrap());
            let address = group.required("pin").unwrap();
            journal
                .create(
                    "task-board/native-plan",
                    &"plan".into(),
                    root_id.as_ref(),
                    plan,
                    &"2026-01-02T03:04:05.000Z".into(),
                    &text(address, "objectId"),
                )
                .unwrap();
            let store = JournalStore {
                journal,
                client: Arc::new(NoRpc),
                contracts: Policy {
                    trace: RefCell::default(),
                    allowed_records: HashSet::new(),
                },
                limits: HistoryLimits::default(),
                now: || String::from("2026-01-02T03:04:05.000Z"),
                new_id: || JsString::from("unused"),
                cache: None,
            };
            let before = database.total_changes();
            let subject = NativeIntent {
                store: &store,
                catalogs: &catalogs,
            };
            let result: IvyResult<Json> =
                match row.required("action").unwrap().as_str().unwrap().as_str() {
                    "resolved" => resolved_plan(&store, &catalogs, plan),
                    "read" => read_plan(&store, &catalogs, address).await,
                    "retain" => {
                        let mut request = input
                            .get("request")
                            .unwrap_or(base.required("request").unwrap())
                            .clone();
                        if input.get("large").and_then(Json::as_bool) == Some(true) {
                            let mut params = request.required("params").unwrap().clone();
                            params.insert("extra", "x".repeat(1048576).into());
                            request.insert("params", params);
                        }
                        subject.retain_native_request(
                            &request,
                            &input
                                .get("operation")
                                .and_then(Json::as_js_string)
                                .cloned()
                                .unwrap_or_else(|| "native-operation".into()),
                        )
                    }
                    "resolve" => subject.resolve_native_request(
                        input
                            .get("retained")
                            .unwrap_or(base.required("retained").unwrap()),
                    ),
                    "identity" => subject
                        .evidence_identity(
                            input.get("run").unwrap_or(base.required("run").unwrap()),
                            input.get("call").unwrap_or(base.required("call").unwrap()),
                            &input
                                .get("principal")
                                .and_then(Json::as_js_string)
                                .cloned()
                                .unwrap_or_else(|| "caller".into()),
                        )
                        .map(|identity| identity.to_json()),
                    other => panic!("Unexpected native consumer action {other}"),
                };
            let mut actual = match result {
                Ok(value) => Json::object([("value", value)]),
                Err(error) => Json::object([("error", error.to_wire())]),
            };
            actual.insert("trace", Json::Array(store.contracts.trace.into_inner()));
            assert_eq!(
                canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
                canonical(row.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
                "{} / {}",
                group.required("path").unwrap().as_str().unwrap(),
                row.required("name").unwrap().as_str().unwrap()
            );
            assert_eq!(
                database.total_changes(),
                before,
                "Inline native consumers changed their journal"
            );
        }
    }
    assert!(count >= 140);
}

#[tokio::test(flavor = "current_thread")]
async fn operation_evidence_replays_and_rejects_corruption_in_its_real_journal() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-native-evidence-oracle.mjs",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let base = data.required("base").unwrap();
    let run = base.required("run").unwrap();
    let call = base.required("call").unwrap();
    let observation = base.required("observation").unwrap();
    let database = Connection::open_in_memory().unwrap();
    local_journal::initialize(&database).unwrap();
    let journal = LocalJournal {
        connection: &database,
        root_object_id: Some("root".into()),
    };
    let parent = base.required("parent").unwrap();
    journal
        .create(
            "task-board/run",
            &"Run".into(),
            Some(&"root".into()),
            run,
            &"2026-01-02T03:04:05.000Z".into(),
            &text(parent, "objectId"),
        )
        .unwrap();
    let store = JournalStore {
        journal,
        client: Arc::new(NoRpc),
        contracts: Policy {
            trace: RefCell::default(),
            allowed_records: [hash_json(run).unwrap(), hash_json(call).unwrap()].into(),
        },
        limits: HistoryLimits::default(),
        now: || String::from("2026-01-02T03:04:05.000Z"),
        new_id: || JsString::from(uuid::Uuid::new_v4().to_string()),
        cache: None,
    };
    let catalogs = CheckedNativeContracts::new(root);
    let identity = NativeIntent {
        store: &store,
        catalogs: &catalogs,
    }
    .evidence_identity(run, call, &"caller".into())
    .unwrap();
    let evidence = NativeEvidence {
        store: &store,
        catalogs: &catalogs,
    };
    let artifact = evidence.save(parent, observation, &identity).await.unwrap();
    assert_eq!(
        canonical(
            &evidence.read(parent, &artifact, &identity).await.unwrap(),
            MANAGEMENT_FRAME_BYTES
        )
        .unwrap(),
        canonical(observation, MANAGEMENT_FRAME_BYTES).unwrap()
    );
    assert_eq!(
        evidence.save(parent, observation, &identity).await.unwrap(),
        artifact
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM documents", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let (blob_id, bytes): (String, Vec<u8>) = database
        .query_row("SELECT id,bytes FROM blobs", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    let mut changed = bytes.clone();
    changed[0] ^= 1;
    database
        .execute(
            "UPDATE blobs SET bytes=? WHERE id=?",
            ivy_store::rusqlite::params![changed, blob_id],
        )
        .unwrap();
    assert_eq!(
        evidence
            .read(parent, &artifact, &identity)
            .await
            .unwrap_err()
            .code,
        "task_board_evidence_mismatch"
    );
    database
        .execute(
            "UPDATE blobs SET bytes=? WHERE id=?",
            ivy_store::rusqlite::params![bytes, blob_id],
        )
        .unwrap();
    let mut wrong = artifact.clone();
    wrong.insert("mediaType", "text/plain".into());
    assert_eq!(
        evidence
            .read(parent, &wrong, &identity)
            .await
            .unwrap_err()
            .code,
        "task_board_evidence_mismatch"
    );
    let mut wrong = identity.clone();
    wrong.caller_principal_id = "other-caller".into();
    assert_eq!(
        evidence
            .read(parent, &artifact, &wrong)
            .await
            .unwrap_err()
            .code,
        "task_board_evidence_mismatch"
    );
    assert!(database.is_autocommit());
}
