#![cfg(windows)]
use ivy_contracts::{
    IvyJson as J,
    json::{parse, stringify},
};
use ivy_phone_bridge::micro::*;
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

fn obj<const N: usize>(pairs: [(&str, J); N]) -> J {
    J::object(pairs)
}
fn n(value: impl Into<f64>) -> J {
    J::Number(value.into())
}
fn text(value: &J, key: &str) -> String {
    value.get(key).unwrap().as_str().unwrap()
}
fn number(value: &J, key: &str) -> i64 {
    value.get(key).unwrap().as_f64().unwrap() as i64
}
fn flag(value: &J, key: &str) -> bool {
    value.get(key).and_then(J::as_bool).unwrap_or(false)
}
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for b in bytes {
        write!(&mut s, "{b:02X}").unwrap();
    }
    s
}
fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn trace(name: &str, value: &J) {
    if let Ok(root) = std::env::var("IVY_PHONE_MICRO_TRANSPORT_TRACE") {
        std::fs::write(
            std::path::Path::new(&root).join(name),
            stringify(value, 16 * 1024 * 1024).unwrap(),
        )
        .unwrap();
    }
}
fn fixture(mode: &str, cases: &[J]) -> J {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-micro-transport-cases.json");
    let data = parse(&std::fs::read_to_string(path).unwrap(), 4 * 1024 * 1024).unwrap();
    let row = data.get(mode).unwrap();
    assert_eq!(
        row.get("cases").unwrap(),
        &J::Array(cases.to_vec()),
        "{mode} input corpus"
    );
    row.get("expected").unwrap().clone()
}
fn compare(mode: &str, cases: &[J], actual: Vec<J>) {
    trace(&format!("{mode}-rust.json"), &J::Array(actual.clone()));
    let expected = fixture(mode, cases);
    let expected = expected.as_array().unwrap();
    assert_eq!(expected.len(), actual.len());
    for (i, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(
            actual,
            expected,
            "{mode} case {i}: {}",
            text(&cases[i], "name")
        );
    }
    trace(
        &format!("{mode}-comparisons.json"),
        &obj([
            ("cases", n(cases.len() as u32)),
            ("expected", J::Array(expected.to_vec())),
            ("rust", J::Array(actual)),
        ]),
    );
}
fn protocol_state(p: &MicroProtocol) -> J {
    obj([
        ("rgb", p.saw_rgb_configuration().into()),
        ("status", p.saw_status().into()),
        ("statusRequests", n(p.status_requests() as f64)),
        ("dictationSequence", n(p.dictation_sequence() as f64)),
        ("lighting", p.lighting_state().into()),
        ("method", p.last_method().map_or(J::Null, J::from)),
    ])
}
fn protocol_case(name: impl Into<String>, reports: Vec<Vec<u8>>) -> J {
    obj([
        ("name", name.into().into()),
        (
            "reports",
            J::Array(reports.iter().map(|r| hex(r).into()).collect()),
        ),
    ])
}
fn one_report(bytes: &[u8]) -> Vec<u8> {
    assert!(bytes.len() <= 61);
    let mut r = vec![0; 64];
    r[..3].copy_from_slice(&[6, 2, bytes.len() as u8]);
    r[3..3 + bytes.len()].copy_from_slice(bytes);
    r
}

#[test]
fn firmware_rpc_fragmentation_preserves_public_replies() {
    let mut cases = Vec::new();
    for request in [
        r#"{"id":1,"method":"device.status"}"#,
        r#"{"i":"0002","m":"sys.version"}"#,
        r#"{"id":9007199254740991,"m":"unknown🙂"}"#,
        r#"{"id":-0,"m":"device.status"}"#,
        r#"{"id":1,"id":2,"m":"sys.version","m":"device.status"}"#,
        r#"{"i":2,"id":null,"method":"sys.version"}"#,
        r#"{"id":1.0,"m":"device.status"}"#,
        r#"{"id":1e0,"m":"device.status"}"#,
        r#"{"id":"-0","m":"device.status"}"#,
        r#"{"id":"+1","m":"device.status"}"#,
        r#"{"id":"١","m":"device.status"}"#,
        r#"{"id":9223372036854775808,"m":"x"}"#,
        r#"{"id":9007199254740992,"m":"x"}"#,
        r#"{"id":1,"method":null,"m":"device.status"}"#,
        r#"{"id":1,"method":""}"#,
        r#"{"m":null}"#,
        r#"{}"#,
        r#"[]"#,
        r#"true "#,
        r#"0 "#,
        r#"{"id":1,"m":"\uD800"}"#,
        r#"{"id":1,"m":"unknown","opaque":"\uD800"}"#,
    ] {
        cases.push(protocol_case(
            request,
            frame(request.as_bytes())
                .unwrap()
                .into_iter()
                .map(Vec::from)
                .collect(),
        ));
    }
    let split =
        r#"{"i":"00042","m":"v.oai.rgbcfg","p":{"ambient":{"e":2,"c":3050327}},"opaque":"🙂é"}"#
            .as_bytes();
    for at in 1..split.len() {
        cases.push(protocol_case(
            format!("split-{at}"),
            split[..at]
                .chunks(61)
                .chain(split[at..].chunks(61))
                .map(one_report)
                .collect(),
        ));
    }
    let lighting = [
        r#"{"id":1,"m":"v.oai.rgbcfg","p":{"ambient":{"e":2,"c":3050327}}}"#,
        r#"{"id":2,"m":"v.oai.rgbcfg","p":{"ambient":{"effect":2,"color":3050327}}}"#,
        r#"{"id":3,"m":"v.oai.rgbcfg","p":{"ambient":{"e":0,"c":0}}}"#,
        r#"{"id":4,"m":"v.oai.rgbcfg","params":{"ambient":{"e":2,"c":3050327}},"p":null}"#,
        r#"{"id":5,"m":"v.oai.rgbcfg","p":{"ambient":{"e":2,"e":0,"c":3050327}}}"#,
    ];
    cases.push(protocol_case(
        "lighting-sequence",
        lighting
            .into_iter()
            .flat_map(|s| frame(s.as_bytes()).unwrap())
            .map(Vec::from)
            .collect(),
    ));
    for value in [
        "2.0",
        "2e0",
        "2147483648",
        "null",
        "true",
        "[]",
        "{}",
        "\"2\"",
    ] {
        let input = format!(
            "{{\"id\":1,\"m\":\"v.oai.rgbcfg\",\"p\":{{\"ambient\":{{\"e\":{value},\"c\":3050327}}}}}}"
        );
        cases.push(protocol_case(
            format!("effect-{value}"),
            frame(input.as_bytes())
                .unwrap()
                .into_iter()
                .map(Vec::from)
                .collect(),
        ));
    }
    for bytes in [
        b"{\"id\":1,\"m\":\"x\"}{\"id\":2,\"m\":\"sys.version\"}\n".to_vec(),
        b"{}\t\r\n {\"id\":1,\"m\":\"device.status\"} [1]".to_vec(),
        b"{\"id\":1,\"m\":\"device.status\",}".to_vec(),
        b"{\"id\":1,\"m\":\"device.status\"} !".to_vec(),
        b"{\"id\":1,\"m\":\"bad\\x".to_vec(),
        b"{\"id\":1,\"m\":\"x\",\"v\":01".to_vec(),
        [
            b"{\"id\":1,\"m\":\"x\",\"v\":\"".as_slice(),
            &[0xff],
            b"\"}",
        ]
        .concat(),
    ] {
        cases.push(protocol_case(
            format!("wire-{}", cases.len()),
            bytes.chunks(61).map(one_report).collect(),
        ));
    }
    for count in [31, 32, 33] {
        let bytes = format!(
            "{{\"id\":1,\"m\":\"x\",\"v\":{}{} }}",
            "[".repeat(count),
            "]".repeat(count)
        );
        cases.push(protocol_case(
            format!("depth-{count}"),
            bytes.as_bytes().chunks(61).map(one_report).collect(),
        ));
    }
    for count in [32, 33] {
        cases.push(protocol_case(
            format!("partial-depth-{count}"),
            "[".repeat(count)
                .as_bytes()
                .chunks(61)
                .map(one_report)
                .collect(),
        ));
    }
    for report in [vec![], vec![0; 63], vec![0; 65], {
        let mut r = vec![0; 64];
        r[..3].copy_from_slice(&[6, 2, 62]);
        r
    }] {
        cases.push(protocol_case(
            format!("report-{}", cases.len()),
            vec![report],
        ));
    }
    cases.push(protocol_case(
        "pending-bound",
        vec![one_report(&[b' '; 61]); 270],
    ));
    cases.push(protocol_case(
        "pending-string-bound",
        std::iter::once(one_report(b"{\"opaque\":\""))
            .chain(std::iter::repeat_n(one_report(&[b'x'; 61]), 269))
            .collect(),
    ));
    for scalar in ["1", "-0", "true", "false", "null", "\"x\""] {
        cases.push(protocol_case(
            format!("scalar-boundary-{scalar}"),
            vec![one_report(scalar.as_bytes()), one_report(b" ")],
        ));
    }
    cases.push(protocol_case(
        "surrogate-string-id",
        frame(br#"{"id":"\uD800","m":"x"}"#)
            .unwrap()
            .into_iter()
            .map(Vec::from)
            .collect(),
    ));
    for (key, prefix, suffix) in [
        (
            "method",
            b"{\"id\":1,\"m\":\"".as_slice(),
            b"\"}".as_slice(),
        ),
        (
            "id",
            b"{\"id\":\"".as_slice(),
            b"\",\"m\":\"x\"}".as_slice(),
        ),
        (
            "opaque",
            b"{\"id\":1,\"m\":\"x\",\"opaque\":\"".as_slice(),
            b"\"}".as_slice(),
        ),
    ] {
        for invalid in [&[0xff][..], &[0xf0, 0x9f][..], &[0xc0, 0x80][..]] {
            let bytes = [prefix, invalid, suffix].concat();
            cases.push(protocol_case(
                format!("invalid-utf8-{key}-{}", hex(invalid)),
                bytes.chunks(61).map(one_report).collect(),
            ));
        }
    }
    let actual = cases
        .iter()
        .map(|case| {
            let mut p = MicroProtocol::default();
            let mut steps = Vec::new();
            for report in case.get("reports").unwrap().as_array().unwrap() {
                let bytes = unhex(&report.as_str().unwrap());
                let (frames, error) = match p.receive(&bytes) {
                    Ok(frames) => (frames, J::Null),
                    Err(e) => (
                        Vec::new(),
                        obj([
                            (
                                "kind",
                                if !e.is_argument() {
                                    "unexpected"
                                } else {
                                    "argument"
                                }
                                .into(),
                            ),
                            ("message", e.to_string().into()),
                        ]),
                    ),
                };
                steps.push(obj([
                    (
                        "frames",
                        J::Array(frames.iter().map(|r| hex(r).into()).collect()),
                    ),
                    ("error", error),
                    ("state", protocol_state(&p)),
                ]));
            }
            obj([
                ("name", case.get("name").unwrap().clone()),
                ("steps", J::Array(steps)),
            ])
        })
        .collect();
    compare("protocol", &cases, actual);
}

#[derive(Default)]
struct PipeState {
    inbound: VecDeque<u8>,
    writes: Vec<Vec<u8>>,
    closed: bool,
    waiting: bool,
    blocks: u64,
    fail_write: bool,
}
#[derive(Clone, Default)]
struct Pipe(Arc<(Mutex<PipeState>, Condvar)>);
struct ClosePipe(Pipe);
impl Drop for ClosePipe {
    fn drop(&mut self) {
        self.0.close();
    }
}
struct CloseDevice(MicroUsbDevice);
impl Drop for CloseDevice {
    fn drop(&mut self) {
        let _ = self.0.disposal();
    }
}
impl Pipe {
    fn await_read(&self) {
        let (lock, event) = &*self.0;
        let mut s = lock.lock().unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !s.waiting && !s.closed {
            assert!(Instant::now() < until);
            s = event.wait_timeout(s, Duration::from_millis(10)).unwrap().0;
        }
    }
    fn feed(&self, bytes: &[u8]) {
        let (lock, event) = &*self.0;
        let mut s = lock.lock().unwrap();
        assert!(!s.closed);
        let before = s.blocks;
        s.inbound.extend(bytes);
        s.waiting = false;
        event.notify_all();
        let until = Instant::now() + Duration::from_secs(5);
        while !s.closed && !(s.waiting && s.blocks > before && s.inbound.is_empty()) {
            assert!(Instant::now() < until, "Rust session did not consume input");
            s = event.wait_timeout(s, Duration::from_millis(10)).unwrap().0;
        }
    }
    fn take(&self) -> J {
        let mut s = self.0.0.lock().unwrap();
        let writes = std::mem::take(&mut s.writes);
        obj([
            ("closed", s.closed.into()),
            (
                "writes",
                J::Array(writes.iter().map(|b| hex(b).into()).collect()),
            ),
        ])
    }
}
impl MicroStream for Pipe {
    fn read_exact(&self, bytes: &mut [u8]) -> io::Result<()> {
        let (lock, event) = &*self.0;
        let mut s = lock.lock().unwrap();
        for b in bytes {
            while s.inbound.is_empty() && !s.closed {
                s.waiting = true;
                s.blocks += 1;
                event.notify_all();
                s = event.wait(s).unwrap();
            }
            s.waiting = false;
            *b = s
                .inbound
                .pop_front()
                .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        }
        Ok(())
    }
    fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        let mut s = self.0.0.lock().unwrap();
        if s.closed {
            return Err(io::Error::other("Closed fixture pipe."));
        }
        if s.fail_write {
            s.writes.push(bytes[..7.min(bytes.len())].to_vec());
            return Err(io::Error::other("Owned injected partial write."));
        }
        s.writes.push(bytes.to_vec());
        Ok(())
    }
    fn close(&self) {
        let mut s = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
        s.closed = true;
        s.waiting = false;
        self.0.1.notify_all();
    }
}
#[derive(Default)]
struct Clock(AtomicU64);
impl MicroClock for Clock {
    fn timestamp(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}
fn op(name: &str) -> J {
    obj([("op", name.into())])
}
fn feed(bytes: &[u8]) -> J {
    obj([("op", "feed".into()), ("bytes", hex(bytes).into())])
}
fn advance(ms: u32) -> J {
    obj([("op", "advance".into()), ("milliseconds", n(ms))])
}
fn input(pressed: bool) -> J {
    obj([("op", "input".into()), ("pressed", pressed.into())])
}
fn request(kind: u16) -> Vec<u8> {
    let mut bytes = vec![1, 0x11];
    bytes.extend(kind.to_be_bytes());
    bytes.extend([0; 4]);
    bytes
}
fn import() -> Vec<u8> {
    let mut bytes = request(0x8003);
    let mut bus = [0; 32];
    bus[..BUS_ID.len()].copy_from_slice(BUS_ID.as_bytes());
    bytes.extend(bus);
    bytes
}
fn urb(command: u32, seq: u32, direction: u32, endpoint: u32, length: u32) -> Vec<u8> {
    let mut b = vec![0; 48];
    for (at, v) in [
        (0, command),
        (4, seq),
        (8, DEVICE_ID),
        (12, direction),
        (16, endpoint),
        (24, length),
    ] {
        b[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }
    b
}
fn read(seq: u32) -> J {
    feed(&urb(1, seq, 1, 1, 64))
}
fn output(seq: u32, report: &[u8]) -> J {
    let mut b = urb(1, seq, 0, 1, report.len() as u32);
    b.extend(report);
    feed(&b)
}
fn rpc(seq: u32, message: &str) -> Vec<J> {
    frame(message.as_bytes())
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, r)| output(seq + i as u32, r))
        .collect()
}
fn unlink(seq: u32, victim: u32) -> J {
    let mut b = urb(2, seq, 1, 1, 0);
    b[20..24].copy_from_slice(&victim.to_be_bytes());
    feed(&b)
}
fn configured() -> Vec<J> {
    let mut a = vec![feed(&import())];
    a.extend(rpc(
        10,
        r#"{"id":1,"m":"v.oai.rgbcfg","p":{"ambient":{"e":2,"c":3050327}}}"#,
    ));
    a.extend(rpc(20, r#"{"id":2,"m":"device.status"}"#));
    for seq in 30..33 {
        a.push(read(seq));
    }
    a
}
fn session_case(name: impl Into<String>, actions: Vec<J>) -> J {
    obj([
        ("name", name.into().into()),
        ("generation", n(1)),
        ("actions", J::Array(actions)),
    ])
}

#[test]
fn imported_session_generations_queues_and_lifetimes_are_preserved() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let mut cases = Vec::new();
    let mut a = configured();
    a.extend([
        input(true),
        read(40),
        input(true),
        read(41),
        input(false),
        read(42),
        obj([
            ("op", "input".into()),
            ("pressed", true.into()),
            ("generation", n(99)),
        ]),
        obj([
            ("op", "input".into()),
            ("pressed", true.into()),
            ("current", false.into()),
        ]),
        input(true),
    ]);
    cases.push(session_case("input-admission", a));
    let mut a = configured();
    a.extend([
        read(40),
        read(41),
        unlink(50, 40),
        input(true),
        read(42),
        unlink(51, 42),
        input(false),
        read(43),
        input(true),
    ]);
    a.extend(rpc(60, r#"{"id":3,"m":"v.oai.rgbcfg"}"#));
    a.extend(rpc(70, r#"{"id":4,"m":"device.status"}"#));
    a.extend([read(80), read(81), read(82), input(true)]);
    cases.push(session_case("last-reader-new-epoch", a));
    let mut a = configured();
    a.extend([read(40), op("failWrite"), input(true), input(false)]);
    cases.push(session_case("partial-input-write", a));
    for predicate in ["throws", "reentrantClose"] {
        let mut a = configured();
        a.extend([
            read(40),
            obj([
                ("op", "input".into()),
                ("pressed", true.into()),
                (predicate, true.into()),
            ]),
            input(false),
        ]);
        cases.push(session_case(predicate, a));
    }
    let mut a = vec![feed(&import())];
    a.extend(rpc(1, r#"{"id":1,"m":"device.status"}"#));
    a.extend([advance(2000), read(10)]);
    a.extend(rpc(20, r#"{"id":2,"m":"sys.version"}"#));
    cases.push(session_case("unstarted-response-expiry", a));
    let mut a = vec![feed(&import())];
    a.extend(rpc(1, r#"{"id":1,"m":"device.status"}"#));
    a.extend([read(10), advance(2000), read(11)]);
    cases.push(session_case("partial-response-expiry", a));
    for ms in [1999, 2000] {
        let mut a = vec![feed(&import())];
        a.extend(rpc(1, r#"{"id":1,"m":"sys.version"}"#));
        a.extend([advance(ms), read(10)]);
        cases.push(session_case(format!("reply-boundary-{ms}"), a));
    }
    for ms in [10000, 10001] {
        cases.push(session_case(
            format!("preimport-heartbeat-{ms}"),
            vec![advance(ms), op("heartbeat")],
        ));
    }
    for ms in [239999, 240000, 240001] {
        let mut a = configured();
        a.extend([advance(ms), op("heartbeat"), input(true)]);
        cases.push(session_case(format!("idle-heartbeat-{ms}"), a));
    }
    let mut a = configured();
    a.push(advance(239900));
    a.push(output(99, &one_report(b"{\"id\":99,\"m\":\"device.sta")));
    a.extend([advance(100), op("heartbeat")]);
    cases.push(session_case("partial-status-is-not-heartbeat", a));
    let mut a = configured();
    a.push(advance(239900));
    a.extend(rpc(90, r#"{"id":99,"m":"device.status"}"#));
    a.extend([
        advance(100),
        op("heartbeat"),
        read(100),
        read(101),
        read(102),
        input(true),
    ]);
    cases.push(session_case("complete-status-refreshes-heartbeat", a));
    for variant in ["duplicate-read", "read-limit", "reply-limit"] {
        let mut a = vec![feed(&import())];
        match variant {
            "duplicate-read" => a.extend([read(1), read(1)]),
            "read-limit" => a.extend((1..=33).map(read)),
            _ => {
                for seq in 1..=65 {
                    a.extend(rpc(seq, r#"{"id":1,"m":"device.status"}"#));
                }
            }
        }
        cases.push(session_case(variant, a));
    }
    for (name, at, value) in [
        ("device", 8, DEVICE_ID + 1),
        ("direction", 12, 2),
        ("endpoint", 16, 2),
        ("command", 0, 3),
        ("length", 24, 4097),
        ("iso", 32, 1),
    ] {
        let mut b = urb(1, 1, 1, 1, 64);
        b[at..at + 4].copy_from_slice(&value.to_be_bytes());
        cases.push(session_case(name, vec![feed(&import()), feed(&b)]));
    }
    for packets in [0, u32::MAX] {
        let mut b = urb(1, 1, 1, 1, 64);
        b[32..36].copy_from_slice(&packets.to_be_bytes());
        cases.push(session_case(
            format!("packets-{packets}"),
            vec![feed(&import()), feed(&b), unlink(2, 1)],
        ));
    }
    cases.push(session_case(
        "list-then-import",
        vec![
            feed(&request(0x8005)),
            feed(&request(0x8005)),
            feed(&import()),
            unlink(1, 99),
        ],
    ));
    let mut refused = session_case("activation-refused", vec![feed(&import())]);
    refused.insert("refuse", true.into());
    cases.push(refused);
    for bytes in [
        vec![1, 0x10, 0x80, 5, 0, 0, 0, 0],
        vec![1, 0x11, 0x80, 5, 0, 0, 0, 1],
        request(0x8004),
        {
            let mut b = import();
            *b.last_mut().unwrap() = 1;
            b
        },
    ] {
        cases.push(session_case(
            format!("operation-{}", cases.len()),
            vec![feed(&bytes)],
        ));
    }
    let mut a = configured();
    let mut b = urb(1, 99, 0, 0, 64);
    b[40..48].copy_from_slice(&[0x21, 9, 6, 2, 0, 0, 64, 0]);
    b.extend(one_report(b"{\"id\":3,\"m\":\"sys.version\"}"));
    a.extend([read(80), feed(&b)]);
    cases.push(session_case("control-output-before-ack", a));
    let actual = cases
        .iter()
        .map(|case| {
            let pipe = Pipe::default();
            let _close = ClosePipe(pipe.clone());
            let clock = Arc::new(Clock::default());
            let callbacks = Arc::new(AtomicU32::new(0));
            let counted = callbacks.clone();
            let refuse = flag(case, "refuse");
            let session = MicroUsbSession::new(
                Box::new(pipe.clone()),
                number(case, "generation"),
                move |s| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let _ = s.generation();
                    let _ = s.ready();
                    let _ = s.observe_voice(s.generation());
                    !refuse
                },
                clock.clone(),
                None,
            )
            .unwrap();
            let run = session.start().unwrap();
            let same = run.same_work(&session.start().unwrap());
            pipe.await_read();
            let mut predicates = 0u32;
            let mut steps = Vec::new();
            for action in case.get("actions").unwrap().as_array().unwrap() {
                let operation = text(action, "op");
                let mut receipt = J::Null;
                match operation.as_str() {
                    "feed" => pipe.feed(&unhex(&text(action, "bytes"))),
                    "advance" => {
                        clock
                            .0
                            .fetch_add(number(action, "milliseconds") as u64, Ordering::SeqCst);
                    }
                    "heartbeat" => session.check_heartbeat(),
                    "failWrite" => pipe.0.0.lock().unwrap().fail_write = true,
                    "close" => session.close(),
                    "input" => {
                        let calls = AtomicU32::new(0);
                        let generation = action
                            .get("generation")
                            .map_or_else(|| session.generation(), |_| number(action, "generation"));
                        receipt = session
                            .send_microphone(flag(action, "pressed"), generation, &|| {
                                calls.fetch_add(1, Ordering::SeqCst);
                                if flag(action, "throws") {
                                    return Err(io::Error::other("Owned predicate fault.").into());
                                }
                                if flag(action, "reentrantClose") {
                                    session.close();
                                }
                                let _ = session.generation();
                                let _ = session.ready();
                                Ok(action
                                    .get("current")
                                    .is_none_or(|v| v.as_bool() == Some(true)))
                            })
                            .to_json();
                        predicates += calls.load(Ordering::SeqCst);
                    }
                    _ => panic!("Unknown fixture action"),
                }
                let voice = session.observe_voice(session.generation());
                steps.push(obj([
                    ("op", operation.into()),
                    ("receipt", receipt),
                    ("wire", pipe.take()),
                    ("generation", n(session.generation() as f64)),
                    ("ready", session.ready().into()),
                    (
                        "voice",
                        obj([
                            ("state", voice.state.into()),
                            ("generation", n(voice.generation as f64)),
                            ("dictationSequence", n(voice.dictation_sequence as f64)),
                        ]),
                    ),
                    ("callbacks", n(callbacks.load(Ordering::SeqCst))),
                    ("predicates", n(predicates)),
                ]));
            }
            if text(case, "name") == "last-reader-new-epoch" {
                assert_eq!(session.generation(), (1i64 << 32) + 1);
            }
            runtime.block_on(session.disposal().wait()).unwrap();
            obj([
                ("name", case.get("name").unwrap().clone()),
                ("same", same.into()),
                ("steps", J::Array(steps)),
                ("closed", pipe.take()),
            ])
        })
        .collect();
    compare("sessions", &cases, actual);
}

fn tcp_read(stream: &mut TcpStream, count: usize) -> Vec<u8> {
    let mut bytes = vec![0; count];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}
fn tcp_import(port: u16) -> TcpStream {
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_secs(3),
    )
    .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(&import()).unwrap();
    let reply = tcp_read(&mut stream, 320);
    assert_eq!(&reply[..4], &[1, 0x11, 0, 3]);
    stream
}
fn tcp_rpc(stream: &mut TcpStream, seq: &mut u32, message: &str) {
    for report in frame(message.as_bytes()).unwrap() {
        let mut b = urb(1, *seq, 0, 1, 64);
        b.extend(report);
        stream.write_all(&b).unwrap();
        let ack = tcp_read(stream, 48);
        assert_eq!(u32::from_be_bytes(ack[4..8].try_into().unwrap()), *seq);
        *seq += 1;
    }
}
fn tcp_ready(stream: &mut TcpStream) {
    let mut seq = 10;
    tcp_rpc(stream, &mut seq, r#"{"id":1,"m":"v.oai.rgbcfg"}"#);
    tcp_rpc(stream, &mut seq, r#"{"id":2,"m":"device.status"}"#);
    for seq in 20..23 {
        stream.write_all(&urb(1, seq, 1, 1, 64)).unwrap();
        assert_eq!(tcp_read(stream, 112).len(), 112);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_device_replaces_import_and_joins_owned_connections() {
    let device = MicroUsbDevice::new(0, 0, None).unwrap();
    let _close = CloseDevice(device.clone());
    let port = device.port();
    assert!(device.healthy());
    assert_eq!(device.ready_generation(), None);
    assert!(
        MicroUsbDevice::new(i32::from(port), 0, None).is_err(),
        "exclusive listener"
    );
    let mut first = tcp_import(port);
    tcp_ready(&mut first);
    let old = device.ready_generation().unwrap();
    assert_eq!(old, 1i64 << 32);
    assert_eq!(
        device.send_microphone(true, old, &|| Ok(true)).phase,
        "not_submitted"
    );
    first.write_all(&urb(1, 40, 1, 1, 64)).unwrap();
    first.write_all(&urb(2, 41, 1, 1, 0)).unwrap();
    let ack = tcp_read(&mut first, 48);
    assert_eq!(u32::from_be_bytes(ack[4..8].try_into().unwrap()), 41);
    let receipt = device.send_microphone(true, old, &|| {
        assert_eq!(device.ready_generation(), Some(old));
        Ok(true)
    });
    assert_eq!(receipt.phase, "submitted");
    let event = tcp_read(&mut first, 112);
    assert_eq!(&event[48..], &microphone(true));
    let mut second = tcp_import(port);
    let mut byte = [0];
    assert!(matches!(first.read(&mut byte), Ok(0) | Err(_)));
    assert_eq!(device.ready_generation(), None);
    tcp_ready(&mut second);
    let new = device.ready_generation().unwrap();
    assert_eq!(new, 2i64 << 32);
    assert_eq!(
        device
            .send_microphone(false, old, &|| panic!("stale predicate"))
            .phase,
        "not_submitted"
    );
    assert_eq!(
        device.observe_voice(old),
        MicroVoiceObservation {
            state: "unavailable".into(),
            generation: old,
            dictation_sequence: 0
        }
    );
    second.write_all(&urb(1, 50, 1, 1, 64)).unwrap();
    second.write_all(&urb(2, 51, 1, 1, 0)).unwrap();
    let _ = tcp_read(&mut second, 48);
    let held = device.clone();
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, allowed) = std::sync::mpsc::channel();
    let input = std::thread::spawn(move || {
        held.send_microphone(true, new, &|| {
            entered.send(()).unwrap();
            allowed.recv().unwrap();
            Ok(true)
        })
    });
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    let replacement = std::thread::spawn(move || tcp_import(port));
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !replacement.is_finished(),
        "import must wait for the original write boundary"
    );
    release.send(()).unwrap();
    assert_eq!(input.join().unwrap().phase, "submitted");
    let event = tcp_read(&mut second, 112);
    assert_eq!(&event[48..], &microphone(true));
    let third = replacement.join().unwrap();
    let dispose = device.disposal().unwrap();
    drop(dispose.clone());
    tokio::time::timeout(Duration::from_secs(3), dispose.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!device.healthy());
    assert_eq!(device.ready_generation(), None);
    assert_eq!(
        device.send_microphone(false, new, &|| panic!()).phase,
        "not_submitted"
    );
    assert!(
        TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            Duration::from_millis(250)
        )
        .is_err()
    );
    let _ = third.shutdown(Shutdown::Both);
    let exhausted = MicroUsbDevice::new(0, 2_097_151, None).unwrap();
    let _close_exhausted = CloseDevice(exhausted.clone());
    if let Ok(mut rejected) = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, exhausted.port())),
        Duration::from_secs(3),
    ) {
        rejected
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert!(matches!(rejected.read(&mut byte), Ok(0) | Err(_)));
    }
    let until = Instant::now() + Duration::from_secs(3);
    while exhausted.healthy() {
        assert!(Instant::now() < until);
        tokio::task::yield_now().await;
    }
    assert_eq!(exhausted.last_generation(), 2_097_151);
    assert_eq!(
        exhausted
            .disposal()
            .unwrap()
            .wait()
            .await
            .unwrap_err()
            .to_string(),
        "Micro connection generation limit reached."
    );
    trace(
        "device-ownership.json",
        &obj([
            ("exclusiveListener", true.into()),
            ("importReplacement", true.into()),
            ("linearizedInput", true.into()),
            ("droppedObserverJoined", true.into()),
            ("generationExhaustion", true.into()),
            ("loopbackOnly", true.into()),
        ]),
    );
}
