use super::{EPOCH, OP, request};
use ivy_phone_bridge::{
    native_rpc::{NativeDispatcher, NativeHandler, NativeInvocation, NativeOutcome, NativeRequest},
    sip::{
        DelayedInviteMedia, HostedSipMedia, SipHost, SipHostBinding, SipHostConfiguration,
        SipHostError,
    },
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};
use tokio::sync::Semaphore;

struct Media {
    closed: AtomicBool,
    closes: AtomicUsize,
    drain: Semaphore,
}
impl DelayedInviteMedia for Media {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    fn failed(&self) -> bool {
        false
    }
    fn accept_answer(&self, _: &str) -> bool {
        false
    }
    fn negotiate_offer(&self, _: &str) -> Option<String> {
        None
    }
    fn rejected_answer(&self, _: &str) -> Option<String> {
        None
    }
    fn start(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async { panic!("prepared call must not start media") })
    }
}
impl HostedSipMedia for Media {
    fn request_close(&self) {
        self.closed.store(true, Ordering::Release);
    }
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), SipHostError>> + Send + '_>> {
        Box::pin(async move {
            self.closes.fetch_add(1, Ordering::AcqRel);
            let permit = self.drain.acquire().await.unwrap();
            permit.forget();
            Ok(())
        })
    }
}
struct HostHandler {
    host: SipHost,
    invocations: AtomicUsize,
    cleanup_queries: AtomicUsize,
}
impl NativeHandler for HostHandler {
    fn cleanup_slots(&self) -> usize {
        self.cleanup_queries.fetch_add(1, Ordering::AcqRel);
        self.host.observation().calls.len() * 4
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        self.invocations.fetch_add(1, Ordering::AcqRel);
        let id = request.params().get("callId").and_then(|v| v.as_str().ok());
        // The actual original release DTO and result, using the real host.
        if request.method() != "call.release"
            || request.params().as_object().unwrap().len() != 1
            || id.is_none()
        {
            return Box::pin(async { NativeOutcome::error("invalid_request") });
        }
        let id = id.unwrap();
        let host = self.host.clone();
        let original = host.find(&id);
        Box::pin(async move {
            let Some(original) = original else {
                return NativeOutcome::error("runtime_not_ready");
            };
            match host.release(&original).await {
                Ok(()) => {
                    NativeOutcome::result(format!("{{\"callId\":\"{id}\",\"released\":true}}"))
                }
                Err(_) => NativeOutcome::error("operation_outcome_unknown"),
            }
        })
    }
}
async fn cancel_observer<F: Future>(future: F) {
    let mut future = Box::pin(future);
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(future);
}
async fn until(ready: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn host() -> SipHost {
    let host = SipHost::new();
    let binding = SipHostBinding::new(
        "127.0.0.1:0".parse().unwrap(),
        rsipstack::sip::Transport::Udp,
        None,
        1,
    )
    .unwrap();
    host.configure(SipHostConfiguration::new(binding, None).unwrap())
        .await
        .unwrap();
    host
}

#[tokio::test]
async fn native_receipt_replays_one_actual_host_release_after_all_original_waiters_leave() {
    let host = host().await;
    let media = Arc::new(Media {
        closed: AtomicBool::new(false),
        closes: AtomicUsize::new(0),
        drain: Semaphore::new(0),
    });
    host.prepare(EPOCH.into(), || Ok(media.clone())).unwrap();
    let handler = Arc::new(HostHandler {
        host: host.clone(),
        invocations: AtomicUsize::new(0),
        cleanup_queries: AtomicUsize::new(0),
    });
    let dispatcher = NativeDispatcher::new(EPOCH.into(), 16, handler.clone()).unwrap();
    let params = format!("{{\"callId\":\"{EPOCH}\"}}");
    cancel_observer(dispatcher.execute(request("call.release", Some(OP), &params, 1))).await;
    until(|| media.closes.load(Ordering::Acquire) == 1).await;
    assert_eq!(dispatcher.receipt_count(), 1);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            dispatcher.execute(request("call.release", Some(OP), &params, 2))
        )
        .await
        .is_err()
    );
    assert_eq!(host.observation().calls.len(), 1);
    media.drain.add_permits(1);
    let reply = dispatcher
        .execute(request("call.release", Some(OP), &params, 3))
        .await;
    assert_eq!(reply.request_id(), 3);
    assert_eq!(
        reply.outcome().result_json(),
        Some(format!("{{\"callId\":\"{EPOCH}\",\"released\":true}}").as_str())
    );
    let parsed = ivy_contracts::json::parse(&reply.to_json().unwrap(), 65536).unwrap();
    assert_eq!(parsed.get("requestId").unwrap().as_f64(), Some(3.0));
    assert_eq!(parsed.get("ok").unwrap().as_bool(), Some(true));
    assert!(host.observation().calls.is_empty());
    // A replay cannot act on a newly prepared owner with the same visible ID.
    let next = Arc::new(Media {
        closed: AtomicBool::new(false),
        closes: AtomicUsize::new(0),
        drain: Semaphore::new(1),
    });
    host.prepare(EPOCH.into(), || Ok(next.clone())).unwrap();
    dispatcher
        .execute(request("call.release", Some(OP), &params, 4))
        .await;
    assert_eq!(host.observation().calls.len(), 1);
    assert_eq!(next.closes.load(Ordering::Acquire), 0);
    assert_eq!(handler.invocations.load(Ordering::Acquire), 1);
    assert_eq!(
        handler.cleanup_queries.load(Ordering::Acquire),
        1,
        "replays never call capacity provider"
    );
    let changed = dispatcher
        .execute(request(
            "call.release",
            Some(OP),
            &format!("{{\"callId\":\"{OP}\"}}"),
            5,
        ))
        .await;
    assert_eq!(changed.outcome().error_code(), Some("operation_conflict"));
    host.close().await.unwrap();
}

#[derive(Clone, Copy)]
enum Behavior {
    Success,
    ConstructorPanic,
    FuturePanic,
    OverBudget,
}
struct Handler {
    behavior: Behavior,
    calls: AtomicUsize,
    capacity: AtomicUsize,
}
impl Handler {
    fn new(behavior: Behavior) -> Arc<Self> {
        Arc::new(Self {
            behavior,
            calls: AtomicUsize::new(0),
            capacity: AtomicUsize::new(0),
        })
    }
}
impl NativeHandler for Handler {
    fn cleanup_slots(&self) -> usize {
        self.capacity.fetch_add(1, Ordering::AcqRel);
        0
    }
    fn invoke(&self, _: NativeRequest) -> NativeInvocation {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if matches!(self.behavior, Behavior::ConstructorPanic) {
            panic!("synthetic constructor panic");
        }
        let behavior = self.behavior;
        Box::pin(async move {
            match behavior {
                Behavior::Success => NativeOutcome::result("{\"safe\":true}".into()),
                Behavior::FuturePanic => panic!("synthetic invocation panic"),
                Behavior::OverBudget => {
                    NativeOutcome::result(format!("{{\"value\":\"{}\"}}", "x".repeat(65536)))
                }
                _ => unreachable!(),
            }
        })
    }
}

#[tokio::test]
async fn stop_between_admission_and_invocation_is_checked_before_any_handler_effect() {
    let handler = Handler::new(Behavior::Success);
    let dispatcher = NativeDispatcher::new(EPOCH.into(), 16, handler.clone()).unwrap();
    cancel_observer(dispatcher.execute(request("call.prepare", Some(OP), "{}", 1))).await;
    dispatcher.stop();
    let result = dispatcher
        .execute(request("call.prepare", Some(OP), "{}", 2))
        .await;
    assert_eq!(result.outcome().error_code(), Some("runtime_stopping"));
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert_eq!(handler.calls.load(Ordering::Acquire), 0);
    assert_eq!(dispatcher.receipt_count(), 1);
    let wrong = super::envelope("status", None, "{}", 3).replace(EPOCH, OP);
    let reply = dispatcher
        .execute(NativeRequest::parse(wrong.as_bytes()).unwrap())
        .await;
    assert_eq!(reply.outcome().error_code(), Some("epoch_mismatch"));
    assert_eq!(
        ivy_contracts::json::parse(&reply.to_json().unwrap(), 65536)
            .unwrap()
            .get("epoch")
            .unwrap()
            .as_str()
            .unwrap(),
        EPOCH
    );
}

#[tokio::test]
async fn handler_panics_and_oversized_results_complete_the_original_safe_receipt() {
    for behavior in [
        Behavior::ConstructorPanic,
        Behavior::FuturePanic,
        Behavior::OverBudget,
    ] {
        let handler = Handler::new(behavior);
        let dispatcher = NativeDispatcher::new(EPOCH.into(), 16, handler.clone()).unwrap();
        let first = tokio::time::timeout(
            Duration::from_secs(2),
            dispatcher.execute(request("call.prepare", Some(OP), "{}", 1)),
        )
        .await
        .unwrap();
        assert_eq!(
            first.outcome().error_code(),
            Some("operation_outcome_unknown")
        );
        let next = dispatcher
            .execute(request("call.prepare", Some(OP), "{}", 2))
            .await;
        assert_eq!(next.outcome(), first.outcome());
        assert_eq!(next.request_id(), 2);
        assert_eq!(handler.calls.load(Ordering::Acquire), 1);
        assert_eq!(handler.capacity.load(Ordering::Acquire), 1);
        let json = next.to_json().unwrap();
        assert!(json.len() < 256);
        assert!(!json.contains("synthetic"));
        // A caught constructor panic must not poison later admission.
        assert_eq!(
            dispatcher
                .execute(request("status", None, "{}", 3))
                .await
                .outcome()
                .error_code(),
            Some("operation_outcome_unknown")
        );
    }
}

#[tokio::test]
async fn read_only_requests_have_fresh_replies_and_do_not_reserve_effect_receipts() {
    let handler = Handler::new(Behavior::Success);
    let dispatcher = NativeDispatcher::new(EPOCH.into(), 1, handler.clone()).unwrap();
    for id in [1, 2] {
        let reply = dispatcher.execute(request("status", None, "{}", id)).await;
        assert_eq!(reply.request_id(), id);
        assert_eq!(reply.outcome().error_code(), None);
    }
    assert_eq!(handler.calls.load(Ordering::Acquire), 2);
    assert_eq!(handler.capacity.load(Ordering::Acquire), 0);
    assert_eq!(dispatcher.receipt_count(), 0);
}

struct DepthHandler(usize);
impl NativeHandler for DepthHandler {
    fn cleanup_slots(&self) -> usize {
        0
    }
    fn invoke(&self, _: NativeRequest) -> NativeInvocation {
        let json = format!("{}0{}", "[".repeat(self.0), "]".repeat(self.0));
        Box::pin(async move { NativeOutcome::result(json) })
    }
}
#[tokio::test]
async fn final_response_envelope_preserves_original_native_write_depth_boundary() {
    for (depth, written) in [(23, true), (24, false)] {
        let dispatcher =
            NativeDispatcher::new(EPOCH.into(), 1, Arc::new(DepthHandler(depth))).unwrap();
        let reply = dispatcher.execute(request("status", None, "{}", 1)).await;
        assert_eq!(reply.outcome().error_code(), None);
        assert_eq!(reply.to_json().is_ok(), written);
    }
}
