use super::*;
use futures_util::FutureExt;
use ivy_contracts::IvyResult;
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
};
use tokio::sync::mpsc;

type Call = Pin<Box<dyn Future<Output = IvyResult<IvyJson>> + Send>>;
fn body(bytes: usize) -> IvyJson {
    let mut units = Vec::with_capacity(bytes / 2);
    units.extend([65, 0xd800]);
    IvyJson::String(JsString(units))
}
async fn begin(
    rpc: &NativeRpc,
    peer: &mut codex_rpc::NativeFrameReceiver,
    id: &str,
    priority: codex_admission::Priority,
    committed: &mpsc::UnboundedSender<String>,
) -> Call {
    let id = id.to_owned();
    let expected = id.clone();
    let committed = committed.clone();
    let rpc = rpc.clone();
    let mut call: Call = Box::pin(async move {
        rpc.request(
            "account/logout".into(),
            IvyJson::Null,
            RequestOptions {
                request_id: Some(id.into()),
                priority,
                before_resolve: Some(Arc::new(move |id, _| {
                    committed
                        .send(id.as_js_string().unwrap().to_utf8().unwrap())
                        .unwrap();
                    Box::pin(async { Ok(()) })
                })),
                ..Default::default()
            },
        )
        .await
    });
    poll_fn(|cx| {
        assert!(call.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frame.get("id"), Some(&IvyJson::from(expected)));
    drop(frame);
    call
}
async fn deliver(
    input: &codex_rpc::NativeFrameSender,
    committed: &mut mpsc::UnboundedReceiver<String>,
    id: &str,
    payload: IvyJson,
    error: bool,
) {
    input
        .try_send(IvyJson::object([
            ("id", id.into()),
            (
                if error { "error" } else { "result" },
                if error {
                    IvyJson::object([("code", (-32000.0).into()), ("message", payload)])
                } else {
                    payload
                },
            ),
        ]))
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), committed.recv())
            .await
            .unwrap()
            .unwrap(),
        id
    );
}
fn exact(reply: &IvyJson, bytes: usize, error: bool) {
    let value = if error {
        reply.get("error").unwrap().get("message").unwrap()
    } else {
        reply.get("result").unwrap()
    };
    let text = value.as_js_string().unwrap();
    assert_eq!(text.0, [65, 0xd800]);
    assert_eq!(text.0.capacity(), bytes / 2);
}

#[tokio::test]
async fn borrowed_reply_observers_preserve_the_original_payload_after_waiter_loss() {
    let (rpc, mut peer, input) =
        NativeRpc::start_frames(Arc::new(catalog()), Events::default()).unwrap();
    let scenario = std::panic::AssertUnwindSafe(async {
        for abandoned in [false, true] {
            let payload = body(4 * 1024 * 1024);
            let pointer = payload.as_js_string().unwrap().0.as_ptr() as usize;
            let (observed, mut observations) = mpsc::unbounded_channel();
            let client = rpc.clone();
            let mut call: Call = Box::pin(async move {
                client
                    .request(
                        "account/logout".into(),
                        IvyJson::Null,
                        RequestOptions {
                            request_id: Some(format!("borrowed-{abandoned}").into()),
                            observe_reply: Some(Arc::new(move |reply| {
                                let value = reply.required("result")?.as_js_string().unwrap();
                                assert_eq!(value.0.as_ptr() as usize, pointer);
                                assert_eq!(value.0.capacity(), 2 * 1024 * 1024);
                                observed.send(()).unwrap();
                                Ok(())
                            })),
                            ..Default::default()
                        },
                    )
                    .await
            });
            poll_fn(|cx| {
                assert!(call.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
                .await
                .unwrap()
                .unwrap();
            let id = frame.required("id").unwrap().clone();
            let mut retained = (!abandoned).then_some(call);
            input
                .try_send(IvyJson::object([("id", id), ("result", payload)]))
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), observations.recv())
                .await
                .unwrap()
                .unwrap();
            if let Some(call) = retained.take() {
                let reply = call.await.unwrap();
                assert_eq!(
                    reply
                        .required("result")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .0
                        .as_ptr() as usize,
                    pointer
                );
            }
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
async fn failed_borrowed_reply_observation_keeps_the_original_outcome_unknown() {
    let (rpc, mut peer, input) =
        NativeRpc::start_frames(Arc::new(catalog()), Events::default()).unwrap();
    let scenario = std::panic::AssertUnwindSafe(async {
        let (committed, mut commits) = mpsc::unbounded_channel();
        let client = rpc.clone();
        let mut call: Call = Box::pin(async move {
            client
                .request(
                    "account/logout".into(),
                    IvyJson::Null,
                    RequestOptions {
                        request_id: Some("borrowed-error".into()),
                        before_resolve: Some(Arc::new(move |_, _| {
                            committed.send(()).unwrap();
                            Box::pin(async { Ok(()) })
                        })),
                        observe_reply: Some(Arc::new(|_| {
                            Err(IvyError::new("fixture_observer_failed", "failed"))
                        })),
                        ..Default::default()
                    },
                )
                .await
        });
        poll_fn(|cx| {
            assert!(call.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
            .await
            .unwrap()
            .unwrap();
        input
            .try_send(IvyJson::object([
                ("id", frame.required("id").unwrap().clone()),
                ("result", IvyJson::Null),
            ]))
            .unwrap();
        let error = call.await.unwrap_err();
        assert_eq!(error.code, "native_outcome_commit_failed");
        assert_eq!(error.outcome, Outcome::Unknown);
        assert_eq!(commits.try_recv().unwrap(), ());
        assert!(!rpc.connected());
    })
    .catch_unwind()
    .await;
    rpc.close("test-complete").await;
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn abandoned_native_waiters_still_commit_and_original_hook_errors_keep_heap_until_consumed() {
    use codex_admission::Priority::Background;
    let (rpc, mut peer, input) =
        NativeRpc::start_frames(Arc::new(catalog()), Events::default()).unwrap();
    let (committed, mut observed) = mpsc::unbounded_channel();
    let scenario = std::panic::AssertUnwindSafe(async {
        for id in ["abandoned-original-a", "abandoned-original-b"] {
            let original = begin(&rpc, &mut peer, id, Background, &committed).await;
            drop(original);
            deliver(&input, &mut observed, id, body(40 * 1024 * 1024), false).await;
        }
        let (entered, mut entries) = mpsc::unbounded_channel();
        let mut errors = vec![];
        for (index, bytes) in [24, 24, 12].into_iter().enumerate() {
            let client = rpc.clone();
            let entered = entered.clone();
            let mut original: Call = Box::pin(async move {
                client
                    .request(
                        "account/logout".into(),
                        IvyJson::Null,
                        RequestOptions {
                            request_id: Some(format!("original-hook-{index}").into()),
                            before_send: Some(Arc::new(move |_| {
                                let mut diagnostic = String::with_capacity(bytes * 1024 * 1024);
                                diagnostic.push_str("original hook failure");
                                let mut error = IvyError::new("original_hook_failure", diagnostic);
                                error.wire_message = Some(JsString(vec![65, 0xd800]));
                                entered.send(index).unwrap();
                                Box::pin(async move { Err(error) })
                            })),
                            ..Default::default()
                        },
                    )
                    .await
            });
            poll_fn(|cx| {
                assert!(original.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), entries.recv())
                    .await
                    .unwrap()
                    .unwrap(),
                index
            );
            errors.push(original);
        }
        let refused = errors.pop().unwrap().await.unwrap_err();
        assert_eq!(refused.code, "native_capacity");
        assert_eq!(refused.outcome, Outcome::NotExecuted);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), peer.recv())
                .await
                .is_err(),
            "A rejected before-send hook wrote a native request"
        );
        assert!(rpc.connected());
        rpc.close("original-errors-remain").await;
        for original in errors {
            let error = original.await.unwrap_err();
            assert_eq!(error.code, "original_hook_failure");
            assert_eq!(error.message, "original hook failure");
            assert_eq!(error.message.capacity(), 24 * 1024 * 1024);
            assert_eq!(error.wire_message.unwrap().0, [65, 0xd800]);
            assert_eq!(error.outcome, Outcome::NotExecuted);
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
async fn total_native_reply_heap_is_bounded_while_original_server_answers_remain_live() {
    use codex_admission::Priority::Interaction;
    let (requests, mut request_ids) = mpsc::unbounded_channel();
    let events = Events {
        on_request: Arc::new(move |request| {
            requests.send(request.id).unwrap();
            Box::pin(async { Ok(()) })
        }),
        ..Default::default()
    };
    let (rpc, mut peer, input) = NativeRpc::start_frames(Arc::new(catalog()), events).unwrap();
    let (committed, mut observed) = mpsc::unbounded_channel();
    let scenario = std::panic::AssertUnwindSafe(async {
        let success = begin(
            &rpc,
            &mut peer,
            "original-control-success",
            Interaction,
            &committed,
        )
        .await;
        deliver(
            &input,
            &mut observed,
            "original-control-success",
            body(30 * 1024 * 1024),
            false,
        )
        .await;
        let error = begin(
            &rpc,
            &mut peer,
            "original-control-error",
            Interaction,
            &committed,
        )
        .await;
        deliver(
            &input,
            &mut observed,
            "original-control-error",
            body(30 * 1024 * 1024),
            true,
        )
        .await;
        let oversized = begin(
            &rpc,
            &mut peer,
            "original-committed-overflow",
            Interaction,
            &committed,
        )
        .await;
        deliver(
            &input,
            &mut observed,
            "original-committed-overflow",
            body(8 * 1024 * 1024),
            false,
        )
        .await;
        let refused = oversized.await.unwrap_err();
        assert_eq!(refused.code, "native_capacity");
        assert_eq!(refused.outcome, Outcome::Unknown);
        let original_failure = rpc
            .request(
                "account/logout".into(),
                IvyJson::Null,
                RequestOptions {
                    request_id: Some("original-small-hook-error".into()),
                    before_send: Some(Arc::new(|_| {
                        Box::pin(async {
                            Err(IvyError::new(
                                "original_small_storage_failure",
                                "original hook diagnostic",
                            ))
                        })
                    })),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(original_failure.code, "original_small_storage_failure");
        assert_eq!(original_failure.message, "original hook diagnostic");
        assert_eq!(original_failure.outcome, Outcome::NotExecuted);
        input
            .try_send(value(
                r#"{"id":121,"method":"currentTime/read","params":{"threadId":"original-thread"}}"#,
            ))
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), request_ids.recv())
                .await
                .unwrap()
                .unwrap(),
            121.0.into()
        );
        let answered = Arc::new(AtomicUsize::new(0));
        rpc.answer(
            "currentTime/read".into(),
            121.0.into(),
            value(r#"{"result":{}}"#),
            Arc::new({
                let answered = answered.clone();
                move |_| {
                    answered.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(()) })
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(peer.recv().await.unwrap().get("id"), Some(&121.0.into()));
        assert_eq!(answered.load(Ordering::SeqCst), 1);
        assert!(rpc.connected());
        exact(&success.await.unwrap(), 30 * 1024 * 1024, false);
        let fresh = begin(
            &rpc,
            &mut peer,
            "fresh-original-control",
            Interaction,
            &committed,
        )
        .await;
        deliver(
            &input,
            &mut observed,
            "fresh-original-control",
            body(8 * 1024 * 1024),
            false,
        )
        .await;
        exact(&fresh.await.unwrap(), 8 * 1024 * 1024, false);
        rpc.close("known-originals-remain").await;
        exact(&error.await.unwrap(), 30 * 1024 * 1024, true);
    })
    .catch_unwind()
    .await;
    rpc.close("test-complete").await;
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn completed_original_replies_keep_all_call_slots_until_their_waiters_consume_them() {
    use codex_admission::Priority::{Background, Interaction};
    let (rpc, mut peer, input) =
        NativeRpc::start_frames(Arc::new(catalog()), Events::default()).unwrap();
    let (committed, mut observed) = mpsc::unbounded_channel();
    let scenario = std::panic::AssertUnwindSafe(async {
        let mut originals = vec![];
        for index in 0..56 {
            let id = format!("original-{index}");
            originals.push(begin(&rpc, &mut peer, &id, Background, &committed).await);
            deliver(&input, &mut observed, &id, IvyJson::Null, false).await;
        }
        let refused = tokio::time::timeout(
            Duration::from_secs(2),
            rpc.request(
                "account/logout".into(),
                IvyJson::Null,
                RequestOptions {
                    request_id: Some("refused-background".into()),
                    ..Default::default()
                },
            ),
        )
        .await
        .expect("Completed original replies released call slots before their waiters consumed them")
        .unwrap_err();
        assert_eq!(refused.code, "native_capacity");
        assert_eq!(refused.outcome, Outcome::NotExecuted);
        for index in 56..64 {
            let id = format!("original-{index}");
            originals.push(begin(&rpc, &mut peer, &id, Interaction, &committed).await);
            deliver(&input, &mut observed, &id, body(4), true).await;
        }
        let refused = tokio::time::timeout(
            Duration::from_secs(2),
            rpc.request(
                "account/logout".into(),
                IvyJson::Null,
                RequestOptions {
                    request_id: Some("refused-control".into()),
                    priority: Interaction,
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(refused.code, "native_capacity");
        assert_eq!(refused.outcome, Outcome::NotExecuted);
        rpc.notify("initialized".into(), IvyJson::Null)
            .await
            .unwrap();
        assert_eq!(
            peer.recv().await.unwrap().get("method"),
            Some(&"initialized".into())
        );
        for (index, original) in originals.into_iter().enumerate() {
            let reply = original.await.unwrap();
            if index < 56 {
                assert_eq!(reply, value(r#"{"result":null}"#));
            } else {
                exact(&reply, 4, true);
            }
        }
        let next = begin(
            &rpc,
            &mut peer,
            "refused-background",
            Background,
            &committed,
        )
        .await;
        deliver(
            &input,
            &mut observed,
            "refused-background",
            IvyJson::Null,
            false,
        )
        .await;
        next.await.unwrap();
    })
    .catch_unwind()
    .await;
    rpc.close("test-complete").await;
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn unread_native_success_and_error_payloads_keep_heap_bytes_and_reserved_controls() {
    use codex_admission::Priority::{Background, Interaction};
    let (rpc, mut peer, input) =
        NativeRpc::start_frames(Arc::new(catalog()), Events::default()).unwrap();
    let (committed, mut observed) = mpsc::unbounded_channel();
    let scenario = std::panic::AssertUnwindSafe(async {
        let success = begin(&rpc, &mut peer, "success", Background, &committed).await;
        deliver(
            &input,
            &mut observed,
            "success",
            body(24 * 1024 * 1024),
            false,
        )
        .await;
        let error = begin(&rpc, &mut peer, "error", Background, &committed).await;
        deliver(&input, &mut observed, "error", body(24 * 1024 * 1024), true).await;
        let refused = begin(&rpc, &mut peer, "already-committed", Background, &committed).await;
        deliver(
            &input,
            &mut observed,
            "already-committed",
            body(12 * 1024 * 1024),
            false,
        )
        .await;
        let refused = refused
            .await
            .expect_err("Unread original reply heap reservations were released before consumption");
        assert_eq!(refused.code, "native_capacity");
        assert_eq!(refused.outcome, Outcome::Unknown);
        assert!(
            rpc.connected(),
            "A valid committed reply refusal must not retire unrelated native work"
        );
        let control = begin(&rpc, &mut peer, "control", Interaction, &committed).await;
        deliver(
            &input,
            &mut observed,
            "control",
            body(2 * 1024 * 1024),
            false,
        )
        .await;
        exact(&control.await.unwrap(), 2 * 1024 * 1024, false);
        let taken = success.await.unwrap();
        exact(&taken, 24 * 1024 * 1024, false);
        // Returning the ordinary public J value ends this receiver-only quota.
        // Keep it alive to make that scope explicit; caller heap is separate.
        let next = begin(&rpc, &mut peer, "new-original", Background, &committed).await;
        deliver(
            &input,
            &mut observed,
            "new-original",
            body(12 * 1024 * 1024),
            false,
        )
        .await;
        exact(&next.await.unwrap(), 12 * 1024 * 1024, false);
        rpc.close("known-replies-remain").await;
        exact(&error.await.unwrap(), 24 * 1024 * 1024, true);
        drop(taken);
    })
    .catch_unwind()
    .await;
    rpc.close("test-complete").await;
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}
