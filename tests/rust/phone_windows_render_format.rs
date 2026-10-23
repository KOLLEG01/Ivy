use super::*;
use crate::media::AudioSettings;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|x| format!("{x:02x}")).collect()
}
fn decode(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|x| u8::from_str_radix(std::str::from_utf8(x).unwrap(), 16).unwrap())
        .collect()
}
fn save(name: &str, value: &Value) {
    if let Some(dir) = std::env::var_os("IVY_PHONE_RENDER_FORMAT_TRACE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
fn compact_hex(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "hex" && value.as_str().is_some_and(|text| text.len() > 512) {
                    let text = value.as_str().unwrap();
                    let sha256: String = Sha256::digest(text.as_bytes())
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect();
                    *value = json!({"hexChars":text.len(),"sha256":sha256});
                } else {
                    compact_hex(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                compact_hex(value);
            }
        }
        _ => (),
    }
}
fn compare(name: &str, input: Vec<Value>, mut actual: Value) {
    let data: Value =
        serde_json::from_str(include_str!("../fixtures/phone-render-format-cases.json")).unwrap();
    let mut input = json!(input);
    compact_hex(&mut input);
    compact_hex(&mut actual);
    assert_eq!(data[name]["cases"], input);
    assert_eq!(data[name]["expected"], actual);
}
fn sample_cases() -> Vec<Value> {
    let mut values: Vec<f32> = [
        0, 0x80000000, 1, 0x80000001, 0x007fffff, 0x00800000, 0x3f7fffff, 0xbf7fffff, 0x3f800000,
        0xbf800000, 0x3f800001, 0xbf800001, 0x7f800000, 0xff800000, 0x7fc01234, 0xffc01234,
        0x7f801234, 0xff801234, 0x3eaaaaab, 0xbeaaaaab, 0x38000100, 0xb8000100, 0x34000001,
        0xb4000001,
    ]
    .into_iter()
    .map(f32::from_bits)
    .collect();
    values.extend((0..129).map(|i| (i as f32 - 64.0) / 64.0));
    values.extend((0..256_u32).map(|i| f32::from_bits(i.wrapping_mul(0x01010101))));
    let bytes: Vec<u8> = values.iter().flat_map(|x| x.to_le_bytes()).collect();
    let descriptors = [
        ("float", 3, 32, FLOAT_SUBTYPE, 32, 3),
        ("pcm16", 1, 16, PCM_SUBTYPE, 16, 3),
        ("pcm24", 1, 24, PCM_SUBTYPE, 24, 3),
        ("pcm32-fallback", 1, 32, PCM_SUBTYPE, 32, 3),
        ("pcm8-fallback", 1, 8, PCM_SUBTYPE, 8, 3),
        ("float64-fallback", 3, 64, FLOAT_SUBTYPE, 64, 3),
        ("alaw-fallback", 6, 16, PCM_SUBTYPE, 16, 3),
        ("ext-pcm16", 65534, 16, PCM_SUBTYPE, 16, 3),
        ("ext-pcm24", 65534, 24, PCM_SUBTYPE, 24, 3),
        ("ext-pcm32-fallback", 65534, 32, PCM_SUBTYPE, 24, 3),
        ("ext-float", 65534, 32, FLOAT_SUBTYPE, 32, 3),
        ("ext-float64-fallback", 65534, 64, FLOAT_SUBTYPE, 64, 3),
        ("ext-unknown-fallback", 65534, 32, 0, 32, 3),
        ("ext-valid-bits-ignored", 65534, 24, PCM_SUBTYPE, 20, 4),
        ("ext-invalid-bits-ignored", 65534, 16, PCM_SUBTYPE, -1, 0),
        (
            "ext-float-metadata-ignored",
            65534,
            32,
            FLOAT_SUBTYPE,
            24,
            -1,
        ),
    ];
    let mut cases = vec![];
    for rate in [24000, 48000] {
        for channels in [1, 2] {
            for (name, tag, bits, sub, valid_bits, mask) in descriptors {
                cases.push(json!({"kind":"samples","name":format!("{name}-{rate}-{channels}"),"rate":rate,"sourceRate":rate,"channels":channels,"tag":tag,"bits":bits,"sub":format!("{sub:032x}"),"validBits":valid_bits,"mask":mask,"hex":hex(&bytes),"reads":[0,1,2,3,5,7,31,257,4097,3,0,65]}));
            }
        }
    }
    for (tag, bits, sub) in [
        (3, 32, FLOAT_SUBTYPE),
        (1, 16, PCM_SUBTYPE),
        (65534, 24, PCM_SUBTYPE),
    ] {
        cases.push(json!({"kind":"samples","name":format!("rate-mismatch-{tag}-{bits}"),"rate":48000,"sourceRate":44100,"channels":2,"tag":tag,"bits":bits,"sub":format!("{sub:032x}"),"validBits":bits,"mask":3,"hex":hex(&bytes),"reads":[65]}));
    }
    assert_eq!(cases.len(), 67);
    cases
}
fn adapt(item: &Value) -> Value {
    let format = MixFormat {
        rate: item["rate"].as_u64().unwrap() as u32,
        tag: item["tag"].as_u64().unwrap() as u16,
        bits: item["bits"].as_u64().unwrap() as u16,
        subtype: u128::from_str_radix(item["sub"].as_str().unwrap(), 16).unwrap(),
    };
    let encoding = format.adaptation(item["sourceRate"].as_u64().unwrap() as u32);
    let bytes = decode(item["hex"].as_str().unwrap());
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
        .collect();
    let mut position = 0;
    let mut rows = vec![];
    let mut requests = vec![];
    if let Some(encoding) = encoding {
        for read in item["reads"].as_array().unwrap() {
            let mut output = vec![0xcc; read.as_u64().unwrap() as usize];
            let request = output.len() / encoding.width();
            requests.push(request);
            let count = request.min(samples.len() - position);
            let read = encoding.encode(&samples[position..position + count], &mut output);
            position += count;
            rows.push(json!({"count":read,"hex":hex(&output)}));
        }
    }
    json!({"name":item["name"],"supported":encoding.is_some(),"encoding":encoding.map(|x|format!("{x:?}")),"byteCompatible":encoding==Some(Encoding::Float32),"requests":requests,"position":position,"rows":rows})
}
#[test]
fn published_format_adapter_bytes_and_target_admission_agree() {
    let input = sample_cases();
    let actual = json!(input.iter().map(adapt).collect::<Vec<_>>());
    compare("render-format", input, actual);
}

#[test]
fn published_engine_period_rounding_and_uint_overflow_agree() {
    let mut tuples = vec![
        [480, 48, 96, 960],
        [480, 48, 97, 960],
        [480, 48, 97, 120],
        [480, 48, 96, 50],
        [480, 0, 99, 50],
        [480, 48, 0, 960],
        [480, 48, 1, 0],
        [0, 1, u32::MAX, u32::MAX],
        [0, 2, u32::MAX, u32::MAX],
        [0, u32::MAX, u32::MAX - 1, u32::MAX],
        [0, 0, 0, 0],
        [u32::MAX, 48, 97, 960],
    ];
    let mut seed = 0x4d595df4_u32;
    for index in 0..96 {
        let mut tuple = [0; 4];
        for value in &mut tuple {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *value = seed;
        }
        if index % 4 == 0 {
            tuple[1] = index;
        }
        tuples.push(tuple);
    }
    let input: Vec<Value> = tuples
        .iter()
        .enumerate()
        .map(|(i, x)| json!({"kind":"period","name":format!("period-{i}"),"values":x}))
        .collect();
    let actual=json!(input.iter().zip(tuples).map(|(item,[default,fundamental,minimum,maximum])|json!({"name":item["name"],"period":EnginePeriod{default,fundamental,minimum,maximum}.choose()})).collect::<Vec<_>>());
    assert_eq!(input.len(), 108);
    compare("render-period", input, actual);
}

#[test]
fn format_byte_output_composes_authority_and_generation_without_tail_writes() {
    let settings = AudioSettings {
        capture_endpoint: "capture-fixture".into(),
        render_endpoint: "render-fixture".into(),
        source_mode: "microphone".into(),
        capture_buffer_ms: 20,
        render_latency_ms: 10,
        queue_ms: 200,
        mute: None,
        playback_prebuffer_ms: 0,
        enable_wasapi_low_latency: true,
        loopback_render_endpoint: None,
    };
    let mut rows = vec![];
    for rate in [24000, 48000, 96000] {
        for channels in [1, 2] {
            for encoding in [Encoding::Float32, Encoding::Pcm16, Encoding::Pcm24] {
                for offset in [0, 1] {
                    let allowed = Arc::new(AtomicBool::new(true));
                    let p = allowed.clone();
                    let mut owner = WindowsAudioBuffers::new(
                        &settings,
                        Arc::new(move || p.load(Ordering::SeqCst)),
                    )
                    .unwrap();
                    let mut output = RenderBytes::new(rate, channels, encoding).unwrap();
                    owner.resume().unwrap();
                    owner.write_received(&[0.5; 200]);
                    owner.captured(&[0; 16], 0);
                    // Vec<u32> guarantees the aligned case; offset=1 exercises its unaligned counterpart.
                    let mut storage = vec![0xcccccccc_u32; 128];
                    let bytes = unsafe {
                        std::slice::from_raw_parts_mut(
                            storage.as_mut_ptr().cast::<u8>(),
                            storage.len() * 4,
                        )
                    };
                    let start = 4 + offset;
                    let length = 32 * encoding.width() + encoding.width() - 1;
                    let complete = 32 * encoding.width();
                    let mut blocks = vec![];
                    for stage in 0..3 {
                        bytes.fill(0xcc);
                        if stage == 1 {
                            allowed.store(false, Ordering::SeqCst);
                        }
                        if stage == 2 {
                            allowed.store(true, Ordering::SeqCst);
                            owner.resume().unwrap();
                        }
                        assert_eq!(
                            output.read(&mut owner, &mut bytes[start..start + length]),
                            complete
                        );
                        assert!(bytes[..start].iter().all(|x| *x == 0xcc));
                        assert!(bytes[start + complete..].iter().all(|x| *x == 0xcc));
                        if stage == 0 {
                            assert!(bytes[start..start + complete].iter().any(|x| *x != 0));
                        } else {
                            assert!(bytes[start..start + complete].iter().all(|x| *x == 0));
                        }
                        let status = owner.snapshot();
                        if stage == 1 {
                            assert_eq!(
                                (status.state, status.capture_queued, status.render_queued),
                                ("suspended", 0, 0)
                            );
                        }
                        if stage == 2 {
                            assert_eq!((status.state, status.generation), ("open", 2));
                        }
                        blocks.push(json!({"stage":stage,"hex":hex(&bytes[start..start+length]),"state":status.state,"generation":status.generation,"captureQueued":status.capture_queued,"renderQueued":status.render_queued}));
                    }
                    rows.push(json!({"rate":rate,"channels":channels,"encoding":format!("{encoding:?}"),"offset":offset,"blocks":blocks}));
                }
            }
        }
    }
    assert_eq!(rows.len(), 36);
    for channels in [0, 3, u16::MAX] {
        assert!(matches!(
            RenderBytes::new(48000, channels, Encoding::Pcm16),
            Err(CallAudioError::InvalidRequest)
        ));
    }
    assert!(matches!(
        RenderBytes::new(0, 1, Encoding::Float32),
        Err(CallAudioError::InvalidRequest)
    ));
    save("render-format-composition", &json!(rows));
}
