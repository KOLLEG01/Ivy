use aws_lc_rs::agreement;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_hive::{
    browser_push::{Sender, VapidDetails},
    browser_push_native::{self as native, Entropy, Https, NativeCrypto, NativeSender, PostFuture},
    browser_push_store::Crypto,
};
use serde_json::{Value, json};
use std::{
    collections::{HashSet, VecDeque},
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

// Published encryption example: https://www.rfc-editor.org/rfc/rfc8291#section-5
const RECEIVER_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const RECEIVER_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
const EXPIRATION: u64 = 1_700_043_200;

struct RfcEntropy;
impl Entropy for RfcEntropy {
    fn encryption_key(&self) -> IvyResult<agreement::PrivateKey> {
        agreement::PrivateKey::from_private_key(
            &agreement::ECDH_P256,
            &URL_SAFE_NO_PAD
                .decode("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")
                .unwrap(),
        )
        .map_err(|_| IvyError::internal())
    }
    fn salt(&self) -> IvyResult<[u8; 16]> {
        Ok(URL_SAFE_NO_PAD
            .decode("DGv6ra1nlYgDCS1FRnbzlw")
            .unwrap()
            .try_into()
            .unwrap())
    }
}
fn ivy(value: &Value) -> IvyJson {
    IvyJson::from_serde(value).unwrap()
}
fn subscription() -> Value {
    json!({"endpoint":"https://push.example.test:8443/send/test?q=%2F",
        "keys":{"p256dh":RECEIVER_PUBLIC,"auth":AUTH}})
}
fn details() -> Value {
    let mut keys = NativeCrypto
        .generate_vapid_keys()
        .unwrap()
        .to_serde()
        .unwrap();
    keys["subject"] = json!("mailto:push@example.test");
    keys
}
fn vapid(value: &Value) -> VapidDetails {
    VapidDetails::from_keys(&ivy(value), &value["subject"].as_str().unwrap().into()).unwrap()
}
fn raw_carrier(request: native::Request) -> Value {
    let headers = request
        .headers
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect::<serde_json::Map<_, _>>();
    json!({"endpoint":request.endpoint,"headers":headers,"body":STANDARD.encode(request.body)})
}
fn verify(requests: &[Value]) -> Vec<Value> {
    let input = json!({"receiverPrivate":RECEIVER_PRIVATE,"auth":AUTH,"requests":requests});
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tests/fixtures/hive-push-verify.mjs"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let checked: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(checked.len(), requests.len());
    checked
}
fn assert_verified(value: &Value, payload: &str, subject: &str) {
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(value["plaintext"], STANDARD.encode(payload));
    assert_eq!(
        value["claims"],
        json!({"aud":"https://push.example.test:8443",
        "exp":EXPIRATION,"sub":subject})
    );
}

#[test]
fn rfc8291_encryption_vector_and_advertised_vapid_key_verify() {
    let payload = "When I grow up, I want to be a watermelon";
    let request = native::request(
        &ivy(&subscription()),
        payload,
        &vapid(&details()),
        &|| Ok(EXPIRATION),
        &RfcEntropy,
    )
    .unwrap();
    let expected = concat!(
        "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml",
        "mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT",
        "pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
    );
    assert_eq!(URL_SAFE_NO_PAD.encode(&request.body), expected);
    let raw = raw_carrier(request);
    let mut corrupt_body = raw.clone();
    let mut body = STANDARD.decode(raw["body"].as_str().unwrap()).unwrap();
    *body.last_mut().unwrap() ^= 1;
    corrupt_body["body"] = json!(STANDARD.encode(body));
    let mut corrupt_signature = raw.clone();
    let auth = raw["headers"]["Authorization"].as_str().unwrap();
    let (token, public) = auth
        .strip_prefix("vapid t=")
        .unwrap()
        .split_once(", k=")
        .unwrap();
    let (message, signature) = token.rsplit_once('.').unwrap();
    let mut signature = URL_SAFE_NO_PAD.decode(signature).unwrap();
    signature[0] ^= 1;
    corrupt_signature["headers"]["Authorization"] = json!(format!(
        "vapid t={message}.{}, k={public}",
        URL_SAFE_NO_PAD.encode(signature)
    ));
    let checked = verify(&[raw, corrupt_body, corrupt_signature]);
    assert_verified(&checked[0], payload, "mailto:push@example.test");
    assert_eq!(checked[1]["ok"], false);
    assert_eq!(checked[2]["ok"], false);
}

#[test]
fn generated_keys_and_record_boundaries_decrypt_independently() {
    let payloads = [
        String::new(),
        "Ivy 🐝\0 ä".into(),
        "a".repeat(4078),
        "b".repeat(4079),
        "c".repeat(4080),
        "d".repeat(8158),
        "e".repeat(8159),
    ];
    let raw = payloads
        .iter()
        .map(|payload| {
            let mut keys = details();
            keys["subject"] = json!("https://example.test/contact?language=de");
            raw_carrier(
                native::request(
                    &ivy(&subscription()),
                    payload,
                    &vapid(&keys),
                    &|| Ok(EXPIRATION),
                    &native::SystemEntropy,
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    for (checked, payload) in verify(&raw).iter().zip(&payloads) {
        assert_verified(checked, payload, "https://example.test/contact?language=de");
    }
    // An empty notification needs no encryption keys and emits no content coding.
    assert!(
        native::request(
            &ivy(&json!({"endpoint":subscription()["endpoint"]})),
            "",
            &vapid(&details()),
            &|| Ok(EXPIRATION),
            &RfcEntropy
        )
        .unwrap()
        .body
        .is_empty()
    );
}

fn hex_scalar(text: &str) -> [u8; 32] {
    std::array::from_fn(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap())
}
#[test]
fn valid_scalar_boundaries_use_fresh_verifiable_signatures() {
    let mut one = [0; 32];
    one[31] = 1;
    let upper = hex_scalar("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632550");
    let mut requests = Vec::new();
    let mut signatures = HashSet::new();
    for scalar in [one, upper] {
        let key = agreement::PrivateKey::from_private_key(&agreement::ECDH_P256, &scalar).unwrap();
        let keys = json!({"privateKey":URL_SAFE_NO_PAD.encode(scalar),
            "publicKey":URL_SAFE_NO_PAD.encode(key.compute_public_key().unwrap().as_ref()),
            "subject":"mailto:push@example.test"});
        for _ in 0..4 {
            let raw = raw_carrier(
                native::request(
                    &ivy(&subscription()),
                    "fresh signature",
                    &vapid(&keys),
                    &|| Ok(EXPIRATION),
                    &RfcEntropy,
                )
                .unwrap(),
            );
            assert!(
                signatures.insert(raw["headers"]["Authorization"].as_str().unwrap().to_owned())
            );
            requests.push(raw);
        }
    }
    for checked in verify(&requests) {
        assert_verified(&checked, "fresh signature", "mailto:push@example.test");
    }
}

struct Responses {
    statuses: Mutex<VecDeque<IvyResult<u16>>>,
    requests: Mutex<Vec<Value>>,
}
impl Https for Responses {
    fn post(&self, request: native::Request) -> PostFuture {
        self.requests.lock().unwrap().push(raw_carrier(request));
        let reply = self.statuses.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { reply })
    }
}
#[tokio::test]
async fn native_sender_preserves_status_and_pretransport_rejections() {
    let details = details();
    let expected = [
        199u16, 200, 201, 204, 299, 300, 302, 400, 404, 410, 429, 500,
    ];
    let https = Arc::new(Responses {
        statuses: Mutex::new(
            expected
                .into_iter()
                .map(Ok)
                .chain([Err(IvyError::internal())])
                .collect(),
        ),
        requests: Mutex::new(Vec::new()),
    });
    let sender = NativeSender::new(https.clone(), Arc::new(|| Ok(EXPIRATION)));
    for status in expected {
        assert_eq!(
            sender
                .send(ivy(&subscription()), "notification".into(), vapid(&details))
                .await
                .unwrap(),
            status
        );
    }
    assert!(
        sender
            .send(ivy(&subscription()), "notification".into(), vapid(&details))
            .await
            .is_err()
    );
    let before = https.requests.lock().unwrap().len();
    let mut invalid_details = Vec::new();
    for scalar in [
        [0; 32],
        hex_scalar("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551"),
        hex_scalar("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632552"),
        [255; 32],
    ] {
        let mut invalid = details.clone();
        invalid["privateKey"] = json!(URL_SAFE_NO_PAD.encode(scalar));
        invalid_details.push(invalid);
    }
    for (key, value) in [
        ("publicKey", json!(URL_SAFE_NO_PAD.encode([0; 65]))),
        ("publicKey", json!("AA=")),
        ("publicKey", self::details()["publicKey"].clone()),
        ("privateKey", json!(URL_SAFE_NO_PAD.encode([1; 31]))),
        ("subject", json!("http://example.test")),
        ("subject", json!("invalid")),
    ] {
        let mut invalid = details.clone();
        invalid[key] = value;
        invalid_details.push(invalid);
    }
    for invalid in invalid_details {
        assert!(
            sender
                .send(ivy(&subscription()), "notification".into(), vapid(&invalid))
                .await
                .is_err()
        );
    }
    for endpoint in [
        "",
        "/relative",
        "http://example.test/send",
        "ftp://example.test/send",
    ] {
        let mut invalid = subscription();
        invalid["endpoint"] = json!(endpoint);
        assert!(
            sender
                .send(ivy(&invalid), "notification".into(), vapid(&details))
                .await
                .is_err()
        );
    }
    for (key, value) in [
        ("p256dh", URL_SAFE_NO_PAD.encode([4; 65])),
        ("auth", "AA".into()),
    ] {
        let mut invalid = subscription();
        invalid["keys"][key] = json!(value);
        assert!(
            sender
                .send(ivy(&invalid), "notification".into(), vapid(&details))
                .await
                .is_err()
        );
    }
    assert_eq!(https.requests.lock().unwrap().len(), before);
    let raw = https.requests.lock().unwrap().clone();
    let mut bodies = HashSet::new();
    for item in &raw {
        assert!(bodies.insert(item["body"].as_str().unwrap()));
    }
    for checked in verify(&raw) {
        assert_verified(&checked, "notification", "mailto:push@example.test");
    }
}

#[test]
fn expiration_uses_twelve_elapsed_hours_in_utc() {
    for seconds in [0, 1, 1_700_000_000, 1_774_722_600, 1_792_866_600] {
        let now = UNIX_EPOCH + Duration::new(seconds, 999_999_999);
        assert_eq!(native::expiration_at(now).unwrap(), seconds + 43_200);
    }
    assert!(native::expiration_at(UNIX_EPOCH - Duration::from_millis(1)).is_err());
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let expiration = native::system_expiration()().unwrap();
    let after = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!((before + 43_200..=after + 43_200).contains(&expiration));
}
