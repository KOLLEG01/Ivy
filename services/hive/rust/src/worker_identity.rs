//! Session and durable OAuth state used by the storage worker. Network token
//! admission and the kernel remain separate from these single-owner operations.
use crate::store::{AuthContext, AuthenticatedContext, Credential, Store};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hmac_sha256},
    json::{canonical, number_from_string, parse, try_js_string},
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
fn truthy(value: Option<&IvyJson>) -> bool {
    match value {
        None | Some(IvyJson::Null) => false,
        Some(IvyJson::Bool(v)) => *v,
        Some(IvyJson::Number(v)) => *v != 0.0 && !v.is_nan(),
        Some(IvyJson::String(v)) => !v.0.is_empty(),
        _ => true,
    }
}
fn number(value: Option<&IvyJson>) -> IvyResult<f64> {
    Ok(match value {
        None => f64::NAN,
        Some(IvyJson::Null) => 0.0,
        Some(IvyJson::Bool(v)) => f64::from(u8::from(*v)),
        Some(IvyJson::Number(v)) => *v,
        Some(value) => number_from_string(&try_js_string(value)?),
    })
}
fn equal(a: Option<&IvyJson>, b: Option<&IvyJson>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(IvyJson::Object(_) | IvyJson::Array(_)), Some(other)) => {
            std::ptr::eq(a.unwrap(), other)
        }
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
fn spread(value: &IvyJson) -> IvyJson {
    match value {
        IvyJson::Object(_) => value.clone(),
        IvyJson::Array(values) => IvyJson::object(
            values
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v.clone())),
        ),
        IvyJson::String(value) => IvyJson::object(
            value
                .0
                .iter()
                .enumerate()
                .map(|(i, u)| (i.to_string(), IvyJson::String(JsString(vec![*u])))),
        ),
        _ => IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
    }
}
fn successor(value: Option<&IvyJson>) -> IvyResult<IvyJson> {
    if matches!(
        value,
        Some(IvyJson::String(_) | IvyJson::Array(_) | IvyJson::Object(_))
    ) {
        let mut value = try_js_string(value.unwrap())?;
        value.0.push(49);
        Ok(IvyJson::String(value))
    } else {
        Ok(IvyJson::Number(number(value)? + 1.0))
    }
}
fn family_key(value: Option<&IvyJson>) -> IvyResult<String> {
    Ok(format!(
        "oauth_refresh:{}",
        value
            .map(try_js_string)
            .transpose()?
            .map(|v| text(&v))
            .unwrap_or_else(|| "undefined".into())
    ))
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
        if truthy(state.get("revoked"))
            || number(state.get("expiresAt"))? <= now_ms as f64
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
            && !truthy(state.get("revoked"))
            && number(state.get("expiresAt"))? > now_ms as f64,
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
        if state.is_null() {
            return Err(IvyError::internal());
        }
        let bound = !truthy(state.get("revoked"))
            && number(state.get("expiresAt"))? > now_ms as f64
            && ["credentialReference", "clientId", "resource", "scope"]
                .iter()
                .all(|key| equal(state.get(key), rotation.get(key)));
        let current = bound
            && equal(state.get("generation"), rotation.get("presentedGeneration"))
            && equal(
                state.get("currentTokenDigest"),
                rotation.get("presentedTokenDigest"),
            );
        let mut next = spread(&state);
        if !current {
            next.insert("revoked", true.into());
            store.set_metadata(&key, &canonical(&next, MANAGEMENT_FRAME_BYTES)?)?;
            return Ok(false);
        }
        require(
            equal(
                rotation.get("nextGeneration"),
                Some(&successor(state.get("generation"))?),
            ) && number(rotation.get("nextExpiresAt"))? > now_ms as f64,
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
            params![digest(&token), text(credential_digest), text(now)],
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
        context.get("expiresAt").is_none() || number(context.get("expiresAt"))? > now_ms as f64,
        "unauthenticated",
        "OAuth access token has expired.",
    )
}
