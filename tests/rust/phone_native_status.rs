use super::*;
use ivy_phone_bridge::{
    native_sip_status::signalling_json,
    sip::{SipSignallingEvent, SipSignallingObservation},
};
const SECOND: &str = "21111111-1111-4111-8111-111111111111";
fn selected(id: &str) -> String {
    format!(r#"{{"callId":"{id}"}}"#)
}
fn normalize(v: &mut IvyJson) {
    match v {
        IvyJson::Object(fields) => {
            for (key, v) in fields {
                if ["endpoint", "localRtp"].iter().any(|s| key == &(*s).into()) && !v.is_null() {
                    *v = "owned-endpoint".into();
                } else if key == &"at".into() {
                    let s = v.as_str().unwrap();
                    assert!(s.ends_with("+00:00") && s.contains('T'));
                    *v = "observed-utc".into();
                } else {
                    normalize(v);
                }
            }
        }
        IvyJson::Array(values) => {
            for v in values {
                normalize(v);
            }
        }
        _ => (),
    }
}
fn status_corpus(params: String) -> Vec<(String, String)> {
    let a = selected(CALL);
    let b = selected(SECOND);
    let c = selected("31111111-1111-4111-8111-111111111111");
    vec![
        ("status".into(), "{}".into()),
        ("status".into(), r#"{"callId":null}"#.into()),
        ("status".into(), r#"{"callId":42}"#.into()),
        ("heartbeat".into(), "{}".into()),
        ("heartbeat".into(), a.clone()),
        ("status".into(), r#"{"extra":null}"#.into()),
        ("status".into(), r#"{"callId":42,"extra":true}"#.into()),
        ("configure".into(), params),
        ("status".into(), "{}".into()),
        ("heartbeat".into(), "{}".into()),
        ("status".into(), r#"{"callId":null,"extra":true}"#.into()),
        ("status".into(), r#"{"callId":42,"extra":true}"#.into()),
        ("status".into(), r#"{"callId":true}"#.into()),
        ("heartbeat".into(), r#"{"callId":42}"#.into()),
        ("status".into(), r#"{"callId":"missing"}"#.into()),
        ("status".into(), r#"{"callId":""}"#.into()),
        ("call.prepare".into(), a.clone()),
        ("status".into(), "{}".into()),
        ("heartbeat".into(), "{}".into()),
        (
            "call.features".into(),
            format!(r#"{{"callId":"{CALL}","challenge":null}}"#),
        ),
        ("status".into(), a.clone()),
        ("fixture.offer".into(), a.clone()),
        ("status".into(), a.clone()),
        ("call.prepare".into(), b.clone()),
        ("status".into(), "{}".into()),
        ("status".into(), b.clone()),
        (
            "fixture.signal".into(),
            format!(r#"{{"callId":"{CALL}","count":21}}"#),
        ),
        ("status".into(), a.clone()),
        ("status".into(), b.clone()),
        ("call.hangup".into(), a.clone()),
        ("status".into(), "{}".into()),
        ("status".into(), b.clone()),
        ("call.release".into(), a.clone()),
        ("status".into(), "{}".into()),
        ("status".into(), a.clone()),
        ("call.prepare".into(), a.clone()),
        ("status".into(), a.clone()),
        ("heartbeat".into(), "{}".into()),
        ("call.release".into(), b.clone()),
        ("status".into(), "{}".into()),
        ("call.release".into(), a.clone()),
        // Multiple removals also expose the dictionary's LIFO free list.
        ("call.prepare".into(), a.clone()),
        ("call.prepare".into(), b.clone()),
        ("call.prepare".into(), c.clone()),
        ("call.release".into(), a.clone()),
        ("call.release".into(), b.clone()),
        ("status".into(), "{}".into()),
        ("call.prepare".into(), a.clone()),
        ("heartbeat".into(), "{}".into()),
        ("call.prepare".into(), b.clone()),
        ("heartbeat".into(), "{}".into()),
        ("call.release".into(), a),
        ("call.release".into(), b),
        ("call.release".into(), c),
        ("status".into(), "{}".into()),
        ("fixture.dispose".into(), "{}".into()),
        ("status".into(), "{}".into()),
        ("heartbeat".into(), "{}".into()),
        ("status".into(), r#"{"callId":42,"extra":true}"#.into()),
    ]
}
async fn signal(owner: &NativeSipOperations, peer: &UdpSocket, params: &IvyJson) {
    let id = params.get("callId").unwrap().as_str().unwrap();
    let call = owner.find(&id).unwrap();
    let local = owner.observation().endpoint.unwrap();
    let before = call.signalling().unwrap().received;
    for i in 0..params.get("count").unwrap().as_f64().unwrap() as i64 {
        let body = "protected-fixture-body-never-status";
        let raw = format!(
            "SIP/2.0 200 OK\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-status-{i}\r\nFrom: <sip:fixture@localhost>;tag=foreign\r\nTo: <sip:target@localhost>;tag=unselected\r\nCall-ID: {id}\r\nCSeq: {} MESSAGE\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n{body}",
            peer.local_addr().unwrap(),
            100 + i,
            body.len()
        );
        peer.send_to(raw.as_bytes(), local).await.unwrap();
        until(|| call.signalling().unwrap().received >= before + i + 1).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_full_native_status_heartbeat_and_call_views_follow_owned_lifetimes() {
    let (owner, factory, routes, params) = actual_owner(53000, 53998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        remaining.clone(),
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap();
    let steps = status_corpus(params.replace(
        "\"transport\":\"udp\"",
        "\"transport\":\"udp\",\"maxConcurrentCalls\":3",
    ));
    let inputs: Vec<_> = steps
        .iter()
        .map(|(m, p)| {
            IvyJson::object([
                ("method", m.clone().into()),
                ("params", parse(p, 65536).unwrap()),
            ])
        })
        .collect();
    let original =
        original_session_fixture("tests/fixtures/phone-native-status-oracle.ps1", &inputs);
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut rows = Vec::new();
    let mut saved = None;
    for (i, (method, params)) in steps.iter().enumerate() {
        let row = match method.as_str() {
            "fixture.offer" => {
                owner.find(CALL).unwrap().media().unwrap().create_offer();
                IvyJson::object([("json", "{\"offered\":true}".into())])
            }
            "fixture.signal" => {
                signal(&owner, &peer, &parse(params, 65536).unwrap()).await;
                IvyJson::object([("json", "{\"observed\":true}".into())])
            }
            "fixture.dispose" => match handler.shutdown().await {
                Ok(()) => IvyJson::object([("json", "{\"disposed\":true}".into())]),
                Err(_) => IvyJson::object([("error", "operation_outcome_unknown".into())]),
            },
            _ => {
                let reply = dispatcher
                    .execute(command(method, params, i as u64 + 1, i as u64 + 1))
                    .await;
                if let Some(error) = reply.outcome().error_code() {
                    IvyJson::object([("error", error.into())])
                } else {
                    let raw = reply.outcome().result_json().unwrap();
                    assert!(!raw.contains("protected-fixture-body-never-status"));
                    if method == "call.prepare" && saved.is_none() {
                        saved = Some((i, raw.to_owned()));
                    }
                    IvyJson::object([("json", raw.into())])
                }
            }
        };
        rows.push(row);
    }
    trace(
        "native-status-original",
        IvyJson::object([
            ("inputs", IvyJson::Array(inputs)),
            ("rust", IvyJson::Array(rows.clone())),
            ("original", original.clone().unwrap_or(IvyJson::Null)),
        ]),
    );
    if let Some(original) = original {
        for (i, (a, b)) in rows.iter().zip(original.as_array().unwrap()).enumerate() {
            if let Some(a) = a.get("json") {
                let mut av = parse(&a.as_str().unwrap(), 65536).unwrap();
                let mut bv = parse(
                    &b.get("json")
                        .unwrap_or_else(|| panic!("original step{i}: {b:?}"))
                        .as_str()
                        .unwrap(),
                    65536,
                )
                .unwrap();
                normalize(&mut av);
                normalize(&mut bv);
                assert_eq!(av, bv, "native status step{i} {}", steps[i].0);
            } else {
                assert_eq!(a, b, "native status error step{i} {}", steps[i].0);
            }
        }
    }
    let (i, snapshot) = saved.unwrap();
    let replay = dispatcher
        .execute(command(&steps[i].0, &steps[i].1, i as u64 + 1, 1000))
        .await;
    assert_eq!(replay.outcome().result_json(), Some(snapshot.as_str()));
    assert!(remaining.methods.lock().unwrap().is_empty());
    assert_eq!(factory.starts.load(Ordering::SeqCst), 8);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_signalling_integer_timestamps_and_caller_raw_values_avoid_f64() {
    let signed = [9007199254740993_i64, i64::MAX, i64::MIN];
    let inputs: Vec<_> = signed
        .iter()
        .map(|v| {
            IvyJson::object([
                ("method", "fixture.signallingDto".into()),
                ("params", IvyJson::object([("value", v.to_string().into())])),
            ])
        })
        .collect();
    let original =
        original_session_fixture("tests/fixtures/phone-native-status-oracle.ps1", &inputs);
    let at = std::time::UNIX_EPOCH + Duration::from_nanos(1_664_403_200_000_000_100);
    let mut rows = Vec::new();
    for (i, v) in signed.into_iter().enumerate() {
        let view = SipSignallingObservation {
            sent: v,
            received: v,
            events: vec![
                SipSignallingEvent {
                    at,
                    direction: "sent",
                    method: "INVITE".into(),
                    status: None,
                },
                SipSignallingEvent {
                    at,
                    direction: "received",
                    method: "INVITE".into(),
                    status: Some(200),
                },
            ],
        };
        let raw = signalling_json(&view).unwrap();
        assert!(raw.contains(&format!("\"sent\":{v},\"received\":{v},")));
        assert!(raw.contains("2022-09-28T22:13:20.0000001+00:00"));
        if let Some(e) = &original {
            let outer = parse(
                &e.as_array().unwrap()[i]
                    .get("json")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                65536,
            )
            .unwrap();
            let expected = outer.get("json").unwrap().as_str().unwrap();
            assert_eq!(
                parse(&raw, 65536).unwrap(),
                parse(&expected, 65536).unwrap()
            );
            assert!(expected.contains(&format!("\"sent\":{v},\"received\":{v},")));
        }
        rows.push(IvyJson::object([("json", raw.into())]));
    }
    trace(
        "native-status-signalling-dto",
        IvyJson::object([
            ("inputs", IvyJson::Array(inputs)),
            ("rust", IvyJson::Array(rows)),
            ("original", original.unwrap_or(IvyJson::Null)),
        ]),
    );
    let (owner, _, routes, params) = actual_owner(54000, 54998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let views = NativeSipViews::new(
        || Ok("{\"state\":\"ready\",\"generation\":9007199254740993,\"error\":null}".into()),
        |call| {
            assert_eq!(call.observation().id, CALL);
            Ok("{\"generation\":9223372036854775807}".into())
        },
    );
    let handler = NativeSipCommands::new(owner, remaining, |_| Box::pin(async { Ok(()) }), views);
    assert!(
        handler
            .invoke(command("configure", &params, 1, 1))
            .await
            .error_code()
            .is_none()
    );
    assert!(
        handler
            .invoke(command("call.prepare", &selected(CALL), 2, 2))
            .await
            .error_code()
            .is_none()
    );
    let raw = handler.invoke(command("status", "{}", 0, 3)).await;
    let raw = raw.result_json().unwrap();
    assert!(raw.contains("\"generation\":9007199254740993"));
    assert!(raw.contains("\"generation\":9223372036854775807"));
    handler.shutdown().await.unwrap();
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_status_keeps_selected_original_after_release_and_same_id_reuse() {
    let (owner, factory, routes, params) = actual_owner(55000, 55998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = NativeSipCommands::new(
        owner.clone(),
        remaining,
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    );
    let params = params.replace(
        "\"transport\":\"udp\"",
        "\"transport\":\"udp\",\"maxConcurrentCalls\":3",
    );
    handler.invoke(command("configure", &params, 1, 1)).await;
    handler
        .invoke(command("call.prepare", &selected(CALL), 2, 2))
        .await;
    let old = owner.find(CALL).unwrap();
    let snapshot = handler.invoke(command("status", "{}", 0, 3));
    handler
        .invoke(command("call.release", &selected(CALL), 3, 4))
        .await;
    handler
        .invoke(command("call.prepare", &selected(CALL), 4, 5))
        .await;
    handler
        .invoke(command("call.prepare", &selected(SECOND), 5, 6))
        .await;
    let old_status = snapshot.await;
    let old_status = value(&old_status);
    let current = handler.invoke(command("status", "{}", 0, 7)).await;
    let current = value(&current);
    assert_eq!(
        old_status
            .get("media")
            .unwrap()
            .get("closed")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert_eq!(
        current
            .get("media")
            .unwrap()
            .get("closed")
            .unwrap()
            .as_bool(),
        Some(false)
    );
    assert_eq!(
        old_status.get("callIds").unwrap().as_array().unwrap().len(),
        1
    );
    assert_eq!(current.get("callIds").unwrap().as_array().unwrap().len(), 2);
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let before = old.signalling().unwrap();
    signal(
        &owner,
        &peer,
        &IvyJson::object([("callId", CALL.into()), ("count", n(1))]),
    )
    .await;
    assert_eq!(old.signalling().unwrap(), before);
    assert_eq!(owner.find(CALL).unwrap().signalling().unwrap().received, 1);
    handler.shutdown().await.unwrap();
    assert_eq!(factory.starts.load(Ordering::SeqCst), 3);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    trace(
        "native-status-original-identity",
        IvyJson::object([
            ("originalClosed", true.into()),
            ("replacementOpen", true.into()),
            ("capturedCallIds", n(1)),
            ("currentCallIds", n(2)),
            ("retiredSignallingUnchanged", true.into()),
            ("physicalAudioOpens", n(0)),
        ]),
    );
}
