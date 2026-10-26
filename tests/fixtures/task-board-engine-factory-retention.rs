use super::*;
use driver::{count, frames};
use ivy_task_board::{generation::GenerationClock, service::ServiceControl};
use std::rc::Rc;

#[path = "task-board-engine-factory-retention-snapshot.rs"]
mod snapshot;

#[path = "task-board-engine-factory-retention-success.rs"]
mod success;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Case {
    RowPressure,
    RecoveryQueryFailure,
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
    pub(super) fn advance(&self, millis: f64) {
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
    (
        "retired_operation_receipts",
        "caller_id,operation_id,receipt_json",
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
fn reference() -> &'static Json {
    use std::sync::OnceLock;
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        let bytes = crate::reference_data::read(include_bytes!(
            "task-board-factory-retention-reference.json.gz"
        ));
        json(std::str::from_utf8(&bytes).unwrap())
    })
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
    rpc(
        connection,
        "test.retentionCleanup",
        Json::Object(
            error
                .iter()
                .map(|error| ("error".into(), error.clone()))
                .collect(),
        ),
    )
    .await
    .unwrap();
    let before = tables(fixture);
    let retirement = [
        "completed-control-evicted",
        "recovered-cleanup-query",
        "successful-byte-control-evicted",
    ]
    .contains(&stage);
    let closed = fixture
        .rows("task-board/operation")
        .into_iter()
        .find(|row| f(&row.2, "operationId") == &Json::from("retention-control"));
    let expected = if let Some((id, _, operation)) = closed.filter(|_| retirement) {
        snapshot::after_control(&before, &id, &operation)
    } else {
        before.clone()
    };
    let io = frames(peer, "retention-io").len();
    let pages = fixture.pages.borrow().len();
    fixture.clock.advance(60000.0);
    until(|| {
        fixture.pages.borrow().len() > pages
            && frames(peer, "retention-io").len() > io
            && count(fixture, "SELECT count(*) FROM retired_operations")
                == f(&expected, "retired_operations").as_array().unwrap().len() as i64
    })
    .await;
    let after = tables(fixture);
    assert_eq!(
        hash_json(&after).unwrap(),
        hash_json(&expected).unwrap(),
        "Complete collection snapshot {stage}"
    );
    let actual = frames(peer, "retention-io")
        .into_iter()
        .skip(io)
        .map(|read| Json::Array(vec![f(&read, "method").clone(), f(&read, "params").clone()]))
        .collect();
    assert_eq!(
        hash_json(&Json::Array(actual)).unwrap(),
        hash_json(&Json::Array(vec![Json::Array(vec![
            "objects.query".into(),
            f(reference(), "recoveryQuery").clone(),
        ])]))
        .unwrap(),
        "Original recovery source request {stage}"
    );
    assert_eq!(
        f(&operation(fixture, "factory-question").2, "phase"),
        &Json::from("succeeded")
    );
    assert_eq!(count(fixture, "SELECT count(*) FROM active_gates"), 0);
    if let Some(control) = control {
        let ready = bounded(control.readiness(connection)).await.unwrap();
        assert_eq!(f(&ready, "ready"), &true.into());
        let diagnostics = if let Some(error) = &error {
            assert_eq!(error, f(reference(), "cleanupFailure"));
            assert_eq!(before, after);
            "cleanupDiagnostics"
        } else {
            "cleanDiagnostics"
        };
        assert_eq!(
            hash_json(f(&ready, "diagnostics")).unwrap(),
            hash_json(f(reference(), diagnostics)).unwrap()
        );
    }
}
pub(super) async fn pending(
    fixture: &Fixture,
    peer: &Peer,
    control: &ServiceControl,
    connection: &ServiceConnection,
) {
    checkpoint(fixture, peer, connection, "pending").await;
    if fixture.retention == Some(Case::SuccessfulBytePressure) {
        success::pending(fixture, peer, control, connection).await;
        return;
    }
    let request = Json::object([
        ("action", "transition".into()),
        ("operationId", "retention-control".into()),
        ("taskId", "contact-task".into()),
        ("expectedRevision", Json::Number(1.0)),
        ("workflowState", "done".into()),
        ("detail", Json::Null),
    ]);
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
    if fixture.retention == Some(Case::RecoveryQueryFailure) {
        let unavailable = Json::object([
            ("code", "service_unavailable".into()),
            ("message", "Finite retention query unavailable.".into()),
            ("outcome", "not_executed".into()),
        ]);
        collect(
            fixture,
            peer,
            connection,
            "unavailable-recovery-query",
            Some(unavailable),
            Some(control),
        )
        .await;
        assert_eq!(fixture.rows("task-board/operation").len(), 2);
        assert_eq!(count(fixture, "SELECT count(*) FROM retired_operations"), 0);
        collect(
            fixture,
            peer,
            connection,
            "recovered-cleanup-query",
            None,
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
    let expected_error = f(reference(), "expiredError");
    assert_eq!(f(expected_error, "code"), &Json::from(expired.code.clone()));
    assert_eq!(
        f(expected_error, "message"),
        &Json::from(expired.message.clone())
    );
    assert_eq!(f(expected_error, "outcome"), &expired.outcome.to_json());
    assert_eq!(count(fixture, "SELECT count(*) FROM retired_operations"), 1);
    assert_eq!(
        count(
            fixture,
            "SELECT count(*) FROM revisions WHERE id IN (SELECT root_id FROM cleanup_roots WHERE root_id NOT IN (SELECT id FROM documents))"
        ),
        0
    );
}
pub(super) fn closed(fixture: &Fixture) {
    assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
    assert_eq!(fixture.rows("task-board/operation").len(), 1);
    assert_eq!(fixture.opens.get(), 3);
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
async fn unavailable_recovery_query_preserves_cleanup_error_and_graph_until_source_recovers() {
    delivery::run_delivery_mode(
        "factory question lost notify recovers confirmed",
        Some(Case::RecoveryQueryFailure),
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
