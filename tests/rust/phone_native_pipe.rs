use super::{EPOCH, OP, envelope};
use ivy_contracts::IvyJson;
use ivy_phone_bridge::{
    native_pipe::{
        NativePipeServer, NativePipeSession, NativeRuntime, NativeServerReport, NativeShutdown,
        NativeShutdownStatus, NativeStopReason, run_owned_native_stdio,
    },
    native_rpc::{NativeHandler, NativeInvocation, NativeOutcome, NativeRequest},
    sip::{SipHost, SipHostBinding, SipHostConfiguration},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{
        AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf,
    },
    sync::Semaphore,
};

struct Runtime {
    invoked: Arc<AtomicUsize>,
    finished: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    hold_all: AtomicBool,
    effects: Arc<Semaphore>,
    cleanup: Arc<Semaphore>,
    host: SipHost,
}
impl Runtime {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            invoked: Arc::new(AtomicUsize::new(0)),
            finished: Arc::new(AtomicUsize::new(0)),
            shutdowns: Arc::new(AtomicUsize::new(0)),
            closed: Arc::new(AtomicBool::new(false)),
            hold_all: AtomicBool::new(false),
            effects: Arc::new(Semaphore::new(0)),
            cleanup: Arc::new(Semaphore::new(1)),
            host: SipHost::new(),
        })
    }
}
impl NativeHandler for Runtime {
    fn cleanup_slots(&self) -> usize {
        4
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        self.invoked.fetch_add(1, Ordering::AcqRel);
        let held = self.hold_all.load(Ordering::Acquire)
            || request.params().get("hold").and_then(IvyJson::as_bool) == Some(true);
        let panics = request.params().get("panic").and_then(IvyJson::as_bool) == Some(true);
        let effects = self.effects.clone();
        let finished = self.finished.clone();
        let method = request.method().to_string();
        Box::pin(async move {
            if panics {
                panic!("synthetic-private-panic-payload");
            }
            if held {
                effects.acquire().await.unwrap().forget();
            }
            finished.fetch_add(1, Ordering::AcqRel);
            if method == "audio.probe" {
                return NativeOutcome::result(format!("{}0{}", "[".repeat(24), "]".repeat(24)));
            }
            NativeOutcome::result(format!("{{\"method\":\"{method}\"}}"))
        })
    }
}
impl NativeRuntime for Runtime {
    fn shutdown(&self) -> NativeShutdown {
        self.shutdowns.fetch_add(1, Ordering::AcqRel);
        let gate = self.cleanup.clone();
        let effects = self.effects.clone();
        let host = self.host.clone();
        let closed = self.closed.clone();
        Box::pin(async move {
            gate.acquire().await.unwrap().forget();
            host.close()
                .await
                .map_err(|_| ivy_phone_bridge::native_pipe::NativeShutdownError::OutcomeUnknown)?;
            effects.add_permits(128);
            closed.store(true, Ordering::Release);
            Ok(())
        })
    }
}
struct Peer {
    writer: WriteHalf<DuplexStream>,
    reader: BufReader<ReadHalf<DuplexStream>>,
    session: NativePipeSession,
}
fn peer(runtime: Arc<Runtime>, bytes: usize) -> Peer {
    let (parent, child) = tokio::io::duplex(bytes);
    let (input, output) = tokio::io::split(child);
    let (reader, writer) = tokio::io::split(parent);
    Peer {
        writer,
        reader: BufReader::new(reader),
        session: NativePipeServer::start(EPOCH.into(), input, output, runtime).unwrap(),
    }
}
async fn send(peer: &mut Peer, method: &str, operation: Option<&str>, params: &str, id: u64) {
    peer.writer
        .write_all(format!("{}\n", envelope(method, operation, params, id)).as_bytes())
        .await
        .unwrap();
}
async fn reply(peer: &mut Peer) -> IvyJson {
    let mut line = String::new();
    let count = tokio::time::timeout(Duration::from_secs(2), peer.reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert!(count > 0);
    assert_eq!(line.bytes().filter(|byte| *byte == b'\n').count(), 1);
    ivy_contracts::json::parse(line.trim_end_matches('\n'), 65536).unwrap()
}
async fn until(ready: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn report(peer: &mut Peer) -> NativeServerReport {
    tokio::time::timeout(Duration::from_secs(6), peer.session.wait())
        .await
        .unwrap()
}
async fn bind(host: &SipHost) -> std::net::SocketAddr {
    let binding = SipHostBinding::new(
        "127.0.0.1:0".parse().unwrap(),
        rsipstack::sip::Transport::Udp,
        None,
        1,
    )
    .unwrap();
    host.configure(SipHostConfiguration::new(binding, None).unwrap())
        .await
        .unwrap();
    host.observation().endpoint.unwrap()
}

#[tokio::test]
async fn split_frames_exact_correlation_and_replay_do_not_repeat_effects() {
    let runtime = Runtime::new();
    let mut peer = peer(runtime.clone(), 8192);
    let line = format!("{}\n", envelope("call.release", Some(OP), "{}", 1));
    peer.writer.write_all(&line.as_bytes()[..19]).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(runtime.invoked.load(Ordering::Acquire), 0);
    peer.writer.write_all(&line.as_bytes()[19..]).await.unwrap();
    let first = reply(&mut peer).await;
    assert_eq!(first.get("requestId").unwrap().as_f64(), Some(1.0));
    assert_eq!(first.get("epoch").unwrap().as_str().unwrap(), EPOCH);
    assert_eq!(first.get("ok").unwrap().as_bool(), Some(true));
    send(&mut peer, "call.release", Some(OP), "{}", 2).await;
    assert_eq!(
        reply(&mut peer).await.get("requestId").unwrap().as_f64(),
        Some(2.0)
    );
    assert_eq!(runtime.invoked.load(Ordering::Acquire), 1);
    peer.writer.shutdown().await.unwrap();
    let result = report(&mut peer).await;
    assert_eq!(result.reason, NativeStopReason::Eof);
    assert_eq!(result.shutdown, NativeShutdownStatus::Confirmed);
    assert_eq!(result.retained_receipts, 1);
}

#[tokio::test]
async fn malformed_frames_are_terminal_but_preceding_same_read_effect_is_admitted() {
    let nested = format!("{{\"value\":{}0{}}}", "[".repeat(23), "]".repeat(23));
    let mut cases = vec![
        b"\xff\n".to_vec(),
        b"\n".to_vec(),
        b"{}\n".to_vec(),
        vec![b'x'; 65537],
    ];
    for params in ["{\"duplicate\":1,\"duplicate\":2}", &nested] {
        cases.push(format!("{}\n", envelope("status", None, params, 2)).into_bytes());
    }
    for invalid in cases {
        let runtime = Runtime::new();
        let mut peer = peer(runtime.clone(), 131072);
        let mut bytes = format!("{}\n", envelope("call.release", Some(OP), "{}", 1)).into_bytes();
        bytes.extend_from_slice(&invalid);
        peer.writer.write_all(&bytes).await.unwrap();
        let result = report(&mut peer).await;
        assert_eq!(result.reason, NativeStopReason::InvalidFrame);
        assert_eq!(result.shutdown, NativeShutdownStatus::Confirmed);
        // Invocation can be stopped after its original yield, just as in C#;
        // admission and its safe receipt must already exist in either case.
        assert_eq!(result.retained_receipts, 1);
        assert!(runtime.invoked.load(Ordering::Acquire) <= 1);
        assert_eq!(runtime.shutdowns.load(Ordering::Acquire), 1);
    }
}

#[tokio::test]
async fn pb01_realtime_stop_uses_reserved_pipe_capacity_without_reserving_rejections() {
    let runtime = Runtime::new();
    runtime.hold_all.store(true, Ordering::Release);
    let mut peer = peer(runtime.clone(), 32768);
    for id in 1..=32 {
        send(&mut peer, "desktop.observe", None, "{}", id).await;
    }
    until(|| runtime.invoked.load(Ordering::Acquire) == 32).await;
    send(&mut peer, "call.prepare", Some(EPOCH), "{}", 33).await;
    let rejected = reply(&mut peer).await;
    assert_eq!(
        rejected.get("error").unwrap().as_str().unwrap(),
        "capacity_exceeded"
    );
    assert_eq!(rejected.get("requestId").unwrap().as_f64(), Some(33.0));
    for id in 34..=48 {
        send(&mut peer, "status", None, "{}", id).await;
    }
    until(|| runtime.invoked.load(Ordering::Acquire) == 47).await;
    send(&mut peer, "call.realtime.stop", Some(OP), "{}", 49).await;
    until(|| runtime.invoked.load(Ordering::Acquire) == 48).await;
    send(&mut peer, "call.release", Some(EPOCH), "{}", 50).await;
    let rejected = reply(&mut peer).await;
    assert_eq!(
        rejected.get("error").unwrap().as_str().unwrap(),
        "capacity_exceeded"
    );
    assert_eq!(rejected.get("requestId").unwrap().as_f64(), Some(50.0));
    peer.session.request_stop();
    assert_eq!(report(&mut peer).await.retained_receipts, 1);
    until(|| runtime.finished.load(Ordering::Acquire) == 48).await;
    assert_eq!(runtime.shutdowns.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn eof_discards_partial_frame_and_closes_actual_original_sip_binding() {
    let runtime = Runtime::new();
    let address = bind(&runtime.host).await;
    let mut peer = peer(runtime.clone(), 8192);
    peer.writer
        .write_all(envelope("call.release", Some(OP), "{}", 1).as_bytes())
        .await
        .unwrap();
    peer.writer.shutdown().await.unwrap();
    let result = report(&mut peer).await;
    assert_eq!(result.reason, NativeStopReason::Eof);
    assert_eq!(result.retained_receipts, 0);
    assert_eq!(runtime.invoked.load(Ordering::Acquire), 0);
    assert!(std::net::UdpSocket::bind(address).is_ok());
}

#[tokio::test]
async fn cancelled_wait_and_shutdown_deadline_retain_original_cleanup_and_socket_fence() {
    let runtime = Runtime::new();
    runtime.cleanup.acquire().await.unwrap().forget();
    let address = bind(&runtime.host).await;
    let mut peer = peer(runtime.clone(), 8192);
    peer.session.request_stop();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.session.wait())
            .await
            .is_err()
    );
    until(|| runtime.shutdowns.load(Ordering::Acquire) == 1).await;
    assert!(std::net::UdpSocket::bind(address).is_err());
    let result = report(&mut peer).await;
    assert_eq!(result.shutdown, NativeShutdownStatus::Unconfirmed);
    assert_eq!(peer.session.wait().await, result);
    assert!(std::net::UdpSocket::bind(address).is_err());
    assert!(!runtime.closed.load(Ordering::Acquire));
    runtime.cleanup.add_permits(1);
    until(|| runtime.closed.load(Ordering::Acquire)).await;
    assert!(std::net::UdpSocket::bind(address).is_ok());
    assert_eq!(runtime.shutdowns.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn dropping_server_observer_keeps_original_shutdown_alive() {
    let runtime = Runtime::new();
    runtime.cleanup.acquire().await.unwrap().forget();
    let peer = peer(runtime.clone(), 8192);
    drop(peer.session);
    until(|| runtime.shutdowns.load(Ordering::Acquire) == 1).await;
    assert!(!runtime.closed.load(Ordering::Acquire));
    runtime.cleanup.add_permits(1);
    until(|| runtime.closed.load(Ordering::Acquire)).await;
}

#[tokio::test]
async fn response_envelope_failure_stops_without_waiting_for_more_input() {
    let runtime = Runtime::new();
    let mut peer = peer(runtime, 8192);
    send(&mut peer, "audio.probe", None, "{}", 1).await;
    let result = report(&mut peer).await;
    assert_eq!(result.reason, NativeStopReason::OutputFailed);
    assert_eq!(result.shutdown, NativeShutdownStatus::Confirmed);
}

// This inert test is also the isolated process wrapper for real inherited
// stdio. The parent consumes the known libtest prefix separately; the server's
// exact output is independently asserted by duplex tests above.
#[test]
fn stdio_fixture() {
    if std::env::var_os("IVY_PHONE_PIPE_FIXTURE").is_none() {
        return;
    }
    let result = run_owned_native_stdio(EPOCH.into(), Runtime::new()).unwrap();
    eprintln!(
        "phone_fixture_exit:{:?}:{:?}",
        result.reason, result.shutdown
    );
    std::process::exit(if result.shutdown == NativeShutdownStatus::Confirmed {
        0
    } else {
        1
    });
}
fn child() -> tokio::process::Command {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "native_pipe::stdio_fixture",
            "--nocapture",
            "--format=terse",
        ])
        .env("IVY_PHONE_PIPE_FIXTURE", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command
}
#[tokio::test]
async fn actual_stdio_eof_safe_panic_and_valid_controls() {
    let mut child = child().spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut prefix = String::new();
    loop {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(3), output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        prefix.push_str(&line);
        if line.contains("running 1 test") {
            break;
        }
        assert!(prefix.len() < 128);
    }
    assert_eq!(prefix.replace('\r', ""), "\nrunning 1 test\n");
    for (method, operation, params, id, error) in [
        (
            "heartbeat",
            None,
            "{\"unexpected\":true}",
            1,
            Some("invalid_request"),
        ),
        (
            "shutdown",
            Some(OP),
            "{\"unexpected\":true}",
            2,
            Some("invalid_request"),
        ),
        (
            "desktop.capture",
            None,
            "{\"panic\":true}",
            3,
            Some("operation_outcome_unknown"),
        ),
        ("heartbeat", None, "{}", 4, None),
    ] {
        input
            .write_all(format!("{}\n", envelope(method, operation, params, id)).as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(3), output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let response = ivy_contracts::json::parse(line.trim_end(), 65536).unwrap();
        assert_eq!(response.get("requestId").unwrap().as_f64(), Some(id as f64));
        assert_eq!(
            response
                .get("error")
                .and_then(|v| v.as_str().ok())
                .as_deref(),
            error
        );
    }
    drop(input);
    let result = tokio::time::timeout(Duration::from_secs(6), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(result.status.success());
    let stderr = String::from_utf8(result.stderr).unwrap().replace('\r', "");
    assert_eq!(
        stderr,
        "phone_native_panic\nphone_fixture_exit:Eof:Confirmed\n"
    );
    let mut trailing = Vec::new();
    output.read_to_end(&mut trailing).await.unwrap();
    assert!(trailing.is_empty());
}

#[tokio::test]
async fn actual_stdio_lease_loss_exits_while_inherited_stdin_stays_open() {
    let mut child = child().spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let started = std::time::Instant::now();
    tokio::time::sleep(Duration::from_secs(10)).await;
    // Invalid heartbeat must not extend the original lease to 25 seconds.
    input
        .write_all(
            format!(
                "{}\n",
                envelope("heartbeat", None, "{\"unexpected\":true}", 1)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(11), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(result.status.success());
    assert!(started.elapsed() >= Duration::from_secs(14));
    assert!(started.elapsed() < Duration::from_secs(21));
    let stderr = String::from_utf8(result.stderr).unwrap().replace('\r', "");
    assert_eq!(stderr, "phone_fixture_exit:HeartbeatLost:Confirmed\n");
    let output = String::from_utf8(result.stdout).unwrap().replace('\r', "");
    let response = output.strip_prefix("\nrunning 1 test\n").unwrap();
    let response = ivy_contracts::json::parse(response.trim_end(), 65536).unwrap();
    assert_eq!(
        response.get("error").unwrap().as_str().unwrap(),
        "invalid_request"
    );
    drop(input);
}

#[tokio::test]
async fn valid_heartbeat_renews_the_monotonic_lease() {
    let runtime = Runtime::new();
    let mut peer = peer(runtime, 8192);
    tokio::time::sleep(Duration::from_secs(2)).await;
    send(&mut peer, "heartbeat", None, "{}", 1).await;
    assert_eq!(
        reply(&mut peer).await.get("ok").unwrap().as_bool(),
        Some(true)
    );
    tokio::time::sleep(Duration::from_millis(13100)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(10), peer.session.wait())
            .await
            .is_err()
    );
    peer.session.request_stop();
    assert_eq!(report(&mut peer).await.reason, NativeStopReason::Requested);
}

#[tokio::test]
async fn blocked_output_cannot_prevent_heartbeat_shutdown() {
    let runtime = Runtime::new();
    let mut peer = peer(runtime.clone(), 1);
    send(&mut peer, "status", None, "{}", 1).await;
    let result = tokio::time::timeout(Duration::from_secs(18), peer.session.wait())
        .await
        .unwrap();
    assert_eq!(result.reason, NativeStopReason::HeartbeatLost);
    assert_eq!(result.shutdown, NativeShutdownStatus::Confirmed);
    assert_eq!(runtime.shutdowns.load(Ordering::Acquire), 1);
}
