//! Native cryptography and delivery for Hive's fixed browser-push options.
//! VAPID uses native P-256 keys and a twelve-hour UTC expiration.
use crate::{
    browser_push::{SendFuture, Sender, VapidDetails},
    browser_push_store::Crypto,
};
use aws_lc_rs::{
    aead, agreement,
    encoding::{AsBigEndian, EcPrivateKeyBin},
    hkdf,
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair},
};
use base64::{
    Engine, alphabet,
    engine::{
        DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose::URL_SAFE_NO_PAD,
    },
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::stringify};
use reqwest::Url;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
const RECORD_SIZE: usize = 4096;

fn checked<T, E>(result: Result<T, E>) -> IvyResult<T> {
    // Only statusCode is observed by the coordinator; primitive errors have none.
    result.map_err(|_| IvyError::internal())
}
fn text(value: &IvyJson, key: &str) -> IvyResult<String> {
    value
        .get(key)
        .ok_or_else(IvyError::internal)
        .and_then(IvyJson::as_str)
}
fn base64(value: &str) -> IvyResult<Vec<u8>> {
    // Persisted subscription keys pass the store's alphabet and length checks.
    checked(
        GeneralPurpose::new(
            &alphabet::URL_SAFE,
            GeneralPurposeConfig::new()
                .with_decode_padding_mode(DecodePaddingMode::Indifferent)
                .with_decode_allow_trailing_bits(true),
        )
        .decode(value),
    )
}
fn vapid_key(value: &str, length: usize) -> IvyResult<Vec<u8>> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || v == b'-' || v == b'_')
    {
        return Err(IvyError::internal());
    }
    let bytes = base64(value)?;
    if bytes.len() != length {
        return Err(IvyError::internal());
    }
    Ok(bytes)
}

/// The same native P-256 owner serves subscription admission and persisted VAPID keys.
pub struct NativeCrypto;
impl Crypto for NativeCrypto {
    fn generate_vapid_keys(&self) -> IvyResult<IvyJson> {
        let key = checked(agreement::PrivateKey::generate(&agreement::ECDH_P256))?;
        let public = checked(key.compute_public_key())?;
        let private: EcPrivateKeyBin<'_> = checked(key.as_be_bytes())?;
        Ok(IvyJson::object([
            ("publicKey", URL_SAFE_NO_PAD.encode(public.as_ref()).into()),
            (
                "privateKey",
                URL_SAFE_NO_PAD.encode(private.as_ref()).into(),
            ),
        ]))
    }
    fn validate_p256_public_key(&self, bytes: &[u8]) -> IvyResult<()> {
        checked(agreement::ParsedPublicKey::try_from(
            agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, bytes),
        ))
        .map(|_| ())
    }
}

/// Each send requires fresh ephemeral key material and a fresh 16-byte salt.
pub trait Entropy: Send + Sync {
    fn encryption_key(&self) -> IvyResult<agreement::PrivateKey>;
    fn salt(&self) -> IvyResult<[u8; 16]>;
}
pub struct SystemEntropy;
impl Entropy for SystemEntropy {
    fn encryption_key(&self) -> IvyResult<agreement::PrivateKey> {
        checked(agreement::PrivateKey::generate(&agreement::ECDH_P256))
    }
    fn salt(&self) -> IvyResult<[u8; 16]> {
        let mut salt = [0; 16];
        checked(getrandom::fill(&mut salt))?;
        Ok(salt)
    }
}
struct Length(usize);
impl hkdf::KeyType for Length {
    fn len(&self) -> usize {
        self.0
    }
}
fn expand<const N: usize>(key: &hkdf::Prk, info: &[u8]) -> IvyResult<[u8; N]> {
    let infos = [info];
    let output = checked(key.expand(&infos, Length(N)))?;
    let mut bytes = [0; N];
    checked(output.fill(&mut bytes))?;
    Ok(bytes)
}
fn encrypt(payload: &[u8], peer: &[u8], auth: &[u8], entropy: &dyn Entropy) -> IvyResult<Vec<u8>> {
    let private = entropy.encryption_key()?;
    let public = checked(private.compute_public_key())?;
    let salt = entropy.salt()?;
    let peer_key = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, peer);
    let shared = agreement::agree(&private, &peer_key, IvyError::internal(), |bytes| {
        Ok(bytes.to_vec())
    })?;
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(peer);
    info.extend_from_slice(public.as_ref());
    let auth_prk = hkdf::Salt::new(hkdf::HKDF_SHA256, auth).extract(&shared);
    let ikm = expand::<32>(&auth_prk, &info)?;
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &salt).extract(&ikm);
    let key = expand::<16>(&prk, b"Content-Encoding: aes128gcm\0")?;
    let nonce = expand::<12>(&prk, b"Content-Encoding: nonce\0")?;
    let cipher = aead::LessSafeKey::new(checked(aead::UnboundKey::new(&aead::AES_128_GCM, &key))?);
    let mut output = salt.to_vec();
    output.extend_from_slice(&(RECORD_SIZE as u32).to_be_bytes());
    output.push(public.as_ref().len() as u8);
    output.extend_from_slice(public.as_ref());
    let mut offset = 0;
    let mut counter = 0u64;
    loop {
        let end = payload.len().min(offset + RECORD_SIZE - 17);
        let last = end == payload.len();
        let mut record = payload[offset..end].to_vec();
        record.push(if last { 2 } else { 1 });
        let mut iv = nonce;
        for (target, byte) in iv[4..].iter_mut().zip(counter.to_be_bytes()) {
            *target ^= byte;
        }
        checked(cipher.seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(iv),
            aead::Aad::empty(),
            &mut record,
        ))?;
        output.extend_from_slice(&record);
        if last {
            break;
        }
        offset = end;
        counter += 1;
    }
    Ok(output)
}

/// A signed, encrypted request ready for HTTPS delivery.
pub struct Request {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
pub fn request(
    subscription: &IvyJson,
    payload: &str,
    details: &VapidDetails,
    expiration: &dyn Fn() -> IvyResult<u64>,
    entropy: &dyn Entropy,
) -> IvyResult<Request> {
    let endpoint = text(subscription, "endpoint")?;
    let url = checked(Url::parse(&endpoint))?;
    if url.scheme() != "https" || url.host().is_none() {
        return Err(IvyError::internal());
    }
    let subject = details.subject.as_str();
    let subject_url = checked(Url::parse(subject))?;
    if !matches!(subject_url.scheme(), "https" | "mailto") {
        return Err(IvyError::internal());
    }
    let public = details.public_key.as_str();
    let public_bytes = vapid_key(public, 65)?;
    let private = vapid_key(&details.private_key, 32)?;
    let signer = checked(EcdsaKeyPair::from_private_key_and_public_key(
        &ECDSA_P256_SHA256_FIXED_SIGNING,
        &private,
        &public_bytes,
    ))?;
    let mut headers = vec![("TTL".into(), "3600".into())];
    let body = if payload.is_empty() {
        Vec::new()
    } else {
        let keys = subscription.get("keys").ok_or_else(IvyError::internal)?;
        let peer = base64(&text(keys, "p256dh")?)?;
        let auth = base64(&text(keys, "auth")?)?;
        if peer.len() != 65 || auth.len() < 16 {
            return Err(IvyError::internal());
        }
        encrypt(payload.as_bytes(), &peer, &auth, entropy)?
    };
    headers.push(("Content-Length".into(), body.len().to_string()));
    if !payload.is_empty() {
        headers.push(("Content-Type".into(), "application/octet-stream".into()));
        headers.push(("Content-Encoding".into(), "aes128gcm".into()));
    }
    let expiration = expiration()?;
    let claims = stringify(
        &IvyJson::object([
            ("aud", url.origin().ascii_serialization().into()),
            ("exp", IvyJson::Number(expiration as f64)),
            ("sub", subject.into()),
        ]),
        usize::MAX,
    )?;
    let token = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#),
        URL_SAFE_NO_PAD.encode(claims)
    );
    let signature = checked(signer.sign(&SystemRandom::new(), token.as_bytes()))?;
    headers.push((
        "Authorization".into(),
        format!(
            "vapid t={token}.{}, k={public}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        ),
    ));
    headers.push(("Urgency".into(), "normal".into()));
    Ok(Request {
        endpoint,
        headers,
        body,
    })
}

pub type PostFuture = Pin<Box<dyn Future<Output = IvyResult<u16>> + Send>>;
pub trait Https: Send + Sync {
    /// Deliver one request and observe its status after the body completes.
    /// The transport owns TLS trust and the total deadline, without redirects
    /// or retries.
    fn post(&self, request: Request) -> PostFuture;
}

pub type Expiration = Arc<dyn Fn() -> IvyResult<u64> + Send + Sync>;
pub fn expiration_at(now: SystemTime) -> IvyResult<u64> {
    checked(now.duration_since(UNIX_EPOCH))?
        .as_secs()
        .checked_add(43_200)
        .ok_or_else(IvyError::internal)
}
pub fn system_expiration() -> Expiration {
    Arc::new(|| expiration_at(SystemTime::now()))
}
pub struct NativeSender {
    https: Arc<dyn Https>,
    expiration: Expiration,
    entropy: Arc<dyn Entropy>,
}
impl NativeSender {
    pub fn new(https: Arc<dyn Https>, expiration: Expiration) -> Self {
        Self {
            https,
            expiration,
            entropy: Arc::new(SystemEntropy),
        }
    }
}
impl Sender for NativeSender {
    fn send(&self, subscription: IvyJson, payload: String, details: VapidDetails) -> SendFuture {
        let built = request(
            &subscription,
            &payload,
            &details,
            self.expiration.as_ref(),
            self.entropy.as_ref(),
        );
        let sent = built.map(|request| self.https.post(request));
        Box::pin(async move { sent?.await })
    }
}
