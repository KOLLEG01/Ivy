//! Exercise the production document writer's saved archive condition and replay.
use super::*;
use crate::journal_store::persistent_reads::{document_reads::TreeReads, local_reads::SqliteReads};
use crate::local_journal::{LocalJournal, initialize};
use ivy_contracts::{hash::hash_json, json::parse};
use ivy_store::rusqlite::Connection;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

type Failure = WriteFailure<()>;
struct Hive {
    archived: Cell<bool>,
    revision: Cell<u64>,
    lost: Cell<bool>,
    calls: RefCell<Vec<Value>>,
    receipts: RefCell<HashMap<String, (String, Value)>>,
}
impl LocalMutationSource<()> for Hive {
    async fn request_owned(&self, method: &str, params: Option<Value>) -> Result<Value, ()> {
        assert_eq!(method, "objects.write");
        let params = params.unwrap();
        self.calls.borrow_mut().push(params.clone());
        let mutation = params.required("mutationId")?.as_str()?;
        let hash = hash_json(&params)?;
        if let Some((original_hash, receipt)) = self.receipts.borrow().get(&mutation) {
            assert_eq!(
                original_hash, &hash,
                "Replay retains every original wire parameter"
            );
            return Ok(receipt.clone());
        }
        if params.get("expectedArchived") != Some(&self.archived.get().into())
            || params.get("expectedRevision") != Some(&(self.revision.get() as f64).into())
        {
            return Err(IvyError::new(
                "revision_conflict",
                "Original archive or revision condition changed.",
            )
            .into());
        }
        self.revision.set(self.revision.get() + 1);
        let receipt = obj([
            (
                "object",
                obj([("id", text("task")), ("parentId", text("root"))]),
            ),
            (
                "revision",
                obj([("revision", Value::Number(self.revision.get() as f64))]),
            ),
        ]);
        self.receipts
            .borrow_mut()
            .insert(mutation, (hash, receipt.clone()));
        if self.lost.replace(false) {
            return Err(IvyError::new("transport_lost", "Committed reply was lost.")
                .with_outcome(ivy_contracts::Outcome::Unknown)
                .into());
        }
        Ok(receipt)
    }
}
struct NoReads;
impl ObjectReads<Failure> for NoReads {
    async fn read(&self, _: Option<Json>) -> std::result::Result<Json, Failure> {
        panic!("The saved write does not rediscover its archive condition")
    }
}
impl TreeReads<Failure> for NoReads {
    async fn hydrate(&self, _: &Json, _: &Json) -> std::result::Result<Option<Json>, Failure> {
        unreachable!()
    }
    fn summary(&self, _: &Json, _: &Json, _: &Json) -> std::result::Result<Option<Json>, Failure> {
        unreachable!()
    }
    async fn range(
        &self,
        _: &Json,
        _: &Json,
        _: usize,
        _: usize,
    ) -> std::result::Result<Vec<Json>, Failure> {
        unreachable!()
    }
}
async fn publish(
    source: &Hive,
    db: &Connection,
    destination: &Value,
    mutation: &str,
) -> Result<Value, ()> {
    let contracts = crate::runtime_inputs::contracts()?;
    let local = SqliteReads {
        journal: LocalJournal {
            connection: db,
            root_object_id: Some("root".into()),
        },
        contracts: &contracts,
        parse: |raw: &str| parse(raw, MANAGEMENT_FRAME_BYTES).map_err(Failure::from),
        sqlite_failure: |e| Failure::from(ivy_store::sqlite::error(e)),
    };
    let reads = DocumentReads {
        local: &local,
        objects: &NoReads,
        comments: &NoReads,
        cache: None,
    };
    let hydrate = |_: Value, _: Value| std::future::ready(Err(Failure::from(IvyError::internal())));
    // Conversation publication is orthogonal to the saved Task destination.
    let comments =
        |value: Value, _: Value, _: Value, _: Value| std::future::ready(Ok::<_, Failure>(value));
    let identity = |_: Value| std::future::ready(Err(Failure::from(IvyError::internal())));
    let now = || "2026-01-02T03:04:05.006Z".to_owned();
    let id = || JsString::from("unused");
    let writer = DocumentWrites {
        source,
        hydrate: &hydrate,
        comments: &comments,
        identity: &identity,
        now: &now,
        new_id: &id,
    };
    let task = parse(
        r#"{"comments":[],"commentDeliveries":[],"attachments":[],"lastRun":null,"latestResult":null}"#,
        1024,
    )?;
    writer
        .write_graph(
            &reads,
            "task-board/task",
            &task,
            &text(mutation),
            destination,
            true,
            &text("3.4.0"),
        )
        .await
}
#[tokio::test(flavor = "current_thread")]
async fn production_writer_replays_original_archive_condition_and_conflicts_after_membership_changes()
 {
    for expected in [None, Some(false), Some(true)] {
        let archived = expected.unwrap_or(false);
        let hive = Hive {
            archived: Cell::new(archived),
            revision: Cell::new(1),
            lost: Cell::new(true),
            calls: RefCell::default(),
            receipts: RefCell::default(),
        };
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let mut destination = obj([
            ("objectId", text("task")),
            ("expectedRevision", Value::Number(1.0)),
        ]);
        if let Some(expected) = expected {
            destination.insert("expectedArchived", expected.into());
        }
        let failure = publish(&hive, &db, &destination, "original")
            .await
            .unwrap_err();
        assert_eq!(typed_write_cause(&failure).unwrap().code, "transport_lost");
        assert_eq!(hive.revision.get(), 2);
        // Reconstruct all writer/read owners, retaining only the journal and Hive ledger.
        let result = publish(&hive, &db, &destination, "original").await.unwrap();
        assert_eq!(result.get("revision"), Some(&2.0.into()));
        assert_eq!(hive.revision.get(), 2);
        for request in hive.calls.borrow().iter() {
            assert_eq!(request.get("expectedArchived"), Some(&archived.into()));
        }
        hive.archived.set(!archived);
        let mut next = destination.clone();
        next.insert("expectedRevision", 2.0.into());
        let failure = publish(&hive, &db, &next, "final").await.unwrap_err();
        assert_eq!(
            typed_write_cause(&failure).unwrap().code,
            "revision_conflict"
        );
        assert_eq!(hive.revision.get(), 2);
        // A prior receipt remains exact even after a later independent archive change.
        assert_eq!(
            publish(&hive, &db, &destination, "original").await.unwrap(),
            result
        );
        assert_eq!(hive.receipts.borrow().len(), 1);
    }
}
