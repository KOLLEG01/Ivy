use super::*;
use rsipstack::sip::{Method, Request, prelude::*};
use std::net::SocketAddr;
fn args(
    id: &str,
    destination: &str,
    username: &str,
    password: &str,
    ring: &str,
    waiting: &str,
) -> String {
    format!(
        r#"{{"callId":{id},"destination":{destination},"username":{username},"password":{password},"ringSeconds":{ring},"waiting":{waiting}}}"#
    )
}
fn normal_params(id: &str) -> String {
    args(
        id,
        r#""sip:peer@127.0.0.1:9""#,
        r#""fixture-user""#,
        r#""fixture-only""#,
        "5",
        "false",
    )
}
pub(super) fn original_raw(steps: &[(String, String)]) -> Option<IvyJson> {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        return None;
    }
    let raw = format!(
        "[{}]",
        steps
            .iter()
            .map(|(m, p)| format!(r#"{{"method":"{m}","params":{p}}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut child = Command::new("pwsh")
        .args([
            "-NoProfile",
            "-File",
            "tests/fixtures/phone-native-dial-oracle.ps1",
        ])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(raw.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(
        parse(
            std::str::from_utf8(&output.stdout)
                .unwrap()
                .trim_start_matches('\u{feff}'),
            2_000_000,
        )
        .unwrap(),
    )
}
fn normalize(v: &mut IvyJson) {
    if let IvyJson::Object(fields) = v {
        for (k, v) in fields {
            if k == &"endpoint".into() {
                *v = "owned-endpoint".into();
            } else if k == &"requestUri".into()
                && v.as_str().unwrap().starts_with("sip:peer@127.0.0.1:")
            {
                let raw = v.as_str().unwrap();
                let port = raw.strip_prefix("sip:peer@127.0.0.1:").unwrap();
                assert_ne!(port.parse::<u16>().unwrap(), 0);
                *v = "sip:peer@127.0.0.1:owned-peer".into();
            } else if k == &"fromUri".into()
                && v.as_str()
                    .unwrap()
                    .starts_with("sip:fixture-user@127.0.0.1:")
            {
                let raw = v.as_str().unwrap();
                let port = raw
                    .strip_prefix("sip:fixture-user@127.0.0.1:")
                    .expect("original From identity");
                assert_ne!(port.parse::<u16>().unwrap(), 0);
                *v = "sip:fixture-user@127.0.0.1:owned-peer".into();
            } else {
                normalize(v);
            }
        }
    }
    if let IvyJson::Array(rows) = v {
        for v in rows {
            normalize(v);
        }
    }
}
pub(super) fn compare(rows: &[IvyJson], original: &Option<IvyJson>, steps: &[(String, String)]) {
    if let Some(original) = original {
        for (i, (a, b)) in rows.iter().zip(original.as_array().unwrap()).enumerate() {
            if let Some(raw) = a.get("json") {
                let mut a = parse(&raw.as_str().unwrap(), 65536).unwrap();
                let mut b = parse(
                    &b.get("json")
                        .unwrap_or_else(|| panic!("step{i}: {b:?}"))
                        .as_str()
                        .unwrap(),
                    65536,
                )
                .unwrap();
                normalize(&mut a);
                normalize(&mut b);
                assert_eq!(a, b, "step{i}: {:?}", steps[i]);
            } else {
                assert_eq!(a, b, "step{i}: {:?}", steps[i]);
            }
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_dial_exact_dto_lookup_bounds_uri_and_attempt_precedence_match_original() {
    let (owner, factory, routes, params) = actual_owner(51000, 51998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        remaining.clone(),
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let id = format!("\"{CALL}\"");
    let mut steps = vec![
        ("call.dial".into(), r#"{"callId":42}"#.into()),
        ("configure".into(), params),
    ];
    for id in ["null", "42", "true", r#""missing""#] {
        steps.push(("call.dial".into(), normal_params(id)));
    }
    steps.push(("call.dial".into(),r#"{"callId":"missing","destination":false,"username":null,"password":null,"ringSeconds":5,"waiting":false}"#.into()));
    steps.push(("call.prepare".into(), selected.clone()));
    let mut invalids = vec![
        selected.clone(),
        normal_params(&id).replace("\"waiting\":false", "\"waiting\":false,\"extra\":null"),
    ];
    for (d, u, p, r, w) in [
        ("null", "null", "null", "5", "false"),
        ("false", "null", "null", "5", "false"),
        (r#""""#, "null", "null", "5", "false"),
        (r#""bad-uri""#, "null", "null", "5", "false"),
        (r#""sip:""#, "null", "null", "5", "false"),
        (r#""tel:123""#, "null", "null", "5", "false"),
        (r#""sip:peer@example.test""#, "7", "null", "5", "false"),
        (r#""sip:peer@example.test""#, "null", "[]", "5", "false"),
        (r#""sip:peer@example.test""#, "null", "null", "0", "false"),
        (r#""sip:peer@example.test""#, "null", "null", "121", "false"),
        (r#""sip:peer@example.test""#, "null", "null", "5.0", "false"),
        (r#""sip:peer@example.test""#, "null", "null", "5e0", "false"),
        (
            r#""sip:peer@example.test""#,
            "null",
            "null",
            "null",
            "false",
        ),
        (
            r#""sip:peer@example.test""#,
            "null",
            "null",
            "2147483648",
            "false",
        ),
        (r#""sip:peer@example.test""#, "null", "null", "5", "null"),
        (r#""sip:peer@example.test""#, "null", "null", "5", "0"),
        (
            r#""sip:peer@example.test""#,
            "null",
            "null",
            "5",
            r#""false""#,
        ),
    ] {
        invalids.push(args(&id, d, u, p, r, w));
    }
    for field in ["destination", "username", "password"] {
        let n = match field {
            "destination" => 2049,
            "username" => 257,
            _ => 4097,
        };
        let raw = normal_params(&id);
        let mut v = parse(&raw, 65536).unwrap();
        if let IvyJson::Object(ref mut f) = v {
            *f.iter_mut().find(|(k, _)| k == &field.into()).unwrap() =
                (field.into(), "x".repeat(n).into());
        }
        invalids.push(stringify(&v, 65536).unwrap());
    }
    for p in &invalids {
        steps.push(("call.dial".into(), p.clone()));
    }
    steps.push(("call.hangup".into(), selected.clone()));
    for p in &invalids {
        steps.push(("call.dial".into(), p.clone()));
    }
    steps.push(("call.dial".into(), normal_params(&id)));
    steps.push(("call.release".into(), selected));
    let original = original_raw(&steps);
    let dispatcher = NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap();
    let mut rows = Vec::new();
    for (i, (m, p)) in steps.iter().enumerate() {
        let r = dispatcher
            .execute(command(m, p, i as u64 + 1, i as u64 + 1))
            .await;
        rows.push(if let Some(e) = r.outcome().error_code() {
            IvyJson::object([("error", e.into())])
        } else {
            IvyJson::object([("json", r.outcome().result_json().unwrap().into())])
        });
    }
    trace(
        "native-dial-validation",
        IvyJson::object([
            ("rust", IvyJson::Array(rows.clone())),
            ("original", original.clone().unwrap_or(IvyJson::Null)),
            ("rawInputs", format!("{steps:?}").into()),
        ]),
    );
    compare(&rows, &original, &steps);
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    assert!(remaining.methods.lock().unwrap().is_empty());
    handler.shutdown().await.unwrap();
}
async fn received(peer: &UdpSocket) -> (Request, SocketAddr) {
    crate::sip_host::received(peer).await
}
async fn outgoing(
    owner: &NativeSipOperations,
    dispatcher: Arc<NativeDispatcher>,
    scenario: &str,
    operation: u64,
    supplied_peer: Option<UdpSocket>,
    target: Option<&str>,
) -> IvyJson {
    let peer = match supplied_peer {
        Some(peer) => peer,
        None => UdpSocket::bind("127.0.0.1:0").await.unwrap(),
    };
    let (rtp, _rtcp) = pair().await;
    let id = format!("\"{CALL}\"");
    let destination = target
        .map(|s| format!("\"{s}\""))
        .unwrap_or_else(|| format!("\"sip:peer@{}\"", peer.local_addr().unwrap()));
    let params = args(
        &id,
        &destination,
        if target.is_some() {
            "null"
        } else {
            r#""fixture-user""#
        },
        if target.is_some() {
            "null"
        } else {
            r#""fixture-only""#
        },
        "5",
        "false",
    );
    let original_call = owner.find(CALL).unwrap();
    let media = original_call.media().unwrap();
    let local_media = media.observation().receive.transport.local_rtp;
    let feature_params = format!(r#"{{"callId":"{CALL}","challenge":null}}"#);
    let configured = dispatcher
        .execute(command(
            "call.features",
            &feature_params,
            operation + 4,
            operation + 4,
        ))
        .await;
    assert!(configured.outcome().error_code().is_none());
    let original = dispatcher.clone();
    let request = command("call.dial", &params, operation, operation);
    let pending = tokio::spawn(async move { original.execute(request).await });
    let (invite, remote) = received(&peer).await;
    assert_eq!(invite.method, Method::Invite);
    assert_eq!(remote, owner.observation().endpoint.unwrap());
    assert_eq!(media.observation().receive.transport.local_rtp, local_media);
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let hungup = if scenario == "hangup_before_answer" {
        let r = dispatcher
            .execute(command(
                "call.hangup",
                &selected,
                operation + 1,
                operation + 1,
            ))
            .await;
        value(r.outcome())
    } else {
        IvyJson::Null
    };
    let sdp = format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n",
        rtp.local_addr().unwrap().port()
    );
    let code = if scenario == "refused" { 486 } else { 200 };
    let body = if code == 200 { sdp.as_str() } else { "" };
    let extra = if code == 200 {
        format!(
            "Contact: <sip:peer@{}>\r\nContent-Type: application/sdp\r\n",
            peer.local_addr().unwrap()
        )
    } else {
        String::new()
    };
    crate::sip_host::send(
        &peer,
        remote,
        &crate::sip_host::response(&invite, code, &extra, body),
    )
    .await;
    let (ack, _) = received(&peer).await;
    assert_eq!(ack.method, Method::Ack);
    let answer = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    let call = value(answer.outcome());
    let saved = answer.outcome().result_json().unwrap().to_owned();
    if scenario == "connected" {
        for digit in ['*', '0', '#'] {
            original_call
                .features()
                .digit(digit, ivy_phone_bridge::dtmf::InputSource::SipInfo)
                .unwrap();
        }
    }
    let policy = original_call.features().observation();
    if scenario == "connected" {
        assert_eq!(policy.command, Some("new_voice"));
        assert_eq!(policy.command_sequence, 1);
    }
    let mut ended = None;
    if scenario == "remote_bye" {
        let from = invite.from_header().unwrap();
        let tag = from.tag().unwrap().unwrap().value().to_string();
        let remote_tag = ack
            .to_header()
            .unwrap()
            .tag()
            .unwrap()
            .unwrap()
            .value()
            .to_string();
        let raw = format!(
            "BYE sip:fixture@{remote} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-owned-bye\r\nFrom: <sip:peer@{}>;tag={remote_tag}\r\nTo: <{}>;tag={tag}\r\nCall-ID: {CALL}\r\nCSeq: 2 BYE\r\nContent-Length: 0\r\n\r\n",
            peer.local_addr().unwrap(),
            peer.local_addr().unwrap(),
            from.uri().unwrap()
        );
        for _ in 0..2 {
            peer.send_to(raw.as_bytes(), remote).await.unwrap();
            let mut bytes = [0u8; 4096];
            let (n, source) =
                tokio::time::timeout(Duration::from_secs(4), peer.recv_from(&mut bytes))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(source, remote);
            let response =
                rsipstack::sip::Response::try_from(std::str::from_utf8(&bytes[..n]).unwrap())
                    .unwrap();
            assert_eq!(response.status_code.code(), 200);
        }
        until(|| {
            original_call.observation().phase == ivy_phone_bridge::sip::SipHostCallPhase::LocalEnded
                && media.observation().closed
        })
        .await;
        ended = Some(
            parse(
                &ivy_phone_bridge::native_sip_status::call_json(&original_call).unwrap(),
                65536,
            )
            .unwrap(),
        );
    } else if scenario != "refused" {
        let h = dispatcher
            .execute(command(
                "call.hangup",
                &selected,
                operation + 2,
                operation + 2,
            ))
            .await;
        assert!(h.outcome().error_code().is_none());
        let (bye, bye_remote) = received(&peer).await;
        assert_eq!(bye.method, Method::Bye);
        crate::sip_host::send(
            &peer,
            bye_remote,
            &crate::sip_host::response(&bye, 200, "", ""),
        )
        .await;
    }
    let release = dispatcher
        .execute(command(
            "call.release",
            &selected,
            operation + 3,
            operation + 3,
        ))
        .await;
    assert!(
        release.outcome().error_code().is_none(),
        "{:?}",
        release.outcome().error_code()
    );
    let replay = dispatcher
        .execute(command("call.dial", &params, operation, operation + 1000))
        .await;
    assert_eq!(replay.outcome().result_json(), Some(saved.as_str()));
    assert!(media.observation().closed);
    assert_eq!(media.observation().receive.transport.local_rtp, local_media);
    let mut result = IvyJson::object([
        ("call", call),
        ("hungup", hungup),
        ("policy", parse(&policy.json(), 65536).unwrap()),
        ("inviteHasSdp", (!invite.body.is_empty()).into()),
        ("ackHasSdp", (!ack.body.is_empty()).into()),
        ("requestUri", invite.uri.to_string().into()),
        (
            "fromUri",
            invite
                .from_header()
                .unwrap()
                .uri()
                .unwrap()
                .to_string()
                .into(),
        ),
        (
            "sameCallId",
            (invite.call_id_header().unwrap().value() == CALL
                && ack.call_id_header().unwrap().value() == CALL)
                .into(),
        ),
        ("released", owner.find(CALL).is_none().into()),
    ]);
    if let (Some(ended), IvyJson::Object(fields)) = (ended, &mut result) {
        fields.push(("ended".into(), ended));
    }
    result
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_initial_and_delayed_dial_use_original_media_policy_and_saved_receipts() {
    for mode in ["initial", "delayed"] {
        let (owner, factory, routes, params) = actual_owner(50000, 50998);
        let params = params.replace(
            "\"transport\":\"udp\"",
            &format!("\"transport\":\"udp\",\"outgoingOfferMode\":\"{mode}\""),
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
        let selected = format!(r#"{{"callId":"{CALL}"}}"#);
        let mut steps = vec![("configure".into(), params.clone())];
        for scenario in ["connected", "refused", "hangup_before_answer", "remote_bye"] {
            steps.push(("call.prepare".into(), selected.clone()));
            steps.push((
                "fixture.outgoing".into(),
                format!(r#"{{"callId":"{CALL}","scenario":"{scenario}"}}"#),
            ));
        }
        let original = original_raw(&steps);
        let mut rows = Vec::new();
        for (i, (m, p)) in steps.iter().enumerate() {
            let result = if m == "fixture.outgoing" {
                outgoing(
                    &owner,
                    dispatcher.clone(),
                    &parse(p, 65536)
                        .unwrap()
                        .get("scenario")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                    100 + 10 * i as u64,
                    None,
                    None,
                )
                .await
            } else {
                let r = dispatcher
                    .execute(command(m, p, i as u64 + 1, i as u64 + 1))
                    .await;
                value(r.outcome())
            };
            rows.push(IvyJson::object([(
                "json",
                stringify(&result, 65536).unwrap().into(),
            )]));
        }
        trace(
            &format!("native-dial-{mode}"),
            IvyJson::object([
                ("rust", IvyJson::Array(rows.clone())),
                ("original", original.clone().unwrap_or(IvyJson::Null)),
                ("savedReceiptReplayedAfterEachRelease", true.into()),
            ]),
        );
        compare(&rows, &original, &steps);
        assert_eq!(factory.starts.load(Ordering::SeqCst), 4);
        assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
        assert!(remaining.methods.lock().unwrap().is_empty());
        handler.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_descriptor_dispatch_failures_keep_attempted_original_receipts() {
    for mode in ["initial", "delayed"] {
        let (owner, _, routes, params) = actual_owner(49000, 49998);
        let params = params.replace(
            "\"transport\":\"udp\"",
            &format!("\"transport\":\"udp\",\"outgoingOfferMode\":\"{mode}\""),
        );
        let remaining = Arc::new(Remaining::default());
        remaining.close_gate.release();
        let handler = Arc::new(NativeSipCommands::new(
            owner.clone(),
            remaining.clone(),
            |_| Box::pin(async { Ok(()) }),
            closed_platform_views(),
        ));
        let id = format!("\"{CALL}\"");
        let selected = format!(r#"{{"callId":"{CALL}"}}"#);
        let mut steps = vec![("configure".into(), params)];
        for destination in ["sip:", "sips:", "sip:alice@", "sip:@"] {
            steps.push(("call.prepare".into(), selected.clone()));
            steps.push((
                "call.dial".into(),
                args(
                    &id,
                    &format!("\"{destination}\""),
                    "null",
                    "null",
                    "1",
                    "false",
                ),
            ));
            steps.push(("call.release".into(), selected.clone()));
        }
        let original = original_raw(&steps);
        let dispatcher = NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap();
        let mut rows = Vec::new();
        let mut saved = Vec::new();
        for (i, (m, p)) in steps.iter().enumerate() {
            let r = dispatcher
                .execute(command(m, p, i as u64 + 1, i as u64 + 1))
                .await;
            if m == "call.dial" {
                let call = owner.find(CALL).unwrap();
                assert!(call.observation().attempted && call.observation().ending);
                assert_eq!(call.signalling().unwrap().sent, 0);
                saved.push((i, r.outcome().result_json().unwrap().to_owned()));
            }
            rows.push(if let Some(e) = r.outcome().error_code() {
                IvyJson::object([("error", e.into())])
            } else {
                IvyJson::object([("json", r.outcome().result_json().unwrap().into())])
            });
        }
        trace(
            &format!("native-dial-dispatch-failure-{mode}"),
            IvyJson::object([
                ("rust", IvyJson::Array(rows.clone())),
                ("original", original.clone().unwrap_or(IvyJson::Null)),
            ]),
        );
        compare(&rows, &original, &steps);
        for (i, snapshot) in saved {
            let r = dispatcher
                .execute(command(
                    &steps[i].0,
                    &steps[i].1,
                    i as u64 + 1,
                    1000 + i as u64,
                ))
                .await;
            assert_eq!(r.outcome().result_json(), Some(snapshot.as_str()));
        }
        assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
        handler.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_telephone_uri_uses_only_the_configured_owned_proxy() {
    for mode in ["initial", "delayed"] {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (owner, factory, routes, params) = actual_owner(48000, 48998);
        let config=params.replace("\"transport\":\"udp\"",&format!("\"transport\":\"udp\",\"outgoingOfferMode\":\"{mode}\",\"outboundProxy\":\"$PROXY\""));
        let inputs = vec![(
            "fixture.proxyDial".into(),
            format!(
                r#"{{"callId":"{CALL}","scenario":"connected","destination":"tel:123","configuration":{config}}}"#
            ),
        )];
        let original = original_raw(&inputs);
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
        let actual = config.replace("$PROXY", &format!("udp:{}", peer.local_addr().unwrap()));
        let r = dispatcher
            .execute(command("configure", &actual, 1, 1))
            .await;
        assert!(r.outcome().error_code().is_none());
        let selected = format!(r#"{{"callId":"{CALL}"}}"#);
        let r = dispatcher
            .execute(command("call.prepare", &selected, 2, 2))
            .await;
        assert!(r.outcome().error_code().is_none());
        let result = outgoing(
            &owner,
            dispatcher.clone(),
            "connected",
            100,
            Some(peer),
            Some("tel:123"),
        )
        .await;
        let rows = vec![IvyJson::object([(
            "json",
            stringify(&result, 65536).unwrap().into(),
        )])];
        trace(
            &format!("native-dial-tel-proxy-{mode}"),
            IvyJson::object([
                ("rust", IvyJson::Array(rows.clone())),
                ("original", original.clone().unwrap_or(IvyJson::Null)),
                ("explicitOwnedProxy", true.into()),
            ]),
        );
        compare(&rows, &original, &inputs);
        assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
        assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
        handler.shutdown().await.unwrap();
    }
}
