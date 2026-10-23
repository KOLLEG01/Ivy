use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
fn compact_pcm(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if ["pcm", "hex"].contains(&key.as_str())
                    && value.as_str().is_some_and(|text| text.len() > 512)
                {
                    let text = value.as_str().unwrap();
                    let sha256: String = Sha256::digest(text.as_bytes())
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect();
                    *value = json!({"hexChars":text.len(),"sha256":sha256});
                } else {
                    compact_pcm(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                compact_pcm(value);
            }
        }
        _ => (),
    }
}

use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn bytes(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
fn pcm(values: &[f32]) -> String {
    hex(&values
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>())
}
fn permit(mode: i32) -> Value {
    json!({"op":"permit","mode":mode})
}
fn capture(values: &[f32], flags: u32) -> Value {
    json!({"op":"capture","hex":pcm(values),"flags":flags})
}
fn receive(values: &[f32]) -> Value {
    json!({"op":"write","hex":pcm(values)})
}
fn read(op: &str, count: usize) -> Value {
    json!({"op":op,"count":count})
}
fn op(value: &str) -> Value {
    json!({"op":value})
}
fn scenario(name: &str, prebuffer: usize, actions: Vec<Value>) -> Value {
    json!({"name":name,"queue":20,"prebuffer":prebuffer,"actions":actions})
}
fn opened(mut actions: Vec<Value>) -> Vec<Value> {
    let mut result = vec![permit(1), op("resume")];
    result.append(&mut actions);
    result
}

fn cases() -> Vec<Value> {
    let stereo: Vec<f32> = (0..1200)
        .flat_map(|i| {
            let x = (i % 17) as f32 / 16.0;
            [x, -x * 0.5]
        })
        .collect();
    let received: Vec<f32> = (0..1200).map(|i| (i % 17) as f32 / 16.0).collect();
    let mut late_bad = stereo[..2050].to_vec();
    late_bad[2048] = f32::NAN;
    let mut bad_receive = received.clone();
    bad_receive[1199] = f32::NAN;
    vec![
        scenario(
            "signed-generation-wrap",
            0,
            vec![
                json!({"op":"generation","value":i64::MAX}),
                permit(1),
                op("resume"),
                op("status"),
                op("suspend"),
                json!({"op":"generation","value":-1}),
                op("resume"),
                op("status"),
            ],
        ),
        scenario(
            "initial-denial-and-ignored-input",
            0,
            vec![
                op("status"),
                capture(&[f32::NAN, f32::NAN], 0),
                receive(&[f32::INFINITY]),
                read("read", 3),
                read("render", 3),
                op("open"),
                op("resume"),
            ],
        ),
        scenario(
            "stereo-downmix-clamp-and-signed-zero",
            0,
            opened(vec![
                capture(&[0.25, 0.75, -0.5, 0.25, 2., 2., -2., -2., -0., -0.], 0),
                read("read", 7),
            ]),
        ),
        scenario(
            "capture-block-boundaries",
            0,
            opened(vec![capture(&stereo[..2050], 0), read("read", 965)]),
        ),
        scenario(
            "capture-overflow-is-oldest-first",
            0,
            opened(vec![
                capture(&stereo, 0),
                read("read", 965),
                op("suspend"),
                op("resume"),
                op("status"),
            ]),
        ),
        scenario(
            "nonfinite-after-two-blocks-clears-both-queues",
            0,
            opened(vec![
                receive(&[0.5; 5]),
                capture(&late_bad, 0),
                read("read", 4),
                read("render", 4),
                op("resume"),
            ]),
        ),
        scenario(
            "misaligned-open-capture-fails",
            0,
            opened(vec![
                receive(&[0.5; 5]),
                json!({"op":"capture","hex":"00000000000000","flags":0}),
                op("status"),
            ]),
        ),
        scenario(
            "misaligned-suspended-capture-is-ignored",
            0,
            vec![
                json!({"op":"capture","hex":"00","flags":0}),
                op("status"),
                op("resume"),
            ],
        ),
        scenario(
            "silent-capture-never-decodes-nan",
            0,
            opened(vec![
                capture(&[f32::NAN, f32::INFINITY, -f32::INFINITY, f32::NAN], 2),
                read("read", 4),
            ]),
        ),
        scenario(
            "non-silent-flags-are-carried",
            0,
            opened(vec![capture(&[0.25, 0.75], 5), read("read", 2)]),
        ),
        scenario(
            "all-flags-include-silence",
            0,
            opened(vec![
                capture(&[f32::NAN, f32::NAN], u32::MAX),
                read("read", 2),
            ]),
        ),
        scenario(
            "nonfinite-left-capture-fails",
            0,
            opened(vec![
                capture(&[f32::NAN, 0.], 0),
                op("open"),
                read("read", 2),
            ]),
        ),
        scenario(
            "nonfinite-right-capture-fails",
            0,
            opened(vec![capture(&[0., f32::NEG_INFINITY], 0), op("resume")]),
        ),
        scenario(
            "finite-sum-overflow-fails-before-clamp",
            0,
            opened(vec![capture(&[f32::MAX, f32::MAX], 0), read("read", 2)]),
        ),
        scenario(
            "receive-overflow-and-retained-drop-count",
            0,
            opened(vec![
                receive(&received),
                read("render", 964),
                op("suspend"),
                op("resume"),
                op("status"),
            ]),
        ),
        scenario(
            "render-prebuffer-and-no-rebuffer-after-underrun",
            20,
            opened(vec![
                receive(&received[..479]),
                read("render", 4),
                receive(&received[479..960]),
                read("render", 964),
                receive(&[0.75]),
                read("render", 2),
            ]),
        ),
        scenario(
            "suspend-and-resume-clears-capture-and-render",
            0,
            opened(vec![
                capture(&[1., 0., -1., 0.], 0),
                receive(&[0.5, 0.25]),
                op("suspend"),
                read("read", 2),
                read("render", 2),
                op("resume"),
                read("read", 2),
                read("render", 2),
            ]),
        ),
        scenario(
            "status-is-passive-until-permit-observation",
            0,
            opened(vec![
                capture(&[1., 0.], 0),
                receive(&[0.5]),
                permit(0),
                op("status"),
                read("read", 2),
                op("resume"),
                permit(1),
                op("resume"),
                read("render", 2),
            ]),
        ),
        scenario(
            "permit-callback-errors-deny-without-route-failure",
            0,
            vec![
                permit(-1),
                op("resume"),
                permit(1),
                op("resume"),
                capture(&[1., 0.], 0),
                permit(-1),
                read("render", 2),
                op("status"),
                permit(1),
                op("resume"),
                read("read", 2),
            ],
        ),
        scenario(
            "resume-is-idempotent-but-still-checks-permit",
            0,
            opened(vec![
                capture(&[1., 0.], 0),
                op("resume"),
                permit(0),
                op("resume"),
                op("status"),
                op("open"),
                permit(1),
                op("resume"),
            ]),
        ),
        scenario(
            "failed-and-closed-states-never-resume",
            0,
            opened(vec![
                capture(&[1., 0.], 0),
                op("fail"),
                op("resume"),
                op("suspend"),
                read("render", 3),
                op("close"),
                op("fail"),
                op("resume"),
                op("close"),
            ]),
        ),
        scenario(
            "late-callback-after-close-is-ignored",
            0,
            opened(vec![
                receive(&[0.5]),
                op("close"),
                capture(&[f32::NAN, f32::NAN], 0),
                receive(&[f32::NAN]),
                op("open"),
                read("read", 2),
                read("render", 2),
            ]),
        ),
        scenario(
            "receive-validation-is-before-queue-write",
            0,
            opened(vec![
                capture(&[1., 0.], 0),
                receive(&[0.5; 10]),
                receive(&bad_receive),
                op("status"),
                read("read", 2),
                read("render", 2),
            ]),
        ),
        scenario(
            "receive-range-rejects-before-clipping",
            0,
            opened(vec![receive(&[0., 1.0001]), op("status")]),
        ),
        scenario(
            "receive-exact-finite-bits",
            0,
            opened(vec![
                receive(&[
                    -1.,
                    -0.,
                    0.,
                    1.,
                    f32::from_bits(1),
                    f32::from_bits(0x80000001),
                ]),
                read("render", 8),
            ]),
        ),
        scenario(
            "empty-open-operations-still-check-permit",
            0,
            opened(vec![
                capture(&[], 0),
                receive(&[]),
                read("read", 0),
                read("render", 0),
                op("status"),
            ]),
        ),
        scenario(
            "render-permit-loss-clears-queued-capture",
            0,
            opened(vec![
                capture(&[1., 0.], 0),
                receive(&[0.5]),
                permit(0),
                read("render", 2),
                op("status"),
                permit(1),
                op("resume"),
                read("read", 2),
            ]),
        ),
        scenario(
            "stop-before-first-resume",
            0,
            vec![
                op("fail"),
                permit(-1),
                op("resume"),
                op("suspend"),
                op("close"),
                op("resume"),
                op("status"),
            ],
        ),
    ]
}

fn run_rust(item: &Value) -> Value {
    let mode = Arc::new(AtomicI32::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let settings = AudioSettings {
        capture_endpoint: "capture-fixture".into(),
        render_endpoint: "render-fixture".into(),
        source_mode: "microphone".into(),
        capture_buffer_ms: 20,
        render_latency_ms: 10,
        queue_ms: item["queue"].as_u64().unwrap() as u16,
        mute: None,
        playback_prebuffer_ms: item["prebuffer"].as_u64().unwrap() as u16,
        enable_wasapi_low_latency: true,
        loopback_render_endpoint: None,
    };
    let (m, c) = (mode.clone(), calls.clone());
    let mut route = WindowsAudioBuffers::new(
        &settings,
        Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            let m = m.load(Ordering::SeqCst);
            assert!(m >= 0, "Synthetic permit");
            m > 0
        }),
    )
    .unwrap();
    let mut trace = vec![];
    for action in item["actions"].as_array().unwrap() {
        let name = action["op"].as_str().unwrap();
        let mut ok = true;
        let mut count = None;
        let mut open = None;
        let mut output = vec![];
        match name {
            "permit" => mode.store(action["mode"].as_i64().unwrap() as i32, Ordering::SeqCst),
            "generation" => route.generation = action["value"].as_i64().unwrap(),
            "resume" => ok = route.resume().is_ok(),
            "suspend" => route.suspend(),
            "fail" => route.fail(),
            "close" => route.close(),
            "capture" => route.captured(
                &bytes(action["hex"].as_str().unwrap()),
                action["flags"].as_u64().unwrap() as u32,
            ),
            "write" => route.write_received(
                &bytes(action["hex"].as_str().unwrap())
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                    .collect::<Vec<_>>(),
            ),
            "read" | "render" => {
                output = vec![0.75; action["count"].as_u64().unwrap() as usize];
                count = Some(if name == "read" {
                    route.read_captured(&mut output)
                } else {
                    route.render(&mut output)
                });
            }
            "open" => open = Some(route.is_open()),
            "status" => {}
            _ => panic!("Unknown action"),
        }
        let s = route.snapshot();
        trace.push(json!({"op":name,"ok":ok,"count":count,"open":open,"pcm":pcm(&output),"snapshot":{"state":s.state,"generation":s.generation,"captureDropped":s.capture_dropped,"renderDropped":s.render_dropped,"captureQueued":s.capture_queued,"renderQueued":s.render_queued,"permitCalls":calls.load(Ordering::SeqCst)}}));
    }
    route.close();
    json!({"name":item["name"],"trace":trace,"noNativeResources":true})
}

#[test]
fn published_route_capture_render_authority_and_queue_boundaries_agree() {
    let input = cases();
    assert_eq!(input.len(), 28);
    let data: Value =
        serde_json::from_str(include_str!("../fixtures/phone-windows-audio-cases.json")).unwrap();
    let original = data["buffers"]["expected"].clone();
    let mut normalized_input = json!(input);
    compact_pcm(&mut normalized_input);
    assert_eq!(data["buffers"]["cases"], normalized_input);
    let mut actual: Value = input.iter().map(run_rust).collect::<Vec<_>>().into();
    compact_pcm(&mut actual);
    if let Some(dir) = std::env::var_os("IVY_PHONE_WINDOWS_AUDIO_TRACE") {
        std::fs::create_dir_all(&dir).unwrap();
        for (name, value) in [
            ("windows-audio-buffer-original.json", &original),
            ("windows-audio-buffer-rust.json", &actual),
            ("windows-audio-buffer-input.json", &json!(input)),
        ] {
            std::fs::write(
                std::path::Path::new(&dir).join(name),
                serde_json::to_vec_pretty(value).unwrap(),
            )
            .unwrap();
        }
    }
    for (index, (original, actual)) in original
        .as_array()
        .unwrap()
        .iter()
        .zip(actual.as_array().unwrap())
        .enumerate()
    {
        assert_eq!(actual, original, "case{} {}", index, input[index]["name"]);
    }
    assert_eq!(actual, original);
}
