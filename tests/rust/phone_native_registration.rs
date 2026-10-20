use super::*;
use crate::sip_host::{received, response, send};
use ivy_phone_bridge::{
    native_registration::registration_json,
    sip::{
        RegistrationKeepaliveError, RegistrationLease, RegistrationObservation, RegistrationPeer,
        RegistrationPhase, RegistrationTransport,
    },
};
use rsipstack::sip::prelude::*;
use tokio::net::UdpSocket;

fn phase(value: RegistrationPhase) -> &'static str {
    match value {
        RegistrationPhase::Registering => "registering",
        RegistrationPhase::Registered => "registered",
        RegistrationPhase::Failed => "failed",
        RegistrationPhase::Stopped => "stopped",
    }
}
fn endpoint(peer: Option<RegistrationPeer>) -> IvyJson {
    match peer {
        None => IvyJson::Null,
        Some(p) => {
            let protocol = match p.transport {
                RegistrationTransport::Udp => "udp",
                RegistrationTransport::Tcp => "tcp",
                RegistrationTransport::Tls => "tls",
            };
            format!("{protocol}:{}", p.address).into()
        }
    }
}
fn input(view: &RegistrationObservation) -> IvyJson {
    IvyJson::object([
        ("state", phase(view.phase).into()),
        (
            "responseCode",
            view.response_code
                .map(|code| n(code as usize))
                .unwrap_or(IvyJson::Null),
        ),
        (
            "remoteRemoved",
            view.remote_removed.map(Into::into).unwrap_or(IvyJson::Null),
        ),
        ("localEndpoint", endpoint(view.local)),
        ("remoteEndpoint", endpoint(view.remote)),
        (
            "grantedExpirySeconds",
            view.granted_expiry_seconds
                .map(|n| n.to_string().into())
                .unwrap_or(IvyJson::Null),
        ),
        (
            "lastKeepAliveUtcTicks",
            view.last_keepalive_utc_ticks
                .map(|n| n.to_string().into())
                .unwrap_or(IvyJson::Null),
        ),
        (
            "lastKeepAliveError",
            match view.last_keepalive_error {
                Some(RegistrationKeepaliveError::SendFailed) => "keepalive_send_failed".into(),
                _ => IvyJson::Null,
            },
        ),
        (
            "socketError",
            match view.last_keepalive_error {
                Some(RegistrationKeepaliveError::SocketError(code)) => code.to_string().into(),
                _ => IvyJson::Null,
            },
        ),
    ])
}
#[test]
fn native_registration_projection_matches_original_dto_without_integer_or_date_loss() {
    let blank = RegistrationLease::default().observation(Duration::ZERO);
    let mut views = vec![blank];
    for state in [
        RegistrationPhase::Registered,
        RegistrationPhase::Failed,
        RegistrationPhase::Stopped,
    ] {
        for protocol in [
            RegistrationTransport::Udp,
            RegistrationTransport::Tcp,
            RegistrationTransport::Tls,
        ] {
            for address in [
                "127.0.0.1:5060",
                "[::1]:5070",
                "[fe80::1%12]:5080",
                "[::ffff:127.0.0.1]:5090",
            ] {
                views.push(RegistrationObservation {
                    phase: state,
                    response_code: Some(200),
                    remote_removed: Some(false),
                    local: Some(RegistrationPeer {
                        address: address.parse().unwrap(),
                        transport: protocol,
                    }),
                    remote: Some(RegistrationPeer {
                        address: "127.0.0.2:5091".parse().unwrap(),
                        transport: protocol,
                    }),
                    granted_expiry_seconds: Some(60),
                    ..blank
                });
            }
        }
    }
    for ms in [
        -62_135_596_800_000,
        -62_135_596_799_999,
        -2_203_891_200_000,
        -2_203_891_199_877,
        -1,
        0,
        1,
        10,
        100,
        110,
        999,
        1000,
        951_782_400_123,
        1_709_164_800_100,
        253_402_300_799_999,
    ] {
        views.push(RegistrationObservation {
            phase: RegistrationPhase::Registered,
            last_keepalive_utc_ticks: Some(ms * 10_000 + 621_355_968_000_000_000),
            ..blank
        });
    }
    for ticks in [
        0,
        1,
        9,
        10,
        99,
        100,
        999,
        1000,
        9999,
        621_355_967_999_999_999,
        621_355_968_000_000_001,
        621_355_968_001_234_567,
        3_155_378_975_999_999_999,
    ] {
        views.push(RegistrationObservation {
            last_keepalive_utc_ticks: Some(ticks),
            ..blank
        });
    }
    // Every named SocketError member plus unrecognized enum values. The
    // original managed enum, not copied expected strings, supplies the oracle.
    for code in [
        0,
        -1,
        995,
        997,
        10004,
        10013,
        10014,
        10022,
        10024,
        10035,
        10036,
        10037,
        10038,
        10039,
        10040,
        10041,
        10042,
        10043,
        10044,
        10045,
        10046,
        10047,
        10048,
        10049,
        10050,
        10051,
        10052,
        10053,
        10054,
        10055,
        10056,
        10057,
        10058,
        10060,
        10061,
        10064,
        10065,
        10067,
        10091,
        10092,
        10093,
        10101,
        10109,
        11001,
        11002,
        11003,
        11004,
        i32::MIN,
        10059,
        i32::MAX,
    ] {
        views.push(RegistrationObservation {
            last_keepalive_error: Some(RegistrationKeepaliveError::SocketError(code)),
            ..blank
        });
    }
    views.push(RegistrationObservation {
        last_keepalive_error: Some(RegistrationKeepaliveError::SendFailed),
        ..blank
    });
    for expiry in [
        0,
        u64::from(u32::MAX) + 17,
        9_007_199_254_740_993,
        i64::MAX as u64,
    ] {
        views.push(RegistrationObservation {
            phase: RegistrationPhase::Registered,
            granted_expiry_seconds: Some(expiry),
            ..blank
        });
    }
    views.push(RegistrationObservation {
        phase: RegistrationPhase::Stopped,
        remote_removed: Some(true),
        ..blank
    });
    let inputs: Vec<_> = views.iter().map(input).collect();
    trace(
        "native-registration-dto-inputs",
        IvyJson::Array(inputs.clone()),
    );
    let original = original_session_fixture(
        "tests/fixtures/phone-native-registration-oracle.ps1",
        &inputs,
    );
    if let Some(v) = &original {
        trace("native-registration-dto-original", v.clone());
    }
    let mut rows = Vec::new();
    for (index, view) in views.iter().enumerate() {
        let raw = registration_json(view).unwrap();
        let parsed = parse(&raw, 65536).unwrap();
        if let Some(expiry) = view.granted_expiry_seconds {
            assert!(raw.contains(&format!("\"grantedExpirySeconds\":{expiry},")));
        }
        if let Some(original) = &original {
            let expected = original.as_array().unwrap()[index].as_str().unwrap();
            assert_eq!(parsed, parse(&expected, 65536).unwrap(), "case {index}");
            if let Some(expiry) = view.granted_expiry_seconds {
                assert!(expected.contains(&format!("\"grantedExpirySeconds\":{expiry},")));
            }
        }
        rows.push(raw.into());
    }
    for invalid in [
        RegistrationObservation {
            last_keepalive_utc_ticks: Some(-1),
            ..blank
        },
        RegistrationObservation {
            last_keepalive_utc_ticks: Some(3_155_378_976_000_000_000),
            ..blank
        },
        RegistrationObservation {
            granted_expiry_seconds: Some(u64::MAX),
            ..blank
        },
        RegistrationObservation {
            last_keepalive_error: Some(RegistrationKeepaliveError::UnmappedSocketError(Some(111))),
            ..blank
        },
    ] {
        assert!(registration_json(&invalid).is_err());
    }
    trace(
        "native-registration-dto",
        IvyJson::object([
            ("inputs", IvyJson::Array(inputs)),
            ("rust", IvyJson::Array(rows)),
            ("original", original.unwrap_or(IvyJson::Null)),
            ("unrepresentableViewsRejected", n(4)),
        ]),
    );
}

#[test]
fn retained_keepalive_errors_ticks_and_late_completions_match_original_owner() {
    let mut lease = RegistrationLease::default();
    let local = RegistrationPeer {
        address: "127.0.0.1:5098".parse().unwrap(),
        transport: RegistrationTransport::Udp,
    };
    let remote = RegistrationPeer {
        address: "127.0.0.1:5099".parse().unwrap(),
        transport: RegistrationTransport::Udp,
    };
    lease.confirmed(Duration::ZERO, 200, 120, local, remote);
    let epoch = 621_355_968_000_000_000;
    let actions = [
        ("observe", epoch, None),
        ("send", epoch + 1_234_567, None),
        ("send", epoch + 2, Some(10061)),
        ("send", epoch + 3, Some(i32::MIN)),
        ("throw", epoch + 4, None),
        ("send", epoch - 1, None),
        ("late-confirm", epoch + 5, None),
        ("late-confirm", epoch + 6, Some(10013)),
        ("send", 3_155_378_975_999_999_999, None),
        ("late-stop", epoch + 7, Some(10022)),
        ("send", epoch + 8, None),
    ];
    let inputs: Vec<_> = actions
        .iter()
        .map(|(op, ticks, code)| {
            let mut fields = vec![("op", (*op).into()), ("ticks", ticks.to_string().into())];
            if let Some(code) = code {
                fields.push(("code", IvyJson::Number(*code as f64)));
            }
            IvyJson::object(fields)
        })
        .collect();
    let original = original_session_fixture(
        "tests/fixtures/phone-registration-keepalive-oracle.ps1",
        &inputs,
    );
    let mut rows = Vec::new();
    let mut sends = 0;
    for (index, (op, ticks, code)) in actions.into_iter().enumerate() {
        if op != "observe" {
            if let Some(token) = lease.keepalive_flow(Duration::ZERO) {
                sends += 1;
                if op == "late-confirm" {
                    lease.confirmed(Duration::ZERO, 200, 120, local, remote);
                }
                if op == "late-stop" {
                    lease.stop();
                }
                let error = if op == "throw" {
                    Some(RegistrationKeepaliveError::SendFailed)
                } else {
                    code.map(RegistrationKeepaliveError::SocketError)
                };
                lease.keepalive_completed(token, ticks, error);
            }
        }
        let view = registration_json(&lease.observation(Duration::ZERO)).unwrap();
        let row = IvyJson::object([("observation", view.clone().into()), ("sends", n(sends))]);
        if let Some(original) = &original {
            let expected = &original.as_array().unwrap()[index];
            assert_eq!(
                parse(&view, 65536).unwrap(),
                parse(
                    expected.get("observation").unwrap().as_str().unwrap(),
                    65536
                )
                .unwrap(),
                "action {index}"
            );
            assert_eq!(row.get("sends"), expected.get("sends"));
        }
        rows.push(row);
    }
    trace(
        "native-registration-keepalive",
        IvyJson::object([
            ("inputs", IvyJson::Array(inputs)),
            ("rust", IvyJson::Array(rows)),
            ("original", original.unwrap_or(IvyJson::Null)),
            ("physicalAudioOpens", n(0)),
            ("networkSocketOpens", n(0)),
        ]),
    );
}

struct Session {
    owner: NativeSipOperations,
    factory: Arc<CountedFactory>,
    routes: Arc<Factory>,
    remaining: Arc<Remaining>,
    handler: Arc<NativeSipCommands>,
    dispatcher: NativeDispatcher,
    peer: UdpSocket,
    local: SocketAddr,
}
impl Session {
    async fn registered() -> Self {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (owner, factory, routes, params) = actual_owner(46000, 46998);
        let registration = format!(
            r#"{{"accountUri":"sip:public-user@account.example.test","registrarUri":"sip:{}","contactUser":"contact-user","authUsername":"digest-user","realm":null,"expirySeconds":60,"retrySeconds":10,"keepAliveIntervalSeconds":0}}"#,
            peer.local_addr().unwrap()
        );
        let params = params
            .replace(
                "\"registration\":null",
                &format!("\"registration\":{registration}"),
            )
            .replace("\"password\":null", "\"password\":\"fixture-only\"");
        let remaining = Arc::new(Remaining::default());
        remaining.close_gate.release();
        let handler = Arc::new(NativeSipCommands::new(
            owner.clone(),
            remaining.clone(),
            |_| Box::pin(async { Ok(()) }),
            subset,
        ));
        let dispatcher = NativeDispatcher::new(CALL.into(), 64, handler.clone()).unwrap();
        let reply = dispatcher
            .execute(command("configure", &params, 1, 1))
            .await;
        let local = value(reply.outcome())
            .get("endpoint")
            .unwrap()
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let (request, source) = received(&peer).await;
        assert_eq!(source, local);
        send(
            &peer,
            source,
            &response(&request, 200, "Expires: 60\r\n", ""),
        )
        .await;
        until(|| {
            owner
                .observation()
                .registration
                .is_some_and(|r| r.phase == RegistrationPhase::Registered)
        })
        .await;
        Self {
            owner,
            factory,
            routes,
            remaining,
            handler,
            dispatcher,
            peer,
            local,
        }
    }
    async fn close(&self) {
        let work = self.handler.shutdown();
        let (remove, source) = received(&self.peer).await;
        assert_eq!(remove.expires_header().unwrap().value(), "0");
        assert_eq!(source, self.local);
        send(&self.peer, source, &response(&remove, 200, "", "")).await;
        work.await.unwrap();
        assert_eq!(self.remaining.closes.load(Ordering::SeqCst), 1);
        assert_eq!(self.routes.opens.load(Ordering::SeqCst), 0);
        drop(UdpSocket::bind(self.local).await.unwrap());
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_reconnect_reserves_before_observation_and_keeps_the_same_binding() {
    let s = Session::registered().await;
    let work = s.owner.reconnect_registration().unwrap();
    assert_eq!(
        s.owner.prepare(CALL.into()).err(),
        Some(SipHostError::OperationConflict)
    );
    assert_eq!(
        s.owner.reconnect_registration().err(),
        Some(SipHostError::OperationConflict)
    );
    drop(work);
    let (remove, source) = received(&s.peer).await;
    assert_eq!(source, s.local);
    assert_eq!(remove.expires_header().unwrap().value(), "0");
    let old_id = remove.call_id_header().unwrap().value().to_string();
    send(&s.peer, source, &response(&remove, 200, "", "")).await;
    let (next, source) = received(&s.peer).await;
    assert_eq!(source, s.local);
    assert_ne!(next.call_id_header().unwrap().value(), old_id);
    send(
        &s.peer,
        source,
        &response(&next, 200, "Expires: 60\r\n", ""),
    )
    .await;
    until(|| {
        s.owner
            .observation()
            .registration
            .is_some_and(|r| r.phase == RegistrationPhase::Registered)
    })
    .await;
    assert_eq!(s.factory.starts.load(Ordering::SeqCst), 0);
    s.close().await;
    trace(
        "native-registration-admission",
        IvyJson::object([
            ("reservedBeforePolling", true.into()),
            ("lostObserverContinued", true.into()),
            ("sameBoundSocket", true.into()),
            ("factoryInvocations", n(0)),
            ("physicalAudioOpens", n(0)),
        ]),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_dispatcher_reconnect_receipt_cannot_repeat_wire_effects_after_live_state_changes() {
    let s = Session::registered().await;
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let prepared = s
        .dispatcher
        .execute(command("call.prepare", &selected, 2, 2))
        .await;
    assert!(prepared.outcome().error_code().is_none());
    assert_eq!(
        s.dispatcher
            .execute(command("registration.reconnect", "{}", 3, 3))
            .await
            .outcome()
            .error_code(),
        Some("operation_conflict")
    );
    assert!(
        s.dispatcher
            .execute(command("call.release", &selected, 4, 4))
            .await
            .outcome()
            .error_code()
            .is_none()
    );
    cancel(
        s.dispatcher
            .execute(command("registration.reconnect", "{}", 5, 5)),
    )
    .await;
    let (remove, source) = received(&s.peer).await;
    assert_eq!(source, s.local);
    assert_eq!(remove.expires_header().unwrap().value(), "0");
    assert!(
        timeout(
            Duration::from_millis(10),
            s.dispatcher
                .execute(command("registration.reconnect", "{}", 5, 6))
        )
        .await
        .is_err()
    );
    assert_eq!(
        s.dispatcher
            .execute(command("registration.reconnect", "{}", 6, 7))
            .await
            .outcome()
            .error_code(),
        Some("operation_conflict")
    );
    assert_eq!(
        s.dispatcher
            .execute(command("call.prepare", &selected, 7, 8))
            .await
            .outcome()
            .error_code(),
        Some("operation_conflict")
    );
    let old_id = remove.call_id_header().unwrap().value().to_string();
    send(&s.peer, source, &response(&remove, 200, "", "")).await;
    let (next, source) = received(&s.peer).await;
    assert_eq!(source, s.local);
    assert_ne!(next.call_id_header().unwrap().value(), old_id);
    let retained = s
        .dispatcher
        .execute(command("registration.reconnect", "{}", 5, 9))
        .await;
    let snapshot = value(retained.outcome());
    assert_eq!(
        snapshot.get("state").unwrap().as_str().unwrap(),
        "registering"
    );
    assert_eq!(snapshot.as_object().unwrap().len(), 8);
    assert!(snapshot.get("localEndpoint").unwrap().is_null());
    send(
        &s.peer,
        source,
        &response(&next, 200, "Expires: 60\r\n", ""),
    )
    .await;
    until(|| {
        s.owner
            .observation()
            .registration
            .is_some_and(|r| r.phase == RegistrationPhase::Registered)
    })
    .await;
    let replay = s
        .dispatcher
        .execute(command("registration.reconnect", "{}", 5, 10))
        .await;
    assert_eq!(retained.outcome(), replay.outcome());
    assert_eq!(replay.request_id(), 10);
    let changed = s
        .dispatcher
        .execute(command(
            "registration.reconnect",
            "{\"changed\":true}",
            5,
            11,
        ))
        .await;
    assert_eq!(changed.outcome().error_code(), Some("operation_conflict"));
    assert!(s.remaining.methods.lock().unwrap().is_empty());
    assert_eq!(s.factory.starts.load(Ordering::SeqCst), 1);
    s.close().await;
    trace(
        "native-registration-receipts",
        IvyJson::object([
            ("savedSnapshot", snapshot),
            ("wireTransitions", n(1)),
            ("sameBoundSocket", true.into()),
            ("replayAfterRegisteredUnchanged", true.into()),
            ("blockedSecondReconnect", true.into()),
            ("blockedPrepareWithoutFactory", true.into()),
            ("callerMethodsForwarded", n(0)),
            ("physicalAudioOpens", n(0)),
        ]),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_reconnect_drains_original_without_starting_another_registration() {
    let s = Session::registered().await;
    cancel(
        s.dispatcher
            .execute(command("registration.reconnect", "{}", 2, 2)),
    )
    .await;
    let (remove, source) = received(&s.peer).await;
    assert_eq!(remove.expires_header().unwrap().value(), "0");
    let work = s.handler.shutdown();
    drop(work);
    assert!(s.owner.observation().stopping);
    assert_eq!(
        s.owner.reconnect_registration().err(),
        Some(SipHostError::Stopping)
    );
    send(&s.peer, source, &response(&remove, 200, "", "")).await;
    s.handler.shutdown().await.unwrap();
    assert_eq!(
        s.dispatcher
            .execute(command("registration.reconnect", "{}", 2, 3))
            .await
            .outcome()
            .error_code(),
        Some("runtime_stopping")
    );
    let mut buffered = 0;
    let mut bytes = [0; 65536];
    while let Ok((count, _)) = s.peer.try_recv_from(&mut bytes) {
        buffered += 1;
        assert!(buffered <= 8);
        let request: rsipstack::sip::Request = std::str::from_utf8(&bytes[..count])
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            request.expires_header().unwrap().value(),
            "0",
            "shutdown cannot start another REGISTER"
        );
        assert_eq!(
            request.call_id_header().unwrap(),
            remove.call_id_header().unwrap()
        );
    }
    assert_eq!(s.remaining.closes.load(Ordering::SeqCst), 1);
    assert_eq!(s.factory.starts.load(Ordering::SeqCst), 0);
    drop(UdpSocket::bind(s.local).await.unwrap());
    trace(
        "native-registration-stop",
        IvyJson::object([
            ("oldRegistrationDrained", true.into()),
            ("noReplacementRegistration", true.into()),
            ("bindingReleased", true.into()),
            ("callerShutdownInvocations", n(1)),
            ("factoryInvocations", n(0)),
        ]),
    );
}
