use futures_util::FutureExt;
use ivy_agent_manager::service_work::{AgentRun, AgentWork};
use ivy_contracts::{IvyError, IvyJson, JsString, Outcome};
use ivy_sdk::service::{InvocationContext, ToolHandler};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;

fn context() -> InvocationContext {
    InvocationContext::detached(
        "caller".into(),
        None,
        1,
        Default::default(),
        IvyError::internal(),
    )
}
fn payload(mebibytes: usize) -> IvyJson {
    let mut units = Vec::with_capacity(mebibytes * 1024 * 1024 / 2);
    units.extend([65, 0xd800, 0xdc01, 0xdfff]);
    IvyJson::String(JsString(units))
}
fn pointer(value: &IvyJson) -> (usize, usize) {
    let units = &value.as_js_string().unwrap().0;
    (units.as_ptr() as usize, units.capacity())
}
fn refused(result: Result<AgentRun, IvyError>) {
    match result {
        Err(error) => {
            assert_eq!(error.code, "native_capacity");
            assert_eq!(error.outcome, Outcome::NotExecuted);
        }
        Ok(_) => panic!("Unread results must retain their admission"),
    }
}
fn unavailable(error: IvyError) {
    assert_eq!(error.code, "agent_reply_unavailable");
    assert_eq!(error.outcome, Outcome::Unknown);
}

#[tokio::test]
async fn last_receiver_moves_the_original_success_and_error_allocations() {
    let owner = AgentWork::default();
    let value = payload(2);
    let expected = pointer(&value);
    let run = owner.track(Box::pin(async move { Ok(value) })).unwrap();
    let second = run.clone();
    assert_eq!(
        run.wait()
            .await
            .unwrap()
            .as_js_string()
            .unwrap()
            .0
            .as_slice(),
        &[65, 0xd800, 0xdc01, 0xdfff]
    );
    assert_eq!(
        pointer(&second.wait().await.unwrap()),
        expected,
        "The final observer must move the retained allocation"
    );
    let mut error = IvyError::new_js(JsString(vec![0xdfff]), JsString(vec![0xd800]));
    error.details = Some(payload(2));
    error = error.with_outcome(Outcome::Unknown);
    let expected = pointer(error.details.as_ref().unwrap());
    let original = owner
        .track(Box::pin(async move { Err(error) }))
        .unwrap()
        .wait()
        .await
        .unwrap_err();
    assert_eq!(pointer(original.details.as_ref().unwrap()), expected);
    assert_eq!(original.code_js().0, vec![0xdfff]);
    assert_eq!(original.message_js().0, vec![0xd800]);
    assert_eq!(original.outcome, Outcome::Unknown);
}

#[tokio::test]
async fn unread_completed_results_keep_slots_without_blocking_owner_close_or_answers() {
    let owner = AgentWork::default();
    let mut results = Vec::new();
    for _ in 0..56 {
        results.push(owner.track(Box::pin(async { Ok(IvyJson::Null) })).unwrap());
    }
    owner.drain_snapshot().await.unwrap();
    assert_eq!(owner.pending().unwrap(), 0);
    refused(owner.track(Box::pin(async {
        panic!("Unadmitted work must not execute")
    })));
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let handler: ToolHandler = Arc::new(move |_, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(IvyJson::Null) })
    });
    let replies = owner.reply_handler(handler);
    let mut answers = Vec::new();
    for _ in 0..8 {
        answers.push(replies(IvyJson::Null, context()));
    }
    let rejected = replies(IvyJson::Null, context()).await.unwrap_err();
    assert_eq!(rejected.code, "native_capacity");
    assert_eq!(calls.load(Ordering::SeqCst), 8);
    for answer in answers {
        answer.await.unwrap();
    }
    drop(results.pop());
    owner
        .track(Box::pin(async { Ok(IvyJson::Null) }))
        .unwrap()
        .wait()
        .await
        .unwrap();
    // Retirement drains accepted effects, not retained caller result handles.
    owner.close().await.unwrap();
    assert_eq!(owner.pending().unwrap(), 0);
    drop(results);
}

#[tokio::test]
async fn retained_result_heap_reserves_answers_and_refusal_keeps_the_executed_outcome_unknown() {
    let owner = AgentWork::default();
    let first = owner.track(Box::pin(async { Ok(payload(30)) })).unwrap();
    let second = owner.track(Box::pin(async { Ok(payload(20)) })).unwrap();
    owner.drain_snapshot().await.unwrap();
    let effects = Arc::new(AtomicUsize::new(0));
    let counted = effects.clone();
    let refused = owner
        .track(Box::pin(async move {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(payload(7))
        }))
        .unwrap();
    unavailable(refused.wait().await.unwrap_err());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let control = owner.reply_handler(Arc::new(|_, _| Box::pin(async { Ok(payload(7)) })))(
        IvyJson::Null,
        context(),
    );
    owner.drain_snapshot().await.unwrap();
    let refused = owner.reply_handler(Arc::new(|_, _| Box::pin(async { Ok(payload(8)) })))(
        IvyJson::Null,
        context(),
    );
    unavailable(refused.await.unwrap_err());
    let error = owner
        .track(Box::pin(async {
            let mut error =
                IvyError::new("original_failure", "original").with_outcome(Outcome::Unknown);
            error.details = Some(payload(20));
            Err(error)
        }))
        .unwrap();
    unavailable(error.wait().await.unwrap_err());
    let small = owner
        .track(Box::pin(async {
            Err(IvyError::new("original_small_error", "original"))
        }))
        .unwrap()
        .wait()
        .await
        .unwrap_err();
    assert_eq!(small.code, "original_small_error");
    drop(first);
    let recovered = owner.track(Box::pin(async { Ok(payload(20)) })).unwrap();
    assert_eq!(
        recovered
            .wait()
            .await
            .unwrap()
            .as_js_string()
            .unwrap()
            .0
            .len(),
        4
    );
    control.await.unwrap();
    drop(second);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn handler_input_heap_is_reserved_before_the_factory_and_survives_abandoned_waiters() {
    let owner = AgentWork::default();
    let gate = Arc::new(Semaphore::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let held = gate.clone();
    let counted = calls.clone();
    let factory: ToolHandler = Arc::new(move |args, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        let gate = held.clone();
        Box::pin(async move {
            gate.acquire().await.unwrap().forget();
            drop(args);
            Ok(IvyJson::Null)
        })
    });
    let handler = owner.handler(factory.clone());
    let first = handler(payload(30), context());
    let second = handler(payload(20), context());
    drop(first);
    let denied = handler(payload(7), context());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "Capacity must be checked before invoking the factory"
    );
    assert_eq!(
        denied.now_or_never().unwrap().unwrap_err().code,
        "native_capacity"
    );
    let oversized = handler(payload(49), context());
    assert_eq!(
        oversized.now_or_never().unwrap().unwrap_err().code,
        "native_capacity"
    );
    let closed = owner.close();
    let answer = owner.reply_handler(factory)(payload(7), context());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    gate.add_permits(3);
    second.await.unwrap();
    answer.await.unwrap();
    closed.await.unwrap();
    assert_eq!(owner.pending().unwrap(), 0);
}

#[tokio::test]
async fn a_lost_accepted_producer_stays_unknown_and_keeps_its_unread_slot() {
    let owner = AgentWork::default();
    let effects = Arc::new(AtomicUsize::new(0));
    let counted = effects.clone();
    let original = owner
        .track(Box::pin(async move {
            counted.fetch_add(1, Ordering::SeqCst);
            panic!("Fixture producer lost after its accepted effect");
        }))
        .unwrap();
    let observer = original.clone();
    let error = observer.wait().await.unwrap_err();
    assert_eq!(error.code, "internal_error");
    assert_eq!(error.outcome, Outcome::Unknown);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    owner.drain_snapshot().await.unwrap();
    assert_eq!(owner.pending().unwrap(), 0);
    let mut held = Vec::new();
    for _ in 0..55 {
        held.push(owner.track(Box::pin(async { Ok(IvyJson::Null) })).unwrap());
    }
    owner.drain_snapshot().await.unwrap();
    refused(owner.track(Box::pin(async {
        panic!("Unread unknown work must keep its slot")
    })));
    assert_eq!(original.wait().await.unwrap_err(), error);
    owner
        .track(Box::pin(async { Ok(IvyJson::Null) }))
        .unwrap()
        .wait()
        .await
        .unwrap();
    owner.close().await.unwrap();
    drop(held);
}
