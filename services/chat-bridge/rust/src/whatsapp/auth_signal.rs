//! Existing v1 Signal session JSON mapped through the native protocol library.
//! Unknown fields are retained when an existing session advances.
use super::auth::{AuthStore, set};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult,
    json::{parse, stringify},
};
use rusqlite::{OptionalExtension, TransactionBehavior};
use wacore_libsignal::protocol::{
    LegacyIndexedSessionV1, LegacySessionBaseKeyRoleV1, LegacySessionChainCounterV1,
    LegacySessionChainKeyV1, LegacySessionChainRoleV1, LegacySessionChainV1,
    LegacySessionDispositionV1, LegacySessionIndexV1, LegacySessionKeyPairV1,
    LegacySessionLocalContext, LegacySessionMessageKeyV1, LegacySessionPendingPreKeyV1,
    LegacySessionRatchetV1, LegacySessionRecordV1, LegacySessionV1, SessionRecord,
};
type Decoded<T> = Result<T, &'static str>;
fn field<'a>(value: &'a Json, name: &'static str) -> Decoded<&'a Json> {
    value.get(name).ok_or(name)
}
fn number(value: &Json) -> Decoded<i64> {
    value
        .as_f64()
        .filter(|v| v.is_finite() && v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0)
        .map(|v| v as i64)
        .ok_or("integer")
}
fn unsigned(value: &Json) -> Decoded<u32> {
    u32::try_from(number(value)?).map_err(|_| "u32")
}
fn bytes(value: &Json) -> Decoded<Vec<u8>> {
    let text = match value {
        Json::String(_) => value.as_str().map_err(|_| "base64")?,
        Json::Object(_) if field(value, "type")?.as_str().ok().as_deref() == Some("Buffer") => {
            field(value, "data")?
                .as_str()
                .map_err(|_| "BufferJSON data")?
        }
        _ => return Err("encoded bytes"),
    };
    STANDARD.decode(text).map_err(|_| "base64")
}
fn object(value: &Json) -> Decoded<&[(ivy_contracts::json::JsString, Json)]> {
    value.as_object().ok_or("object")
}

/// The original v1 serializer stores a JSON object with base64 strings, while
/// credentials use BufferJSON. Decoding never modifies the stored value.
pub fn decode(value: &Json) -> Decoded<LegacySessionRecordV1> {
    if field(value, "version")?.as_str().ok().as_deref() != Some("v1") {
        return Err("session version");
    }
    let mut sessions = Vec::new();
    for (key, value) in object(field(value, "_sessions")?)? {
        let ratchet = field(value, "currentRatchet")?;
        let pair = field(ratchet, "ephemeralKeyPair")?;
        let index = field(value, "indexInfo")?;
        let mut chains = Vec::new();
        for (key, chain) in object(field(value, "_chains")?)? {
            let chain_key = field(chain, "chainKey")?;
            let mut message_keys = Vec::new();
            for (position, seed) in object(field(chain, "messageKeys")?)? {
                message_keys.push(LegacySessionMessageKeyV1 {
                    index: position
                        .to_utf8()
                        .map_err(|_| "message-key index")?
                        .parse()
                        .map_err(|_| "message-key index")?,
                    seed: bytes(seed)?.into(),
                });
            }
            chains.push(LegacySessionChainV1 {
                ratchet_key: STANDARD
                    .decode(key.to_utf8().map_err(|_| "chain index")?)
                    .map_err(|_| "chain index")?
                    .into(),
                role: LegacySessionChainRoleV1::try_from(unsigned(field(chain, "chainType")?)?)
                    .map_err(|_| "chain role")?,
                chain_key: LegacySessionChainKeyV1 {
                    counter: LegacySessionChainCounterV1::new(number(field(
                        chain_key, "counter",
                    )?)?)
                    .map_err(|_| "chain counter")?,
                    key: chain_key
                        .get("key")
                        .filter(|v| !v.is_null())
                        .map(bytes)
                        .transpose()?
                        .map(Into::into),
                },
                message_keys,
            });
        }
        let pending_pre_key = value
            .get("pendingPreKey")
            .filter(|v| !v.is_null())
            .map(|pending| -> Decoded<_> {
                Ok(LegacySessionPendingPreKeyV1 {
                    pre_key_id: pending
                        .get("preKeyId")
                        .filter(|v| !v.is_null())
                        .map(unsigned)
                        .transpose()?,
                    signed_pre_key_id: unsigned(field(pending, "signedKeyId")?)?,
                    base_key: bytes(field(pending, "baseKey")?)?.into(),
                })
            })
            .transpose()?;
        sessions.push(LegacyIndexedSessionV1 {
            index_key: STANDARD
                .decode(key.to_utf8().map_err(|_| "session index")?)
                .map_err(|_| "session index")?
                .into(),
            session: LegacySessionV1 {
                registration_id: unsigned(field(value, "registrationId")?)?,
                ratchet: LegacySessionRatchetV1 {
                    key_pair: LegacySessionKeyPairV1 {
                        public: bytes(field(pair, "pubKey")?)?.into(),
                        private: bytes(field(pair, "privKey")?)?.into(),
                    },
                    last_remote_ephemeral_key: bytes(field(ratchet, "lastRemoteEphemeralKey")?)?
                        .into(),
                    previous_counter: number(field(ratchet, "previousCounter")?)?,
                    root_key: bytes(field(ratchet, "rootKey")?)?.into(),
                },
                index: LegacySessionIndexV1 {
                    base_key: bytes(field(index, "baseKey")?)?.into(),
                    base_key_role: LegacySessionBaseKeyRoleV1::try_from(unsigned(field(
                        index,
                        "baseKeyType",
                    )?)?)
                    .map_err(|_| "base-key role")?,
                    disposition: LegacySessionDispositionV1::from_closed_timestamp(number(field(
                        index, "closed",
                    )?)?)
                    .map_err(|_| "closed timestamp")?,
                    used_at_ms: u64::try_from(number(field(index, "used")?)?)
                        .map_err(|_| "used timestamp")?,
                    created_at_ms: u64::try_from(number(field(index, "created")?)?)
                        .map_err(|_| "created timestamp")?,
                    remote_identity_key: bytes(field(index, "remoteIdentityKey")?)?.into(),
                },
                chains,
                pending_pre_key,
            },
        });
    }
    LegacySessionRecordV1::from_indexed_sessions(sessions).map_err(|_| "indexed sessions")
}

fn retained(value: Option<&Json>) -> Json {
    value
        .filter(|v| v.as_object().is_some())
        .cloned()
        .unwrap_or_else(|| Json::Object(Vec::new()))
}
fn encoded(bytes: &[u8]) -> Json {
    STANDARD.encode(bytes).into()
}
fn integer(value: i64) -> Json {
    Json::Number(value as f64)
}

/// Write the established v1 format while preserving extension fields and the
/// original creation/base-key metadata. Protocol state comes only from the
/// native library, including skipped-message seeds and archived sessions.
pub fn encode(record: SessionRecord, previous: Option<&Json>, now_ms: i64) -> Decoded<Json> {
    if !(0..=9_007_199_254_740_991).contains(&now_ms) {
        return Err("timestamp");
    }
    if let Some(previous) = previous {
        decode(previous)?;
    }
    let projected = record
        .into_legacy_session_v1_operational()
        .map_err(|_| "session projection")?;
    let mut result = retained(previous);
    let old_sessions = previous.and_then(|v| v.get("_sessions"));
    let mut sessions = Json::Object(Vec::new());
    for entry in projected.into_indexed_sessions() {
        let key = STANDARD.encode(&entry.index_key);
        let state = entry.session;
        let old = old_sessions.and_then(|v| v.get(&key));
        let mut session = retained(old);
        session.insert("registrationId", integer(i64::from(state.registration_id)));
        let old_ratchet = old.and_then(|v| v.get("currentRatchet"));
        let mut ratchet = retained(old_ratchet);
        let mut pair = retained(old_ratchet.and_then(|v| v.get("ephemeralKeyPair")));
        pair.insert("pubKey", encoded(&state.ratchet.key_pair.public));
        pair.insert("privKey", encoded(&state.ratchet.key_pair.private));
        ratchet.insert("ephemeralKeyPair", pair);
        ratchet.insert(
            "lastRemoteEphemeralKey",
            encoded(&state.ratchet.last_remote_ephemeral_key),
        );
        ratchet.insert("previousCounter", integer(state.ratchet.previous_counter));
        ratchet.insert("rootKey", encoded(&state.ratchet.root_key));
        session.insert("currentRatchet", ratchet);

        let old_index = old.and_then(|v| v.get("indexInfo"));
        let mut index = retained(old_index);
        index.insert("baseKey", encoded(&state.index.base_key));
        if index.get("baseKeyType").is_none() {
            index.insert(
                "baseKeyType",
                integer(match state.index.base_key_role {
                    LegacySessionBaseKeyRoleV1::Local => 1,
                    LegacySessionBaseKeyRoleV1::Remote => 2,
                }),
            );
        }
        index.insert(
            "remoteIdentityKey",
            encoded(&state.index.remote_identity_key),
        );
        if index.get("created").is_none() {
            index.insert("created", integer(now_ms));
        }
        if state.index.disposition == LegacySessionDispositionV1::Current {
            index.insert("closed", integer(-1));
            index.insert("used", integer(now_ms));
        } else {
            if index
                .get("closed")
                .and_then(Json::as_f64)
                .is_none_or(|v| v < 0.0)
            {
                index.insert("closed", integer(now_ms));
            }
            if index.get("used").is_none() {
                index.insert("used", integer(now_ms));
            }
        }
        session.insert("indexInfo", index);

        let mut chains = Json::Object(Vec::new());
        let old_chains = old.and_then(|v| v.get("_chains"));
        for chain in state.chains {
            let key = STANDARD.encode(&chain.ratchet_key);
            let old = old_chains.and_then(|v| v.get(&key));
            let mut value = retained(old);
            let mut chain_key = retained(old.and_then(|v| v.get("chainKey")));
            chain_key.insert("counter", integer(chain.chain_key.counter.value()));
            if let Some(key) = chain.chain_key.key {
                chain_key.insert("key", encoded(&key));
            } else {
                chain_key.remove("key");
            }
            value.insert("chainKey", chain_key);
            value.insert(
                "chainType",
                integer(match chain.role {
                    LegacySessionChainRoleV1::Sending => 1,
                    LegacySessionChainRoleV1::Receiving => 2,
                }),
            );
            value.insert(
                "messageKeys",
                Json::object(
                    chain
                        .message_keys
                        .into_iter()
                        .map(|key| (key.index.to_string(), encoded(&key.seed))),
                ),
            );
            chains.insert(key, value);
        }
        session.insert("_chains", chains);
        if let Some(pending) = state.pending_pre_key {
            let mut value = retained(old.and_then(|v| v.get("pendingPreKey")));
            value.insert("baseKey", encoded(&pending.base_key));
            value.insert("signedKeyId", integer(i64::from(pending.signed_pre_key_id)));
            if let Some(id) = pending.pre_key_id {
                value.insert("preKeyId", integer(i64::from(id)));
            } else {
                value.remove("preKeyId");
            }
            session.insert("pendingPreKey", value);
        } else {
            session.remove("pendingPreKey");
        }
        sessions.insert(key, session);
    }
    result.insert("version", "v1".into());
    result.insert("_sessions", sessions);
    // Reject any projected state that the reader cannot reopen before writing.
    decode(&result)?;
    Ok(result)
}

impl AuthStore {
    /// Load an existing session directly into the native protocol library.
    pub fn load_signal_session(
        &self,
        id: &str,
        local: LegacySessionLocalContext,
    ) -> IvyResult<Option<SessionRecord>> {
        self.get("session", id)
            .map_err(|_| signal_storage())?
            .map(|value| {
                let value = parse(&value, usize::MAX).map_err(|_| signal_invalid())?;
                decode(&value)
                    .map_err(|_| signal_invalid())?
                    .into_session_record(local)
                    .map(|mut record| {
                        // This adapter commits every state change synchronously
                        // before the caller may release the resulting message.
                        record.waive_counter_lease();
                        record
                    })
                    .map_err(|_| signal_invalid())
            })
            .transpose()
    }

    /// Commit advanced protocol state in the existing JSON row before releasing
    /// a ciphertext or acknowledging a decrypted message to the caller.
    pub fn save_signal_session(
        &mut self,
        id: &str,
        record: SessionRecord,
        local: LegacySessionLocalContext,
        now_ms: i64,
    ) -> IvyResult<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| signal_storage())?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT value FROM auth WHERE category='session' AND id=?",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| signal_storage())?;
        let previous = previous
            .map(|value| parse(&value, usize::MAX).map_err(|_| signal_invalid()))
            .transpose()?;
        if let Some(previous) = &previous {
            let stored = decode(previous)
                .map_err(|_| signal_invalid())?
                .into_session_record(local)
                .map_err(|_| signal_invalid())?;
            // The protocol library also commits an unchanged checkout after a
            // failed decrypt. Preserve the exact row, including timestamps and
            // extension fields, instead of recording a successful session use.
            if stored.serialize().map_err(|_| signal_invalid())?
                == record.serialize().map_err(|_| signal_invalid())?
            {
                return tx.commit().map_err(|_| signal_storage());
            }
        }
        let value = encode(record, previous.as_ref(), now_ms).map_err(|_| signal_invalid())?;
        let value = stringify(&value, usize::MAX)?;
        set(&tx, "session", id, Some(&value)).map_err(|_| signal_storage())?;
        tx.commit().map_err(|_| signal_storage())
    }
}

fn signal_storage() -> IvyError {
    IvyError::new(
        "storage_unavailable",
        "Authentication storage did not confirm completion.",
    )
}
fn signal_invalid() -> IvyError {
    IvyError::new(
        "auth_session_invalid",
        "Authentication session cannot be represented in the existing format.",
    )
}
