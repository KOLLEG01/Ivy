use super::*;
const EPOCH: &str = "10000000-0000-4000-8000-000000000011";
const CALL: &str = "10000000-0000-4000-8000-000000000005";

fn scenario(name: &str) -> IvyJson {
    value(include_str!(
        "../fixtures/phone-journal-commands-cases.json"
    ))
    .as_array()
    .unwrap()
    .iter()
    .find(|v| v.get("name") == Some(&name.into()))
    .unwrap()
    .get("payload")
    .unwrap()
    .get("steps")
    .unwrap()
    .clone()
}
async fn steps(journal: &PhoneJournal, values: &[IvyJson]) {
    for step in values {
        let action = step.get("action").unwrap().as_str().unwrap();
        let args = resolve_command_arguments(step.get("arguments").unwrap(), &CALL.into());
        command_step(journal, &action, &args).await.unwrap();
    }
}
fn reply(result: IvyJson) -> IvyJson {
    IvyJson::object([
        ("version", 1.0.into()),
        ("epoch", EPOCH.into()),
        ("requestId", 1.0.into()),
        ("ok", true.into()),
        ("result", result),
        ("error", IvyJson::Null),
    ])
}
async fn seed_call(journal: &PhoneJournal, count: u32) -> Vec<IvyJson> {
    let original = scenario("released-call-compaction-keeps-identities");
    steps(journal, &original.as_array().unwrap()[..5]).await;
    let mut intents = Vec::new();
    for sequence in 1..=count {
        let intent = IvyJson::object([
            ("epoch", EPOCH.into()),
            ("operationId", uuid::Uuid::new_v4().to_string().into()),
            ("callId", CALL.into()),
            ("method", "call.command.feedback".into()),
            ("requestHash", digest("maintenance-feedback").into()),
            ("commandSequence", (sequence as f64).into()),
        ]);
        journal.submit(intent.clone()).await.unwrap();
        journal
            .finish(
                intent.clone(),
                reply(IvyJson::object([("fixture", true.into())])),
            )
            .await
            .unwrap();
        intents.push(intent);
    }
    intents
}
async fn release_call(journal: &PhoneJournal) {
    let original = scenario("released-call-compaction-keeps-identities");
    steps(journal, &original.as_array().unwrap()[5..8]).await;
}
async fn page(journal: &PhoneJournal) -> IvyResult<bool> {
    journal.request(|db| db.compact_page()).await
}
async fn drain(journal: &PhoneJournal) {
    for _ in 0..1000 {
        if !page(journal).await.unwrap() {
            return;
        }
    }
    panic!("Bounded journal maintenance did not settle");
}
async fn remaining(journal: &PhoneJournal) -> usize {
    journal
        .status()
        .await
        .unwrap()
        .get("operations")
        .unwrap()
        .as_f64()
        .unwrap() as usize
}

#[tokio::test]
async fn pages_preserve_first_original_sequence_and_counters_when_cleanup_rolls_back() {
    let root = Root::new();
    let journal = PhoneJournal::open_with_maintenance(root.options(), false)
        .await
        .unwrap();
    let intents = seed_call(&journal, 200).await;
    release_call(&journal).await;
    let before = command_snapshot(&journal).await;
    journal.fixture_db(|db| db.execute_batch("CREATE TEMP TRIGGER fail_page BEFORE DELETE ON commands BEGIN SELECT RAISE(ABORT,'page_failure'); END;").map_err(error)).await.unwrap();
    // A known original is read before any cleanup, including a cleanup failure.
    assert_eq!(
        journal.submit(intents[0].clone()).await.unwrap_err().code,
        "phone_operation_retained"
    );
    assert_eq!(
        page(&journal).await.unwrap_err().code,
        "storage_unavailable"
    );
    same(&command_snapshot(&journal).await, &before);
    journal
        .fixture_db(|db| db.execute_batch("DROP TRIGGER fail_page;").map_err(error))
        .await
        .unwrap();
    let count = remaining(&journal).await;
    assert!(page(&journal).await.unwrap());
    assert_eq!(
        count - remaining(&journal).await,
        maintenance::PAGE as usize
    );
    assert_eq!(
        journal.status().await.unwrap().get("calls"),
        Some(&1.0.into())
    );
    let first = journal
        .call_command(CALL.into(), "call.command.feedback".into(), 0)
        .await
        .unwrap()
        .unwrap();
    same(first.get("intent").unwrap(), &intents[0]);
    for sequence in [1, 200] {
        let found = journal
            .feedback_command(CALL.into(), sequence)
            .await
            .unwrap()
            .unwrap();
        same(
            found.get("intent").unwrap(),
            &intents[sequence as usize - 1],
        );
    }
    drain(&journal).await;
    let status = journal.status().await.unwrap();
    for key in ["operations", "calls", "reservedBytes"] {
        assert_eq!(status.get(key), Some(&0.0.into()));
    }
    journal.close().wait().await.unwrap();
    let reopened = PhoneJournal::open_with_maintenance(root.options(), false)
        .await
        .unwrap();
    for sequence in [1, 200] {
        let original = reopened
            .feedback_command(CALL.into(), sequence)
            .await
            .unwrap()
            .unwrap();
        same(
            original.get("intent").unwrap(),
            &intents[sequence as usize - 1],
        );
        let replay = reopened
            .finish(
                intents[sequence as usize - 1].clone(),
                reply(IvyJson::object([("fixture", true.into())])),
            )
            .await
            .unwrap();
        same(&replay, &original);
    }
    assert_eq!(
        reopened.submit(intents[0].clone()).await.unwrap_err().code,
        "phone_operation_retained"
    );
    reopened.close().wait().await.unwrap();
}

#[tokio::test]
async fn multiple_archive_confirmations_pin_call_and_unknown_originals_after_reopen() {
    let root = Root::new();
    let journal = PhoneJournal::open_with_maintenance(root.options(), false)
        .await
        .unwrap();
    let _ = seed_call(&journal, 40).await;
    let original = scenario("unresolved-archive-observation-keeps-original");
    let submitted = original
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s.get("action") == Some(&"submit".into()))
        .unwrap();
    let template = resolve_command_arguments(submitted.get("arguments").unwrap(), &CALL.into())
        .as_array()
        .unwrap()[0]
        .clone();
    let mut archives = Vec::new();
    for generation in 0..2 {
        let mut intent = template.clone();
        intent.insert("operationId", uuid::Uuid::new_v4().to_string().into());
        intent.insert("voiceGeneration", (generation as f64).into());
        journal.submit(intent.clone()).await.unwrap();
        let result = value(
            r#"{"state":"outcome_unknown","archivedIds":["10000000-0000-4000-8000-000000000001"],"failedThreadId":"10000000-0000-4000-8000-000000000002"}"#,
        );
        journal
            .finish_archive(intent.clone(), result)
            .await
            .unwrap();
        archives.push(intent);
    }
    release_call(&journal).await;
    let count = remaining(&journal).await;
    assert!(!page(&journal).await.unwrap());
    assert_eq!(remaining(&journal).await, count);
    let id = |n: usize| {
        archives[n]
            .get("operationId")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone()
    };
    journal.resolve_archive(id(0)).await.unwrap();
    assert!(journal.unresolved_archive().await.unwrap());
    assert!(!page(&journal).await.unwrap());
    assert_eq!(remaining(&journal).await, count);
    journal.resolve_archive(id(1)).await.unwrap();
    assert!(!journal.unresolved_archive().await.unwrap());
    drain(&journal).await;
    assert_eq!(remaining(&journal).await, 0);
    let history: i64 = journal
        .fixture_db(|db| {
            db.query_row("SELECT count(*) FROM receipt_archive_history", [], |r| {
                r.get(0)
            })
            .map_err(error)
        })
        .await
        .unwrap();
    assert_eq!(
        history, 0,
        "Uncertain original archive receipts still pin every result of that call"
    );
    let saved = journal.operation(id(0)).await.unwrap().unwrap();
    journal.close().wait().await.unwrap();
    let reopened = PhoneJournal::open_with_maintenance(root.options(), false)
        .await
        .unwrap();
    same(&saved, &reopened.operation(id(0)).await.unwrap().unwrap());
    assert_eq!(
        reopened
            .submit(archives[0].clone())
            .await
            .unwrap_err()
            .outcome,
        Outcome::Unknown
    );
    let complete = value(
        r#"{"state":"archived","archivedIds":["10000000-0000-4000-8000-000000000001","10000000-0000-4000-8000-000000000002"],"failedThreadId":null}"#,
    );
    assert_eq!(
        reopened
            .finish_archive(archives[0].clone(), complete)
            .await
            .unwrap_err()
            .code,
        "phone_receipt_conflict"
    );
    same(&saved, &reopened.operation(id(0)).await.unwrap().unwrap());
    reopened.close().wait().await.unwrap();
}

#[tokio::test]
async fn idle_worker_retires_cold_pages_and_drop_closes_without_losing_late_original() {
    let root = Root::new();
    let journal = PhoneJournal::open_with_maintenance(root.options(), false)
        .await
        .unwrap();
    let intents = seed_call(&journal, 160).await;
    release_call(&journal).await;
    let late = IvyJson::object([
        ("epoch", EPOCH.into()),
        ("operationId", uuid::Uuid::new_v4().to_string().into()),
        ("callId", IvyJson::Null),
        ("method", "configure".into()),
        ("requestHash", digest("late-original").into()),
    ]);
    journal.submit(late.clone()).await.unwrap();
    journal.lose_epoch(EPOCH.into()).await.unwrap();
    let late_id = late
        .get("operationId")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let lost = journal.operation(late_id.clone()).await.unwrap().unwrap();
    journal.close().wait().await.unwrap();
    let reopened = PhoneJournal::open(root.options()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(6), async {
        while remaining(&reopened).await != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    same(
        &reopened.operation(late_id.clone()).await.unwrap().unwrap(),
        &lost,
    );
    let first = reopened
        .feedback_command(CALL.into(), 1)
        .await
        .unwrap()
        .unwrap();
    same(first.get("intent").unwrap(), &intents[0]);
    assert_eq!(
        reopened
            .finish(
                late.clone(),
                reply(IvyJson::object([("fixture", true.into())]))
            )
            .await
            .unwrap_err()
            .code,
        "phone_receipt_conflict"
    );
    assert_eq!(
        reopened.submit(late).await.unwrap_err().outcome,
        Outcome::Unknown
    );
    let closed = PhoneJournalClose(reopened.0.closed.clone());
    drop(reopened);
    tokio::time::timeout(Duration::from_secs(2), closed.wait())
        .await
        .unwrap()
        .unwrap();
}
