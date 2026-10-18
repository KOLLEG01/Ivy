use ivy_contracts::hash::{hmac_sha256, verify_hmac_sha256};

fn bytes(value: &serde_json::Value) -> Vec<u8> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| u8::try_from(v.as_u64().unwrap()).unwrap())
        .collect()
}

#[test]
fn hmac_bytes_and_full_tag_verification_match_node() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/hash-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 149);
    for (i, row) in rows.iter().enumerate() {
        let key = bytes(&row["key"]);
        let message = bytes(&row["message"]);
        assert_eq!(
            hmac_sha256(&key, &message).as_slice(),
            bytes(&row["signature"]),
            "sign case {i}"
        );
        for check in row["verification"].as_array().unwrap() {
            assert_eq!(
                verify_hmac_sha256(&key, &message, &bytes(&check["signature"])),
                check["valid"].as_bool().unwrap(),
                "verify case {i}"
            );
        }
        let mut changed_message = message.clone();
        changed_message.push(0);
        assert!(!verify_hmac_sha256(
            &key,
            &changed_message,
            &bytes(&row["signature"])
        ));
    }
}

#[test]
fn persisted_hex_looking_secret_is_text_not_decoded_bytes() {
    let secret = "0123456789abcdef".repeat(4);
    let decoded: Vec<u8> = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef].repeat(4);
    let body = b"eyJib3VuZGFyeSI6MX0";
    let tag = hmac_sha256(secret.as_bytes(), body);
    assert_ne!(tag, hmac_sha256(&decoded, body));
    assert!(verify_hmac_sha256(secret.as_bytes(), body, &tag));
    assert!(!verify_hmac_sha256(&decoded, body, &tag));
}
