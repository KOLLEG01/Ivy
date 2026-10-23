use super::*;
use driver::{count, frames};
use ivy_task_board::{generation::GenerationClock, service::ServiceControl};
use std::{io::Write, rc::Rc};

#[path = "task-board-engine-factory-retention-success.rs"]
mod success;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Case {
    RowPressure,
    RemoteReadFailure,
    SuccessfulBytePressure,
}
pub(super) fn limits(case: Case) -> HistoryLimits {
    match case {
        Case::SuccessfulBytePressure => HistoryLimits {
            count: 10000,
            bytes: 0,
        },
        _ => HistoryLimits {
            count: 0,
            ..HistoryLimits::default()
        },
    }
}
#[derive(Clone, Default)]
pub(super) struct FactoryClock(Option<Rc<ManualClock>>);
#[derive(Default)]
struct ManualClock {
    now: Cell<f64>,
    sleeps: RefCell<Vec<(f64, Rc<Gate>)>>,
}
impl FactoryClock {
    pub(super) fn manual() -> Self {
        Self(Some(Rc::new(ManualClock::default())))
    }
    fn advance(&self, millis: f64) {
        let manual = self.0.as_ref().unwrap();
        manual.now.set(manual.now.get() + millis);
        manual.sleeps.borrow_mut().retain(|(at, gate)| {
            if *at <= manual.now.get() {
                gate.release();
                false
            } else {
                true
            }
        });
    }
}
impl GenerationClock for FactoryClock {
    fn now_millis(&self) -> f64 {
        self.0
            .as_ref()
            .map_or_else(|| SystemClock.now_millis(), |s| s.now.get())
    }
    async fn sleep(&self, millis: f64) {
        if let Some(manual) = &self.0 {
            let gate = Rc::new(Gate::default());
            manual
                .sleeps
                .borrow_mut()
                .push((manual.now.get() + millis, gate.clone()));
            gate.wait().await;
        } else {
            GenerationClock::sleep(&SystemClock, millis).await;
        }
    }
}
pub(super) async fn drive(fixture: &Fixture, mut condition: impl FnMut() -> bool) {
    bounded(async {
        while !condition() {
            fixture.clock.advance(1000.0);
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
}
const TABLES: &[(&str, &str)] = &[
    (
        "documents",
        "id,key,name,parent_id,current_revision,created_at,updated_at",
    ),
    ("revisions", "id,revision,value_json,created_at"),
    ("blobs", "id,key,name,parent_id,hex(bytes)"),
    ("markers", "key,value"),
    ("cleanup_roots", "root_id,delete_after"),
    ("cleanup_dependencies", "root_id,operation_id"),
    ("active_gates", "gate_id,operation_id"),
    ("operation_gates", "operation_id,gate_name"),
    (
        "workspace_reservations",
        "host_id,canonical_cwd,task_id,operation_id",
    ),
    ("storage_usage", "kind,rows,bytes"),
    (
        "retired_operations",
        "caller_id,operation_id,request_hash,name",
    ),
];
fn tables(fixture: &Fixture) -> Json {
    use ivy_store::rusqlite::types::ValueRef;
    let db = fixture.database();
    Json::Object(
        TABLES
            .iter()
            .map(|(table, columns)| {
                let mut statement = db
                    .prepare(&format!("SELECT {columns} FROM {table} ORDER BY 1,2"))
                    .unwrap();
                let columns = statement.column_count();
                let rows = statement
                    .query_map([], |row| {
                        Ok(Json::Array(
                            (0..columns)
                                .map(|index| match row.get_ref(index).unwrap() {
                                    ValueRef::Null => Json::Null,
                                    ValueRef::Integer(v) => Json::Number(v as f64),
                                    ValueRef::Real(v) => Json::Number(v),
                                    ValueRef::Text(v) => std::str::from_utf8(v).unwrap().into(),
                                    _ => panic!("Unbound retention snapshot value"),
                                })
                                .collect(),
                        ))
                    })
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                ((*table).into(), Json::Array(rows))
            })
            .collect(),
    )
}
fn original(input: &Json) -> Json {
    let mut process = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-retention-oracle.mjs",
            if input.get("configurationControl").is_some() {
                "--configuration-control"
            } else {
                "--factory-snapshot"
            },
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process
        .stdin
        .take()
        .unwrap()
        .write_all(
            ivy_contracts::json::stringify(input, 32 * 1024 * 1024)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let output = process.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json(std::str::from_utf8(&output.stdout).unwrap())
}
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.required(key).unwrap()
}
fn operation(fixture: &Fixture, id: &str) -> (String, u64, Json) {
    fixture
        .rows("task-board/operation")
        .into_iter()
        .find(|row| f(&row.2, "operationId") == &Json::from(id))
        .unwrap()
}
fn raw_delivery(fixture: &Fixture) -> Json {
    let rows = fixture.rows("task-board/delivery");
    assert_eq!(rows.len(), 1);
    let (id, revision, value) = &rows[0];
    Json::object([
        (
            "object",
            Json::object([
                ("objectId", id.clone().into()),
                ("contractKey", "task-board/delivery".into()),
                ("currentRevision", Json::Number(*revision as f64)),
            ]),
        ),
        (
            "content",
            Json::object([("encoding", "json".into()), ("value", value.clone())]),
        ),
    ])
}
fn proof(stage: &str, value: Json) {
    println!(
        "factory-retention {}",
        ivy_contracts::json::stringify(
            &Json::object([("stage", stage.into()), ("proof", value)]),
            32 * 1024 * 1024
        )
        .unwrap()
    );
}
pub(super) async fn checkpoint(
    fixture: &Fixture,
    peer: &Peer,
    connection: &ServiceConnection,
    stage: &str,
) {
    collect(fixture, peer, connection, stage, None, None).await;
}
async fn collect(
    fixture: &Fixture,
    peer: &Peer,
    connection: &ServiceConnection,
    stage: &str,
    error: Option<Json>,
    control: Option<&ServiceControl>,
) {
    let raw = raw_delivery(fixture);
    let mut remote = Json::object([("raw", raw.clone())]);
    if let Some(error) = &error {
        remote.insert("error", error.clone());
    }
    rpc(connection, "test.retentionDelivery", remote)
        .await
        .unwrap();
    let before = tables(fixture);
    let limit = limits(fixture.retention.unwrap());
    let mut input = Json::object([
        ("tables", before.clone()),
        (
            "limits",
            Json::object([
                ("count", Json::Number(limit.count as f64)),
                ("bytes", Json::Number(limit.bytes as f64)),
            ]),
        ),
        ("nowMs", Json::Number(fixture.clock.now_millis() + 60000.0)),
        (
            "remote",
            Json::Object(vec![(
                f(f(&raw, "object"), "objectId")
                    .as_js_string()
                    .unwrap()
                    .clone(),
                error
                    .as_ref()
                    .map_or_else(|| raw.clone(), |e| Json::object([("error", e.clone())])),
            )]),
        ),
        (
            "lookups",
            Json::Array(vec![Json::Array(vec![
                Json::String(context(1).caller_principal_id),
                "retention-control".into(),
            ])]),
        ),
    ]);
    let db = fixture.database();
    let mut roots = db
        .prepare("SELECT root_id FROM cleanup_roots ORDER BY rowid")
        .unwrap();
    let order = roots
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| Json::from(r.unwrap()))
        .collect();
    input.insert("cleanupOrder", Json::Array(order));
    if control.is_some() {
        input.insert(
            "diagnostic",
            Json::object([
                ("source", "fixture-task-board".into()),
                ("now", clock().into()),
            ]),
        );
    }
    let expected = original(&input);
    let reads = frames(peer, "delivery-retention-read").len();
    let pages = fixture.pages.borrow().len();
    fixture.clock.advance(60000.0);
    until(|| {
        fixture.pages.borrow().len() > pages
            && frames(peer, "delivery-retention-read").len() > reads
            && (stage != "successful-byte-control-evicted"
                || count(fixture, "SELECT count(*) FROM retired_operations") == 1)
    })
    .await;
    let after = tables(fixture);
    assert_eq!(
        hash_json(&after).unwrap(),
        hash_json(f(&expected, "tables")).unwrap(),
        "Original collection {stage}"
    );
    let observed = frames(peer, "delivery-retention-read")
        .into_iter()
        .skip(reads)
        .collect::<Vec<_>>();
    for read in &observed {
        if let Some(error) = &error {
            assert_eq!(f(read, "error"), error);
            assert!(
                read.get("raw").is_none(),
                "An unavailable read is not returned evidence"
            );
        } else {
            assert_eq!(f(read, "raw"), &raw);
        }
    }
    let actual = observed
        .into_iter()
        .map(|read| Json::Array(vec!["objects.read".into(), f(&read, "address").clone()]))
        .collect::<Vec<_>>();
    assert_eq!(
        hash_json(&Json::Array(actual)).unwrap(),
        hash_json(f(&expected, "trace")).unwrap(),
        "Original remote veto reads {stage}"
    );
    assert_eq!(
        f(&operation(fixture, "factory-question").2, "phase"),
        &"succeeded".into()
    );
    assert_eq!(count(fixture, "SELECT count(*) FROM active_gates"), 0);
    let readiness = if let Some(control) = control {
        let ready = bounded(control.readiness(connection)).await.unwrap();
        assert_eq!(f(&ready, "ready"), &true.into());
        assert_eq!(
            hash_json(f(&ready, "diagnostics")).unwrap(),
            hash_json(f(&expected, "diagnostics")).unwrap()
        );
        if let Some(error) = error
            .as_ref()
            .filter(|e| f(e, "code") != &"not_found".into())
        {
            assert_eq!(f(&expected, "failure"), error);
            assert_eq!(hash_json(&before).unwrap(), hash_json(&after).unwrap());
        } else {
            assert!(expected.get("failure").is_none());
        }
        ready
    } else {
        Json::Null
    };
    proof(
        stage,
        Json::object([
            ("input", input),
            ("original", expected),
            ("actual", after),
            ("readiness", readiness),
            ("opens", Json::Number(fixture.opens.get() as f64)),
        ]),
    );
}
pub(super) async fn pending(
    fixture: &Fixture,
    peer: &Peer,
    control: &ServiceControl,
    connection: &ServiceConnection,
    question: &Json,
) {
    checkpoint(fixture, peer, connection, "pending").await;
    if fixture.retention == Some(Case::SuccessfulBytePressure) {
        success::pending(fixture, peer, control, connection).await;
        return;
    }
    let mut request = question.clone();
    request.insert("operationId", "retention-control".into());
    // The original revision is stale. Admission succeeds, retained execution
    // fails before a domain write, and its closed unreferenced graph is eligible.
    let failure = bounded(control.handler("update")(
        request.clone(),
        c(1, "retention-control"),
    ))
    .await
    .unwrap_err();
    assert_eq!(failure.code, "revision_conflict");
    assert_eq!(failure.outcome, Outcome::NotExecuted);
    let failed = operation(fixture, "retention-control");
    assert_eq!(f(&failed.2, "phase"), &"failed".into());
    assert_eq!(f(&failed.2, "writes"), &Json::Array(vec![]));
    assert_eq!(
        f(&failed.2, "requestHash"),
        &hash_json(&request).unwrap().into()
    );
    assert_eq!(
        hash_json(f(&failed.2, "request")).unwrap(),
        hash_json(&request).unwrap()
    );
    assert_eq!(f(f(&failed.2, "actor"), "generation"), &Json::Number(1.0));
    let before = tables(fixture);
    let replay = bounded(control.handler("update")(
        request.clone(),
        c(1, "retention-control"),
    ))
    .await
    .unwrap_err();
    assert_eq!(replay.code, failure.code);
    assert_eq!(replay.message, failure.message);
    assert_eq!(replay.outcome, failure.outcome);
    assert_eq!(
        hash_json(&tables(fixture)).unwrap(),
        hash_json(&before).unwrap()
    );
    // The source collector ignores age but honors pressure. The same saved graph
    // at a distant future clock remains unchanged with a sufficient row budget.
    let raw = raw_delivery(fixture);
    let no_pressure = original(&Json::object([
        ("tables", before.clone()),
        (
            "limits",
            Json::object([
                ("count", Json::Number(10000.0)),
                ("bytes", Json::Number(268435456.0)),
            ]),
        ),
        ("nowMs", Json::Number(4102444800000.0)),
        ("remote", Json::Object(vec![])),
    ]));
    assert_eq!(f(&no_pressure, "tables"), &before);
    assert_eq!(f(&no_pressure, "trace"), &Json::Array(vec![]));
    proof(
        "cached-control-before-eviction",
        Json::object([
            ("operation", failed.2.clone()),
            ("noPressureOriginal", no_pressure),
            ("rawDelivery", raw),
        ]),
    );
    if fixture.retention == Some(Case::RemoteReadFailure) {
        let unavailable = Json::object([
            ("code", "service_unavailable".into()),
            ("message", "Finite retention read unavailable.".into()),
            ("outcome", "not_executed".into()),
        ]);
        collect(
            fixture,
            peer,
            connection,
            "unavailable-delivery-read",
            Some(unavailable),
            Some(control),
        )
        .await;
        assert_eq!(fixture.rows("task-board/operation").len(), 2);
        assert_eq!(count(fixture, "SELECT count(*) FROM retired_operations"), 0);
        let absent = Json::object([
            ("code", "not_found".into()),
            ("message", "Finite retention delivery absent.".into()),
            ("outcome", "not_executed".into()),
        ]);
        collect(
            fixture,
            peer,
            connection,
            "missing-delivery-recovery",
            Some(absent),
            Some(control),
        )
        .await;
        assert_eq!(fixture.rows("task-board/operation").len(), 1);
        assert_eq!(count(fixture, "SELECT count(*) FROM retired_operations"), 1);
    }
    checkpoint(fixture, peer, connection, "completed-control-evicted").await;
    assert_eq!(fixture.rows("task-board/operation").len(), 1);
    let before_expired = tables(fixture);
    let expired = bounded(control.handler("update")(
        request,
        c(1, "retention-control"),
    ))
    .await
    .unwrap_err();
    assert_eq!(expired.code, "operation_expired");
    assert_eq!(
        expired.message,
        "The completed TaskBoard operation was evicted; its identity cannot be reused."
    );
    assert_eq!(expired.outcome, Outcome::NotExecuted);
    assert_eq!(
        hash_json(&tables(fixture)).unwrap(),
        hash_json(&before_expired).unwrap()
    );
    let original_expired = original(&Json::object([
        ("tables", before_expired),
        (
            "limits",
            Json::object([
                ("count", Json::Number(10000.0)),
                ("bytes", Json::Number(268435456.0)),
            ]),
        ),
        ("nowMs", Json::Number(fixture.clock.now_millis())),
        ("remote", Json::Object(vec![])),
        (
            "lookups",
            Json::Array(vec![Json::Array(vec![
                Json::String(context(1).caller_principal_id),
                "retention-control".into(),
            ])]),
        ),
    ]));
    let expected_error = f(
        &f(&original_expired, "lookups").as_array().unwrap()[0],
        "error",
    );
    assert_eq!(f(expected_error, "code"), &Json::from(expired.code.clone()));
    assert_eq!(
        f(expected_error, "message"),
        &Json::from(expired.message.clone())
    );
    assert_eq!(
        f(expected_error, "outcome"),
        &expired.outcome.to_json()
    );
    assert_eq!(count(fixture, "SELECT count(*) FROM retired_operations"), 1);
    assert_eq!(
        count(
            fixture,
            "SELECT count(*) FROM revisions WHERE id IN (SELECT root_id FROM cleanup_roots WHERE root_id NOT IN (SELECT id FROM documents))"
        ),
        0
    );
    proof(
        "expired-control-replay",
        Json::object([
            ("code", expired.code.into()),
            ("message", expired.message.into()),
            ("operation", failed.2),
            ("originalExpired", original_expired),
            ("tables", tables(fixture)),
        ]),
    );
}
pub(super) fn closed(fixture: &Fixture) {
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
    assert_eq!(fixture.rows("task-board/operation").len(), 1);
    proof(
        "cached-question-and-closed",
        Json::object([
            ("tables", tables(fixture)),
            ("publication", operation(fixture, "factory-question").2),
            ("opens", Json::Number(fixture.opens.get() as f64)),
            (
                "drops",
                Json::Array(
                    fixture
                        .drops
                        .borrow()
                        .iter()
                        .map(|g| Json::Number(*g as f64))
                        .collect(),
                ),
            ),
        ]),
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pending_unknown_and_confirmed_delivery_pins_protect_publication_while_unreferenced_terminal_control_expires()
 {
    delivery::run_delivery_mode(
        "factory question lost notify recovers confirmed",
        Some(Case::RowPressure),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unavailable_delivery_read_preserves_cleanup_error_and_graph_until_absent_receipt_recovery()
{
    delivery::run_delivery_mode(
        "factory question lost notify recovers confirmed",
        Some(Case::RemoteReadFailure),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn successful_unreferenced_configuration_expires_under_byte_pressure_preserving_question_graph()
 {
    delivery::run_delivery_mode(
        "factory question lost notify recovers confirmed",
        Some(Case::SuccessfulBytePressure),
    )
    .await;
}
