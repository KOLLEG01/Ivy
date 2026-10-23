use super::*;
use crate::loopback_probe::{World, save};
use ivy_phone_bridge::native_probe::NativeProbeCommands;
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_probe_command_guards_and_fresh_failed_runs_agree() {
    let data: Value =
        serde_json::from_str(include_str!("../fixtures/phone-native-platform-cases.json")).unwrap();
    let original = data["probe"]["expected"].clone();
    let mut actual = vec![];
    for stage in ["unconfigured", "call", "closed", "sip-stopping"] {
        for extra in [false, true] {
            if stage == "unconfigured" && !extra {
                continue;
            }
            let (sip, media, _, config) = actual_owner(57000, 57998);
            let remaining = Arc::new(Remaining::default());
            remaining.close_gate.release();
            let world = World::new("open-failed");
            let probe = NativeProbeCommands::new(sip.clone(), remaining, Arc::new(world.clone()));
            if stage != "unconfigured" {
                sip.configure(&configuration(&config), |_| async { Ok(()) })
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
            }
            if stage == "call" {
                sip.prepare(CALL.into()).unwrap().prepared().await.unwrap();
            }
            if stage == "closed" {
                probe.shutdown().await.unwrap();
            }
            if stage == "sip-stopping" {
                sip.close().wait().await.unwrap();
            }
            let out = probe
                .invoke(command(
                    "audio.probe",
                    if extra { r#"{"extra":null}"# } else { "{}" },
                    1,
                    1,
                ))
                .await;
            actual.push(json!({"name":format!("{stage}:{extra}"),"error":out.error_code()}));
            assert_eq!(world.opens(), 0);
            probe.shutdown().await.unwrap();
            sip.close().wait().await.unwrap();
            assert_eq!(
                media.starts.load(Ordering::SeqCst),
                usize::from(stage == "call")
            );
        }
    }
    save("probe-guards-original.json", &original);
    save("probe-guards-rust.json", &json!(actual));
    assert_eq!(json!(actual), original);
    let (sip, _, _, _) = actual_owner(57000, 57998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let world = World::new("open-failed");
    let probe = NativeProbeCommands::new(sip.clone(), remaining, Arc::new(world.clone()));
    for _ in 0..2 {
        assert_eq!(
            probe
                .invoke(command("audio.probe", "{}", 1, 1))
                .await
                .error_code(),
            Some("operation_outcome_unknown")
        );
    }
    assert_eq!(world.opens(), 2);
    probe.shutdown().await.unwrap();
    sip.close().wait().await.unwrap();
    save(
        "probe-guards-comparison.json",
        &json!({"cases":7,"equal":true,"admittedPublicOriginalProbe":false,"failedFreshOpens":world.opens()}),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn composed_probe_pending_readers_call_admission_and_shutdown_hold_the_same_owner() {
    let mut rows = vec![];
    for failure in [false, true] {
        let (sip, media, _, config) = actual_owner(57000, 57998);
        let endpoint = sip
            .configure(&configuration(&config), |_| async { Ok(()) })
            .unwrap()
            .wait()
            .await
            .unwrap();
        let remaining = Arc::new(Remaining::default());
        remaining
            .close_mode
            .store(usize::from(failure), Ordering::SeqCst);
        let world = World::gated();
        let handler = Arc::new(
            NativeSipCommands::new(
                sip.clone(),
                remaining.clone(),
                |_| Box::pin(async { Ok(()) }),
                closed_platform_views(),
            )
            .with_probe(Arc::new(world.clone())),
        );
        let first = handler.start(command("audio.probe", "{}", 1, 1));
        assert_eq!(world.opens(), 1);
        drop(first);
        let waiting = handler.invoke(command("audio.probe", "{}", 1, 2));
        assert_eq!(world.opens(), 1);
        let call = sip.prepare(CALL.into()).unwrap();
        call.prepared().await.unwrap();
        assert_eq!(
            handler
                .invoke(command("audio.probe", "{}", 1, 3))
                .await
                .error_code(),
            Some("runtime_not_ready")
        );
        assert_eq!(
            handler
                .invoke(command("audio.probe", r#"{"extra":null}"#, 1, 4))
                .await
                .error_code(),
            Some("invalid_request")
        );
        assert_eq!(world.opens(), 1);
        drop(handler.shutdown());
        drop(waiting);
        let shutdown = handler.shutdown();
        world.release_open();
        crate::loopback_probe::until(|| world.closes() == 1).await;
        assert_eq!(world.live(), 1);
        assert_eq!(remaining.closes.load(Ordering::SeqCst), 0);
        assert!(!world.events().iter().any(|e| e == "start"));
        sip.close().wait().await.unwrap();
        assert!(std::net::UdpSocket::bind(endpoint).is_ok());
        world.release_close();
        until(|| remaining.closes.load(Ordering::SeqCst) == 1).await;
        assert_eq!(world.live(), 0);
        cancel(handler.shutdown()).await;
        remaining.close_gate.release();
        assert_eq!(shutdown.await.is_err(), failure);
        assert_eq!(handler.shutdown().await.is_err(), failure);
        assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
        assert_eq!(media.starts.load(Ordering::SeqCst), 1);
        rows.push(json!({"remainingFailure":failure,"opens":world.opens(),"recorderCloses":world.closes(),"remainingCloses":remaining.closes.load(Ordering::SeqCst),"allReadObserversDropped":true,"pendingShared":true,"callGuardBeforePendingReuse":true,"sipRetiredBeforeLateRecorder":true,"events":world.events()}));
    }
    save("probe-composed-shutdown.json", &json!(rows));
}
