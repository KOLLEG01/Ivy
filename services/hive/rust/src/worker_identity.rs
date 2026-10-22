//! Session and durable OAuth state used by the storage worker. Network token
//! admission and the kernel remain separate from these single-owner operations.
use crate::store::{AuthContext, AuthenticatedContext, Credential, Store};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hmac_sha256},
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn counter(value: Option<&IvyJson>) -> IvyResult<u64> {
    value
        .and_then(IvyJson::as_f64)
        .filter(|v| {
            v.is_finite() && v.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(v)
        })
        .map(|v| v as u64)
        .ok_or_else(|| {
            IvyError::new(
                "invalid_arguments",
                "OAuth timestamps and generations must be nonnegative integers.",
            )
        })
}
fn family_key(value: Option<&IvyJson>) -> IvyResult<String> {
    let digest = value
        .and_then(IvyJson::as_js_string)
        .filter(|v| !v.0.is_empty())
        .ok_or_else(|| {
            IvyError::new(
                "invalid_arguments",
                "OAuth refresh-family identity is required.",
            )
        })?;
    Ok(format!("oauth_refresh:{}", text(digest)))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn from_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
fn oauth_keys(store: &Store) -> IvyResult<(String, String)> {
    let root = store.metadata("cursor_secret")?.unwrap_or_default();
    require(
        root.len() == 64
            && root
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "storage_invalid",
        "Hive OAuth root key is unavailable.",
    )?;
    let key = from_hex(&root);
    Ok((
        hex(&hmac_sha256(&key, b"ivy/oauth/signing-key/v2")),
        hex(&hmac_sha256(&key, b"ivy/oauth/credential-reference-key/v2")),
    ))
}
pub fn oauth_secrets(store: &Store) -> IvyResult<IvyJson> {
    let (signing, reference) = oauth_keys(store)?;
    Ok(IvyJson::object([
        ("signingKey", signing.into()),
        ("referenceKey", reference.into()),
    ]))
}
pub fn credential_reference(store: &Store, credential_digest: &JsString) -> IvyResult<JsString> {
    let (_, key) = oauth_keys(store)?;
    let message = format!(
        "ivy/oauth/credential-reference/v2\0{}",
        text(credential_digest)
    );
    Ok(format!(
        "ivyr2.{}",
        URL_SAFE_NO_PAD.encode(hmac_sha256(&from_hex(&key), message.as_bytes()))
    )
    .into())
}
fn families(store: &Store) -> IvyResult<Vec<(String, IvyJson)>> {
    let mut statement = store
        .connection()
        .prepare_cached("SELECT key,value FROM metadata WHERE key LIKE 'oauth_refresh:%'")
        .map_err(|e| store.query_error(e))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| store.query_error(e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| store.query_error(e))?;
    rows.into_iter()
        .map(|(key, encoded)| {
            let invalid =
                || IvyError::new("storage_invalid", "Retained OAuth family state is invalid.");
            let state = parse(&encoded, usize::MAX).map_err(|_| invalid())?;
            if state.as_object().is_none() {
                return Err(invalid());
            }
            Ok((key, state))
        })
        .collect()
}
/// The caller supplies the original transaction boundary (retention and credential
/// rotation use this inside their own cleanup transaction).
pub fn collect_oauth_families(
    store: &Store,
    now_ms: u64,
    active: Option<&[JsString]>,
) -> IvyResult<usize> {
    let active = active.map(|values| values.iter().collect::<HashSet<_>>());
    let mut retained = 0;
    for (key, state) in families(store)? {
        let inactive = active.as_ref().is_some_and(|values| {
            !state
                .get("credentialReference")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|v| values.contains(&v))
        });
        if state.get("revoked") == Some(&true.into())
            || counter(state.get("expiresAt"))? <= now_ms
            || inactive
        {
            store
                .connection()
                .execute("DELETE FROM metadata WHERE key=?", [key])
                .map_err(|e| store.query_error(e))?;
        } else {
            retained += 1;
        }
    }
    Ok(retained)
}
pub fn create_oauth_family(
    store: &mut Store,
    state: &IvyJson,
    active: &[JsString],
    now_ms: u64,
) -> IvyResult<IvyJson> {
    if state.is_null() {
        return Err(IvyError::internal());
    }
    require(
        state
            .get("credentialReference")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| active.contains(v))
            && state.get("generation") == Some(&IvyJson::Number(1.0))
            && state.get("revoked") != Some(&true.into())
            && counter(state.get("expiresAt"))? > now_ms,
        "unauthenticated",
        "OAuth refresh family binding is invalid.",
    )?;
    store.transaction(|store| {
        let retained = collect_oauth_families(store, now_ms, Some(active))?;
        require(
            retained < 1024,
            "limit_exceeded",
            "OAuth refresh-family capacity is full.",
        )?;
        let key = family_key(state.get("familyDigest"))?;
        require(
            store.metadata(&key)?.is_none_or(|v| v.is_empty()),
            "target_conflict",
            "OAuth refresh family identity already exists.",
        )?;
        store.set_metadata(&key, &canonical(state, MANAGEMENT_FRAME_BYTES)?)
    })?;
    Ok(IvyJson::object([("created", true.into())]))
}
pub fn rotate_oauth_family(
    store: &mut Store,
    rotation: &IvyJson,
    now_ms: u64,
) -> IvyResult<IvyJson> {
    if rotation.is_null() {
        return Err(IvyError::internal());
    }
    let key = family_key(rotation.get("familyDigest"))?;
    let rotated = store.transaction(|store| {
        let Some(encoded) = store.metadata(&key)?.filter(|v| !v.is_empty()) else {
            return Ok(false);
        };
        let state = parse(&encoded, usize::MAX).map_err(|_| IvyError::internal())?;
        if state.as_object().is_none() {
            return Err(IvyError::new(
                "storage_invalid",
                "Retained OAuth family state is invalid.",
            ));
        }
        let bound = state.get("revoked") != Some(&true.into())
            && counter(state.get("expiresAt"))? > now_ms
            && ["credentialReference", "clientId", "resource", "scope"]
                .iter()
                .all(|key| {
                    state
                        .get(key)
                        .and_then(IvyJson::as_js_string)
                        .is_some_and(|value| {
                            Some(value) == rotation.get(key).and_then(IvyJson::as_js_string)
                        })
                });
        let current = bound
            && counter(state.get("generation"))? == counter(rotation.get("presentedGeneration"))?
            && state
                .get("currentTokenDigest")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|value| {
                    Some(value)
                        == rotation
                            .get("presentedTokenDigest")
                            .and_then(IvyJson::as_js_string)
                });
        let mut next = state.clone();
        if !current {
            next.insert("revoked", true.into());
            store.set_metadata(&key, &canonical(&next, MANAGEMENT_FRAME_BYTES)?)?;
            return Ok(false);
        }
        require(
            counter(state.get("generation"))?.checked_add(1)
                == Some(counter(rotation.get("nextGeneration"))?)
                && counter(rotation.get("nextExpiresAt"))? > now_ms,
            "invalid_arguments",
            "OAuth refresh successor is invalid.",
        )?;
        for (field, source) in [
            ("generation", "nextGeneration"),
            ("currentTokenDigest", "nextTokenDigest"),
            ("expiresAt", "nextExpiresAt"),
        ] {
            if let Some(value) = rotation.get(source) {
                next.insert(field, value.clone());
            } else {
                next.remove(field);
            }
        }
        store.set_metadata(&key, &canonical(&next, MANAGEMENT_FRAME_BYTES)?)?;
        Ok(true)
    })?;
    // Reuse revocation commits before the original unauthenticated response.
    require(
        rotated,
        "unauthenticated",
        "OAuth refresh token was already consumed or revoked.",
    )?;
    Ok(IvyJson::object([("rotated", true.into())]))
}
pub fn configure_credentials(
    store: &mut Store,
    credentials: &[Credential],
    now_ms: u64,
) -> IvyResult<IvyJson> {
    // Configuration commits first. A later OAuth cleanup failure does not undo it.
    store.configure_credentials(credentials)?;
    store.transaction(|store| {
        let active = credentials
            .iter()
            .map(|value| credential_reference(store, &value.digest))
            .collect::<IvyResult<Vec<_>>>()?;
        collect_oauth_families(store, now_ms, Some(&active))?;
        Ok(())
    })?;
    Ok(IvyJson::object([("configured", true.into())]))
}
pub fn create_session(
    store: &Store,
    credential_digest: &JsString,
    now: &JsString,
) -> IvyResult<IvyJson> {
    create_session_with_date(store, credential_digest, || Ok(now.clone()))
}
pub(crate) fn create_session_with_date(
    store: &Store,
    credential_digest: &JsString,
    now: impl FnOnce() -> IvyResult<JsString>,
) -> IvyResult<IvyJson> {
    store.authenticate(&AuthContext {
        credential_digest: credential_digest.clone(),
        session_digest: None,
    })?;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| IvyError::internal())?;
    let token = URL_SAFE_NO_PAD.encode(bytes);
    store
        .connection()
        .execute(
            "INSERT INTO browser_sessions VALUES (?,?,?)",
            params![digest(&token), text(credential_digest), text(&now()?)],
        )
        .map_err(|e| store.query_error(e))?;
    Ok(IvyJson::object([("sessionToken", token.into())]))
}
pub fn resolve_session(store: &Store, session_digest: &JsString) -> IvyResult<IvyJson> {
    let credential = store
        .connection()
        .query_row(
            "SELECT credential_digest FROM browser_sessions WHERE digest=?",
            [text(session_digest)],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| store.query_error(e))?
        .ok_or_else(|| IvyError::new("unauthenticated", "Browser session is not active."))?;
    let principal = store.authenticate(&AuthContext {
        credential_digest: credential.clone().into(),
        session_digest: Some(session_digest.clone()),
    })?;
    Ok(IvyJson::object([
        ("credentialDigest", credential.into()),
        ("sessionDigest", IvyJson::String(session_digest.clone())),
        ("principalId", IvyJson::String(principal.principal_id)),
    ]))
}
pub fn end_session(
    store: &Store,
    context: &AuthenticatedContext,
    session_digest: Option<&JsString>,
) -> IvyResult<IvyJson> {
    require(
        session_digest.is_some_and(|v| !v.0.is_empty()),
        "unauthenticated",
        "Logout requires the browser session.",
    )?;
    store
        .connection()
        .execute(
            "DELETE FROM browser_sessions WHERE digest=?",
            [text(session_digest.unwrap())],
        )
        .map_err(|e| store.query_error(e))?;
    Ok(IvyJson::object([(
        "credentialDigest",
        IvyJson::String(context.credential_digest.clone()),
    )]))
}
pub fn check_access_expiry(context: &IvyJson, now_ms: u64) -> IvyResult<()> {
    if context.is_null() {
        return Err(IvyError::internal());
    }
    require(
        context.get("expiresAt").is_none()
            || context
                .get("expiresAt")
                .and_then(IvyJson::as_f64)
                .is_some_and(|expires| expires > now_ms as f64),
        "unauthenticated",
        "OAuth access token has expired.",
    )
}
