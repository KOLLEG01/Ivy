use ivy_contracts::{
    JsString,
    json::parse,
    node_buffer::{base64url_decode, base64url_text, canonical_base64_decode},
};
use std::sync::OnceLock;

fn oracle() -> &'static serde_json::Value {
    static VALUE: OnceLock<serde_json::Value> = OnceLock::new();
    VALUE.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let output = std::process::Command::new("node")
            .arg("tests/fixtures/node-buffer-oracle.mjs")
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    })
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn check(input: &JsString, expected: &serde_json::Value) {
    assert_eq!(
        hex(&base64url_decode(input)),
        expected["bytes"].as_str().unwrap(),
        "bytes {input:?}"
    );
    assert_eq!(
        base64url_text(input),
        JsString::from(expected["text"].as_str().unwrap()),
        "text {input:?}"
    );
}
#[test]
fn every_utf16_unit_matches_node_permissive_base64_classes() {
    let fixture = oracle();
    assert_eq!(fixture["unitComparisons"], 327680);
    for group in fixture["units"].as_array().unwrap() {
        let context = group["context"].as_str().unwrap();
        for unit in 0..=u16::MAX {
            let mut input = JsString::from(context);
            let index = input.0.iter().position(|unit| *unit == 88).unwrap();
            input.0[index] = unit;
            check(&input, &group["expected"][usize::from(unit & 255)]);
        }
    }
}
#[test]
fn every_one_two_byte_value_and_utf8_replacement_boundary_matches_node() {
    let rows = oracle()["utf8"].as_array().unwrap();
    assert_eq!(rows.len(), 65817);
    let mut index = 0;
    for first in 0..=u8::MAX {
        let row = &rows[index];
        index += 1;
        assert_eq!(
            base64url_decode(&JsString::from(row[0].as_str().unwrap())),
            [first]
        );
        assert_eq!(
            base64url_text(&JsString::from(row[0].as_str().unwrap())),
            JsString::from(row[1].as_str().unwrap())
        );
        for second in 0..=u8::MAX {
            let row = &rows[index];
            index += 1;
            assert_eq!(
                base64url_decode(&JsString::from(row[0].as_str().unwrap())),
                [first, second]
            );
            assert_eq!(
                base64url_text(&JsString::from(row[0].as_str().unwrap())),
                JsString::from(row[1].as_str().unwrap())
            );
        }
    }
    for row in &rows[index..] {
        assert_eq!(
            base64url_text(&JsString::from(row[0].as_str().unwrap())),
            JsString::from(row[1].as_str().unwrap())
        );
    }
}
#[test]
fn malformed_padding_alphabets_and_seeded_cursor_text_match_node() {
    let rows = oracle()["malformed"].as_array().unwrap();
    assert!(rows.len() > 20_000);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), usize::MAX).unwrap();
        check(input.as_js_string().unwrap(), row);
    }
}

#[test]
fn canonical_standard_base64_and_original_eight_mib_bound_match_node() {
    let fixture = oracle();
    let check = |input: &JsString, expected: &serde_json::Value| {
        let actual = canonical_base64_decode(input, 8 * 1024 * 1024).map(|bytes| hex(&bytes));
        assert_eq!(actual.as_deref(), expected.as_str(), "canonical {input:?}");
    };
    let rows = fixture["canonicalRows"].as_array().unwrap();
    assert!(rows.len() > 80_000);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), usize::MAX).unwrap();
        check(input.as_js_string().unwrap(), &row["expected"]);
    }
    for group in fixture["canonicalUnits"].as_array().unwrap() {
        let context = group["context"].as_str().unwrap();
        for unit in 0..=u16::MAX {
            let mut input = JsString::from(context);
            let index = input.0.iter().position(|unit| *unit == 88).unwrap();
            input.0[index] = unit;
            check(
                &input,
                if unit <= 255 {
                    &group["expected"][usize::from(unit)]
                } else {
                    &serde_json::Value::Null
                },
            );
        }
    }
    for row in fixture["limits"].as_array().unwrap() {
        let length = row["length"].as_u64().unwrap() as usize;
        let mut encoded = "AAAA".repeat(length / 3);
        encoded.push_str(match length % 3 {
            0 => "",
            1 => "AA==",
            _ => "AAA=",
        });
        let actual = canonical_base64_decode(&encoded.into(), 8 * 1024 * 1024);
        assert_eq!(
            actual.is_some(),
            row["accepted"].as_bool().unwrap(),
            "length={length}"
        );
        if let Some(bytes) = actual {
            assert_eq!(bytes.len(), length);
            assert!(bytes.iter().all(|byte| *byte == 0));
        }
    }
    assert_eq!(
        canonical_base64_decode(&JsString::from(""), 0),
        Some(Vec::new())
    );
    assert!(canonical_base64_decode(&JsString::from("AA=="), 0).is_none());
}
