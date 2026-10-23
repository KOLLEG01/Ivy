use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn guid(value: u128) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        value >> 96,
        (value >> 80) & 0xffff,
        (value >> 64) & 0xffff,
        (value >> 48) & 0xffff,
        value & 0xffffffffffff
    )
}
fn mix(tag: u16, bits: u16, subtype: u128, samples: u16) -> Vec<u8> {
    let extra = match tag {
        65534 => 22,
        2 => 32,
        49 => 2,
        _ => 0,
    };
    let mut bytes = vec![0; 18 + extra];
    bytes[..2].copy_from_slice(&tag.to_le_bytes());
    bytes[2..4].copy_from_slice(&2_u16.to_le_bytes());
    bytes[4..8].copy_from_slice(&48000_u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&768000_u32.to_le_bytes());
    bytes[12..14].copy_from_slice(&16_u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&bits.to_le_bytes());
    bytes[16..18].copy_from_slice(&(extra as u16).to_le_bytes());
    if tag == 65534 {
        bytes[18..20].copy_from_slice(&bits.to_le_bytes());
        bytes[20..24].copy_from_slice(&3_u32.to_le_bytes());
        bytes[24..28].copy_from_slice(&((subtype >> 96) as u32).to_le_bytes());
        bytes[28..30].copy_from_slice(&((subtype >> 80) as u16).to_le_bytes());
        bytes[30..32].copy_from_slice(&((subtype >> 64) as u16).to_le_bytes());
        bytes[32..40].copy_from_slice(&(subtype as u64).to_be_bytes());
    }
    if tag == 2 {
        bytes[18..20].copy_from_slice(&samples.to_le_bytes());
        bytes[20..22].copy_from_slice(&7_u16.to_le_bytes());
    }
    bytes
}
fn info(mix: Vec<u8>, unavailable: Option<Unavailable>) -> Info {
    Info {
        rate: 48000,
        channels: 2,
        latency_ms: 37,
        low_latency: false,
        unavailable,
        mix,
    }
}
fn save(name: &str, bytes: &[u8]) {
    if let Some(root) = std::env::var_os("IVY_PHONE_RENDER_STATUS_TRACE") {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(std::path::Path::new(&root).join(name), bytes).unwrap();
    }
}
#[test]
fn published_status_names_descriptions_and_fallback_reasons_agree() {
    let subtypes = names::SUBTYPES
        .iter()
        .map(|x| x.0)
        .chain([
            0,
            u128::MAX,
            0x123456789abcdef01032547698badcfe,
            0x0000000100000010800000aa00389b70,
        ])
        .collect::<Vec<_>>();
    let mut cases = names::ENCODINGS
        .iter()
        .map(|x| (format!("tag-{:04x}", x.0), mix(x.0, 64, 0, 1012)))
        .collect::<Vec<_>>();
    cases.extend(
        [0x000b, 0x0123, 0x8001]
            .into_iter()
            .map(|tag| (format!("unknown-{tag:04x}"), mix(tag, 64, 0, 1012))),
    );
    cases.extend(
        subtypes
            .iter()
            .map(|value| (format!("ext-{}", guid(*value)), mix(65534, 64, *value, 0))),
    );
    for bits in [0, 32768, 65535] {
        for tag in [1, 3, 65534, 2] {
            cases.push((
                format!("signed-bits-{tag}-{bits}"),
                mix(tag, bits, subtypes[0], 65535),
            ));
        }
    }
    let input = json!({"subtypes":subtypes.iter().map(|x|guid(*x)).collect::<Vec<_>>(),"cases":cases.iter().map(|(name,bytes)|json!({"name":name,"hex":bytes.iter().map(|x|format!("{x:02x}")).collect::<String>()})).collect::<Vec<_>>()});
    let mut actual = json!({
        "names":(0..=u16::MAX).map(names::encoding).collect::<Vec<_>>(),
        "subtypes":subtypes.iter().map(|value|json!({"guid":guid(*value),"name":names::subtype(*value)})).collect::<Vec<_>>(),
        "cases":cases.iter().map(|(name,bytes)|{let status=Status::new(&info(bytes.clone(),Some(Unavailable::Format))).unwrap();assert_eq!((status.latency_ms,status.low_latency),(37,false));json!({"name":name,"description":description(bytes).unwrap(),"reason":status.unavailable,"noNativeResources":true})}).collect::<Vec<_>>(),
        "literals":{"client3":Status::new(&info(vec![],Some(Unavailable::Client3))).unwrap().unavailable,"engine":Status::new(&info(vec![],Some(Unavailable::Engine))).unwrap().unavailable}
    });
    let data: Value =
        serde_json::from_str(include_str!("../fixtures/phone-windows-audio-cases.json")).unwrap();
    assert_eq!(data["status"]["cases"], input);
    let original = data["status"]["expected"].clone();
    let names = serde_json::to_vec(&actual["names"]).unwrap();
    let sha256: String = Sha256::digest(&names)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    actual["names"] = json!({"count":actual["names"].as_array().unwrap().len(),"sha256":sha256});
    save("status-input.json", &serde_json::to_vec(&input).unwrap());
    save("status-rust.json", &serde_json::to_vec(&actual).unwrap());
    for field in ["subtypes", "cases"] {
        let new = actual[field].as_array().unwrap();
        let old = original[field].as_array().unwrap();
        assert_eq!(new.len(), old.len(), "{field}");
        for (index, (new, old)) in new.iter().zip(old).enumerate() {
            assert_eq!(new, old, "{field}[{index}]");
        }
    }
    assert_eq!(actual, original);
    assert_eq!(
        Status::new(&info(vec![], None)).unwrap(),
        Status {
            latency_ms: 37,
            low_latency: false,
            unavailable: None
        }
    );
    let mut active = info(vec![], None);
    active.low_latency = true;
    assert!(Status::new(&active).unwrap().low_latency);
    for bytes in [
        vec![],
        vec![0; 17],
        mix(65534, 64, 0, 0)[..39].to_vec(),
        mix(2, 64, 0, 0)[..19].to_vec(),
    ] {
        assert_eq!(
            Status::new(&info(bytes, Some(Unavailable::Format))),
            Err(RenderError::Configuration)
        );
    }
    save("status-summary.json",&serde_json::to_vec(&json!({"names":65536,"subtypes":subtypes.len(),"descriptors":cases.len(),"metadata":2,"shortDescriptorRefusals":4,"nativeConstructorsExecuted":false,"client3EngineLiteralSource":"published compiled IL"})).unwrap());
}
