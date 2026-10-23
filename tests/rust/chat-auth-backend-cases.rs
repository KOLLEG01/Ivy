//! Real native Backend calls against locally generated original-client stores.
use super::{
    Fixture,
    auth_compat::{phase, rows, transfer},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_chat_bridge::whatsapp::{
    auth::{AuthAccount, AuthStore},
    auth_backend::NativeAuthStore,
};
use ivy_contracts::{
    IvyJson as Json,
    json::{canonical, parse},
};
use std::{path::Path, sync::Arc};
use wacore::{
    appstate::processor::AppStateMutationMAC,
    libsignal::protocol::{
        Direction, IdentityChange, IdentityKey, IdentityKeyPair, IdentityKeyStore, PreKeyRecord,
        ProtocolAddress, SessionRecord, SessionStore, SignalMessage, SignalProtocolError,
        message_decrypt_signal, message_encrypt,
    },
    store::{Device, traits::*},
};

fn open(root: &Path, name: &str) -> NativeAuthStore {
    NativeAuthStore::new(
        AuthAccount::acquire(&root.join(name), &root.join("backend-owner.sqlite")).unwrap(),
    )
}
fn snapshot(path: &Path) -> String {
    canonical(
        &rows(&AuthStore::open(path).unwrap().snapshot().unwrap()),
        usize::MAX,
    )
    .unwrap()
}
fn bytes(v: &Json) -> Vec<u8> {
    STANDARD
        .decode(v.get("data").unwrap_or(v).as_str().unwrap())
        .unwrap()
}
fn group_value(store: &AuthStore, id: &str) -> Json {
    let raw = parse(&store.get("sender-key", id).unwrap().unwrap(), usize::MAX).unwrap();
    parse(std::str::from_utf8(&bytes(&raw)).unwrap(), usize::MAX).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn native_backend_app_sync_reopens_only_complete_patch_and_snapshot_checkpoints() {
    use wacore::appstate::{WAPATCH_INTEGRITY, hash::HashState};
    fn mac(id: u8) -> AppStateMutationMAC {
        AppStateMutationMAC {
            index_mac: vec![id; 32],
            value_mac: vec![id + 10; 32],
        }
    }
    fn version(n: u64, ids: &[u8]) -> HashState {
        let mut state = HashState {
            version: n,
            ..Default::default()
        };
        let values: Vec<_> = ids.iter().map(|id| mac(*id).value_mac).collect();
        WAPATCH_INTEGRITY.subtract_then_add_in_place(&mut state.hash, &[] as &[Vec<u8>], &values);
        state
    }
    fn encoded(path: &Path) -> Option<String> {
        AuthStore::open(path)
            .unwrap()
            .get("app-state-sync-version", "regular")
            .unwrap()
    }
    let fixture = Fixture::new();
    for snapshot in [false, true] {
        for stop in 1..=3 {
            let name = format!("sync-{snapshot}-{stop}.sqlite");
            let path = fixture.0.join(&name);
            let backend = open(&fixture.0, &name);
            backend.get_version("regular").await.unwrap();
            backend
                .put_mutation_macs("regular", 1, &[mac(1), mac(2)])
                .await
                .unwrap();
            assert!(
                encoded(&path).is_none(),
                "Snapshot MACs do not publish before their version"
            );
            backend
                .set_version("regular", version(1, &[1, 2]))
                .await
                .unwrap();
            let store = AuthStore::open(&path).unwrap();
            let mut original = parse(&encoded(&path).unwrap(), usize::MAX).unwrap();
            original.insert("extension", Json::from("retained"));
            store
                .set(
                    "app-state-sync-version",
                    "regular",
                    Some(&ivy_contracts::json::stringify(&original, usize::MAX).unwrap()),
                )
                .unwrap();
            drop(store);
            let before = encoded(&path).unwrap();
            let mut unchanged = backend.get_version("regular").await.unwrap();
            unchanged.index_value_map.clear();
            backend.set_version("regular", unchanged).await.unwrap();
            assert_eq!(
                encoded(&path).as_deref(),
                Some(before.as_str()),
                "A version-only save preserves raw multi-index ordering"
            );
            if snapshot {
                backend.clear_mutation_macs("regular").await.unwrap();
                if stop >= 2 {
                    backend
                        .put_mutation_macs("regular", 2, &[mac(3)])
                        .await
                        .unwrap();
                }
                if stop == 3 {
                    backend
                        .set_version("regular", version(2, &[3]))
                        .await
                        .unwrap();
                }
            } else {
                backend
                    .set_version("regular", version(2, &[2, 3]))
                    .await
                    .unwrap();
                if stop >= 2 {
                    backend
                        .delete_mutation_macs("regular", &[vec![1; 32]])
                        .await
                        .unwrap();
                }
                if stop == 3 {
                    backend
                        .put_mutation_macs("regular", 2, &[mac(3)])
                        .await
                        .unwrap();
                }
            }
            if stop < 3 {
                assert_eq!(encoded(&path).as_deref(), Some(before.as_str()));
            }
            drop(backend); // Abrupt owner loss: no completion callback or cleanup.
            let backend = open(&fixture.0, &name);
            let recovered = backend.get_version("regular").await.unwrap();
            assert_eq!(recovered.version, if stop < 3 { 1 } else { 2 });
            assert_eq!(
                recovered.hash,
                if stop < 3 {
                    version(1, &[1, 2]).hash
                } else if snapshot {
                    version(2, &[3]).hash
                } else {
                    version(2, &[2, 3]).hash
                }
            );
            assert_eq!(
                backend
                    .get_mutation_mac("regular", &[1; 32])
                    .await
                    .unwrap()
                    .is_some(),
                stop < 3
            );
            assert_eq!(
                backend
                    .get_mutation_mac("regular", &[3; 32])
                    .await
                    .unwrap()
                    .is_some(),
                stop == 3
            );
            assert_eq!(
                parse(&encoded(&path).unwrap(), usize::MAX)
                    .unwrap()
                    .get("extension"),
                Some(&Json::from("retained"))
            );
            let subset = backend
                .get_mutation_macs("regular", &[[1; 32], [3; 32], [99; 32]])
                .await
                .unwrap();
            assert_eq!(subset.len(), 1);
            // Retrying an interrupted cycle on the same owner also starts from
            // its complete checkpoint, and an explicit reset clears its MACs.
            backend
                .set_version("regular", version(3, &[8]))
                .await
                .unwrap();
            assert_eq!(
                backend.get_version("regular").await.unwrap().version,
                recovered.version
            );
            backend
                .set_version("regular", HashState::default())
                .await
                .unwrap();
            let reset = backend.get_version("regular").await.unwrap();
            assert_eq!(reset.version, 0);
            assert!(reset.index_value_map.is_empty());
        }
    }
    let backend = open(&fixture.0, "sync-failure.sqlite");
    let path = fixture.0.join("sync-failure.sqlite");
    backend
        .put_mutation_macs("regular", 1, &[mac(1)])
        .await
        .unwrap();
    backend
        .set_version("regular", version(1, &[1]))
        .await
        .unwrap();
    let before = encoded(&path).unwrap();
    backend
        .set_version("regular", version(2, &[2]))
        .await
        .unwrap();
    backend
        .delete_mutation_macs("regular", &[vec![1; 32]])
        .await
        .unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_sync BEFORE UPDATE OF value ON auth WHEN NEW.category='app-state-sync-version' BEGIN SELECT RAISE(ABORT,'injected sync checkpoint failure'); END;").unwrap();
    assert!(
        backend
            .put_mutation_macs("regular", 2, &[mac(2)])
            .await
            .is_err()
    );
    assert_eq!(encoded(&path).as_deref(), Some(before.as_str()));
    assert_eq!(backend.get_version("regular").await.unwrap().version, 1);
    db.execute_batch("DROP TRIGGER fail_sync").unwrap();
    backend
        .set_version("regular", version(2, &[2]))
        .await
        .unwrap();
    backend
        .delete_mutation_macs("regular", &[vec![1; 32]])
        .await
        .unwrap();
    backend
        .put_mutation_macs("regular", 2, &[mac(2)])
        .await
        .unwrap();
    assert_eq!(backend.get_version("regular").await.unwrap().version, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn native_backend_app_sync_runs_authenticated_native_processor_and_recovers_failed_patch() {
    use std::{future::Future, pin::Pin, time::Duration};
    use wacore::{
        appstate::{
            encode_record, expand_app_state_keys,
            hash::{HashState, generate_patch_mac},
            patch_decode::{PatchList, WAPatchName},
        },
        appstate_sync::AppStateProcessor,
        runtime::{AbortHandle, Runtime},
    };
    use waproto::whatsapp as wa;
    struct TestRuntime;
    #[async_trait::async_trait]
    impl Runtime for TestRuntime {
        fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> AbortHandle {
            let task = tokio::spawn(future);
            AbortHandle::new(move || task.abort())
        }
        fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(tokio::time::sleep(duration))
        }
        fn spawn_blocking(
            &self,
            action: Box<dyn FnOnce() + Send + 'static>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(async move {
                tokio::task::spawn_blocking(action).await.unwrap();
            })
        }
        fn yield_now(&self) -> Option<Pin<Box<dyn Future<Output = ()> + Send>>> {
            None
        }
    }
    fn list(snapshot: Option<wa::SyncdSnapshot>, patches: Vec<wa::SyncdPatch>) -> PatchList {
        PatchList {
            name: WAPatchName::Regular,
            has_more_patches: false,
            snapshot,
            patches,
            snapshot_ref: None,
            error: None,
        }
    }
    let fixture = Fixture::new();
    let path = fixture.0.join("processor.sqlite");
    let backend = Arc::new(open(&fixture.0, "processor.sqlite"));
    let master = [47; 32];
    let key_id = [6; 32];
    backend
        .set_sync_key(
            &key_id,
            AppStateSyncKey {
                key_data: master.to_vec(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let keys = expand_app_state_keys(&master);
    let action = wa::SyncActionValue {
        timestamp: Some(1_730_000_000),
        push_name_setting: wa::sync_action_value::PushNameSetting {
            name: Some("Synthetic native sync".into()),
        }
        .into(),
        ..Default::default()
    };
    let (first, _) = encode_record(
        wa::syncd_mutation::SyncdOperation::SET,
        br#"["setting_pushName"]"#,
        &action,
        &keys,
        &key_id,
        &[1; 16],
        1,
    );
    let records = vec![first.record.as_option().unwrap().clone()];
    let mut expected = HashState {
        version: 1,
        ..Default::default()
    };
    expected.update_hash_from_records(&records);
    let snapshot = wa::SyncdSnapshot {
        version: wa::SyncdVersion { version: Some(1) }.into(),
        records,
        mac: Some(expected.generate_snapshot_mac("regular", &keys.snapshot_mac)),
        key_id: wa::KeyId {
            id: Some(key_id.to_vec()),
        }
        .into(),
        ..Default::default()
    };
    let processor = AppStateProcessor::new(backend.clone(), Arc::new(TestRuntime));
    let (decoded, applied, _) = processor
        .process_parsed_patch_list(
            list(Some(snapshot), vec![]),
            &|_| unreachable!("No network or external blobs"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(decoded.len(), 1);
    assert_eq!(applied.hash, expected.hash);
    assert_eq!(backend.get_version("regular").await.unwrap().version, 1);
    let before = snapshot_value(&path);
    let (remove, _) = encode_record(
        wa::syncd_mutation::SyncdOperation::REMOVE,
        br#"["setting_pushName"]"#,
        &action,
        &keys,
        &key_id,
        &[2; 16],
        1,
    );
    let (second, _) = encode_record(
        wa::syncd_mutation::SyncdOperation::SET,
        br#"["setting_pushName"]"#,
        &action,
        &keys,
        &key_id,
        &[3; 16],
        1,
    );
    let old_index = first.record.index.blob.as_ref().unwrap();
    let old_mac = backend
        .get_mutation_mac("regular", old_index)
        .await
        .unwrap();
    let mutations = vec![remove, second];
    let (_, hash_result) = expected.update_hash(&mutations, |_, _| Ok(old_mac.clone()));
    hash_result.unwrap();
    expected.version = 2;
    let mut patch = wa::SyncdPatch {
        version: wa::SyncdVersion { version: Some(2) }.into(),
        mutations,
        snapshot_mac: Some(expected.generate_snapshot_mac("regular", &keys.snapshot_mac)),
        key_id: wa::KeyId {
            id: Some(key_id.to_vec()),
        }
        .into(),
        ..Default::default()
    };
    patch.patch_mac = Some(generate_patch_mac(&patch, "regular", &keys.patch_mac, 2));
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_sync BEFORE UPDATE OF value ON auth WHEN NEW.category='app-state-sync-version' BEGIN SELECT RAISE(ABORT,'injected processor checkpoint failure'); END;").unwrap();
    assert!(
        processor
            .process_parsed_patch_list(list(None, vec![patch.clone()]), &|_| unreachable!(), true)
            .await
            .is_err()
    );
    assert_eq!(snapshot_value(&path), before);
    drop(processor);
    drop(backend);
    db.execute_batch("DROP TRIGGER fail_sync").unwrap();
    let backend = Arc::new(open(&fixture.0, "processor.sqlite"));
    let processor = AppStateProcessor::new(backend.clone(), Arc::new(TestRuntime));
    let (decoded, applied, _) = processor
        .process_parsed_patch_list(list(None, vec![patch]), &|_| unreachable!(), true)
        .await
        .unwrap();
    assert_eq!(decoded.len(), 2);
    assert_eq!(applied.version, 2);
    assert_eq!(applied.hash, expected.hash);
    let stored = backend.get_version("regular").await.unwrap();
    assert_eq!(stored.hash, expected.hash);
    assert_eq!(stored.index_value_map.len(), 1);
    fn snapshot_value(path: &Path) -> String {
        AuthStore::open(path)
            .unwrap()
            .get("app-state-sync-version", "regular")
            .unwrap()
            .unwrap()
    }
}

struct Group {
    backend: NativeAuthStore,
    incarnation: [u8; 16],
}
#[async_trait::async_trait]
impl wacore::libsignal::protocol::SenderKeyStore for Group {
    async fn load_sender_key(
        &self,
        name: &wacore::libsignal::protocol::SenderKeyName,
    ) -> SignalResult<Option<wacore::libsignal::protocol::SenderKeyRecord>> {
        self.backend
            .get_sender_key(name.cache_key())
            .await
            .map_err(signal_error)?
            .map(|b| {
                wacore::libsignal::protocol::SenderKeyRecord::deserialize_for_store(
                    &b,
                    &self.incarnation,
                )
            })
            .transpose()
    }
    async fn store_sender_key(
        &mut self,
        name: &wacore::libsignal::protocol::SenderKeyName,
        record: wacore::libsignal::protocol::SenderKeyRecord,
    ) -> SignalResult<()> {
        self.backend
            .put_sender_key(
                name.cache_key(),
                &record.serialize_for_store(&self.incarnation)?,
            )
            .await
            .map_err(signal_error)
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_backend_group_keys_resume_the_original_client() {
    use wacore::libsignal::protocol::{SenderKeyName, group_decrypt};
    let fixture = Fixture::new();
    let root = &fixture.0;
    phase("seed", root);
    let path = root.join("backend-group.sqlite");
    transfer(&root.join("original.sqlite"), &path);
    let store = AuthStore::open(&path).unwrap();
    let id = "synthetic-auth@g.us::15555550101::1";
    let legacy = group_value(&store, id);
    let decorated = Json::Array(
        legacy
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                let mut s = s.clone();
                s.insert("extension", "retained group".into());
                let keys = s
                    .get("senderMessageKeys")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|k| {
                        let mut k = k.clone();
                        k.insert("extension", "retained skipped key".into());
                        k
                    })
                    .collect();
                s.insert("senderMessageKeys", Json::Array(keys));
                s
            })
            .collect(),
    );
    store
        .set(
            "sender-key",
            id,
            Some(
                &canonical(
                    &Json::object([
                        ("type", "Buffer".into()),
                        (
                            "data",
                            STANDARD
                                .encode(canonical(&decorated, usize::MAX).unwrap())
                                .into(),
                        ),
                    ]),
                    usize::MAX,
                )
                .unwrap(),
            ),
        )
        .unwrap();
    let mut group = Group {
        backend: open(root, "backend-group.sqlite"),
        incarnation: *uuid::Uuid::new_v4().as_bytes(),
    };
    let name = SenderKeyName::new("synthetic-auth@g.us".into(), "15555550101:1@c.us.0".into());
    let wire = parse(
        &std::fs::read_to_string(root.join("wire.json")).unwrap(),
        usize::MAX,
    )
    .unwrap();
    let pending = wire.get("group").unwrap().as_array().unwrap();
    let first = bytes(&pending[0]);
    let mut corrupt = first.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    let before = snapshot(&path);
    assert!(group_decrypt(&corrupt, &mut group, &name).await.is_err());
    assert!(snapshot(&path) == before);
    assert_eq!(
        group_decrypt(&first, &mut group, &name).await.unwrap(),
        b"group-0"
    );
    let advanced = group_value(&store, id);
    assert!(
        advanced
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s.get("extension").is_some())
    );
    assert!(
        advanced
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s.get("senderMessageKeys").unwrap().as_array().unwrap())
            .any(|k| k.get("extension").is_some()),
        "Retained skipped-key metadata survives native advancement"
    );
    drop(group);
    phase("native-group-reopen", root);
    let mut group = Group {
        backend: open(root, "backend-group.sqlite"),
        incarnation: *uuid::Uuid::new_v4().as_bytes(),
    };
    assert_eq!(
        group_decrypt(&bytes(&pending[1]), &mut group, &name)
            .await
            .unwrap(),
        b"group-1"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_backend_reads_and_updates_original_credentials_keys_and_sync_state() {
    let fixture = Fixture::new();
    let root = &fixture.0;
    phase("seed", root);
    let path = root.join("backend.sqlite");
    transfer(&root.join("original.sqlite"), &path);
    let before = snapshot(&path);
    let backend: Arc<dyn Backend> = Arc::new(open(root, "backend.sqlite"));
    let mut device = backend.load().await.unwrap().unwrap();
    assert_eq!(
        device.pn.as_ref().unwrap().to_string(),
        "15555550102:1@s.whatsapp.net"
    );
    backend.save(&device).await.unwrap();
    backend
        .get_session("15555550101:1@c.us.0")
        .await
        .unwrap()
        .unwrap();
    backend
        .get_sender_key("synthetic-auth@g.us:15555550101:1@c.us.0")
        .await
        .unwrap()
        .unwrap();
    let signed = backend
        .load_signed_prekey(device.signed_pre_key_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!signed.is_empty());
    let mut state = backend.get_version("regular").await.unwrap();
    assert_eq!(state.version, 7);
    assert_eq!(state.hash, [17; 128]);
    assert!(
        snapshot(&path) == before,
        "Reads and unchanged saves preserve every raw row"
    );

    device.next_pre_key_id = 23;
    device.push_name = "Native fixture".into();
    device.edge_routing_info = Some(vec![4; 8]);
    backend.save(&device).await.unwrap();
    let pre = PreKeyRecord::new(21.into(), &device.noise_key)
        .serialize()
        .unwrap();
    backend.store_prekey(21, &pre, false).await.unwrap();
    backend.remove_prekey(21).await.unwrap();
    backend.mark_prekeys_uploaded(&[21]).await.unwrap();
    assert!(
        backend.load_prekey(21).await.unwrap().is_none(),
        "Upload completion cannot resurrect a consumed key"
    );
    backend
        .store_prekey(
            22,
            &PreKeyRecord::new(22.into(), &device.noise_key)
                .serialize()
                .unwrap(),
            true,
        )
        .await
        .unwrap();
    let id = [7; 32];
    let fingerprint = waproto::codec::app_state_sync_key_fingerprint_to_vec(
        &waproto::whatsapp::message::AppStateSyncKeyFingerprint {
            raw_id: Some(9),
            current_index: Some(4),
            device_indexes: vec![1, 2],
            ..Default::default()
        },
    );
    backend
        .set_sync_key(
            &id,
            AppStateSyncKey {
                key_data: vec![8; 32],
                fingerprint,
                timestamp: 1_730_000_000_001,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        backend.get_sync_key(&id).await.unwrap().unwrap().key_data,
        vec![8; 32]
    );
    assert_eq!(
        backend.get_latest_sync_key_id().await.unwrap(),
        Some(id.to_vec())
    );
    backend
        .set_sync_key(
            &[6; 32],
            AppStateSyncKey {
                key_data: vec![6; 32],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        backend.get_latest_sync_key_id().await.unwrap(),
        Some(vec![6; 32]),
        "A received key without a timestamp still becomes current"
    );
    state.version = 8;
    wacore::appstate::WAPATCH_INTEGRITY.subtract_then_add_in_place(
        &mut state.hash,
        &[] as &[Vec<u8>],
        &[vec![9; 32]],
    );
    backend.set_version("regular", state).await.unwrap();
    backend
        .put_mutation_macs(
            "regular",
            8,
            &[AppStateMutationMAC {
                index_mac: vec![7; 32],
                value_mac: vec![9; 32],
            }],
        )
        .await
        .unwrap();
    assert_eq!(
        backend.get_mutation_mac("regular", &[7; 32]).await.unwrap(),
        Some(vec![9; 32])
    );
    let mut metadata = backend.get_version("regular").await.unwrap();
    metadata.index_value_map.clear();
    backend.set_version("regular", metadata).await.unwrap();
    assert_eq!(
        backend.get_mutation_mac("regular", &[7; 32]).await.unwrap(),
        Some(vec![9; 32]),
        "Native version-only updates retain independently stored MACs"
    );
    let mapping = LidPnMappingEntry {
        lid: "100000000000004".into(),
        phone_number: "15555550104".into(),
        created_at: 17,
        updated_at: 18,
        learning_source: "fixture".into(),
    };
    backend.put_lid_mapping(&mapping).await.unwrap();
    assert_eq!(
        backend
            .get_lid_mapping(&mapping.lid)
            .await
            .unwrap()
            .unwrap()
            .phone_number,
        mapping.phone_number
    );
    backend
        .update_device_list(DeviceListRecord {
            user: "15555550101".into(),
            devices: vec![DeviceInfo::new(1, Some(2)), DeviceInfo::new(3, None)],
            timestamp: 19,
            phash: Some("fixture".into()),
            raw_id: Some(7),
        })
        .await
        .unwrap();
    backend
        .set_sender_key_status(
            "synthetic-auth@g.us",
            &[("15555550101:3@s.whatsapp.net", true)],
        )
        .await
        .unwrap();
    backend
        .put_tc_token(
            "15555550104@s.whatsapp.net",
            &TcTokenEntry {
                token: vec![5; 32],
                token_timestamp: 17,
                sender_timestamp: Some(19),
            },
        )
        .await
        .unwrap();
    drop(backend);
    phase("native-backend-verify", root);
    let backend = open(root, "backend.sqlite");
    let loaded = backend.load().await.unwrap().unwrap();
    assert_eq!(loaded.next_pre_key_id, 23);
    assert_eq!(loaded.push_name, "Native fixture");
    assert_eq!(
        backend
            .get_devices("15555550101")
            .await
            .unwrap()
            .unwrap()
            .devices[0]
            .key_index,
        Some(2)
    );
    assert_eq!(
        backend.get_version("regular").await.unwrap().version,
        9,
        "Original client writeback is authoritative"
    );
    let store = AuthStore::open(&path).unwrap();
    let creds = parse(&store.get("creds", "default").unwrap().unwrap(), usize::MAX).unwrap();
    assert!(
        creds.get("additionalData").is_some(),
        "Unknown encoded credential fields survive"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_backend_rolls_back_key_batches_and_keeps_durable_protocol_work() {
    let fixture = Fixture::new();
    let root = &fixture.0;
    let path = root.join("backend.sqlite");
    let backend = open(root, "backend.sqlite");
    backend.create().await.unwrap();
    let d = backend.load().await.unwrap().unwrap();
    let a = PreKeyRecord::new(1.into(), &d.noise_key)
        .serialize()
        .unwrap();
    let b = PreKeyRecord::new(2.into(), &d.noise_key)
        .serialize()
        .unwrap();
    let before = snapshot(&path);
    let inspect = rusqlite::Connection::open(&path).unwrap();
    inspect.execute_batch("CREATE TRIGGER fail_batch BEFORE INSERT ON auth WHEN NEW.category='pre-key' AND NEW.id='2' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        backend
            .store_prekeys_batch(&[(1, a.into()), (2, b.into())], false)
            .await
            .is_err()
    );
    assert!(
        snapshot(&path) == before,
        "A failed native batch publishes no partial keys or metadata"
    );
    let mut rotated = backend.load().await.unwrap().unwrap();
    let previous = rotated.signed_pre_key_id;
    rotated.signed_pre_key = wacore::libsignal::protocol::KeyPair::generate(&mut rand::rng());
    rotated.signed_pre_key_id += 1;
    rotated.signed_pre_key_signature = rotated
        .identity_key
        .private_key
        .calculate_signature(
            &rotated.signed_pre_key.public_key.serialize(),
            &mut rand::rng(),
        )
        .unwrap()
        .as_ref()
        .try_into()
        .unwrap();
    backend.save(&rotated).await.unwrap();
    assert!(
        backend
            .load_signed_prekey(previous)
            .await
            .unwrap()
            .is_some(),
        "Rotation retains the old signed key for delayed envelopes"
    );
    backend
        .store_sent_message("a:b", "c", b"first")
        .await
        .unwrap();
    backend
        .store_sent_message("a", "b:c", b"second")
        .await
        .unwrap();
    backend
        .store_pending_inbound("a", "b", "id", b"durable")
        .await
        .unwrap();
    backend
        .put_msg_secrets(vec![MsgSecretEntry {
            chat: Arc::from("a"),
            sender: Arc::from("b"),
            msg_id: Arc::from("id"),
            secret: [7; 32],
            expires_at: 100,
            message_ts: 9,
        }])
        .await
        .unwrap();
    backend
        .put_msg_secrets(vec![MsgSecretEntry {
            chat: Arc::from("a"),
            sender: Arc::from("b"),
            msg_id: Arc::from("id"),
            secret: [7; 32],
            expires_at: 90,
            message_ts: 0,
        }])
        .await
        .unwrap();
    drop(backend);
    let backend = open(root, "backend.sqlite");
    assert_eq!(
        backend.take_sent_message("a:b", "c").await.unwrap(),
        Some(b"first".to_vec())
    );
    assert!(
        backend
            .take_sent_message("a:b", "c")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        backend.take_sent_message("a", "b:c").await.unwrap(),
        Some(b"second".to_vec())
    );
    assert_eq!(
        backend.get_pending_inbound("a", "b", "id").await.unwrap(),
        Some(b"durable".to_vec())
    );
    assert_eq!(backend.delete_expired_msg_secrets(99).await.unwrap(), 0);
    assert_eq!(
        backend
            .get_msg_secret_with_ts("a", "b", "id")
            .await
            .unwrap(),
        Some((vec![7; 32], 9))
    );
    assert_eq!(backend.delete_expired_msg_secrets(100).await.unwrap(), 1);
}

type SignalResult<T> = Result<T, SignalProtocolError>;
fn signal_error(_: wacore::store::error::StoreError) -> SignalProtocolError {
    SignalProtocolError::InvalidState("backend", "Storage refused completion".into())
}
struct Sessions {
    backend: Arc<NativeAuthStore>,
    incarnation: [u8; 16],
}
#[async_trait::async_trait]
impl SessionStore for Sessions {
    async fn load_session(&self, address: &ProtocolAddress) -> SignalResult<Option<SessionRecord>> {
        self.backend
            .get_session(address.as_str())
            .await
            .map_err(signal_error)?
            .map(|b| SessionRecord::deserialize_for_store(&b, &self.incarnation))
            .transpose()
    }
    async fn has_session(&self, address: &ProtocolAddress) -> SignalResult<bool> {
        Ok(self
            .backend
            .get_session(address.as_str())
            .await
            .map_err(signal_error)?
            .is_some())
    }
    async fn store_session(
        &mut self,
        address: &ProtocolAddress,
        record: SessionRecord,
    ) -> SignalResult<()> {
        let mut bytes = Vec::new();
        record.serialize_into_for_store(&mut bytes, &self.incarnation);
        self.backend
            .put_session(address.as_str(), &bytes)
            .await
            .map_err(signal_error)
    }
}
struct Identity {
    device: Device,
    remote: IdentityKey,
}
#[async_trait::async_trait]
impl IdentityKeyStore for Identity {
    async fn get_identity_key_pair(&self) -> SignalResult<IdentityKeyPair> {
        Ok(IdentityKeyPair::new(
            IdentityKey::new(self.device.identity_key.public_key),
            self.device.identity_key.private_key.clone(),
        ))
    }
    async fn get_local_registration_id(&self) -> SignalResult<u32> {
        Ok(self.device.registration_id)
    }
    async fn is_trusted_identity(
        &self,
        _: &ProtocolAddress,
        key: &IdentityKey,
        _: Direction,
    ) -> SignalResult<bool> {
        Ok(*key == self.remote)
    }
    async fn get_identity(&self, _: &ProtocolAddress) -> SignalResult<Option<IdentityKey>> {
        Ok(Some(self.remote))
    }
    async fn save_identity(
        &mut self,
        _: &ProtocolAddress,
        key: &IdentityKey,
    ) -> SignalResult<IdentityChange> {
        assert_eq!(*key, self.remote);
        Ok(IdentityChange::from_changed(false))
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_backend_sessions_reopen_across_native_and_original_crypto() {
    let fixture = Fixture::new();
    let root = &fixture.0;
    phase("seed", root);
    transfer(
        &root.join("original.sqlite"),
        &root.join("native-bob.sqlite"),
    );
    let backend = Arc::new(open(root, "native-bob.sqlite"));
    let device = backend.load().await.unwrap().unwrap();
    let alice = AuthStore::open(root.join("alice.sqlite")).unwrap();
    let creds = parse(&alice.get("creds", "default").unwrap().unwrap(), usize::MAX).unwrap();
    let key = bytes(
        creds
            .get("signedIdentityKey")
            .unwrap()
            .get("public")
            .unwrap(),
    );
    let mut identity = Identity {
        device,
        remote: IdentityKey::new(
            wacore::libsignal::protocol::PublicKey::from_djb_public_key_bytes(&key).unwrap(),
        ),
    };
    let address = ProtocolAddress::new("15555550101:1@c.us", 0.into());
    let mut sessions = Sessions {
        backend: backend.clone(),
        incarnation: *uuid::Uuid::new_v4().as_bytes(),
    };
    let incoming = phase("crypto-wire", root);
    let wire = bytes(incoming.get("wire").unwrap().get("ciphertext").unwrap());
    let before = snapshot(&root.join("native-bob.sqlite"));
    let mut corrupt = wire.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(
        message_decrypt_signal(
            &SignalMessage::try_from(corrupt.as_slice()).unwrap(),
            &address,
            &mut sessions,
            &mut identity,
            &mut rand::rng()
        )
        .await
        .is_err()
    );
    assert!(
        snapshot(&root.join("native-bob.sqlite")) == before,
        "Rejected MAC leaves all encoded rows untouched"
    );
    assert_eq!(
        message_decrypt_signal(
            &SignalMessage::try_from(wire.as_slice()).unwrap(),
            &address,
            &mut sessions,
            &mut identity,
            &mut rand::rng()
        )
        .await
        .unwrap()
        .plaintext,
        b"native-current-send"
    );
    let fault = rusqlite::Connection::open(root.join("native-bob.sqlite")).unwrap();
    fault.execute_batch("CREATE TRIGGER fail_native_session BEFORE INSERT ON auth WHEN NEW.category='native-session' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let committed = snapshot(&root.join("native-bob.sqlite"));
    assert!(
        message_encrypt(b"not-released", &address, &mut sessions, &mut identity)
            .await
            .is_err()
    );
    assert!(
        snapshot(&root.join("native-bob.sqlite")) == committed,
        "A failed native reservation write rolls back the public row before any ciphertext is returned"
    );
    fault
        .execute_batch("DROP TRIGGER fail_native_session;")
        .unwrap();
    let reply = message_encrypt(
        b"native-current-reply",
        &address,
        &mut sessions,
        &mut identity,
    )
    .await
    .unwrap();
    std::fs::write(
        root.join("native-reply.json"),
        canonical(
            &Json::object([
                ("type", "msg".into()),
                ("ciphertext", STANDARD.encode(reply.serialize()).into()),
            ]),
            usize::MAX,
        )
        .unwrap(),
    )
    .unwrap();
    phase("crypto-reply", root);
    drop(sessions);
    drop(backend);
    // The original client consumes a retained skipped key and advances the same
    // sender chain; the native side must notice the changed public row.
    let next = phase("native-reopen", root);
    let backend = Arc::new(open(root, "native-bob.sqlite"));
    let mut sessions = Sessions {
        backend,
        incarnation: *uuid::Uuid::new_v4().as_bytes(),
    };
    let wire = bytes(next.get("wire").unwrap().get("ciphertext").unwrap());
    assert_eq!(
        message_decrypt_signal(
            &SignalMessage::try_from(wire.as_slice()).unwrap(),
            &address,
            &mut sessions,
            &mut identity,
            &mut rand::rng()
        )
        .await
        .unwrap()
        .plaintext,
        b"native-after-original"
    );
}
