//! Exact Hive pins, current-only ledger retention and lost publication replies.
use super::*;
use ivy_contracts::IvyResult;
use ivy_task_board::revision_protection::protect_run_sources;
use std::collections::BTreeMap;

const KEY: &str = "task-board/revision-protection";
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    v.required(key).unwrap()
}
fn p(id: &str, revision: u64) -> Json {
    Json::object([
        ("objectId", id.into()),
        ("revision", Json::Number(revision as f64)),
    ])
}
fn run(task: &str) -> Json {
    let mut value = json(
        r#"{"schemaVersion":1,"taskId":"task","attempt":1,"operationId":"original","callerPrincipalId":"board","originalRequest":{"action":"continue","operationId":"original","taskId":"task","expectedRevision":7,"feedback":"Continue original work","target":{"hostId":"host","serviceNodeId":"host.agent"},"intent":{"objectId":"plan","revision":1},"workspace":{"hostId":"host","serviceNodeId":"host.agent","canonicalCwd":"/workspace","taskRoot":"/tasks","bootstrapPath":"/tasks/task","intendedPath":"/workspace","sourcePath":null,"project":null,"useWorktree":false,"repository":null},"commentIds":[]},"requestHash":null,"target":{"hostId":"host","serviceNodeId":"host.agent"},"workspace":null,"plan":{"nativeVersion":"1.0.0","catalogSourceHash":null,"definitions":{},"threadStart":{},"threadResume":{},"turnStart":{}},"owner":{"principalId":"board","serviceNodeId":"host.board","generation":1,"source":"scheduler"},"phase":"running","primaryResourceRef":null,"turnId":null,"nativeEpoch":null,"calls":{"thread":null,"turn":null,"interrupt":null},"notificationCursor":0,"result":null,"cancellation":null,"externalOutcome":{"state":"unknown","code":null,"evidence":null},"createdAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","finishedAt":null}"#,
    );
    value.insert("taskId", task.into());
    field_mut(&mut value, "externalOutcome").insert("observedAt", Json::Null);
    value.insert("primaryResourceRef", json(r#"{"kind":"thread","namespace":"codex","nativeId":"thread","serviceNodeId":"host.agent"}"#));
    value.insert("turnId", "turn".into());
    value.insert("nativeEpoch", "epoch".into());
    field_mut(&mut value, "originalRequest").insert("taskId", task.into());
    value.insert(
        "workspace",
        f(f(&value, "originalRequest"), "workspace").clone(),
    );
    value.insert(
        "requestHash",
        hash_json(f(&value, "originalRequest")).unwrap().into(),
    );
    let plan = field_mut(&mut value, "plan");
    let hash = digest(b"fixture-catalog");
    plan.insert("catalogSourceHash", hash.clone().into());
    plan.insert(
        "definitions",
        Json::object(
            ["threadStart", "threadResume", "turnStart", "turnInterrupt"]
                .map(|k| (k, hash.clone().into())),
        ),
    );
    value
}
fn versioned(id: &str, key: &str, value: Json, root: &Json, revision: u64) -> Json {
    let mut record = record(
        id,
        key,
        value,
        root.clone(),
        Json::Null,
        Json::Object(vec![]),
    );
    field_mut(&mut record, "revision").insert(
        "contractVersion",
        if key == KEY {
            "1.0.0"
        } else if key == "task-board/run" {
            "3.0.0"
        } else {
            "3.1.0"
        }
        .into(),
    );
    field_mut(&mut record, "revision").insert("revision", Json::Number(revision as f64));
    field_mut(&mut record, "object").insert("currentRevision", Json::Number(revision as f64));
    record
}
fn finish(record: &mut Json, phase: &str) {
    let value = field_mut(field_mut(record, "content"), "value");
    value.insert("phase", phase.into());
    value.insert("finishedAt", clock().into());
    let outcome = field_mut(value, "externalOutcome");
    outcome.insert(
        "state",
        if phase == "completed" {
            "succeeded"
        } else {
            phase
        }
        .into(),
    );
    outcome.insert("observedAt", clock().into());
    if phase == "completed" {
        value.insert("result", p("result", 1));
    } else if phase == "cancelled" {
        value.insert(
            "cancellation",
            Json::object([
                ("operationId", "original-cancellation".into()),
                ("reason", "Stop original work".into()),
                ("requestedAt", clock().into()),
                ("actor", f(value, "owner").clone()),
            ]),
        );
    }
}
fn predicate(record: &Json, clause: &Json) -> bool {
    let op = f(clause, "op").as_str().unwrap();
    match op.as_str() {
        "and" => f(clause, "args")
            .as_array()
            .unwrap()
            .iter()
            .all(|v| predicate(record, v)),
        "not" => !predicate(record, f(clause, "arg")),
        _ => {
            let field = f(clause, "field").as_str().unwrap();
            let selected = if let Some(field) = field.strip_prefix("object.") {
                f(record, "object").get(field)
            } else {
                f(f(record, "content"), "value").get(field.strip_prefix("data:/").unwrap())
            };
            match op.as_str() {
                "eq" => selected == clause.get("value"),
                "isNull" => selected.is_some_and(Json::is_null),
                "in" => f(clause, "value")
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| Some(v) == selected),
                "contains" => selected
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .contains(&f(clause, "value").as_str().unwrap()),
                _ => panic!("Unexpected query predicate {op}"),
            }
        }
    }
}
#[derive(Default)]
struct ProtectionState {
    records: BTreeMap<String, Json>,
    history: BTreeMap<(String, u64), Json>,
    writes: usize,
    pages: Vec<(String, usize, bool)>,
    read_failure: Option<(String, u64, String)>,
    prune_before_write: bool,
    lose_reply: bool,
}
#[derive(Default)]
struct ProtectionClient(Mutex<ProtectionState>);
impl ProtectionClient {
    fn change_run(&self, id: &str, change: impl FnOnce(&mut Json)) {
        let mut state = self.0.lock().unwrap();
        let prior = state.records[id].clone();
        let revision = f(f(&prior, "revision"), "revision").as_f64().unwrap() as u64;
        let mut next = prior.clone();
        change(&mut next);
        let value = f(f(&next, "content"), "value");
        let content_hash = hash_json(value).unwrap();
        let byte_length = stringify(value, usize::MAX).unwrap().len();
        field_mut(&mut next, "revision").insert("contentHash", content_hash.into());
        field_mut(&mut next, "revision").insert("byteLength", Json::Number(byte_length as f64));
        field_mut(&mut next, "revision").insert("revision", Json::Number((revision + 1) as f64));
        field_mut(&mut next, "object")
            .insert("currentRevision", Json::Number((revision + 1) as f64));
        state.history.insert((id.into(), revision), prior);
        state.records.insert(id.into(), next);
    }
    fn original_hashes(&self) -> BTreeMap<String, String> {
        self.0
            .lock()
            .unwrap()
            .records
            .iter()
            .filter(|(_, r)| f(f(r, "object"), "contractKey") != &Json::from(KEY))
            .map(|(id, record)| (id.clone(), hash_json(record).unwrap()))
            .collect()
    }
    fn prune_source(&self) -> IvyResult<()> {
        let mut state = self.0.lock().unwrap();
        if state.records.values().any(|r| {
            f(f(r, "revision"), "references")
                .as_object()
                .unwrap()
                .iter()
                .any(|(_, pin)| hash_json(pin).unwrap() == hash_json(&p("task", 7)).unwrap())
        }) {
            return Err(IvyError::new(
                "target_conflict",
                "Exact source pin retained.",
            ));
        }
        state.history.remove(&("task".into(), 7));
        Ok(())
    }
    fn ledgers(&self) -> Vec<Json> {
        self.0
            .lock()
            .unwrap()
            .records
            .values()
            .filter(|r| f(f(r, "object"), "contractKey") == &Json::from(KEY))
            .cloned()
            .collect()
    }
}
impl RpcClient for ProtectionClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "system.status" {
                return Ok(Json::object([("runtimeEpoch", "fixture-epoch".into())]));
            }
            let mut state = self.0.lock().unwrap();
            match method {
                "objects.query" => {
                    let key = f(&params, "contractKey").as_str()?;
                    assert_eq!(f(&params, "includeArchived"), &true.into());
                    assert_eq!(f(&params, "limit"), &Json::Number(100.0));
                    let after = params.get("cursor").map(Json::as_str).transpose()?;
                    let limit = f(&params, "limit").as_f64().unwrap() as usize;
                    let matches = state
                        .records
                        .iter()
                        .filter(|(id, r)| {
                            after
                                .as_ref()
                                .is_none_or(|after| id.as_str() > after.as_str())
                                && f(f(r, "object"), "contractKey") == &Json::from(key.clone())
                                && predicate(r, f(&params, "where"))
                        })
                        .map(|(id, _)| id.clone())
                        .collect::<Vec<_>>();
                    let rows: Vec<_> = matches
                        .iter()
                        .take(limit)
                        .map(|id| {
                            Json::object([
                                ("objectId", id.clone().into()),
                                (
                                    "revision",
                                    f(f(&state.records[id], "revision"), "revision").clone(),
                                ),
                            ])
                        })
                        .collect();
                    let next = if matches.len() > limit {
                        matches[limit - 1].clone().into()
                    } else {
                        Json::Null
                    };
                    state.pages.push((key, rows.len(), !next.is_null()));
                    Ok(Json::object([
                        ("items", Json::Array(rows)),
                        ("nextCursor", next),
                    ]))
                }
                "objects.read" => {
                    let id = f(&params, "objectId").as_str()?;
                    let revision = params.get("revision").map(|v| v.as_f64().unwrap() as u64);
                    if let Some((failed_id, failed_revision, code)) = &state.read_failure
                        && failed_id == &id
                        && (revision == Some(*failed_revision)
                            || *failed_revision == 0 && revision.is_none())
                    {
                        return Err(IvyError::new(code, "Original source read failed."));
                    }
                    let record = match revision {
                        Some(rev)
                            if state.records.get(&id).is_none_or(|r| {
                                f(f(r, "revision"), "revision") != &Json::Number(rev as f64)
                            }) =>
                        {
                            state.history.get(&(id, rev)).ok_or_else(|| {
                                IvyError::new(
                                    "revision_pruned",
                                    "Exact source revision unavailable.",
                                )
                            })?
                        }
                        _ => state
                            .records
                            .get(&id)
                            .ok_or_else(|| IvyError::new("not_found", "No record."))?,
                    };
                    Ok(record.clone())
                }
                "objects.write" => {
                    if state.prune_before_write {
                        state.prune_before_write = false;
                        state.history.remove(&("task".into(), 7));
                    }
                    let references = f(&params, "references");
                    for (_, pin) in references.as_object().unwrap() {
                        let id = f(pin, "objectId").as_str()?;
                        let revision = f(pin, "revision").as_f64().unwrap() as u64;
                        if !state.records.get(&id).is_some_and(|r| {
                            f(f(r, "revision"), "revision") == &Json::Number(revision as f64)
                        }) && !state.history.contains_key(&(id, revision))
                        {
                            return Err(IvyError::new(
                                "revision_pruned",
                                "A reference target disappeared before publication.",
                            ));
                        }
                    }
                    let (id, mut saved) = if let Some(create) = params.get("create") {
                        assert_eq!(f(create, "contractKey"), &Json::from(KEY));
                        let id = format!("ledger-{:04}", state.writes);
                        (
                            id.clone(),
                            versioned(
                                &id,
                                KEY,
                                f(f(&params, "content"), "value").clone(),
                                f(create, "parentId"),
                                1,
                            ),
                        )
                    } else {
                        let id = f(&params, "objectId").as_str()?;
                        let prior = &state.records[&id];
                        assert_eq!(
                            params.get("expectedRevision"),
                            f(prior, "revision").get("revision")
                        );
                        assert_eq!(
                            params.get("expectedArchived"),
                            f(prior, "object").get("effectivelyArchived")
                        );
                        let mut next = prior.clone();
                        field_mut(&mut next, "content")
                            .insert("value", f(f(&params, "content"), "value").clone());
                        let rev = f(f(prior, "revision"), "revision").as_f64().unwrap() + 1.0;
                        field_mut(&mut next, "revision").insert("revision", Json::Number(rev));
                        field_mut(&mut next, "object").insert("currentRevision", Json::Number(rev));
                        (id, next)
                    };
                    if let Some(create) = params.get("create") {
                        field_mut(&mut saved, "object").insert("name", f(create, "name").clone());
                    }
                    field_mut(&mut saved, "revision").insert("references", references.clone());
                    assert_eq!(f(&params, "contractVersion"), &Json::from("1.0.0"));
                    state.records.insert(id, saved.clone());
                    state.writes += 1;
                    if state.lose_reply {
                        state.lose_reply = false;
                        return Err(IvyError::new(
                            "connection_closed",
                            "Lost committed ledger reply.",
                        )
                        .with_outcome(Outcome::Unknown));
                    }
                    Ok(saved)
                }
                _ => panic!(
                    "Source protection must not dispatch workflow or native effects: {method}"
                ),
            }
        })
    }
}
fn initialize_sources(client: &ProtectionClient, root: &Json, count: usize) {
    let mut state = client.0.lock().unwrap();
    state.records.insert(
        "task".into(),
        versioned("task", "task-board/task", task(), root, 8),
    );
    state.history.insert(
        ("task".into(), 7),
        versioned("task", "task-board/task", task(), root, 7),
    );
    for n in 0..count {
        let id = format!("run-{n:04}");
        let mut record = versioned(&id, "task-board/run", run("task"), root, 1);
        if n % 2 == 0 {
            field_mut(&mut record, "object").insert("effectivelyArchived", true.into());
        }
        state.records.insert(id, record);
    }
}
fn store<'a>(
    db: &'a Connection,
    client: Arc<ProtectionClient>,
    contracts: &'a RuntimeContracts,
    root: Option<JsString>,
) -> JournalStore<'a, ProtectionClient, &'a RuntimeContracts, fn() -> String, fn() -> JsString> {
    initialize(db).unwrap();
    JournalStore {
        journal: LocalJournal {
            connection: db,
            root_object_id: root,
        },
        client,
        contracts,
        limits: HistoryLimits::default(),
        now: clock,
        new_id: id,
        cache: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_sources_survive_paging_archival_restart_and_then_release_without_run_rewrites() {
    let contracts = ivy_task_board::runtime_inputs::contracts().unwrap();
    for root in [Json::Null, Json::from("workspace-root")] {
        let client = Arc::new(ProtectionClient::default());
        initialize_sources(&client, &root, 105);
        let originals = client.original_hashes();
        for _ in 0..2 {
            let db = Connection::open_in_memory().unwrap();
            let store = store(
                &db,
                client.clone(),
                &contracts,
                root.as_js_string().cloned(),
            );
            let changes = db.total_changes();
            assert_eq!(protect_run_sources(&store).await.unwrap(), 0);
            assert_eq!(changes, db.total_changes());
        }
        assert_eq!(client.original_hashes(), originals);
        assert_eq!(client.0.lock().unwrap().writes, 105);
        assert_eq!(client.ledgers().len(), 105);
        assert!(
            client
                .0
                .lock()
                .unwrap()
                .pages
                .iter()
                .any(|(key, n, next)| key == "task-board/run" && *n == 100 && *next)
        );
        assert_eq!(client.prune_source().unwrap_err().code, "target_conflict");
        for ledger in client.ledgers() {
            let pair = &f(f(&ledger, "content"), "value")
                .required("sources")
                .unwrap()
                .as_array()
                .unwrap()[0];
            assert_eq!(f(pair, "source"), &p("task", 7));
            assert_eq!(
                f(f(&ledger, "revision"), "references").get("source"),
                Some(&p("task", 7))
            );
            contracts
                .validate_record(KEY, &"1.0.0".into(), f(f(&ledger, "content"), "value"))
                .unwrap();
        }
        for n in 0..105 {
            client.change_run(&format!("run-{n:04}"), |record| {
                finish(record, ["completed", "failed", "cancelled"][n % 3]);
            });
        }
        let terminal = client.original_hashes();
        let db = Connection::open_in_memory().unwrap();
        let store = store(
            &db,
            client.clone(),
            &contracts,
            root.as_js_string().cloned(),
        );
        assert_eq!(protect_run_sources(&store).await.unwrap(), 0);
        assert_eq!(client.original_hashes(), terminal);
        assert!(client.ledgers().iter().all(|r| {
            f(f(r, "revision"), "references")
                .as_object()
                .unwrap()
                .is_empty()
        }));
        client.prune_source().unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn committed_lost_reply_reuses_ledger_and_self_protected_run_releases_it() {
    let contracts = ivy_task_board::runtime_inputs::contracts().unwrap();
    let client = Arc::new(ProtectionClient::default());
    initialize_sources(&client, &Json::Null, 1);
    client.0.lock().unwrap().lose_reply = true;
    let original = client.original_hashes();
    let db = Connection::open_in_memory().unwrap();
    let store = store(&db, client.clone(), &contracts, None);
    assert_eq!(
        protect_run_sources(&store).await.unwrap_err().outcome,
        Outcome::Unknown
    );
    assert_eq!(client.0.lock().unwrap().writes, 1);
    assert_eq!(client.prune_source().unwrap_err().code, "target_conflict");
    assert_eq!(protect_run_sources(&store).await.unwrap(), 0);
    assert_eq!(client.0.lock().unwrap().writes, 1);
    assert_eq!(original, client.original_hashes());
    client.change_run("run-0000", |record| {
        let reversed = Json::object([("revision", Json::Number(7.0)), ("objectId", "task".into())]);
        field_mut(record, "revision")
            .insert("references", Json::object([("source-task", reversed)]));
    });
    assert_eq!(protect_run_sources(&store).await.unwrap(), 0);
    assert_eq!(client.0.lock().unwrap().writes, 2);
    assert_eq!(client.prune_source().unwrap_err().code, "target_conflict");
    assert_eq!(protect_run_sources(&store).await.unwrap(), 0);
    assert_eq!(client.0.lock().unwrap().writes, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn pruned_sources_are_reported_but_read_errors_and_reference_races_fail_without_replay() {
    let contracts = ivy_task_board::runtime_inputs::contracts().unwrap();
    for failure in [
        "revision_pruned",
        "not_found",
        "connection_closed",
        "reference_race",
    ] {
        let client = Arc::new(ProtectionClient::default());
        initialize_sources(&client, &Json::Null, 1);
        let original = client.original_hashes();
        {
            let mut state = client.0.lock().unwrap();
            if failure == "reference_race" {
                state.prune_before_write = true;
            } else {
                state.read_failure = Some(("task".into(), 7, failure.into()));
            }
        }
        let db = Connection::open_in_memory().unwrap();
        let store = store(&db, client.clone(), &contracts, None);
        let result = protect_run_sources(&store).await;
        if matches!(failure, "revision_pruned" | "not_found") {
            assert_eq!(result.unwrap(), 1);
        } else {
            assert_eq!(
                result.unwrap_err().code,
                if failure == "reference_race" {
                    "revision_pruned"
                } else {
                    failure
                }
            );
        }
        assert_eq!(client.0.lock().unwrap().writes, 0);
        assert_eq!(client.original_hashes(), original);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn failed_refresh_keeps_original_ledger_pins_until_the_run_can_be_observed() {
    let contracts = ivy_task_board::runtime_inputs::contracts().unwrap();
    let client = Arc::new(ProtectionClient::default());
    initialize_sources(&client, &Json::Null, 2);
    let db = Connection::open_in_memory().unwrap();
    let store = store(&db, client.clone(), &contracts, None);
    assert_eq!(protect_run_sources(&store).await.unwrap(), 0);
    let ledgers = client.ledgers();
    client.change_run("run-0000", |record| {
        finish(record, "completed");
    });
    client.0.lock().unwrap().read_failure =
        Some(("run-0001".into(), 0, "connection_closed".into()));
    assert_eq!(
        protect_run_sources(&store).await.unwrap_err().code,
        "connection_closed"
    );
    assert_eq!(client.ledgers(), ledgers);
    assert_eq!(client.prune_source().unwrap_err().code, "target_conflict");
    client.0.lock().unwrap().read_failure = None;
    assert_eq!(protect_run_sources(&store).await.unwrap(), 0);
    assert_eq!(client.0.lock().unwrap().writes, 3);
    assert_eq!(client.prune_source().unwrap_err().code, "target_conflict");
}

#[tokio::test(flavor = "current_thread")]
async fn scope_identity_and_exact_ledger_references_remain_required() {
    let contracts = ivy_task_board::runtime_inputs::contracts().unwrap();
    for failure in [
        "task_scope",
        "run_identity",
        "ledger_references",
        "duplicate_ledger",
    ] {
        let client = Arc::new(ProtectionClient::default());
        initialize_sources(&client, &Json::Null, 1);
        let db = Connection::open_in_memory().unwrap();
        let store = store(&db, client.clone(), &contracts, None);
        if failure.starts_with("ledger") || failure == "duplicate_ledger" {
            protect_run_sources(&store).await.unwrap();
        }
        {
            let mut state = client.0.lock().unwrap();
            match failure {
                "task_scope" => field_mut(
                    state.history.get_mut(&("task".into(), 7)).unwrap(),
                    "object",
                )
                .insert("ownerObjectId", "another-task".into()),
                "run_identity" => field_mut(
                    field_mut(state.records.get_mut("run-0000").unwrap(), "content"),
                    "value",
                )
                .insert("taskId", "another-task".into()),
                "ledger_references" => {
                    field_mut(state.records.get_mut("ledger-0000").unwrap(), "revision").insert(
                        "references",
                        Json::object([("source", p("task", 8)), ("run", p("run-0000", 1))]),
                    )
                }
                "duplicate_ledger" => {
                    let mut record = state.records["ledger-0000"].clone();
                    field_mut(&mut record, "object").insert("id", "duplicate".into());
                    field_mut(&mut record, "revision").insert("objectId", "duplicate".into());
                    state.records.insert("duplicate".into(), record);
                }
                _ => unreachable!(),
            }
        }
        let before = client.original_hashes();
        let writes = client.0.lock().unwrap().writes;
        assert_eq!(
            protect_run_sources(&store).await.unwrap_err().code,
            match failure {
                "task_scope" => "task_board_scope_mismatch",
                "duplicate_ledger" => "task_board_identity_conflict",
                _ => "task_board_evidence_mismatch",
            }
        );
        assert_eq!(client.original_hashes(), before);
        assert_eq!(client.0.lock().unwrap().writes, writes);
    }
}
