use super::*;

#[tokio::test(flavor = "current_thread")]
async fn sdk_reconnect_keeps_prior_sqlite_owner_until_calls_tick_stop_and_drain_finish() {
    let peer = Peer::start().await;
    let admission = Admission {
        calls: Cell::new(0),
        refuse: false,
    };
    let (service, owner) = make_service(setup(&peer, true), &admission).unwrap();
    assert_eq!(admission.calls.get(), 1);
    fn send_sync<T: Send + Sync>(_: &T) {}
    send_sync(&service.control);
    send_sync(&service.control.handler("workspace"));
    let factory = Factory::new(vec![Plan::default(), Plan::opened()]);
    let script = async {
        service.start();
        until(|| factory.count(1, "verify") == 1).await;
        assert!(!service.client.ready());
        assert!(!peer.methods(1).contains(&"objects.stat".into()));
        let checking = factory.connections.borrow()[0].clone();
        assert_eq!(
            service
                .control
                .readiness(&checking)
                .await
                .unwrap_err()
                .message_js(),
            "Workflow state is not attached to this generation.".into()
        );
        let refused = service.control.handler("workspace")(Json::Null, context(1))
            .await
            .unwrap_err();
        assert_eq!(
            refused.message_js(),
            "TaskBoard has no reconciled current connection generation.".into()
        );
        factory.plans[0].verify.release();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        until(|| factory.count(1, "tick") == 1).await;
        let remote = {
            let first = first.clone();
            tokio::spawn(async move {
                rpc(
                    &first,
                    "test.invoke",
                    Json::object([("value", "socket invocation".into())]),
                )
                .await
            })
        };
        until(|| factory.count(1, "call") == 1).await;
        let control = service.control.clone();
        std::thread::spawn(move || {
            drop(control.handler("workspace")(
                Json::object([("lost", true.into())]),
                context(1),
            ))
        })
        .join()
        .unwrap();
        until(|| factory.count(1, "call") == 2).await;
        rpc(
            &first,
            "test.notification",
            Json::object([
                ("method", "turn/completed".into()),
                ("serviceNodeId", "fixture-agent".into()),
                ("epoch", "fixture-epoch".into()),
                (
                    "params",
                    Json::object([("threadId", "fixture-thread".into())]),
                ),
            ]),
        )
        .await
        .unwrap();
        until(|| factory.count(1, "notification") == 1).await;
        assert_eq!(factory.count(1, "tick"), 1);
        rpc(&first, "test.close", Json::Null).await.unwrap();
        bounded(first.closed()).await;
        // SDK proceeds through the next real connection and registry sync while
        // the first local owner still retains its accepted work.
        until(|| peer.methods(2).contains(&"notifications.subscribe".into())).await;
        assert_eq!(factory.count(2, "create"), 0);
        assert_eq!(factory.count(1, "release"), 0);
        assert!(remote.await.unwrap().is_err());
        factory.plans[0].call.release();
        until(|| factory.count(1, "write") == 2 && factory.count(1, "stop") == 1).await;
        assert_eq!(factory.count(1, "drain"), 0);
        factory.plans[0].stop.release();
        until(|| factory.count(1, "stop.done") == 1).await;
        assert_eq!(factory.count(1, "drain"), 0);
        factory.plans[0].tick.release();
        until(|| factory.count(1, "drain") == 1).await;
        assert_eq!(factory.count(2, "create"), 0);
        factory.plans[0].drain.release();
        let second = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(second.generation(), 2);
        assert_eq!(factory.count(1, "release"), 1);
        assert_eq!(factory.count(2, "create"), 1);
        service.control.state(&ServiceState {
            status: "offline",
            generation: Some(1),
            code: None,
            message: None,
            phase: None,
        });
        let result = rpc(
            &second,
            "test.invoke",
            Json::object([("value", "next owner".into())]),
        )
        .await
        .unwrap();
        assert_eq!(n(result.get("result").unwrap(), "generation"), 2);
        let methods = peer.methods(1);
        let position = |name: &str| methods.iter().position(|s| s == name).unwrap();
        assert!(position("registry.sync") < position("notifications.subscribe"));
        assert!(position("notifications.subscribe") < position("serviceNodes.get"));
        assert!(position("serviceNodes.get") < position("objects.stat"));
        assert!(position("objects.stat") < position("service.heartbeat"));
        let frames = peer.frames.lock().unwrap();
        let open = frames
            .iter()
            .find(|v| v.get("kind") == Some(&"open".into()))
            .unwrap();
        assert_eq!(
            open.get("authorization"),
            Some(&"Bearer fixture-token".into())
        );
        assert_eq!(open.get("path"), Some(&"/ivy/ws".into()));
        let connect = frames
            .iter()
            .find(|v| v.get("method") == Some(&"service.connect".into()))
            .unwrap()
            .get("params")
            .unwrap();
        assert_eq!(connect.get("serviceName"), Some(&"task-board".into()));
        assert_eq!(connect.get("instanceMode"), Some(&"singleton".into()));
        let filter = frames
            .iter()
            .find(|v| v.get("method") == Some(&"notifications.subscribe".into()))
            .unwrap()
            .get("params")
            .unwrap()
            .get("filters")
            .unwrap();
        assert_eq!(
            filter,
            &json(r#"[{"namespace":"agent","name":"notification","version":"1.0.0"}]"#)
        );
        drop(frames);
        // Eager retirement and SDK stop continue after a lost close observer.
        drop(service.close());
        assert!(
            service.control.handler("workspace")(Json::Null, context(2))
                .await
                .is_err()
        );
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    result.unwrap();
    assert_eq!(factory.count(2, "release"), 1);
    let events = factory.events.borrow();
    assert!(
        events.iter().position(|v| *v == (1, "release")).unwrap()
            < events.iter().position(|v| *v == (2, "create")).unwrap()
    );
    drop(events);
    peer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn startup_refusal_and_failed_retirement_preserve_exact_errors_without_replacing_owner() {
    let peer = Peer::start().await;
    let admission = Admission {
        calls: Cell::new(0),
        refuse: true,
    };
    let mut invalid = setup(&peer, true);
    invalid.component_id = "wrong".into();
    assert_eq!(
        make_service(invalid, &admission).err().unwrap(),
        failure("settings")
    );
    assert_eq!(admission.calls.get(), 1);
    let admission = Admission {
        calls: Cell::new(0),
        refuse: false,
    };
    let mut invalid = setup(&peer, false);
    invalid.credential.0.clear();
    assert_eq!(
        make_service(invalid, &admission)
            .err()
            .unwrap()
            .message_js(),
        "TaskBoard requires its own component identity and Hive credential.".into()
    );
    let (service, owner) = make_service(setup(&peer, true), &admission).unwrap();
    let mut first = Plan::opened();
    first.owner_error = true;
    let mut second = Plan::opened();
    second.drain_error = true;
    let factory = Factory::new(vec![first, second]);
    let script = async {
        service.start();
        let ready = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(ready.generation(), 2);
        assert_eq!(factory.count(1, "release"), 1);
        assert!(!peer.methods(1).contains(&"objects.stat".into()));
        assert!(!peer.methods(1).contains(&"service.heartbeat".into()));
        let refused = factory.connections.borrow()[0].close_reason().unwrap();
        assert_eq!(refused, failure("owner"));
        rpc(&ready, "test.close", Json::Null).await.unwrap();
        until(|| factory.count(2, "release") == 1).await;
        until(|| peer.methods(3).contains(&"notifications.subscribe".into())).await;
        assert_eq!(factory.connections.borrow().len(), 2);
        assert_eq!(
            service.control.reconcile(ready).await.unwrap_err(),
            failure("drain")
        );
        assert_eq!(
            bounded(service.close()).await.unwrap_err(),
            failure("drain")
        );
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    assert_eq!(result.unwrap_err(), failure("drain"));
    assert_eq!(factory.count(2, "release"), 1);
    peer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn lost_reconcile_observer_still_installs_and_dropped_root_reports_unknown() {
    let peer = Peer::start().await;
    let admission = Admission {
        calls: Cell::new(0),
        refuse: false,
    };
    let (service, owner) = make_service(setup(&peer, false), &admission).unwrap();
    let factory = Factory::new(vec![Plan::opened()]);
    let script = async {
        service.start();
        let ready = bounded(service.client.wait_ready(None)).await.unwrap();
        // Reconcile a current connection directly, dropping the observation.
        // The local root retires and reconstructs the graph without a waiter.
        let superseded = (0..128)
            .map(|_| service.control.reconcile(ready.clone()))
            .collect::<Vec<_>>();
        drop(service.control.reconcile(ready.clone()));
        for original in superseded {
            assert_eq!(original.await.unwrap_err().code, "service_not_ready");
        }
        until(|| factory.count(1, "create") == 2).await;
        bounded(async {
            loop {
                match service.control.readiness(&ready).await {
                    Ok(value) => {
                        assert_eq!(value.get("ready"), Some(&true.into()));
                        break;
                    }
                    Err(error) if error.code_js() == JsString::from("service_not_ready") => {
                        tokio::time::sleep(Duration::from_millis(2)).await
                    }
                    Err(error) => panic!("Unexpected readiness refusal: {error:?}"),
                }
            }
        })
        .await;
        let reply = service.control.handler("workspace")(
            Json::object([("lostReconcile", true.into())]),
            context(1),
        );
        assert_eq!(n(&reply.await.unwrap(), "generation"), 1);
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    result.unwrap();
    let (service, owner) = make_service(setup(&peer, false), &admission).unwrap();
    let closed = service.control.close();
    drop(owner);
    let error = closed.await.unwrap_err();
    assert_eq!(error.code_js(), "internal_error".into());
    assert_eq!(error.outcome, Outcome::Unknown);
    bounded(service.close()).await.unwrap_err();
    peer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn startup_root_refusal_prevents_readiness_and_preserves_rpc_error_before_reconnect() {
    let peer = Peer::start().await;
    let admission = Admission {
        calls: Cell::new(0),
        refuse: false,
    };
    let mut options = setup(&peer, true);
    options
        .settings
        .insert("rootObjectId", "missing-root".into());
    let (service, owner) = make_service(options, &admission).unwrap();
    let factory = Factory::new(vec![Plan::opened(), Plan::opened()]);
    let script = async {
        service.start();
        let ready = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(ready.generation(), 2);
        assert_eq!(factory.count(1, "verify"), 1);
        assert_eq!(factory.count(1, "tick"), 0);
        assert_eq!(factory.count(1, "release"), 1);
        let error = factory.connections.borrow()[0].close_reason().unwrap();
        let expected = failure("root");
        assert_eq!(error.code_js(), expected.code_js());
        assert_eq!(error.message_js(), expected.message_js());
        assert_eq!(error.outcome, expected.outcome);
        assert_eq!(error.details, expected.details);
        let methods = peer.methods(1);
        assert!(methods.contains(&"objects.stat".into()));
        assert!(!methods.contains(&"service.heartbeat".into()));
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    result.unwrap();
    peer.finish().await;
}
