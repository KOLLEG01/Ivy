use super::*;
use futures_util::FutureExt;
use ivy_agent_manager::claude::bridge::{Bridge, SdkReply};
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
    time::Duration,
};
use tokio::time::timeout;

type Call = Pin<Box<dyn Future<Output = Result<SdkReply>> + Send>>;
fn call(bridge: &Bridge, method: &str, params: J) -> Call {
    let (bridge, method) = (bridge.clone(), method.to_owned());
    Box::pin(async move { bridge.call(&method, params).await })
}
async fn wait(call: Call) -> Result<SdkReply> {
    timeout(Duration::from_secs(15), call)
        .await
        .expect("Original SDK reply did not settle")
}
async fn admitted(call: &mut Call) {
    poll_fn(|cx| match call.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(result) => panic!("Original SDK call was not admitted: {result:?}"),
    })
    .await;
}
async fn bridge() -> Bridge {
    super::command_cases::transport_entry("tests/fixtures/agent-claude-command-pressure.mjs")
        .await
        .0
}
fn params(units: usize, tag: usize) -> J {
    J::object([
        ("units", (units as f64).into()),
        ("tag", (tag as f64).into()),
    ])
}

#[tokio::test]
async fn completed_original_replies_keep_slots_through_unpolled_and_retained_consumers() {
    let bridge = bridge().await;
    let scenario = std::panic::AssertUnwindSafe(async {
        let mut originals = VecDeque::new();
        for tag in 0..56 {
            let mut params = params(0, tag);
            params.insert("fail", (tag % 2 == 1).into());
            let mut original = call(&bridge, "fixture/replySize", params);
            admitted(&mut original).await;
            originals.push_back(original);
        }
        // This original reply is an ordered reader barrier: all preceding
        // SDK success/error replies arrived, but none of their callers polled.
        let mut controls = vec![
            wait(call(&bridge, "query/interrupt", v("{}")))
                .await
                .unwrap(),
        ];
        assert_eq!(
            wait(call(&bridge, "fixture/echo", v("{}")))
                .await
                .unwrap_err()
                .name,
            "native_capacity"
        );
        for tag in 1..8 {
            controls.push(
                wait(call(&bridge, "query/interrupt", params(0, tag)))
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(
            wait(call(&bridge, "permission/reply", v("{}")))
                .await
                .unwrap_err()
                .name,
            "native_capacity"
        );
        let first = wait(originals.pop_front().unwrap()).await.unwrap();
        assert_eq!(f(&first, "originalId"), &J::from("1"));
        assert_eq!(f(&first, "tag"), &J::Number(0.0));
        assert_eq!(
            wait(call(&bridge, "fixture/echo", v("{}")))
                .await
                .unwrap_err()
                .name,
            "native_capacity"
        );
        drop(first);
        let replacement = wait(call(
            &bridge,
            "fixture/echo",
            v(r#"{"original":"replacement\ud800"}"#),
        ))
        .await
        .unwrap();
        assert_eq!(replacement, v(r#"{"original":"replacement\ud800"}"#));
        let error = wait(originals.pop_front().unwrap()).await.unwrap_err();
        let expected = JsString(
            [
                "original-1-2".encode_utf16().collect::<Vec<_>>(),
                vec![0xd800],
            ]
            .concat(),
        );
        assert_eq!(error.message, expected);
        assert_eq!(error.name, "adapter_error");
        let another = wait(call(
            &bridge,
            "fixture/echo",
            v(r#"{"original":"after-error"}"#),
        ))
        .await
        .unwrap();
        assert_eq!(another, v(r#"{"original":"after-error"}"#));
        for (offset, original) in originals.into_iter().enumerate() {
            let tag = offset + 2;
            if tag % 2 == 0 {
                let reply = wait(original).await.unwrap();
                assert_eq!(f(&reply, "originalId"), &J::from((tag + 1).to_string()));
                assert_eq!(f(&reply, "tag"), &J::Number(tag as f64));
            } else {
                let error = wait(original).await.unwrap_err();
                let mut expected = format!("original-{tag}-{}", tag + 1)
                    .encode_utf16()
                    .collect::<Vec<_>>();
                expected.push(0xd800);
                assert_eq!(error.message, JsString(expected));
            }
        }
        bridge.stop().await.unwrap();
        assert_eq!(f(&controls[0], "tag"), &J::Null);
    })
    .catch_unwind()
    .await;
    bridge.stop().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn reply_heap_budget_preserves_controls_then_closes_without_replaying_an_accepted_overflow() {
    let bridge = bridge().await;
    let scenario = std::panic::AssertUnwindSafe(async {
        let mut replies = Vec::new();
        for tag in 0..3 {
            let reply = wait(call(
                &bridge,
                "fixture/replySize",
                params(6 * 1024 * 1024, tag),
            ))
            .await
            .unwrap();
            assert_eq!(s(f(&reply, "body")).0.len(), 6 * 1024 * 1024);
            assert_eq!(f(&reply, "tag"), &J::Number(tag as f64));
            replies.push(reply);
        }
        assert!(
            replies
                .iter()
                .map(|reply| reply.retained_bytes())
                .sum::<usize>()
                >= 48 * 1024 * 1024
        );
        let control = wait(call(
            &bridge,
            "query/interrupt",
            v(r#"{"original":"control"}"#),
        ))
        .await
        .unwrap();
        assert_eq!(control, v(r#"{"original":"control"}"#));
        let overflow = wait(call(
            &bridge,
            "fixture/replySize",
            params(4 * 1024 * 1024, 3),
        ))
        .await
        .unwrap_err();
        assert_eq!(overflow.name, "outcome_unknown");
        bridge.stop().await.unwrap();
        // Already delivered original results retain their identity and bytes
        // through exact-child stop; admission never keeps that child alive.
        for (tag, reply) in replies.iter().enumerate() {
            assert_eq!(f(reply, "originalId"), &J::from((tag + 1).to_string()));
            assert_eq!(s(f(reply, "body")).0.first(), Some(&120));
            assert_eq!(s(f(reply, "body")).0.last(), Some(&120));
        }
    })
    .catch_unwind()
    .await;
    bridge.stop().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn unpolled_original_sdk_errors_keep_their_heap_reservation_until_consumed() {
    let bridge = bridge().await;
    let scenario = std::panic::AssertUnwindSafe(async {
        let mut originals = Vec::new();
        for tag in 0..3 {
            let mut params = params(6 * 1024 * 1024, tag);
            params.insert("fail", true.into());
            let mut original = call(&bridge, "fixture/replySize", params);
            admitted(&mut original).await;
            originals.push(original);
        }
        let control = wait(call(
            &bridge,
            "query/interrupt",
            v(r#"{"original":"error-barrier"}"#),
        ))
        .await
        .unwrap();
        assert_eq!(control, v(r#"{"original":"error-barrier"}"#));
        let mut overflow = params(3 * 1024 * 1024, 3);
        overflow.insert("fail", true.into());
        let overflow = wait(call(&bridge, "fixture/replySize", overflow))
            .await
            .unwrap_err();
        assert_eq!(overflow.name, "outcome_unknown");
        bridge.stop().await.unwrap();
        for (tag, original) in originals.into_iter().enumerate() {
            let error = wait(original).await.unwrap_err();
            assert_eq!(error.name, "adapter_error");
            let mut suffix = format!("original-{tag}-{}", tag + 1)
                .encode_utf16()
                .collect::<Vec<_>>();
            suffix.push(0xd800);
            assert_eq!(error.message.0.len(), 6 * 1024 * 1024 + suffix.len());
            assert_eq!(&error.message.0[6 * 1024 * 1024..], suffix.as_slice());
        }
    })
    .catch_unwind()
    .await;
    bridge.stop().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn oversized_reply_is_refused_but_abandoned_originals_release_admission() {
    let bridge = bridge().await;
    let scenario = std::panic::AssertUnwindSafe(async {
        let mut oversized = params(16 * 1024 * 1024, 0);
        oversized.insert("extraUnits", J::Number((8 * 1024 * 1024) as f64));
        let mut abandoned = call(&bridge, "fixture/replySize", oversized.clone());
        admitted(&mut abandoned).await;
        drop(abandoned);
        // The original oversized result was read before this barrier. Its
        // absent recipient needs neither a retained value nor a byte permit.
        let barrier = wait(call(
            &bridge,
            "query/interrupt",
            v(r#"{"original":"after-abandon"}"#),
        ))
        .await
        .unwrap();
        assert_eq!(barrier, v(r#"{"original":"after-abandon"}"#));
        oversized.insert("tag", J::Number(1.0));
        let observed = wait(call(&bridge, "fixture/replySize", oversized))
            .await
            .unwrap_err();
        assert_eq!(observed.name, "outcome_unknown");
    })
    .catch_unwind()
    .await;
    bridge.stop().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
}
