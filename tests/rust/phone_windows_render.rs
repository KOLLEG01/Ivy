use super::*;
use crate::media::AudioSettings;
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

use std::sync::{Arc, Mutex};

fn pcm(samples: &[f32]) -> String {
    samples
        .iter()
        .flat_map(|x| x.to_le_bytes())
        .map(|x| format!("{x:02x}"))
        .collect()
}
fn input(value: &Value) -> Vec<f32> {
    value["hex"]
        .as_str()
        .unwrap()
        .as_bytes()
        .chunks_exact(8)
        .map(|word| {
            let mut bytes = [0; 4];
            for (byte, chars) in bytes.iter_mut().zip(word.chunks_exact(2)) {
                *byte = u8::from_str_radix(std::str::from_utf8(chars).unwrap(), 16).unwrap();
            }
            f32::from_le_bytes(bytes)
        })
        .collect()
}
fn op(name: &str) -> Value {
    json!({"op":name})
}
fn read(count: usize) -> Value {
    json!({"op":"output","count":count})
}
fn write(samples: &[f32]) -> Value {
    json!({"op":"write","hex":pcm(samples)})
}
fn permit(mode: i32) -> Value {
    json!({"op":"permit","mode":mode})
}
fn scenario(name: String, rate: u32, channels: u16, prebuffer: u16, actions: Vec<Value>) -> Value {
    json!({"name":name,"rate":rate,"channels":channels,"queue":200,"prebuffer":prebuffer,"actions":actions})
}
fn opened(mut actions: Vec<Value>) -> Vec<Value> {
    let mut prefix = vec![permit(1), op("resume")];
    prefix.append(&mut actions);
    prefix
}

fn cases() -> Vec<Value> {
    // Dyadic input avoids unrelated libm/signal-generator rounding differences.
    let signal: Vec<f32> = (0..9200)
        .map(|i| ((i * 13 % 113) as f32 - 56.0) / 64.0)
        .collect();
    let mut cases = Vec::new();
    for rate in [
        8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 88200, 96000, 192000,
    ] {
        for channels in [1, 2] {
            let mut actions = vec![
                read(3),
                write(&[f32::NAN]),
                permit(1),
                op("resume"),
                write(&signal),
            ];
            actions.extend([0, 1, 2, 3, 7, 65, 127, 480, 19, 0, 257].map(read));
            actions.extend([
                op("resume"),
                read(33),
                write(&signal[..7]),
                read(9),
                op("status"),
            ]);
            cases.push(scenario(
                format!("rate-{rate}-channels-{channels}-chunk-carry"),
                rate,
                channels,
                0,
                actions,
            ));
        }
    }
    for rate in [24000, 44100, 48000, 96000] {
        for channels in [1, 2] {
            cases.push(scenario(
                format!("prebuffer-{rate}-{channels}"),
                rate,
                channels,
                20,
                opened(vec![
                    write(&signal[..959]),
                    read(0),
                    read(17),
                    write(&signal[959..960]),
                    read(21),
                    read(2100),
                    write(&signal[..17]),
                    read(37),
                    op("suspend"),
                    op("resume"),
                    read(21),
                ]),
            ));
            cases.push(scenario(
                format!("resume-without-suspended-read-{rate}-{channels}"),
                rate,
                channels,
                0,
                opened(vec![
                    write(&signal[..400]),
                    read(31),
                    op("suspend"),
                    op("resume"),
                    read(127),
                    write(&[0.75]),
                    read(11),
                    op("resume"),
                    read(9),
                    op("close"),
                    read(7),
                ]),
            ));
            cases.push(scenario(
                format!("underrun-refill-keeps-history-{rate}-{channels}"),
                rate,
                channels,
                0,
                opened(vec![
                    write(&[1.0]),
                    read(7),
                    read(11),
                    write(&[-0.5, 0.25, -0.125]),
                    read(9),
                    read(4097),
                    write(&signal),
                    read(17),
                    op("status"),
                ]),
            ));
        }
    }
    for rate in [24000, 48000, 96000] {
        for channels in [1, 2] {
            for decisions in [
                vec![0],
                vec![1, 0],
                vec![1, 1, 0],
                vec![-1],
                vec![1, -1],
                vec![1, 1, -1],
            ] {
                cases.push(scenario(
                    format!("permit-{rate}-{channels}-{decisions:?}"),
                    rate,
                    channels,
                    0,
                    opened(vec![
                        write(&signal[..400]),
                        read(31),
                        json!({"op":"capture","hex":pcm(&[0.5,0.5]),"flags":0}),
                        json!({"op":"permit","mode":1,"sequence":decisions}),
                        read(17),
                        op("status"),
                        permit(1),
                        op("resume"),
                        read(33),
                        op("status"),
                    ]),
                ));
            }
        }
    }
    for rate in [44100, 48000, 96000] {
        for channels in [1, 2] {
            cases.push(scenario(
                format!("finite-bits-{rate}-{channels}"),
                rate,
                channels,
                0,
                opened(vec![
                    write(&[
                        -0.0,
                        0.0,
                        f32::from_bits(1),
                        f32::from_bits(0x80000001),
                        -1.0,
                        1.0,
                    ]),
                    read(3),
                    read(0),
                    read(25),
                    read(4097),
                    op("status"),
                ]),
            ));
            cases.push(scenario(
                format!("generation-wrap-and-fail-{rate}-{channels}"),
                rate,
                channels,
                0,
                opened(vec![
                    write(&signal[..400]),
                    read(31),
                    op("suspend"),
                    json!({"op":"generation","value":i64::MAX}),
                    op("resume"),
                    read(37),
                    write(&[1.0]),
                    read(5),
                    write(&[f32::NAN]),
                    read(7),
                    op("resume"),
                    op("close"),
                    read(9),
                ]),
            ));
        }
    }
    // A zero-length read can fetch four input samples even at a very low rate.
    cases.push(scenario(
        "one-hz-zero-length-prefetch".into(),
        1,
        2,
        0,
        opened(vec![write(&signal), read(0), read(1), op("status")]),
    ));
    cases
}

struct Permit {
    mode: i32,
    calls: usize,
    sequence: Vec<i32>,
    next: usize,
}
fn settings(prebuffer: u16) -> AudioSettings {
    AudioSettings {
        capture_endpoint: "capture-fixture".into(),
        render_endpoint: "render-fixture".into(),
        source_mode: "microphone".into(),
        capture_buffer_ms: 20,
        render_latency_ms: 10,
        queue_ms: 200,
        mute: None,
        playback_prebuffer_ms: prebuffer,
        enable_wasapi_low_latency: true,
        loopback_render_endpoint: None,
    }
}
fn run_rust(item: &Value) -> Value {
    let permit = Arc::new(Mutex::new(Permit {
        mode: 0,
        calls: 0,
        sequence: vec![],
        next: 0,
    }));
    let p = permit.clone();
    let mut owner = WindowsAudioBuffers::new(
        &settings(item["prebuffer"].as_u64().unwrap() as u16),
        Arc::new(move || {
            let decision = {
                let mut p = p.lock().unwrap();
                p.calls += 1;
                let value = p.sequence.get(p.next).copied().unwrap_or(p.mode);
                p.next += 1;
                value
            };
            assert!(decision >= 0, "Synthetic permit");
            decision > 0
        }),
    )
    .unwrap();
    let rate = item["rate"].as_u64().unwrap() as u32;
    let channels = item["channels"].as_u64().unwrap() as u16;
    let mut renderer = RenderOutput::new(rate, channels).unwrap();
    let mut trace = vec![];
    for action in item["actions"].as_array().unwrap() {
        let name = action["op"].as_str().unwrap();
        let mut ok = true;
        let mut count = None;
        let mut open = None;
        let mut output = vec![];
        match name {
            "permit" => {
                let mut p = permit.lock().unwrap();
                p.mode = action["mode"].as_i64().unwrap() as i32;
                p.next = 0;
                p.sequence = action["sequence"]
                    .as_array()
                    .map(|a| a.iter().map(|v| v.as_i64().unwrap() as i32).collect())
                    .unwrap_or_default();
            }
            "resume" => ok = owner.resume().is_ok(),
            "suspend" => owner.suspend(),
            "fail" => owner.fail(),
            "close" => owner.close(),
            "generation" => owner.generation = action["value"].as_i64().unwrap(),
            "capture" => {
                let values = input(action);
                let bytes: Vec<u8> = values.iter().flat_map(|x| x.to_le_bytes()).collect();
                owner.captured(&bytes, action["flags"].as_u64().unwrap() as u32);
            }
            "write" => owner.write_received(&input(action)),
            "output" => {
                output = vec![0.75; action["count"].as_u64().unwrap() as usize];
                count = Some(renderer.read(&mut owner, &mut output));
            }
            "open" => open = Some(owner.is_open()),
            "status" => {}
            _ => panic!("Unknown action"),
        }
        let s = owner.snapshot();
        trace.push(json!({"op":name,"ok":ok,"count":count,"open":open,"pcm":pcm(&output),"pipeline":renderer.pipeline.is_some(),"snapshot":{"state":s.state,"generation":s.generation,"captureDropped":s.capture_dropped,"renderDropped":s.render_dropped,"captureQueued":s.capture_queued,"renderQueued":s.render_queued,"permitCalls":permit.lock().unwrap().calls}}));
    }
    owner.close();
    json!({"name":item["name"],"trace":trace,"noNativeResources":true,"format":{"sampleRate":rate,"channels":channels,"bits":32,"blockAlign":channels*4,"encoding":3}})
}

#[test]
fn published_render_output_matches_pcm_generation_and_permit_boundaries() {
    let input = cases();
    assert_eq!(input.len(), 95);
    let data: Value =
        serde_json::from_str(include_str!("../fixtures/phone-windows-audio-cases.json")).unwrap();
    let original = data["render"]["expected"].clone();
    let mut normalized_input = json!(input);
    compact_pcm(&mut normalized_input);
    assert_eq!(data["render"]["cases"], normalized_input);
    let mut actual: Value = input.iter().map(run_rust).collect::<Vec<_>>().into();
    compact_pcm(&mut actual);
    if let Some(dir) = std::env::var_os("IVY_PHONE_WINDOWS_RENDER_TRACE") {
        std::fs::create_dir_all(&dir).unwrap();
        for (name, value) in [
            ("render-input.json", json!(input)),
            ("render-original.json", original.clone()),
            ("render-rust.json", actual.clone()),
        ] {
            std::fs::write(
                std::path::Path::new(&dir).join(name),
                serde_json::to_vec_pretty(&value).unwrap(),
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
        for (step, (old, new)) in original["trace"]
            .as_array()
            .unwrap()
            .iter()
            .zip(actual["trace"].as_array().unwrap())
            .enumerate()
        {
            assert_eq!(new, old, "case{index} {} step{step}", input[index]["name"]);
        }
        assert_eq!(actual, original, "case{index} {}", input[index]["name"]);
    }
    assert_eq!(actual, original);
}

#[test]
fn native_mix_format_admits_mono_stereo_and_refuses_zero_rate() {
    for channels in [0, 3, u16::MAX] {
        assert!(matches!(
            RenderOutput::new(48000, channels),
            Err(CallAudioError::InvalidRequest)
        ));
    }
    assert!(matches!(
        RenderOutput::new(0, 1),
        Err(CallAudioError::InvalidRequest)
    ));
    assert!(RenderOutput::new(1, 1).is_ok());
    assert!(RenderOutput::new(192000, 2).is_ok());
}
