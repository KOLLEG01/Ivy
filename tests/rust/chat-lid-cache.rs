use super::*;
use crate::whatsapp::auth::{AuthAccount, AuthStore};
use std::{path::PathBuf, sync::atomic::AtomicUsize};
use wacore::store::traits::{DeviceStore, ProtocolStore, SignalStore};
#[path = "fixtures/chat-signal-session.rs"]
mod signal_fixture;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-lid-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn backend(&self) -> Arc<NativeAuthStore> {
        Arc::new(NativeAuthStore::new(
            AuthAccount::acquire(&self.0.join("auth.sqlite"), &self.0.join("owner")).unwrap(),
        ))
    }
    fn snapshot(&self) -> Vec<(String, String, String)> {
        AuthStore::open(self.0.join("auth.sqlite"))
            .unwrap()
            .snapshot()
            .unwrap()
            .into_iter()
            .map(|r| (r.category, r.id, r.value))
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn small_cache(backend: Arc<NativeAuthStore>) -> Arc<LidCache> {
    backend.write(seed_contact_hashes).unwrap();
    Arc::new(LidCache {
        backend,
        hot: Mutex::new(HotMappings::new(2, 4096)),
        failed: AtomicBool::new(false),
    })
}
fn library(cache: &Arc<LidCache>) -> whatsapp_rust::lid_pn_cache::LidPnCache {
    whatsapp_rust::lid_pn_cache::LidPnCache::with_config(
        &cache.config().lid_pn_cache,
        Some(cache.clone()),
    )
}
fn entry(lid: &str, phone: &str, time: i64) -> LidPnEntry {
    LidPnEntry::with_timestamp(lid, phone, time, LearningSource::PeerLidMessage)
}
fn bounded(cache: &LidCache) {
    let hot = cache.hot.lock().unwrap();
    assert!(hot.entries.len() <= hot.max_entries && hot.bytes <= hot.max_bytes);
    assert_eq!(
        hot.bytes,
        hot.entries.capacity() * size_of::<HotEntry>()
            + hot.entries.iter().map(|e| e.bytes).sum::<usize>()
    );
}
#[tokio::test]
async fn eviction_and_cold_reads_preserve_original_rows_and_both_mapping_directions() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let original = AuthStore::open(fixture.0.join("auth.sqlite")).unwrap();
    original
        .set("lid-mapping", "1555001", Some(" \"9001\"\n"))
        .unwrap();
    original
        .set("lid-mapping", "9001_reverse", Some(" \"1555001\"\n"))
        .unwrap();
    original.set("native-lid-mapping","9001",Some(r#" {"lid":"9001","phone_number":"1555001","created_at":100,"updated_at":50,"learning_source":"future-source","opaque":[1,{"x":"unchanged 😀"}]} "#)).unwrap();
    let before = fixture.snapshot();
    let cache = small_cache(backend.clone());
    let seeded = fixture.snapshot();
    assert!(before.iter().all(|row| seeded.contains(row)));
    let derived: Vec<_> = seeded.iter().filter(|row| !before.contains(row)).collect();
    assert_eq!(derived.len(), 2);
    assert!(
        derived
            .iter()
            .all(|row| row.0 == "native-contact-hash" && row.2 == "\"9001\"")
    );
    let native = library(&cache);
    native
        .warm_up(
            backend
                .get_all_lid_mappings()
                .await
                .unwrap()
                .into_iter()
                .map(cached),
        )
        .await;
    assert_eq!(
        fixture.snapshot(),
        seeded,
        "Warm-up must not rewrite retained metadata"
    );
    for i in 2..32 {
        native
            .add(&entry(&format!("900{i}"), &format!("155500{i}"), i))
            .await;
        bounded(&cache);
    }
    assert!(
        cache
            .hot
            .lock()
            .unwrap()
            .entries
            .iter()
            .any(|e| e.direction == Direction::ContactHash),
        "Native hash index must use the same custom store"
    );
    assert!(
        cache
            .hot
            .lock()
            .unwrap()
            .entries
            .iter()
            .all(|e| e.key != "9001" && e.key != "1555001")
    );
    assert_eq!(
        native.get_current_lid("1555001").await.as_deref(),
        Some("9001")
    );
    assert_eq!(
        native.get_phone_number("9001").await.as_deref(),
        Some("1555001")
    );
    native.clear().await;
    let original_hash = crate::whatsapp::auth_protocol::contact_hash_key("1555001");
    assert_eq!(
        serde_json::from_slice::<String>(
            &cache
                .get("lid_pn_contact_hash", &original_hash)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        "9001"
    );
    assert_eq!(
        native.get_current_lid("1555001").await.as_deref(),
        Some("9001")
    );
    let large = entry(&"9".repeat(8192), "1555099", 500);
    native.add(&large).await;
    bounded(&cache);
    assert!(
        cache
            .hot
            .lock()
            .unwrap()
            .entries
            .iter()
            .all(|e| e.key != large.lid.as_ref())
    );
    assert_eq!(
        native.get_phone_number(&large.lid).await.as_deref(),
        Some("1555099")
    );
    bounded(&cache);
    backend.close().unwrap();
    let reopened = fixture.backend();
    let cold = small_cache(reopened.clone());
    let native = library(&cold);
    assert_eq!(
        native.get_current_lid("1555001").await.as_deref(),
        Some("9001")
    );
    assert_eq!(
        native.get_phone_number("9001").await.as_deref(),
        Some("1555001")
    );
    assert_eq!(
        native.get_current_lid("1555099").await.as_deref(),
        Some(large.lid.as_ref())
    );
    for row in before {
        assert!(
            fixture.snapshot().contains(&row),
            "Original auth row changed"
        );
    }
    bounded(&cold);
    reopened.close().unwrap();
}
#[tokio::test]
async fn historical_lid_aliases_and_timestamp_winners_survive_eviction_warmup_and_restart() {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    let cache = small_cache(backend.clone());
    let native = library(&cache);
    for (lid, time) in [("9001", 100), ("9002", 200), ("9000", 0), ("9003", 200)] {
        native.add(&entry(lid, "1555001", time)).await;
    }
    native.clear().await;
    assert_eq!(
        native.get_current_lid("1555001").await.as_deref(),
        Some("9003")
    );
    for lid in ["9000", "9001", "9002", "9003"] {
        assert_eq!(
            native.get_phone_number(lid).await.as_deref(),
            Some("1555001")
        );
    }
    native.add(&entry("9003", "1555002", 300)).await;
    native.add(&entry("9004", "1555001", 150)).await;
    assert_eq!(
        backend
            .get_pn_mapping("1555001")
            .await
            .unwrap()
            .unwrap()
            .created_at,
        200
    );
    backend.close().unwrap();
    let backend = fixture.backend();
    let cache = small_cache(backend.clone());
    let native = library(&cache);
    let before = fixture.snapshot();
    native
        .warm_up(
            backend
                .get_all_lid_mappings()
                .await
                .unwrap()
                .into_iter()
                .map(cached),
        )
        .await;
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(
        native.get_current_lid("1555001").await.as_deref(),
        Some("9003")
    );
    assert_eq!(
        native.get_current_lid("1555002").await.as_deref(),
        Some("9003")
    );
    assert_eq!(
        native.get_phone_number("9003").await.as_deref(),
        Some("1555002")
    );
    assert_eq!(
        native.get_phone_number("9001").await.as_deref(),
        Some("1555001")
    );
    // The native client's detached ProtocolStore writer can commit after a
    // hot cache read. Its new winner must become visible without eviction.
    backend
        .put_lid_mapping(&LidPnMappingEntry {
            lid: "9005".into(),
            phone_number: "1555001".into(),
            created_at: 400,
            updated_at: 400,
            learning_source: "usync".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        native.get_current_lid("1555001").await.as_deref(),
        Some("9005")
    );
    assert_eq!(
        native.get_current_lid("1555002").await.as_deref(),
        Some("9003")
    );
    bounded(&cache);
    backend.close().unwrap();
}
#[derive(Clone, Default)]
struct Network {
    creates: Arc<AtomicUsize>,
    writes: Arc<AtomicUsize>,
}
struct Wire(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transport for Wire {
    async fn send(&self, _: bytes::Bytes) -> Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn disconnect(&self) {}
    fn resource_report(&self) -> Option<wacore::stats::TransportResourceReport> {
        Some(wacore::stats::TransportResourceReport {
            read_buffer_bytes: Some(1234),
            write_buffer_bytes: Some(4321),
            tls_state_bytes: None,
        })
    }
}
#[async_trait::async_trait]
impl TransportFactory for Network {
    async fn create_transport(
        &self,
    ) -> Result<(
        Arc<dyn Transport>,
        whatsapp_rust::async_channel::Receiver<TransportEvent>,
    )> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        let (_, rx) = whatsapp_rust::async_channel::bounded(1);
        Ok((Arc::new(Wire(self.writes.clone())), rx))
    }
}
#[async_trait::async_trait]
impl whatsapp_rust::http::HttpClient for Network {
    async fn execute(&self, _: wacore::net::HttpRequest) -> Result<wacore::net::HttpResponse> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("Offline synthetic HTTP"))
    }
}
#[tokio::test]
async fn failed_mapping_transaction_cannot_send_a_fallback_namespace_through_an_existing_transport()
{
    for failure in 0..5 {
        let fixture = Fixture::new();
        let backend = fixture.backend();
        let cache = small_cache(backend.clone());
        let native = library(&cache);
        let network = Network::default();
        let factory = MappingTransportFactory {
            inner: network.clone(),
            cache: cache.clone(),
        };
        let (wire, _) = factory.create_transport().await.unwrap();
        let report = wire.resource_report().unwrap();
        assert_eq!(
            (
                report.read_buffer_bytes,
                report.write_buffer_bytes,
                report.tls_state_bytes
            ),
            (Some(1234), Some(4321), None)
        );
        wire.send(bytes::Bytes::from_static(b"prior-valid-frame"))
            .await
            .unwrap();
        if failure == 2 {
            let original = AuthStore::open(fixture.0.join("auth.sqlite")).unwrap();
            original
                .set("lid-mapping", "1555999", Some("\"9999\""))
                .unwrap();
            original.set("native-lid-mapping","9999",Some(r#"{"lid":"wrong-key","phone_number":"1555999","created_at":1,"updated_at":1,"learning_source":"usync"}"#)).unwrap();
        }
        let contact_key = crate::whatsapp::auth_protocol::contact_hash_key("1555999");
        if failure == 3 {
            AuthStore::open(fixture.0.join("auth.sqlite"))
                .unwrap()
                .set("native-contact-hash", &contact_key, Some("\"unknown-lid\""))
                .unwrap();
        }
        let before = fixture.snapshot();
        if failure == 1 {
            backend.close().unwrap();
        } else if failure == 0 || failure == 4 {
            let db = rusqlite::Connection::open(fixture.0.join("auth.sqlite")).unwrap();
            if failure == 0 {
                db.execute_batch("CREATE TRIGGER reject_mapping BEFORE INSERT ON auth WHEN NEW.category='native-lid-mapping' AND NEW.id='9999' BEGIN SELECT RAISE(ABORT,'fixture_failure'); END;").unwrap();
            } else {
                db.execute_batch("CREATE TRIGGER reject_mapping BEFORE INSERT ON auth WHEN NEW.category='native-contact-hash' BEGIN SELECT RAISE(ABORT,'fixture_failure'); END;").unwrap();
            }
            // TypedCache intentionally swallows the failed write. The commit rolls
            // back all directions; the owner and wire gate latch the failure anyway.
            native.add(&entry("9999", "1555999", 300)).await;
        }
        assert_eq!(fixture.snapshot(), before);
        if failure == 3 {
            assert!(
                cache
                    .get("lid_pn_contact_hash", &contact_key)
                    .await
                    .is_err()
            );
        } else {
            assert!(native.get_current_lid("1555999").await.is_none());
        }
        assert!(cache.check().is_err());
        native.clear().await;
        assert!(cache.check().is_err());
        assert!(
            wire.send(bytes::Bytes::from_static(b"refuse-fallback-frame"))
                .await
                .is_err()
        );
        assert!(factory.create_transport().await.is_err());
        assert_eq!(network.creates.load(Ordering::SeqCst), 1);
        assert_eq!(network.writes.load(Ordering::SeqCst), 1);
        backend.close().unwrap();
    }
}
#[tokio::test]
async fn public_signal_encryption_uses_the_original_lid_session_after_cache_eviction_and_cold_start()
 {
    let fixture = Fixture::new();
    let backend = fixture.backend();
    backend.create().await.unwrap();
    let mut device = backend.load().await.unwrap().unwrap();
    device.pn = Some("1555000:1@s.whatsapp.net".parse().unwrap());
    backend.save(&device).await.unwrap();
    let record = signal_fixture::native_session_with_long_current_chain(&device);
    let raw = format!(
        " \n{}\n",
        ivy_contracts::json::stringify(
            &crate::whatsapp::auth_signal::encode(record, None, 1000).unwrap(),
            usize::MAX
        )
        .unwrap()
    );
    let original = AuthStore::open(fixture.0.join("auth.sqlite")).unwrap();
    original.set("session", "9001002_1.0", Some(&raw)).unwrap();
    backend
        .put_lid_mapping(&LidPnMappingEntry {
            lid: "9001002".into(),
            phone_number: "1555001".into(),
            created_at: 100,
            updated_at: 100,
            learning_source: "usync".into(),
        })
        .await
        .unwrap();
    let mut old_ceiling = 0;
    let mut store = backend;
    for pass in 0..2 {
        let cache = small_cache(store.clone());
        let native = library(&cache);
        for i in 2..8 {
            native
                .add(&entry(&format!("90100{i}"), &format!("155500{i}"), i))
                .await;
        }
        assert!(
            cache
                .hot
                .lock()
                .unwrap()
                .entries
                .iter()
                .all(|e| e.key != "1555001")
        );
        let network = Network::default();
        let bot = whatsapp_rust::bot::Bot::builder()
            .with_backend_arc(store.clone())
            .with_cache_config(cache.config())
            .with_transport_factory(MappingTransportFactory {
                inner: network.clone(),
                cache: cache.clone(),
            })
            .with_http_client(network.clone())
            .skip_history_sync()
            .build()
            .await
            .unwrap();
        let (_, encrypted) = bot
            .client()
            .signal()
            .encrypt_message(
                &"1555001@s.whatsapp.net".parse().unwrap(),
                b"original namespace after cache miss",
            )
            .await
            .unwrap();
        assert!(!encrypted.is_empty());
        let bytes = store.get_session("9001002@lid.0").await.unwrap().unwrap();
        let ceiling = wacore::libsignal::protocol::SessionRecord::deserialize(&bytes)
            .unwrap()
            .reserved_sender_chain_index();
        assert!(ceiling > old_ceiling);
        old_ceiling = ceiling;
        assert!(
            store
                .get_session("1555001@s.whatsapp.net.0")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            original.get("session", "9001002_1.0").unwrap().as_deref(),
            Some(raw.as_str())
        );
        assert_eq!(network.creates.load(Ordering::SeqCst), 0);
        assert_eq!(network.writes.load(Ordering::SeqCst), 0);
        drop(bot);
        bounded(&cache);
        store.close().unwrap();
        if pass == 0 {
            store = fixture.backend();
        }
    }
}
