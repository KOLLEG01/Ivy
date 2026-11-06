use super::*;
use futures_util::FutureExt;
use ivy_agent_manager::claude::bridge::{Bridge, SdkReply};
use std::{
    future::{Future, poll_fn},
    task::Poll,
    time::Duration,
};
use tokio::time::timeout;

async fn call(bridge: &Bridge, method: &str, params: J) -> Result<SdkReply> {
    timeout(Duration::from_secs(10), bridge.call(method, params))
        .await
        .expect("SDK reply did not arrive")
}

#[tokio::test]
async fn full_original_event_window_keeps_rpc_controls_and_callback_results_live() {
    let (bridge, mut events) =
        super::command_cases::transport_entry("tests/fixtures/agent-claude-bridge-transport.mjs")
            .await;
    let process = bridge.owned_process();
    let scenario = std::panic::AssertUnwindSafe(async {
        call(&bridge, "query/start", v(r#"{"turnId":"original","options":{"fixtureTag":"original"},"messages":[],"permissionCallback":true}"#)).await.unwrap();
        call(&bridge, "fixture/burst", v(r#"{"turnId":"original","count":256}"#)).await.unwrap();
        let data = timeout(Duration::from_secs(10), events.recv()).await.unwrap().unwrap();
        assert_eq!(f(&data, "type"), &J::from("message"));
        assert_eq!(f(f(&data, "message"), "index"), &J::Number(0.0));
        let mut callbacks = Vec::new();
        for index in 0..8 {
            let mut callback = Box::pin(bridge.call("fixture/permission", J::object([
                ("turnId", "original".into()), ("toolUseId", format!("original-{index}").into()),
            ])));
            poll_fn(|cx| match callback.as_mut().poll(cx) {
                Poll::Pending => Poll::Ready(()),
                Poll::Ready(result) => panic!("Callback was not admitted: {result:?}"),
            }).await;
            callbacks.push(callback);
        }
        timeout(Duration::from_secs(10), async {
            loop {
                call(&bridge, "fixture/inspect", v("{}")).await.unwrap();
                if events.len() == 8 { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("The control event window did not fill");
        assert_eq!(f(&data, "eventSequence"), &J::Number(1.0));
        let models = call(&bridge, "models", v(r#"{"options":{},"timeoutMs":30000}"#)).await.unwrap();
        assert_eq!(f(&models.as_array().unwrap()[0], "value"), &J::from("sonnet"));
        call(&bridge, "query/interrupt", v(r#"{"turnId":"original"}"#)).await.unwrap();
        for callback in callbacks {
            let result = timeout(Duration::from_secs(10), callback).await.unwrap().unwrap();
            assert_eq!(f(&result, "behavior"), &J::from("deny"));
            assert_eq!(f(&result, "message"), &J::from("turn already finished"));
        }
        // No queued SDK event must be drained to obtain those original replies.
        assert_eq!(events.len(), 8);
        let mut permissions = Vec::new();
        for id in 1..=8 {
            let frame = timeout(Duration::from_secs(10), events.recv()).await.unwrap().unwrap();
            assert_eq!(f(&frame, "type"), &J::from("permission"));
            assert_eq!(f(&frame, "callbackId"), &J::Number(f64::from(id)));
            permissions.push(frame);
        }
        // Receiving alone keeps the original credit. Only processing/drop can
        // release the next data frame; all eight controls remain held as well.
        assert!(events.try_recv().is_err());
        drop(data);
        let data = timeout(Duration::from_secs(10), events.recv()).await.unwrap().unwrap();
        assert_eq!(f(&data, "type"), &J::from("message"));
        assert_eq!(f(f(&data, "message"), "index"), &J::Number(1.0));
        bridge.stop().await.unwrap();
        assert_eq!(process.completion.wait().await.exit_code, Some(0));
        assert!(events.recv().await.is_none());
        drop(permissions);
        drop(data);
    }).catch_unwind().await;
    bridge.stop().await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&process)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn oversized_sdk_control_event_closes_without_acknowledging_or_replaying_its_original_call() {
    let (bridge, mut events) =
        super::command_cases::transport_entry("tests/fixtures/agent-claude-bridge-transport.mjs")
            .await;
    call(
        &bridge,
        "query/start",
        v(r#"{"turnId":"original","options":{},"messages":[],"permissionCallback":true}"#),
    )
    .await
    .unwrap();
    let result = call(
        &bridge,
        "fixture/permission",
        J::object([
            ("turnId", "original".into()),
            (
                "input",
                J::object([("body", JsString(vec![120; 2 * 1024 * 1024]).into())]),
            ),
        ]),
    )
    .await;
    bridge.stop().await.unwrap();
    assert_eq!(result.unwrap_err().name, "outcome_unknown");
    let failure = events.recv().await.unwrap();
    assert_eq!(f(&failure, "type"), &J::from("transportError"));
    assert!(f(&failure, "eventSequence").is_null());
    assert!(events.recv().await.is_none());
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn previous_sdk_wire_version_is_refused_before_an_original_result_is_accepted() {
    let (bridge, _events) =
        super::command_cases::transport_entry("tests/fixtures/agent-claude-command-pressure.mjs")
            .await;
    let result = call(&bridge, "fixture/wrongVersion", v("{}")).await;
    bridge.stop().await.unwrap();
    assert_eq!(result.unwrap_err().name, "outcome_unknown");
    assert!(
        ivy_host_runtime::process::owned_process_tree_stopped(&bridge.owned_process())
            .await
            .unwrap()
    );
}
