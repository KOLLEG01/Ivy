use super::*;
use rsipstack::sip::Response;
use std::net::SocketAddr;

async fn receive(peer: &UdpSocket, milliseconds: u64) -> Option<Response> {
    tokio::time::timeout(Duration::from_millis(milliseconds), async {
        let mut bytes = [0u8; 8192];
        loop {
            let (n, _) = peer.recv_from(&mut bytes).await.unwrap();
            let response = Response::try_from(std::str::from_utf8(&bytes[..n]).unwrap()).unwrap();
            if response.status_code.code() != 100 {
                return response;
            }
        }
    })
    .await
    .ok()
}
fn raw(
    method: &str,
    branch: &str,
    wire: &str,
    asserted: &str,
    source: SocketAddr,
    target: SocketAddr,
    spoofed: bool,
) -> String {
    let via = if spoofed {
        format!("127.0.0.2:{}", source.port())
    } else {
        source.to_string()
    };
    format!(
        "{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {via};rport;branch={branch}\r\nFrom: <sip:caller@{source}>;tag=caller-tag\r\nTo: <sip:fixture@{target}>\r\nCall-ID: {wire}\r\nCSeq: 1 {method}\r\nContact: <sip:caller@{source}>\r\nMax-Forwards: 70\r\nP-Asserted-Identity: {asserted}\r\nContent-Length: 0\r\n\r\n"
    )
}
fn wrap(json: String) -> IvyJson {
    IvyJson::object([("json", json.into())])
}
fn outcome(value: &NativeOutcome) -> IvyJson {
    value
        .error_code()
        .map(|e| IvyJson::object([("error", e.into())]))
        .unwrap_or_else(|| wrap(value.result_json().unwrap().into()))
}
fn normalize(value: &mut IvyJson, generated: &str) {
    match value {
        IvyJson::Object(fields) => {
            for (key, value) in fields {
                if key == &"json".into() {
                    let mut decoded = parse(&value.as_str().unwrap(), 65536).unwrap();
                    normalize(&mut decoded, generated);
                    *value = stringify(&decoded, 65536).unwrap().into();
                } else if key == &"id".into() && value.as_str().is_ok_and(|s| s == generated) {
                    assert_eq!(generated.len(), 36);
                    assert!(generated.bytes().enumerate().all(|(i, b)| {
                        if [8, 13, 18, 23].contains(&i) {
                            b == b'-'
                        } else {
                            b.is_ascii_hexdigit()
                        }
                    }));
                    assert_eq!(generated.as_bytes()[14], b'4');
                    assert!(b"89ab".contains(&generated.as_bytes()[19]));
                    *value = "original-incoming-id".into();
                } else if key == &"peerPort".into() {
                    assert!((1.0..=65535.0).contains(&value.as_f64().unwrap()));
                    *value = "owned-source-port".into();
                } else if key == &"fromUri".into() {
                    let uri = value.as_str().unwrap();
                    assert!(uri.starts_with("sip:caller@127.0.0.1:"));
                    assert_ne!(
                        uri.strip_prefix("sip:caller@127.0.0.1:")
                            .unwrap()
                            .parse::<u16>()
                            .unwrap(),
                        0
                    );
                    *value = "sip:caller@127.0.0.1:owned-source-port".into();
                } else {
                    normalize(value, generated)
                }
            }
        }
        IvyJson::Array(values) => {
            for value in values {
                normalize(value, generated)
            }
        }
        _ => (),
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_incoming_admission_claim_cancel_expiry_and_source_identity_use_one_owner() {
    for scenario in [
        "cancel_unclaimed",
        "cancel_claimed",
        "hangup_claimed",
        "expire_unclaimed",
        "expire_claimed",
        "duplicate",
        "denied",
        "busy",
        "invalid_context",
        "spoofed",
    ] {
        let (owner, factory, routes, params) = actual_owner(44000, 44998);
        let config = params.replace(
            "\"transport\":\"udp\"",
            "\"transport\":\"udp\",\"incomingRingSeconds\":1",
        );
        let config = if scenario == "denied" {
            config.replace("[\"registered\",\"127.0.0.1\"]", "[]")
        } else if scenario == "spoofed" {
            config.replace("[\"registered\",\"127.0.0.1\"]", "[\"127.0.0.2\"]")
        } else {
            config
        };
        let wire = if scenario == "invalid_context" {
            "x".repeat(1025)
        } else {
            "original-wire-call".into()
        };
        let asserted = "<sip:+49123456789@identity.example.test>, <sip:+11234567890@identity.example.test>, <sip:+49123456789@identity.example.test>, <sip:123+19876543210@identity.example.test>, <sip:+0123456789@identity.example.test>, <sip:+1234567890123456@identity.example.test>";
        let input = IvyJson::object([
            ("scenario", scenario.into()),
            ("wireId", wire.clone().into()),
            ("asserted", asserted.into()),
            ("configuration", parse(&config, 65536).unwrap()),
        ]);
        let steps = vec![("fixture.incoming".into(), stringify(&input, 65536).unwrap())];
        let original = super::dial::original_raw(&steps);
        let remaining = Arc::new(Remaining::default());
        remaining.close_gate.release();
        let handler = Arc::new(NativeSipCommands::new(
            owner.clone(),
            remaining.clone(),
            |_| Box::pin(async { Ok(()) }),
            closed_platform_views(),
        ));
        let dispatcher = NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap();
        assert!(
            dispatcher
                .execute(command("configure", &config, 1, 1))
                .await
                .outcome()
                .error_code()
                .is_none()
        );
        if scenario == "busy" {
            assert!(
                dispatcher
                    .execute(command(
                        "call.prepare",
                        &format!(r#"{{"callId":"{CALL}"}}"#),
                        2,
                        2
                    ))
                    .await
                    .outcome()
                    .error_code()
                    .is_none()
            );
        }
        let target = owner.observation().endpoint.unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let source = peer.local_addr().unwrap();
        let invite = raw(
            "INVITE",
            "z9hG4bK-owned-incoming",
            &wire,
            asserted,
            source,
            target,
            scenario == "spoofed",
        );
        peer.send_to(invite.as_bytes(), target).await.unwrap();
        let code = receive(&peer, 4000)
            .await
            .expect("Original incoming response")
            .status_code
            .code();
        let mut generated = String::new();
        let mut saved_claim = None;
        let result = if code != 180 {
            assert_eq!(
                code,
                if scenario == "busy" { 486 } else { 403 },
                "{scenario}"
            );
            let retained = owner.observation().calls.len();
            let existing = if scenario == "busy" {
                parse(
                    &ivy_phone_bridge::native_sip_status::call_json(&owner.find(CALL).unwrap())
                        .unwrap(),
                    65536,
                )
                .unwrap()
            } else {
                IvyJson::Null
            };
            if scenario == "busy" {
                owner
                    .release(&owner.find(CALL).unwrap())
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
            }
            handler.shutdown().await.unwrap();
            IvyJson::object([
                ("code", IvyJson::Number(code.into())),
                ("call", IvyJson::Null),
                ("existing", existing),
                ("retained", n(retained)),
                ("closed", true.into()),
            ])
        } else {
            until(|| !owner.observation().calls.is_empty()).await;
            let call = owner.find(&owner.observation().calls[0].id).unwrap();
            call.prepared().await.unwrap();
            generated = call.observation().id;
            assert_ne!(generated, wire);
            let media = call.media().unwrap();
            let original_rtp = media.observation().receive.transport.local_rtp;
            let admitted = parse(
                &ivy_phone_bridge::native_sip_status::call_json(&call).unwrap(),
                65536,
            )
            .unwrap();
            let selected = format!(r#"{{"callId":"{generated}"}}"#);
            let mut claimed = scenario.contains("claimed") && !scenario.contains("unclaimed");
            let mut duplicate_ignored = false;
            if scenario == "duplicate" {
                peer.send_to(invite.as_bytes(), target).await.unwrap();
                assert_eq!(receive(&peer, 4000).await.unwrap().status_code.code(), 180);
                let duplicate = raw(
                    "INVITE",
                    "z9hG4bK-duplicate-wire",
                    &wire,
                    asserted,
                    source,
                    target,
                    false,
                );
                peer.send_to(duplicate.as_bytes(), target).await.unwrap();
                duplicate_ignored = receive(&peer, 150).await.is_none();
                assert!(duplicate_ignored);
                assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
                claimed = true;
            }
            let mut claim = IvyJson::Null;
            let mut features = IvyJson::Null;
            let mut hungup = IvyJson::Null;
            if claimed {
                let reply = dispatcher
                    .execute(command("call.claim", &selected, 10, 10))
                    .await;
                saved_claim = Some(reply.outcome().result_json().unwrap().to_owned());
                claim = outcome(reply.outcome());
                features = outcome(
                    dispatcher
                        .execute(command(
                            "call.features",
                            &format!(r#"{{"callId":"{generated}","challenge":null}}"#),
                            11,
                            11,
                        ))
                        .await
                        .outcome(),
                );
            }
            let mut statuses = Vec::new();
            if scenario.starts_with("cancel") {
                let cancel = raw(
                    "CANCEL",
                    "z9hG4bK-owned-incoming",
                    &wire,
                    asserted,
                    source,
                    target,
                    false,
                );
                peer.send_to(cancel.as_bytes(), target).await.unwrap();
                statuses.push(receive(&peer, 4000).await.unwrap().status_code.code());
                statuses.push(receive(&peer, 4000).await.unwrap().status_code.code());
                statuses.sort();
            } else if scenario.starts_with("expire") {
                statuses.push(receive(&peer, 4000).await.unwrap().status_code.code());
            } else {
                hungup = outcome(
                    dispatcher
                        .execute(command("call.hangup", &selected, 12, 12))
                        .await
                        .outcome(),
                );
                statuses.push(receive(&peer, 4000).await.unwrap().status_code.code());
            }
            until(|| {
                call.observation().phase == ivy_phone_bridge::sip::SipHostCallPhase::LocalEnded
                    && media.observation().closed
            })
            .await;
            let ended = parse(
                &ivy_phone_bridge::native_sip_status::call_json(&call).unwrap(),
                65536,
            )
            .unwrap();
            if !claimed {
                until(|| owner.find(&generated).is_none()).await;
            }
            let retained = owner.observation().calls.len();
            let expired = outcome(
                dispatcher
                    .execute(command("call.claim", &selected, 13, 13))
                    .await
                    .outcome(),
            );
            if claimed {
                owner.release(&call).unwrap().wait().await.unwrap();
            }
            if let Some(saved) = &saved_claim {
                assert_eq!(
                    dispatcher
                        .execute(command("call.claim", &selected, 10, 100))
                        .await
                        .outcome()
                        .result_json(),
                    Some(saved.as_str())
                );
            }
            let released = owner.find(&generated).is_none();
            handler.shutdown().await.unwrap();
            assert!(media.observation().closed);
            assert_eq!(
                media.observation().receive.transport.local_rtp,
                original_rtp
            );
            IvyJson::object([
                ("code", IvyJson::Number(code.into())),
                ("call", admitted),
                ("claim", claim),
                ("features", features),
                ("hungup", hungup),
                ("ended", ended),
                (
                    "statuses",
                    IvyJson::Array(
                        statuses
                            .iter()
                            .map(|v| IvyJson::Number((*v).into()))
                            .collect(),
                    ),
                ),
                ("retained", n(retained)),
                ("expired", expired),
                ("released", released.into()),
                ("duplicateIgnored", duplicate_ignored.into()),
                ("closed", true.into()),
            ])
        };
        drop(
            UdpSocket::bind(target)
                .await
                .expect("Original incoming SIP socket drain"),
        );
        assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
        assert!(remaining.methods.lock().unwrap().is_empty());
        let original_result = original.as_ref().map(|v| {
            parse(
                &v.as_array().unwrap()[0]
                    .get("json")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                65536,
            )
            .unwrap()
        });
        let mut actual = result.clone();
        normalize(&mut actual, &generated);
        let mut expected = original_result.clone();
        if let Some(value) = &mut expected {
            let id = value
                .get("call")
                .and_then(|c| c.get("id"))
                .and_then(|id| id.as_str().ok())
                .unwrap_or_default();
            normalize(value, &id);
        }
        trace(
            &format!("native-incoming-{scenario}"),
            IvyJson::object([
                ("rust", result),
                ("original", original_result.unwrap_or(IvyJson::Null)),
                ("normalizedRust", actual.clone()),
                (
                    "normalizedOriginal",
                    expected.clone().unwrap_or(IvyJson::Null),
                ),
                ("physicalAudioOpens", n(0)),
                ("originalBindingReleased", true.into()),
            ]),
        );
        if let Some(expected) = expected {
            assert_eq!(actual, expected, "{scenario}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_claim_preserves_original_dto_lookup_and_outgoing_refusal_order() {
    let (owner, _, _, params) = actual_owner(43000, 43998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner,
        remaining,
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap();
    let id = format!(r#"{{"callId":"{CALL}"}}"#);
    let mut steps = vec![
        ("call.claim".into(), "{}".into()),
        ("call.claim".into(), r#"{"callId":false}"#.into()),
        ("configure".into(), params),
    ];
    for params in [
        "{}",
        r#"{"callId":null}"#,
        r#"{"callId":42}"#,
        r#"{"callId":false}"#,
        r#"{"callId":"missing"}"#,
        r#"{"callId":""}"#,
        r#"{"callId":"missing","extra":true}"#,
    ] {
        steps.push(("call.claim".into(), params.into()));
    }
    steps.extend([
        ("call.prepare".into(), id.clone()),
        ("call.claim".into(), id.clone()),
        ("call.hangup".into(), id.clone()),
        ("call.claim".into(), id.clone()),
        ("call.release".into(), id.clone()),
        ("call.claim".into(), id),
    ]);
    let original = super::dial::original_raw(&steps);
    let mut rows = Vec::new();
    for (i, (method, params)) in steps.iter().enumerate() {
        rows.push(outcome(
            dispatcher
                .execute(command(method, params, i as u64 + 1, i as u64 + 1))
                .await
                .outcome(),
        ));
    }
    trace(
        "native-claim-validation",
        IvyJson::object([
            ("rust", IvyJson::Array(rows.clone())),
            ("original", original.clone().unwrap_or(IvyJson::Null)),
        ]),
    );
    super::dial::compare(&rows, &original, &steps);
    handler.shutdown().await.unwrap();
}
