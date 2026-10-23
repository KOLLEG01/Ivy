use super::auth_backend::*;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_contracts::IvyJson as Json;
use rusqlite::Connection;
use wacore::{
    appstate::{WAPATCH_INTEGRITY, hash::HashState, processor::AppStateMutationMAC},
    store::{
        error::Result,
        traits::{AppStateSyncKey, AppSyncStore},
    },
};
use waproto::{codec, whatsapp::message::AppStateSyncKeyFingerprint};

fn timestamp(v: &Json) -> Result<i64> {
    if let Json::String(_) = v {
        text(v)?.parse().map_err(|_| invalid("sync timestamp"))
    } else {
        integer(v)
    }
}
fn latest_key(db: &Connection) -> Result<Option<String>> {
    let preferred = get(db, "creds", "default")?
        .and_then(|v| v.get("myAppStateKeyId").cloned())
        .map(|v| text(&v))
        .transpose()?;
    if let Some(id) = preferred {
        if get_raw(db, "app-state-sync-key", &id)?.is_some() {
            return Ok(Some(id));
        }
    }
    let mut latest = None;
    for (id, v) in entries(db, "app-state-sync-key")? {
        let time = timestamp(field(&v, "timestamp")?)?;
        if latest.as_ref().is_none_or(|(t, _)| time > *t) {
            latest = Some((time, id));
        }
    }
    Ok(latest.map(|(_, id)| id))
}
fn fingerprint(v: &Json) -> Result<Vec<u8>> {
    let indexes = v
        .get("deviceIndexes")
        .map(|v| v.as_array().ok_or_else(|| invalid("sync fingerprint")))
        .transpose()?
        .unwrap_or(&[])
        .iter()
        .map(uint)
        .collect::<Result<_>>()?;
    Ok(codec::app_state_sync_key_fingerprint_to_vec(
        &AppStateSyncKeyFingerprint {
            raw_id: v.get("rawId").map(uint).transpose()?,
            current_index: v.get("currentIndex").map(uint).transpose()?,
            device_indexes: indexes,
            ..Default::default()
        },
    ))
}
fn decode_state(value: Option<&Json>, flags: Option<&Json>) -> Result<HashState> {
    let Some(v) = value else {
        return Ok(HashState::default());
    };
    let map = field(&v, "indexValueMap")?
        .as_object()
        .ok_or_else(|| invalid("sync MAC map"))?
        .iter()
        .map(|(key, v)| {
            Ok((
                key.to_utf8().map_err(|_| invalid("MAC index"))?,
                bytes(field(v, "valueMac")?)?,
            ))
        })
        .collect::<Result<_>>()?;
    Ok(HashState {
        version: u64::try_from(integer(field(&v, "version")?)?)
            .map_err(|_| invalid("sync version"))?,
        hash: bytes(field(&v, "hash")?)?
            .try_into()
            .map_err(|_| invalid("sync hash"))?,
        index_value_map: map,
        mac_mismatch_fatal: flags
            .and_then(|v| v.get("macMismatchFatal"))
            .and_then(Json::as_bool)
            .unwrap_or(false),
    })
}
fn state(db: &Connection, name: &str) -> Result<HashState> {
    decode_state(
        get(db, "app-state-sync-version", name)?.as_ref(),
        get(db, "native-app-state-flags", name)?.as_ref(),
    )
}

/// The client saves patches version-first and snapshots MACs-first. Stage both
/// callback orders, publishing only when the MAC accumulator reaches the target
/// hash. The native library supplies the accumulator; no protocol math is copied.
/// Canonical rows remain a complete checkpoint for either client after a crash.
struct SyncUpdate {
    value: Json,
    state: HashState,
    mac_hash: [u8; 128],
}
impl SyncUpdate {
    fn load(db: &Connection, name: &str) -> Result<Self> {
        if let Some(pending) = get(db, "native-app-state-pending", name)? {
            let value = field(&pending, "value")?.clone();
            return Ok(Self {
                state: decode_state(Some(&value), pending.get("flags"))?,
                value,
                mac_hash: bytes(field(&pending, "macHash")?)?
                    .try_into()
                    .map_err(|_| invalid("pending sync hash"))?,
            });
        }
        let value = get(db, "app-state-sync-version", name)?;
        let state = decode_state(
            value.as_ref(),
            get(db, "native-app-state-flags", name)?.as_ref(),
        )?;
        Ok(Self {
            value: object(value)?,
            mac_hash: state.hash,
            state,
        })
    }
    fn change_mac(&mut self, index: String, next: Option<Vec<u8>>) {
        let prior = self.state.index_value_map.remove(&index);
        WAPATCH_INTEGRITY.subtract_then_add_in_place(
            &mut self.mac_hash,
            prior.as_slice(),
            next.as_slice(),
        );
        if let Some(next) = next {
            self.state.index_value_map.insert(index, next);
        }
    }
    fn save(mut self, db: &Connection, name: &str) -> Result<()> {
        self.value.insert(
            "version",
            number(i64::try_from(self.state.version).map_err(|_| invalid("sync version"))?)?,
        );
        self.value.insert("hash", buffer(&self.state.hash));
        let mut map = object(self.value.get("indexValueMap").cloned())?;
        let prior_indexes = map
            .as_object()
            .unwrap()
            .iter()
            .map(|(index, _)| index.to_utf8().map_err(|_| invalid("MAC index")))
            .collect::<Result<Vec<_>>>()?;
        for index in prior_indexes {
            if !self.state.index_value_map.contains_key(&index) {
                map.remove(&index);
            }
        }
        for (index, mac) in self.state.index_value_map {
            let mut value = object(map.get(&index).cloned())?;
            value.insert("valueMac", buffer(&mac));
            map.insert(index, value);
        }
        self.value.insert("indexValueMap", map);
        let flags = Json::object([("macMismatchFatal", self.state.mac_mismatch_fatal.into())]);
        if self.mac_hash == self.state.hash {
            put(db, "app-state-sync-version", name, Some(&self.value))?;
            put(db, "native-app-state-flags", name, Some(&flags))?;
            put(db, "native-app-state-pending", name, None)
        } else {
            put(
                db,
                "native-app-state-pending",
                name,
                Some(&Json::object([
                    ("value", self.value),
                    ("flags", flags),
                    ("macHash", buffer(&self.mac_hash)),
                ])),
            )
        }
    }
}
#[async_trait]
impl AppSyncStore for NativeAuthStore {
    async fn get_sync_key(&self, key_id: &[u8]) -> Result<Option<AppStateSyncKey>> {
        self.read(|db| {
            get(db, "app-state-sync-key", &STANDARD.encode(key_id))?
                .map(|v| {
                    Ok(AppStateSyncKey {
                        key_data: bytes(field(&v, "keyData")?)?,
                        fingerprint: v
                            .get("fingerprint")
                            .filter(|v| !v.is_null())
                            .map(fingerprint)
                            .transpose()?
                            .unwrap_or_default(),
                        timestamp: v.get("timestamp").map(timestamp).transpose()?.unwrap_or(0),
                    })
                })
                .transpose()
        })
    }
    async fn set_sync_key(&self, key_id: &[u8], key: AppStateSyncKey) -> Result<()> {
        self.write(|db| {
            let id = STANDARD.encode(key_id);
            let mut value = object(get(db, "app-state-sync-key", &id)?)?;
            let decoded =
                codec::app_state_sync_key_fingerprint_decode(&key.fingerprint).map_err(protocol)?;
            let mut fp = object(value.get("fingerprint").cloned())?;
            for (name, n) in [
                ("rawId", decoded.raw_id),
                ("currentIndex", decoded.current_index),
            ] {
                if let Some(n) = n {
                    fp.insert(name, number(n.into())?);
                } else {
                    fp.remove(name);
                }
            }
            fp.insert(
                "deviceIndexes",
                Json::Array(
                    decoded
                        .device_indexes
                        .into_iter()
                        .map(|n| number(n.into()))
                        .collect::<Result<_>>()?,
                ),
            );
            value.insert("keyData", buffer(&key.key_data));
            value.insert("fingerprint", fp);
            value.insert("timestamp", key.timestamp.to_string().into());
            put(db, "app-state-sync-key", &id, Some(&value))?;
            if let Some(mut creds) = get(db, "creds", "default")? {
                creds.insert("myAppStateKeyId", id.into());
                put(db, "creds", "default", Some(&creds))?;
            }
            Ok(())
        })
    }
    async fn get_version(&self, name: &str) -> Result<HashState> {
        // Each processor cycle starts here under its collection lock. Discard
        // an incomplete earlier attempt before deriving the next patch/snapshot.
        self.write(|db| {
            put(db, "native-app-state-pending", name, None)?;
            state(db, name)
        })
    }
    async fn set_version(&self, name: &str, value: HashState) -> Result<()> {
        // The native processor persists MACs separately and does not populate
        // HashState.index_value_map. Updating the version must retain that map.
        self.write(|db| {
            let mut update = SyncUpdate::load(db, name)?;
            if value.version == 0 && value.hash == [0; 128] {
                // An explicit collection reset requests a fresh snapshot.
                update.state.index_value_map.clear();
                update.mac_hash = [0; 128];
            }
            update.state.version = value.version;
            update.state.hash = value.hash;
            update.state.mac_mismatch_fatal = value.mac_mismatch_fatal;
            update.save(db, name)
        })
    }
    async fn put_mutation_macs(
        &self,
        name: &str,
        version: u64,
        mutations: &[AppStateMutationMAC],
    ) -> Result<()> {
        self.write(|db| {
            let mut update = SyncUpdate::load(db, name)?;
            // A snapshot's target version arrives later via set_version.
            if version < update.state.version {
                return Err(invalid("sync MAC version"));
            }
            for item in mutations {
                if item.index_mac.len() != 32 || item.value_mac.len() != 32 {
                    return Err(invalid("sync mutation MAC"));
                }
                update.change_mac(
                    STANDARD.encode(&item.index_mac),
                    Some(item.value_mac.clone()),
                );
            }
            update.save(db, name)
        })
    }
    async fn get_mutation_mac(&self, name: &str, index_mac: &[u8]) -> Result<Option<Vec<u8>>> {
        self.read(|db| {
            Ok(state(db, name)?
                .index_value_map
                .remove(&STANDARD.encode(index_mac)))
        })
    }
    async fn get_mutation_macs(
        &self,
        name: &str,
        indexes: &[[u8; 32]],
    ) -> Result<std::collections::HashMap<[u8; 32], Vec<u8>>> {
        self.read(|db| {
            let mut current = state(db, name)?.index_value_map;
            Ok(indexes
                .iter()
                .filter_map(|index| {
                    current
                        .remove(&STANDARD.encode(index))
                        .map(|mac| (*index, mac))
                })
                .collect())
        })
    }
    async fn delete_mutation_macs(&self, name: &str, index_macs: &[Vec<u8>]) -> Result<()> {
        self.write(|db| {
            let mut update = SyncUpdate::load(db, name)?;
            for index in index_macs {
                update.change_mac(STANDARD.encode(index), None);
            }
            update.save(db, name)
        })
    }
    async fn clear_mutation_macs(&self, name: &str) -> Result<()> {
        self.write(|db| {
            let mut update = SyncUpdate::load(db, name)?;
            update.state.index_value_map.clear();
            update.mac_hash = [0; 128];
            update.save(db, name)
        })
    }
    async fn get_latest_sync_key_id(&self) -> Result<Option<Vec<u8>>> {
        self.read(|db| {
            latest_key(db)?
                .map(|id| STANDARD.decode(id).map_err(|_| invalid("sync key ID")))
                .transpose()
        })
    }
}
