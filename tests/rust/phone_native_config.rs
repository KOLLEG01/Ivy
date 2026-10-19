use super::{EPOCH, OP, envelope};
use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_phone_bridge::{
    native_config::{NativeSipConfiguration, OfferMode},
    native_rpc::NativeRequest,
    sip::SipHost,
};
const BASE: &str = r#"{"binding":{"address":"127.0.0.1","port":0,"transport":"udp"},"codecs":{"preferences":["PCMA"]},"incomingPeers":[],"registration":null,"password":null}"#;
fn change(path: &[&str], value: &str) -> String {
    fn set(root: &mut IvyJson, path: &[&str], value: IvyJson) {
        if path.len() == 1 {
            root.insert(path[0], value);
            return;
        }
        let IvyJson::Object(fields) = root else {
            panic!()
        };
        let (_, child) = fields
            .iter_mut()
            .find(|(name, _)| name == &path[0].into())
            .unwrap();
        set(child, &path[1..], value);
    }
    let mut root = parse(BASE, 65536).unwrap();
    set(&mut root, path, parse(value, 65536).unwrap());
    stringify(&root, 65536).unwrap()
}
fn configuration(params: &str) -> std::result::Result<NativeSipConfiguration, &'static str> {
    let request = NativeRequest::parse(envelope("configure", Some(OP), params, 1).as_bytes())
        .map_err(|_| "invalid_request")?;
    NativeSipConfiguration::parse(&request).map_err(|error| error.code())
}
fn cases() -> Vec<String> {
    let mut cases = vec![BASE.into()];
    for name in [
        "binding",
        "codecs",
        "incomingPeers",
        "registration",
        "password",
    ] {
        for value in ["null", "0", "false", "[]", "{}", "\"value\""] {
            cases.push(change(&[name], value));
        }
        let mut missing = parse(BASE, 65536).unwrap();
        missing.remove(name);
        cases.push(stringify(&missing, 65536).unwrap());
    }
    for (object, field) in [("binding", "unknown"), ("codecs", "unknown")] {
        cases.push(change(&[object, field], "null"));
    }
    for field in ["unknown", "Binding"] {
        cases.push(change(&[field], "null"));
    }
    for ip in [
        "127.1",
        "2130706433",
        "0177.0.0.1",
        "0x7f000001",
        "127.0.0.01",
        "08.0.0.1",
        "1.2.65535",
        "1.16777215",
        "4294967295",
        "4294967296",
        "0",
        "0x",
        " 127.0.0.1",
        "127.0.0.1 ",
        "localhost",
        "[::1]",
        "[::1]:80",
        "[::1]garbage",
        "::1",
        "::1%",
        "fe80::1%12",
        "fe80::1%bad",
        "::1%4294967296",
        "::ffff:127.0.0.01",
    ] {
        let ip = stringify(&ip.into(), 65536).unwrap();
        cases.push(change(&["binding", "address"], &ip));
        cases.push(change(&["incomingPeers"], &format!("[{ip}]")));
    }
    for peers in [
        r#"["registered"]"#,
        r#"["REGISTERED"]"#,
        r#"[null]"#,
        r#"["127.0.0.1","127.0.0.1"]"#,
        r#"["127.1","127.0.0.1"]"#,
        r#"["bad","bad"]"#,
        r#"["bad",null]"#,
    ] {
        cases.push(change(&["incomingPeers"], peers));
    }
    let max_peers = IvyJson::Array((1..=33).map(|n| format!("127.0.0.{n}").into()).collect());
    cases.push(change(
        &["incomingPeers"],
        &stringify(&max_peers, 65536).unwrap(),
    ));
    for transport in ["udp", "tcp", "tls", "UDP", "ws", ""] {
        cases.push(change(
            &["binding", "transport"],
            &stringify(&transport.into(), 65536).unwrap(),
        ));
    }
    for mode in ["initial", "delayed", "auto", "AUTO", ""] {
        cases.push(change(
            &["binding", "outgoingOfferMode"],
            &stringify(&mode.into(), 65536).unwrap(),
        ));
    }
    cases.push(change(&["binding", "outgoingOfferMode"], "null"));
    for (object, field) in [
        ("binding", "port"),
        ("binding", "incomingRingSeconds"),
        ("binding", "rtpPortRangeStart"),
        ("binding", "rtpPortRangeEnd"),
        ("binding", "maxConcurrentCalls"),
        ("codecs", "packetMs"),
        ("codecs", "receiveReorderMs"),
    ] {
        for number in [
            -1,
            0,
            1,
            19,
            20,
            30,
            32,
            33,
            60,
            61,
            120,
            121,
            1023,
            1024,
            1025,
            30000,
            31000,
            65534,
            65535,
            65536,
            i32::MAX,
        ] {
            cases.push(change(&[object, field], &number.to_string()));
        }
        for value in [
            "null",
            "true",
            "\"20\"",
            "20.0",
            "2e1",
            "2147483648",
            "-2147483649",
        ] {
            let needle = if object == "binding" {
                "\"address\""
            } else {
                "\"preferences\""
            };
            cases.push(if field == "port" {
                BASE.replace("\"port\":0", &format!("\"port\":{value}"))
            } else {
                BASE.replacen(needle, &format!("\"{field}\":{value},{needle}"), 1)
            });
        }
    }
    for prefs in [
        r#"["G722","PCMA","PCMU","OPUS","EVS"]"#,
        r#"["EVS"]"#,
        r#"["PCMA","PCMA"]"#,
        r#"["pcma"]"#,
        r#"[null]"#,
        "[]",
        "null",
        r#"["PCMA",0]"#,
    ] {
        cases.push(change(&["codecs", "preferences"], prefs));
    }
    for mode in ["initial", "delayed", "auto"] {
        let base = change(&["binding", "outgoingOfferMode"], &format!("\"{mode}\""));
        cases.push(base.replace("[\"PCMA\"]", "[\"EVS\"]"));
    }
    for proxy in [
        "127.0.0.1",
        "127.0.0.1:5060",
        "udp:127.0.0.1:5060",
        "UDP:127.0.0.1:5060",
        "tcp:127.0.0.1",
        "udp:[::1]:5060",
        "udp:[fe80::1%12]:5060",
        "udp:127.1:5060",
        "sip:127.0.0.1",
        "localhost:5060",
        "udp:127.0.0.1:0",
        "udp:127.0.0.1:65536",
        "udp:127.0.0.1:5060;x",
        "udp:127.0.0.1:5060;channelid=a;connid=b",
        " udp:127.0.0.1:5060 ",
        "udp:127.0.0.1:-1",
        "udp:127.0.0.1:abc",
        "sip:user@127.0.0.1",
        "sip:127.0.0.1;transport=tcp",
    ] {
        cases.push(change(
            &["binding", "outboundProxy"],
            &stringify(&proxy.into(), 65536).unwrap(),
        ));
    }
    let reg = r#"{"accountUri":"sip:public@account.example.test","registrarUri":"sip:registrar.example.test","contactUser":"contact","authUsername":"auth","realm":null,"expirySeconds":60,"retrySeconds":10}"#;
    let registered = change(&["registration"], reg)
        .replace("\"password\":null", "\"password\":\"synthetic-only\"");
    cases.push(registered.clone());
    for value in ["null", "\"\"", "\"\\u0000\"", "\"\\u007f\"", "\"\\u009f\""] {
        cases.push(registered.replace("\"synthetic-only\"", value));
    }
    for n in [4096, 4097] {
        cases.push(registered.replace("synthetic-only", &"x".repeat(n)));
    }
    for (field, from) in [("expirySeconds", "60"), ("retrySeconds", "10")] {
        for value in [
            "0", "9", "10", "600", "601", "7200", "7201", "10.0", "1e1", "null",
        ] {
            cases.push(registered.replace(
                &format!("\"{field}\":{from}"),
                &format!("\"{field}\":{value}"),
            ));
        }
    }
    for value in ["-1", "0", "20", "300", "301", "null", "20.0"] {
        cases.push(registered.replace(
            "\"expirySeconds\":60",
            &format!("\"keepAliveIntervalSeconds\":{value},\"expirySeconds\":60"),
        ));
    }
    for (field, from) in [
        ("accountUri", "sip:public@account.example.test"),
        ("registrarUri", "sip:registrar.example.test"),
        ("contactUser", "contact"),
        ("authUsername", "auth"),
    ] {
        for value in [
            "",
            " ",
            "x\u{85}",
            "sip:nouser.example.test",
            "sips:host.example.test",
            "sip:public@account.example.test;transport=tcp",
        ] {
            cases.push(registered.replace(
                &format!("\"{field}\":\"{from}\""),
                &format!("\"{field}\":{}", stringify(&value.into(), 65536).unwrap()),
            ));
        }
    }
    for executable in [
        "C:\\fixture\\usbip.exe",
        "C:/fixture/USBIP.EXE",
        "\\\\server\\share\\usbip.exe",
        "/tmp/usbip.exe",
        "\\usbip.exe",
        "C:usbip.exe",
        "\\\\usbip.exe",
        "\\\\?\\C:\\usbip.exe",
        "1:\\usbip.exe",
        "C:\\uſbip.exe",
        "C:\\usbıp.exe",
        "\\\\?\\UNC\\server\\usbip.exe",
        "\\\\?\\usbip.exe",
    ] {
        let micro = IvyJson::object([
            ("usbipExecutable", executable.into()),
            (
                "executableHash",
                format!("sha256:{}", "a".repeat(64)).into(),
            ),
            ("port", IvyJson::Number(3240.0)),
        ]);
        cases.push(change(&["micro"], &stringify(&micro, 65536).unwrap()));
    }
    for hash in [
        format!("sha256:{}", "a".repeat(64)),
        format!("sha256:{}\n", "a".repeat(64)),
        format!("sha256:{}\r\n", "a".repeat(64)),
        format!("sha256:{}", "A".repeat(64)),
        "sha256:00".into(),
    ] {
        let micro = IvyJson::object([
            ("usbipExecutable", "C:\\fixture\\usbip.exe".into()),
            ("executableHash", hash.into()),
            ("port", IvyJson::Number(3240.0)),
        ]);
        cases.push(change(&["micro"], &stringify(&micro, 65536).unwrap()));
    }
    // Serializer failures precede all logical validation, then peers precede
    // bind/codecs. Preserve these different safe error codes too.
    cases.push(change(&["binding", "outboundProxy"], "\"not-an-ip\"").replace("[\"PCMA\"]", "[0]"));
    cases.push(change(&["incomingPeers"], "[\"bad\"]").replace("\"port\":0", "\"port\":-1"));
    cases.push(
        change(&["binding", "outboundProxy"], "\"not-an-ip\"").replace("[\"PCMA\"]", "[\"bad\"]"),
    );
    cases
}

#[cfg(windows)]
#[test]
fn configuration_matches_actual_original_record_validators_without_starting_native_work() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let cases = cases();
    let frames = cases
        .iter()
        .map(|params| envelope("configure", Some(OP), params, 1))
        .collect::<Vec<_>>();
    let mut child = Command::new("pwsh")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args([
            "-NoProfile",
            "-File",
            "tests/fixtures/phone-native-config-oracle.ps1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = stringify(
        &IvyJson::Array(frames.into_iter().map(Into::into).collect()),
        2_000_000,
    )
    .unwrap();
    let written = child.stdin.take().unwrap().write_all(input.as_bytes());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    written.unwrap();
    let results = parse(std::str::from_utf8(&output.stdout).unwrap(), 2_000_000).unwrap();
    let results = results.as_array().unwrap();
    assert_eq!(results.len(), cases.len());
    if let Some(path) = std::env::var_os("IVY_PHONE_CONFIG_ORACLE_TRACE") {
        let records = IvyJson::Array(
            cases
                .iter()
                .zip(results)
                .map(|(input, expected)| {
                    IvyJson::object([
                        ("input", input.clone().into()),
                        ("expected", expected.clone()),
                    ])
                })
                .collect(),
        );
        std::fs::write(path, stringify(&records, 2_000_000).unwrap()).unwrap();
    }
    let mut mismatches = Vec::new();
    for (index, (params, expected)) in cases.iter().zip(results).enumerate() {
        let actual = configuration(params);
        let error = expected.get("error").unwrap();
        let actual_error = actual.as_ref().err().map(|error| (*error).to_string());
        let expected_error = if error.is_null() {
            None
        } else {
            Some(error.as_str().unwrap())
        };
        if actual_error != expected_error {
            mismatches.push(format!(
                "Config case {index}: Rust={actual_error:?}, original={expected_error:?}, {params}"
            ));
            continue;
        }
        let Ok(actual) = actual else {
            continue;
        };
        let binding = actual.binding();
        let (rtp_start, rtp_end) = binding.rtp_port_range();
        let proxy = binding.outbound_proxy().map(|proxy| {
            format!(
                "{}:{}",
                proxy.r#type.unwrap().to_string().to_ascii_lowercase(),
                proxy.get_socketaddr().unwrap()
            )
        });
        let mut addresses = actual
            .incoming_peers()
            .addresses()
            .iter()
            .map(|address| {
                let mut address = address.to_string();
                if let Some(rest) = address.strip_prefix('[') {
                    address = rest[..rest.rfind(']').unwrap()].to_string();
                } else {
                    address = address[..address.rfind(':').unwrap()].to_string();
                }
                address
            })
            .collect::<Vec<_>>();
        addresses.sort();
        let snapshot = IvyJson::object([
            ("endpoint", binding.endpoint().to_string().into()),
            ("proxy", proxy.map(Into::into).unwrap_or(IvyJson::Null)),
            (
                "registered",
                IvyJson::Bool(actual.incoming_peers().uses_registered_peer()),
            ),
            (
                "packetMs",
                IvyJson::Number(actual.codecs().packet_ms().into()),
            ),
            (
                "receiveReorderMs",
                IvyJson::Number(actual.codecs().receive_reorder_ms().into()),
            ),
            (
                "preferences",
                IvyJson::Array(
                    actual
                        .codecs()
                        .preferences()
                        .iter()
                        .map(|codec| codec.name().into())
                        .collect(),
                ),
            ),
            (
                "ringSeconds",
                IvyJson::Number(binding.incoming_ring_seconds().into()),
            ),
            (
                "offerMode",
                match binding.offer_mode() {
                    OfferMode::Initial => "initial",
                    OfferMode::Delayed => "delayed",
                    OfferMode::Auto => "auto",
                }
                .into(),
            ),
            ("delayed", IvyJson::Bool(actual.uses_delayed_offer())),
            ("rtpStart", IvyJson::Number(rtp_start.into())),
            ("rtpEnd", IvyJson::Number(rtp_end.into())),
            (
                "maxCalls",
                IvyJson::Number(binding.max_concurrent_calls().into()),
            ),
            (
                "micro",
                actual
                    .micro()
                    .map(|micro| micro.executable().into())
                    .unwrap_or(IvyJson::Null),
            ),
        ]);
        for (name, value) in snapshot.as_object().unwrap() {
            if Some(value) != expected.get_key(name) {
                mismatches.push(format!(
                    "Config case {index}, field {}: Rust={value:?}, original={:?}; {params}",
                    name.to_utf8().unwrap(),
                    expected.get_key(name)
                ));
            }
        }
        let mut expected_addresses = expected
            .get("incoming")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>();
        expected_addresses.sort();
        if addresses != expected_addresses {
            mismatches.push(format!(
                "Config case {index} incoming: Rust={addresses:?}, original={expected_addresses:?}"
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[tokio::test]
async fn validated_native_configuration_uses_real_binding_and_retains_media_policy() {
    let params = change(&["binding", "outgoingOfferMode"], "\"auto\"")
        .replace("[\"PCMA\"]", "[\"EVS\",\"OPUS\"]");
    let config = configuration(&params).unwrap();
    assert!(config.uses_delayed_offer());
    let parts = config.into_parts();
    let host = SipHost::new();
    let endpoint = host.configure(parts.host).await.unwrap();
    assert_eq!(
        endpoint.ip(),
        "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
    );
    assert_ne!(endpoint.port(), 0);
    assert_eq!(parts.binding.rtp_port_range(), (30000, 31000));
    assert_eq!(parts.codecs.preferences()[0].name(), "EVS");
    assert!(
        host.configure(configuration(BASE).unwrap().into_parts().host)
            .await
            .is_err()
    );
    host.close().await.unwrap();
    assert!(std::net::UdpSocket::bind(endpoint).is_ok());
}

#[test]
fn incoming_configuration_keeps_scoped_ip_identity_and_requires_live_registration() {
    use ivy_phone_bridge::sip::{RegistrationPeer, RegistrationTransport};
    let config = configuration(&change(
        &["incomingPeers"],
        r#"["fe80::1%12","registered"]"#,
    ))
    .unwrap();
    let host = SipHost::new();
    let peer = |address: &str| RegistrationPeer {
        address: address.parse().unwrap(),
        transport: RegistrationTransport::Udp,
    };
    assert!(
        config
            .incoming_peers()
            .allows(peer("[fe80::1%12]:5060"), &host)
    );
    assert!(
        !config
            .incoming_peers()
            .allows(peer("[fe80::1%13]:5060"), &host)
    );
    assert!(
        !config
            .incoming_peers()
            .allows(peer("127.0.0.1:5060"), &host)
    );
    assert_eq!(host.observation().configured, false);
}
