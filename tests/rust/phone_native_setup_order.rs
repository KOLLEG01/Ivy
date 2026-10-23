use super::lifecycle_boundaries::{Caller, micro_json};
use super::*;
use ivy_phone_bridge::{
    desktop::{MicroFactoryError, VoiceInputOwner},
    native_rpc::NativeReply,
};
use rsipstack::sip::{Method, Response, prelude::*};
use std::{error::Error, fmt, net::SocketAddr};

#[derive(Debug)]
struct Argument;
impl fmt::Display for Argument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("fixture argument")
    }
}
impl Error for Argument {}
fn micro_params(base: &str) -> String {
    [base.trim_end_matches('}'), r#", "micro":{"usbipExecutable":"C:/fixture/usbip.exe","executableHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000","port":3240}}"#].concat()
}
fn safe(reply: &NativeReply) -> IvyJson {
    reply
        .outcome()
        .error_code()
        .map(|e| IvyJson::object([("error", e.into())]))
        .unwrap_or_else(|| IvyJson::object([("ok", true.into())]))
}
fn project(method: &str, reply: &NativeReply) -> IvyJson {
    if reply.outcome().error_code().is_some() {
        return safe(reply);
    }
    let value = value(reply.outcome());
    match method {
        "configure" => IvyJson::object([("configured", value.get("configured").unwrap().clone())]),
        "call.prepare" => IvyJson::object([
            ("id", value.get("id").unwrap().clone()),
            ("state", value.get("state").unwrap().clone()),
        ]),
        _ => value,
    }
}
fn configured_owner(
    mode: usize,
) -> (
    NativeSipOperations,
    Arc<NativeSipCommands>,
    Arc<NativeDispatcher>,
    Arc<CaptureGate>,
    Arc<AtomicUsize>,
    Arc<Factory>,
    String,
) {
    let (owner, _, routes, base) = actual_owner(47000, 47006);
    let gate = Arc::new(CaptureGate::default());
    let factories = Arc::new(AtomicUsize::new(0));
    let count = factories.clone();
    let entered = gate.clone();
    let input = VoiceInputOwner::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        entered.block();
        match mode {
            4 => Err(MicroFactoryError::Other(Box::new(Argument))),
            5 => Err(MicroFactoryError::Other(
                std::io::Error::other("fixture factory").into(),
            )),
            _ => Ok(None),
        }
    });
    let caller = Arc::new(Caller {
        input: input.clone(),
        closes: AtomicUsize::new(0),
    });
    let status = input.clone();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        caller,
        move |micro| {
            let input = input.clone();
            Box::pin(async move {
                // This controlled synchronous factory must not occupy the SIP
                // reactor's worker while the network-arrival boundary is tested.
                tokio::task::spawn_blocking(move || input.prepare_micro(micro.unwrap()))
                    .await
                    .map_err(|_| SipHostError::CleanupUnconfirmed)?
                    .map_err(|e| {
                        if e.is::<Argument>() {
                            SipHostError::InvalidRequest
                        } else {
                            SipHostError::CleanupUnconfirmed
                        }
                    })
            })
        },
        NativeSipViews::new(move || micro_json(&status), |_| Ok("null".into())),
    ));
    let dispatcher = Arc::new(NativeDispatcher::new(CALL.into(), 64, handler.clone()).unwrap());
    (
        owner,
        handler,
        dispatcher,
        gate,
        factories,
        routes,
        micro_params(&base),
    )
}
fn released(endpoint: SocketAddr) -> bool {
    std::net::UdpSocket::bind(endpoint).is_ok()
}
async fn receive(peer: &UdpSocket) -> Response {
    let mut bytes = [0u8; 8192];
    loop {
        let (n, _) = timeout(Duration::from_secs(4), peer.recv_from(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        let response = Response::try_from(std::str::from_utf8(&bytes[..n]).unwrap()).unwrap();
        if response.status_code.code() != 100 {
            return response;
        }
    }
}
fn raw(method: &str, source: SocketAddr, target: SocketAddr, sequence: u32) -> String {
    format!(
        "{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {source};rport;branch=z9hG4bK-setup-{sequence}\r\nFrom: <sip:caller@{source}>;tag=setup-source\r\nTo: <sip:fixture@{target}>\r\nCall-ID: owned-setup-{sequence}\r\nCSeq: 1 {method}\r\nContact: <sip:caller@{source}>\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n"
    )
}
async fn latch(mode: usize) -> (IvyJson, IvyJson) {
    let (owner, handler, dispatcher, gate, factories, routes, base) = configured_owner(mode);
    let registrar = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let port = registrar.local_addr().unwrap().port();
    let registration = format!(
        r#"{{"accountUri":"sip:registered-user@127.0.0.1","registrarUri":"sip:127.0.0.1:{port}","contactUser":"contact-user","authUsername":"digest-user","realm":null,"expirySeconds":60,"retrySeconds":600,"keepAliveIntervalSeconds":0}}"#
    );
    let params = base.replace(
        r#""registration":null,"password":null"#,
        &format!(r#""registration":{registration},"password":"fixture-only""#),
    );
    let registers = Arc::new(AtomicUsize::new(0));
    let count = registers.clone();
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    let listening = registrar.clone();
    let registration_loop = tokio::spawn(async move {
        loop {
            let mut bytes = [0u8; 8192];
            let received = tokio::select! { _=stopped.cancelled()=>break, received=listening.recv_from(&mut bytes)=>received.unwrap() };
            let request = rsipstack::sip::Request::try_from(
                std::str::from_utf8(&bytes[..received.0]).unwrap(),
            )
            .unwrap();
            assert_eq!(request.method, Method::Register);
            count.fetch_add(1, Ordering::SeqCst);
            crate::sip_host::send(
                &listening,
                received.1,
                &crate::sip_host::response(&request, 403, "", ""),
            )
            .await;
        }
    });
    cancel(dispatcher.execute(command("configure", &params, 1, 1))).await;
    until(|| gate.entered.load(Ordering::SeqCst)).await;
    let before = owner.configuration().unwrap().has_registration();
    let pending = owner.configuration_result().is_none();
    let endpoint = owner.observation().endpoint.unwrap();
    gate.release();
    let configured = dispatcher
        .execute(command("configure", &params, 1, 2))
        .await;
    let replay = dispatcher
        .execute(command("configure", &params, 1, 3))
        .await;
    let reached = owner.configuration().unwrap().has_registration();
    let owned = owner.observation().registration.is_some();
    if reached {
        until(|| registers.load(Ordering::SeqCst) >= 1).await;
    }
    let reconnect = dispatcher
        .execute(command("registration.reconnect", "{}", 2, 4))
        .await;
    if reached {
        until(|| registers.load(Ordering::SeqCst) >= 2).await;
    }
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let prepared = dispatcher
        .execute(command("call.prepare", &selected, 3, 5))
        .await;
    assert_eq!(prepared.outcome().error_code(), None);
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let dial_args = format!(
        r#"{{"callId":"{CALL}","destination":"sip:peer@{}","username":"caller-user","password":null,"ringSeconds":5,"waiting":false}}"#,
        peer.local_addr().unwrap()
    );
    let dial_dispatcher = dispatcher.clone();
    let dialing = tokio::spawn(async move {
        dial_dispatcher
            .execute(command("call.dial", &dial_args, 4, 6))
            .await
    });
    let (invite, remote) = crate::sip_host::received(&peer).await;
    assert_eq!(invite.method, Method::Invite);
    let from = invite.from_header().unwrap().uri().unwrap();
    let from_user = from.auth.as_ref().unwrap().user.clone();
    crate::sip_host::send(
        &peer,
        remote,
        &crate::sip_host::response(&invite, 486, "", ""),
    )
    .await;
    let ended = dialing.await.unwrap();
    let released_call = dispatcher
        .execute(command("call.release", &selected, 5, 7))
        .await;
    handler.shutdown().await.unwrap();
    stop.cancel();
    registration_loop.await.unwrap();
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    assert_eq!(configured.outcome(), replay.outcome());
    assert!(!before && pending);
    let input = IvyJson::object([
        ("kind", "latch".into()),
        ("mode", n(mode)),
        ("registrarPort", n(port.into())),
        ("params", parse(&params, 65536).unwrap()),
    ]);
    let output = IvyJson::object([
        ("kind", "latch".into()),
        ("mode", n(mode)),
        ("settingsBefore", before.into()),
        ("setupPending", pending.into()),
        ("settingsAfter", reached.into()),
        ("registrationOwned", owned.into()),
        (
            "rows",
            IvyJson::Array(vec![
                project("configure", &configured),
                project("configure", &replay),
                safe(&reconnect),
                project("call.prepare", &prepared),
                safe(&ended),
                project("call.release", &released_call),
            ]),
        ),
        ("fromUser", from_user.into()),
        ("registers", n(registers.load(Ordering::SeqCst))),
        ("factories", n(factories.load(Ordering::SeqCst))),
        ("bindingReleased", released(endpoint).into()),
    ]);
    (input, output)
}
async fn receive_order(mode: usize, allowed: bool) -> (IvyJson, IvyJson) {
    let (owner, handler, dispatcher, gate, factories, routes, base) = configured_owner(mode);
    let params = if allowed {
        base
    } else {
        base.replace(r#"["registered","127.0.0.1"]"#, "[]")
    };
    cancel(dispatcher.execute(command("configure", &params, 1, 1))).await;
    until(|| gate.entered.load(Ordering::SeqCst)).await;
    let target = owner.observation().endpoint.unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let source = peer.local_addr().unwrap();
    let mut codes = Vec::new();
    for (index, method) in ["OPTIONS", "MESSAGE", "NOTIFY"].into_iter().enumerate() {
        peer.send_to(
            raw(method, source, target, index as u32 + 1).as_bytes(),
            target,
        )
        .await
        .unwrap();
        codes.push(n(receive(&peer).await.status_code.code().into()));
    }
    peer.send_to(raw("INVITE", source, target, 4).as_bytes(), target)
        .await
        .unwrap();
    let offer = receive(&peer).await;
    codes.push(n(offer.status_code.code().into()));
    let pending = owner.configuration_result().is_none();
    let calls = owner.observation().calls;
    let count = calls.len();
    let before_call = calls.first().map_or(IvyJson::Null, |call| {
        let value = parse(
            &ivy_phone_bridge::native_sip_status::call_json(&owner.find(&call.id).unwrap())
                .unwrap(),
            65536,
        )
        .unwrap();
        IvyJson::object([
            ("direction", value.get("direction").unwrap().clone()),
            ("state", value.get("state").unwrap().clone()),
        ])
    });
    gate.release();
    let configured = dispatcher
        .execute(command("configure", &params, 1, 2))
        .await;
    let mut rows = vec![project("configure", &configured)];
    if allowed {
        assert_eq!(count, 1);
        assert_eq!(offer.status_code.code(), 180);
        let selected = format!(r#"{{"callId":"{}"}}"#, calls[0].id);
        rows.push(safe(
            &dispatcher
                .execute(command("call.claim", &selected, 2, 3))
                .await,
        ));
        rows.push(safe(
            &dispatcher
                .execute(command("call.hangup", &selected, 3, 4))
                .await,
        ));
        let final_response = receive(&peer).await;
        assert_eq!(final_response.status_code.code(), 603);
        let tag = final_response
            .to_header()
            .unwrap()
            .tag()
            .unwrap()
            .unwrap()
            .value()
            .to_string();
        let ack = raw("ACK", source, target, 4).replace(
            &format!("To: <sip:fixture@{target}>"),
            &format!("To: <sip:fixture@{target}>;tag={tag}"),
        );
        peer.send_to(ack.as_bytes(), target).await.unwrap();
        rows.push(safe(
            &dispatcher
                .execute(command("call.release", &selected, 4, 5))
                .await,
        ));
    } else {
        assert_eq!(count, 0);
        assert_eq!(offer.status_code.code(), 403);
    }
    handler.shutdown().await.unwrap();
    assert!(pending);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    (
        IvyJson::object([
            ("kind", "receive".into()),
            ("mode", n(mode)),
            ("allowed", allowed.into()),
            ("params", parse(&params, 65536).unwrap()),
        ]),
        IvyJson::object([
            ("kind", "receive".into()),
            ("mode", n(mode)),
            ("allowed", allowed.into()),
            ("setupPending", pending.into()),
            ("codes", IvyJson::Array(codes)),
            ("beforeCalls", n(count)),
            ("beforeCall", before_call),
            ("rows", IvyJson::Array(rows)),
            ("factories", n(factories.load(Ordering::SeqCst))),
            ("bindingReleased", released(target).into()),
        ]),
    )
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_registration_latch_and_postbind_network_order_agree() {
    let mut inputs = Vec::new();
    let mut actual = Vec::new();
    for mode in [0, 4, 5] {
        let (input, row) = latch(mode).await;
        inputs.push(input);
        actual.push(row);
        trace("setup-order-inputs", IvyJson::Array(inputs.clone()));
        trace("setup-order-rust", IvyJson::Array(actual.clone()));
    }
    for mode in [0, 4, 5] {
        for allowed in [false, true] {
            let (input, row) = receive_order(mode, allowed).await;
            inputs.push(input);
            actual.push(row);
            trace("setup-order-inputs", IvyJson::Array(inputs.clone()));
            trace("setup-order-rust", IvyJson::Array(actual.clone()));
        }
    }
    trace("setup-order-inputs", IvyJson::Array(inputs.clone()));
    trace("setup-order-rust", IvyJson::Array(actual.clone()));
    let normalized_inputs = inputs
        .iter()
        .map(|input| {
            let Some(port) = input.get("registrarPort") else {
                return input.clone();
            };
            let port = port.as_f64().unwrap() as u16;
            assert_ne!(port, 0);
            let raw = stringify(input, 65536).unwrap();
            let uri = format!("\"registrarUri\":\"sip:127.0.0.1:{port}\"");
            assert!(raw.contains(&uri));
            parse(
                &raw.replace(&uri, "\"registrarUri\":\"sip:127.0.0.1:0\"")
                    .replace(&format!("\"registrarPort\":{port}"), "\"registrarPort\":0"),
                65536,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let original = boundary_cases("setup", &normalized_inputs);
    trace("setup-order-original", original.clone());
    assert_eq!(IvyJson::Array(actual), original);
}
