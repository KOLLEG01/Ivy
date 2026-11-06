use super::*;
use ivy_contracts::IvyResult;
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
};
use tokio::sync::{Semaphore, mpsc};

type Call = Pin<Box<dyn Future<Output = IvyResult<IvyJson>> + Send>>;
enum Transport {
    Stream(Peer),
    Local(codex_rpc::NativeFrameReceiver, codex_rpc::NativeFrameSender),
}
impl Transport {
    fn start(local: bool) -> (NativeRpc, Self) {
        if local {
            let (rpc, output, input) =
                NativeRpc::start_frames(Arc::new(catalog()), Events::default()).unwrap();
            (rpc, Self::Local(output, input))
        } else {
            let (rpc, peer) = fixture(Events::default());
            (rpc, Self::Stream(peer))
        }
    }
    async fn sent(&mut self) -> IvyJson {
        match self {
            Self::Stream(peer) => peer.sent().await,
            Self::Local(peer, _) => (*tokio::time::timeout(Duration::from_secs(5), peer.recv())
                .await
                .unwrap()
                .unwrap())
            .clone(),
        }
    }
    async fn reply(&mut self, id: IvyJson) {
        match self {
            Self::Stream(peer) => peer.reply(id).await,
            Self::Local(_, input) => input
                .try_send(IvyJson::object([("id", id), ("result", IvyJson::Null)]))
                .unwrap(),
        }
    }
}
async fn once<F: Future + ?Sized>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}
fn heap(bytes: usize) -> IvyJson {
    let mut units = Vec::with_capacity(bytes / 2);
    units.extend([65, 0xd800]);
    IvyJson::String(JsString(units))
}
fn call(rpc: &NativeRpc, id: &str, params: IvyJson, mut options: RequestOptions) -> Call {
    options.request_id = Some(id.into());
    let rpc = rpc.clone();
    Box::pin(async move { rpc.request("account/logout".into(), params, options).await })
}
fn refused<T: std::fmt::Debug>(poll: Poll<IvyResult<T>>) {
    let Poll::Ready(Err(error)) = poll else {
        panic!("Command retained its payload while the actor was blocked: {poll:?}");
    };
    assert_eq!(error.code, "native_capacity");
    assert_eq!(error.outcome, Outcome::NotExecuted);
}
async fn gate(rpc: &NativeRpc) -> (Call, Arc<Semaphore>) {
    let (entered, mut entering) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let permit = release.clone();
    let mut first = call(
        rpc,
        "gate",
        IvyJson::Null,
        RequestOptions {
            before_send: Some(Arc::new(move |_| {
                let permit = permit.clone();
                entered.send(()).unwrap();
                Box::pin(async move {
                    permit.acquire().await.unwrap().forget();
                    Ok(())
                })
            })),
            ..Default::default()
        },
    );
    assert!(once(first.as_mut()).await.is_pending());
    tokio::time::timeout(Duration::from_secs(5), entering.recv())
        .await
        .unwrap()
        .unwrap();
    (first, release)
}

async fn queued_heap(local: bool) {
    let (rpc, mut peer) = Transport::start(local);
    let (first, release) = gate(&rpc).await;
    let (committed, mut commits) = mpsc::unbounded_channel();
    let hooks = Arc::new(AtomicUsize::new(0));
    let mut originals = Vec::new();
    for index in 0..6 {
        let hooks = hooks.clone();
        let committed = committed.clone();
        let mut original = call(
            &rpc,
            &format!("bulk-{index}"),
            heap(8 * 1024 * 1024),
            RequestOptions {
                before_send: Some(Arc::new(move |_| {
                    hooks.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(()) })
                })),
                before_resolve: Some(Arc::new(move |id, _| {
                    committed.send(id).unwrap();
                    Box::pin(async { Ok(()) })
                })),
                ..Default::default()
            },
        );
        assert!(once(original.as_mut()).await.is_pending());
        originals.push(original);
    }
    let mut overflow = call(
        &rpc,
        "overflow",
        heap(8 * 1024 * 1024),
        RequestOptions::default(),
    );
    refused(once(overflow.as_mut()).await);
    drop(originals.remove(0));
    drop(first);
    // Abandoning original waiters must not free queued or processing ownership.
    let mut still_full = call(
        &rpc,
        "overflow",
        heap(8 * 1024 * 1024),
        RequestOptions::default(),
    );
    refused(once(still_full.as_mut()).await);
    assert_eq!(hooks.load(Ordering::SeqCst), 0);
    let mut notification = Box::pin(rpc.notify("initialized".into(), IvyJson::Null));
    assert!(once(notification.as_mut()).await.is_pending());
    let answer_id = IvyJson::String(JsString(vec![65, 0xd800]));
    let answered = Arc::new(AtomicUsize::new(0));
    let seen = answered.clone();
    let mut answer = Box::pin(rpc.answer(
        "currentTime/read".into(),
        answer_id.clone(),
        IvyJson::object([("result", IvyJson::object([] as [(&str, IvyJson); 0]))]),
        Arc::new(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }),
    ));
    assert!(once(answer.as_mut()).await.is_pending());
    let mut control = call(
        &rpc,
        "control",
        IvyJson::Null,
        RequestOptions {
            priority: codex_admission::Priority::Interaction,
            ..Default::default()
        },
    );
    assert!(once(control.as_mut()).await.is_pending());
    release.add_permits(1);
    for index in 0..7 {
        let frame = peer.sent().await;
        let id = if index == 0 {
            "gate".to_owned()
        } else {
            format!("bulk-{}", index - 1)
        };
        assert_eq!(frame.get("id"), Some(&IvyJson::from(id)));
        if index > 0 {
            assert_eq!(frame.get("params"), Some(&heap(4)));
        }
        peer.reply(frame.required("id").unwrap().clone()).await;
    }
    assert_eq!(peer.sent().await.get("method"), Some(&"initialized".into()));
    assert_eq!(peer.sent().await.get("id"), Some(&answer_id));
    let frame = peer.sent().await;
    assert_eq!(frame.get("id"), Some(&"control".into()));
    peer.reply("control".into()).await;
    for original in originals {
        assert!(
            tokio::time::timeout(Duration::from_secs(5), original)
                .await
                .unwrap()
                .is_ok()
        );
    }
    assert!(notification.await.is_ok());
    assert!(answer.await.is_ok());
    assert!(control.await.is_ok());
    for index in 0..6 {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), commits.recv())
                .await
                .unwrap()
                .unwrap(),
            IvyJson::from(format!("bulk-{index}"))
        );
    }
    assert_eq!(hooks.load(Ordering::SeqCst), 6);
    assert_eq!(answered.load(Ordering::SeqCst), 1);
    // Consumption releases the same pool and leaves a refused original ID unused.
    let mut retry = call(
        &rpc,
        "overflow",
        heap(8 * 1024 * 1024),
        RequestOptions::default(),
    );
    assert!(once(retry.as_mut()).await.is_pending());
    let frame = peer.sent().await;
    assert_eq!(frame.get("id"), Some(&"overflow".into()));
    peer.reply("overflow".into()).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), retry)
            .await
            .unwrap()
            .is_ok()
    );
    rpc.close("fixture_complete").await;
}
#[tokio::test]
async fn stream_queued_commands_bound_heap_before_the_actor_and_keep_abandoned_owners() {
    queued_heap(false).await;
}
#[tokio::test]
async fn local_queued_commands_bound_heap_before_the_actor_and_keep_abandoned_owners() {
    queued_heap(true).await;
}

#[tokio::test]
async fn tiny_queued_commands_reserve_eight_controls_and_allow_original_close() {
    for local in [false, true] {
        let (rpc, _peer) = Transport::start(local);
        let (first, release) = gate(&rpc).await;
        let mut originals = vec![first];
        let sent = Arc::new(AtomicUsize::new(0));
        for index in 0..55 {
            let sent = sent.clone();
            let mut original = call(
                &rpc,
                &format!("data-{index}"),
                IvyJson::Null,
                RequestOptions {
                    before_send: Some(Arc::new(move |_| {
                        sent.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async { Ok(()) })
                    })),
                    ..Default::default()
                },
            );
            assert!(once(original.as_mut()).await.is_pending());
            originals.push(original);
        }
        let mut extra = call(&rpc, "refused", IvyJson::Null, RequestOptions::default());
        refused(once(extra.as_mut()).await);
        for index in 0..7 {
            let sent = sent.clone();
            let mut control = call(
                &rpc,
                &format!("control-{index}"),
                IvyJson::Null,
                RequestOptions {
                    priority: codex_admission::Priority::Interaction,
                    before_send: Some(Arc::new(move |_| {
                        sent.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async { Ok(()) })
                    })),
                    ..Default::default()
                },
            );
            assert!(once(control.as_mut()).await.is_pending());
            originals.push(control);
        }
        let mut notification = Box::pin(rpc.notify("initialized".into(), IvyJson::Null));
        assert!(once(notification.as_mut()).await.is_pending());
        let mut extra_control = call(
            &rpc,
            "control-refused",
            IvyJson::Null,
            RequestOptions {
                priority: codex_admission::Priority::Interaction,
                ..Default::default()
            },
        );
        refused(once(extra_control.as_mut()).await);
        let mut close = Box::pin(rpc.close("original_close"));
        assert!(once(close.as_mut()).await.is_pending());
        release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), close)
            .await
            .unwrap();
        assert_eq!(rpc.closed().await, "original_close");
        for original in originals {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), original)
                    .await
                    .unwrap()
                    .unwrap_err()
                    .outcome,
                Outcome::Unknown
            );
        }
        assert!(notification.await.is_ok());
        // Close never promises that buffered stream writes reached the provider.
        // All original intents ran, and their waiters keep the unknown outcome.
        assert_eq!(sent.load(Ordering::SeqCst), 62);
    }
}

#[tokio::test]
async fn oversized_request_notification_answer_and_opaque_metadata_refuse_before_hooks() {
    for local in [false, true] {
        let (rpc, mut peer) = Transport::start(local);
        let (first, release) = gate(&rpc).await;
        let mut request = call(
            &rpc,
            "large",
            heap(50 * 1024 * 1024),
            RequestOptions::default(),
        );
        refused(once(request.as_mut()).await);
        let mut notification = Box::pin(rpc.notify("initialized".into(), heap(50 * 1024 * 1024)));
        refused(once(notification.as_mut()).await);
        let called = Arc::new(AtomicUsize::new(0));
        let seen = called.clone();
        let mut answer = Box::pin(rpc.answer(
            "currentTime/read".into(),
            (-2.0).into(),
            IvyJson::object([("result", heap(50 * 1024 * 1024))]),
            Arc::new(move |_| {
                seen.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }),
        ));
        refused(once(answer.as_mut()).await);
        let large_method = heap(50 * 1024 * 1024).into_js_string().unwrap();
        let mut method = Box::pin(rpc.notify(large_method, IvyJson::Null));
        refused(once(method.as_mut()).await);
        let large_id = heap(50 * 1024 * 1024).into_js_string().unwrap();
        let mut identity = Box::pin(rpc.request(
            "account/logout".into(),
            IvyJson::Null,
            RequestOptions {
                request_id: Some(large_id),
                ..Default::default()
            },
        ));
        refused(once(identity.as_mut()).await);
        assert_eq!(called.load(Ordering::SeqCst), 0);
        release.add_permits(1);
        assert_eq!(peer.sent().await.get("id"), Some(&"gate".into()));
        peer.reply("gate".into()).await;
        assert!(
            tokio::time::timeout(Duration::from_secs(5), first)
                .await
                .unwrap()
                .is_ok()
        );
        rpc.close("fixture_complete").await;
    }
}
