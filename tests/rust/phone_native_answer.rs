use super::incoming::{normalize, outcome, receive};
use super::*;
use rsipstack::sip::{Method, prelude::*};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_answer_dto_global_lookup_and_outgoing_attempt_precedence_match_original() {
    let (owner, _, routes, params) = actual_owner(42000, 42998);
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
    let valid = format!(r#"{{"callId":"{CALL}","waiting":false}}"#);
    let mut steps: Vec<(String, String)> = vec![
        (
            "call.answer".into(),
            r#"{"callId":42,"waiting":false}"#.into(),
        ),
        ("configure".into(), params),
    ];
    for params in [
        "{}",
        r#"{"callId":null,"waiting":false}"#,
        r#"{"callId":42,"waiting":false}"#,
        r#"{"callId":false,"waiting":false}"#,
        r#"{"callId":"missing","waiting":false}"#,
        r#"{"callId":"missing","waiting":null}"#,
        r#"{"callId":"missing"}"#,
        r#"{"callId":"missing","waiting":false,"extra":0}"#,
    ] {
        steps.push(("call.answer".into(), params.into()));
    }
    steps.push(("call.prepare".into(), selected.clone()));
    for waiting in ["null", "42", r#""false""#, "[]", "{}"] {
        steps.push((
            "call.answer".into(),
            format!(r#"{{"callId":"{CALL}","waiting":{waiting}}}"#),
        ));
    }
    steps.extend([
        ("call.answer".into(), selected.clone()),
        ("call.answer".into(), valid.clone()),
        ("call.hangup".into(), selected.clone()),
        ("call.answer".into(), valid.clone()),
        ("call.answer".into(), valid.replace("false", "null")),
        ("call.release".into(), selected),
        ("call.answer".into(), valid),
    ]);
    let original = super::dial::expected("native-answer-validation");
    let mut rows = vec![];
    for (i, (method, params)) in steps.iter().enumerate() {
        rows.push(outcome(
            dispatcher
                .execute(command(method, params, i as u64 + 1, i as u64 + 1))
                .await
                .outcome(),
        ));
    }
    trace(
        "native-answer-validation",
        IvyJson::object([
            ("rust", IvyJson::Array(rows.clone())),
            ("original", original.clone()),
            ("steps", n(steps.len())),
        ]),
    );
    super::dial::compare(&rows, &original, &steps);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    assert!(remaining.methods.lock().unwrap().is_empty());
    handler.shutdown().await.unwrap();
}
fn raw(
    method: &str,
    seq: u32,
    branch: &str,
    tag: &str,
    source: std::net::SocketAddr,
    target: std::net::SocketAddr,
    body: &str,
) -> String {
    format!(
        "{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {source};rport;branch={branch}\r\nFrom: <sip:caller@{source}>;tag=caller-tag\r\nTo: <sip:fixture@{target}>{}\r\nCall-ID: owned-native-answer\r\nCSeq: {seq} {method}\r\nContact: <sip:caller@{source}>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
        if tag.is_empty() {
            String::new()
        } else {
            format!(";tag={tag}")
        },
        body.len()
    )
}
pub(super) async fn send(
    peer: &UdpSocket,
    target: std::net::SocketAddr,
    method: &str,
    seq: u32,
    branch: &str,
    tag: &str,
    body: &str,
) {
    peer.send_to(
        raw(
            method,
            seq,
            branch,
            tag,
            peer.local_addr().unwrap(),
            target,
            body,
        )
        .as_bytes(),
        target,
    )
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_native_answers_keep_original_media_ack_controls_and_saved_receipts() {
    for scenario in [
        "initial_connected",
        "delayed_connected",
        "initial_remote_bye",
        "delayed_remote_bye",
        "initial_waiting",
        "initial_slow_ack",
        "initial_invalid_offer",
        "delayed_invalid_ack",
        "initial_hangup_before_ack",
        "delayed_hangup_before_ack",
        "initial_reinvite",
        "delayed_reinvite",
        "initial_media_failure_before_ack",
        "delayed_media_failure_before_ack",
        "initial_media_failure_after_start",
        "delayed_media_failure_after_start",
        "initial_media_failure_before_ack_format",
        "delayed_media_failure_before_ack_format",
        "initial_media_failure_after_start_format",
        "delayed_media_failure_after_start_format",
    ] {
        let trace_scenario = scenario;
        let format_failure = scenario.ends_with("_format");
        let scenario = scenario.strip_suffix("_format").unwrap_or(scenario);
        let delayed = scenario.starts_with("delayed");
        let waiting = scenario.ends_with("waiting");
        let events = Arc::new(super::notifications::FailureEvents::default());
        let (owner, factory, routes, params) =
            actual_owner_with(40000, 41998, Arc::new(Clock), events.clone());
        let config = params.replace(
            "\"transport\":\"udp\"",
            "\"transport\":\"udp\",\"incomingRingSeconds\":1",
        );
        let original = super::dial::expected(&format!("native-answer-{trace_scenario}"));
        if format_failure {
            trace(
                &format!("native-answer-original-{trace_scenario}"),
                original.clone(),
            );
        }
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
        let sdp = format!(
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n",
            rtp.local_addr().unwrap().port()
        );
        send(
            &peer,
            target,
            "INVITE",
            1,
            "z9hG4bK-owned-answer",
            "",
            if scenario.ends_with("invalid_offer") {
                "invalid-sdp"
            } else if delayed {
                ""
            } else {
                &sdp
            },
        )
        .await;
        assert_eq!(
            receive(&peer, 4000).await.unwrap().status_code.code(),
            180,
            "{scenario}"
        );
        until(|| !owner.observation().calls.is_empty()).await;
        let id = owner.observation().calls[0].id.clone();
        let call = owner.find(&id).unwrap();
        call.prepared().await.unwrap();
        let media = call.media().unwrap();
        let original_rtp = media.observation().receive.transport.local_rtp;
        let selected = format!(r#"{{"callId":"{id}"}}"#);
        let features = format!(r#"{{"callId":"{id}","challenge":null}}"#);
        for (m, p, op) in [
            ("call.claim", &selected, 2),
            ("call.features", &features, 3),
        ] {
            assert!(
                dispatcher
                    .execute(command(m, p, op, op))
                    .await
                    .outcome()
                    .error_code()
                    .is_none()
            );
        }
        let params = format!(r#"{{"callId":"{id}","waiting":{waiting}}}"#);
        let saved_request = command("call.answer", &params, 4, 4);
        let dispatch = dispatcher.clone();
        let mut pending = tokio::spawn(async move { dispatch.execute(saved_request).await });
        let final_response = receive(&peer, 4000).await.expect("Original final answer");
        let status = final_response.status_code.code();
        let tag = final_response
            .to_header()
            .unwrap()
            .tag()
            .unwrap()
            .unwrap()
            .value()
            .to_owned();
        let mut hungup = IvyJson::Null;
        let mut end_request = IvyJson::Null;
        let no_bye_window =
            if scenario.ends_with("invalid_ack") || scenario.ends_with("hangup_before_ack") {
                150
            } else {
                0
            };
        let mut controls = vec![];
        *events.call.lock().unwrap() = Some(call.clone());
        let mut early_answer = None;
        let mut media_started_before_ack = false;
        if status == 200 {
            match tokio::time::timeout(Duration::from_millis(150), &mut pending).await {
                Ok(answer) => early_answer = Some(answer.unwrap()),
                Err(_) => assert!(delayed, "Initial-offer answer must complete before ACK"),
            }
            assert_eq!(early_answer.is_some(), !delayed);
            media_started_before_ack = media.observation().started;
            assert_eq!(media_started_before_ack, !delayed);
            if scenario.ends_with("hangup_before_ack") {
                hungup = outcome(
                    dispatcher
                        .execute(command("call.hangup", &selected, 5, 5))
                        .await
                        .outcome(),
                );
            }
            if scenario.ends_with("slow_ack") {
                tokio::time::sleep(Duration::from_millis(1250)).await;
                assert!(early_answer.is_some());
                while let Some(response) = receive(&peer, 50).await {
                    assert_eq!(
                        response.status_code.code(),
                        200,
                        "Answer retired the original ring timer"
                    );
                }
            }
            if scenario.ends_with("media_failure_before_ack") {
                super::notifications::fail_media(&media, &sdp);
            }
            send(
                &peer,
                target,
                "ACK",
                1,
                "z9hG4bK-owned-ack",
                &tag,
                if delayed {
                    if scenario.ends_with("invalid_ack") {
                        "invalid-sdp"
                    } else {
                        &sdp
                    }
                } else {
                    ""
                },
            )
            .await;
        } else {
            assert_eq!(status, 406, "{scenario}");
            send(&peer, target, "ACK", 1, "z9hG4bK-owned-answer", &tag, "").await;
        }
        let answer_completed_before_ack = early_answer.is_some();
        let answer = match early_answer {
            Some(answer) => answer,
            None => tokio::time::timeout(Duration::from_secs(7), &mut pending)
                .await
                .unwrap()
                .unwrap(),
        };
        let answered = outcome(answer.outcome());
        if scenario.ends_with("hangup_before_ack") || scenario.ends_with("media_failure_before_ack")
        {
            assert!(media.observation().closed);
            assert_eq!(
                media.observation().started,
                !delayed,
                "Ending cannot restart original media"
            );
        }
        let saved = answer
            .outcome()
            .result_json()
            .expect("Native answer outcome snapshot")
            .to_owned();
        if scenario.ends_with("media_failure_after_start") {
            assert!(media.observation().started);
            super::notifications::fail_media(&media, &sdp);
        }
        if status == 200 && scenario.ends_with("reinvite") {
            send(
                &peer,
                target,
                "INVITE",
                2,
                "z9hG4bK-owned-reinvite",
                &tag,
                if delayed { "" } else { &sdp },
            )
            .await;
            let re_status = receive(&peer, 4000).await.unwrap().status_code.code();
            controls.push(re_status);
            send(
                &peer,
                target,
                "INVITE",
                3,
                "z9hG4bK-owned-overlap",
                &tag,
                &sdp,
            )
            .await;
            controls.push(receive(&peer, 4000).await.unwrap().status_code.code());
            send(
                &peer,
                target,
                "INVITE",
                1,
                "z9hG4bK-owned-stale",
                &tag,
                &sdp,
            )
            .await;
            controls.push(receive(&peer, 4000).await.unwrap().status_code.code());
            send(
                &peer,
                target,
                "ACK",
                2,
                if re_status == 200 {
                    "z9hG4bK-owned-reack"
                } else {
                    "z9hG4bK-owned-reinvite"
                },
                &tag,
                "",
            )
            .await;
            send(
                &peer,
                target,
                "ACK",
                3,
                "z9hG4bK-owned-overlap-ack",
                &tag,
                "",
            )
            .await;
            send(&peer, target, "ACK", 1, "z9hG4bK-owned-stale-ack", &tag, "").await;
            send(
                &peer,
                target,
                "OPTIONS",
                4,
                "z9hG4bK-owned-options",
                &tag,
                "",
            )
            .await;
            controls.push(receive(&peer, 4000).await.unwrap().status_code.code());
        }
        if status == 200 && scenario.ends_with("remote_bye") {
            for _ in 0..2 {
                send(&peer, target, "BYE", 5, "z9hG4bK-owned-bye", &tag, "").await;
                controls.push(receive(&peer, 4000).await.unwrap().status_code.code());
            }
        } else if status == 200 {
            if hungup.is_null()
                && !scenario.ends_with("invalid_ack")
                && !scenario.contains("media_failure")
            {
                hungup = outcome(
                    dispatcher
                        .execute(command("call.hangup", &selected, 5, 5))
                        .await
                        .outcome(),
                );
            }
            match tokio::time::timeout(
                Duration::from_millis(if no_bye_window > 0 {
                    no_bye_window
                } else {
                    4000
                }),
                crate::sip_host::received(&peer),
            )
            .await
            {
                Ok((bye, remote)) => {
                    end_request = "BYE".into();
                    assert_eq!(bye.method, Method::Bye, "{scenario}");
                    assert_eq!(bye.call_id_header().unwrap().value(), "owned-native-answer");
                    crate::sip_host::send(
                        &peer,
                        remote,
                        &crate::sip_host::response(&bye, 200, "", ""),
                    )
                    .await;
                }
                Err(_) => assert!(no_bye_window > 0, "Original incoming BYE unavailable"),
            }
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
        let failure_callbacks = events.receipts.lock().unwrap().clone();
        events.call.lock().unwrap().take();
        let release_result = if format_failure {
            let released = dispatcher
                .execute(command("call.release", &selected, 7, 7))
                .await;
            assert!(released.outcome().error_code().is_none());
            Some(outcome(released.outcome()))
        } else {
            owner.release(&call).unwrap().wait().await.unwrap();
            None
        };
        // Reusing the ID cannot lend the original saved answer another handle.
        let replacement = owner.prepare(id.clone()).unwrap();
        replacement.prepared().await.unwrap();
        assert_eq!(
            dispatcher
                .execute(command("call.answer", &params, 4, 1004))
                .await
                .outcome()
                .result_json(),
            Some(saved.as_str())
        );
        assert!(!replacement.observation().attempted);
        owner.release(&replacement).unwrap().wait().await.unwrap();
        let released = owner.find(&id).is_none();
        handler.shutdown().await.unwrap();
        assert!(media.observation().closed);
        assert_eq!(
            media.observation().receive.transport.local_rtp,
            original_rtp
        );
        assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
        assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
        assert!(remaining.methods.lock().unwrap().is_empty());
        drop(
            UdpSocket::bind(target)
                .await
                .expect("Original answered binding released"),
        );
        let mut result = IvyJson::object([
            ("status", IvyJson::Number(status.into())),
            ("answered", answered),
            ("hungup", hungup),
            ("ended", ended),
            (
                "controls",
                IvyJson::Array(
                    controls
                        .iter()
                        .map(|n| IvyJson::Number((*n).into()))
                        .collect(),
                ),
            ),
            ("endRequest", end_request),
            (
                "noByeObservationMs",
                IvyJson::Number((no_bye_window as u32).into()),
            ),
            ("finalHasSdp", (!final_response.body.is_empty()).into()),
            ("ackHasSdp", (status == 200 && delayed).into()),
            ("sameMedia", true.into()),
            ("released", released.into()),
            ("closed", true.into()),
            ("failureCallbacks", IvyJson::Array(failure_callbacks)),
            (
                "answerCompletedBeforeAck",
                answer_completed_before_ack.into(),
            ),
            ("mediaStartedBeforeAck", media_started_before_ack.into()),
        ]);
        if let Some(release_result) = release_result {
            if let IvyJson::Object(fields) = &mut result {
                fields.push(("releaseResult".into(), release_result));
            }
        }
        let mut actual = result.clone();
        normalize(&mut actual, &id);
        let original_result = {
            let o = &original;
            let result = parse(
                &o.as_array().unwrap()[0]
                    .get("json")
                    .unwrap_or_else(|| panic!("{scenario}: {o:?}"))
                    .as_str()
                    .unwrap(),
                65536,
            )
            .unwrap();
            if format_failure {
                let measured = result.get("result").and_then(|v| v.get("comparison"));
                assert!(
                    measured.is_some(),
                    "Original format failure observation unavailable: {result:?}"
                );
                measured.unwrap().clone()
            } else {
                result
            }
        };
        let mut expected = original_result.clone();
        {
            let v = &mut expected;
            let wrapped = v
                .get("answered")
                .unwrap()
                .get("json")
                .unwrap()
                .as_str()
                .unwrap();
            let original_id = parse(&wrapped, 65536)
                .unwrap()
                .get("id")
                .unwrap()
                .as_str()
                .unwrap();
            normalize(v, &original_id);
        }
        trace(
            &format!("native-answer-{trace_scenario}"),
            IvyJson::object([
                ("rust", result),
                ("original", original_result),
                ("normalizedRust", actual.clone()),
                ("normalizedOriginal", expected.clone()),
                ("originalBindingReleased", true.into()),
                ("savedAnswerReplayedAfterReplacement", true.into()),
                ("physicalAudioOpens", n(0)),
            ]),
        );
        assert_eq!(actual, expected, "{scenario}");
    }
}
