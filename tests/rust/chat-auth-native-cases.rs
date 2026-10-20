//! Offline representability probe. This is a container codec into upstream types,
//! not a protocol implementation or an enabled authentication migration.
use super::auth_compat::{phase, rows, transfer};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_chat_bridge::whatsapp::auth::{AuthEntry, AuthStore};
use ivy_contracts::{
    IvyJson as Json,
    json::{canonical, parse},
};
use std::{fs, path::Path};
use wacore_libsignal::protocol::{
    IdentityKey, LegacyIndexedSessionV1, LegacySessionBaseKeyRoleV1, LegacySessionChainCounterV1,
    LegacySessionChainKeyV1, LegacySessionChainRoleV1, LegacySessionChainV1,
    LegacySessionDispositionV1, LegacySessionIndexV1, LegacySessionInteropError,
    LegacySessionKeyPairV1, LegacySessionLocalContext, LegacySessionMessageKeyV1,
    LegacySessionPendingPreKeyV1, LegacySessionRatchetV1, LegacySessionRecordV1,
    LegacySessionUnrepresentableFieldV1, LegacySessionV1, MessageKeyGenerator,
    PendingPreKeyComponents, PublicKey, SessionMessageKeyMaterial, SessionRecord,
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
/// credentials use BufferJSON. Unknown data stays in the untouched raw snapshot.
fn decode(value: &Json) -> Decoded<LegacySessionRecordV1> {
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

fn source(entries: &[AuthEntry]) -> (Json, LegacySessionLocalContext) {
    let session = entries.iter().find(|e| e.category == "session").unwrap();
    let creds = entries.iter().find(|e| e.category == "creds").unwrap();
    let creds = parse(&creds.value, usize::MAX).unwrap();
    let public =
        bytes(field(field(&creds, "signedIdentityKey").unwrap(), "public").unwrap()).unwrap();
    let identity_key = IdentityKey::new(PublicKey::from_djb_public_key_bytes(&public).unwrap());
    (
        parse(&session.value, usize::MAX).unwrap(),
        LegacySessionLocalContext {
            identity_key,
            registration_id: unsigned(field(&creds, "registrationId").unwrap()).unwrap(),
        },
    )
}
fn import(value: &Json, context: LegacySessionLocalContext) -> SessionRecord {
    decode(value)
        .expect("original v1 container")
        .into_session_record(context)
        .expect("native session import")
}
fn skipped(record: &LegacySessionRecordV1) -> usize {
    record
        .indexed_sessions()
        .iter()
        .flat_map(|e| &e.session.chains)
        .map(|c| c.message_keys.len())
        .sum()
}

fn probe(entries: &[AuthEntry], expected_skipped: usize) -> Json {
    let raw_before = canonical(&rows(entries), usize::MAX).unwrap();
    let (value, context) = source(entries);
    let original = decode(&value).unwrap();
    assert_eq!(original.indexed_sessions().len(), 2);
    assert_eq!(skipped(&original), expected_skipped);
    let native = import(&value, context);
    let serialized = native.serialize().unwrap();
    let projected = SessionRecord::deserialize(&serialized)
        .unwrap()
        .into_legacy_session_v1_operational()
        .unwrap();
    assert_eq!(projected.indexed_sessions().len(), 2);
    assert_eq!(skipped(&projected), expected_skipped);
    let mut metadata_changed = 0;
    for before in original.indexed_sessions() {
        let after = &projected
            .indexed_sessions()
            .iter()
            .find(|e| e.index_key == before.index_key)
            .unwrap()
            .session;
        let before = &before.session;
        assert!(
            before.registration_id == after.registration_id,
            "registration retained"
        );
        assert!(
            before.ratchet.key_pair.public == after.ratchet.key_pair.public,
            "public ratchet retained"
        );
        assert!(
            before.ratchet.key_pair.private == after.ratchet.key_pair.private,
            "private ratchet retained"
        );
        assert!(
            before.ratchet.root_key == after.ratchet.root_key,
            "root retained"
        );
        assert!(
            before.ratchet.last_remote_ephemeral_key == after.ratchet.last_remote_ephemeral_key,
            "remote ratchet retained"
        );
        assert!(
            before.ratchet.previous_counter == after.ratchet.previous_counter,
            "previous counter retained"
        );
        assert!(
            before.index.remote_identity_key == after.index.remote_identity_key,
            "remote identity retained"
        );
        assert_eq!(before.chains.len(), after.chains.len());
        for chain in &before.chains {
            let other = after
                .chains
                .iter()
                .find(|c| c.ratchet_key == chain.ratchet_key)
                .unwrap();
            assert!(
                chain.role == other.role
                    && chain.chain_key.counter == other.chain_key.counter
                    && chain.chain_key.key == other.chain_key.key,
                "chain state retained"
            );
            assert_eq!(chain.message_keys.len(), other.message_keys.len());
            for key in &chain.message_keys {
                assert!(
                    other
                        .message_keys
                        .iter()
                        .any(|k| k.index == key.index && k.seed == key.seed),
                    "skipped seed retained"
                );
            }
        }
        if before.index.used_at_ms != after.index.used_at_ms
            || before.index.created_at_ms != after.index.created_at_ms
            || before.index.disposition != after.index.disposition
            || before.index.base_key_role != after.index.base_key_role
        {
            metadata_changed += 1;
        }
    }
    assert_eq!(
        metadata_changed, 2,
        "Operational export normalizes both lifecycle records"
    );
    // No projected state is ever written back: keeping the full raw snapshot
    // is mandatory until a lossless envelope has been proved independently.
    assert!(
        canonical(&rows(entries), usize::MAX).unwrap() == raw_before,
        "Raw metadata and every category remain untouched"
    );
    Json::object([
        ("sessions", Json::Number(2.0)),
        ("skippedKeys", Json::Number(expected_skipped as f64)),
        ("protocolFieldsRetained", true.into()),
        (
            "lifecycleRecordsNormalized",
            Json::Number(metadata_changed as f64),
        ),
        ("losslessMigrationAllowed", false.into()),
        ("rawRows", rows(entries)),
        ("nativeRecord", STANDARD.encode(serialized).into()),
    ])
}

fn rejection_probes(entries: &[AuthEntry]) -> Json {
    let (value, context) = source(entries);
    let mut components = import(&value, context).into_components().unwrap();
    let key = components
        .current_session
        .as_mut()
        .unwrap()
        .receiver_chains
        .iter_mut()
        .flat_map(|c| &mut c.message_keys)
        .next()
        .unwrap();
    let SessionMessageKeyMaterial::Seed(seed) = key.material else {
        panic!("retained skipped seed")
    };
    let keys = MessageKeyGenerator::new_from_seed(&seed, key.index).generate_keys();
    key.material = SessionMessageKeyMaterial::Derived {
        cipher_key: *keys.cipher_key(),
        mac_key: *keys.mac_key(),
        iv: *keys.iv(),
    };
    let native = SessionRecord::from_components(components).unwrap();
    let persisted = native.serialize().unwrap();
    assert!(matches!(
        SessionRecord::deserialize(&persisted)
            .unwrap()
            .into_legacy_session_v1_operational(),
        Err(LegacySessionInteropError::ChainNotRepresentable {
            field: LegacySessionUnrepresentableFieldV1::DerivedMessageKey,
            ..
        })
    ));
    let mut components = import(&value, context).into_components().unwrap();
    let current = components.current_session.as_mut().unwrap();
    current.pending_pre_key = Some(PendingPreKeyComponents {
        pre_key_id: None,
        signed_pre_key_id: Some(1),
        base_key: current.alice_base_key.clone(),
        kyber_pre_key_id: Some(2),
        kyber_ciphertext: Some(vec![3; 32]),
    });
    assert!(matches!(
        SessionRecord::from_components(components)
            .unwrap()
            .into_legacy_session_v1_operational(),
        Err(LegacySessionInteropError::NotRepresentable {
            field: LegacySessionUnrepresentableFieldV1::PostQuantumPreKey,
            ..
        })
    ));
    let mut sessions = decode(&value).unwrap().into_indexed_sessions();
    let current = sessions
        .iter_mut()
        .find(|e| e.session.index.disposition == LegacySessionDispositionV1::Current)
        .unwrap();
    assert!(current.session.index.base_key_role == LegacySessionBaseKeyRoleV1::Remote);
    current.session.pending_pre_key = Some(LegacySessionPendingPreKeyV1 {
        pre_key_id: None,
        signed_pre_key_id: 1,
        base_key: current.index_key.clone(),
    });
    assert!(matches!(
        LegacySessionRecordV1::from_indexed_sessions(sessions)
            .unwrap()
            .into_session_record(context),
        Err(LegacySessionInteropError::InvalidPendingPreKeyRole { .. })
    ));
    let mut invalid = value.clone();
    invalid.insert("version", "future".into());
    assert!(decode(&invalid).is_err());
    Json::object([
        ("derivedSkippedKeysRejected", true.into()),
        ("postQuantumPreKeyRejected", true.into()),
        ("invalidClassicPreKeyRoleRejected", true.into()),
        ("unknownContainerVersionRejected", true.into()),
    ])
}

fn unchanged(path: &Path, expected: &[AuthEntry]) {
    let mut store = AuthStore::open(path).unwrap();
    assert!(
        canonical(&rows(&store.snapshot().unwrap()), usize::MAX).unwrap()
            == canonical(&rows(expected), usize::MAX).unwrap(),
        "Native inspection leaves the original encoded store untouched"
    );
    store.close().unwrap();
}

#[test]
fn nonempty_native_session_projection_preserves_protocol_and_rejects_lossy_migration() {
    let root = std::env::temp_dir().join(format!("ivy-rust-auth-native-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let seed = phase("seed", &root);
    let original = transfer(&root.join("original.sqlite"), &root.join("copied.sqlite"));
    let initial = probe(&original, 4);
    let rejected = rejection_probes(&original);
    unchanged(&root.join("original.sqlite"), &original);
    let evolved = phase("advance", &root);
    let advanced = transfer(&root.join("copied.sqlite"), &root.join("rollback.sqlite"));
    let advanced_projection = probe(&advanced, 3);
    let restored = phase("verify", &root);
    let mut store = AuthStore::open(root.join("rollback.sqlite")).unwrap();
    let final_rows = store.snapshot().unwrap();
    store.close().unwrap();
    let final_projection = probe(&final_rows, 2);
    unchanged(&root.join("original.sqlite"), &original);
    let capture = Json::object([
        ("synthetic", true.into()),
        ("seed", seed),
        ("evolved", evolved),
        ("restored", restored),
        ("initialProjection", initial),
        ("advancedProjection", advanced_projection),
        ("finalProjection", final_projection),
        ("rejections", rejected),
        ("nativeCryptoEvolutionExercised", false.into()),
        ("originalStoreUnchanged", true.into()),
        ("productionMigrationEnabled", false.into()),
    ]);
    if let Some(path) = std::env::var_os("IVY_CHAT_AUTH_NATIVE_CAPTURE") {
        fs::write(path, canonical(&capture, usize::MAX).unwrap()).unwrap();
    }
    assert_eq!(
        root.canonicalize().unwrap().parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    assert!(
        !fs::symlink_metadata(&root)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_dir_all(root).unwrap();
}
