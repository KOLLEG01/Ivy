use super::*;
use crate::media::{
    AudioNegotiator, Codec, RtcpWallTime, RtpHeader, RtpPeer, SdpDocument, SdpType,
    WindowsRtcpRuntime,
};
use ivy_contracts::{IvyJson, json::stringify};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::{
    net::UdpSocket,
    time::{sleep, timeout},
};
struct ClosedAudio;
impl PcmAudioPort for ClosedAudio {
    fn state(&self) -> PcmAudioState {
        PcmAudioState {
            open: false,
            generation: 1,
        }
    }
    fn read_captured(&self, output: &mut [f32], _: u64) -> usize {
        output.fill(0.0);
        0
    }
    fn write_received(&self, _: &[f32], _: u64) -> bool {
        false
    }
}
struct FastClock;
impl RtcpRuntime for FastClock {
    fn now(&self) -> Result<RtcpWallTime, MediaError> {
        WindowsRtcpRuntime.now()
    }
    fn interval_ms(&self) -> Result<u16, MediaError> {
        Ok(2500)
    }
}
struct Events {
    mode: u8,
    receiver: Mutex<Option<SipRtpReceiver>>,
    calls: AtomicUsize,
    timeouts: AtomicUsize,
    pending: AtomicBool,
    completed: Mutex<Vec<Duration>>,
}
impl SipReceiveEvents for Events {
    fn digit(&self, _: char, _: InputSource) {}
    fn failed(&self) {
        panic!("RTCP callback must not fail the media receiver");
    }
    fn rtcp(&self, _: SocketAddr, _: &[u8]) {}
    fn rtcp_timeout(&self) {
        let receiver = self.receiver.lock().unwrap().as_ref().unwrap().clone();
        assert!(receiver.observation().rtcp_session.unwrap().timed_out);
        self.timeouts.fetch_add(1, Ordering::SeqCst);
        if self.mode == 3 {
            panic!("Synthetic timeout observer failure");
        }
    }
    fn report_sent(&self, _: &[u8]) {
        let receiver = self.receiver.lock().unwrap().as_ref().unwrap().clone();
        assert!(receiver.observation().rtcp.reports_sent > 0); // Both owner locks must be released.
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.mode == 1 {
            panic!("Synthetic report observer failure");
        }
        if self.mode == 2 {
            use std::{
                future::Future,
                task::{Context, Waker},
            };
            self.receiver.lock().unwrap().take();
            let mut close = Box::pin(receiver.close());
            self.pending.store(
                close
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending(),
                Ordering::SeqCst,
            );
            drop(close); // Cancel only this close observer, keeping cleanup active.
        } else {
            // Detect rearming before callback work finishes, not just cadence.
            std::thread::sleep(Duration::from_millis(80));
        }
        self.completed.lock().unwrap().push(receiver.handle.0.now());
    }
}
async fn wait(mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(12), async {
        while !condition() {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
async fn setup(
    port: u16,
    mode: u8,
    real_entropy: bool,
) -> (SipRtpReceiver, Arc<Events>, UdpSocket, UdpSocket) {
    let events = Arc::new(Events {
        mode,
        receiver: Mutex::new(None),
        calls: AtomicUsize::new(0),
        timeouts: AtomicUsize::new(0),
        pending: AtomicBool::new(false),
        completed: Mutex::new(Vec::new()),
    });
    let receiver = SipRtpReceiver::bind(
        "127.0.0.1:0".parse().unwrap(),
        port,
        port + 98,
        20,
        Arc::new(ClosedAudio),
        None,
        events.clone(),
    )
    .unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let control = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sdp=SdpDocument::parse(&format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na=sendrecv\r\n",peer.local_addr().unwrap().port())).unwrap();
    let mut negotiation = AudioNegotiator::new(&[Codec::Pcmu]).unwrap();
    negotiation
        .set_remote_description(SdpType::Offer, &sdp)
        .unwrap();
    receiver
        .configure(
            negotiation.state().unwrap(),
            RtpPeer::new(peer.local_addr().unwrap(), control.local_addr().unwrap()).unwrap(),
        )
        .unwrap();
    let runtime: Arc<dyn RtcpRuntime> = if real_entropy {
        Arc::new(WindowsRtcpRuntime)
    } else {
        Arc::new(FastClock)
    };
    receiver.enable_reporting(9, "fixture", runtime).unwrap();
    *events.receiver.lock().unwrap() = Some(receiver.clone());
    (receiver, events, peer, control)
}
fn admitted_fixture_send(receiver: &SipRtpReceiver, sequence: u16) {
    // An actual synthetic RTP send through the same private transport. This
    // does not expose a production route/authority bypass or a second socket.
    let inner = &receiver.handle.0;
    let mut reports = inner.reports.lock().unwrap();
    let header = RtpHeader {
        marker: false,
        payload_type: 0,
        sequence,
        timestamp: u32::from(sequence) * 160,
        source: 9,
    };
    inner
        .transport
        .get()
        .unwrap()
        .send_rtp(header, &[0; 160])
        .unwrap();
    reports
        .session
        .as_mut()
        .unwrap()
        .sent(header, 160, || inner.now())
        .unwrap();
    inner.report_pending.notify_one();
}
async fn packet(control: &UdpSocket, expected: SocketAddr) -> Vec<u8> {
    let mut bytes = vec![0; 2048];
    let (size, source) = timeout(Duration::from_secs(12), control.recv_from(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(source, expected);
    bytes.truncate(size);
    bytes
}
fn record(name: &str, items: Vec<IvyJson>) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_RTCP_DRIVER_TRACE_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("rtcp-driver-{name}.json")),
            stringify(&IvyJson::Array(items), 100000).unwrap(),
        )
        .unwrap();
    }
}
fn n(v: u32) -> IvyJson {
    IvyJson::Number(f64::from(v))
}
fn wire(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    })
}
fn release(receiver: &SipRtpReceiver) {
    let local = receiver.observation().transport;
    assert!(local.cleanup_confirmed);
    assert!(receiver.observation().cleanup_confirmed);
    let _rtp = std::net::UdpSocket::bind(local.local_rtp).unwrap();
    let _rtcp = std::net::UdpSocket::bind(local.local_rtcp).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_rtcp_timer_uses_original_pair_after_work_sr_rr_missing_destination_and_final_bye() {
    let (receiver, events, peer, control) = setup(43000, 0, true).await;
    let local = receiver.observation().transport;
    let start_before = receiver.handle.0.now();
    admitted_fixture_send(&receiver, 10);
    let start_after = receiver.handle.0.now();
    let mut rtp = [0; 2048];
    let (size, source) = timeout(Duration::from_secs(2), peer.recv_from(&mut rtp))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(source, local.local_rtp);
    assert_eq!(size, 172);
    let sent = crate::media::RtpPacket::parse(&rtp[..size]).unwrap();
    assert_eq!(sent.header.sequence, 10);
    assert_eq!(sent.header.source, 9);
    let first_deadline = receiver.observation().rtcp_session.unwrap().next.unwrap();
    assert!(
        first_deadline >= start_before + Duration::from_millis(2500)
            && first_deadline <= start_after + Duration::from_millis(7500)
    );
    let first = packet(&control, local.local_rtcp).await;
    let sender = RtcpCompound::parse(&first).unwrap().sender.unwrap();
    assert_eq!(sender.info.packet_count, 1);
    assert_eq!(sender.info.octet_count, 160);
    assert_eq!(sender.info.rtp_timestamp, 1600);
    wait(|| {
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .next
            .is_some()
    })
    .await;
    let finished = events.completed.lock().unwrap()[0];
    assert!(
        receiver.observation().rtcp_session.unwrap().next.unwrap()
            >= finished + Duration::from_millis(2500)
    );
    receiver
        .handle
        .0
        .transport
        .get()
        .unwrap()
        .set_peer(None)
        .unwrap();
    wait(|| {
        events.calls.load(Ordering::SeqCst) >= 2
            && receiver
                .observation()
                .rtcp_session
                .as_ref()
                .unwrap()
                .next
                .is_some()
    })
    .await;
    assert_eq!(receiver.observation().rtcp.reports_sent, 2);
    assert_eq!(receiver.observation().transport.counters.rtcp_sent, 1);
    assert!(!receiver.observation().failed);
    receiver
        .handle
        .0
        .transport
        .get()
        .unwrap()
        .set_peer(Some(
            RtpPeer::new(peer.local_addr().unwrap(), control.local_addr().unwrap()).unwrap(),
        ))
        .unwrap();
    let second = packet(&control, local.local_rtcp).await;
    assert!(RtcpCompound::parse(&second).unwrap().receiver.is_some());
    wait(|| {
        events.calls.load(Ordering::SeqCst) == 3
            && receiver
                .observation()
                .rtcp_session
                .as_ref()
                .unwrap()
                .next
                .is_some()
    })
    .await;
    let reports = receiver.observation().rtcp.reports_sent;
    assert_eq!(reports, 3);
    receiver.close().await;
    events.receiver.lock().unwrap().take();
    let bye = packet(&control, local.local_rtcp).await;
    let parsed = RtcpCompound::parse(&bye).unwrap();
    assert!(parsed.receiver.is_some());
    assert!(parsed.bye_packet.is_some());
    assert_eq!(receiver.observation().rtcp.reports_sent, reports);
    assert_eq!(receiver.observation().transport.counters.rtcp_sent, 3);
    release(&receiver);
    record(
        "normal",
        vec![IvyJson::object([
            ("rtpBytes", n(size as u32)),
            ("first", wire(&first).into()),
            ("following", wire(&second).into()),
            ("bye", wire(&bye).into()),
            ("applicationSent", n(reports as u32)),
            ("transportSent", n(3)),
            (
                "firstDeadlineNanos",
                first_deadline.as_nanos().to_string().into(),
            ),
            (
                "callbackCompletedNanos",
                finished.as_nanos().to_string().into(),
            ),
        ])],
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_report_callback_failure_stops_only_report_timer_and_new_rtp_does_not_restart_it() {
    let (receiver, events, _peer, control) = setup(43200, 1, false).await;
    let local = receiver.observation().transport;
    admitted_fixture_send(&receiver, 1);
    let first = packet(&control, local.local_rtcp).await;
    wait(|| {
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .timer_failure
            .is_some()
    })
    .await;
    assert_eq!(
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .timer_failure,
        Some(MediaError::ReportCallbackFailed)
    );
    assert!(!receiver.observation().failed);
    assert!(!receiver.observation().transport.closed);
    assert!(
        RtcpCompound::parse(&receiver.read_report().unwrap())
            .unwrap()
            .sender
            .is_some()
    );
    admitted_fixture_send(&receiver, 2);
    assert_eq!(
        receiver.observation().rtcp_session.as_ref().unwrap().next,
        None
    );
    receiver.close().await;
    events.receiver.lock().unwrap().take();
    let bye = packet(&control, local.local_rtcp).await;
    assert_eq!(receiver.observation().rtcp.reports_sent, 1);
    release(&receiver);
    record(
        "callback-failure",
        vec![IvyJson::object([
            ("first", wire(&first).into()),
            ("bye", wire(&bye).into()),
            ("applicationSent", n(1)),
            ("mediaFailed", false.into()),
            ("timerStopped", true.into()),
        ])],
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_timeout_callback_failure_precedes_report_and_keeps_media_cleanup() {
    let (receiver, events, _peer, control) = setup(43600, 3, false).await;
    let local = receiver.observation().transport;
    receiver
        .handle
        .0
        .reports
        .lock()
        .unwrap()
        .session
        .as_mut()
        .unwrap()
        .set_timeout_ms(0);
    receiver.start_reporting().unwrap();
    wait(|| {
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .timer_failure
            .is_some()
    })
    .await;
    let observation = receiver.observation();
    assert_eq!(
        observation.rtcp_session.unwrap().timer_failure,
        Some(MediaError::ReportCallbackFailed)
    );
    assert!(!observation.failed);
    assert!(!observation.transport.closed);
    assert_eq!(observation.transport.counters.rtcp_sent, 0);
    assert_eq!(observation.rtcp.reports_sent, 0);
    assert_eq!(events.calls.load(Ordering::SeqCst), 0);
    assert_eq!(events.timeouts.load(Ordering::SeqCst), 1);
    receiver.close().await;
    events.receiver.lock().unwrap().take();
    let bye = packet(&control, local.local_rtcp).await;
    let parsed = RtcpCompound::parse(&bye).unwrap();
    assert!(parsed.receiver.is_some());
    assert!(parsed.bye_packet.is_some());
    release(&receiver);
    record(
        "timeout-failure",
        vec![IvyJson::object([
            ("bye", wire(&bye).into()),
            ("applicationSent", n(0)),
            ("timeouts", n(1)),
            ("mediaFailed", false.into()),
        ])],
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn report_callback_reentrant_close_keeps_one_bye_count_commit_and_original_cleanup_after_cancel()
 {
    let (receiver, events, _peer, control) = setup(43400, 2, false).await;
    let local = receiver.observation().transport;
    admitted_fixture_send(&receiver, 1);
    let first = packet(&control, local.local_rtcp).await;
    let bye = packet(&control, local.local_rtcp).await;
    receiver.close().await;
    assert!(events.pending.load(Ordering::SeqCst));
    assert_eq!(events.calls.load(Ordering::SeqCst), 1);
    assert!(RtcpCompound::parse(&first).unwrap().sender.is_some());
    let close = RtcpCompound::parse(&bye).unwrap();
    assert!(close.sender.is_some());
    assert!(close.bye_packet.is_some());
    assert!(
        RtcpCompound::parse(&receiver.read_report().unwrap())
            .unwrap()
            .receiver
            .is_some()
    );
    assert_eq!(receiver.observation().rtcp.reports_sent, 1);
    release(&receiver);
    record(
        "reentrant-close",
        vec![IvyJson::object([
            ("first", wire(&first).into()),
            ("bye", wire(&bye).into()),
            ("applicationSent", n(1)),
            ("cancelledCloseStayedPending", true.into()),
            ("postCloseReportIsReceiver", true.into()),
        ])],
    );
}
