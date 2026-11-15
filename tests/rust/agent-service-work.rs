use super::*;
use ivy_sdk::service::InvocationContext;
use std::time::Duration;
use tokio::sync::oneshot;

fn context() -> InvocationContext {
    InvocationContext::detached(
        "caller".into(),
        None,
        1,
        Default::default(),
        IvyError::internal(),
    )
}

#[tokio::test]
async fn original_state_reads_survive_native_drain_and_settle_before_retirement() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let work = AgentWork::default();
        let (retire, native_done) = oneshot::channel();
        let drain = work.close_after(Box::pin(async move {
            native_done.await.unwrap();
            Ok(())
        }));
        let handler: ToolHandler = Arc::new(|args, _| Box::pin(async move { Ok(args) }));
        let invoke = work.invoke_handler(handler.clone());
        for method in ["model/list", "thread/read", "thread/turns/list"] {
            let args = IvyJson::object([("method", method.into())]);
            assert_eq!(invoke(args.clone(), context()).await.unwrap(), args);
        }
        let error = invoke(
            IvyJson::object([("method", "turn/start".into())]),
            context(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "service_unavailable");
        let error = work.handler(handler)(IvyJson::Null, context())
            .await
            .unwrap_err();
        assert_eq!(error.code, "service_unavailable");

        let (finish_read, read_done) = oneshot::channel();
        let read_done = Mutex::new(Some(read_done));
        let read = work.read_handler(Arc::new(move |_, _| {
            let read_done = read_done.lock().unwrap().take().unwrap();
            Box::pin(async move { Ok(read_done.await.unwrap()) })
        }));
        let accepted_read = read(IvyJson::Null, context());
        retire.send(()).unwrap();
        let drained = tokio::spawn(drain);
        while !work.0.lock().unwrap().replies_closed {
            tokio::task::yield_now().await;
        }
        assert!(
            !drained.is_finished(),
            "Accepted reads still own native storage"
        );
        let error = read(IvyJson::Null, context()).await.unwrap_err();
        assert_eq!(error.code, "service_unavailable");
        finish_read.send(IvyJson::Bool(true)).unwrap();
        assert_eq!(accepted_read.await.unwrap(), IvyJson::Bool(true));
        drained.await.unwrap().unwrap();
        assert_eq!(work.pending().unwrap(), 0);
    })
    .await
    .expect("Read admission must retire without hanging shutdown");
}
