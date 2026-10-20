use super::*;
use rsipstack::sip::{Method, Request, Response, prelude::*};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pending_host_shutdown_retains_original_binding_media_and_completion() {
    use ivy_phone_bridge::sip::SipHostError;
    for preferred in ["PCMA", "PCMU"] {
        for mode in ["initial", "delayed"] {
            for pre_released in [false, true] {
                let control = format!(
                    "{mode}_{preferred}_shutdown_{}",
                    if pre_released { "released" } else { "pending" }
                );
                let (_, _, _, configuration) = actual_owner(43000, 43998);
                let configuration = configuration.replace(
                    r#"["PCMA","PCMU"]"#,
                    if preferred == "PCMA" {
                        r#"["PCMA","PCMU"]"#
                    } else {
                        r#"["PCMU","PCMA"]"#
                    },
                );
                let fixture = IvyJson::object([
                    ("scenario", control.clone().into()),
                    ("configuration", parse(&configuration, 65536).unwrap()),
                ]);
                let original = super::dial::original_raw(&[(
                    "fixture.upgrade".into(),
                    stringify(&fixture, 65536).unwrap(),
                )]);
                trace(
                    &format!("pending-shutdown-original-{control}"),
                    original.clone().unwrap_or(IvyJson::Null),
                );
                for outcome in ["accepted", "refused", "unconfirmed"] {
                    if outcome == "unconfirmed" && preferred != "PCMU" {
                        continue;
                    }
                    let scenario = format!("{control}_{outcome}");
                    let (owner, factory, routes, _) = actual_owner(43000, 43998);
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
                            .execute(command("configure", &configuration, 1, 1))
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
                    let initial = sdp("PCMA", 8);
                    super::answer::send(
                        &peer,
                        target,
                        "INVITE",
                        1,
                        "z9hG4bK-owned-upgrade",
                        "",
                        if mode == "delayed" { "" } else { &initial },
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
                    let original_transport = media.observation().receive.transport;
                    let selected = format!(r#"{{"callId":"{id}"}}"#);
                    for (method, params, op) in [
                        ("call.claim", selected.clone(), 2),
                        (
                            "call.features",
                            format!(
                                r#"{{"callId":"{id}","challenge":{{"code":"*123#","timeoutSeconds":30,"maxFailedAttempts":3,"failedAttemptWindowSeconds":60}}}}"#
                            ),
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
                        if mode == "delayed" { &initial } else { "" },
                    )
                    .await;
                    assert!(answered.await.unwrap().outcome().error_code().is_none());
                    for digit in "*123#".chars() {
                        call.features().digit(digit, InputSource::SipInfo).unwrap();
                    }
                    let before = parse(&call.features().observation().json(), 65536).unwrap();
                    let dispatch = dispatcher.clone();
                    let params = selected.clone();
                    let upgrade = tokio::spawn(async move {
                        dispatch
                            .execute(command("call.codec.upgrade", &params, 5, 5))
                            .await
                    });
                    let (invite, remote) = next(&peer, Method::Invite).await;
                    let mut requests = vec![invite_row(&invite, &tag)];
                    upgrade.abort();
                    let _ = upgrade.await;
                    let mut replacement = None;
                    let mut bye_sequence = None;
                    if pre_released {
                        bye_sequence = Some(end(&dispatcher, &selected, &peer).await);
                        tokio::time::timeout(
                            Duration::from_secs(2),
                            owner.release(&call).unwrap().wait(),
                        )
                        .await
                        .unwrap()
                        .unwrap();
                        let fresh = owner.prepare(id.clone()).unwrap();
                        fresh.prepared().await.unwrap();
                        assert!(!fresh.observation().attempted && !fresh.observation().ending);
                        assert!(media.observation().closed);
                        assert!(!fresh.media().unwrap().observation().closed);
                        replacement = Some(fresh);
                    }
                    let started = tokio::time::Instant::now();
                    // This polls then abandons an observer of the real retained
                    // NativeSipCommands shutdown, not a synthetic close result.
                    cancel(handler.shutdown()).await;
                    let close = owner.close();
                    if !pre_released {
                        let (bye, source) = next(&peer, Method::Bye).await;
                        bye_sequence = Some(bye.cseq_header().unwrap().seq().unwrap());
                        crate::sip_host::send(
                            &peer,
                            source,
                            &crate::sip_host::response(&bye, 200, "", ""),
                        )
                        .await;
                    }
                    until(|| media.observation().cleanup_confirmed).await;
                    assert!(
                        media.observation().closed && media.observation().send_format.is_none()
                    );
                    if let Some(fresh) = &replacement {
                        until(|| fresh.media().unwrap().observation().cleanup_confirmed).await;
                    }
                    for address in [original_transport.local_rtp, original_transport.local_rtcp] {
                        drop(
                            UdpSocket::bind(address)
                                .await
                                .expect("Original media socket did not retire"),
                        );
                    }
                    // A bounded observer timeout never retires the original
                    // pending INVITE, clears the binding, or runs other cleanup.
                    assert!(
                        tokio::time::timeout(Duration::from_millis(100), close.clone().wait())
                            .await
                            .is_err()
                    );
                    assert!(close.result().is_none());
                    assert_eq!(remaining.closes.load(Ordering::SeqCst), 0);
                    assert!(UdpSocket::bind(target).await.is_err());
                    assert!(matches!(
                        owner.prepare(id.clone()),
                        Err(SipHostError::Stopping)
                    ));
                    assert_eq!(
                        dispatcher
                            .execute(command("call.prepare", &selected, 9, 9))
                            .await
                            .outcome()
                            .error_code(),
                        Some("runtime_stopping")
                    );
                    let sent = media.observation().sent_frames;
                    let mut finals = vec![];
                    if outcome != "unconfirmed" {
                        let body = if outcome == "accepted" {
                            sdp("PCMU", 0)
                        } else {
                            String::new()
                        };
                        let mut response = crate::sip_host::response(
                            &invite,
                            if outcome == "accepted" { 200 } else { 488 },
                            &format!(
                                "{}Contact: <sip:caller@{}>\r\n",
                                if outcome == "accepted" {
                                    "Content-Type: application/sdp\r\n"
                                } else {
                                    ""
                                },
                                peer.local_addr().unwrap()
                            ),
                            &body,
                        );
                        response
                            .headers
                            .unique_push(invite.to_header().unwrap().clone().into());
                        finals.push(final_row(&response, rtp.local_addr().unwrap().port()));
                        crate::sip_host::send(&peer, remote, &response).await;
                        let (ack, _) = next(&peer, Method::Ack).await;
                        requests.push(ack_row(&ack));
                    }
                    let result =
                        tokio::time::timeout(Duration::from_secs(40), close.clone().wait())
                            .await
                            .expect("Per-call close did not produce a bounded outcome");
                    let expected = if outcome == "unconfirmed" {
                        Err(SipHostError::CleanupUnconfirmed)
                    } else {
                        Ok(())
                    };
                    assert_eq!(result, expected, "{scenario}");
                    assert_eq!(owner.close().wait().await, expected);
                    assert_eq!(
                        handler.shutdown().await,
                        if outcome == "unconfirmed" {
                            Err(NativeShutdownError::OutcomeUnknown)
                        } else {
                            Ok(())
                        }
                    );
                    assert_eq!(
                        handler.shutdown().await,
                        if outcome == "unconfirmed" {
                            Err(NativeShutdownError::OutcomeUnknown)
                        } else {
                            Ok(())
                        }
                    );
                    assert_eq!(remaining.closes.load(Ordering::SeqCst), 1);
                    assert!(remaining.methods.lock().unwrap().is_empty());
                    assert_eq!(owner.observation().closed, outcome != "unconfirmed");
                    assert!(owner.observation().stopping);
                    assert_eq!(media.observation().sent_frames, sent);
                    assert!(
                        media.observation().closed
                            && media.observation().cleanup_confirmed
                            && media.observation().send_format.is_none()
                    );
                    if let Some(fresh) = &replacement {
                        assert!(
                            fresh.media().unwrap().observation().closed
                                && fresh.media().unwrap().observation().cleanup_confirmed
                        );
                    }
                    let row =
                        super::feedback::row(&dispatcher, "call.codec.upgrade", &selected, 5).await;
                    assert_eq!(
                        parse(
                            &row.get("output")
                                .unwrap()
                                .get("json")
                                .unwrap()
                                .as_str()
                                .unwrap(),
                            65536
                        )
                        .unwrap(),
                        IvyJson::object([
                            ("state", "fallback".into()),
                            ("codec", IvyJson::Null),
                            ("payload", IvyJson::Null)
                        ])
                    );
                    assert_eq!(
                        super::feedback::row(&dispatcher, "call.codec.upgrade", &selected, 5).await,
                        row
                    );
                    let mut shared = IvyJson::object([
                        ("rows", IvyJson::Array(vec![row])),
                        ("before", before),
                        ("sameMedia", true.into()),
                        ("preReleased", pre_released.into()),
                        ("replacementDistinct", replacement.is_some().into()),
                        ("sendCodecAfterShutdown", IvyJson::Null),
                        ("byeSequence", n(bye_sequence.unwrap() as usize)),
                    ]);
                    normalize(&mut shared);
                    let original_shared = if outcome == "accepted" {
                        original.as_ref().map(|raw| {
                            let observed = parse(
                                &raw.as_array().unwrap()[0]
                                    .get("json")
                                    .expect("Original shutdown control unavailable")
                                    .as_str()
                                    .unwrap(),
                                65536,
                            )
                            .unwrap();
                            assert!(
                                observed
                                    .get("shutdownBeforeFinal")
                                    .unwrap()
                                    .as_bool()
                                    .unwrap()
                            );
                            assert!(observed.get("bindingReleased").unwrap().as_bool().unwrap());
                            assert!(
                                observed
                                    .get("shutdown")
                                    .unwrap()
                                    .get("closed")
                                    .unwrap()
                                    .as_bool()
                                    .unwrap()
                            );
                            // Socket failure is unavailable late-ACK evidence,
                            // never normalized into a passing ACK comparison.
                            assert!(!observed.get("ackObserved").unwrap().as_bool().unwrap());
                            assert!(!observed.get("ackError").unwrap().is_null());
                            assert_eq!(
                                observed.get("requests").unwrap(),
                                &IvyJson::Array(vec![requests[0].clone()])
                            );
                            assert_eq!(
                                observed.get("finals").unwrap(),
                                &IvyJson::Array(finals.clone())
                            );
                            let mut comparison = observed.get("shared").unwrap().clone();
                            normalize(&mut comparison);
                            assert_eq!(shared, comparison, "{scenario}");
                            comparison
                        })
                    } else {
                        None
                    };
                    assert_eq!(
                        factory.starts.load(Ordering::SeqCst),
                        if pre_released { 2 } else { 1 }
                    );
                    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
                    let host_released = match UdpSocket::bind(target).await {
                        Ok(socket) => {
                            drop(socket);
                            true
                        }
                        Err(_) => false,
                    };
                    assert_eq!(host_released, outcome != "unconfirmed");
                    drop(replacement);
                    drop(call);
                    drop(media);
                    drop(dispatcher);
                    drop(handler);
                    drop(owner);
                    let after_drop_released = match UdpSocket::bind(target).await {
                        Ok(socket) => {
                            drop(socket);
                            true
                        }
                        Err(_) => false,
                    };
                    assert_eq!(
                        after_drop_released,
                        outcome != "unconfirmed",
                        "Last-handle drop must not clear an unconfirmed fence"
                    );
                    trace(
                        &format!("pending-shutdown-{scenario}"),
                        IvyJson::object([
                            ("sharedRust", shared),
                            ("sharedOriginal", original_shared.unwrap_or(IvyJson::Null)),
                            ("outcome", outcome.into()),
                            ("requests", IvyJson::Array(requests)),
                            ("finals", IvyJson::Array(finals)),
                            ("portHeldBeforeFinal", true.into()),
                            ("mediaRetiredBeforeFinal", true.into()),
                            ("abandonedCloseObserver", true.into()),
                            ("oldReceiptReplayed", true.into()),
                            ("lateMediaResurrected", false.into()),
                            ("hostClosed", (outcome != "unconfirmed").into()),
                            ("hostPortReleased", host_released.into()),
                            ("hostPortReleasedAfterDrop", after_drop_released.into()),
                            ("remainingCloses", n(1)),
                            (
                                "elapsedMilliseconds",
                                n(started.elapsed().as_millis() as usize),
                            ),
                            ("physicalAudioOpens", n(0)),
                        ]),
                    );
                }
            }
        }
    }
}

fn invite_row(invite: &Request, tag: &str) -> IvyJson {
    let body = String::from_utf8(invite.body.clone()).unwrap();
    IvyJson::object([
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
                && invite.to_header().unwrap().tag().unwrap().unwrap().value() == "caller-tag")
                .into(),
        ),
        ("hasPcma", body.contains("PCMA/8000").into()),
        ("hasPcmu", body.contains("PCMU/8000").into()),
        ("hasSendrecv", body.contains("sendrecv").into()),
    ])
}
fn ack_row(ack: &Request) -> IvyJson {
    IvyJson::object([
        ("method", "ACK".into()),
        (
            "sequence",
            n(ack.cseq_header().unwrap().seq().unwrap() as usize),
        ),
        (
            "sameCallId",
            (ack.call_id_header().unwrap().value() == "owned-native-answer").into(),
        ),
    ])
}
fn final_row(response: &Response, rtp_port: u16) -> IvyJson {
    IvyJson::object([
        ("status", n(response.status_code.code() as usize)),
        (
            "sequence",
            n(response.cseq_header().unwrap().seq().unwrap() as usize),
        ),
        (
            "contentType",
            response
                .headers
                .iter()
                .find_map(|header| match header {
                    rsipstack::sip::Header::ContentType(value) => Some(value.value().into()),
                    _ => None,
                })
                .unwrap_or(IvyJson::Null),
        ),
        (
            "body",
            String::from_utf8(response.body.clone())
                .unwrap()
                .replace(&format!("m=audio {rtp_port}"), "m=audio owned-rtp")
                .into(),
        ),
    ])
}
async fn next(peer: &UdpSocket, method: Method) -> (Request, std::net::SocketAddr) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let (request, remote) = crate::sip_host::received(peer).await;
            if request.method == method {
                return (request, remote);
            }
            // Late finals can follow queued retransmissions of the same offer.
            // Only the selected INVITE/ACK controls are compared, not retry timing.
            assert_eq!(request.method, Method::Invite);
            assert_ne!(method, Method::Invite);
        }
    })
    .await
    .expect("Original dialog control unavailable")
}
async fn end(dispatcher: &NativeDispatcher, selected: &str, peer: &UdpSocket) -> u32 {
    assert!(
        dispatcher
            .execute(command("call.hangup", selected, 6, 6))
            .await
            .outcome()
            .error_code()
            .is_none()
    );
    let (bye, remote) = next(peer, Method::Bye).await;
    let sequence = bye.cseq_header().unwrap().seq().unwrap();
    crate::sip_host::send(peer, remote, &crate::sip_host::response(&bye, 200, "", "")).await;
    sequence
}

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
    for preferred in ["PCMA", "PCMU"] {
        for mode in ["initial", "delayed"] {
            for kind in [
                "upgraded",
                "fallback",
                "refused",
                "disabled",
                "unauthenticated",
                "trusted",
                "late",
                "repeated",
                "retransmitted",
                "pending_hangup",
                "concurrent",
                "concurrent_reverse",
                "repeat_downgrade",
                "hangup_success",
                "pending_release",
                "concurrent_refused_first",
                "concurrent_refused_second",
                "concurrent_missing_type",
                "concurrent_other_type",
                "concurrent_malformed",
                "concurrent_late",
            ] {
                let late = ["late", "concurrent_late"].contains(&kind);
                let mixed = kind.starts_with("concurrent_")
                    && !["concurrent_reverse", "concurrent_late"].contains(&kind);
                let concurrent = ["concurrent", "concurrent_reverse", "concurrent_late"]
                    .contains(&kind)
                    || mixed;
                let repeated = ["repeated", "repeat_downgrade"].contains(&kind);
                let early_end =
                    ["pending_hangup", "hangup_success", "pending_release"].contains(&kind);
                let lifecycle = [
                    "late",
                    "repeated",
                    "retransmitted",
                    "pending_hangup",
                    "concurrent",
                    "concurrent_reverse",
                    "repeat_downgrade",
                    "hangup_success",
                    "pending_release",
                    "concurrent_late",
                ]
                .contains(&kind)
                    || mixed;
                let scenario = format!("{mode}_{preferred}_{kind}");
                let delayed = mode == "delayed";
                let (owner, factory, routes, config) = actual_owner(40000, 41998);
                let config = config.replace(
                    r#"["PCMA","PCMU"]"#,
                    if preferred == "PCMU" {
                        r#"["PCMU","PCMA"]"#
                    } else {
                        r#"["PCMA","PCMU"]"#
                    },
                );
                assert_eq!(
                    parse(&config, 65536)
                        .unwrap()
                        .get("codecs")
                        .unwrap()
                        .get("preferences")
                        .unwrap()
                        .as_array()
                        .unwrap()[0]
                        .as_str()
                        .unwrap(),
                    preferred
                );
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
                let initial_sdp = sdp("PCMA", 8);
                super::answer::send(
                    &peer,
                    target,
                    "INVITE",
                    1,
                    "z9hG4bK-owned-upgrade",
                    "",
                    if delayed { "" } else { &initial_sdp },
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
                    if delayed { &initial_sdp } else { "" },
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
                let mut pending =
                    Some(tokio::spawn(async move { dispatch.execute(request).await }));
                let mut requests = vec![];
                let mut finals = vec![];
                let mut early = None;
                let mut again = None;
                let mut bye_sequence = None;
                let mut release = None;
                let mut release_before_final = false;
                if !["disabled", "unauthenticated", "trusted"].contains(&kind) {
                    let (mut invite, remote) = next(&peer, Method::Invite).await;
                    assert_eq!(invite.method, Method::Invite);
                    requests.push(invite_row(&invite, &tag));
                    let mut other_invite = None;
                    let mut second = None;
                    if concurrent {
                        let dispatch = dispatcher.clone();
                        let params = selected.clone();
                        second = Some(tokio::spawn(async move {
                            dispatch
                                .execute(command("call.codec.upgrade", &params, 8, 8))
                                .await
                        }));
                        let (other, _) = next(&peer, Method::Invite).await;
                        assert_ne!(
                            invite.cseq_header().unwrap().seq().unwrap(),
                            other.cseq_header().unwrap().seq().unwrap()
                        );
                        requests.push(invite_row(&other, &tag));
                        other_invite = Some(other);
                        if kind == "concurrent_reverse" {
                            std::mem::swap(&mut invite, other_invite.as_mut().unwrap());
                        }
                    }
                    // The retained receipt/driver survives abandonment of its first IPC waiter.
                    pending.take().unwrap().abort();
                    if late {
                        early = Some(
                            super::feedback::row(&dispatcher, "call.codec.upgrade", &selected, 5)
                                .await,
                        );
                        if kind == "concurrent_late" {
                            second.take().unwrap().await.unwrap();
                            again = Some(
                                super::feedback::row(
                                    &dispatcher,
                                    "call.codec.upgrade",
                                    &selected,
                                    8,
                                )
                                .await,
                            );
                        }
                    }
                    if early_end {
                        bye_sequence = Some(end(&dispatcher, &selected, &peer).await);
                    }
                    if kind == "pending_release" {
                        let work = owner.release(&call).unwrap();
                        release_before_final = matches!(
                            tokio::time::timeout(Duration::from_secs(1), work.clone().wait()).await,
                            Ok(Ok(()))
                        );
                        release = Some(work);
                    }
                    let answer_sdp = if kind == "concurrent_malformed" {
                        "invalid-sdp".to_owned()
                    } else {
                        sdp(
                            if kind == "fallback" { "PCMA" } else { "PCMU" },
                            if kind == "fallback" { 8 } else { 0 },
                        )
                    };
                    let refused =
                        ["refused", "pending_hangup", "concurrent_refused_first"].contains(&kind);
                    let content_type = if refused || kind == "concurrent_missing_type" {
                        ""
                    } else if kind == "concurrent_other_type" {
                        "Content-Type: application/octet-stream\r\n"
                    } else {
                        "Content-Type: application/sdp\r\n"
                    };
                    let response = crate::sip_host::response(
                        &invite,
                        if refused { 488 } else { 200 },
                        &format!(
                            "{content_type}Contact: <sip:caller@{}>\r\n",
                            peer.local_addr().unwrap()
                        ),
                        if refused { "" } else { &answer_sdp },
                    );
                    let mut response = response;
                    response
                        .headers
                        .unique_push(invite.to_header().unwrap().clone().into());
                    if mixed {
                        finals.push(final_row(&response, rtp.local_addr().unwrap().port()));
                    }
                    crate::sip_host::send(&peer, remote, &response).await;
                    let (ack, _) = next(&peer, Method::Ack).await;
                    assert_eq!(ack.method, Method::Ack);
                    requests.push(ack_row(&ack));
                    if concurrent {
                        let other = other_invite.unwrap();
                        let pcma = !mixed
                            || ["concurrent_missing_type", "concurrent_other_type"].contains(&kind);
                        let body = if kind == "concurrent_refused_second" {
                            String::new()
                        } else {
                            sdp(if pcma { "PCMA" } else { "PCMU" }, if pcma { 8 } else { 0 })
                        };
                        let content_type = if kind == "concurrent_refused_second" {
                            ""
                        } else {
                            "Content-Type: application/sdp\r\n"
                        };
                        let mut accepted = crate::sip_host::response(
                            &other,
                            if kind == "concurrent_refused_second" {
                                488
                            } else {
                                200
                            },
                            &format!(
                                "{content_type}Contact: <sip:caller@{}>\r\n",
                                peer.local_addr().unwrap()
                            ),
                            &body,
                        );
                        accepted
                            .headers
                            .unique_push(other.to_header().unwrap().clone().into());
                        if mixed {
                            finals.push(final_row(&accepted, rtp.local_addr().unwrap().port()));
                            crate::sip_host::send(&peer, remote, &accepted).await;
                            let (ack, _) = next(&peer, Method::Ack).await;
                            requests.push(ack_row(&ack));
                        }
                        if kind != "concurrent_late" {
                            early = Some(
                                super::feedback::row(
                                    &dispatcher,
                                    "call.codec.upgrade",
                                    &selected,
                                    5,
                                )
                                .await,
                            );
                            second.take().unwrap().await.unwrap();
                            again = Some(
                                super::feedback::row(
                                    &dispatcher,
                                    "call.codec.upgrade",
                                    &selected,
                                    8,
                                )
                                .await,
                            );
                        }
                        if !mixed {
                            crate::sip_host::send(&peer, remote, &accepted).await;
                            let (ack, _) = next(&peer, Method::Ack).await;
                            requests.push(ack_row(&ack));
                        }
                    }
                    if kind == "retransmitted" {
                        crate::sip_host::send(&peer, remote, &response).await;
                        let (repeated_ack, _) = next(&peer, Method::Ack).await;
                        assert_eq!(ack.to_string(), repeated_ack.to_string());
                        requests.push(ack_row(&repeated_ack));
                    }
                    if repeated {
                        // A fresh operation gets new protocol work on this same call.
                        early = Some(
                            super::feedback::row(&dispatcher, "call.codec.upgrade", &selected, 5)
                                .await,
                        );
                        let dispatch = dispatcher.clone();
                        let params = selected.clone();
                        let pending = tokio::spawn(async move {
                            dispatch
                                .execute(command("call.codec.upgrade", &params, 8, 8))
                                .await
                        });
                        let (invite, remote) = next(&peer, Method::Invite).await;
                        requests.push(invite_row(&invite, &tag));
                        let accepted_sdp = if kind == "repeat_downgrade" {
                            sdp("PCMA", 8)
                        } else {
                            sdp("PCMU", 0)
                        };
                        let mut response = crate::sip_host::response(
                            &invite,
                            200,
                            &format!(
                                "Content-Type: application/sdp\r\nContact: <sip:caller@{}>\r\n",
                                peer.local_addr().unwrap()
                            ),
                            &accepted_sdp,
                        );
                        response
                            .headers
                            .unique_push(invite.to_header().unwrap().clone().into());
                        crate::sip_host::send(&peer, remote, &response).await;
                        let (ack, _) = next(&peer, Method::Ack).await;
                        requests.push(ack_row(&ack));
                        pending.await.unwrap();
                        again = Some(
                            super::feedback::row(&dispatcher, "call.codec.upgrade", &selected, 8)
                                .await,
                        );
                    }
                }
                if let Some(pending) = pending {
                    pending.await.unwrap();
                }
                let first = match early {
                    Some(saved) => saved,
                    None => {
                        super::feedback::row(&dispatcher, "call.codec.upgrade", &selected, 5).await
                    }
                };
                if kind != "unauthenticated" {
                    let result = parse(
                        &first
                            .get("output")
                            .unwrap()
                            .get("json")
                            .unwrap()
                            .as_str()
                            .unwrap(),
                        65536,
                    )
                    .unwrap();
                    let sent = if !late
                        && (mixed
                            || [
                                "upgraded",
                                "repeated",
                                "retransmitted",
                                "concurrent",
                                "concurrent_reverse",
                                "repeat_downgrade",
                            ]
                            .contains(&kind))
                    {
                        "PCMU"
                    } else {
                        "PCMA"
                    };
                    let state = if ["disabled", "trusted"].contains(&kind) {
                        "skipped"
                    } else if kind != "refused" && !early_end && !late && sent == preferred {
                        "upgraded"
                    } else {
                        "fallback"
                    };
                    assert_eq!(
                        result.get("state").unwrap().as_str().unwrap(),
                        state,
                        "{scenario}"
                    );
                    if kind == "pending_release" {
                        assert!(result.get("codec").unwrap().is_null());
                    } else {
                        assert_eq!(
                            result.get("codec").unwrap().as_str().unwrap(),
                            sent,
                            "{scenario}"
                        );
                    }
                }
                let mut rows = vec![first.clone()];
                if let Some(again) = again {
                    rows.push(again);
                }
                if late {
                    until(|| {
                        media
                            .observation()
                            .send_format
                            .is_some_and(|f| f.name() == "PCMU")
                    })
                    .await;
                }
                let send_codec = media.observation().send_format.map(|f| f.name().to_owned());
                let bye_sequence = match bye_sequence {
                    Some(sequence) => sequence,
                    None => end(&dispatcher, &selected, &peer).await,
                };
                rows.push(
                    super::feedback::row(&dispatcher, "call.codec.upgrade", &selected, 7).await,
                );
                if let Some(release) = release {
                    release.wait().await.unwrap();
                } else {
                    owner.release(&call).unwrap().wait().await.unwrap();
                }
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
                if lifecycle {
                    if let IvyJson::Object(fields) = &mut result {
                        fields.push((
                            "dialog".into(),
                            IvyJson::object([
                                ("firstBeforeFinal", late.into()),
                                ("hangupBeforeFinal", early_end.into()),
                                (
                                    "sendCodecBeforeEnd",
                                    send_codec.map_or(IvyJson::Null, Into::into),
                                ),
                                ("byeSequence", n(bye_sequence as usize)),
                            ]),
                        ));
                    }
                }
                if ["pending_release", "hangup_success"].contains(&kind) {
                    if let IvyJson::Object(fields) = &mut result {
                        if kind == "pending_release" {
                            fields.push(("releaseBeforeFinal".into(), release_before_final.into()));
                        }
                        fields.push(("ackAfterFinal".into(), true.into()));
                    }
                }
                if kind == "concurrent_late" {
                    if let IvyJson::Object(fields) = &mut result {
                        let (_, IvyJson::Object(dialog)) = fields
                            .iter_mut()
                            .find(|(key, _)| key == &"dialog".into())
                            .unwrap()
                        else {
                            panic!("Lifecycle observation absent");
                        };
                        dialog.insert(1, ("secondBeforeFinal".into(), true.into()));
                    }
                }
                if mixed {
                    if let IvyJson::Object(fields) = &mut result {
                        fields.push(("finals".into(), IvyJson::Array(finals)));
                    }
                }
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
}
