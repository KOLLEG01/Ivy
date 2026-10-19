use crate::rtcp::{hex, original};
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
fn time(v: &IvyJson) -> RtcpWallTime {
    RtcpWallTime::new(ticks(v.get("local").unwrap()), ticks(v.get("utc").unwrap())).unwrap()
}
fn hexnum(v: &IvyJson) -> u64 {
    u64::from_str_radix(&v.as_str().unwrap(), 16).unwrap()
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
fn original_ntp_clock_era_local_comparison_float_rounding_and_precise_timeout_are_retained() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original NTP clock unavailable; differential not executed");
        return;
    }
    let input = clock_case();
    let old = original(&[input.clone()]).remove(0);
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

struct ScriptClock {
    values: Mutex<VecDeque<RtcpWallTime>>,
}
impl ScriptClock {
    fn push(&self, t: RtcpWallTime) {
        self.values.lock().unwrap().push_back(t);
    }
    fn empty(&self) {
        assert!(self.values.lock().unwrap().is_empty());
    }
}
impl RtcpRuntime for ScriptClock {
    fn now(&self) -> Result<RtcpWallTime, MediaError> {
        self.values
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(MediaError::ClockUnavailable)
    }
    fn interval_ms(&self) -> Result<u16, MediaError> {
        Ok(2500)
    }
}
// Replay the original's observed clock quantum, not copied report arithmetic.
// Its full/middle NTP timestamp and actual before/after bounds determine a
// representative pair producing that exact timestamp. Rx uses the original
// public rate override and captured private transit solely to recover arrival.
fn shifted(t: RtcpWallTime, delta: i64) -> RtcpWallTime {
    RtcpWallTime::new(t.local_ticks() + delta, t.utc_ticks() + delta).unwrap()
}
fn clock64(before: RtcpWallTime, target: u64) -> RtcpWallTime {
    let delta = target.wrapping_sub(before.ntp()) as f64 * 10_000_000.0 / 4_294_967_296.0;
    for adjust in -32..=32 {
        let t = shifted(before, delta as i64 + adjust);
        if t.ntp() == target {
            return t;
        }
    }
    panic!("Original full NTP quantum not found: before={before:?},target={target:016x}");
}
fn report_clock(row: &IvyJson) -> RtcpWallTime {
    let before = time(row.get("before").unwrap());
    if let Some(value) = row.get("ntp").filter(|v| !v.is_null()) {
        return clock64(before, hexnum(value));
    }
    if let Some(value) = row.get("sampleTime").filter(|v| !v.is_null()) {
        let target = value.as_f64().unwrap() as u32;
        let delta = u64::from(target.wrapping_sub(before.ntp32())) * 10_000_000 / 65536;
        for adjust in -180..=180 {
            let t = shifted(before, delta as i64 + adjust);
            if t.ntp32() == target {
                return t;
            }
        }
        panic!("Original middle NTP quantum not found: {row:?}");
    }
    before
}
fn sent_clock(row: &IvyJson) -> RtcpWallTime {
    clock64(
        time(row.get("before").unwrap()),
        hexnum(row.get("lastNtp").unwrap()),
    )
}
fn action(kind: &str, values: &[u32]) -> IvyJson {
    IvyJson::Array(
        std::iter::once(kind.into())
            .chain(values.iter().copied().map(n))
            .collect(),
    )
}
#[test]
fn actual_original_session_actions_match_composed_single_source_activity_lsr_sampling_and_bye() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original RTCP session unavailable; differential not executed");
        return;
    }
    let mut inputs = Vec::new();
    for first in [0, 7, u32::MAX] {
        for sequences in [[65534, 0, 65535], [1000, 1001, 1001]] {
            inputs.push(IvyJson::object([
                ("op", "session".into()),
                (
                    "actions",
                    IvyJson::Array(vec![
                        action("report", &[]),
                        IvyJson::Array(vec!["sr".into(), "8123456789abcdef".into()]),
                        action("report", &[]),
                        action("rx", &[sequences[0], 1000000, first, 160]),
                        action("rx", &[sequences[1], 1000320, 8, 80]),
                        action("report", &[]),
                        action("rx", &[sequences[2], 1000160, first, 0]),
                        action("tx", &[4, 2000000, 9, 160]),
                        action("report", &[]),
                        action("timer", &[]),
                        action("report", &[]),
                        IvyJson::Array(vec!["sr".into(), "823456789abcdef0".into()]),
                        action("report", &[]),
                        action("remove", &[8]),
                        action("report", &[]),
                        action("remove", &[first]),
                        action("report", &[]),
                        action("rx", &[1, 3200, 8, 80]),
                        action("report", &[]),
                        action("close", &[]),
                        action("close", &[]),
                    ]),
                ),
            ]));
        }
    }
    let old = original(&inputs);
    let mut actual = Vec::new();
    for (input, output) in inputs.iter().zip(&old) {
        assert_eq!(
            output.get("ok").unwrap().as_bool(),
            Some(true),
            "{output:?}"
        );
        let runtime = Arc::new(ScriptClock {
            values: Mutex::new(VecDeque::new()),
        });
        let created = time(output.get("created").unwrap());
        runtime.push(created);
        let mut session = RtcpSession::new(
            9,
            &output.get("cname").unwrap().as_str().unwrap(),
            runtime.clone(),
        )
        .unwrap();
        session.set_payload_sample_rate(10000000);
        let mut elapsed = Duration::ZERO;
        let mut snapshots = Vec::new();
        for (action, row) in input
            .get("actions")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .zip(output.get("trace").unwrap().as_array().unwrap())
        {
            let a = action.as_array().unwrap();
            let kind = a[0].as_str().unwrap();
            if session.observation().started_local_ticks.is_none()
                && !row.get("started").unwrap().is_null()
            {
                runtime.push(time(row.get("started").unwrap()));
                session.start(|| elapsed).unwrap();
            }
            let header = || RtpHeader {
                marker: false,
                payload_type: 0,
                sequence: a[1].as_f64().unwrap() as u16,
                timestamp: a[2].as_f64().unwrap() as u32,
                source: a[3].as_f64().unwrap() as u32,
            };
            let report = match kind.as_str() {
                "rx" => {
                    runtime.push(time(row.get("activity").unwrap()));
                    let arrival = number(row, "arrival");
                    let mut chosen = None;
                    for adjust in -4..=4 {
                        let t = shifted(created, i64::from(arrival) + adjust);
                        if t.arrival(created, 10000000) == arrival {
                            chosen = Some(t);
                            break;
                        }
                    }
                    runtime.push(chosen.expect("Original arrival quantum"));
                    session
                        .received(header(), a[4].as_f64().unwrap() as usize)
                        .unwrap();
                    None
                }
                "tx" => {
                    runtime.push(sent_clock(row));
                    session
                        .sent(header(), a[4].as_f64().unwrap() as usize, || elapsed)
                        .unwrap();
                    None
                }
                "sr" => {
                    runtime.push(time(row.get("activity").unwrap()));
                    if session.observation().reception_source.is_some() {
                        runtime.push(time(row.get("lsr").unwrap()));
                    }
                    session
                        .report_received(Some(hexnum(&a[1])), || elapsed)
                        .unwrap();
                    None
                }
                "report" => {
                    runtime.push(report_clock(row));
                    Some(session.report().unwrap())
                }
                "timer" => {
                    elapsed = session.observation().next.unwrap();
                    runtime.push(time(row.get("before").unwrap()));
                    assert_eq!(session.begin_report(elapsed).unwrap(), Some(false));
                    runtime.push(report_clock(row));
                    let bytes = session.report().unwrap();
                    elapsed += Duration::from_millis(137);
                    session.complete_report(|| elapsed).unwrap();
                    Some(bytes)
                }
                "remove" => {
                    session.remove_reception(a[1].as_f64().unwrap() as u32);
                    None
                }
                "close" => {
                    if !session.observation().closed {
                        runtime.push(report_clock(row));
                    }
                    session.close().unwrap()
                }
                _ => unreachable!(),
            };
            assert_eq!(
                report.as_ref().map(|b| hex(b)),
                row.get("hex")
                    .filter(|v| !v.is_null())
                    .map(|v| v.as_str().unwrap()),
                "{kind}: {row:?}"
            );
            let state = session.observation();
            assert_eq!(state.sent.packets, number(row, "sent"));
            assert_eq!(state.sent.octets, number(row, "sentOctets"));
            assert_eq!(state.last_ntp_sent, hexnum(row.get("lastNtp").unwrap()));
            assert_eq!(
                u32::from(state.sent.last_sequence),
                number(row, "lastSequence")
            );
            assert_eq!(state.sent.last_timestamp, number(row, "lastTimestamp"));
            assert_eq!(state.received_packets, number(row, "received"));
            assert_eq!(state.received_octets, number(row, "receivedOctets"));
            assert_eq!(
                state.reception_source,
                row.get("receptionSource")
                    .filter(|v| !v.is_null())
                    .map(|v| v.as_f64().unwrap() as u32)
            );
            assert_eq!(
                state.started_local_ticks,
                row.get("started")
                    .filter(|v| !v.is_null())
                    .map(|v| time(v).local_ticks())
            );
            assert_eq!(
                state.last_activity_local_ticks,
                row.get("activity")
                    .filter(|v| !v.is_null())
                    .map(|v| time(v).local_ticks())
            );
            assert_eq!(Some(state.closed), row.get("closed").unwrap().as_bool());
            runtime.empty();
            snapshots.push(IvyJson::object([
                ("kind", kind.into()),
                (
                    "wire",
                    report.map(|b| hex(&b).into()).unwrap_or(IvyJson::Null),
                ),
                (
                    "receptionSource",
                    state.reception_source.map(n).unwrap_or(IvyJson::Null),
                ),
            ]));
        }
        actual.push(IvyJson::Array(snapshots));
    }
    if let Some(path) = std::env::var_os("IVY_PHONE_RTCP_SESSION_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("inputs", IvyJson::Array(inputs)),
                    ("original", IvyJson::Array(old)),
                    ("rust", IvyJson::Array(actual)),
                ]),
                2000000,
            )
            .unwrap(),
        )
        .unwrap();
    }
}

pub(crate) struct FixedClock {
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
fn original_callback_failure_reentrant_close_and_missing_destination_keep_sent_snapshots() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original timer failure unavailable; differential not executed");
        return;
    }
    let mut inputs: Vec<_> = [false, true]
        .into_iter()
        .map(|late| IvyJson::object([("op", "timer-failure".into()), ("late", late.into())]))
        .collect();
    inputs.push(IvyJson::object([
        ("op", "timer-failure".into()),
        ("late", false.into()),
        ("closeInside", true.into()),
    ]));
    inputs.push(IvyJson::object([("op", "missing-destination".into())]));
    let old = original(&inputs);
    for (late, result) in [false, true].into_iter().zip(&old) {
        assert_eq!(result.get("ok").unwrap().as_bool(), Some(true));
        assert_eq!(result.get("after").unwrap().as_bool(), Some(!late));
        assert_eq!(result.get("closed").unwrap().as_bool(), Some(false));
        assert_eq!(result.get("afterNewSend").unwrap().as_bool(), Some(true));
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
    let closed = &old[2];
    assert_eq!(closed.get("ok").unwrap().as_bool(), Some(true));
    assert_eq!(closed.get("after").unwrap().as_bool(), Some(false));
    assert_eq!(closed.get("closed").unwrap().as_bool(), Some(true));
    assert_eq!(closed.get("afterNewSend").unwrap().as_bool(), Some(true));
    let trace = closed.get("trace").unwrap().as_array().unwrap();
    assert_eq!(trace.len(), 2);
    assert_eq!(trace[0].get("bye").unwrap().as_bool(), Some(false));
    assert_eq!(trace[1].get("bye").unwrap().as_bool(), Some(true));
    for row in trace {
        assert_eq!(row.get("sender").unwrap().as_bool(), Some(true));
    }
    let missing = &old[3];
    assert_eq!(missing.get("ok").unwrap().as_bool(), Some(true));
    assert_eq!(number(missing.get("before").unwrap(), "reportsSent"), 0);
    assert_eq!(number(missing.get("after").unwrap(), "reportsSent"), 1);
    assert_eq!(missing.get("after"), missing.get("closed"));
    // The corresponding real Rust callback/close/socket cases are in the
    // private runtime tests; they exercise the retained owner, not a mock send.
    if let Some(path) = std::env::var_os("IVY_PHONE_RTCP_FAILURE_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("inputs", IvyJson::Array(inputs)),
                    ("original", IvyJson::Array(old)),
                ]),
                100000,
            )
            .unwrap(),
        )
        .unwrap();
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
