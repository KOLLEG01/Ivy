use super::*;
use aws_lc_rs::agreement;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
static NATIVE_CORPUS: Mutex<()> = Mutex::new(());
use ivy_hive::browser_push_native::{
    self as native, Entropy, Https, NativeCrypto, NativeSender, PostFuture,
};

struct Fixed;
impl Entropy for Fixed {
    fn encryption_key(&self) -> IvyResult<agreement::PrivateKey> {
        let mut scalar = [0; 32];
        scalar[31] = 2;
        agreement::PrivateKey::from_private_key(&agreement::ECDH_P256, &scalar)
            .map_err(|_| IvyError::internal())
    }
    fn salt(&self) -> IvyResult<[u8; 16]> {
        Ok(std::array::from_fn(|i| i as u8))
    }
}
fn oracle(input: &Value) -> Value {
    super::original_output("tests/fixtures/hive-push-crypto-oracle.mjs", input)
}
fn ivy(value: &Value) -> IvyJson {
    IvyJson::from_serde(value).unwrap()
}
fn raw_carrier(request: native::Request) -> Value {
    let headers = request
        .headers
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect::<serde_json::Map<_, _>>();
    json!({"endpoint":request.endpoint,"headers":headers,"body":STANDARD.encode(request.body)})
}
fn save(input: &Value, original: &Value, actual: &Value) {
    if let Some(path) = std::env::var_os("IVY_HIVE_PUSH_CRYPTO_CORPUS") {
        use std::fs::OpenOptions;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({"input":input,"original":original,"native":actual})
        )
        .unwrap();
    }
}
#[test]
fn native_ece_and_vapid_match_the_original_on_record_boundaries_and_invalid_keys() {
    let _serial = NATIVE_CORPUS.lock().unwrap();
    let cases = vec![
        json!({"payload":""}),
        json!({"payload":"Ivy 🐝\u{0000} ä"}),
        json!({"payload":"a".repeat(4078)}),
        json!({"payload":"b".repeat(4079)}),
        json!({"payload":"c".repeat(4080)}),
        json!({"payload":"d".repeat(8158)}),
        json!({"payload":"e".repeat(8159)}),
        json!({"payload":"x","subscription":{"keys":{"auth":"AA"}}}),
        json!({"payload":"x","subscription":{"keys":{"p256dh":URL_SAFE_NO_PAD.encode([4u8;65])}}}),
        json!({"payload":"x","details":{"subject":"http://example.test"}}),
        json!({"payload":"x","details":{"subject":"https://example.test/a?b"}}),
        json!({"payload":"x","details":{"publicKey":"AA="}}),
        json!({"payload":"x","details":{"publicKey":URL_SAFE_NO_PAD.encode([0u8;65])}}),
        json!({"payload":"","subscription":{"keys":{"p256dh":"invalid","auth":"invalid"}}}),
    ];
    let input = json!({"cases":cases});
    let source = oracle(&input);
    let mut actual = Vec::new();
    let mut raw = Vec::new();
    for case in input["cases"].as_array().unwrap() {
        let mut sub = source["subscription"].clone();
        if let Some(keys) = case["subscription"]["keys"].as_object() {
            for (k, v) in keys {
                sub["keys"][k] = v.clone();
            }
        }
        let mut details = source["signer"].clone();
        details["subject"] = json!("mailto:push@example.test");
        if let Some(fields) = case["details"].as_object() {
            for (k, v) in fields {
                details[k] = v.clone();
            }
        }
        let clock_reads = std::sync::atomic::AtomicUsize::new(0);
        let expiration = || {
            clock_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(source["expiration"].as_u64().unwrap())
        };
        match native::request(
            &ivy(&sub),
            case["payload"].as_str().unwrap(),
            &ivy(&details),
            &expiration,
            &Fixed,
        ) {
            Ok(request) => {
                raw.push(raw_carrier(request));
                actual.push(json!({"ok":true,"index":raw.len()-1,"clockReads":clock_reads.load(std::sync::atomic::Ordering::SeqCst)}));
            }
            Err(_) => actual.push(json!({"ok":false,"clockReads":clock_reads.load(std::sync::atomic::Ordering::SeqCst)})),
        }
    }
    let verified = oracle(&json!({"cases":[],"native":raw}));
    for (i, value) in actual.iter_mut().enumerate() {
        if value["ok"] == true {
            let checked = &verified["native"][value["index"].as_u64().unwrap() as usize];
            assert_eq!(checked["ok"], true);
            assert_eq!(checked["signatureValid"], true);
            assert_eq!(
                checked["plaintext"],
                STANDARD.encode(input["cases"][i]["payload"].as_str().unwrap())
            );
            let reads = value["clockReads"].clone();
            *value = checked.clone();
            value["clockReads"] = reads;
            value.as_object_mut().unwrap().remove("plaintext");
        }
    }
    let expected = source["requests"].clone();
    let result = json!(actual);
    save(&input, &expected, &result);
    assert_eq!(result, expected);
}
#[test]
fn native_generated_vapid_and_subscription_points_are_accepted_by_node() {
    let _serial = NATIVE_CORPUS.lock().unwrap();
    let profile = oracle(&json!({"cases":[]}));
    let key = URL_SAFE_NO_PAD
        .decode(profile["receiver"]["publicKey"].as_str().unwrap())
        .unwrap();
    let mut bad = key.clone();
    bad[10] ^= 1;
    let mut hybrid = key.clone();
    hybrid[0] = 6 | (hybrid[64] & 1);
    let mut compressed = vec![2 | (key[64] & 1)];
    compressed.extend_from_slice(&key[1..33]);
    let vectors = vec![key, hybrid, compressed, bad, vec![], vec![0], vec![4; 65]];
    let generated = (0..8)
        .map(|_| {
            NativeCrypto
                .generate_vapid_keys()
                .unwrap()
                .to_serde()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let input = json!({"op":"validate","keys":generated,"points":vectors.iter().map(|v|URL_SAFE_NO_PAD.encode(v)).collect::<Vec<_>>()});
    let original = oracle(&input);
    let points = vectors
        .iter()
        .map(|v| ivy_hive::browser_push_store::push_subscription(
            &ivy(&json!({"endpoint":"https://fcm.googleapis.com/send/test","keys":{"p256dh":URL_SAFE_NO_PAD.encode(v),"auth":URL_SAFE_NO_PAD.encode([7u8;16])}})),
            &NativeCrypto,
        ).is_ok())
        .collect::<Vec<_>>();
    let actual = json!({"generated":vec![true;8],"points":points});
    save(&input, &original, &actual);
    assert_eq!(actual, original);
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
    let profile = oracle(&json!({"cases":[]}));
    let mut details = profile["signer"].clone();
    details["subject"] = json!("mailto:push@example.test");
    let options = ivy(&json!({"vapidDetails":details,"TTL":3600,"timeout":10000}));
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
    let sender = NativeSender::new(https.clone(), Arc::new(|| Ok(1700043200)));
    for status in expected {
        match sender
            .send(
                ivy(&profile["subscription"]),
                "notification".into(),
                options.clone(),
            )
            .await
        {
            SendOutcome::Response(Some(IvyJson::Number(v))) => {
                assert!((200..=299).contains(&status));
                assert_eq!(v, status as f64);
            }
            SendOutcome::Rejected(Some(IvyJson::Number(v))) => {
                assert!(!(200..=299).contains(&status));
                assert_eq!(v, status as f64);
            }
            _ => panic!("Unexpected native status"),
        }
    }
    assert!(matches!(
        sender
            .send(
                ivy(&profile["subscription"]),
                "notification".into(),
                options.clone()
            )
            .await,
        SendOutcome::Rejected(None)
    ));
    let before = https.requests.lock().unwrap().len();
    let mut invalid = options.clone();
    invalid.insert("vapidDetails", IvyJson::Null);
    assert!(matches!(
        sender
            .send(
                ivy(&profile["subscription"]),
                "notification".into(),
                invalid
            )
            .await,
        SendOutcome::Rejected(None)
    ));
    assert_eq!(https.requests.lock().unwrap().len(), before);
    // System entropy is used by the production sender; the original decrypts it.
    let raw = https.requests.lock().unwrap().clone();
    let verified = oracle(&json!({"cases":[],"native":raw}));
    for item in verified["native"].as_array().unwrap() {
        assert_eq!(item["ok"], true);
        assert_eq!(item["plaintext"], STANDARD.encode("notification"));
        assert_eq!(item["signatureValid"], true);
    }
}

#[test]
fn persisted_scalar_boundaries_preserve_original_signing_and_fresh_nonces() {
    let _serial = NATIVE_CORPUS.lock().unwrap();
    let scalars = [
        [0u8; 32],
        {
            let mut v = [0; 32];
            v[31] = 1;
            v
        },
        hex_scalar("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632550"),
        hex_scalar("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551"),
        hex_scalar("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632552"),
        [255u8; 32],
    ];
    let cases = scalars
        .iter()
        .flat_map(|scalar| {
            (0..4).map(move |_| json!({
        "payload":"scalar boundary", "details":{"privateKey":URL_SAFE_NO_PAD.encode(scalar)}
    }))
        })
        .collect::<Vec<_>>();
    let input = json!({"cases":cases});
    let source = oracle(&input);
    let mut raw = Vec::new();
    let mut signatures = std::collections::HashSet::new();
    for (index, case) in input["cases"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            source["requests"][index]["ok"], true,
            "original scalar {index}"
        );
        assert_eq!(source["requests"][index]["signatureValid"], true);
        let mut details = source["signer"].clone();
        details["subject"] = json!("mailto:push@example.test");
        details["privateKey"] = case["details"]["privateKey"].clone();
        let request = native::request(
            &ivy(&source["subscription"]),
            "scalar boundary",
            &ivy(&details),
            &|| Ok(1700043200),
            &Fixed,
        )
        .unwrap();
        let mut carrier = raw_carrier(request);
        let signature = carrier["headers"]["Authorization"]
            .as_str()
            .unwrap()
            .split(", k=")
            .next()
            .unwrap()
            .rsplit('.')
            .next()
            .unwrap()
            .to_string();
        assert!(signatures.insert(signature), "nonce/signature reused");
        carrier["signingKey"] = details;
        raw.push(carrier);
    }
    let checked = oracle(&json!({"cases":[],"native":raw}));
    let mut actual = checked["native"].clone();
    for item in actual.as_array_mut().unwrap() {
        assert_eq!(item["signatureValid"], true);
        assert_eq!(item["plaintext"], STANDARD.encode("scalar boundary"));
        item.as_object_mut().unwrap().remove("plaintext");
        item["clockReads"] = json!(1);
    }
    save(
        &json!({"cases":cases,"raw":raw}),
        &source["requests"],
        &actual,
    );
    assert_eq!(actual, source["requests"]);
}

fn hex_scalar(text: &str) -> [u8; 32] {
    std::array::from_fn(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap())
}

struct TestClock {
    now: std::sync::atomic::AtomicI64,
    reads: std::sync::atomic::AtomicUsize,
}
impl ivy_sdk::clock::Clock for TestClock {
    fn utc_millis(&self) -> i64 {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.now.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}

#[test]
fn local_clock_child() {
    let Ok(zone) = std::env::var("IVY_HIVE_PUSH_CLOCK_ZONE") else {
        return;
    };
    let times: [i64; 20] = [
        -8640000000000001,
        -8640000000000000,
        -2208988800001,
        -43200001,
        -1,
        0,
        1001,
        1325226600000,
        1700000000000,
        1772303400000,
        1772908200000,
        1774722600000,
        1775331000000,
        1792866600000,
        1793471400000,
        8639999956799999,
        8639999999999999,
        8640000000000000,
        8640000000000001,
        i64::MAX,
    ];
    let cases = times
        .iter()
        .map(|now| json!({"now":now,"payload":"clock"}))
        .collect::<Vec<_>>();
    let source = oracle(&json!({"cases":cases,"timezone":zone}));
    let mut details = source["signer"].clone();
    details["subject"] = json!("mailto:push@example.test");
    let clock = Arc::new(TestClock {
        now: 0.into(),
        reads: 0.into(),
    });
    let expiration = native::local_expiration(clock.clone());
    let mut raw = Vec::new();
    for (index, now) in times.iter().enumerate() {
        clock.now.store(*now, std::sync::atomic::Ordering::SeqCst);
        let request = native::request_with_numeric_expiration(
            &ivy(&source["subscription"]),
            "clock",
            &ivy(&details),
            expiration.as_ref(),
            &Fixed,
        )
        .unwrap();
        assert_eq!(
            clock.reads.load(std::sync::atomic::Ordering::SeqCst),
            index + 1
        );
        raw.push(raw_carrier(request));
    }
    let checked = oracle(&json!({"cases":[],"native":raw}));
    let mut actual = checked["native"].clone();
    for item in actual.as_array_mut().unwrap() {
        assert_eq!(item["signatureValid"], true);
        item.as_object_mut().unwrap().remove("plaintext");
        item["clockReads"] = json!(1);
    }
    save(
        &json!({"timezone":zone,"cases":cases}),
        &source["requests"],
        &actual,
    );
    assert_eq!(actual, source["requests"], "local clock in {zone}");
}

#[test]
fn actual_local_clock_matches_original_across_zones_dst_and_timeclip() {
    for zone in [
        "UTC",
        "Europe/Berlin",
        "America/New_York",
        "Australia/Lord_Howe",
        "Pacific/Apia",
        "Asia/Kathmandu",
    ] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_crypto::local_clock_child", "--nocapture"])
            .env("IVY_HIVE_PUSH_CLOCK_ZONE", zone)
            .env("TZ", zone)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "TZ={zone}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn warning_child() {
    let Ok(subject) = std::env::var("IVY_HIVE_PUSH_WARNING_SUBJECT") else {
        return;
    };
    let source = oracle(&json!({"cases":[]}));
    let mut details = source["signer"].clone();
    details["subject"] = json!(subject);
    details["publicKey"] = json!("AA=");
    let clock_reads = std::sync::atomic::AtomicUsize::new(0);
    let expiration = || {
        clock_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(1700043200)
    };
    assert!(
        native::request(
            &ivy(&source["subscription"]),
            "warning",
            &ivy(&details),
            &expiration,
            &Fixed
        )
        .is_err()
    );
    assert_eq!(clock_reads.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn localhost_subject_warning_precedes_invalid_key_failure() {
    let warning = "Vapid subject points to a localhost web URI, which is unsupported by Apple's push notification server and will result in a BadJwtToken error when sending notifications.";
    let subjects = [
        "https://localhost/path",
        "https://LOCALHOST:8443/path",
        "mailto:localhost",
        "https://localhost.example.test",
        "https://example.test",
        "http://localhost",
    ];
    let cases = subjects
        .iter()
        .map(|subject| json!({"payload":"warning","details":{"subject":subject,"publicKey":"AA="}}))
        .collect::<Vec<_>>();
    let source = oracle(&json!({"op":"warnings","cases":cases}));
    let mut actual = Vec::new();
    for (index, subject) in subjects.iter().enumerate() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_crypto::warning_child", "--nocapture"])
            .env("IVY_HIVE_PUSH_WARNING_SUBJECT", subject)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        let expected = source["requests"][index]["warnings"].as_array().unwrap();
        assert_eq!(
            stderr.lines().collect::<Vec<_>>(),
            expected
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
            "{subject}"
        );
        assert!(expected.iter().all(|v| v == warning));
        actual
            .push(json!({"ok":false,"clockReads":0,"warnings":stderr.lines().collect::<Vec<_>>()}));
    }
    save(
        &json!({"op":"warnings","cases":cases}),
        &source["requests"],
        &json!(actual),
    );
    assert_eq!(json!(actual), source["requests"]);
}
