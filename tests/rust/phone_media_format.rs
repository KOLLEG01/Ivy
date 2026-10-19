use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_phone_bridge::media::{
    AudioFormat, Codec, evs_channels, evs_header_full, evs_select, evs_sender,
};

fn number(value: impl Into<f64>) -> IvyJson {
    IvyJson::Number(value.into())
}
fn nullable(text: Option<&str>) -> IvyJson {
    text.map(Into::into).unwrap_or(IvyJson::Null)
}
fn format_snapshot(format: &AudioFormat) -> IvyJson {
    IvyJson::object([
        ("payload", number(format.payload())),
        ("name", format.name().into()),
        ("pcmRate", number(format.pcm_rate())),
        ("rtpRate", number(format.rtp_rate())),
        ("channels", number(format.channels())),
        ("parameters", nullable(format.parameters())),
        ("duration", number(format.rtp_duration())),
    ])
}
fn original_case(operation: &str, text: Option<&str>) -> IvyJson {
    IvyJson::object([("operation", operation.into()), ("text", nullable(text))])
}
fn select_case(text: Option<&str>, answering: bool, channels: u8) -> IvyJson {
    IvyJson::object([
        ("operation", "select".into()),
        ("text", nullable(text)),
        ("answering", IvyJson::Bool(answering)),
        ("channels", number(channels)),
    ])
}
fn cases() -> Vec<IvyJson> {
    let mut cases = Vec::new();
    for rtpmap in [
        None,
        Some("EVS/16000"),
        Some("evs/16000/1"),
        Some("eVs/16000/2"),
        Some("EVS/48000/2"),
        Some(" EVS/16000"),
        Some("EVS/16000/01"),
        Some("EVS/16000/0"),
        Some("EVS/16000/3"),
        Some("EVS/16000/2 "),
        Some("ＥＶＳ/16000/2"),
    ] {
        cases.push(original_case("channels", rtpmap));
    }
    let rates = [
        "5.9", "7.2", "8", "9.6", "13.2", "16.4", "24.4", "32", "48", "64", "96", "128",
    ];
    let bandwidths = ["nb", "wb", "swb", "fb", "nb-wb", "nb-swb", "nb-fb"];
    let mut texts = vec![None, Some(String::new())];
    for text in [
        " ",
        "\t\r\n",
        "\u{85}\u{a0}\u{2007}",
        ";",
        ";;;",
        " ; ",
        "=0",
        "x=",
        "=",
        "x",
        "x=1=2",
        "x=UPPER;X=lower",
        "br=7.2-128; BR = 7.2-128",
        "br=7.2-128\n",
        "x=a\u{7f}",
        "x=a\u{85}",
        "hf-only=0",
        "hf-only=1",
        "hf-only=01",
        "hf-only=2",
        "HF-ONLY = 1",
        "hf-only=1;channels=2",
        "br=8.00-16.400;bw=nb-fb",
        "br=7.200-128.00",
        "br=0008.000",
        "br=.8",
        "br=8.",
        "br=+8",
        "br=8e0",
        "br=8,0",
        "br=8.0-8.00",
        "br=128-7.2",
        "br=7.2-128-128",
        "br=5.9",
        "br=5.9-7.2",
        "br=5.9-128",
        "br=7.20000000000000000000000000001",
        "br=7.19999999999999999999999999999",
        "br=8.00000000000000000000000000000",
        "br=128.00000000000000000000000000000",
        "br=79228162514264337593543950335",
        "br=79228162514264337593543950336",
        "br=1.00000000000000000000000000000000000000001",
        "br=7.2;bw=NB",
        "br=16.4;bw=wb-fb",
        "br=7.2;bw=wb-swb",
        "br=16.4;bw=swb-fb",
        "br=7.2-128;br-send=7.20-128.00;br-recv=7.2-128",
        "br=7.2-128;br-send=16.4-128",
        "bw=nb-fb;bw-send=fb",
        "bw=nb-fb;bw-send=nb-fb;bw-recv=nb-fb",
        "channels=1;ch-send=1;ch-recv=1",
        "channels=2;hf-only=1;ch-send=2;ch-recv=2",
        "channels=2;hf-only=0",
        "channels=02;hf-only=1",
        "channels=2;hf-only=1;ch-send=1",
        "br-send=7.2-24.4;br-recv=16.4-128;bw-send=nb;bw-recv=fb",
        "br-send=7.2-128;br-recv=7.2-24.4;bw-send=fb;bw-recv=nb",
        "br = 7.2-128 ; bw = nb-fb ; max-red = 0 ; ",
        "x-y_2=abc",
        "x.y=abc",
        "х=1",
        "x=İ",
    ] {
        texts.push(Some(text.into()));
    }
    for (key, allowed) in [
        ("dtx", &["0", "1", "-1", "true"][..]),
        ("dtx-recv", &["0", "1"][..]),
        ("evs-mode-switch", &["0", "1", "01"][..]),
        ("cmr", &["-1", "0", "1"][..]),
        ("ch-aw-recv", &["-1", "0", "1"][..]),
        ("max-red", &["0", "1", "-1"][..]),
    ] {
        for value in allowed {
            texts.push(Some(format!("{key}={value}")));
        }
    }
    for low in 0..rates.len() {
        for high in low..rates.len() {
            for bandwidth in bandwidths {
                let rate = if low == high {
                    rates[low].to_string()
                } else {
                    format!("{}-{}", rates[low], rates[high])
                };
                texts.push(Some(format!("br={rate};bw={bandwidth}")));
            }
        }
    }
    for send in ["5.9-128", "7.2-24.4", "16.4", "128"] {
        for receive in ["5.9-128", "7.2-24.4", "16.4", "128"] {
            for bandwidth in bandwidths {
                texts.push(Some(format!(
                    "br-send={send};br-recv={receive};bw-send={bandwidth};bw-recv=nb-fb"
                )));
            }
        }
    }
    for count in [24, 25] {
        texts.push(Some(
            (0..count)
                .map(|i| format!("x{i}=abc"))
                .collect::<Vec<_>>()
                .join(";"),
        ));
    }
    for count in [64, 65] {
        texts.push(Some(format!("{}=1", "x".repeat(count))));
    }
    for count in [128, 129] {
        texts.push(Some(format!("x={}", "a".repeat(count))));
    }
    for count in [64, 65] {
        texts.push(Some(format!("x={}", "😀".repeat(count))));
    }
    for count in [1024, 1025] {
        texts.push(Some(" ".repeat(count)));
    }
    let bounded = (0..8)
        .map(|i| format!("x{i}={}", "a".repeat(124)))
        .collect::<Vec<_>>()
        .join(";");
    assert_eq!(bounded.len(), 1023);
    texts.push(Some(bounded.clone() + ";"));
    texts.push(Some(bounded + ";;"));
    for text in &texts {
        cases.push(original_case("header", text.as_deref()));
        cases.push(original_case("sender", text.as_deref()));
        cases.push(select_case(text.as_deref(), true, 1));
        cases.push(select_case(text.as_deref(), false, 1));
        let stereo = text.as_ref().map_or("channels=2;hf-only=1".into(), |text| {
            format!("{text};channels=2;hf-only=1")
        });
        cases.push(select_case(Some(&stereo), true, 2));
        cases.push(select_case(Some(&stereo), false, 2));
    }
    for channels in [0, 1, 2, 3] {
        cases.push(select_case(None, true, channels));
    }
    for codec in [
        Codec::G722,
        Codec::Pcma,
        Codec::Pcmu,
        Codec::Opus,
        Codec::Evs,
    ] {
        cases.push(IvyJson::object([
            ("operation", "formats".into()),
            ("preferences", IvyJson::Array(vec![codec.name().into()])),
        ]));
    }
    for preferences in [
        vec!["G722", "PCMA", "PCMU", "OPUS", "EVS"],
        vec!["EVS", "OPUS", "PCMA", "G722", "PCMU"],
        vec!["OPUS", "EVS"],
    ] {
        cases.push(IvyJson::object([
            ("operation", "formats".into()),
            (
                "preferences",
                IvyJson::Array(preferences.into_iter().map(Into::into).collect()),
            ),
        ]));
    }
    for name in ["G722", "PCMA", "PCMU", "OPUS", "EVS", "evs", "unknown"] {
        for pcm in [8000, 16000, 48000] {
            for rtp in [8000, 16000, 48000] {
                for channels in [0, 1, 2, 3] {
                    for payload in [-1, 0, 127, 128] {
                        cases.push(IvyJson::object([
                            ("operation", "validate".into()),
                            ("payload", number(payload)),
                            ("name", name.into()),
                            ("pcmRate", number(pcm)),
                            ("rtpRate", number(rtp)),
                            ("channels", number(channels)),
                            ("parameters", nullable(Some("hf-only=1"))),
                        ]));
                    }
                }
            }
        }
    }
    for name in ["EVS", "evs"] {
        for channels in [1, 2] {
            for text in &texts {
                cases.push(IvyJson::object([
                    ("operation", "validate".into()),
                    ("payload", number(126)),
                    ("name", name.into()),
                    ("pcmRate", number(48000)),
                    ("rtpRate", number(16000)),
                    ("channels", number(channels)),
                    ("parameters", nullable(text.as_deref())),
                ]));
            }
        }
    }
    cases
}

fn actual(case: &IvyJson) -> IvyJson {
    let get = |key| case.get(key).unwrap();
    let text = case.get("text").and_then(|value| value.as_str().ok());
    let error = || IvyJson::object([("error", "invalid_format".into())]);
    match get("operation").as_str().unwrap().as_str() {
        "channels" => IvyJson::object([
            ("error", IvyJson::Null),
            ("value", number(evs_channels(text.as_deref()))),
        ]),
        "header" => match evs_header_full(text.as_deref()) {
            Ok(value) => {
                IvyJson::object([("error", IvyJson::Null), ("value", IvyJson::Bool(value))])
            }
            Err(_) => error(),
        },
        "sender" => match evs_sender(text.as_deref()) {
            Ok(value) => IvyJson::object([
                ("error", IvyJson::Null),
                ("bitrate", number(value.bitrate)),
                ("bandwidth", number(value.bandwidth)),
            ]),
            Err(_) => error(),
        },
        "select" => {
            let selected = evs_select(
                text.as_deref(),
                get("answering").as_bool().unwrap(),
                get("channels").as_f64().unwrap() as u8,
            );
            IvyJson::object([
                ("error", IvyJson::Null),
                ("accepted", IvyJson::Bool(selected.is_some())),
                ("selected", nullable(selected.as_deref())),
            ])
        }
        "validate" => {
            let name = get("name").as_str().unwrap();
            let parameters = get("parameters").as_str().ok();
            match AudioFormat::new(
                get("payload").as_f64().unwrap() as i32,
                &name,
                get("pcmRate").as_f64().unwrap() as u32,
                get("rtpRate").as_f64().unwrap() as u32,
                get("channels").as_f64().unwrap() as u8,
                parameters.as_deref(),
            ) {
                Ok(format) => IvyJson::object([
                    ("error", IvyJson::Null),
                    ("duration", number(format.rtp_duration())),
                ]),
                Err(_) => error(),
            }
        }
        "formats" => {
            let preferences = get("preferences").as_array().unwrap();
            let codecs = preferences.iter().map(|name| {
                [
                    Codec::G722,
                    Codec::Pcma,
                    Codec::Pcmu,
                    Codec::Opus,
                    Codec::Evs,
                ]
                .into_iter()
                .find(|codec| name.as_str().is_ok_and(|name| codec.name() == name))
                .unwrap()
            });
            IvyJson::object([
                ("error", IvyJson::Null),
                (
                    "formats",
                    IvyJson::Array(
                        codecs
                            .flat_map(|codec| codec.offered_formats())
                            .map(|format| format_snapshot(&format))
                            .collect(),
                    ),
                ),
            ])
        }
        _ => panic!("Unknown case"),
    }
}

#[test]
fn configured_formats_keep_packet_clocks_and_directional_evs_constraints() {
    use ivy_phone_bridge::{native_config::NativeSipConfiguration, native_rpc::NativeRequest};
    let frame = r#"{"version":1,"epoch":"01234567-89ab-cdef-0123-456789abcdef","requestId":1,"operationId":"11234567-89ab-cdef-0123-456789abcdef","method":"configure","params":{"binding":{"address":"127.0.0.1","port":0,"transport":"udp"},"codecs":{"preferences":["EVS","G722","OPUS","PCMA","PCMU"]},"incomingPeers":[],"registration":null,"password":null}}"#;
    let request = NativeRequest::parse(frame.as_bytes()).unwrap();
    let config = NativeSipConfiguration::parse(&request).unwrap();
    let formats = config.codecs().formats();
    assert_eq!(
        formats
            .iter()
            .map(|f| (f.payload(), f.pcm_rate(), f.rtp_duration()))
            .collect::<Vec<_>>(),
        [
            (126, 48000, 320),
            (125, 48000, 320),
            (9, 16000, 160),
            (111, 48000, 960),
            (8, 8000, 160),
            (0, 8000, 160)
        ]
    );
    let selected = evs_select(
        Some("br-send=7.2-24.4;br-recv=16.4-128;bw-send=nb;bw-recv=fb"),
        true,
        1,
    )
    .unwrap();
    assert_eq!(
        selected,
        "br-send=128;br-recv=7.2-24.4;bw-send=fb;bw-recv=nb;max-red=0"
    );
    assert_eq!(evs_sender(Some(&selected)).unwrap().bitrate, 128000);
    assert!(evs_select(Some("br=5.9-128"), false, 1).is_none());
    assert!(evs_select(Some("channels=2"), true, 2).is_none());
    assert!(evs_select(Some("channels=2;hf-only=1;dtx=1"), true, 2).is_none());
}

#[test]
fn original_media_format_and_evs_differential() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original managed media oracle unavailable; this comparison was not executed.");
        return;
    }
    let cases = cases();
    let mut child = Command::new("pwsh")
        .args([
            "-NoProfile",
            "-File",
            "tests/fixtures/phone-media-format-oracle.ps1",
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
            stringify(&IvyJson::Array(cases.clone()), 16_000_000)
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
    let results = parse(std::str::from_utf8(&output.stdout).unwrap(), 16_000_000).unwrap();
    let results = results.as_array().unwrap();
    assert_eq!(results.len(), cases.len());
    let records = IvyJson::Array(
        cases
            .iter()
            .zip(results)
            .map(|(input, expected)| {
                IvyJson::object([
                    ("input", input.clone()),
                    ("expected", expected.clone()),
                    ("actual", actual(input)),
                ])
            })
            .collect(),
    );
    if let Some(path) = std::env::var_os("IVY_PHONE_MEDIA_ORACLE_TRACE") {
        std::fs::write(path, stringify(&records, 32_000_000).unwrap()).unwrap();
    }
    let mut failures = Vec::new();
    for (index, (input, expected)) in cases.iter().zip(results).enumerate() {
        let value = actual(input);
        if &value != expected {
            failures.push(format!(
                "Case {index}: {} != {}; input {}",
                stringify(&value, 65536).unwrap(),
                stringify(expected, 65536).unwrap(),
                stringify(input, 65536).unwrap()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches of {}: {}",
        failures.len(),
        cases.len(),
        failures.into_iter().take(25).collect::<Vec<_>>().join("\n")
    );
}
