use super::auth_backend::*;
use async_trait::async_trait;
use ivy_contracts::IvyJson as Json;
use rusqlite::Connection;
use serde::{Serialize, de::DeserializeOwned};
use wacore::store::{error::Result, traits::*};

fn encode<T: Serialize>(value: &T) -> Result<Json> {
    ivy_contracts::json::parse(&serde_json::to_string(value).map_err(protocol)?, usize::MAX)
        .map_err(|_| invalid("native metadata"))
}
fn decode<T: DeserializeOwned>(value: &Json) -> Result<T> {
    serde_json::from_str(
        &ivy_contracts::json::stringify(value, usize::MAX)
            .map_err(|_| invalid("native metadata"))?,
    )
    .map_err(protocol)
}
fn typed<T: DeserializeOwned>(db: &Connection, category: &str, id: &str) -> Result<Option<T>> {
    get(db, category, id)?.map(|v| decode(&v)).transpose()
}
fn timestamp(v: &Json) -> Result<i64> {
    if let Json::String(_) = v {
        text(v)?.parse().map_err(|_| invalid("timestamp"))
    } else {
        integer(v)
    }
}
fn mapping(db: &Connection, pn: &str, lid: &str) -> Result<LidPnMappingEntry> {
    if let Some(value) = typed::<LidPnMappingEntry>(db, "native-lid-mapping", lid)? {
        if value.phone_number == pn {
            return Ok(value);
        }
    }
    Ok(LidPnMappingEntry {
        lid: lid.into(),
        phone_number: pn.into(),
        created_at: 0,
        updated_at: 0,
        learning_source: "retained".into(),
    })
}
fn lid_for_pn(db: &Connection, pn: &str) -> Result<Option<String>> {
    get(db, "lid-mapping", pn)?.map(|v| text(&v)).transpose()
}
fn mappings(db: &Connection) -> Result<Vec<LidPnMappingEntry>> {
    entries(db, "lid-mapping")?
        .into_iter()
        .filter(|(id, _)| !id.ends_with("_reverse") && !id.starts_with("pn:"))
        .map(|(pn, v)| mapping(db, &pn, &text(&v)?))
        .collect()
}
fn token(v: &Json) -> Result<TcTokenEntry> {
    Ok(TcTokenEntry {
        token: v.get("token").map(bytes).transpose()?.unwrap_or_default(),
        token_timestamp: v.get("timestamp").map(timestamp).transpose()?.unwrap_or(0),
        sender_timestamp: v.get("senderTimestamp").map(timestamp).transpose()?,
    })
}
fn save_token(db: &Connection, jid: &str, value: &TcTokenEntry) -> Result<()> {
    let mut v = object(get(db, "tctoken", jid)?)?;
    v.insert("token", buffer(&value.token));
    v.insert("timestamp", value.token_timestamp.to_string().into());
    if let Some(t) = value.sender_timestamp {
        v.insert("senderTimestamp", number(t)?);
    } else {
        v.remove("senderTimestamp");
    }
    put(db, "tctoken", jid, Some(&v))
}
fn device_list(db: &Connection, user: &str) -> Result<Option<DeviceListRecord>> {
    let Some(v) = get(db, "device-list", user)? else {
        return Ok(None);
    };
    let devices = v
        .as_array()
        .ok_or_else(|| invalid("device list"))?
        .iter()
        .map(|v| {
            Ok(DeviceInfo::new(
                text(v)?.parse().map_err(|_| invalid("device ID"))?,
                None,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(record) = typed::<DeviceListRecord>(db, "native-device-list", user)? {
        if record
            .devices
            .iter()
            .map(|d| d.device_id)
            .eq(devices.iter().map(|d| d.device_id))
        {
            return Ok(Some(record));
        }
    }
    Ok(Some(DeviceListRecord {
        user: user.into(),
        devices,
        timestamp: 0,
        phash: None,
        raw_id: None,
    }))
}
fn save_devices(db: &Connection, record: &DeviceListRecord) -> Result<()> {
    put(
        db,
        "device-list",
        &record.user,
        Some(&Json::Array(
            record
                .devices
                .iter()
                .map(|d| d.device_id.to_string().into())
                .collect(),
        )),
    )?;
    put(
        db,
        "native-device-list",
        &record.user,
        Some(&encode(record)?),
    )
}
fn stored_message(db: &Connection, category: &str, id: &str, payload: &[u8]) -> Result<()> {
    put(
        db,
        category,
        id,
        Some(&Json::object([
            ("created", number(now_ms()? / 1000)?),
            ("payload", buffer(payload)),
        ])),
    )
}
fn expire_messages(db: &Connection, category: &str, cutoff: i64) -> Result<u32> {
    let mut count = 0;
    for (id, v) in entries(db, category)? {
        if integer(field(&v, "created")?)? < cutoff {
            put(db, category, &id, None)?;
            count += 1;
        }
    }
    Ok(count)
}

#[async_trait]
impl ProtocolStore for NativeAuthStore {
    async fn get_sender_key_devices(&self, group: &str) -> Result<Vec<(String, bool)>> {
        self.read(|db| {
            let v = object(get(db, "sender-key-memory", group)?)?;
            v.as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| {
                    Ok((
                        k.to_utf8().map_err(|_| invalid("device JID"))?,
                        v.as_bool().ok_or_else(|| invalid("sender status"))?,
                    ))
                })
                .collect()
        })
    }
    async fn set_sender_key_status(&self, group: &str, entries: &[(&str, bool)]) -> Result<()> {
        self.write(|db| {
            let mut v = object(get(db, "sender-key-memory", group)?)?;
            for (jid, status) in entries {
                v.insert(*jid, (*status).into());
            }
            put(db, "sender-key-memory", group, Some(&v))
        })
    }
    async fn clear_sender_key_devices(&self, group: &str) -> Result<()> {
        self.write(|db| put(db, "sender-key-memory", group, None))
    }
    async fn delete_sender_key_device_rows(&self, jids: &[&str]) -> Result<()> {
        self.write(|db| {
            for (group, mut v) in entries(db, "sender-key-memory")? {
                if v.as_object().is_none() {
                    return Err(invalid("sender status"));
                }
                for jid in jids {
                    v.remove(jid);
                }
                put(db, "sender-key-memory", &group, Some(&v))?
            }
            Ok(())
        })
    }
    async fn clear_all_sender_key_devices(&self) -> Result<()> {
        self.write(|db| {
            db.execute("DELETE FROM auth WHERE category='sender-key-memory'", [])
                .map_err(database)?;
            Ok(())
        })
    }
    async fn get_pn_mapping(&self, pn: &str) -> Result<Option<LidPnMappingEntry>> {
        self.read(|db| {
            lid_for_pn(db, pn)?
                .map(|lid| mapping(db, pn, &lid))
                .transpose()
        })
    }
    async fn get_lid_mapping(&self, lid: &str) -> Result<Option<LidPnMappingEntry>> {
        self.read(|db| {
            if let Some(pn) = get(db, "lid-mapping", &format!("{lid}_reverse"))?
                .map(|v| text(&v))
                .transpose()?
            {
                if lid_for_pn(db, &pn)?.as_deref() == Some(lid) {
                    return mapping(db, &pn, lid).map(Some);
                }
            }
            Ok(mappings(db)?.into_iter().find(|m| m.lid == lid))
        })
    }
    async fn put_lid_mapping(&self, entry: &LidPnMappingEntry) -> Result<()> {
        self.put_lid_mappings(std::slice::from_ref(entry)).await
    }
    async fn put_lid_mappings(&self, entries: &[LidPnMappingEntry]) -> Result<()> {
        self.write(|db| {
            for entry in entries {
                put(
                    db,
                    "lid-mapping",
                    &entry.phone_number,
                    Some(&entry.lid.as_str().into()),
                )?;
                put(
                    db,
                    "lid-mapping",
                    &format!("{}_reverse", entry.lid),
                    Some(&entry.phone_number.as_str().into()),
                )?;
                put(db, "native-lid-mapping", &entry.lid, Some(&encode(entry)?))?
            }
            Ok(())
        })
    }
    async fn get_all_lid_mappings(&self) -> Result<Vec<LidPnMappingEntry>> {
        self.read(mappings)
    }
    async fn save_base_key(&self, address: &str, id: &str, value: &[u8]) -> Result<()> {
        self.write(|db| {
            put(
                db,
                "native-base-key",
                &key(&[address, id]),
                Some(&buffer(value)),
            )
        })
    }
    async fn has_same_base_key(&self, address: &str, id: &str, current: &[u8]) -> Result<bool> {
        self.read(|db| {
            Ok(get(db, "native-base-key", &key(&[address, id]))?
                .map(|v| bytes(&v))
                .transpose()?
                .as_deref()
                == Some(current))
        })
    }
    async fn delete_base_key(&self, address: &str, id: &str) -> Result<()> {
        self.write(|db| put(db, "native-base-key", &key(&[address, id]), None))
    }
    async fn get_devices(&self, user: &str) -> Result<Option<DeviceListRecord>> {
        self.read(|db| device_list(db, user))
    }
    async fn update_device_list(&self, record: DeviceListRecord) -> Result<()> {
        self.update_device_lists(vec![record]).await
    }
    async fn update_device_lists(&self, records: Vec<DeviceListRecord>) -> Result<()> {
        self.write(|db| {
            for record in records {
                save_devices(db, &record)?
            }
            Ok(())
        })
    }
    async fn delete_devices(&self, user: &str) -> Result<()> {
        self.write(|db| {
            put(db, "device-list", user, None)?;
            put(db, "native-device-list", user, None)
        })
    }
    async fn get_group_metadata(&self, jid: &str) -> Result<Option<Vec<u8>>> {
        self.read(|db| {
            get(db, "native-group-metadata", jid)?
                .map(|v| bytes(&v))
                .transpose()
        })
    }
    async fn put_group_metadata(&self, jid: &str, blob: &[u8]) -> Result<()> {
        self.write(|db| put(db, "native-group-metadata", jid, Some(&buffer(blob))))
    }
    async fn delete_group_metadata(&self, jid: &str) -> Result<()> {
        self.write(|db| put(db, "native-group-metadata", jid, None))
    }
    async fn get_tc_token(&self, jid: &str) -> Result<Option<TcTokenEntry>> {
        self.read(|db| get(db, "tctoken", jid)?.map(|v| token(&v)).transpose())
    }
    async fn put_tc_token(&self, jid: &str, entry: &TcTokenEntry) -> Result<()> {
        self.write(|db| save_token(db, jid, entry))
    }
    async fn delete_tc_token(&self, jid: &str) -> Result<()> {
        self.write(|db| put(db, "tctoken", jid, None))
    }
    async fn get_all_tc_token_jids(&self) -> Result<Vec<String>> {
        self.read(|db| {
            Ok(entries(db, "tctoken")?
                .into_iter()
                .map(|(id, _)| id)
                .collect())
        })
    }
    async fn delete_expired_tc_tokens(&self, token_cutoff: i64, sender_cutoff: i64) -> Result<u32> {
        self.write(|db| {
            let mut count = 0;
            for (id, v) in entries(db, "tctoken")? {
                let t = token(&v)?;
                if t.token_timestamp < token_cutoff
                    && t.sender_timestamp.unwrap_or(0) < sender_cutoff
                {
                    put(db, "tctoken", &id, None)?;
                    count += 1;
                }
            }
            Ok(count)
        })
    }
    async fn touch_tc_token_sender_timestamp(&self, jid: &str, time: i64) -> Result<()> {
        self.write(|db| {
            let mut entry = get(db, "tctoken", jid)?
                .map(|v| token(&v))
                .transpose()?
                .unwrap_or(TcTokenEntry {
                    token: Vec::new(),
                    token_timestamp: time,
                    sender_timestamp: None,
                });
            entry.sender_timestamp = Some(entry.sender_timestamp.unwrap_or(time).max(time));
            save_token(db, jid, &entry)
        })
    }
    async fn store_received_tc_token(&self, jid: &str, value: &[u8], time: i64) -> Result<()> {
        self.write(|db| {
            let old = get(db, "tctoken", jid)?.map(|v| token(&v)).transpose()?;
            if old
                .as_ref()
                .is_some_and(|e| !e.token.is_empty() && e.token_timestamp > time)
            {
                return Ok(());
            }
            save_token(
                db,
                jid,
                &TcTokenEntry {
                    token: value.to_vec(),
                    token_timestamp: time,
                    sender_timestamp: old.and_then(|e| e.sender_timestamp),
                },
            )
        })
    }
    async fn store_sent_message(&self, chat: &str, id: &str, payload: &[u8]) -> Result<()> {
        self.write(|db| stored_message(db, "native-sent-message", &key(&[chat, id]), payload))
    }
    async fn take_sent_message(&self, chat: &str, id: &str) -> Result<Option<Vec<u8>>> {
        let saved = self.write(|db| {
            let id = key(&[chat, id]);
            let value = get(db, "native-sent-message", &id)?
                .map(|v| bytes(field(&v, "payload")?))
                .transpose()?;
            put(db, "native-sent-message", &id, None)?;
            Ok(value)
        })?;
        if saved.is_some() {
            return Ok(saved);
        }
        // The retained journal is the original retry-message source, including
        // text sent before the native client owned this account.
        let Some(journal) = self.1.get() else {
            return Ok(None);
        };
        let row = journal
            .access(|journal| {
                use rusqlite::OptionalExtension;
                journal
                    .connection()
                    .query_row("SELECT jid,text FROM outgoing WHERE id=?", [id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .optional()
                    .map_err(|_| ivy_contracts::IvyError::internal())
            })
            .map_err(|_| invalid("outgoing journal"))?;
        let Some((jid, text)) = row else {
            return Ok(None);
        };
        let matched = if super::socket::same_user(&jid, chat) {
            true
        } else if let Some((phone, lid)) = super::socket::lid_for_phone(&jid, chat) {
            self.get_lid_mapping(lid.user.as_ref())
                .await?
                .is_some_and(|entry| entry.phone_number == phone)
        } else {
            false
        };
        Ok(matched.then(|| {
            waproto::codec::message_to_vec(&waproto::whatsapp::Message {
                conversation: Some(text),
                ..Default::default()
            })
        }))
    }
    async fn delete_expired_sent_messages(&self, cutoff: i64) -> Result<u32> {
        self.write(|db| expire_messages(db, "native-sent-message", cutoff))
    }
    async fn store_pending_inbound(
        &self,
        chat: &str,
        sender: &str,
        id: &str,
        message: &[u8],
    ) -> Result<()> {
        self.write(|db| {
            stored_message(
                db,
                "native-pending-inbound",
                &key(&[chat, sender, id]),
                message,
            )
        })
    }
    async fn get_pending_inbound(
        &self,
        chat: &str,
        sender: &str,
        id: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.read(|db| {
            get(db, "native-pending-inbound", &key(&[chat, sender, id]))?
                .map(|v| bytes(field(&v, "payload")?))
                .transpose()
        })
    }
    async fn delete_pending_inbound(&self, chat: &str, sender: &str, id: &str) -> Result<()> {
        self.write(|db| {
            put(
                db,
                "native-pending-inbound",
                &key(&[chat, sender, id]),
                None,
            )
        })
    }
    async fn store_pending_inbound_batch(&self, rows: &[PendingInboundRow<'_>]) -> Result<()> {
        self.write(|db| {
            for row in rows {
                stored_message(
                    db,
                    "native-pending-inbound",
                    &key(&[row.chat, row.sender, row.id]),
                    row.message,
                )?
            }
            Ok(())
        })
    }
    async fn delete_pending_inbound_batch(&self, rows: &[PendingInboundKey<'_>]) -> Result<()> {
        self.write(|db| {
            for row in rows {
                put(
                    db,
                    "native-pending-inbound",
                    &key(&[row.chat, row.sender, row.id]),
                    None,
                )?
            }
            Ok(())
        })
    }
    async fn delete_expired_pending_inbound(&self, cutoff: i64) -> Result<u32> {
        self.write(|db| expire_messages(db, "native-pending-inbound", cutoff))
    }
}

#[async_trait]
impl MsgSecretStore for NativeAuthStore {
    async fn put_msg_secrets(&self, entries: Vec<MsgSecretEntry>) -> Result<usize> {
        self.write(|db| {
            let count = entries.len();
            for mut entry in entries {
                let id = key(&[&entry.chat, &entry.sender, &entry.msg_id]);
                if let Some(old) = typed::<MsgSecretEntry>(db, "native-msg-secret", &id)? {
                    entry.expires_at = merge_msg_secret_expiry(old.expires_at, entry.expires_at);
                    entry.message_ts =
                        merge_msg_secret_message_ts(old.message_ts, entry.message_ts);
                }
                put(db, "native-msg-secret", &id, Some(&encode(&entry)?))?
            }
            Ok(count)
        })
    }
    async fn get_msg_secret(&self, chat: &str, sender: &str, id: &str) -> Result<Option<Vec<u8>>> {
        self.get_msg_secret_with_ts(chat, sender, id)
            .await
            .map(|v| v.map(|(s, _)| s))
    }
    async fn get_msg_secret_with_ts(
        &self,
        chat: &str,
        sender: &str,
        id: &str,
    ) -> Result<Option<(Vec<u8>, i64)>> {
        self.read(|db| {
            Ok(
                typed::<MsgSecretEntry>(db, "native-msg-secret", &key(&[chat, sender, id]))?
                    .map(|v| (v.secret.to_vec(), v.message_ts)),
            )
        })
    }
    async fn delete_expired_msg_secrets(&self, cutoff: i64) -> Result<u32> {
        self.write(|db| {
            let mut count = 0;
            for (id, v) in entries(db, "native-msg-secret")? {
                let e: MsgSecretEntry = decode(&v)?;
                if e.expires_at != 0 && e.expires_at <= cutoff {
                    put(db, "native-msg-secret", &id, None)?;
                    count += 1;
                }
            }
            Ok(count)
        })
    }
}
