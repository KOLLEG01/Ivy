//! Synthetic protocol evolution through the upstream API. Persistence adapters
//! below store its opaque serialized records; they implement no protocol logic.
use super::{
    bytes, decode, field, import, number, phase, rejection_probes, rows, source, unchanged,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_chat_bridge::whatsapp::auth::{AuthEntry, AuthStore};
use ivy_contracts::{
    IvyJson as Json,
    json::{canonical, parse},
};
use std::{fs, path::Path};
use wacore_libsignal::protocol::{
    CiphertextMessage, Direction, IdentityChange, IdentityKey, IdentityKeyPair, IdentityKeyStore,
    LegacySessionInteropError, LegacySessionUnrepresentableFieldV1, PrivateKey, ProtocolAddress,
    SessionRecord, SessionStore, SignalMessage, SignalProtocolError, message_decrypt_signal,
    message_encrypt,
};

type NativeResult<T> = Result<T, SignalProtocolError>;

#[derive(Clone)]
struct Sessions {
    address: ProtocolAddress,
    encoded: Vec<u8>,
    incarnation: [u8; 16],
}

#[async_trait::async_trait]
impl SessionStore for Sessions {
    async fn load_session(&self, address: &ProtocolAddress) -> NativeResult<Option<SessionRecord>> {
        assert!(address == &self.address, "Synthetic peer address");
        Ok(Some(SessionRecord::deserialize_for_store(
            &self.encoded,
            &self.incarnation,
        )?))
    }

    async fn has_session(&self, address: &ProtocolAddress) -> NativeResult<bool> {
        Ok(address == &self.address)
    }

    async fn store_session(
        &mut self,
        address: &ProtocolAddress,
        record: SessionRecord,
    ) -> NativeResult<()> {
        assert!(address == &self.address, "Synthetic peer address");
        record.serialize_into_for_store(&mut self.encoded, &self.incarnation);
        Ok(())
    }
}

#[derive(Clone)]
struct Identity {
    pair: IdentityKeyPair,
    registration: u32,
    address: ProtocolAddress,
    remote: IdentityKey,
}

#[async_trait::async_trait]
impl IdentityKeyStore for Identity {
    async fn get_identity_key_pair(&self) -> NativeResult<IdentityKeyPair> {
        Ok(self.pair.clone())
    }
    async fn get_local_registration_id(&self) -> NativeResult<u32> {
        Ok(self.registration)
    }
    async fn save_identity(
        &mut self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
    ) -> NativeResult<IdentityChange> {
        assert!(
            address == &self.address && identity == &self.remote,
            "Pinned synthetic identity"
        );
        Ok(IdentityChange::from_changed(false))
    }
    async fn is_trusted_identity(
        &self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
        _direction: Direction,
    ) -> NativeResult<bool> {
        Ok(address == &self.address && identity == &self.remote)
    }
    async fn get_identity(&self, address: &ProtocolAddress) -> NativeResult<Option<IdentityKey>> {
        Ok((address == &self.address).then_some(self.remote))
    }
}

#[derive(Clone)]
struct Peer {
    sessions: Sessions,
    identity: Identity,
}

impl Peer {
    fn new(entries: &[AuthEntry], remote: &[AuthEntry], remote_name: &str) -> Self {
        let (container, context) = source(entries);
        let record = import(&container, context);
        assert_eq!(
            record.previous_session_count(),
            1,
            "Nonempty archived state"
        );
        let creds = entries.iter().find(|e| e.category == "creds").unwrap();
        let creds = parse(&creds.value, usize::MAX).unwrap();
        let private =
            bytes(field(field(&creds, "signedIdentityKey").unwrap(), "private").unwrap()).unwrap();
        let pair = IdentityKeyPair::new(
            context.identity_key,
            PrivateKey::deserialize(&private).unwrap(),
        );
        let address = ProtocolAddress::new(remote_name, 1_u32.into());
        Self {
            sessions: Sessions {
                address: address.clone(),
                encoded: record.serialize().unwrap(),
                incarnation: *uuid::Uuid::new_v4().as_bytes(),
            },
            identity: Identity {
                pair,
                registration: context.registration_id,
                address,
                remote: source(remote).1.identity_key,
            },
        }
    }

    async fn encrypt(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let message = message_encrypt(
            plaintext,
            &self.sessions.address.clone(),
            &mut self.sessions,
            &mut self.identity,
        )
        .await
        .unwrap();
        assert!(
            matches!(&message, CiphertextMessage::SignalMessage(_)),
            "Established Signal envelope"
        );
        message.serialize().to_vec()
    }

    async fn decrypt(&mut self, wire: &[u8]) -> NativeResult<Vec<u8>> {
        let message = SignalMessage::try_from(wire)?;
        let result = message_decrypt_signal(
            &message,
            &self.sessions.address.clone(),
            &mut self.sessions,
            &mut self.identity,
            &mut rand::rng(),
        )
        .await?;
        assert!(result.consumed_prekey_id.is_none());
        Ok(result.plaintext)
    }

    fn reopen(&mut self, checkpoint: &[u8]) -> Json {
        let record =
            SessionRecord::deserialize_for_store(checkpoint, &self.sessions.incarnation).unwrap();
        assert_eq!(
            record.previous_session_count(),
            1,
            "Current and archived sessions survive persistence"
        );
        let ceiling = record.reserved_sender_chain_index();
        // Component export conservatively burns reservations. Read the live
        // chain first so the recovery check cannot pass through that projection.
        let before_live = record
            .session_state()
            .unwrap()
            .get_sender_chain_key()
            .unwrap();
        let before = record.clone().into_components().unwrap();
        let mut encoded = Vec::new();
        record.serialize_into_for_store(&mut encoded, &self.sessions.incarnation);
        let clean =
            SessionRecord::deserialize_for_store(&encoded, &self.sessions.incarnation).unwrap();
        let clean_live = clean
            .session_state()
            .unwrap()
            .get_sender_chain_key()
            .unwrap();
        assert_eq!(
            clean_live.index(),
            before_live.index(),
            "Clean reload preserves the active sender counter"
        );
        assert!(
            clean_live.key() == before_live.key(),
            "Clean reload preserves the active sender key"
        );
        assert!(
            clean.into_components().unwrap() == before,
            "Clean reload preserves every protocol component"
        );

        self.sessions.incarnation = *uuid::Uuid::new_v4().as_bytes();
        let recovered =
            SessionRecord::deserialize_for_store(checkpoint, &self.sessions.incarnation).unwrap();
        let after_live = recovered
            .session_state()
            .unwrap()
            .get_sender_chain_key()
            .unwrap();
        let after = recovered.clone().into_components().unwrap();
        let after_key = after
            .current_session
            .as_ref()
            .unwrap()
            .sender_chain
            .as_ref()
            .unwrap()
            .chain_key
            .as_ref()
            .unwrap();
        let before_index = before_live.index();
        let after_index = after_live.index();
        assert_eq!(
            after_index,
            before_index.max(ceiling),
            "Untrusted recovery burns the reserved sender range"
        );
        if after_index > before_index {
            assert!(
                after_live.key() != before_live.key(),
                "Recovery advances the sender key"
            );
        }
        let mut expected = before.clone();
        expected
            .current_session
            .as_mut()
            .unwrap()
            .sender_chain
            .as_mut()
            .unwrap()
            .chain_key = Some(after_key.clone());
        assert!(
            after == expected,
            "Recovery preserves every other current/archived protocol component"
        );
        recovered.serialize_into_for_store(&mut self.sessions.encoded, &self.sessions.incarnation);
        Json::object([
            ("cleanReloadPreserved", true.into()),
            ("freshIncarnation", true.into()),
            ("senderIndexBefore", Json::Number(before_index as f64)),
            ("reservedCeiling", Json::Number(ceiling as f64)),
            ("senderIndexRecovered", Json::Number(after_index as f64)),
            (
                "activeSenderRangeAdvanced",
                (after_index > before_index).into(),
            ),
            ("allOtherProtocolFieldsPreserved", true.into()),
        ])
    }
}

fn snapshot(path: &Path) -> Vec<AuthEntry> {
    let mut store = AuthStore::open(path).unwrap();
    let rows = store.snapshot().unwrap();
    store.close().unwrap();
    rows
}

fn wire(value: &Json) -> Vec<u8> {
    assert_eq!(field(value, "type").unwrap().as_str().unwrap(), "msg");
    let bytes = STANDARD
        .decode(field(value, "ciphertext").unwrap().as_str().unwrap())
        .unwrap();
    let parsed = SignalMessage::try_from(bytes.as_slice()).unwrap();
    assert!(
        parsed.serialized() == bytes,
        "Exact original Signal wire bytes"
    );
    bytes
}

fn pending(value: &Json, category: &'static str, index: usize) -> Vec<u8> {
    wire(&field(value, category).unwrap().as_array().unwrap()[index])
}

/// Operational projection is not a lossless storage envelope. Keep the original
/// encoded rows and never write this projection into an authentication store.
fn export_gate(peer: &Peer, original: &[AuthEntry]) -> Json {
    let before = canonical(&rows(original), usize::MAX).unwrap();
    let projected =
        SessionRecord::deserialize_for_store(&peer.sessions.encoded, &peer.sessions.incarnation)
            .unwrap()
            .into_legacy_session_v1_operational();
    let reason = match projected {
        Ok(projected) => {
            let original = decode(&source(original).0).unwrap();
            assert_eq!(
                projected.indexed_sessions().len(),
                original.indexed_sessions().len()
            );
            let normalized = original
                .indexed_sessions()
                .iter()
                .filter(|entry| {
                    let after = &projected
                        .indexed_sessions()
                        .iter()
                        .find(|p| p.index_key == entry.index_key)
                        .unwrap()
                        .session
                        .index;
                    let before = &entry.session.index;
                    before.created_at_ms != after.created_at_ms
                        || before.used_at_ms != after.used_at_ms
                        || before.base_key_role != after.base_key_role
                        || before.disposition != after.disposition
                })
                .count();
            assert!(
                normalized > 0,
                "Lossy lifecycle metadata is explicitly refused"
            );
            "lifecycle-metadata-normalized"
        }
        Err(LegacySessionInteropError::ChainNotRepresentable {
            field: LegacySessionUnrepresentableFieldV1::DerivedMessageKey,
            ..
        }) => "derived-skipped-key-not-representable",
        Err(_) => panic!("Unexpected native export limitation"),
    };
    assert!(canonical(&rows(original), usize::MAX).unwrap() == before);
    Json::object([
        ("execution", "completed".into()),
        ("conditionMet", false.into()),
        ("reason", reason.into()),
        ("writebackAllowed", false.into()),
        ("rawRowsUntouched", true.into()),
    ])
}

#[tokio::test(flavor = "current_thread")]
async fn native_crypto_evolution_preserves_raw_state_and_rejects_lossy_writeback() {
    let root = std::env::temp_dir().join(format!("ivy-rust-auth-crypto-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let seed = phase("seed", &root);
    let bob_rows = snapshot(&root.join("original.sqlite"));
    let alice_rows = snapshot(&root.join("alice.sqlite"));
    let raw_bob = canonical(&rows(&bob_rows), usize::MAX).unwrap();
    assert_eq!(bob_rows.len(), 13);
    let categories = bob_rows
        .iter()
        .map(|r| r.category.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(categories.len(), 12);
    assert!(categories.contains("future-category"));
    // Validate this slice's own inputs and rejection boundary before crypto.
    let mut invalid = source(&bob_rows).0;
    invalid.insert("version", "future".into());
    assert!(decode(&invalid).is_err());
    let rejections = rejection_probes(&bob_rows);
    let wire_file = fs::read_to_string(root.join("wire.json")).unwrap();
    let original_wire = parse(&wire_file, usize::MAX).unwrap();
    let current0 = pending(&original_wire, "current", 0);
    let current1 = pending(&original_wire, "current", 1);
    let archived0 = pending(&original_wire, "archived", 0);
    let archived1 = pending(&original_wire, "archived", 1);
    let mut bob = Peer::new(&bob_rows, &alice_rows, "15555550101");
    let initial_gate = export_gate(&bob, &bob_rows);
    let original_current = phase("crypto-wire", &root);
    let mut alice = Peer::new(&alice_rows, &bob_rows, "15555550102");
    let native_wire = alice.encrypt(b"native-current-send").await;
    assert!(
        native_wire == wire(field(&original_current, "wire").unwrap()),
        "Native encryption matches original ciphertext byte for byte"
    );

    let before_failure = bob.sessions.encoded.clone();
    let mut corrupted = current0.clone();
    *corrupted.last_mut().unwrap() ^= 1;
    assert!(
        bob.decrypt(&corrupted).await.is_err(),
        "Modified MAC rejected"
    );
    assert!(
        bob.sessions.encoded == before_failure,
        "Failed decrypt preserves durable state"
    );
    assert_eq!(bob.decrypt(&current0).await.unwrap(), b"current-0");
    assert_eq!(
        bob.decrypt(&native_wire).await.unwrap(),
        b"native-current-send"
    );
    let native_reply = bob.encrypt(b"native-current-reply").await;
    fs::write(
        root.join("native-reply.json"),
        canonical(
            &Json::object([
                ("type", "msg".into()),
                ("ciphertext", STANDARD.encode(&native_reply).into()),
            ]),
            usize::MAX,
        )
        .unwrap(),
    )
    .unwrap();
    let checkpoint = bob.sessions.encoded.clone();
    fs::write(root.join("native-checkpoint.bin"), &checkpoint).unwrap();
    let original_followup = phase("crypto-reply", &root);
    let followup = wire(field(&original_followup, "wire").unwrap());
    assert_eq!(bob.decrypt(&followup).await.unwrap(), b"original-follow-up");
    let current_recovery = bob.reopen(&fs::read(root.join("native-checkpoint.bin")).unwrap());
    assert!(
        number(field(&current_recovery, "senderIndexRecovered").unwrap()).unwrap()
            > number(field(&current_recovery, "senderIndexBefore").unwrap()).unwrap(),
        "The current checkpoint fixture exercises an actual reserved sender gap"
    );
    assert_eq!(
        bob.decrypt(&followup).await.unwrap(),
        b"original-follow-up",
        "Rollback/reopen accepts the same original peer message"
    );
    let before_duplicate = bob.sessions.encoded.clone();
    assert!(
        bob.decrypt(&current0).await.is_err(),
        "Consumed skipped key cannot be replayed"
    );
    assert!(
        bob.sessions.encoded == before_duplicate,
        "Duplicate rejection preserves durable state"
    );
    assert_eq!(bob.decrypt(&current1).await.unwrap(), b"current-1");
    let current_gate = export_gate(&bob, &bob_rows);

    let mut archived_bob = Peer::new(&bob_rows, &alice_rows, "15555550101");
    let mut archived_alice = Peer::new(&alice_rows, &bob_rows, "15555550102");
    assert_eq!(
        archived_bob.decrypt(&archived0).await.unwrap(),
        b"archived-0"
    );
    let archived_reply = archived_bob.encrypt(b"native-archived-reply").await;
    assert_eq!(
        archived_alice.decrypt(&archived_reply).await.unwrap(),
        b"native-archived-reply"
    );
    let archived_ack = archived_alice.encrypt(b"native-archived-ack").await;
    assert_eq!(
        archived_bob.decrypt(&archived_ack).await.unwrap(),
        b"native-archived-ack"
    );
    let archived_checkpoint = archived_bob.sessions.encoded.clone();
    let archived_recovery = archived_bob.reopen(&archived_checkpoint);
    assert_eq!(
        archived_bob.decrypt(&archived1).await.unwrap(),
        b"archived-1"
    );
    assert_eq!(
        archived_bob.decrypt(&current0).await.unwrap(),
        b"current-0",
        "Archived evolution retains the newer session"
    );
    let archived_gate = export_gate(&archived_bob, &bob_rows);
    unchanged(&root.join("original.sqlite"), &bob_rows);
    unchanged(&root.join("alice.sqlite"), &alice_rows);
    assert!(canonical(&rows(&bob_rows), usize::MAX).unwrap() == raw_bob);
    assert_eq!(number(field(&seed, "skipped").unwrap()).unwrap(), 4);
    let capture = Json::object([
        ("synthetic", true.into()),
        ("seed", seed),
        ("rawRows", rows(&bob_rows)),
        ("nativeCryptoEvolutionExercised", true.into()),
        ("systemCsprng", true.into()),
        ("originalCurrentCiphertext", original_current),
        (
            "nativeCurrentCiphertext",
            STANDARD.encode(native_wire).into(),
        ),
        (
            "nativeReplyCiphertext",
            STANDARD.encode(native_reply).into(),
        ),
        ("originalFollowup", original_followup),
        (
            "nativeArchivedReply",
            STANDARD.encode(archived_reply).into(),
        ),
        ("nativeArchivedAck", STANDARD.encode(archived_ack).into()),
        ("checkpoint", STANDARD.encode(checkpoint).into()),
        (
            "finalNativeRecord",
            STANDARD.encode(&bob.sessions.encoded).into(),
        ),
        (
            "archivedCheckpoint",
            STANDARD.encode(archived_checkpoint).into(),
        ),
        (
            "finalArchivedRecord",
            STANDARD.encode(&archived_bob.sessions.encoded).into(),
        ),
        ("exactOriginalWireParity", true.into()),
        ("originalPeerDecryptsNativeReply", true.into()),
        ("nativeDecryptsOriginalFollowup", true.into()),
        ("rollbackReopenQualified", true.into()),
        ("currentRecovery", current_recovery),
        ("archivedRecovery", archived_recovery),
        ("failedMacAndReplayPreserveState", true.into()),
        ("archivedBidirectionalEvolution", true.into()),
        ("newerSessionRetained", true.into()),
        ("rejections", rejections),
        ("initialExport", initial_gate),
        ("currentExport", current_gate),
        ("archivedExport", archived_gate),
        ("originalStoresUnchanged", true.into()),
        ("productionMigrationEnabled", false.into()),
    ]);
    if let Some(path) = std::env::var_os("IVY_CHAT_AUTH_CRYPTO_CAPTURE") {
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
