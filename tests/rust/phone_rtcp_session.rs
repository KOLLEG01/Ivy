use crate::rtcp::protocol_cases;
use ivy_contracts::{IvyJson, json::stringify};
use ivy_phone_bridge::media::{
    MediaError, RtcpCompound, RtcpReportSchedule, RtcpRuntime, RtcpSession, RtcpWallTime,
    RtpHeader, rtcp_interval,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
fn n(v: impl Into<f64>) -> IvyJson {
    IvyJson::Number(v.into())
}
fn ticks(v: &IvyJson) -> i64 {
    v.as_str().unwrap().parse().unwrap()
}
fn number(v: &IvyJson, key: &str) -> u32 {
    v.get(key).unwrap().as_f64().unwrap() as u32
}
fn clock_case() -> IvyJson {
    let mut values = Vec::new();
    for base in [
        0_i64,
        599266080000000000,
        642215752960000000,
        RtcpWallTime::UNIX_TICKS,
        RtcpWallTime::MAX_TICKS,
    ] {
        for delta in [0, 1, -1, 10000, -10000, 36000000000, -36000000000] {
            if let Some(value) = base
                .checked_add(delta)
                .filter(|v| (0..=RtcpWallTime::MAX_TICKS).contains(v))
            {
                values.push(value.to_string().into());
            }
        }
    }
    IvyJson::object([
        ("op", "clock".into()),
        ("utcTicks", IvyJson::Array(values)),
        (
            "casts",
            IvyJson::Array(vec![n(-1.5), n(-0.5), n(0), n(1.5), n(4294967296_f64)]),
        ),
    ])
}
#[test]
fn ntp_clock_vectors_and_native_timeout_boundaries_are_retained() {
    let input = clock_case();
    let old = protocol_cases("clock", &[input.clone()]).remove(0);
    assert_eq!(old.get("ok").unwrap().as_bool(), Some(true), "{old:?}");
    let trace = old.get("trace").unwrap().as_array().unwrap();
    assert_eq!(trace.len(), 87);
    for row in trace {
        let clock = RtcpWallTime::new(
            ticks(row.get("localTicks").unwrap()),
            ticks(row.get("utcTicks").unwrap()),
        )
        .unwrap();
        assert_eq!(
            format!("{:016x}", clock.ntp()),
            row.get("ntp").unwrap().as_str().unwrap(),
            "{row:?}"
        );
        assert_eq!(clock.ntp32(), number(row, "ntp32"));
    }
    for row in old.get("casts").unwrap().as_array().unwrap() {
        let v = row.get("value").unwrap().as_f64().unwrap();
        assert_eq!(v as u32, number(row, "uint32"));
        assert_eq!(
            (v as u64).to_string(),
            row.get("uint64").unwrap().as_str().unwrap()
        );
    }
    let mut schedule = RtcpReportSchedule::from_wall_ticks(0);
    schedule.start(Duration::ZERO, 2500).unwrap();
    assert!(
        !schedule
            .begin_report_ticks(Duration::from_millis(2500), 300000000)
            .unwrap()
            .unwrap()
            .new_timeout
    );
    schedule
        .complete_report(Duration::from_millis(2500), 2500, 0)
        .unwrap();
    assert!(
        schedule
            .begin_report_ticks(Duration::from_millis(5000), 300000001)
            .unwrap()
            .unwrap()
            .new_timeout
    );
    for (value, expected) in [(0, 2500), (5000, 7500), (5001, 2500)] {
        assert_eq!(rtcp_interval(|| Ok(value)), Ok(expected));
    }
    let max = 1_u64 << 32;
    let limit = (max - max % 5001) as u32;
    let mut count = 0;
    assert_eq!(
        rtcp_interval(|| {
            count += 1;
            Ok(limit + 1)
        }),
        Err(MediaError::ClockUnavailable)
    );
    assert_eq!(count, 10);
    let mut entropy = VecDeque::from([u32::MAX, limit]);
    assert_eq!(rtcp_interval(|| Ok(entropy.pop_front().unwrap())), Ok(2500));
    assert!(entropy.is_empty());
    assert!(RtcpWallTime::new(-1, 0).is_err());
    assert!(RtcpWallTime::new(0, RtcpWallTime::MAX_TICKS + 1).is_err());
    #[cfg(windows)]
    {
        let runtime = ivy_phone_bridge::media::WindowsRtcpRuntime;
        for _ in 0..128 {
            let before = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                / 100;
            let clock = runtime.now().unwrap();
            let after = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                / 100;
            let unix = clock.utc_ticks() - RtcpWallTime::UNIX_TICKS;
            assert!(unix >= before as i64 && unix <= after as i64);
            assert!((2500..=7500).contains(&runtime.interval_ms().unwrap()));
        }
    }
    if let Some(path) = std::env::var_os("IVY_PHONE_RTCP_CLOCK_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([("input", input), ("original", old)]),
                1000000,
            )
            .unwrap(),
        )
        .unwrap();
    }
}

fn shifted(t: RtcpWallTime, delta: i64) -> RtcpWallTime {
    RtcpWallTime::new(t.local_ticks() + delta, t.utc_ticks() + delta).unwrap()
}

struct FixedClock {
    pub time: Mutex<RtcpWallTime>,
    pub fail_interval: AtomicBool,
}
impl FixedClock {
    pub fn new() -> Self {
        Self {
            time: Mutex::new(
                RtcpWallTime::new(
                    RtcpWallTime::UNIX_TICKS + 17923900000000000,
                    RtcpWallTime::UNIX_TICKS + 17923900000000000,
                )
                .unwrap(),
            ),
            fail_interval: AtomicBool::new(false),
        }
    }
}
impl RtcpRuntime for FixedClock {
    fn now(&self) -> Result<RtcpWallTime, MediaError> {
        Ok(*self.time.lock().unwrap())
    }
    fn interval_ms(&self) -> Result<u16, MediaError> {
        if self.fail_interval.load(Ordering::SeqCst) {
            Err(MediaError::ClockUnavailable)
        } else {
            Ok(2500)
        }
    }
}
#[test]
fn callback_and_interval_failures_preserve_native_sender_snapshots() {
    for late in [false, true] {
        let runtime = Arc::new(FixedClock::new());
        let mut session = RtcpSession::new(9, "fixture", runtime.clone()).unwrap();
        let header = RtpHeader {
            marker: false,
            payload_type: 0,
            sequence: 1,
            timestamp: 777,
            source: 9,
        };
        session.sent(header, 160, || Duration::ZERO).unwrap();
        assert_eq!(
            session.begin_report(Duration::from_millis(2500)).unwrap(),
            Some(false)
        );
        assert!(
            RtcpCompound::parse(&session.report().unwrap())
                .unwrap()
                .sender
                .is_some()
        );
        if late {
            runtime.fail_interval.store(true, Ordering::SeqCst);
            let e = session
                .complete_report(|| Duration::from_millis(2500))
                .unwrap_err();
            session.stop_timer(e);
        } else {
            session.stop_timer(MediaError::ReportCallbackFailed);
        }
        assert_eq!(
            RtcpCompound::parse(&session.report().unwrap())
                .unwrap()
                .sender
                .is_some(),
            !late
        );
        runtime.fail_interval.store(false, Ordering::SeqCst);
        session
            .sent(header, 160, || Duration::from_millis(5000))
            .unwrap();
        assert!(
            RtcpCompound::parse(&session.report().unwrap())
                .unwrap()
                .sender
                .is_some()
        );
        assert_eq!(session.observation().next, None);
        assert!(!session.observation().closed);
        assert!(!session.start(|| Duration::from_millis(6000)).unwrap());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn bound_receive_updates_default_ntp_statistics_lsr_and_only_matched_bye_removes_the_source()
{
    use crate::sip_receiver::{configure, send, setup, wait};
    use ivy_phone_bridge::media::{RtcpReportBuilder, RtcpSenderInfo};
    let (receiver, audio, events, peer, control) = setup(42600, false, false).await;
    let runtime = Arc::new(FixedClock::new());
    receiver
        .enable_reporting(17, "fixture", runtime.clone())
        .unwrap();
    let local = receiver.observation().transport;
    send(&peer, local.local_rtp, 1, 0, 7, &[0; 160]).await;
    wait(|| {
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .received_packets
            == 1
    })
    .await;
    let first = receiver.observation().rtcp_session.unwrap();
    assert_eq!(first.reception_source, Some(7));
    assert_eq!(first.payload_sample_rate, 0);
    assert_eq!(first.next, None);
    send(&peer, local.local_rtp, 2, 127, 8, &[0]).await;
    wait(|| receiver.observation().unsupported_payload_packets == 1).await;
    assert_eq!(receiver.observation().rtcp_session.unwrap(), first);
    let ntp = 0x8123456789abcdef_u64;
    let sender = RtcpReportBuilder::sender_packet(
        7,
        RtcpSenderInfo {
            ntp_timestamp: ntp,
            rtp_timestamp: 777,
            packet_count: 1,
            octet_count: 160,
        },
        &[],
    )
    .unwrap();
    async fn send_control(
        receiver: &ivy_phone_bridge::media::SipRtpReceiver,
        control: &tokio::net::UdpSocket,
        events: &crate::sip_receiver::Events,
        bytes: &[u8],
    ) {
        let before = events.controls();
        control
            .send_to(bytes, receiver.observation().transport.local_rtcp)
            .await
            .unwrap();
        wait(|| events.controls() == before + 1).await;
    }
    send_control(&receiver, &control, &events, &sender).await;
    let bytes = receiver.read_report().unwrap();
    let sample = RtcpCompound::parse(&bytes)
        .unwrap()
        .receiver
        .unwrap()
        .blocks
        .iter()
        .next()
        .unwrap();
    assert_eq!(sample.source, 7);
    assert_eq!(sample.last_sender_report, (ntp >> 16) as u32);
    assert_eq!(sample.delay_since_sender_report, 0);
    assert_eq!(receiver.observation().rtcp.reports_received, 1);
    assert!(
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .started_local_ticks
            .is_some()
    );
    {
        let mut t = runtime.time.lock().unwrap();
        *t = shifted(*t, 10_000_000);
    }
    let bytes = receiver.read_report().unwrap();
    assert_eq!(
        RtcpCompound::parse(&bytes)
            .unwrap()
            .receiver
            .unwrap()
            .blocks
            .iter()
            .next()
            .unwrap()
            .delay_since_sender_report,
        65536
    );
    let bye = RtcpReportBuilder::new(7, "fixture").unwrap().bye_packet("");
    let frozen = receiver.observation();
    control
        .send_to(&[bye.clone(), sender, vec![0]].concat(), local.local_rtcp)
        .await
        .unwrap();
    send_control(&receiver, &control, &events, &[0x80, 207, 0, 1, 0, 0, 0, 7]).await;
    assert_eq!(receiver.observation().rtcp_session, frozen.rtcp_session);
    assert_eq!(receiver.observation().rtcp, frozen.rtcp);
    assert_eq!(receiver.observation().transport.peer, frozen.transport.peer);
    let rr = RtcpReportBuilder::receiver_packet(7, &[]).unwrap();
    send_control(&receiver, &control, &events, &[bye, rr].concat()).await;
    assert_eq!(receiver.observation().rtcp.learned_source, Some(0));
    assert_eq!(
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .reception_source,
        None
    );
    assert_eq!(
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .last_activity_local_ticks,
        first.last_activity_local_ticks
    );
    send(&peer, local.local_rtp, 3, 0, 8, &[0; 160]).await;
    wait(|| {
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .received_packets
            == 2
    })
    .await;
    configure(
        &receiver,
        peer.local_addr().unwrap(),
        control.local_addr().unwrap(),
        false,
    );
    assert_eq!(receiver.observation().rtcp.learned_source, Some(0));
    assert_eq!(
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .reception_source,
        Some(8)
    );
    audio.set(false, 99);
    send(&peer, local.local_rtp, 4, 0, 9, &[0; 160]).await;
    wait(|| {
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .received_packets
            == 3
    })
    .await;
    assert_eq!(receiver.observation().rtcp.learned_source, Some(9));
    assert_eq!(
        receiver
            .observation()
            .rtcp_session
            .as_ref()
            .unwrap()
            .reception_source,
        Some(8)
    );
    receiver.close().await;
    let mut data = [0; 2048];
    let (size, _) = tokio::time::timeout(Duration::from_secs(2), control.recv_from(&mut data))
        .await
        .unwrap()
        .unwrap();
    let close = RtcpCompound::parse(&data[..size]).unwrap();
    assert!(close.bye_packet.is_some());
    assert_eq!(close.receiver.unwrap().source, 17);
    assert_eq!(
        receiver.observation().rtcp.reports_sent,
        0,
        "Original close unsubscribes the application send counter before BYE"
    );
    assert!(receiver.observation().rtcp_session.unwrap().closed);
    let _rtp = std::net::UdpSocket::bind(local.local_rtp).unwrap();
    let _rtcp = std::net::UdpSocket::bind(local.local_rtcp).unwrap();
}
