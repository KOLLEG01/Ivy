use super::auth_backend::*;
use async_trait::async_trait;
use ivy_contracts::IvyJson as Json;
use rusqlite::Connection;
use std::sync::Arc;
use wacore::{
    libsignal::protocol::{
        GenericSignedPreKey, KeyPair, PrivateKey, PublicKey, SignedPreKeyRecord, Timestamp,
    },
    store::{Device, device::DEVICE_PROPS, error::Result, traits::DeviceStore},
};
use waproto::whatsapp as wa;

pub(super) fn pair(v: &Json) -> Result<KeyPair> {
    let private = PrivateKey::deserialize(&bytes(field(v, "private")?)?).map_err(protocol)?;
    let public =
        PublicKey::from_djb_public_key_bytes(&bytes(field(v, "public")?)?).map_err(protocol)?;
    if private.public_key().map_err(protocol)? != public {
        return Err(invalid("key pair"));
    }
    Ok(KeyPair::new(public, private))
}
fn pair_json(pair: &KeyPair, old: Option<&Json>) -> Result<Json> {
    let mut v = object(old.cloned())?;
    v.insert("public", buffer(pair.public_key.public_key_bytes()));
    v.insert("private", buffer(pair.private_key.serialize().as_ref()));
    Ok(v)
}
fn optional_bytes(v: Option<&Json>) -> Result<Option<Vec<u8>>> {
    v.filter(|v| !v.is_null()).map(bytes).transpose()
}
fn optional_text(v: Option<&Json>) -> Result<Option<String>> {
    v.filter(|v| !v.is_null()).map(text).transpose()
}
fn optional_number(v: Option<&Json>, default: i64) -> Result<i64> {
    v.map(integer).transpose().map(|v| v.unwrap_or(default))
}
fn account(v: &Json) -> Result<wa::ADVSignedDeviceIdentity> {
    Ok(wa::ADVSignedDeviceIdentity {
        details: optional_bytes(v.get("details"))?,
        account_signature_key: optional_bytes(v.get("accountSignatureKey"))?,
        account_signature: optional_bytes(v.get("accountSignature"))?,
        device_signature: optional_bytes(v.get("deviceSignature"))?,
        ..Default::default()
    })
}

pub(super) fn load_device(db: &Connection) -> Result<Option<Device>> {
    let Some(creds) = get(db, "creds", "default")? else {
        return Ok(None);
    };
    let signed = field(&creds, "signedPreKey")?;
    let me = creds.get("me").filter(|v| !v.is_null());
    let native = get(db, "native-device", "default")?;
    // Only native-only metadata is stored separately. Shared public fields
    // always come from creds, so reopening after the original client is safe.
    let mut device = Device {
        pn: me
            .and_then(|v| v.get("id"))
            .map(text)
            .transpose()?
            .map(|s| s.parse().map_err(protocol))
            .transpose()?,
        lid: me
            .and_then(|v| v.get("lid"))
            .map(text)
            .transpose()?
            .map(|s| s.parse().map_err(protocol))
            .transpose()?,
        registration_id: uint(field(&creds, "registrationId")?)?,
        noise_key: pair(field(&creds, "noiseKey")?)?,
        identity_key: pair(field(&creds, "signedIdentityKey")?)?,
        signed_pre_key: pair(field(signed, "keyPair")?)?,
        signed_pre_key_id: uint(field(signed, "keyId")?)?,
        signed_pre_key_signature: bytes(field(signed, "signature")?)?
            .try_into()
            .map_err(|_| invalid("signature"))?,
        adv_secret_key: bytes(field(&creds, "advSecretKey")?)?
            .try_into()
            .map_err(|_| invalid("ADV key"))?,
        account: creds
            .get("account")
            .filter(|v| !v.is_null())
            .map(account)
            .transpose()?
            .map(Arc::new),
        push_name: me
            .and_then(|v| v.get("name"))
            .map(text)
            .transpose()?
            .unwrap_or_default(),
        app_version_primary: 2,
        app_version_secondary: 3000,
        app_version_tertiary: 1042742319,
        app_version_last_fetched_ms: 0,
        device_props: Arc::new(DEVICE_PROPS.clone()),
        client_profile: Default::default(),
        edge_routing_info: optional_bytes(creds.get("routingInfo"))?,
        props_hash: optional_text(creds.get("lastPropHash"))?,
        next_pre_key_id: u32::try_from(optional_number(creds.get("nextPreKeyId"), 1)?)
            .map_err(|_| invalid("pre-key counter"))?,
        first_unupload_pre_key_id: u32::try_from(optional_number(
            creds.get("firstUnuploadedPreKeyId"),
            1,
        )?)
        .map_err(|_| invalid("pre-key watermark"))?,
        server_has_prekeys: false,
        nct_salt: None,
        nct_salt_sync_seen: false,
        server_cert_chain: None,
        login_counter: 0,
        lid_migrated: false,
        last_signed_pre_key_rotation_ms: 0,
        read_receipts_disabled: false,
    };
    if let Some(native) = native {
        let mut encoded = serde_json::to_value(&device).map_err(protocol)?;
        let extras: serde_json::Value = serde_json::from_str(
            &ivy_contracts::json::stringify(&native, usize::MAX)
                .map_err(|_| invalid("native device"))?,
        )
        .map_err(protocol)?;
        for name in EXTRA_FIELDS {
            if let Some(v) = extras.get(*name) {
                encoded[name] = v.clone()
            }
        }
        device = serde_json::from_value(encoded).map_err(protocol)?;
        device.device_props = Arc::new(DEVICE_PROPS.clone());
    }
    Ok(Some(device))
}
const EXTRA_FIELDS: &[&str] = &[
    "app_version_primary",
    "app_version_secondary",
    "app_version_tertiary",
    "app_version_last_fetched_ms",
    "server_has_prekeys",
    "nct_salt",
    "server_cert_chain",
    "login_counter",
    "lid_migrated",
    "last_signed_pre_key_rotation_ms",
    "read_receipts_disabled",
];

pub(super) fn save_device(db: &Connection, device: &Device) -> Result<()> {
    if let Some(old) = load_device(db)? {
        if serde_json::to_value(&old).map_err(protocol)?
            == serde_json::to_value(device).map_err(protocol)?
        {
            return Ok(());
        }
        if old.identity_key.public_key != device.identity_key.public_key
            || old.identity_key.private_key.serialize()
                != device.identity_key.private_key.serialize()
            || old.registration_id != device.registration_id
        {
            return Err(invalid("account identity replacement"));
        }
        if old.signed_pre_key_id != device.signed_pre_key_id {
            let time = u64::try_from(old.last_signed_pre_key_rotation_ms)
                .map_err(|_| invalid("signed pre-key timestamp"))?;
            let record = SignedPreKeyRecord::new(
                old.signed_pre_key_id.into(),
                Timestamp::from_epoch_millis(time),
                &old.signed_pre_key,
                &old.signed_pre_key_signature,
            )
            .serialize()
            .map_err(protocol)?;
            put(
                db,
                "native-signed-pre-key",
                &old.signed_pre_key_id.to_string(),
                Some(&buffer(&record)),
            )?;
        }
    }
    let previous = get(db, "creds", "default")?;
    let fresh = previous.is_none();
    let mut creds = object(previous)?;
    if fresh {
        let ephemeral = KeyPair::generate(&mut rand::rng());
        creds.insert("pairingEphemeralKeyPair", pair_json(&ephemeral, None)?);
        creds.insert("processedHistoryMessages", Json::Array(Vec::new()));
        creds.insert("accountSyncCounter", number(0)?);
        creds.insert(
            "accountSettings",
            Json::object([("unarchiveChats", false.into())]),
        );
    }
    creds.insert(
        "noiseKey",
        pair_json(&device.noise_key, creds.get("noiseKey"))?,
    );
    creds.insert(
        "signedIdentityKey",
        pair_json(&device.identity_key, creds.get("signedIdentityKey"))?,
    );
    let mut signed = object(creds.get("signedPreKey").cloned())?;
    signed.insert(
        "keyPair",
        pair_json(&device.signed_pre_key, signed.get("keyPair"))?,
    );
    signed.insert("keyId", number(i64::from(device.signed_pre_key_id))?);
    signed.insert("signature", buffer(&device.signed_pre_key_signature));
    creds.insert("signedPreKey", signed);
    creds.insert("registrationId", number(i64::from(device.registration_id))?);
    creds.insert(
        "advSecretKey",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            device.adv_secret_key,
        )
        .into(),
    );
    creds.insert("nextPreKeyId", number(i64::from(device.next_pre_key_id))?);
    creds.insert(
        "firstUnuploadedPreKeyId",
        number(i64::from(device.first_unupload_pre_key_id))?,
    );
    creds.insert("registered", device.is_registered().into());
    if let Some(pn) = &device.pn {
        let mut me = object(creds.get("me").cloned())?;
        me.insert("id", pn.to_string().into());
        me.insert("name", device.push_name.as_str().into());
        if let Some(lid) = &device.lid {
            me.insert("lid", lid.to_string().into());
        } else {
            me.remove("lid");
        }
        creds.insert("me", me);
    } else {
        creds.remove("me");
    }
    if let Some(account) = &device.account {
        let mut v = object(creds.get("account").cloned())?;
        for (name, value) in [
            ("details", &account.details),
            ("accountSignatureKey", &account.account_signature_key),
            ("accountSignature", &account.account_signature),
            ("deviceSignature", &account.device_signature),
        ] {
            if let Some(value) = value {
                v.insert(name, buffer(value));
            } else {
                v.remove(name);
            }
        }
        creds.insert("account", v);
    } else {
        creds.remove("account");
    }
    for (name, value) in [
        (
            "routingInfo",
            device.edge_routing_info.as_deref().map(buffer),
        ),
        ("lastPropHash", device.props_hash.as_deref().map(Json::from)),
    ] {
        if let Some(value) = value {
            creds.insert(name, value);
        } else {
            creds.remove(name);
        }
    }
    put(db, "creds", "default", Some(&creds))?;
    let full = serde_json::to_value(device).map_err(protocol)?;
    let mut extra = serde_json::Map::new();
    for name in EXTRA_FIELDS {
        if let Some(value) = full.get(*name) {
            extra.insert((*name).into(), value.clone());
        }
    }
    let extra = ivy_contracts::json::parse(
        &serde_json::to_string(&extra).map_err(protocol)?,
        usize::MAX,
    )
    .map_err(|_| invalid("native device"))?;
    put(db, "native-device", "default", Some(&extra))
}

#[async_trait]
impl DeviceStore for NativeAuthStore {
    async fn load(&self) -> Result<Option<Device>> {
        self.read(load_device)
    }
    async fn save(&self, device: &Device) -> Result<()> {
        self.write(|db| save_device(db, device))
    }
    async fn exists(&self) -> Result<bool> {
        self.read(|db| Ok(get_raw(db, "creds", "default")?.is_some()))
    }
    async fn create(&self) -> Result<i32> {
        self.write(|db| {
            if get_raw(db, "creds", "default")?.is_none() {
                save_device(db, &Device::new())?;
            }
            Ok(1)
        })
    }
}
