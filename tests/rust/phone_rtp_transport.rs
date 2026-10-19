use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_phone_bridge::media::{
    Codec, G711Codec, RtpAudioPacket, RtpDatagramSink, RtpHeader, RtpPacket, RtpPeer,
    RtpReceiveQueue, RtpTransport, RtpTransportError,
};
use std::{
    net::{SocketAddr, UdpSocket as StdUdpSocket},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    sync::Notify,
    time::{Instant, timeout},
};

fn header(sequence: u16) -> RtpHeader {
    RtpHeader {
        marker: true,
        payload_type: 0,
        sequence,
        timestamp: u32::MAX,
        source: 0x12345678,
    }
}
fn encoded(header: RtpHeader, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 12 + payload.len()];
    header.write(payload, &mut bytes).unwrap();
    bytes
}
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::new();
    for byte in bytes {
        write!(text, "{byte:02x}").unwrap();
    }
    text
}
fn packet_snapshot(packet: RtpPacket<'_>) -> IvyJson {
    IvyJson::object([
        ("sequence", IvyJson::Number(packet.header.sequence.into())),
        ("timestamp", IvyJson::Number(packet.header.timestamp.into())),
        ("source", IvyJson::Number(packet.header.source.into())),
        (
            "payloadType",
            IvyJson::Number(packet.header.payload_type.into()),
        ),
        (
            "marker",
            IvyJson::Number(if packet.header.marker { 1.0 } else { 0.0 }),
        ),
        ("payload", hex(packet.payload).into()),
    ])
}
fn layout_cases() -> Vec<Vec<u8>> {
    let mut cases = Vec::new();
    for csrc in [0, 1, 15] {
        for extension in [None, Some(0), Some(4), Some(12)] {
            for padding in [0, 1, 4, 255] {
                for length in [0, 1, 4, 160] {
                    for marker in [false, true] {
                        let mut bytes = encoded(
                            RtpHeader {
                                marker,
                                ..header(65535)
                            },
                            &[],
                        );
                        bytes[0] |= csrc;
                        for source in 0..csrc {
                            bytes.extend_from_slice(&u32::from(source).to_be_bytes());
                        }
                        if let Some(length) = extension {
                            bytes[0] |= 0x10;
                            bytes.extend_from_slice(&[0xbe, 0xde]);
                            bytes.extend_from_slice(&((length / 4) as u16).to_be_bytes());
                            bytes.extend((0..length).map(|i| i as u8));
                        }
                        bytes.extend((0..length).map(|i| i as u8));
                        if padding > 0 {
                            bytes[0] |= 0x20;
                            bytes.extend(std::iter::repeat_n(0, padding - 1));
                            bytes.push(padding as u8);
                        }
                        cases.push(bytes);
                    }
                }
            }
        }
    }
    cases
}
#[test]
fn rtp_layout_checks_every_header_region_before_borrowing_payload() {
    for bytes in layout_cases() {
        let packet = RtpPacket::parse(&bytes).unwrap();
        assert_eq!(packet.header.sequence, 65535);
        assert_eq!(packet.header.timestamp, u32::MAX);
        assert_eq!(
            packet.contributing_sources.len(),
            usize::from(bytes[0] & 15) * 4
        );
        if let Some(extension) = packet.extension {
            assert_eq!(extension.profile, 0xbede);
            assert_eq!(extension.data.len() % 4, 0);
        }
    }
    let mut bytes = encoded(header(0), &[1]);
    for prefix in 0..12 {
        assert!(RtpPacket::parse(&bytes[..prefix]).is_err());
    }
    for version in [0, 1, 3] {
        bytes[0] = version << 6;
        assert!(RtpPacket::parse(&bytes).is_err());
    }
    bytes[0] = 0x8f;
    assert!(RtpPacket::parse(&bytes).is_err());
    bytes[0] = 0x90;
    assert!(RtpPacket::parse(&bytes).is_err());
    bytes.extend_from_slice(&[0, 0, 255, 255]);
    assert!(RtpPacket::parse(&bytes).is_err());
    bytes = encoded(header(0), &[0]);
    bytes[0] |= 0x20;
    assert!(RtpPacket::parse(&bytes).is_err());
    bytes[12] = 2;
    assert!(RtpPacket::parse(&bytes).is_err());
    assert!(
        RtpHeader {
            payload_type: 128,
            ..header(0)
        }
        .write(&[1], &mut [0; 13])
        .is_err()
    );
    assert!(header(0).write(&[1], &mut [0; 12]).is_err());
    let mut seed = 0x11335577_u32;
    for count in 0..20_000 {
        let mut data = vec![0; count % 300];
        for byte in &mut data {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *byte = (seed >> 24) as u8;
        }
        let _ = RtpPacket::parse(&data);
    }
}

#[test]
fn original_rtp_packet_layout_matches_valid_csrc_extension_and_padding_matrix() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original RTP packet oracle unavailable; comparison not executed.");
        return;
    }
    let cases = layout_cases();
    let mut child = Command::new("pwsh")
        .args([
            "-NoProfile",
            "-File",
            "tests/fixtures/phone-rtp-packet-oracle.ps1",
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
            stringify(
                &IvyJson::Array(cases.iter().map(|b| hex(b).into()).collect()),
                2_000_000,
            )
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
    let results = parse(std::str::from_utf8(&output.stdout).unwrap(), 2_000_000).unwrap();
    let results = results.as_array().unwrap();
    assert_eq!(results.len(), cases.len());
    let rows = IvyJson::Array(
        cases
            .iter()
            .zip(results)
            .map(|(bytes, expected)| {
                IvyJson::object([
                    ("input", hex(bytes).into()),
                    ("expected", expected.clone()),
                    ("actual", packet_snapshot(RtpPacket::parse(bytes).unwrap())),
                ])
            })
            .collect(),
    );
    if let Some(path) = std::env::var_os("IVY_PHONE_RTP_PACKET_TRACE") {
        std::fs::write(path, stringify(&rows, 4_000_000).unwrap()).unwrap();
    }
    let failures: Vec<_> = cases
        .iter()
        .zip(results)
        .enumerate()
        .filter_map(|(i, (bytes, expected))| {
            let actual = packet_snapshot(RtpPacket::parse(bytes).unwrap());
            (&actual != expected).then(|| {
                format!(
                    "Case {i}: {} != {}",
                    stringify(&actual, 65536).unwrap(),
                    stringify(expected, 65536).unwrap()
                )
            })
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} mismatches: {}",
        failures.len(),
        failures.into_iter().take(10).collect::<Vec<_>>().join("\n")
    );
}

#[derive(Default)]
struct Sink {
    packets: Mutex<Vec<(SocketAddr, RtpHeader, Vec<u8>)>>,
    controls: Mutex<Vec<Vec<u8>>>,
    changed: Notify,
}
impl RtpDatagramSink for Sink {
    fn rtp(&self, source: SocketAddr, packet: RtpPacket<'_>) {
        self.packets
            .lock()
            .unwrap()
            .push((source, packet.header, packet.payload.to_vec()));
        self.changed.notify_one();
    }
    fn rtcp(&self, _: SocketAddr, bytes: &[u8]) {
        self.controls.lock().unwrap().push(bytes.to_vec());
        self.changed.notify_one();
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(4), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn pair(transport: &RtpTransport) -> RtpPeer {
    let o = transport.observation();
    RtpPeer::new(o.local_rtp, o.local_rtcp).unwrap()
}
fn released(transport: &RtpTransport) {
    let o = transport.observation();
    assert!(o.cleanup_confirmed);
    let _rtp = StdUdpSocket::bind(o.local_rtp).unwrap();
    let _rtcp = StdUdpSocket::bind(o.local_rtcp).unwrap();
}
async fn exchange(interface: SocketAddr) {
    let left_sink = Arc::new(Sink::default());
    let right_sink = Arc::new(Sink::default());
    let left = RtpTransport::bind(interface, 36000, 36200, left_sink.clone()).unwrap();
    let right = RtpTransport::bind(interface, 36202, 36400, right_sink.clone()).unwrap();
    assert_eq!(
        left.send_rtp(header(0), &[1]),
        Err(RtpTransportError::PeerUnavailable)
    );
    left.set_peer(Some(pair(&right))).unwrap();
    right.set_peer(Some(pair(&left))).unwrap();
    left.send_rtp(header(65535), &[1, 2, 3, 4]).unwrap();
    until(|| right_sink.packets.lock().unwrap().len() == 1).await;
    assert_eq!(
        right_sink.packets.lock().unwrap()[0],
        (
            left.observation().local_rtp,
            header(65535),
            vec![1, 2, 3, 4]
        )
    );
    let report = [0x80, 201, 0, 1, 0x12, 0x34, 0x56, 0x78];
    right.send_rtcp(&report).unwrap();
    until(|| left_sink.controls.lock().unwrap().len() == 1).await;
    assert_eq!(left_sink.controls.lock().unwrap()[0], report);
    let foreign = UdpSocket::bind(interface).await.unwrap();
    let control = UdpSocket::bind(interface).await.unwrap();
    foreign
        .send_to(&encoded(header(1), &[5]), right.observation().local_rtp)
        .await
        .unwrap();
    control
        .send_to(&report, right.observation().local_rtcp)
        .await
        .unwrap();
    until(|| {
        right.observation().counters.foreign_rtp == 1
            && right.observation().counters.foreign_rtcp == 1
    })
    .await;
    assert_eq!(right_sink.packets.lock().unwrap().len(), 1);
    right
        .set_peer(Some(
            RtpPeer::new(foreign.local_addr().unwrap(), control.local_addr().unwrap()).unwrap(),
        ))
        .unwrap();
    foreign
        .send_to(&[0, 1, 2], right.observation().local_rtp)
        .await
        .unwrap();
    until(|| right.observation().counters.malformed_rtp == 1).await;
    right.set_peer(Some(pair(&left))).unwrap();
    left.send_rtp(header(0), &vec![7; 8192]).unwrap();
    until(|| right_sink.packets.lock().unwrap().len() == 2).await;
    assert_eq!(right_sink.packets.lock().unwrap()[1].2.len(), 8192);
    assert_eq!(
        left.send_rtp(header(0), &[]),
        Err(RtpTransportError::InvalidPacket)
    );
    assert_eq!(
        left.send_rtp(header(0), &vec![0; 8193]),
        Err(RtpTransportError::InvalidPacket)
    );
    right.request_close();
    assert_eq!(
        right.send_rtp(header(0), &[1]),
        Err(RtpTransportError::Closed)
    );
    assert_eq!(right.set_peer(None), Err(RtpTransportError::Closed));
    right.close().await;
    left.close().await;
    released(&right);
    released(&left);
}
#[tokio::test]
async fn actual_ipv4_pair_preserves_source_ports_and_rejects_foreign_or_malformed_media() {
    exchange("127.0.0.1:0".parse().unwrap()).await;
}
#[tokio::test]
async fn actual_ipv6_pair_preserves_source_ports_and_rejects_foreign_or_malformed_media() {
    exchange("[::1]:0".parse().unwrap()).await;
}

#[tokio::test]
async fn partial_rtcp_collision_releases_rtp_before_trying_the_next_even_pair() {
    let sink = Arc::new(Sink::default());
    for (first, last) in [
        (0, 0),
        (1023, 1024),
        (1024, 1025),
        (65534, 65535),
        (30002, 30000),
    ] {
        assert!(matches!(
            RtpTransport::bind("127.0.0.1:0".parse().unwrap(), first, last, sink.clone()),
            Err(RtpTransportError::InvalidBinding)
        ));
    }
    let mut selected = None;
    for port in (40000..40200).step_by(2) {
        if let (Ok(rtp), Ok(rtcp)) = (
            StdUdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, port)),
            StdUdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, port + 1)),
        ) {
            selected = Some((port, rtp, rtcp));
            break;
        }
    }
    let (port, rtp, rtcp) = selected.unwrap();
    drop(rtp);
    assert!(matches!(
        RtpTransport::bind("127.0.0.1:0".parse().unwrap(), port, port, sink.clone()),
        Err(RtpTransportError::AddressInUse)
    ));
    let channel =
        RtpTransport::bind("127.0.0.1:0".parse().unwrap(), port, port + 20, sink).unwrap();
    assert!(channel.observation().local_rtp.port() > port);
    let free = StdUdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
    drop(free);
    drop(rtcp);
    channel.close().await;
    released(&channel);
}

struct BlockingSink {
    entered: Notify,
    release: std::sync::Condvar,
    ready: Mutex<bool>,
}
impl RtpDatagramSink for BlockingSink {
    fn rtp(&self, _: SocketAddr, _: RtpPacket<'_>) {
        self.entered.notify_one();
        let ready = self.ready.lock().unwrap();
        let (ready, _) = self
            .release
            .wait_timeout_while(ready, Duration::from_secs(8), |ready| !*ready)
            .unwrap();
        assert!(*ready, "Owned test callback was not released");
    }
    fn rtcp(&self, _: SocketAddr, _: &[u8]) {}
}
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn lost_close_observer_retains_both_sockets_until_running_receive_callback_stops() {
    let sink = Arc::new(BlockingSink {
        entered: Notify::new(),
        release: std::sync::Condvar::new(),
        ready: Mutex::new(false),
    });
    let transport =
        RtpTransport::bind("127.0.0.1:0".parse().unwrap(), 38000, 38200, sink.clone()).unwrap();
    let source = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    transport
        .set_peer(Some(
            RtpPeer::new(source.local_addr().unwrap(), source.local_addr().unwrap()).unwrap(),
        ))
        .unwrap();
    source
        .send_to(&encoded(header(1), &[1]), transport.observation().local_rtp)
        .await
        .unwrap();
    timeout(Duration::from_secs(3), sink.entered.notified())
        .await
        .unwrap();
    let original = transport.clone();
    let observer = tokio::spawn(async move {
        original.close().await;
    });
    until(|| transport.observation().closed).await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    let pending = transport.observation();
    assert!(!pending.cleanup_confirmed);
    assert!(StdUdpSocket::bind(pending.local_rtp).is_err());
    assert!(StdUdpSocket::bind(pending.local_rtcp).is_err());
    assert_eq!(
        transport.send_rtp(header(0), &[1]),
        Err(RtpTransportError::Closed)
    );
    *sink.ready.lock().unwrap() = true;
    sink.release.notify_all();
    timeout(Duration::from_secs(3), transport.close())
        .await
        .unwrap();
    released(&transport);
}
struct PanicSink;
impl RtpDatagramSink for PanicSink {
    fn rtp(&self, _: SocketAddr, _: RtpPacket<'_>) {
        panic!("synthetic_rtp_sink_failure");
    }
    fn rtcp(&self, _: SocketAddr, _: &[u8]) {}
}
#[tokio::test]
async fn receive_panic_is_observed_and_last_handle_drop_still_releases_original_pair() {
    let transport = RtpTransport::bind(
        "127.0.0.1:0".parse().unwrap(),
        38400,
        38600,
        Arc::new(PanicSink),
    )
    .unwrap();
    let source = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    transport
        .set_peer(Some(
            RtpPeer::new(source.local_addr().unwrap(), source.local_addr().unwrap()).unwrap(),
        ))
        .unwrap();
    source
        .send_to(&encoded(header(1), &[1]), transport.observation().local_rtp)
        .await
        .unwrap();
    until(|| transport.observation().cleanup_confirmed).await;
    assert!(transport.observation().failed);
    transport.close().await;
    released(&transport);
    let live = RtpTransport::bind(
        "127.0.0.1:0".parse().unwrap(),
        38800,
        39000,
        Arc::new(Sink::default()),
    )
    .unwrap();
    let local = live.observation();
    drop(live);
    until(|| {
        let rtp = StdUdpSocket::bind(local.local_rtp);
        let rtcp = StdUdpSocket::bind(local.local_rtcp);
        rtp.is_ok() && rtcp.is_ok()
    })
    .await;
}

struct AudioState {
    queue: RtpReceiveQueue,
    open: bool,
    generation: u64,
    signs: Vec<i32>,
    headers: Vec<RtpHeader>,
}
struct AudioSink {
    state: Mutex<AudioState>,
    clock: Instant,
    codec: G711Codec,
}
impl AudioSink {
    fn new() -> Self {
        Self {
            state: Mutex::new(AudioState {
                queue: RtpReceiveQueue::new(60).unwrap(),
                open: true,
                generation: 1,
                signs: Vec::new(),
                headers: Vec::new(),
            }),
            clock: Instant::now(),
            codec: G711Codec::new(&Codec::Pcmu.offered_formats()[0]).unwrap(),
        }
    }
    fn drain(&self) {
        let mut state = self.state.lock().unwrap();
        self.drain_locked(&mut state);
    }
    fn drain_locked(&self, state: &mut AudioState) {
        while let Some((packet, _)) = state.queue.take(self.clock.elapsed(), false).unwrap() {
            assert_eq!(packet.generation, state.generation);
            let total: i64 = self
                .codec
                .decode(&packet.payload)
                .unwrap()
                .into_iter()
                .map(i64::from)
                .sum();
            state.signs.push(total.signum() as i32);
        }
    }
}
impl RtpDatagramSink for AudioSink {
    fn rtp(&self, _: SocketAddr, packet: RtpPacket<'_>) {
        let mut state = self.state.lock().unwrap();
        state.headers.push(packet.header);
        if !state.open {
            state
                .queue
                .skip(
                    packet.header.source,
                    packet.header.sequence,
                    self.clock.elapsed(),
                )
                .unwrap();
            return;
        }
        let audio = RtpAudioPacket {
            sequence: packet.header.sequence,
            timestamp: packet.header.timestamp,
            source: packet.header.source,
            payload_type: packet.header.payload_type,
            payload: packet.payload.to_vec(),
            generation: state.generation,
        };
        state.queue.add(&audio, self.clock.elapsed()).unwrap();
        self.drain_locked(&mut state);
    }
    fn rtcp(&self, _: SocketAddr, _: &[u8]) {}
}
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn original_managed_peer_exchanges_actual_rtp_rtcp_and_ordered_g711_with_rust_channel() {
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        process::Command,
    };
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original RTP peer unavailable; interoperability not executed.");
        return;
    }
    let sink = Arc::new(AudioSink::new());
    let channel =
        RtpTransport::bind("127.0.0.1:0".parse().unwrap(), 39200, 39400, sink.clone()).unwrap();
    let mut child = Command::new("pwsh")
        .args(["-NoProfile", "-File", "tests/fixtures/phone-rtp-peer.ps1"])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    async fn read(output: &mut BufReader<tokio::process::ChildStdout>) -> IvyJson {
        let mut line = String::new();
        let count = timeout(Duration::from_secs(15), output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(count > 0, "Original peer exited; see captured stderr");
        parse(&line, 65536).unwrap()
    }
    async fn command(
        input: &mut tokio::process::ChildStdin,
        output: &mut BufReader<tokio::process::ChildStdout>,
        text: &str,
        trace: &mut Vec<IvyJson>,
    ) -> IvyJson {
        input
            .write_all(format!("{text}\n").as_bytes())
            .await
            .unwrap();
        input.flush().await.unwrap();
        let reply = read(output).await;
        trace.push(IvyJson::object([
            ("command", parse(text, 65536).unwrap()),
            ("reply", reply.clone()),
        ]));
        if let Some(path) = std::env::var_os("IVY_PHONE_RTP_ORACLE_TRACE") {
            std::fs::write(
                path,
                stringify(&IvyJson::Array(trace.clone()), 2_000_000).unwrap(),
            )
            .unwrap();
        }
        reply
    }
    let hello = read(&mut output).await;
    let peer = RtpPeer::new(
        format!(
            "127.0.0.1:{}",
            hello.get("rtp").unwrap().as_f64().unwrap() as u16
        )
        .parse()
        .unwrap(),
        format!(
            "127.0.0.1:{}",
            hello.get("rtcp").unwrap().as_f64().unwrap() as u16
        )
        .parse()
        .unwrap(),
    )
    .unwrap();
    channel.set_peer(Some(peer)).unwrap();
    let local = channel.observation();
    let mut trace = vec![IvyJson::object([("hello", hello)])];
    command(
        &mut input,
        &mut output,
        &format!(
            r#"{{"op":"peer","rtp":{},"rtcp":{}}}"#,
            local.local_rtp.port(),
            local.local_rtcp.port()
        ),
        &mut trace,
    )
    .await;
    async fn send(
        input: &mut tokio::process::ChildStdin,
        output: &mut BufReader<tokio::process::ChildStdout>,
        sequence: u16,
        timestamp: u32,
        positive: bool,
        trace: &mut Vec<IvyJson>,
    ) {
        command(input,output,&format!(r#"{{"op":"send","sequence":{sequence},"timestamp":{timestamp},"positive":{positive}}}"#),trace).await;
    }
    send(&mut input, &mut output, 65534, 0, true, &mut trace).await;
    until(|| sink.state.lock().unwrap().signs.len() == 1).await;
    for (sequence, timestamp, positive) in [(0, 320, true), (0, 320, true), (65535, 160, false)] {
        send(
            &mut input,
            &mut output,
            sequence,
            timestamp,
            positive,
            &mut trace,
        )
        .await;
    }
    until(|| sink.state.lock().unwrap().signs.len() == 3).await;
    assert_eq!(sink.state.lock().unwrap().signs, [1, -1, 1]);
    assert_eq!(
        sink.state.lock().unwrap().queue.status().duplicate_packets,
        1
    );
    send(&mut input, &mut output, 2, 640, false, &mut trace).await;
    until(|| sink.state.lock().unwrap().queue.status().queued_packets == 1).await;
    tokio::time::sleep(Duration::from_millis(75)).await;
    sink.drain();
    assert_eq!(sink.state.lock().unwrap().signs, [1, -1, 1, -1]);
    send(&mut input, &mut output, 1, 480, true, &mut trace).await;
    until(|| sink.state.lock().unwrap().queue.status().late_packets >= 1).await;
    {
        let mut state = sink.state.lock().unwrap();
        state.open = false;
        state.generation += 1;
        state.queue.clear();
    }
    send(&mut input, &mut output, 3, 800, true, &mut trace).await;
    until(|| sink.state.lock().unwrap().queue.status().gated_packets >= 1).await;
    {
        let mut state = sink.state.lock().unwrap();
        state.open = true;
        state.generation += 1;
    }
    send(&mut input, &mut output, 3, 800, true, &mut trace).await;
    send(&mut input, &mut output, 4, 960, false, &mut trace).await;
    until(|| sink.state.lock().unwrap().signs.len() == 5).await;
    assert_eq!(sink.state.lock().unwrap().signs, [1, -1, 1, -1, -1]);
    let encoded = sink.codec.encode_20ms(&[8000; 160]);
    channel.send_rtp(header(65535), &encoded).unwrap();
    let second = RtpHeader {
        sequence: 0,
        timestamp: 159,
        marker: false,
        ..header(0)
    };
    channel.send_rtp(second, &encoded).unwrap();
    // A complete sender report + CNAME compound, rather than reduced-size RR.
    let mut report = vec![0x80, 200, 0, 6];
    for value in [0x12345678_u32, 3_900_000_000, 0, 159, 2, 320] {
        report.extend_from_slice(&value.to_be_bytes());
    }
    report.extend_from_slice(&[
        0x81, 202, 0, 4, 0x12, 0x34, 0x56, 0x78, 1, 7, b'f', b'i', b'x', b't', b'u', b'r', b'e', 0,
        0, 0,
    ]);
    channel.send_rtcp(&report).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let status = loop {
        let value = command(&mut input, &mut output, r#"{"op":"status"}"#, &mut trace).await;
        if value.get("packets").unwrap().as_array().unwrap().len() >= 2
            && value.get("reports").unwrap().as_f64().unwrap() >= 1.0
        {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "Original peer did not receive actual RTP/RTCP: {}",
            stringify(&value, 65536).unwrap()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let packets = status.get("packets").unwrap().as_array().unwrap();
    assert_eq!(packets.len(), 2);
    for (packet, expected) in packets.iter().zip([header(65535), second]) {
        assert_eq!(
            packet.get("sequence").unwrap().as_f64().unwrap(),
            f64::from(expected.sequence)
        );
        assert_eq!(
            packet.get("timestamp").unwrap().as_f64().unwrap(),
            f64::from(expected.timestamp)
        );
        assert_eq!(
            packet.get("ssrc").unwrap().as_f64().unwrap(),
            f64::from(expected.source)
        );
        assert_eq!(
            packet.get("marker").unwrap().as_f64().unwrap(),
            if expected.marker { 1.0 } else { 0.0 }
        );
        assert_eq!(
            packet.get("peer").unwrap().as_str().unwrap(),
            local.local_rtp.to_string()
        );
        assert_eq!(
            packet.get("payload").unwrap().as_str().unwrap(),
            hex(&encoded)
        );
    }
    command(&mut input, &mut output, r#"{"op":"stop"}"#, &mut trace).await;
    drop(input);
    assert!(
        timeout(Duration::from_secs(4), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    channel.close().await;
    released(&channel);
    let state = sink.state.lock().unwrap();
    let queue = state.queue.status();
    trace.push(IvyJson::object([
        (
            "rustSigns",
            IvyJson::Array(
                state
                    .signs
                    .iter()
                    .map(|v| IvyJson::Number((*v).into()))
                    .collect(),
            ),
        ),
        (
            "duplicatePackets",
            IvyJson::Number(queue.duplicate_packets as f64),
        ),
        ("latePackets", IvyJson::Number(queue.late_packets as f64)),
        ("gatedPackets", IvyJson::Number(queue.gated_packets as f64)),
        (
            "missingPackets",
            IvyJson::Number(queue.missing_packets as f64),
        ),
        (
            "maximumResidenceMs",
            IvyJson::Number(queue.maximum_residence_ms),
        ),
    ]));
    if let Some(path) = std::env::var_os("IVY_PHONE_RTP_ORACLE_TRACE") {
        std::fs::write(path, stringify(&IvyJson::Array(trace), 2_000_000).unwrap()).unwrap();
    }
}
