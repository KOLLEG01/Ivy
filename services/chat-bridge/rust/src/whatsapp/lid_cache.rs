//! Bounded L1 mappings backed by the original account's SQLite store.
use super::{
    auth_backend::{NativeAuthStore, get, invalid, text},
    auth_protocol::{
        lid_mapping, pn_mapping, save_mapping, save_mapping_hashes, seed_contact_hashes,
    },
};
use std::{
    mem::{align_of, size_of},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use wacore::{
    net::{Transport, TransportEvent, TransportFactory},
    store::traits::LidPnMappingEntry,
    types::{LearningSource, LidPnEntry},
};
use whatsapp_rust::{
    CacheConfig, CacheEntryConfig, CacheStore,
    anyhow::{self, Result},
};

// Keep hundreds of active pairs hot in both directions, while independently
// bounding retained backing and payload bytes.
const MAX_ENTRIES: usize = 512;
const MAX_BYTES: usize = 128 * 1024;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Lid,
    Phone,
    ContactHash,
}
impl Direction {
    fn parse(namespace: &str) -> Result<Self> {
        match namespace {
            "lid_pn_by_lid" => Ok(Self::Lid),
            "lid_pn_by_pn" => Ok(Self::Phone),
            "lid_pn_contact_hash" => Ok(Self::ContactHash),
            _ => Err(anyhow::anyhow!("Unsupported mapping cache namespace")),
        }
    }
}
#[derive(Clone, serde::Serialize)]
#[serde(untagged)]
enum HotValue {
    Mapping(LidPnEntry),
    Contact(Arc<str>),
}
impl HotValue {
    fn bytes(&self) -> usize {
        let arc_bytes = |text: &str| {
            (text.len() + 2 * size_of::<usize>()).next_multiple_of(align_of::<usize>())
        };
        match self {
            Self::Mapping(v) => arc_bytes(&v.lid) + arc_bytes(&v.phone_number),
            Self::Contact(v) => arc_bytes(v),
        }
    }
}
struct HotEntry {
    direction: Direction,
    key: String,
    value: HotValue,
    bytes: usize,
}
struct HotMappings {
    entries: Vec<HotEntry>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
    revision: u64,
}
impl HotMappings {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        let entries = Vec::with_capacity(max_entries);
        let bytes = entries.capacity() * size_of::<HotEntry>();
        Self {
            entries,
            bytes,
            max_entries,
            max_bytes,
            revision: 0,
        }
    }
    fn remove(&mut self, index: usize) -> HotEntry {
        let entry = self.entries.remove(index);
        self.bytes -= entry.bytes;
        entry
    }
    fn clear(&mut self, direction: Option<Direction>) {
        let mut i = 0;
        while i < self.entries.len() {
            if direction.is_none_or(|d| d == self.entries[i].direction) {
                self.remove(i);
            } else {
                i += 1;
            }
        }
    }
    fn get(&mut self, direction: Direction, key: &str) -> Option<HotValue> {
        let index = self
            .entries
            .iter()
            .position(|e| e.direction == direction && e.key == key)?;
        let entry = self.entries.remove(index);
        let value = entry.value.clone();
        self.entries.push(entry);
        Some(value)
    }
    fn put(&mut self, direction: Direction, key: &str, value: HotValue) {
        if let Some(i) = self
            .entries
            .iter()
            .position(|e| e.direction == direction && e.key == key)
        {
            self.remove(i);
        }
        let key = key.to_owned();
        let bytes = key.capacity() + value.bytes();
        let backing = self.entries.capacity() * size_of::<HotEntry>();
        if self.max_entries == 0 || backing + bytes > self.max_bytes {
            return;
        }
        while self.entries.len() >= self.max_entries || self.bytes + bytes > self.max_bytes {
            self.remove(0);
        }
        self.bytes += bytes;
        self.entries.push(HotEntry {
            direction,
            key,
            value,
            bytes,
        });
    }
}
pub(super) struct LidCache {
    backend: Arc<NativeAuthStore>,
    hot: Mutex<HotMappings>,
    failed: AtomicBool,
}
fn cached(entry: LidPnMappingEntry) -> LidPnEntry {
    LidPnEntry::with_timestamp(
        entry.lid,
        entry.phone_number,
        entry.created_at,
        LearningSource::parse(&entry.learning_source),
    )
}
fn same(entry: &LidPnMappingEntry, value: &LidPnEntry) -> bool {
    entry.lid == value.lid.as_ref()
        && entry.phone_number == value.phone_number.as_ref()
        && entry.created_at == value.created_at
        && LearningSource::parse(&entry.learning_source) == value.learning_source
}
impl LidCache {
    pub(super) fn new(backend: Arc<NativeAuthStore>) -> Result<Arc<Self>> {
        backend.write(seed_contact_hashes)?;
        Ok(Arc::new(Self {
            backend,
            hot: Mutex::new(HotMappings::new(MAX_ENTRIES, MAX_BYTES)),
            failed: AtomicBool::new(false),
        }))
    }
    pub(super) fn config(self: &Arc<Self>) -> CacheConfig {
        let mut config = CacheConfig::default();
        // Pair and hash indexes share the bounded L1. The remaining native
        // persisted tracker is a hint: eviction only repeats persistence.
        config.lid_pn_cache = CacheEntryConfig::new(None, MAX_ENTRIES as u64);
        config.cache_stores.lid_pn_cache = Some(self.clone());
        config
    }
    pub(super) fn check(&self) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
            Err(anyhow::anyhow!(
                "Mapping persistence failed; restart the account owner after recovery"
            ))
        } else {
            Ok(())
        }
    }
    fn finish<T>(&self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    fn lookup(&self, direction: Direction, key: &str) -> Result<Option<HotValue>> {
        if direction == Direction::ContactHash {
            let hash = key.parse::<u32>()?;
            if hash > 0x00ff_ffff {
                return Err(anyhow::anyhow!("Invalid contact hash"));
            }
            return Ok(self
                .backend
                .read(|db| {
                    let Some(value) = get(db, "native-contact-hash", key)? else {
                        return Ok(None);
                    };
                    let lid = text(&value)?;
                    if lid_mapping(db, &lid)?.is_none() {
                        return Err(invalid("contact hash mapping"));
                    }
                    Ok(Some(lid))
                })?
                .map(|v| HotValue::Contact(v.into())));
        }
        Ok(self
            .backend
            .read(|db| match direction {
                Direction::Lid => lid_mapping(db, key),
                Direction::Phone => pn_mapping(db, key),
                Direction::ContactHash => unreachable!(),
            })?
            .map(cached)
            .map(HotValue::Mapping))
    }
    fn sync_revision(&self, hot: &mut HotMappings) {
        let revision = self.backend.mapping_revision();
        if hot.revision != revision {
            hot.clear(None);
            hot.revision = revision;
        }
    }
}
#[async_trait::async_trait]
impl CacheStore for LidCache {
    async fn get(&self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>> {
        self.finish((|| {
            self.check()?;
            let direction = Direction::parse(namespace)?;
            let mut hot = self
                .hot
                .lock()
                .map_err(|_| anyhow::anyhow!("Mapping cache lock failed"))?;
            self.sync_revision(&mut hot);
            let value = match hot.get(direction, key) {
                Some(v) => Some(v),
                None => {
                    let value = self.lookup(direction, key)?;
                    if let Some(v) = &value {
                        hot.put(direction, key, v.clone());
                    }
                    value
                }
            };
            value
                .map(|v| serde_json::to_vec(&v).map_err(Into::into))
                .transpose()
        })())
    }
    async fn set(
        &self,
        namespace: &str,
        key: &str,
        bytes: &[u8],
        _: Option<Duration>,
    ) -> Result<()> {
        self.finish((|| {
            self.check()?;
            let direction = Direction::parse(namespace)?;
            if direction == Direction::ContactHash {
                let _: Arc<str> = serde_json::from_slice(bytes)?;
                let mut hot = self
                    .hot
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Mapping cache lock failed"))?;
                self.sync_revision(&mut hot);
                // The accepted pair already committed this derived index.
                // Load its winner rather than trusting a stale background set.
                let stored = self
                    .lookup(direction, key)?
                    .ok_or_else(|| anyhow::anyhow!("Contact hash index missing"))?;
                hot.put(direction, key, stored);
                return Ok(());
            }
            let value: LidPnEntry = serde_json::from_slice(bytes)?;
            if match direction {
                Direction::Lid => value.lid.as_ref() != key,
                Direction::Phone => value.phone_number.as_ref() != key,
                Direction::ContactHash => unreachable!(),
            } {
                return Err(anyhow::anyhow!("Mapping cache identity mismatch"));
            }
            let mut hot = self
                .hot
                .lock()
                .map_err(|_| anyhow::anyhow!("Mapping cache lock failed"))?;
            self.sync_revision(&mut hot);
            if direction == Direction::Lid {
                // Cache-only offline learns must survive eviction too. Commit
                // the accepted pair before any caller can resolve it from L1.
                let changed = self.backend.write(|db| {
                    let old = lid_mapping(db, &value.lid)?;
                    let forward = pn_mapping(db, &value.phone_number)?;
                    if old.as_ref().is_some_and(|e| same(e, &value))
                        && forward.as_ref().is_some_and(|e| same(e, &value))
                    {
                        return save_mapping_hashes(db, &value.lid, &value.phone_number, true);
                    }
                    save_mapping(
                        db,
                        &LidPnMappingEntry {
                            lid: value.lid.to_string(),
                            phone_number: value.phone_number.to_string(),
                            created_at: value.created_at,
                            updated_at: super::auth_backend::now_ms()? / 1000,
                            learning_source: value.learning_source.as_str().into(),
                        },
                    )?;
                    Ok(true)
                })?;
                if changed {
                    self.backend.mappings_changed();
                    self.sync_revision(&mut hot);
                }
            }
            // The timestamp winner in SQLite also governs the PN side of L1.
            // In particular a stale LID alias cannot replace a newer winner.
            if let Some(stored) = self.lookup(direction, key)? {
                hot.put(direction, key, stored);
            }
            Ok(())
        })())
    }
    async fn delete(&self, namespace: &str, key: &str) -> Result<()> {
        self.finish((|| {
            let direction = Direction::parse(namespace)?;
            let mut hot = self
                .hot
                .lock()
                .map_err(|_| anyhow::anyhow!("Mapping cache lock failed"))?;
            if let Some(i) = hot
                .entries
                .iter()
                .position(|e| e.direction == direction && e.key == key)
            {
                hot.remove(i);
            }
            Ok(())
        })())
    }
    async fn clear(&self, namespace: &str) -> Result<()> {
        self.finish((|| {
            let direction = Direction::parse(namespace)?;
            self.hot
                .lock()
                .map_err(|_| anyhow::anyhow!("Mapping cache lock failed"))?
                .clear(Some(direction));
            Ok(())
        })())
    }
    async fn entry_count(&self, namespace: &str) -> Result<u64> {
        let direction = Direction::parse(namespace)?;
        Ok(self
            .hot
            .lock()
            .map_err(|_| anyhow::anyhow!("Mapping cache lock failed"))?
            .entries
            .iter()
            .filter(|e| e.direction == direction)
            .count() as u64)
    }
}

pub(super) struct MappingTransportFactory<F> {
    pub inner: F,
    pub cache: Arc<LidCache>,
}
struct MappingTransport {
    inner: Arc<dyn Transport>,
    cache: Arc<LidCache>,
}
#[async_trait::async_trait]
impl Transport for MappingTransport {
    async fn send(&self, bytes: bytes::Bytes) -> Result<()> {
        // TypedCache treats store errors as misses. A miss caused by failed
        // persistence must never put a fallback Signal namespace on the wire,
        // even when the library still has leased sessions in its own cache.
        self.cache.check()?;
        self.inner.send(bytes).await
    }
    async fn disconnect(&self) {
        self.inner.disconnect().await;
    }
    fn resource_report(&self) -> Option<wacore::stats::TransportResourceReport> {
        self.inner.resource_report()
    }
}
#[async_trait::async_trait]
impl<F: TransportFactory + 'static> TransportFactory for MappingTransportFactory<F> {
    async fn create_transport(
        &self,
    ) -> Result<(
        Arc<dyn Transport>,
        whatsapp_rust::async_channel::Receiver<TransportEvent>,
    )> {
        self.cache.check()?;
        let (inner, events) = self.inner.create_transport().await?;
        Ok((
            Arc::new(MappingTransport {
                inner,
                cache: self.cache.clone(),
            }),
            events,
        ))
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/chat-lid-cache.rs"]
mod tests;
