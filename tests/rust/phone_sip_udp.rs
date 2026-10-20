use super::*;
use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::{
        Condvar,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
};

fn trace(name: &str, value: IvyJson) {
    if let Some(path) = std::env::var_os("IVY_PHONE_CALL_AUDIO_TRACE_DIR") {
        std::fs::write(
            std::path::PathBuf::from(path).join(format!("{name}.json")),
            stringify(&value, 8_000_000).unwrap(),
        )
        .unwrap();
    }
}
fn number(value: usize) -> IvyJson {
    IvyJson::Number(value as f64)
}
fn original(inputs: &[IvyJson]) -> Option<IvyJson> {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original UDP oracle unavailable; differential not executed");
        return None;
    }
    let mut child = Command::new("pwsh")
        .args([
            "-NoProfile",
            "-File",
            "tests/fixtures/phone-sip-udp-oracle.ps1",
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
        .write_all(
            stringify(&IvyJson::Array(inputs.to_vec()), 1_000_000)
                .unwrap()
                .as_bytes(),
        )
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
            8_000_000,
        )
        .unwrap(),
    )
}

#[test]
fn shared_failed_destinations_match_original_keys_membership_and_expiry_boundaries() {
    const T: i64 = 10_000_000_000;
    let actions = [
        ("record", T, None),
        ("record", T, Some("127.0.0.1:0")),
        ("record", T, Some("127.0.0.1:21001")),
        ("record", T + 200_000_000, Some("127.0.0.1:21001")),
        ("contains", T + 310_000_000, Some("127.0.0.1:21001")),
        ("sweep", T + 300_000_000, Some("127.0.0.1:21001")),
        ("sweep", T + 300_000_001, Some("127.0.0.1:21001")),
        ("record", T, Some("[::ffff:127.0.0.1]:21001")),
        ("contains", T, Some("127.0.0.1:21001")),
        ("record", T + 50_000_000, Some("127.0.0.1:21002")),
        ("record", T + 50_000_000, Some("[fe80::1%12]:21001")),
        ("contains", T, Some("[fe80::1%13]:21001")),
        ("record", T + 250_000_000, Some("[fe80::1%12]:21001")),
        ("sweep", T - 10_000_000, Some("127.0.0.1:21001")),
        ("snapshot", T + 310_000_000, Some("127.0.0.1:21001")),
        ("remove", T + 310_000_000, Some("127.0.0.1:21001")),
        ("record", T + 310_000_000, Some("127.0.0.1:21001")),
        ("apply", T + 310_000_000, Some("127.0.0.1:21001")),
        ("sweep", T + 350_000_000, Some("127.0.0.1:21002")),
        ("sweep", T + 350_000_001, Some("127.0.0.1:21002")),
    ];
    let time = Arc::new(AtomicI64::new(T));
    let clock: LocalClock = {
        let time = time.clone();
        Arc::new(move || Ok(time.load(Ordering::SeqCst)))
    };
    let cache = FailedDestinations::default();
    let mut expired = Vec::new();
    let mut inputs = Vec::new();
    let mut rows = Vec::new();
    for (op, ticks, peer) in actions {
        time.store(ticks, Ordering::SeqCst);
        let parsed: Option<SocketAddr> = peer.map(|value| value.parse().unwrap());
        match op {
            "record" => cache.receive_error(parsed, &clock).unwrap(),
            "remove" => {
                cache.0.lock().unwrap().remove(&canonical(parsed.unwrap()));
            }
            "snapshot" | "sweep" => expired = cache.expired(&clock).unwrap(),
            _ => {}
        }
        if op == "apply" || op == "sweep" {
            cache.remove(expired.clone());
        }
        let stored = parsed.and_then(|p| cache.0.lock().unwrap().get(&canonical(p)).copied());
        let count = cache.0.lock().unwrap().len();
        rows.push(IvyJson::object([
            ("count", number(count)),
            ("blocked", parsed.is_some_and(|p| cache.contains(p)).into()),
            (
                "stored",
                stored
                    .map(|t| t.to_string().into())
                    .unwrap_or(IvyJson::Null),
            ),
        ]));
        inputs.push(IvyJson::object([
            ("op", op.into()),
            ("ticks", ticks.to_string().into()),
            ("peer", peer.map(Into::into).unwrap_or(IvyJson::Null)),
        ]));
    }
    assert!(Arc::ptr_eq(
        &FailedDestinations::shared(),
        &FailedDestinations::shared()
    ));
    let reference = original(&inputs);
    if let Some(reference) = &reference {
        assert_eq!(
            reference.get("rows").unwrap(),
            &IvyJson::Array(rows.clone())
        );
        let admission = reference.get("admission").unwrap();
        assert_eq!(
            admission.get("codes").unwrap(),
            &IvyJson::Array(vec![
                number(10061),
                number(10061),
                number(10061),
                number(10101),
                "invalid_buffer".into(),
                number(0),
                number(0)
            ])
        );
        assert_eq!(admission.get("sameSocket").unwrap().as_bool(), Some(true));
        let sweep = reference.get("sweep").unwrap();
        for key in ["oldRemoved", "freshRetained", "futureRetained"] {
            assert_eq!(sweep.get(key).unwrap().as_bool(), Some(true));
        }
        let failure = reference.get("receiveFailure").unwrap();
        assert_eq!(failure.get("peerPort"), Some(&number(0)));
        assert_ne!(failure.get("kind").unwrap().as_str().unwrap(), "success");
        #[cfg(windows)]
        {
            use crate::media::{RtcpRuntime, WindowsRtcpRuntime};
            let time = WindowsRtcpRuntime.now().unwrap();
            assert_eq!(
                reference
                    .get("localUtcOffsetTicks")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                (time.local_ticks() - time.utc_ticks()).to_string()
            );
        }
    }
    trace(
        "native-udp-policy",
        IvyJson::object([
            ("inputs", IvyJson::Array(inputs)),
            ("rust", IvyJson::Array(rows)),
            ("original", reference.unwrap_or(IvyJson::Null)),
        ]),
    );
}

#[test]
fn unknown_and_zero_port_receive_failures_do_not_infer_a_peer_or_read_the_clock() {
    let reads = Arc::new(AtomicUsize::new(0));
    let clock: LocalClock = {
        let reads = reads.clone();
        Arc::new(move || {
            reads.fetch_add(1, Ordering::SeqCst);
            Err(())
        })
    };
    let cache = FailedDestinations::default();
    cache.receive_error(None, &clock).unwrap();
    cache
        .receive_error(Some("127.0.0.1:0".parse().unwrap()), &clock)
        .unwrap();
    assert!(cache.expired(&clock).unwrap().is_empty());
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert!(
        cache
            .receive_error(Some("127.0.0.1:5060".parse().unwrap()), &clock)
            .is_err()
    );
    assert!(cache.0.lock().unwrap().is_empty());
    cache
        .0
        .lock()
        .unwrap()
        .insert("127.0.0.1:5060".parse().unwrap(), 1);
    assert!(cache.expired(&clock).is_err());
    assert_eq!(cache.0.lock().unwrap().len(), 1);
}

#[test]
fn endpoint_identity_keeps_scope_and_port_without_ipv6_flow_information() {
    let a = std::net::SocketAddrV6::new("fe80::1".parse().unwrap(), 5060, 123, 12);
    let b = std::net::SocketAddrV6::new(*a.ip(), a.port(), 0, a.scope_id());
    assert_eq!(canonical(a.into()), SocketAddr::V6(b));
    assert_ne!(canonical(a.into()), "[fe80::1%13]:5060".parse().unwrap());
}

#[tokio::test]
async fn native_udp_send_uses_original_ipv6_and_mapped_ipv4_endpoint_layouts() {
    let mut cases = Vec::new();
    for (binding, mapped) in [("127.0.0.1:0", true), ("[::1]:0", false)] {
        let raw = Arc::new(UdpSocket::bind(binding).await.unwrap());
        let local = raw.local_addr().unwrap();
        let weak = Arc::downgrade(&raw);
        let owner = SipUdpSocket::with_policy(
            raw,
            Arc::new(FailedDestinations::default()),
            Arc::new(|| Ok(1)),
        )
        .unwrap();
        let peer = UdpSocket::bind(binding).await.unwrap();
        let destination = if mapped {
            let SocketAddr::V4(address) = peer.local_addr().unwrap() else {
                unreachable!()
            };
            std::net::SocketAddrV6::new(address.ip().to_ipv6_mapped(), address.port(), 0, 0).into()
        } else {
            peer.local_addr().unwrap()
        };
        owner.begin_send(b"\r\n\r\n", destination).unwrap();
        let mut bytes = [0; 8];
        let (size, source) =
            tokio::time::timeout(Duration::from_secs(2), peer.recv_from(&mut bytes))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(&bytes[..size], b"\r\n\r\n");
        assert_eq!(source, local);
        assert_eq!(owner.completions().await, vec![Some(Ok(4))]);
        owner.request_stop();
        owner.drain().await.unwrap();
        drop(owner);
        assert_eq!(weak.strong_count(), 0);
        let _rebound = UdpSocket::bind(local).await.unwrap();
        cases.push(IvyJson::object([
            ("mappedIpv4", mapped.into()),
            ("originalSource", true.into()),
            ("bindingReleased", true.into()),
        ]));
    }
    trace("native-udp-addresses", IvyJson::Array(cases));
}

#[tokio::test]
async fn both_channel_owners_share_failure_admission_before_close_and_keep_the_original_socket() {
    let cache = Arc::new(FailedDestinations::default());
    let clock: LocalClock = Arc::new(|| Ok(10_000_000_000));
    let a = SipUdpSocket::with_policy(
        Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap()),
        cache.clone(),
        clock.clone(),
    )
    .unwrap();
    let b = SipUdpSocket::with_policy(
        Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap()),
        cache.clone(),
        clock.clone(),
    )
    .unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let remote = peer.local_addr().unwrap();
    let weak_a = Arc::downgrade(&a.socket);
    let weak_b = Arc::downgrade(&b.socket);
    let local_a = a.local;
    let local_b = b.local;
    cache.receive_error(Some(remote), &clock).unwrap();
    let mut codes = Vec::new();
    let code = |result: Result<(), RegistrationKeepaliveError>| match result {
        Ok(()) => number(0),
        Err(RegistrationKeepaliveError::SocketError(code)) => number(code as usize),
        Err(RegistrationKeepaliveError::SendFailed) => "invalid_buffer".into(),
        _ => panic!("Unexpected admission"),
    };
    codes.push(code(a.begin_send(b"\r\n\r\n", remote)));
    codes.push(code(b.begin_send(b"\r\n\r\n", remote)));
    a.request_stop();
    codes.push(code(a.begin_send(b"\r\n\r\n", remote)));
    cache.0.lock().unwrap().remove(&remote);
    codes.push(code(a.begin_send(b"\r\n\r\n", remote)));
    codes.push(code(a.begin_send(b"", remote)));
    codes.push(code(b.begin_send(b"\r\n\r\n", remote)));
    let mut bytes = [0; 8];
    let (size, source) = tokio::time::timeout(Duration::from_secs(2), peer.recv_from(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(source, local_b);
    assert_eq!(&bytes[..size], b"\r\n\r\n");
    codes.push(code(b.begin_send(b"\r\n\r\n", "[::1]:9".parse().unwrap())));
    let completions = b.completions().await;
    #[cfg(windows)]
    assert_eq!(completions, vec![Some(Ok(4)), Some(Err(Some(10047)))]);
    assert!(cache.0.lock().unwrap().is_empty());
    b.request_stop();
    a.drain().await.unwrap();
    b.drain().await.unwrap();
    drop(a);
    drop(b);
    assert_eq!(weak_a.strong_count(), 0);
    assert_eq!(weak_b.strong_count(), 0);
    let _a = UdpSocket::bind(local_a).await.unwrap();
    let _b = UdpSocket::bind(local_b).await.unwrap();
    trace(
        "native-udp-admission",
        IvyJson::object([
            ("codes", IvyJson::Array(codes)),
            ("sameSocket", true.into()),
            ("completionFailureDidNotPopulateCache", true.into()),
            ("originalBindingsReleased", true.into()),
            ("replacementSockets", number(0)),
        ]),
    );
}

#[tokio::test]
async fn pending_completions_survive_lost_observers_and_stop_drains_the_original_workers() {
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let local = socket.local_addr().unwrap();
    let weak = Arc::downgrade(&socket);
    let owner = SipUdpSocket::with_policy(
        socket,
        Arc::new(FailedDestinations::default()),
        Arc::new(|| Ok(1)),
    )
    .unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let remote = peer.local_addr().unwrap();
    let reserve = || {
        owner.sends.state.lock().unwrap().count += 1;
        SendLease(owner.sends.clone())
    };
    // Force the same pending branch used for WouldBlock, without relying on
    // machine-specific UDP pressure or adding a production capacity gate.
    owner.defer_send(b"\r\n\r\n".to_vec(), remote, reserve());
    let mut observer = Box::pin(owner.wait_sends());
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(observer.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    drop(observer);
    let mut bytes = [0; 8];
    let (size, source) = tokio::time::timeout(Duration::from_secs(2), peer.recv_from(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(source, local);
    assert_eq!(&bytes[..size], b"\r\n\r\n");
    owner.wait_sends().await;
    owner.defer_send(b"\r\n\r\n".to_vec(), remote, reserve());
    owner.request_stop();
    owner.drain().await.unwrap();
    assert_eq!(owner.completions().await, vec![Some(Ok(4)), None]);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.recv_from(&mut bytes))
            .await
            .is_err()
    );
    drop(owner);
    assert_eq!(weak.strong_count(), 0);
    let _rebound = UdpSocket::bind(local).await.unwrap();
    trace(
        "native-udp-pending",
        IvyJson::object([
            ("forcedWouldBlockBranch", true.into()),
            ("lostObserverPreservedSend", true.into()),
            ("stopPreventedUnstartedSend", true.into()),
            ("originalBindingReleased", true.into()),
        ]),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_sweep_drain_observer_keeps_the_original_worker_for_final_retirement() {
    let cache = Arc::new(FailedDestinations::default());
    cache
        .0
        .lock()
        .unwrap()
        .insert("127.0.0.1:21001".parse().unwrap(), 1);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let entered = Arc::new(Notify::new());
    let clock: LocalClock = {
        let release = release.clone();
        let entered = entered.clone();
        Arc::new(move || {
            entered.notify_one();
            let (lock, changed) = &*release;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = changed.wait(open).unwrap();
            }
            Ok(1)
        })
    };
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let local = socket.local_addr().unwrap();
    let weak = Arc::downgrade(&socket);
    let owner = SipUdpSocket::with_policy(socket, cache, clock).unwrap();
    entered.notified().await;
    owner.request_stop();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), owner.drain())
            .await
            .is_err()
    );
    {
        let (lock, changed) = &*release;
        *lock.lock().unwrap() = true;
        changed.notify_one();
    }
    owner.drain().await.unwrap();
    assert!(owner.sweep.lock().await.is_none());
    drop(owner);
    assert_eq!(weak.strong_count(), 0);
    let _rebound = UdpSocket::bind(local).await.unwrap();
    trace(
        "native-udp-retirement",
        IvyJson::object([
            ("lostDrainObserverSafe", true.into()),
            ("originalWorkerRetired", true.into()),
            ("originalBindingReleased", true.into()),
        ]),
    );
}
