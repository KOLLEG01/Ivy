use super::{auth_backend::*, auth_device::load_device, auth_signal};
use async_trait::async_trait;
use bytes::Bytes;
use ivy_contracts::IvyJson as Json;
use rusqlite::Connection;
use std::sync::Arc;
use wacore::{
    libsignal::protocol::{
        GenericSignedPreKey, IdentityKey, LegacySessionLocalContext, PreKeyRecord,
        SenderChainKeyComponents, SenderKeyRecord, SenderKeyRecordComponents,
        SenderKeyStateComponents, SenderMessageKeyComponents, SenderSigningKeyComponents,
        SessionRecord, SignedPreKeyRecord, Timestamp,
    },
    store::{error::Result, traits::SignalStore},
};

fn local(db: &Connection) -> Result<LegacySessionLocalContext> {
    let device = load_device(db)?.ok_or_else(|| invalid("missing credentials"))?;
    Ok(LegacySessionLocalContext {
        identity_key: IdentityKey::new(device.identity_key.public_key),
        registration_id: device.registration_id,
    })
}

fn binary_record(
    db: &Connection,
    category: &str,
    id: &str,
    decode: impl FnOnce(&Json) -> Result<Vec<u8>>,
) -> Result<Option<Vec<u8>>> {
    let Some(raw) = get_raw(db, category, id)? else {
        return Ok(None);
    };
    if let Some(saved) = get(db, &format!("native-{category}"), id)? {
        if text(field(&saved, "legacyHash")?)? == ivy_contracts::hash::digest(&raw) {
            return Ok(Some(bytes(field(&saved, "record")?)?));
        }
    }
    decode(&ivy_contracts::json::parse(&raw, usize::MAX).map_err(|_| invalid("key JSON"))?)
        .map(Some)
}
fn save_binary(
    db: &Connection,
    category: &str,
    id: &str,
    record: &[u8],
    value: Json,
) -> Result<()> {
    put(db, category, id, Some(&value))?;
    let legacy = get_raw(db, category, id)?.ok_or_else(|| invalid("missing key"))?;
    // Counter reservations are native durability metadata. Keep the original
    // binary as well as its legacy projection in this same transaction. The
    // projection burns reserved counters, so an older client cannot reuse them.
    put(
        db,
        &format!("native-{category}"),
        id,
        Some(&Json::object([
            ("legacyHash", ivy_contracts::hash::digest(&legacy).into()),
            ("record", buffer(record)),
        ])),
    )
}
fn delete_binary(db: &Connection, category: &str, id: &str) -> Result<()> {
    put(db, category, id, None)?;
    put(db, &format!("native-{category}"), id, None)
}
fn get_session(db: &Connection, address: &str) -> Result<Option<Bytes>> {
    let id = signal_id(address)?;
    binary_record(db, "session", &id, |value| {
        auth_signal::decode(value)
            .map_err(|_| invalid("session"))?
            .into_session_record(local(db)?)
            .map_err(protocol)?
            .serialize()
            .map_err(protocol)
    })
    .map(|v| v.map(Bytes::from))
}
fn put_session(db: &Connection, address: &str, bytes: &[u8]) -> Result<()> {
    let id = signal_id(address)?;
    // Deserializing without a process incarnation advances all reserved ranges
    // before exporting the ordinary v1 record for the original client.
    let record = SessionRecord::deserialize(bytes).map_err(protocol)?;
    let previous = get(db, "session", &id)?;
    if let Some(previous) = &previous {
        let stored = auth_signal::decode(previous)
            .map_err(|_| invalid("session"))?
            .into_session_record(local(db)?)
            .map_err(protocol)?;
        if stored.serialize().map_err(protocol)? == record.serialize().map_err(protocol)? {
            return Ok(());
        }
    }
    let value = auth_signal::encode(record, previous.as_ref(), now_ms()?)
        .map_err(|_| invalid("session projection"))?;
    save_binary(db, "session", &id, bytes, value)
}

fn sender_value(value: &Json) -> Result<Json> {
    let value = bytes(value)?;
    ivy_contracts::json::parse(
        std::str::from_utf8(&value).map_err(|_| invalid("sender text"))?,
        usize::MAX,
    )
    .map_err(|_| invalid("sender JSON"))
}
fn sender_decode(value: &Json) -> Result<SenderKeyRecord> {
    let value = sender_value(value)?;
    let mut states = Vec::new();
    // The old format is oldest first; native records are newest first.
    for v in value
        .as_array()
        .ok_or_else(|| invalid("sender states"))?
        .iter()
        .rev()
    {
        let chain = field(v, "senderChainKey")?;
        let signing = field(v, "senderSigningKey")?;
        let public = bytes(field(signing, "public")?)?;
        let public = if public.len() == 32 {
            std::iter::once(5).chain(public).collect()
        } else {
            public
        };
        let private = signing
            .get("private")
            .map(bytes)
            .transpose()?
            .filter(|v| !v.is_empty());
        states.push(SenderKeyStateComponents {
            key_id: uint(field(v, "senderKeyId")?)?,
            chain_key: SenderChainKeyComponents {
                iteration: uint(field(chain, "iteration")?)?,
                seed: bytes(field(chain, "seed")?)?,
            },
            signing_key: SenderSigningKeyComponents { public, private },
            message_keys: field(v, "senderMessageKeys")?
                .as_array()
                .ok_or_else(|| invalid("sender skipped keys"))?
                .iter()
                .map(|v| {
                    Ok(SenderMessageKeyComponents {
                        iteration: uint(field(v, "iteration")?)?,
                        seed: bytes(field(v, "seed")?)?,
                    })
                })
                .collect::<Result<_>>()?,
        });
    }
    // The library bounds this history; refuse excess data rather than silently
    // truncating a retained record during import.
    if states.len() > 5 {
        return Err(invalid("sender history"));
    }
    SenderKeyRecord::from_components(SenderKeyRecordComponents { states }).map_err(protocol)
}
fn sender_encode(record: SenderKeyRecord, old: Option<&Json>) -> Result<Json> {
    let states = record.into_components().map_err(protocol)?.states;
    let old = old
        .map(|v| v.as_array().ok_or_else(|| invalid("sender states")))
        .transpose()?
        .unwrap_or(&[]);
    let mut values = Vec::new();
    for state in states.into_iter().rev() {
        let previous = old
            .iter()
            .find(|v| v.get("senderKeyId").and_then(Json::as_f64) == Some(state.key_id as f64));
        let mut v = object(previous.cloned())?;
        v.insert("senderKeyId", number(state.key_id.into())?);
        let mut chain = object(previous.and_then(|v| v.get("senderChainKey")).cloned())?;
        chain.insert("iteration", number(state.chain_key.iteration.into())?);
        chain.insert("seed", buffer(&state.chain_key.seed));
        v.insert("senderChainKey", chain);
        let mut signing = object(previous.and_then(|v| v.get("senderSigningKey")).cloned())?;
        let old_public = signing.get("public").map(bytes).transpose()?;
        let public = if old_public.as_ref().is_some_and(|b| b.len() == 32)
            && state.signing_key.public.len() == 33
        {
            &state.signing_key.public[1..]
        } else {
            &state.signing_key.public
        };
        signing.insert("public", buffer(public));
        signing.insert(
            "private",
            buffer(state.signing_key.private.as_deref().unwrap_or(&[])),
        );
        v.insert("senderSigningKey", signing);
        v.insert(
            "senderMessageKeys",
            Json::Array(
                state
                    .message_keys
                    .into_iter()
                    .map(|k| {
                        let prior = previous
                            .and_then(|v| v.get("senderMessageKeys"))
                            .and_then(Json::as_array)
                            .and_then(|keys| {
                                keys.iter().find(|v| {
                                    v.get("iteration").and_then(Json::as_f64)
                                        == Some(k.iteration as f64)
                                })
                            });
                        let mut value = object(prior.cloned())?;
                        value.insert("iteration", number(k.iteration.into())?);
                        value.insert("seed", buffer(&k.seed));
                        Ok(value)
                    })
                    .collect::<Result<_>>()?,
            ),
        );
        values.push(v);
    }
    Ok(Json::Array(values))
}
fn put_sender(db: &Connection, address: &str, bytes: &[u8]) -> Result<()> {
    let id = sender_id(address)?;
    let raw = get(db, "sender-key", &id)?;
    let old = raw.as_ref().map(sender_value).transpose()?;
    let value = sender_encode(
        SenderKeyRecord::deserialize(bytes).map_err(protocol)?,
        old.as_ref(),
    )?;
    let encoded =
        ivy_contracts::json::stringify(&value, usize::MAX).map_err(|_| invalid("sender JSON"))?;
    let mut envelope = object(raw)?;
    envelope.insert("type", "Buffer".into());
    envelope.insert(
        "data",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            encoded.as_bytes(),
        )
        .into(),
    );
    save_binary(db, "sender-key", &id, bytes, envelope)
}
fn prekey(db: &Connection, id: u32, record: &[u8], uploaded: bool) -> Result<()> {
    let record = PreKeyRecord::deserialize(record).map_err(protocol)?;
    if u32::from(record.id().map_err(protocol)?) != id {
        return Err(invalid("pre-key ID"));
    }
    let pair = record.key_pair().map_err(protocol)?;
    let mut value = object(get(db, "pre-key", &id.to_string())?)?;
    value.insert("public", buffer(pair.public_key.public_key_bytes()));
    value.insert("private", buffer(pair.private_key.serialize().as_ref()));
    put(db, "pre-key", &id.to_string(), Some(&value))?;
    put(
        db,
        "native-pre-key-upload",
        &id.to_string(),
        Some(&uploaded.into()),
    )
}
fn load_prekey(db: &Connection, id: u32) -> Result<Option<Bytes>> {
    get(db, "pre-key", &id.to_string())?
        .map(|v| {
            let pair = super::auth_device::pair(&v)?;
            PreKeyRecord::new(id.into(), &pair)
                .serialize()
                .map(Bytes::from)
                .map_err(protocol)
        })
        .transpose()
}
fn signed(db: &Connection, id: u32) -> Result<Option<Vec<u8>>> {
    let saved = get(db, "native-signed-pre-key", &id.to_string())?
        .map(|v| bytes(&v))
        .transpose()?;
    let device = load_device(db)?;
    if let Some(d) = device.filter(|d| d.signed_pre_key_id == id) {
        if let Some(raw) = &saved {
            let record = SignedPreKeyRecord::deserialize(raw).map_err(protocol)?;
            if record.public_key().map_err(protocol)? == d.signed_pre_key.public_key
                && record.private_key().map_err(protocol)?.serialize()
                    == d.signed_pre_key.private_key.serialize()
                && record.signature().map_err(protocol)? == d.signed_pre_key_signature
            {
                return Ok(saved);
            }
        }
        return SignedPreKeyRecord::new(
            id.into(),
            Timestamp::from_epoch_millis(0),
            &d.signed_pre_key,
            &d.signed_pre_key_signature,
        )
        .serialize()
        .map(Some)
        .map_err(protocol);
    }
    Ok(saved)
}

#[async_trait]
impl SignalStore for NativeAuthStore {
    async fn get_session(&self, address: &str) -> Result<Option<Bytes>> {
        self.read(|db| get_session(db, address))
    }
    async fn put_session(&self, address: &str, session: &[u8]) -> Result<()> {
        self.write(|db| put_session(db, address, session))
    }
    async fn put_sessions_batch(&self, sessions: &[(Arc<str>, Bytes)]) -> Result<()> {
        self.write(|db| {
            for (address, session) in sessions {
                put_session(db, address, session)?
            }
            Ok(())
        })
    }
    async fn delete_session(&self, address: &str) -> Result<()> {
        self.write(|db| delete_binary(db, "session", &signal_id(address)?))
    }
    async fn load_identity(&self, address: &str) -> Result<Option<[u8; 32]>> {
        self.read(|db| {
            get(db, "identity-key", &signal_id(address)?)?
                .map(|v| {
                    let b = bytes(&v)?;
                    let b = if b.len() == 33 && b[0] == 5 {
                        &b[1..]
                    } else {
                        &b
                    };
                    b.try_into().map_err(|_| invalid("identity key"))
                })
                .transpose()
        })
    }
    async fn put_identity(&self, address: &str, value: [u8; 32]) -> Result<()> {
        self.put_identities_batch(&[(Arc::from(address), value)])
            .await
    }
    async fn put_identities_batch(&self, values: &[(Arc<str>, [u8; 32])]) -> Result<()> {
        self.write(|db| {
            for (address, value) in values {
                put(
                    db,
                    "identity-key",
                    &signal_id(address)?,
                    Some(&buffer(
                        &std::iter::once(5).chain(*value).collect::<Vec<_>>(),
                    )),
                )?
            }
            Ok(())
        })
    }
    async fn delete_identity(&self, address: &str) -> Result<()> {
        self.write(|db| put(db, "identity-key", &signal_id(address)?, None))
    }
    async fn store_prekey(&self, id: u32, record: &[u8], uploaded: bool) -> Result<()> {
        self.write(|db| prekey(db, id, record, uploaded))
    }
    async fn store_prekeys_batch(&self, keys: &[(u32, Bytes)], uploaded: bool) -> Result<()> {
        self.write(|db| {
            for (id, record) in keys {
                prekey(db, *id, record, uploaded)?
            }
            Ok(())
        })
    }
    async fn load_prekey(&self, id: u32) -> Result<Option<Bytes>> {
        self.read(|db| load_prekey(db, id))
    }
    async fn mark_prekeys_uploaded(&self, ids: &[u32]) -> Result<()> {
        self.write(|db| {
            for id in ids {
                if get_raw(db, "pre-key", &id.to_string())?.is_some() {
                    put(
                        db,
                        "native-pre-key-upload",
                        &id.to_string(),
                        Some(&true.into()),
                    )?
                }
            }
            Ok(())
        })
    }
    async fn remove_prekey(&self, id: u32) -> Result<()> {
        self.write(|db| {
            put(db, "pre-key", &id.to_string(), None)?;
            put(db, "native-pre-key-upload", &id.to_string(), None)
        })
    }
    async fn get_max_prekey_id(&self) -> Result<u32> {
        self.read(|db| {
            Ok(entries(db, "pre-key")?
                .iter()
                .filter_map(|(id, _)| id.parse().ok())
                .max()
                .unwrap_or(0))
        })
    }
    async fn load_signed_prekey(&self, id: u32) -> Result<Option<Vec<u8>>> {
        self.read(|db| signed(db, id))
    }
    async fn store_signed_prekey(&self, id: u32, record: &[u8]) -> Result<()> {
        self.write(|db| {
            let r = SignedPreKeyRecord::deserialize(record).map_err(protocol)?;
            if u32::from(r.id().map_err(protocol)?) != id {
                return Err(invalid("signed pre-key ID"));
            }
            // Rotation saves the Device separately; retain prior signed keys for
            // delayed pre-key envelopes in the meantime.
            put(
                db,
                "native-signed-pre-key",
                &id.to_string(),
                Some(&buffer(record)),
            )
        })
    }
    async fn load_all_signed_prekeys(&self) -> Result<Vec<(u32, Vec<u8>)>> {
        self.read(|db| {
            let mut values = entries(db, "native-signed-pre-key")?
                .into_iter()
                .map(|(id, v)| {
                    Ok((
                        id.parse().map_err(|_| invalid("signed pre-key ID"))?,
                        bytes(&v)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            if let Some(d) = load_device(db)? {
                values.retain(|(id, _)| *id != d.signed_pre_key_id);
                values.push((
                    d.signed_pre_key_id,
                    signed(db, d.signed_pre_key_id)?.ok_or_else(|| invalid("signed key"))?,
                ));
            }
            Ok(values)
        })
    }
    async fn remove_signed_prekey(&self, id: u32) -> Result<()> {
        self.write(|db| {
            if load_device(db)?.is_some_and(|d| d.signed_pre_key_id == id) {
                return Err(invalid("active signed pre-key deletion"));
            }
            put(db, "native-signed-pre-key", &id.to_string(), None)
        })
    }
    async fn get_sender_key(&self, address: &str) -> Result<Option<Vec<u8>>> {
        self.read(|db| {
            binary_record(db, "sender-key", &sender_id(address)?, |v| {
                sender_decode(v)?.serialize().map_err(protocol)
            })
        })
    }
    async fn put_sender_key(&self, address: &str, record: &[u8]) -> Result<()> {
        self.write(|db| put_sender(db, address, record))
    }
    async fn put_sender_keys_batch(&self, values: &[(Arc<str>, Bytes)]) -> Result<()> {
        self.write(|db| {
            for (address, record) in values {
                put_sender(db, address, record)?
            }
            Ok(())
        })
    }
    async fn delete_sender_key(&self, address: &str) -> Result<()> {
        self.write(|db| delete_binary(db, "sender-key", &sender_id(address)?))
    }
}
