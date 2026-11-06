use super::*;
use futures_util::FutureExt;
use tokio::sync::{Semaphore, mpsc};

fn heap_params(capacity: usize) -> IvyJson {
    let mut units = Vec::with_capacity(capacity);
    units.extend([65, 0xd800]);
    IvyJson::String(JsString(units))
}

#[tokio::test]
async fn received_original_frame_keeps_heap_admission_until_its_consumer_finishes() {
    let (rpc, mut peer, incoming) =
        NativeRpc::start_frames(Arc::new(catalog()), Events::default()).unwrap();
    let (sent, mut observed) = mpsc::unbounded_channel();
    let scenario = std::panic::AssertUnwindSafe(async {
        let mut originals = Vec::new();
        for index in 0..6 {
            let client = rpc.clone();
            let sent = sent.clone();
            originals.push(tokio::spawn(async move {
                client
                    .request(
                        "account/logout".into(),
                        heap_params(4 * 1024 * 1024),
                        RequestOptions {
                            request_id: Some(format!("original-{index}").into()),
                            before_send: Some(Arc::new(move |_| {
                                sent.send(()).unwrap();
                                Box::pin(async { Ok(()) })
                            })),
                            ..Default::default()
                        },
                    )
                    .await
            }));
            tokio::time::timeout(Duration::from_secs(5), observed.recv())
                .await
                .unwrap()
                .unwrap();
        }
        let held = peer.recv().await.unwrap();
        assert_eq!(held.get("id"), Some(&IvyJson::from("original-0")));
        let value = held.get("params").unwrap().as_js_string().unwrap();
        assert_eq!(value.0.capacity(), 4 * 1024 * 1024);
        assert_eq!(value.0.as_slice(), &[65, 0xd800]);
        let next = tokio::spawn({
            let client = rpc.clone();
            async move {
                client
                    .request(
                        "account/logout".into(),
                        heap_params(4 * 1024 * 1024),
                        RequestOptions {
                            request_id: Some("original-6".into()),
                            ..Default::default()
                        },
                    )
                    .await
            }
        });
        let refused = tokio::time::timeout(Duration::from_secs(2), next)
            .await
            .expect("Receiving alone released the original retained-byte reservation")
            .unwrap()
            .unwrap_err();
        assert_eq!(refused.code, "native_capacity");
        assert_eq!(refused.outcome, Outcome::NotExecuted);
        drop(held);
        let retry = tokio::spawn({
            let client = rpc.clone();
            async move {
                client
                    .request(
                        "account/logout".into(),
                        heap_params(4 * 1024 * 1024),
                        RequestOptions {
                            request_id: Some("original-6".into()),
                            ..Default::default()
                        },
                    )
                    .await
            }
        });
        // The previous call was refused before admission; the same original
        // identity is now safe to admit, without replaying either accepted call.
        for id in (1..=6).map(|index| format!("original-{index}")) {
            let frame = peer.recv().await.unwrap();
            assert_eq!(frame.get("id"), Some(&IvyJson::from(id.as_str())));
            incoming
                .try_send(IvyJson::object([
                    ("id", id.into()),
                    ("result", IvyJson::Null),
                ]))
                .unwrap();
        }
        incoming
            .try_send(IvyJson::object([
                ("id", "original-0".into()),
                ("result", IvyJson::Null),
            ]))
            .unwrap();
        assert!(result(retry).await.is_ok());
        for original in originals {
            assert!(result(original).await.is_ok());
        }
    })
    .catch_unwind()
    .await;
    rpc.close("test-complete").await;
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn original_local_event_already_keeps_its_reservation_through_the_durable_hook() {
    let gate = Arc::new(Semaphore::new(0));
    let (started, mut observed) = mpsc::unbounded_channel();
    let events = Events {
        on_notification: Arc::new({
            let gate = gate.clone();
            move |notification| {
                let gate = gate.clone();
                let started = started.clone();
                Box::pin(async move {
                    started.send(()).unwrap();
                    let _permit = gate.acquire().await.unwrap();
                    assert_eq!(
                        notification.params.as_js_string().unwrap().0.as_slice(),
                        &[65, 0xd800]
                    );
                    Ok(())
                })
            }
        }),
        ..Default::default()
    };
    let (rpc, _peer, incoming) = NativeRpc::start_frames(Arc::new(catalog()), events).unwrap();
    let frame = || {
        IvyJson::object([
            ("method", "thread/started".into()),
            ("params", heap_params(12 * 1024 * 1024)),
        ])
    };
    let scenario = std::panic::AssertUnwindSafe(async {
        incoming.try_send(frame()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), observed.recv())
            .await
            .unwrap()
            .unwrap();
        incoming.try_send(frame()).unwrap();
        let refused = incoming.try_send(frame()).unwrap_err();
        assert_eq!(refused.code, "native_capacity");
    })
    .catch_unwind()
    .await;
    gate.add_permits(1);
    rpc.close("test-complete").await;
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}
