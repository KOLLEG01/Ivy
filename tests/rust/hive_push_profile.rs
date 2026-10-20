use ivy_contracts::IvyJson;
use ivy_hive::{
    browser_push::{SendOutcome, Sender},
    browser_push_https::{Lookup, LookupFuture, NativeHttps, NetworkProfile},
    browser_push_native::NativeSender,
};
use serde_json::{Value, json};
use std::{
    io::Write,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[path = "hive_push_peer.rs"]
mod peer;
use peer::*;
#[path = "hive_push_https.rs"]
mod existing_https;

struct Addresses {
    values: Vec<String>,
    delay: Duration,
    calls: AtomicUsize,
}
impl Lookup for Addresses {
    fn addresses(&self, _: String, port: u16) -> LookupFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let values = self.values.clone();
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(values
                .iter()
                .map(|v| SocketAddr::new(v.parse::<IpAddr>().unwrap(), port))
                .collect())
        })
    }
}
fn sender(peer: &Oracle, lookup: Arc<dyn Lookup>, profile: NetworkProfile) -> NativeSender {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use tokio_rustls::rustls::{
        ClientConfig, RootCertStore, crypto::aws_lc_rs, pki_types::CertificateDer,
    };
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(
            STANDARD
                .decode(peer.profile["caDer"].as_str().unwrap())
                .unwrap(),
        ))
        .unwrap();
    let tls = ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    NativeSender::new(
        Arc::new(
            NativeHttps::with_network_profile(
                tokio::runtime::Handle::current(),
                Arc::new(tls),
                lookup,
                profile,
            )
            .unwrap(),
        ),
        Arc::new(|| Ok(1700043200)),
    )
}
fn lookup(values: &[&str], delay: u64) -> Arc<Addresses> {
    Arc::new(Addresses {
        values: values.iter().map(|s| s.to_string()).collect(),
        delay: Duration::from_millis(delay),
        calls: 0.into(),
    })
}
async fn compare(
    peer: &mut Oracle,
    sender: &NativeSender,
    path: &str,
    host: Option<&str>,
) -> Value {
    let original = peer
        .call(json!({"op":"original","path":path,"hostname":host}))
        .await;
    let before = peer.call(json!({"op":"stats"})).await["requests"]
        .as_array()
        .unwrap()
        .len();
    let (status, millis) = send(sender, &peer.profile, path, host).await;
    let all = peer.call(json!({"op":"stats"})).await;
    let requests = &all["requests"].as_array().unwrap()[before..];
    let record = json!({"profile":peer.profile["profile"],"path":path,"host":host,"original":original,"native":{"status":status,"millis":millis,"requests":requests}});
    save(&record);
    record
}
fn signatures(record: &Value) {
    for r in record["original"]["requests"]
        .as_array()
        .unwrap()
        .iter()
        .chain(record["native"]["requests"].as_array().unwrap())
    {
        assert_eq!(r["signatureValid"], true);
        assert_eq!(r["plaintext"], "notification");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_families_order_duplicates_and_dns_deadline_match_original() {
    let cases = [
        (vec!["::1", "127.0.0.1"], true, 0, false, 201),
        (
            vec!["::1", "::1", "127.0.0.2", "127.0.0.1"],
            true,
            0,
            false,
            201,
        ),
        (vec!["::1", "127.0.0.1"], false, 0, false, 0),
        (vec!["127.0.0.1", "::1"], true, 0, true, 201),
        (vec!["::1", "127.0.0.1"], true, 0, true, 201),
        (vec!["127.0.0.1"], true, 11000, false, 0),
    ];
    for (values, auto, delay, dual, status) in cases {
        let spec = json!({"certificate":"valid","addresses":values,"autoSelectFamily":auto,"lookupDelay":delay,"dualStack":dual});
        let mut peer = Oracle::start_profile(Some(&spec)).await;
        let resolver = lookup(&values, delay);
        let sender = sender(
            &peer,
            resolver.clone(),
            NetworkProfile {
                auto_select_family: auto,
                ..NetworkProfile::default()
            },
        );
        let record = compare(&mut peer, &sender, "/status/201", None).await;
        assert_eq!(record["original"]["status"], status, "{record}");
        assert_eq!(record["native"]["status"], status, "{record}");
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
        signatures(&record);
        if dual {
            assert_eq!(
                record["original"]["requests"][0]["remoteAddress"],
                record["native"]["requests"][0]["remoteAddress"]
            );
        }
        if auto && values.len() > 1 && !dual {
            assert!(
                record["native"]["millis"].as_f64().unwrap() < 1600.0,
                "{record}"
            );
        }
        if delay > 10000 {
            for side in ["original", "native"] {
                assert!((9000.0..12000.0).contains(&record[side]["millis"].as_f64().unwrap()));
            }
        }
        drop(sender);
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn certificate_profiles_preserve_checked_outcomes_and_record_common_name_gap() {
    for (kind, host, old, new) in [
        ("valid", None, 201, 201),
        ("expired", None, 0, 0),
        ("future", None, 0, 0),
        ("client-eku", None, 0, 0),
        ("common-name-only", None, 201, 0),
        ("wildcard", Some("a.example.test"), 201, 201),
        ("wildcard", Some("a.b.example.test"), 0, 0),
        ("ip", Some("127.0.0.1"), 201, 201),
    ] {
        let spec = json!({"certificate":kind,"addresses":["127.0.0.1"]});
        let mut peer = Oracle::start_profile(Some(&spec)).await;
        let resolver = lookup(&["127.0.0.1"], 0);
        let sender = sender(&peer, resolver.clone(), NetworkProfile::default());
        let record = compare(&mut peer, &sender, "/status/201", host).await;
        assert_eq!(record["original"]["status"], old, "{record}");
        assert_eq!(record["native"]["status"], new, "{record}");
        signatures(&record);
        if kind == "ip" {
            assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
            for side in ["original", "native"] {
                assert_eq!(record[side]["requests"][0]["servername"], false);
            }
        }
        // This explicit negative comparison is not a verifier parity pass.
        if kind == "common-name-only" {
            save(&json!({"finding":"open-common-name-fallback-gap","record":record}));
        }
        drop(sender);
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls12_tls13_and_session_resumption_match_original() {
    for version in ["TLSv1.2", "TLSv1.3"] {
        let spec = json!({"certificate":"valid","addresses":["127.0.0.1"],"minVersion":version,"maxVersion":version});
        let mut peer = Oracle::start_profile(Some(&spec)).await;
        let sender = sender(&peer, lookup(&["127.0.0.1"], 0), NetworkProfile::default());
        for repeated in [false, true] {
            let record = compare(&mut peer, &sender, "/close", None).await;
            for side in ["original", "native"] {
                assert_eq!(record[side]["status"], 200, "{record}");
                assert_eq!(record[side]["requests"][0]["tls"], version);
                assert_eq!(
                    record[side]["requests"][0]["sessionReused"], repeated,
                    "{record}"
                );
            }
            signatures(&record);
        }
        drop(sender);
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_cache_capacity_difference_is_an_open_profile_gap() {
    let spec = json!({"certificate":"wildcard","addresses":["127.0.0.1"],"minVersion":"TLSv1.2","maxVersion":"TLSv1.2"});
    let mut peer = Oracle::start_profile(Some(&spec)).await;
    let sender = sender(&peer, lookup(&["127.0.0.1"], 0), NetworkProfile::default());
    for index in 0..33 {
        let host = format!("s{index}.example.test");
        let record = compare(&mut peer, &sender, "/close", Some(&host)).await;
        for side in ["original", "native"] {
            assert_eq!(record[side]["status"], 200, "{record}");
            assert_eq!(record[side]["requests"][0]["sessionReused"], false);
        }
    }
    let record = compare(&mut peer, &sender, "/close", Some("s0.example.test")).await;
    save(&json!({"finding":"open-session-cache-capacity-gap","record":record}));
    assert_eq!(
        record["original"]["requests"][0]["sessionReused"], true,
        "{record}"
    );
    assert_eq!(
        record["native"]["requests"][0]["sessionReused"], false,
        "{record}"
    );
    // Node retains this entry in its100-origin cache. The supplied rustls default
    // has already evicted it; this explicit negative is not a parity pass.
    drop(sender);
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_response_admission_matches_original() {
    let spec = json!({"certificate":"valid","addresses":["127.0.0.1"]});
    let mut peer = Oracle::start_profile(Some(&spec)).await;
    let sender = sender(&peer, lookup(&["127.0.0.1"], 0), NetworkProfile::default());
    let mut mismatches = Vec::new();
    for kind in [
        "normal",
        "folded",
        "space",
        "lf",
        "duplicate",
        "conflict",
        "chunked",
        "interim",
        "old",
        "many",
        "large",
    ] {
        let record = compare(&mut peer, &sender, &format!("/raw/{kind}"), None).await;
        signatures(&record);
        if record["original"]["status"] != record["native"]["status"] {
            mismatches.push(record);
        }
    }
    drop(sender);
    peer.close().await;
    assert!(
        mismatches.is_empty(),
        "Response admission findings: {}",
        json!(mismatches)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_startup_roots_and_proxy_flags_have_explicit_profile_requirements() {
    let spec = json!({"certificate":"valid","addresses":["127.0.0.1"]});
    let mut peer = Oracle::start_profile(Some(&spec)).await;
    let trusted = sender(&peer, lookup(&["127.0.0.1"], 0), NetworkProfile::default());
    for (extra, system, insecure, proxy, ambient, status, hops) in [
        (false, false, false, false, false, 0, 0),
        (true, false, false, false, false, 201, 0),
        (false, true, false, false, false, 0, 0),
        (false, false, true, false, false, 201, 0),
        (true, false, false, false, true, 201, 0),
        (true, false, false, true, false, 201, 1),
    ] {
        let original=peer.call(json!({"op":"environment","extraCA":extra,"systemCA":system,"insecure":insecure,"envProxy":proxy,"ambientProxy":ambient})).await;
        assert_eq!(original["code"], 0, "{original}");
        assert_eq!(original["result"]["status"], status, "{original}");
        assert_eq!(original["proxyRequests"], hops, "{original}");
        let (native, _) = send(&trusted, &peer.profile, "/status/201", None).await;
        save(
            &json!({"environmentProfile":{"extraCA":extra,"systemCA":system,"insecure":insecure,"envProxy":proxy,"ambientProxy":ambient},"original":original,"native":{"status":native,"route":"explicit-direct","roots":"explicit-synthetic-CA"},"finding":"Startup root/verification/proxy inputs require a canonical production profile; explicit test trust is not an ambient profile implementation."}),
        );
        assert_eq!(native, 201);
    }
    drop(trusted);
    peer.close().await;
}
