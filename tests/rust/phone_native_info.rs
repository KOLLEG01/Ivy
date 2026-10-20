use super::*;
use ivy_phone_bridge::native_sip::NativeSipCall;
use rsipstack::sip::{Method, Request, Response, headers::To, prelude::*};
use std::net::SocketAddr;

fn case(
    phase: &str,
    label: &str,
    sequence: i32,
    content_type: Option<&str>,
    body: &str,
    reply: bool,
) -> IvyJson {
    IvyJson::object([
        ("phase", phase.into()),
        ("label", label.into()),
        ("sequence", IvyJson::Number(sequence.into())),
        (
            "contentType",
            content_type.map_or(IvyJson::Null, Into::into),
        ),
        ("body", body.into()),
        ("reply", reply.into()),
    ])
}
fn cases(challenge: bool) -> Vec<IvyJson> {
    let plain = Some("application/dtmf");
    let relay = Some("application/dtmf-relay");
    let mut cases = vec![];
    for (phase, base, body) in [
        ("beforeAnswer", 1, "*0#"),
        ("beforeAck", 10, if challenge { "*1" } else { "*0#" }),
    ] {
        for (i, key) in body.chars().enumerate() {
            cases.push(case(
                phase,
                &format!("{phase}-{i}"),
                base + i as i32,
                plain,
                &key.to_string(),
                false,
            ));
        }
    }
    if challenge {
        for (i, key) in "*123#".chars().enumerate() {
            cases.push(case(
                "connected",
                &format!("auth-{i}"),
                20 + i as i32,
                plain,
                &key.to_string(),
                true,
            ));
        }
    }
    for (i, key) in "*0#".chars().enumerate() {
        cases.push(case(
            "connected",
            &format!("voice-{i}"),
            30 + i as i32,
            plain,
            &key.to_string(),
            true,
        ));
    }
    for (label, sequence, body) in [
        ("duplicate-changed-body", 30, "*"),
        ("duplicate-not-started-zero", 33, "0"),
        ("duplicate-not-started-hash", 34, "#"),
    ] {
        cases.push(case("connected", label, sequence, plain, body, true));
    }
    cases.push(case(
        "connected",
        "invalid-not-consumed",
        50,
        relay,
        "Signal=12",
        false,
    ));
    for (i, key) in "*0#".chars().enumerate() {
        cases.push(case(
            "connected",
            &format!("retry-valid-{i}"),
            50 + i as i32,
            plain,
            &key.to_string(),
            true,
        ));
    }
    for mutation in ["wire", "local", "remote", "missingLocal", "missingRemote"] {
        let mut c = case(
            "connected",
            &format!("foreign-{mutation}"),
            80,
            plain,
            "*",
            false,
        );
        if let IvyJson::Object(fields) = &mut c {
            fields.push(("mutation".into(), mutation.into()));
        }
        cases.push(c);
    }
    let boundary = format!("Signal=A\n{}", " ".repeat(1015));
    let unicode = format!("Signal=A\n{} ", "😀".repeat(507));
    for (i, (label, content_type, body, reply)) in [
        ("unsupported", Some("text/plain"), "*".into(), false),
        ("missing-content-type", None, "*".into(), false),
        ("empty", plain, String::new(), false),
        ("only-whitespace", plain, " \r\n\t".into(), false),
        ("multiple-digits", plain, "12".into(), false),
        (
            "lowercase-digit",
            Some("APPLICATION/DTMF; charset=utf-8"),
            "d".into(),
            true,
        ),
        (
            "plain-unicode-space",
            plain,
            "\u{2000}a\u{0085}".into(),
            true,
        ),
        ("plain-bom-not-space", plain, "\u{feff}A".into(), false),
        (
            "relay-colon",
            relay,
            " \tSignal : a\r\nDuration=160".into(),
            true,
        ),
        (
            "relay-case",
            Some("APPLICATION/DTMF-RELAY; x=y"),
            "siGnaL=b\n".into(),
            true,
        ),
        (
            "relay-cross-newline",
            relay,
            "\r\n \tSignal\n=\n C\nother".into(),
            true,
        ),
        (
            "relay-first-valid",
            relay,
            "junk\nSignal=D\nSignal=1".into(),
            true,
        ),
        (
            "relay-invalid-first",
            relay,
            "Signal=12\nSignal=A".into(),
            true,
        ),
        ("relay-trailing-junk", relay, "Signal=1 junk".into(), false),
        ("relay-cr-not-anchor", relay, "junk\rSignal=A".into(), false),
        (
            "relay-nel-not-anchor",
            relay,
            "junk\u{0085}Signal=A".into(),
            false,
        ),
        (
            "relay-nel-whitespace",
            relay,
            "\u{0085}Signal = D\u{0085}".into(),
            true,
        ),
        ("relay-newline-junk", relay, "Signal=A\njunk".into(), true),
        ("relay-leading-zero", relay, "Signal=01".into(), false),
        ("relay-bad-digit", relay, "Signal=E".into(), false),
        ("relay-bad-name", relay, "S i g n a l = 1".into(), false),
        ("body-1024", relay, boundary.clone(), true),
        ("body-1025", relay, boundary + " ", false),
        ("utf16-1024", relay, unicode.clone(), true),
        ("utf16-1025", relay, unicode + " ", false),
        ("plain-nul", plain, "\0A".into(), false),
        ("relay-nul-after-line", relay, "Signal=A\n\0".into(), true),
    ]
    .into_iter()
    .enumerate()
    {
        cases.push(case(
            "connected",
            label,
            200 + i as i32,
            content_type,
            &body,
            reply,
        ));
    }
    for sequence in [0, i32::MAX] {
        cases.push(case(
            "connected",
            &format!("sequence-{sequence}"),
            sequence,
            plain,
            "A",
            true,
        ));
    }
    for (i, key) in "*0#".chars().enumerate() {
        cases.push(case(
            "connected",
            &format!("fifo-original-{i}"),
            4000 + i as i32,
            plain,
            &key.to_string(),
            true,
        ));
    }
    for i in 0..60 {
        cases.push(case(
            "connected",
            &format!("fifo-fill-{i}"),
            1000 + i,
            plain,
            "A",
            true,
        ));
    }
    cases.push(case(
        "connected",
        "fifo-duplicate-does-not-refresh",
        4000,
        plain,
        "*",
        true,
    ));
    for i in 60..64 {
        cases.push(case(
            "connected",
            &format!("fifo-fill-{i}"),
            1000 + i,
            plain,
            "A",
            true,
        ));
    }
    for (i, key) in "*0#".chars().enumerate() {
        cases.push(case(
            "connected",
            &format!("fifo-reused-{i}"),
            4000 + i as i32,
            plain,
            &key.to_string(),
            true,
        ));
    }
    for (i, key) in "**0#".chars().enumerate() {
        let mut c = case(
            "connected",
            &format!("wire-retransmit-{i}"),
            6000 + if i == 0 { 0 } else { i as i32 - 1 },
            plain,
            &key.to_string(),
            true,
        );
        if i < 2 {
            if let IvyJson::Object(fields) = &mut c {
                fields.push(("branch".into(), "z9hG4bK-owned-info-retransmit".into()));
            }
        }
        cases.push(c);
    }
    for (phase, base) in [
        ("ended", 5000),
        ("released", 7000),
        ("replacementOld", 8000),
        ("replacementNew", 9000),
    ] {
        for (i, key) in "*0#".chars().enumerate() {
            cases.push(case(
                phase,
                &format!("{phase}-{i}"),
                base + i as i32,
                plain,
                &key.to_string(),
                phase == "replacementNew",
            ));
        }
    }
    cases
}
struct Identity {
    wire: String,
    local: String,
    remote: String,
}
struct Peer {
    socket: UdpSocket,
    target: SocketAddr,
    branch: usize,
    cases: Vec<IvyJson>,
}
impl Peer {
    fn raw(
        &self,
        method: &str,
        sequence: i32,
        body: &str,
        content_type: Option<&str>,
        id: &Identity,
        branch: &str,
        mutation: &str,
    ) -> String {
        let source = self.socket.local_addr().unwrap();
        let wire = if mutation == "wire" {
            "foreign-wire"
        } else {
            &id.wire
        };
        let remote = if mutation == "remote" {
            "foreign-remote"
        } else {
            &id.remote
        };
        let local = if mutation == "local" {
            "foreign-local"
        } else {
            &id.local
        };
        format!(
            "{method} sip:fixture@{} SIP/2.0\r\nVia: SIP/2.0/UDP {source};rport;branch={branch}\r\nFrom: <sip:caller@{source}>{}\r\nTo: <sip:fixture@{}>{}\r\nCall-ID: {wire}\r\nCSeq: {sequence} {method}\r\nContact: <sip:caller@{source}>\r\nMax-Forwards: 70\r\n{}Content-Length: {}\r\n\r\n{body}",
            self.target,
            if mutation == "missingRemote" {
                String::new()
            } else {
                format!(";tag={remote}")
            },
            self.target,
            if mutation == "missingLocal" || local.is_empty() {
                String::new()
            } else {
                format!(";tag={local}")
            },
            content_type.map_or(String::new(), |ct| format!("Content-Type: {ct}\r\n")),
            body.len()
        )
    }
    async fn send(&self, raw: &str) {
        self.socket
            .send_to(raw.as_bytes(), self.target)
            .await
            .unwrap();
    }
    async fn cases(&mut self, phase: &str, id: &Identity, call: &NativeSipCall) -> Vec<IvyJson> {
        let mut rows = vec![];
        for case in self
            .cases
            .iter()
            .filter(|c| c.get("phase").unwrap().as_str().unwrap() == phase)
        {
            let label = case.get("label").unwrap().as_str().unwrap();
            let seq = case.get("sequence").unwrap().as_f64().unwrap() as i32;
            let ct = case.get("contentType").unwrap();
            let ct = if ct.is_null() {
                None
            } else {
                Some(ct.as_str().unwrap())
            };
            self.branch += 1;
            let branch = case
                .get("branch")
                .map(|b| b.as_str().unwrap())
                .unwrap_or_else(|| format!("z9hG4bK-owned-info-{}", self.branch));
            let mutation = case
                .get("mutation")
                .map(|m| m.as_str().unwrap())
                .unwrap_or_default();
            self.send(&self.raw(
                "INFO",
                seq,
                &case.get("body").unwrap().as_str().unwrap(),
                ct.as_deref(),
                id,
                &branch,
                &mutation,
            ))
            .await;
            let response = tokio::time::timeout(
                Duration::from_millis(if case.get("reply").unwrap().as_bool().unwrap() {
                    4000
                } else {
                    150
                }),
                async {
                    let mut bytes = [0; 8192];
                    loop {
                        let (n, _) = self.socket.recv_from(&mut bytes).await.unwrap();
                        let response =
                            Response::try_from(std::str::from_utf8(&bytes[..n]).unwrap()).unwrap();
                        if response.cseq_header().unwrap().method().unwrap() == Method::Info
                            && response
                                .cseq_header()
                                .unwrap()
                                .value()
                                .split_whitespace()
                                .next()
                                .unwrap()
                                .parse::<i32>()
                                .unwrap()
                                == seq
                        {
                            break response.status_code.code();
                        }
                    }
                },
            )
            .await
            .ok();
            rows.push(IvyJson::object([
                ("label", label.into()),
                (
                    "status",
                    response.map_or(IvyJson::Null, |s| IvyJson::Number(s.into())),
                ),
                (
                    "features",
                    parse(&call.features().observation().json(), 65536).unwrap(),
                ),
            ]));
        }
        rows
    }
}
async fn outgoing(
    owner: &NativeSipOperations,
    dispatcher: Arc<NativeDispatcher>,
    peer: &mut Peer,
    id: &str,
    sdp: &str,
    first: bool,
    rows: &mut Vec<IvyJson>,
    op: u64,
) -> (NativeSipCall, Identity) {
    let call = owner.prepare(id.into()).unwrap();
    call.prepared().await.unwrap();
    let features = format!(r#"{{"callId":"{id}","challenge":null}}"#);
    assert!(
        dispatcher
            .execute(command("call.features", &features, op, op))
            .await
            .outcome()
            .error_code()
            .is_none()
    );
    let params = format!(
        r#"{{"callId":"{id}","destination":"sip:peer@{}","username":"fixture-user","password":"fixture-only","ringSeconds":5,"waiting":false}}"#,
        peer.socket.local_addr().unwrap()
    );
    let pending = tokio::spawn(async move {
        dispatcher
            .execute(command("call.dial", &params, op + 1, op + 1))
            .await
    });
    let (invite, _) = crate::sip_host::received(&peer.socket).await;
    assert_eq!(invite.method, Method::Invite);
    let identity = Identity {
        wire: id.into(),
        local: invite
            .from_header()
            .unwrap()
            .tag()
            .unwrap()
            .unwrap()
            .value()
            .into(),
        remote: "owned-peer".into(),
    };
    if first {
        rows.extend(peer.cases("beforeAnswer", &identity, &call).await);
    }
    let mut response = crate::sip_host::response(
        &invite,
        200,
        &format!(
            "Contact: <sip:peer@{}>\r\nContent-Type: application/sdp\r\n",
            peer.socket.local_addr().unwrap()
        ),
        sdp,
    );
    response.headers.unique_push(
        To::new(format!(
            "{};tag=owned-peer",
            invite.to_header().unwrap().value()
        ))
        .into(),
    );
    peer.send(&response.to_string()).await;
    assert_eq!(
        crate::sip_host::received(&peer.socket).await.0.method,
        Method::Ack
    );
    assert!(pending.await.unwrap().outcome().error_code().is_none());
    (call, identity)
}
async fn end(owner: &NativeSipOperations, peer: &Peer, call: &NativeSipCall) {
    owner.hangup(call).unwrap();
    let (bye, _) = crate::sip_host::received(&peer.socket).await;
    assert_eq!(bye.method, Method::Bye);
    peer.send(&crate::sip_host::response(&bye, 200, "", "").to_string())
        .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_sip_info_original_dialog_lifetime_grammar_fifo_and_feature_authority() {
    for (incoming, delayed, challenge) in [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, false),
        (true, false, true),
        (true, true, true),
    ] {
        let name = format!(
            "{}-{}-{}",
            if incoming { "incoming" } else { "outgoing" },
            if delayed { "delayed" } else { "initial" },
            if challenge { "challenge" } else { "trusted" }
        );
        let (owner, factory, routes, config) = actual_owner(40000, 41998);
        let config = config.replace(
            r#""transport":"udp""#,
            &format!(
                r#""transport":"udp","outgoingOfferMode":"{}""#,
                if delayed { "delayed" } else { "initial" }
            ),
        );
        let corpus = cases(challenge);
        let challenge_json = if challenge {
            parse(r#"{"code":"*123#","timeoutSeconds":30,"maxFailedAttempts":3,"failedAttemptWindowSeconds":60}"#,65536).unwrap()
        } else {
            IvyJson::Null
        };
        let fixture = IvyJson::object([
            ("incoming", incoming.into()),
            ("delayed", delayed.into()),
            ("callId", CALL.into()),
            ("configuration", parse(&config, 65536).unwrap()),
            ("challenge", challenge_json.clone()),
            ("cases", IvyJson::Array(corpus.clone())),
        ]);
        let original = super::dial::original_raw(&[(
            "fixture.info".into(),
            stringify(&fixture, 262144).unwrap(),
        )]);
        trace(
            &format!("native-info-original-{name}"),
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
        let mut peer = Peer {
            socket: UdpSocket::bind("127.0.0.1:0").await.unwrap(),
            target: owner.observation().endpoint.unwrap(),
            branch: 0,
            cases: corpus,
        };
        let (rtp, _rtcp) = pair().await;
        let sdp = format!(
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n",
            rtp.local_addr().unwrap().port()
        );
        let mut rows = vec![];
        let (call, identity) = if incoming {
            let mut identity = Identity {
                wire: "owned-native-info".into(),
                local: String::new(),
                remote: "caller-tag".into(),
            };
            peer.send(&peer.raw(
                "INVITE",
                1,
                if delayed { "" } else { &sdp },
                Some("application/sdp"),
                &identity,
                "z9hG4bK-owned-info-invite",
                "",
            ))
            .await;
            let ring = super::incoming::receive(&peer.socket, 4000).await.unwrap();
            assert_eq!(ring.status_code.code(), 180);
            identity.local = ring
                .to_header()
                .unwrap()
                .tag()
                .unwrap()
                .unwrap()
                .value()
                .into();
            until(|| !owner.observation().calls.is_empty()).await;
            let id = owner.observation().calls[0].id.clone();
            let call = owner.find(&id).unwrap();
            call.prepared().await.unwrap();
            owner.claim(&call).unwrap();
            let features = stringify(
                &IvyJson::object([("callId", id.clone().into()), ("challenge", challenge_json)]),
                65536,
            )
            .unwrap();
            assert!(
                dispatcher
                    .execute(command("call.features", &features, 2, 2))
                    .await
                    .outcome()
                    .error_code()
                    .is_none()
            );
            rows.extend(peer.cases("beforeAnswer", &identity, &call).await);
            let params = format!(r#"{{"callId":"{id}","waiting":false}}"#);
            let dispatch = dispatcher.clone();
            let mut pending = Some(tokio::spawn(async move {
                dispatch
                    .execute(command("call.answer", &params, 3, 3))
                    .await
            }));
            let final_response = super::incoming::receive(&peer.socket, 4000).await.unwrap();
            assert_eq!(final_response.status_code.code(), 200);
            identity.local = final_response
                .to_header()
                .unwrap()
                .tag()
                .unwrap()
                .unwrap()
                .value()
                .into();
            if !delayed {
                assert!(
                    pending
                        .take()
                        .unwrap()
                        .await
                        .unwrap()
                        .outcome()
                        .error_code()
                        .is_none()
                );
            }
            rows.extend(peer.cases("beforeAck", &identity, &call).await);
            peer.send(&peer.raw(
                "ACK",
                1,
                if delayed { &sdp } else { "" },
                if delayed {
                    Some("application/sdp")
                } else {
                    None
                },
                &identity,
                "z9hG4bK-owned-info-ack",
                "",
            ))
            .await;
            if let Some(pending) = pending {
                assert!(pending.await.unwrap().outcome().error_code().is_none());
            }
            (call, identity)
        } else {
            outgoing(
                &owner,
                dispatcher.clone(),
                &mut peer,
                CALL,
                &sdp,
                true,
                &mut rows,
                10,
            )
            .await
        };
        let media = call.media().unwrap();
        let local_rtp = media.observation().receive.transport.local_rtp;
        rows.extend(peer.cases("connected", &identity, &call).await);
        for label in [
            "body-1024",
            "utf16-1024",
            "relay-cross-newline",
            "relay-nel-whitespace",
        ] {
            assert_eq!(
                rows.iter()
                    .find(|r| r.get("label").unwrap().as_str().unwrap() == label)
                    .unwrap()
                    .get("status")
                    .unwrap()
                    .as_f64(),
                Some(200.0),
                "{name}/{label}"
            );
        }
        for label in [
            "body-1025",
            "utf16-1025",
            "plain-bom-not-space",
            "relay-cr-not-anchor",
            "relay-nel-not-anchor",
        ] {
            assert!(
                rows.iter()
                    .find(|r| r.get("label").unwrap().as_str().unwrap() == label)
                    .unwrap()
                    .get("status")
                    .unwrap()
                    .is_null(),
                "{name}/{label}"
            );
        }
        assert_eq!(
            call.features().observation().access,
            if challenge {
                "authenticated"
            } else {
                "trusted"
            }
        );
        end(&owner, &peer, &call).await;
        rows.extend(peer.cases("ended", &identity, &call).await);
        let id = call.observation().id.clone();
        owner.release(&call).unwrap().wait().await.unwrap();
        rows.extend(peer.cases("released", &identity, &call).await);
        let (fresh, fresh_identity) = outgoing(
            &owner,
            dispatcher.clone(),
            &mut peer,
            &id,
            &sdp,
            false,
            &mut rows,
            100,
        )
        .await;
        rows.extend(peer.cases("replacementOld", &identity, &fresh).await);
        assert_eq!(fresh.features().observation().command_sequence, 0);
        rows.extend(peer.cases("replacementNew", &fresh_identity, &fresh).await);
        assert_eq!(fresh.features().observation().command_sequence, 1);
        end(&owner, &peer, &fresh).await;
        owner.release(&fresh).unwrap().wait().await.unwrap();
        handler.shutdown().await.unwrap();
        assert!(media.observation().closed && media.observation().cleanup_confirmed);
        assert_eq!(media.observation().receive.transport.local_rtp, local_rtp);
        assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
        assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
        assert!(remaining.methods.lock().unwrap().is_empty());
        let result = IvyJson::object([
            ("rows", IvyJson::Array(rows)),
            ("released", owner.find(&id).is_none().into()),
            ("originalMediaStable", true.into()),
            ("physicalAudioOpens", n(0)),
        ]);
        trace(
            &format!("native-info-{name}"),
            IvyJson::object([
                ("rust", result.clone()),
                ("original", original.clone().unwrap_or(IvyJson::Null)),
                ("cases", fixture.get("cases").unwrap().clone()),
            ]),
        );
        if let Some(original) = original {
            let expected = parse(
                &original.as_array().unwrap()[0]
                    .get("json")
                    .unwrap_or_else(|| panic!("Original INFO unavailable: {original:?}"))
                    .as_str()
                    .unwrap(),
                262144,
            )
            .unwrap();
            assert_eq!(result, expected, "{name}");
        }
        drop(
            UdpSocket::bind(peer.target)
                .await
                .expect("Original INFO binding released"),
        );
    }
}
