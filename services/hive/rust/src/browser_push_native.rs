//! Native cryptography and delivery for Hive's fixed browser-push options.
//! The service supplies its qualified TLS roots and local-time VAPID expiration.
use crate::{
    browser_push::{SendFuture, SendOutcome, Sender},
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
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::stringify,
    whatwg_url::{parse_url, url_components},
};
use ivy_sdk::clock::{Clock, SystemClock, date_add_local_seconds};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
const RECORD_SIZE: usize = 4096;

fn checked<T, E>(result: Result<T, E>) -> IvyResult<T> {
    // Only statusCode is observed by the coordinator; primitive errors have none.
    result.map_err(|_| IvyError::internal())
}
fn text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a ivy_contracts::JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
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

/// Request bytes before HTTPS. Encryption precedes VAPID validation in web-push.
pub struct Request {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
pub fn request(
    subscription: &IvyJson,
    payload: &str,
    details: &IvyJson,
    expiration: &dyn Fn() -> IvyResult<u64>,
    entropy: &dyn Entropy,
) -> IvyResult<Request> {
    request_with_numeric_expiration(
        subscription,
        payload,
        details,
        &|| expiration().map(|n| n as f64),
        entropy,
    )
}

/// The original default Date helper may produce a negative timestamp or NaN;
/// JSON serialization preserves these as a number or null, respectively.
pub fn request_with_numeric_expiration(
    subscription: &IvyJson,
    payload: &str,
    details: &IvyJson,
    expiration: &dyn Fn() -> IvyResult<f64>,
    entropy: &dyn Entropy,
) -> IvyResult<Request> {
    let endpoint = String::from_utf16_lossy(&text(subscription, "endpoint")?.0);
    if endpoint.is_empty() {
        return Err(IvyError::internal());
    }
    let mut headers = vec![("TTL".into(), "3600".into())];
    let body = if payload.is_empty() {
        Vec::new()
    } else {
        let keys = subscription.get("keys").ok_or_else(IvyError::internal)?;
        let peer = base64(&String::from_utf16_lossy(&text(keys, "p256dh")?.0))?;
        let auth = base64(&String::from_utf16_lossy(&text(keys, "auth")?.0))?;
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
    let subject = text(details, "subject")?;
    let subject_url = parse_url(subject)?;
    if !matches!(subject_url.protocol.as_str(), "https:" | "mailto:") {
        return Err(IvyError::internal());
    }
    if subject_url.hostname == "localhost" {
        eprintln!(
            "Vapid subject points to a localhost web URI, which is unsupported by Apple's push notification server and will result in a BadJwtToken error when sending notifications."
        );
    }
    let public = String::from_utf16_lossy(&text(details, "publicKey")?.0);
    let private = String::from_utf16_lossy(&text(details, "privateKey")?.0);
    vapid_key(&public, 65)?;
    let private = vapid_key(&private, 32)?;
    let expiration = expiration()?;
    let url = url_components(&endpoint.clone().into())?;
    let claims = stringify(
        &IvyJson::object([
            ("aud", url.origin.into()),
            ("exp", IvyJson::Number(expiration)),
            ("sub", IvyJson::String(subject.clone())),
        ]),
        usize::MAX,
    )?;
    let token = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#),
        URL_SAFE_NO_PAD.encode(claims)
    );
    let signature = vapid_signature(&private, token.as_bytes())?;
    headers.push((
        "Authorization".into(),
        format!(
            "vapid t={token}.{}, k={public}",
            URL_SAFE_NO_PAD.encode(signature)
        ),
    ));
    headers.push(("Urgency".into(), "normal".into()));
    Ok(Request {
        endpoint,
        headers,
        body,
    })
}

fn vapid_signature(private: &[u8], token: &[u8]) -> IvyResult<Vec<u8>> {
    // OpenSSL's original SEC1 import/sign path accepts every 32-byte scalar.
    // Keep the existing AWS-LC path for valid keys, and use the vetted raw ECDSA
    // primitive only for the persisted values its key importer rejects.
    const ORDER: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xbc,
        0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63, 0x25, 0x51,
    ];
    if private.iter().all(|b| *b == 0) || private >= ORDER.as_slice() {
        use p256::{
            FieldBytes, NistP256, Scalar,
            elliptic_curve::{bigint::U256, ff::PrimeField, ops::Reduce},
        };
        let raw: [u8; 32] = checked(private.try_into())?;
        let secret = <Scalar as Reduce<U256>>::reduce_bytes(&raw.into());
        let hash = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, token);
        let mut prehash = FieldBytes::default();
        prehash.copy_from_slice(hash.as_ref());
        loop {
            // The native P-256 generator supplies a fresh uniform nonzero scalar.
            // Never derive k from the persisted malformed key or reuse ECE entropy.
            let nonce = checked(agreement::PrivateKey::generate(&agreement::ECDH_P256))?;
            let encoded: EcPrivateKeyBin<'_> = checked(nonce.as_be_bytes())?;
            let raw: [u8; 32] = checked(encoded.as_ref().try_into())?;
            let scalar = Option::<Scalar>::from(Scalar::from_repr(raw.into()))
                .ok_or_else(IvyError::internal)?;
            if let Ok((signature, _)) =
                ecdsa::hazmat::sign_prehashed::<NistP256, Scalar>(&secret, scalar, &prehash)
            {
                return Ok(signature.to_bytes().to_vec());
            }
            // ECDSA requires another fresh nonce when r or s is zero.
        }
    }
    // The signing scalar is independent of the advertised VAPID public key.
    let private = checked(agreement::PrivateKey::from_private_key(
        &agreement::ECDH_P256,
        private,
    ))?;
    let public = checked(private.compute_public_key())?;
    let encoded: EcPrivateKeyBin<'_> = checked(private.as_be_bytes())?;
    let signer = checked(EcdsaKeyPair::from_private_key_and_public_key(
        &ECDSA_P256_SHA256_FIXED_SIGNING,
        encoded.as_ref(),
        public.as_ref(),
    ))?;
    Ok(checked(signer.sign(&SystemRandom::new(), token))?
        .as_ref()
        .to_vec())
}

pub type PostFuture = Pin<Box<dyn Future<Output = IvyResult<u16>> + Send>>;
pub trait Https: Send + Sync {
    /// Start one request when called. Observe its status only after the response
    /// body ends. The implementation owns the original socket-inactivity timeout,
    /// exact trust policy and request headers, without redirects or retries.
    fn post(&self, request: Request) -> PostFuture;
}

/// Expiration deliberately has no UTC-arithmetic fallback: web-push adds twelve
/// hours with Date.setSeconds in the host's local timezone, including DST folds.
pub type Expiration = Arc<dyn Fn() -> IvyResult<u64> + Send + Sync>;
pub type NumericExpiration = Arc<dyn Fn() -> IvyResult<f64> + Send + Sync>;
pub fn local_expiration(clock: Arc<dyn Clock>) -> NumericExpiration {
    Arc::new(move || {
        Ok((date_add_local_seconds(clock.utc_millis() as f64, 43_200.0) / 1000.0).floor())
    })
}
pub fn system_expiration() -> NumericExpiration {
    local_expiration(Arc::new(SystemClock))
}
pub struct NativeSender {
    https: Arc<dyn Https>,
    expiration: NumericExpiration,
    entropy: Arc<dyn Entropy>,
}
impl NativeSender {
    pub fn new(https: Arc<dyn Https>, expiration: Expiration) -> Self {
        Self::with_numeric_expiration(https, Arc::new(move || expiration().map(|n| n as f64)))
    }
    pub fn with_numeric_expiration(https: Arc<dyn Https>, expiration: NumericExpiration) -> Self {
        Self {
            https,
            expiration,
            entropy: Arc::new(SystemEntropy),
        }
    }
}
impl Sender for NativeSender {
    fn send(&self, subscription: IvyJson, payload: String, options: IvyJson) -> SendFuture {
        let built = (|| {
            if options.get("TTL").and_then(IvyJson::as_f64) != Some(3600.0)
                || options.get("timeout").and_then(IvyJson::as_f64) != Some(10_000.0)
            {
                return Err(IvyError::internal());
            }
            request_with_numeric_expiration(
                &subscription,
                &payload,
                options.get("vapidDetails").ok_or_else(IvyError::internal)?,
                self.expiration.as_ref(),
                self.entropy.as_ref(),
            )
        })();
        let sent = built.map(|request| self.https.post(request));
        Box::pin(async move {
            match sent {
                Ok(sent) => match sent.await {
                    Ok(status @ 200..=299) => {
                        SendOutcome::Response(Some(IvyJson::Number(status as f64)))
                    }
                    Ok(status) => SendOutcome::Rejected(Some(IvyJson::Number(status as f64))),
                    Err(_) => SendOutcome::Rejected(None),
                },
                Err(_) => SendOutcome::Rejected(None),
            }
        })
    }
}
