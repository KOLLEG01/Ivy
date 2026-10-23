use ivy_contracts::{IvyJson, json::parse};
use ivy_phone_bridge::media::{
    RtpDatagramSink, RtpHeader, RtpPacket, RtpPeer, RtpTransport, RtpTransportError,
};
use std::{
    net::{SocketAddr, UdpSocket as StdUdpSocket},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::UdpSocket, sync::Notify, time::timeout};

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
fn rtp_packet_csrc_extension_and_padding_match_public_vectors() {
    let cases = layout_cases();
    let fixture = parse(
        include_str!("../fixtures/phone-rtp-packet-cases.json"),
        1_000_000,
    )
    .unwrap();
    let rows = fixture.as_array().unwrap();
    assert_eq!(rows.len(), cases.len());
    for (index, (bytes, row)) in cases.iter().zip(rows).enumerate() {
        assert_eq!(
            row.get("input").unwrap().as_str().unwrap(),
            hex(bytes),
            "RTP input {index}"
        );
        assert_eq!(
            &packet_snapshot(RtpPacket::parse(bytes).unwrap()),
            row.get("expected").unwrap(),
            "RTP result {index}"
        );
    }
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
