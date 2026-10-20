use super::*;
use rsipstack::sip::{Header, Method, prelude::*};

fn normalize(value: &mut IvyJson) {
    match value {
        IvyJson::Object(fields) => {
            for (key, value) in fields {
                if key == &"json".into() {
                    let mut decoded = parse(&value.as_str().unwrap(), 65536).unwrap();
                    normalize(&mut decoded);
                    *value = stringify(&decoded, 65536).unwrap().into();
                } else if key == &"callId".into() {
                    assert_eq!(value.as_str().unwrap().len(), 36);
                    *value = "original-call".into();
                } else {
                    normalize(value);
                }
            }
        }
        IvyJson::Array(values) => {
            for value in values {
                normalize(value)
            }
        }
        _ => (),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_codec_upgrade_original_lookup_and_prepared_ending_guards() {
    let (owner, _, routes, config) = actual_owner(42000, 42998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        remaining.clone(),
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap();
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let mut steps = vec![
        ("call.codec.upgrade".into(), r#"{"callId":42}"#.into()),
        ("configure".into(), config),
    ];
    for params in [
        "{}",
        r#"{"callId":null}"#,
        r#"{"callId":42}"#,
        r#"{"callId":false}"#,
        r#"{"callId":"missing"}"#,
        r#"{"callId":"missing","extra":0}"#,
        r#"{"CallId":"missing"}"#,
    ] {
        steps.push(("call.codec.upgrade".into(), params.into()));
    }
    steps.push(("call.prepare".into(), selected.clone()));
    for params in [
        selected.clone(),
        format!(r#"{{"callId":"{CALL}","extra":null}}"#),
        r#"{"callId":null}"#.into(),
    ] {
        steps.push(("call.codec.upgrade".into(), params));
    }
    steps.extend([
        ("call.hangup".into(), selected.clone()),
        ("call.codec.upgrade".into(), selected.clone()),
        ("call.release".into(), selected.clone()),
        ("call.codec.upgrade".into(), selected),
    ]);
    let original = super::dial::original_raw(&steps);
    let mut rows = vec![];
    for (i, (method, params)) in steps.iter().enumerate() {
        rows.push(super::incoming::outcome(
            dispatcher
                .execute(command(method, params, i as u64 + 1, i as u64 + 1))
                .await
                .outcome(),
        ));
    }
    trace(
        "native-upgrade-validation",
        IvyJson::object([
            ("rust", IvyJson::Array(rows.clone())),
            ("original", original.clone().unwrap_or(IvyJson::Null)),
            ("steps", n(steps.len())),
        ]),
    );
    super::dial::compare(&rows, &original, &steps);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    assert!(remaining.methods.lock().unwrap().is_empty());
    handler.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_codec_upgrade_retains_original_dialog_media_transmission_and_receipts() {
    for mode in ["initial", "delayed"] {
        for kind in [
            "upgraded",
            "fallback",
            "refused",
            "disabled",
            "unauthenticated",
            "trusted",
        ] {
            let scenario = format!("{mode}_{kind}");
            let delayed = mode == "delayed";
            let (owner, factory, routes, config) = actual_owner(40000, 41998);
            let config = config.replace(r#"["PCMA"]"#, r#"["PCMU","PCMA"]"#);
            let fixture = IvyJson::object([
                ("scenario", scenario.clone().into()),
                ("configuration", parse(&config, 65536).unwrap()),
            ]);
            let original = super::dial::original_raw(&[(
                "fixture.upgrade".into(),
                stringify(&fixture, 65536).unwrap(),
            )]);
            trace(
                &format!("native-upgrade-original-{scenario}"),
                original.clone().unwrap_or(IvyJson::Null),
            );
            let remaining = Arc::new(Remaining::default());
            remaining.close_gate.release();
            let handler = Arc::new(NativeSipCommands::new(
                owner.clone(),
                remaining.clone(),
                |_| Box::pin(async { Ok(()) }),
                closed_platform_views(),
            ));
            let dispatcher =
                Arc::new(NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap());
            assert!(
                dispatcher
                    .execute(command("configure", &config, 1, 1))
                    .await
                    .outcome()
                    .error_code()
                    .is_none()
            );
            let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let (rtp, _rtcp) = pair().await;
            let target = owner.observation().endpoint.unwrap();
            let sdp = |codec: &str, payload: u8| {
                format!(
                    "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP {payload} 101\r\na=rtpmap:{payload} {codec}/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n",
                    rtp.local_addr().unwrap().port()
                )
            };
            super::answer::send(
                &peer,
                target,
                "INVITE",
                1,
                "z9hG4bK-owned-upgrade",
                "",
                if delayed { "" } else { &sdp("PCMA", 8) },
            )
            .await;
            assert_eq!(
                super::incoming::receive(&peer, 4000)
                    .await
                    .unwrap()
                    .status_code
                    .code(),
                180
            );
            until(|| !owner.observation().calls.is_empty()).await;
            let id = owner.observation().calls[0].id.clone();
            let call = owner.find(&id).unwrap();
            call.prepared().await.unwrap();
            let media = call.media().unwrap();
            let original_rtp = media.observation().receive.transport.local_rtp;
            let selected = format!(r#"{{"callId":"{id}"}}"#);
            let challenge = if kind == "trusted" {
                "null"
            } else {
                r#"{"code":"*123#","timeoutSeconds":30,"maxFailedAttempts":3,"failedAttemptWindowSeconds":60}"#
            };
            for (method, params, op) in [
                ("call.claim", selected.clone(), 2),
                (
                    "call.features",
                    format!(r#"{{"callId":"{id}","challenge":{challenge}}}"#),
                    3,
                ),
            ] {
                assert!(
                    dispatcher
                        .execute(command(method, &params, op, op))
                        .await
                        .outcome()
                        .error_code()
                        .is_none()
                );
            }
            let answer = format!(r#"{{"callId":"{id}","waiting":false}}"#);
            let dispatch = dispatcher.clone();
            let answered = tokio::spawn(async move {
                dispatch
                    .execute(command("call.answer", &answer, 4, 4))
                    .await
            });
            let final_response = super::incoming::receive(&peer, 4000).await.unwrap();
            assert_eq!(final_response.status_code.code(), 200);
            let tag = final_response
                .to_header()
                .unwrap()
                .tag()
                .unwrap()
                .unwrap()
                .value()
                .to_owned();
            super::answer::send(
                &peer,
                target,
                "ACK",
                1,
                "z9hG4bK-owned-upgrade-ack",
                &tag,
                if delayed { &sdp("PCMA", 8) } else { "" },
            )
            .await;
            assert!(answered.await.unwrap().outcome().error_code().is_none());
            if kind != "unauthenticated" && kind != "trusted" {
                for digit in if kind == "disabled" {
                    "**123#"
                } else {
                    "*123#"
                }
                .chars()
                {
                    call.features().digit(digit, InputSource::SipInfo).unwrap();
                }
            }
            let before = parse(&call.features().observation().json(), 65536).unwrap();
            let request = command("call.codec.upgrade", &selected, 5, 5);
            let dispatch = dispatcher.clone();
            let mut pending = Some(tokio::spawn(async move { dispatch.execute(request).await }));
            let mut requests = vec![];
            if !["disabled", "unauthenticated", "trusted"].contains(&kind) {
                let (invite, remote) = crate::sip_host::received(&peer).await;
                assert_eq!(invite.method, Method::Invite);
                let body = String::from_utf8(invite.body.clone()).unwrap();
                requests.push(IvyJson::object([
                    ("method", "INVITE".into()),
                    (
                        "sequence",
                        n(invite.cseq_header().unwrap().seq().unwrap() as usize),
                    ),
                    (
                        "sameCallId",
                        (invite.call_id_header().unwrap().value() == "owned-native-answer").into(),
                    ),
                    (
                        "sameTags",
                        (invite
                            .from_header()
                            .unwrap()
                            .tag()
                            .unwrap()
                            .unwrap()
                            .value()
                            == tag
                            && invite.to_header().unwrap().tag().unwrap().unwrap().value()
                                == "caller-tag")
                            .into(),
                    ),
                    ("hasPcma", body.contains("PCMA/8000").into()),
                    ("hasPcmu", body.contains("PCMU/8000").into()),
                    ("hasSendrecv", body.contains("sendrecv").into()),
                ]));
                // The retained receipt/driver survives abandonment of its first IPC waiter.
                pending.take().unwrap().abort();
                let response = crate::sip_host::response(
                    &invite,
                    if kind == "refused" { 488 } else { 200 },
                    &format!(
                        "Content-Type: application/sdp\r\nContact: <sip:caller@{}>\r\n",
                        peer.local_addr().unwrap()
                    ),
                    if kind == "refused" {
                        ""
                    } else {
                        &sdp(
                            if kind == "fallback" { "PCMA" } else { "PCMU" },
                            if kind == "fallback" { 8 } else { 0 },
                        )
                    },
                );
                let mut response = response;
                response
                    .headers
                    .unique_push(invite.to_header().unwrap().clone().into());
                crate::sip_host::send(&peer, remote, &response).await;
                let (ack, _) = crate::sip_host::received(&peer).await;
                assert_eq!(ack.method, Method::Ack);
                requests.push(IvyJson::object([
                    ("method", "ACK".into()),
                    (
                        "sequence",
                        n(ack.cseq_header().unwrap().seq().unwrap() as usize),
                    ),
                    (
                        "sameCallId",
                        (ack.call_id_header().unwrap().value() == "owned-native-answer").into(),
                    ),
                ]));
            }
            if let Some(pending) = pending {
                pending.await.unwrap();
            }
            let first = super::feedback::row(&dispatcher, "call.codec.upgrade", &selected, 5).await;
            let mut rows = vec![first.clone()];
            assert!(
                dispatcher
                    .execute(command("call.hangup", &selected, 6, 6))
                    .await
                    .outcome()
                    .error_code()
                    .is_none()
            );
            let (bye, remote) = crate::sip_host::received(&peer).await;
            assert_eq!(bye.method, Method::Bye);
            crate::sip_host::send(&peer, remote, &crate::sip_host::response(&bye, 200, "", ""))
                .await;
            rows.push(super::feedback::row(&dispatcher, "call.codec.upgrade", &selected, 7).await);
            owner.release(&call).unwrap().wait().await.unwrap();
            let replacement = owner.prepare(id.clone()).unwrap();
            replacement.prepared().await.unwrap();
            assert_eq!(
                super::feedback::row(&dispatcher, "call.codec.upgrade", &selected, 5).await,
                first
            );
            assert!(!replacement.observation().attempted);
            assert!(!replacement.observation().ending);
            owner.release(&replacement).unwrap().wait().await.unwrap();
            handler.shutdown().await.unwrap();
            assert!(media.observation().closed);
            assert!(media.observation().cleanup_confirmed);
            assert_eq!(
                media.observation().receive.transport.local_rtp,
                original_rtp
            );
            assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
            assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
            assert!(remaining.methods.lock().unwrap().is_empty());
            let mut result = IvyJson::object([
                ("rows", IvyJson::Array(rows)),
                ("requests", IvyJson::Array(requests)),
                ("before", before),
                ("sameMedia", true.into()),
                ("release", IvyJson::object([("released", true.into())])),
            ]);
            trace(
                &format!("native-upgrade-{scenario}"),
                IvyJson::object([
                    ("rust", result.clone()),
                    ("original", original.clone().unwrap_or(IvyJson::Null)),
                ]),
            );
            if let Some(original) = original {
                let mut expected = parse(
                    &original.as_array().unwrap()[0]
                        .get("json")
                        .unwrap_or_else(|| panic!("Original upgrade unavailable: {original:?}"))
                        .as_str()
                        .unwrap(),
                    65536,
                )
                .unwrap();
                normalize(&mut result);
                normalize(&mut expected);
                assert_eq!(result, expected, "{scenario}");
            }
            drop(
                UdpSocket::bind(target)
                    .await
                    .expect("Original upgrade binding released"),
            );
        }
    }
}
