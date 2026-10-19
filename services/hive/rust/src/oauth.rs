//! Hive's public PKCE client, grant and bearer-token policy. The storage worker,
//! clock, entropy and canonical URL query owner are explicit dependencies.
use crate::{
    browser_push::{RequestFuture, Worker},
    store::Credential,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hmac_sha256, verify_hmac_sha256},
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
    whatwg_url::{parse_url, parse_url_with_base, url_components},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

pub const TRANSACTION_LIFETIME_MS: f64 = 15.0 * 60_000.0;
pub const ACCESS_TOKEN_LIFETIME_MS: f64 = 15.0 * 60_000.0;
pub const REFRESH_TOKEN_INACTIVITY_LIFETIME_MS: f64 = 365.0 * 24.0 * 60.0 * 60_000.0;

/// Ordered URLSearchParams entries. The HTTP/URL owner parses wire forms; this
/// boundary applies their USVString conversion and retains the first duplicate.
#[derive(Clone, Default)]
pub struct Parameters(Vec<(JsString, JsString)>);
impl Parameters {
    pub fn new(entries: impl IntoIterator<Item = (JsString, JsString)>) -> Self {
        Self(
            entries
                .into_iter()
                .map(|(k, v)| (text(&k).into(), text(&v).into()))
                .collect(),
        )
    }
    pub fn get(&self, name: &str) -> Option<&JsString> {
        self.0
            .iter()
            .find(|(k, _)| *k == JsString::from(name))
            .map(|(_, v)| v)
    }
    fn value(&self, name: &str) -> JsString {
        self.get(name).cloned().unwrap_or_default()
    }
}
pub trait FamilyStore: Send + Sync {
    fn create(&self, state: IvyJson, active_references: Vec<JsString>) -> RequestFuture;
    fn rotate(&self, rotation: IvyJson) -> RequestFuture;
}
/// The original storage-worker request shapes and synchronous posting boundary.
pub struct WorkerFamilies(pub Arc<dyn Worker>);
impl FamilyStore for WorkerFamilies {
    fn create(&self, state: IvyJson, active: Vec<JsString>) -> RequestFuture {
        self.0.request(IvyJson::object([
            ("action", "oauth.family.create".into()),
            ("state", state),
            (
                "activeCredentialReferences",
                IvyJson::Array(active.into_iter().map(IvyJson::String).collect()),
            ),
        ]))
    }
    fn rotate(&self, rotation: IvyJson) -> RequestFuture {
        self.0.request(IvyJson::object([
            ("action", "oauth.family.rotate".into()),
            ("rotation", rotation),
        ]))
    }
}
/// Apply ordered URLSearchParams.set operations to an already validated URL.
/// No local URL/form parser or permissive redirect fallback is supplied.
pub trait Redirects: Send + Sync {
    fn set_parameters(
        &self,
        uri: &JsString,
        entries: &[(JsString, JsString)],
    ) -> IvyResult<JsString>;
}
pub trait Entropy: Send + Sync {
    fn bytes(&self, length: usize) -> IvyResult<Vec<u8>>;
}
pub struct OsEntropy;
impl Entropy for OsEntropy {
    fn bytes(&self, length: usize) -> IvyResult<Vec<u8>> {
        let mut bytes = vec![0; length];
        getrandom::fill(&mut bytes).map_err(|_| IvyError::internal())?;
        Ok(bytes)
    }
}
pub struct Dependencies {
    pub families: Arc<dyn FamilyStore>,
    pub redirects: Arc<dyn Redirects>,
    pub clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub entropy: Arc<dyn Entropy>,
}
pub struct Secrets {
    pub signing_key: JsString,
    pub reference_key: JsString,
}
#[derive(Default)]
struct State {
    credentials: Vec<Credential>,
    by_digest: HashMap<JsString, JsString>,
    by_reference: Vec<(JsString, Credential)>,
    pending: HashMap<JsString, IvyJson>,
    codes: HashMap<JsString, IvyJson>,
    signing_key: Option<Vec<u8>>,
    reference_key: Option<Vec<u8>>,
}
struct Inner {
    base: JsString,
    issuer: JsString,
    resource: JsString,
    development_resource: JsString,
    dependencies: Dependencies,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct HiveOAuth(Arc<Inner>);
impl HiveOAuth {
    /// Providers must not synchronously reenter this owner while called. Token
    /// exchange requires a Tokio runtime; abandoning observation does not cancel
    /// a posted family operation or its access-token continuation.
    pub fn new(
        base: &JsString,
        credentials: Vec<Credential>,
        dependencies: Dependencies,
    ) -> IvyResult<Self> {
        let base = parse_url(base)?.href;
        let parts = url_components(&JsString::from(base.clone()))?;
        let path = parts.pathname.strip_suffix('/').unwrap_or(&parts.pathname);
        let resource =
            parse_url_with_base(&format!("{path}/mcp").into(), &parts.origin.clone().into())?.href;
        let development_resource = parse_url_with_base(
            &format!("{path}/mcp-dev").into(),
            &parts.origin.clone().into(),
        )?
        .href;
        let owner = Self(Arc::new(Inner {
            base: base.into(),
            issuer: parts.origin.into(),
            resource: resource.into(),
            development_resource: development_resource.into(),
            dependencies,
            state: Mutex::new(State::default()),
        }));
        owner.set_credentials(credentials);
        Ok(owner)
    }
    pub fn base(&self) -> &JsString {
        &self.0.base
    }
    pub fn issuer(&self) -> &JsString {
        &self.0.issuer
    }
    pub fn resource(&self) -> &JsString {
        &self.0.resource
    }
    pub fn development_resource(&self) -> &JsString {
        &self.0.development_resource
    }
    pub fn initialize(&self, secrets: &Secrets) -> IvyResult<()> {
        let valid =
            |v: &JsString| v.0.len() == 64 && v.0.iter().all(|u| matches!(u,48..=57|97..=102));
        require(
            valid(&secrets.signing_key)
                && valid(&secrets.reference_key)
                && secrets.signing_key != secrets.reference_key,
            "storage_invalid",
            "OAuth key initialization is invalid.",
        )?;
        let decode = |v: &JsString| {
            v.0.chunks_exact(2)
                .map(|p| u8::from_str_radix(&String::from_utf16_lossy(p), 16).unwrap())
                .collect()
        };
        let mut state = self.0.state.lock().unwrap();
        state.signing_key = Some(decode(&secrets.signing_key));
        state.reference_key = Some(decode(&secrets.reference_key));
        index_credentials(&mut state);
        Ok(())
    }
    pub fn set_credentials(&self, credentials: Vec<Credential>) {
        let mut state = self.0.state.lock().unwrap();
        state.by_digest = credentials
            .iter()
            .map(|c| (c.digest.clone(), c.principal_id.clone()))
            .collect();
        state.credentials = credentials;
        index_credentials(&mut state);
    }
    fn is_resource(&self, value: &JsString) -> bool {
        value == self.resource() || value == self.development_resource()
    }
    pub fn protected_resource_metadata(&self, resource: Option<&JsString>) -> IvyResult<IvyJson> {
        let resource = resource.unwrap_or(self.resource());
        require(
            self.is_resource(resource),
            "invalid_arguments",
            "Unknown Hive MCP resource.",
        )?;
        Ok(IvyJson::object([
            ("resource", IvyJson::String(resource.clone())),
            (
                "authorization_servers",
                IvyJson::Array(vec![IvyJson::String(self.issuer().clone())]),
            ),
            ("scopes_supported", IvyJson::Array(vec!["hive".into()])),
        ]))
    }
    pub fn authorization_server_metadata(&self, base_path: &JsString) -> IvyResult<IvyJson> {
        let endpoint = |path: &str| {
            parse_url_with_base(&append(base_path, path), self.issuer())
                .map(|v| IvyJson::from(v.href))
        };
        Ok(IvyJson::object([
            ("issuer", IvyJson::String(self.issuer().clone())),
            (
                "authorization_response_iss_parameter_supported",
                true.into(),
            ),
            ("authorization_endpoint", endpoint("/oauth/authorize")?),
            ("token_endpoint", endpoint("/oauth/token")?),
            ("registration_endpoint", endpoint("/oauth/register")?),
            (
                "response_types_supported",
                IvyJson::Array(vec!["code".into()]),
            ),
            (
                "grant_types_supported",
                IvyJson::Array(vec!["authorization_code".into(), "refresh_token".into()]),
            ),
            (
                "token_endpoint_auth_methods_supported",
                IvyJson::Array(vec!["none".into()]),
            ),
            (
                "code_challenge_methods_supported",
                IvyJson::Array(vec!["S256".into()]),
            ),
            ("scopes_supported", IvyJson::Array(vec!["hive".into()])),
        ]))
    }
    pub fn register(&self, value: &IvyJson) -> IvyResult<IvyJson> {
        require(
            value.as_object().is_some(),
            "invalid_arguments",
            "OAuth client registration requires an object.",
        )?;
        let redirects = value.get("redirect_uris").and_then(IvyJson::as_array);
        require(
            redirects.is_some_and(|v| {
                !v.is_empty() && v.len() <= 16 && v.iter().all(|r| r.as_js_string().is_some())
            }),
            "invalid_arguments",
            "OAuth client registration requires redirect_uris.",
        )?;
        require(
            value
                .get("token_endpoint_auth_method")
                .is_none_or(|v| *v == IvyJson::from("none")),
            "invalid_arguments",
            "Hive supports public PKCE clients only.",
        )?;
        let redirects = IvyJson::Array(
            redirects
                .unwrap()
                .iter()
                .map(|v| safe_redirect(v.as_js_string()))
                .collect::<IvyResult<Vec<_>>>()?
                .into_iter()
                .map(IvyJson::String)
                .collect(),
        );
        let name = value
            .get("client_name")
            .and_then(IvyJson::as_js_string)
            .filter(|v| v.0.len() <= 256)
            .cloned()
            .unwrap_or_else(|| "OpenAI MCP client".into());
        let client = IvyJson::object([
            ("version", IvyJson::Number(1.0)),
            ("redirectUris", redirects.clone()),
            ("clientName", IvyJson::String(name.clone())),
        ]);
        let id = format!(
            "ivyc1.{}",
            URL_SAFE_NO_PAD.encode(canonical(&client, MANAGEMENT_FRAME_BYTES)?)
        );
        require(
            id.len() <= 4096,
            "limit_exceeded",
            "OAuth client registration is too large.",
        )?;
        Ok(IvyJson::object([
            ("client_id", id.into()),
            (
                "client_id_issued_at",
                IvyJson::Number(((self.0.dependencies.clock)() / 1000.0).floor()),
            ),
            ("redirect_uris", redirects),
            ("client_name", IvyJson::String(name)),
            ("token_endpoint_auth_method", "none".into()),
        ]))
    }
    pub fn begin(&self, parameters: &Parameters) -> IvyResult<IvyJson> {
        let mut state = self.0.state.lock().unwrap();
        require(
            parameters.get("response_type") == Some(&"code".into()),
            "invalid_arguments",
            "Hive OAuth requires the authorization-code flow.",
        )?;
        let client_id = parameters.value("client_id");
        let mut pending = client(&client_id)?;
        let redirect = safe_redirect(parameters.get("redirect_uri"))?;
        require(
            pending
                .get("redirectUris")
                .unwrap()
                .as_array()
                .unwrap()
                .contains(&IvyJson::String(redirect.clone())),
            "invalid_arguments",
            "OAuth redirect URI was not registered.",
        )?;
        let state_value = parameters.value("state");
        let challenge = parameters.value("code_challenge");
        let resource = parameters.value("resource");
        require(
            !state_value.0.is_empty()
                && state_value.0.len() <= 4096
                && pkce(&challenge)
                && parameters.get("code_challenge_method") == Some(&"S256".into()),
            "invalid_arguments",
            "OAuth authorization requires state and S256 PKCE.",
        )?;
        require(
            self.is_resource(&resource),
            "invalid_arguments",
            "OAuth resource does not identify this Hive MCP server.",
        )?;
        let requested_scope = scope(parameters.get("scope"))?;
        let transaction = self.random(32)?;
        self.collect(&mut state);
        require(
            state.pending.len() < 1024,
            "limit_exceeded",
            "OAuth authorization capacity is full.",
        )?;
        for (k, v) in [
            ("clientId", IvyJson::String(client_id)),
            ("redirectUri", IvyJson::String(redirect)),
            ("state", IvyJson::String(state_value)),
            ("challenge", IvyJson::String(challenge)),
            ("scope", IvyJson::String(requested_scope.clone())),
            ("resource", IvyJson::String(resource)),
            (
                "expiresAt",
                IvyJson::Number((self.0.dependencies.clock)() + TRANSACTION_LIFETIME_MS),
            ),
        ] {
            pending.insert(k, v);
        }
        let name = pending.get("clientName").unwrap().clone();
        state.pending.insert(transaction.clone(), pending);
        Ok(IvyJson::object([
            ("transaction", IvyJson::String(transaction)),
            ("clientName", name),
            ("scope", IvyJson::String(requested_scope)),
        ]))
    }
    pub fn transaction(&self, id: &JsString) -> IvyResult<IvyJson> {
        let mut state = self.0.state.lock().unwrap();
        self.transaction_in(&mut state, id)
    }
    fn transaction_in(&self, state: &mut State, id: &JsString) -> IvyResult<IvyJson> {
        self.collect(state);
        let value = state.pending.get(id);
        require(
            value.is_some_and(|v| {
                v.get("expiresAt").unwrap().as_f64().unwrap() > (self.0.dependencies.clock)()
            }),
            "invalid_arguments",
            "OAuth authorization transaction expired.",
        )?;
        Ok(value.unwrap().clone())
    }
    pub fn authorize(&self, id: &JsString, credential_digest: &JsString) -> IvyResult<JsString> {
        let mut state = self.0.state.lock().unwrap();
        let mut pending = self.transaction_in(&mut state, id)?;
        let principal = state
            .by_digest
            .get(credential_digest)
            .filter(|v| !v.0.is_empty())
            .cloned();
        require(
            principal.is_some(),
            "unauthenticated",
            "A current Hive credential is required.",
        )?;
        self.collect(&mut state);
        require(
            state.codes.len() < 1024,
            "limit_exceeded",
            "OAuth authorization-code capacity is full.",
        )?;
        state.pending.remove(id);
        let code = self.random(32)?;
        pending.insert("principalId", IvyJson::String(principal.unwrap()));
        pending.insert(
            "credentialDigest",
            IvyJson::String(credential_digest.clone()),
        );
        state.codes.insert(code.clone(), pending.clone());
        self.0.dependencies.redirects.set_parameters(
            string(&pending, "redirectUri"),
            &[
                ("code".into(), code),
                ("state".into(), string(&pending, "state").clone()),
                ("iss".into(), self.issuer().clone()),
            ],
        )
    }
    /// Perform pre-await validation and consume a code synchronously, as the
    /// original async method does. Durable family completion then precedes issue.
    pub fn token(&self, form: &Parameters) -> RequestFuture {
        let mut state = self.0.state.lock().unwrap();
        let prepared = self.prepare_token(form, &mut state);
        let (reference, client_id, resource, requested_scope, refresh, operation) = match prepared {
            Ok(value) => value,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let family = match operation {
            FamilyOperation::Create(value, active) => {
                self.0.dependencies.families.create(value, active)
            }
            FamilyOperation::Rotate(value) => self.0.dependencies.families.rotate(value),
        };
        drop(state);
        let owner = self.clone();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let result = async {
                family.await?;
                let state = owner.0.state.lock().unwrap();
                let mut payload = token_base(
                    "access",
                    &reference,
                    &client_id,
                    &resource,
                    &requested_scope,
                );
                payload.insert("issuedAt", IvyJson::Number((owner.0.dependencies.clock)()));
                payload.insert(
                    "expiresAt",
                    IvyJson::Number((owner.0.dependencies.clock)() + ACCESS_TOKEN_LIFETIME_MS),
                );
                payload.insert("nonce", IvyJson::String(owner.random(16)?));
                let access = owner.issue(&state, &payload)?;
                Ok(IvyJson::object([
                    ("access_token", IvyJson::String(access)),
                    ("token_type", "Bearer".into()),
                    ("expires_in", IvyJson::Number(900.0)),
                    ("refresh_token", IvyJson::String(refresh)),
                    ("scope", IvyJson::String(requested_scope)),
                ]))
            }
            .await;
            let _ = send.send(result);
        });
        Box::pin(async move { receive.await.map_err(|_| IvyError::internal())? })
    }
    fn prepare_token(&self, form: &Parameters, state: &mut State) -> IvyResult<PreparedToken> {
        let client_id = form.value("client_id");
        let resource = form.value("resource");
        client(&client_id)?;
        require(
            self.is_resource(&resource),
            "invalid_arguments",
            "OAuth token resource does not identify this Hive MCP server.",
        )?;
        let (reference, requested_scope, refresh, operation) = if form.get("grant_type")
            == Some(&"authorization_code".into())
        {
            let code = state.codes.remove(&form.value("code"));
            let valid = if let Some(code) = &code {
                string(code, "resource") == &resource
                    && code.get("expiresAt").unwrap().as_f64().unwrap()
                        > (self.0.dependencies.clock)()
                    && string(code, "clientId") == &client_id
                    && string(code, "redirectUri") == &safe_redirect(form.get("redirect_uri"))?
            } else {
                false
            };
            require(
                valid,
                "unauthenticated",
                "OAuth authorization code is invalid or expired.",
            )?;
            let code = code.unwrap();
            let verifier = form.value("code_verifier");
            require(
                pkce(&verifier)
                    && JsString::from(
                        URL_SAFE_NO_PAD.encode(Sha256::digest(text(&verifier).as_bytes())),
                    ) == *string(&code, "challenge"),
                "unauthenticated",
                "OAuth PKCE verification failed.",
            )?;
            require(
                state.by_digest.get(string(&code, "credentialDigest"))
                    == Some(string(&code, "principalId")),
                "unauthenticated",
                "Hive credential is no longer configured.",
            )?;
            let reference = reference(&state, string(&code, "credentialDigest"))?;
            let requested_scope = string(&code, "scope").clone();
            let family_id = self.random(32)?;
            let expires_at = (self.0.dependencies.clock)() + REFRESH_TOKEN_INACTIVITY_LIFETIME_MS;
            let mut payload = token_base(
                "refresh",
                &reference,
                &client_id,
                &resource,
                &requested_scope,
            );
            payload.insert("issuedAt", IvyJson::Number((self.0.dependencies.clock)()));
            payload.insert("expiresAt", IvyJson::Number(expires_at));
            payload.insert("nonce", IvyJson::String(self.random(16)?));
            payload.insert("familyId", IvyJson::String(family_id.clone()));
            payload.insert("generation", IvyJson::Number(1.0));
            let refresh = self.issue(&state, &payload)?;
            let family = IvyJson::object([
                ("familyDigest", digest(text(&family_id)).into()),
                ("credentialReference", IvyJson::String(reference.clone())),
                ("clientId", IvyJson::String(client_id.clone())),
                ("resource", IvyJson::String(resource.clone())),
                ("scope", IvyJson::String(requested_scope.clone())),
                ("generation", IvyJson::Number(1.0)),
                ("currentTokenDigest", digest(text(&refresh)).into()),
                ("expiresAt", IvyJson::Number(expires_at)),
                ("revoked", false.into()),
            ]);
            let active = state.by_reference.iter().map(|(r, _)| r.clone()).collect();
            (
                reference,
                requested_scope,
                refresh,
                FamilyOperation::Create(family, active),
            )
        } else if form.get("grant_type") == Some(&"refresh_token".into()) {
            let presented = form.value("refresh_token");
            let mut payload = self.verify(&state, &presented, "refresh")?;
            require(
                string(&payload, "clientId") == &client_id,
                "unauthenticated",
                "OAuth refresh token belongs to another client.",
            )?;
            require(
                string(&payload, "resource") == &resource,
                "unauthenticated",
                "OAuth refresh token belongs to another MCP resource.",
            )?;
            let reference = string(&payload, "credentialReference").clone();
            let previous_scope = string(&payload, "scope").clone();
            let requested_scope = if form.get("scope").is_some_and(|v| !v.0.is_empty()) {
                scope(form.get("scope"))?
            } else {
                previous_scope.clone()
            };
            require(
                requested_scope == previous_scope,
                "invalid_arguments",
                "Refresh cannot expand OAuth scope.",
            )?;
            let expires_at = (self.0.dependencies.clock)() + REFRESH_TOKEN_INACTIVITY_LIFETIME_MS;
            let generation = payload.get("generation").unwrap().as_f64().unwrap();
            payload.insert("issuedAt", IvyJson::Number((self.0.dependencies.clock)()));
            payload.insert("expiresAt", IvyJson::Number(expires_at));
            payload.insert("nonce", IvyJson::String(self.random(16)?));
            payload.insert("scope", IvyJson::String(requested_scope.clone()));
            payload.insert("generation", IvyJson::Number(generation + 1.0));
            let refresh = self.issue(&state, &payload)?;
            let rotation = IvyJson::object([
                (
                    "familyDigest",
                    digest(text(string(&payload, "familyId"))).into(),
                ),
                ("credentialReference", IvyJson::String(reference.clone())),
                ("clientId", IvyJson::String(client_id.clone())),
                ("resource", IvyJson::String(resource.clone())),
                ("scope", IvyJson::String(previous_scope)),
                ("presentedGeneration", IvyJson::Number(generation)),
                ("presentedTokenDigest", digest(text(&presented)).into()),
                ("nextGeneration", IvyJson::Number(generation + 1.0)),
                ("nextTokenDigest", digest(text(&refresh)).into()),
                ("nextExpiresAt", IvyJson::Number(expires_at)),
            ]);
            (
                reference,
                requested_scope,
                refresh,
                FamilyOperation::Rotate(rotation),
            )
        } else {
            return Err(IvyError::new(
                "invalid_arguments",
                "Unsupported OAuth grant type.",
            ));
        };
        Ok((
            reference,
            client_id,
            resource,
            requested_scope,
            refresh,
            operation,
        ))
    }
    pub fn authenticate(
        &self,
        token: &JsString,
        resource: Option<&JsString>,
    ) -> IvyResult<Option<IvyJson>> {
        if !token.0.starts_with(&JsString::from("ivyoa2.").0) {
            return Ok(None);
        }
        let state = self.0.state.lock().unwrap();
        let payload = self.verify(&state, token, "access")?;
        let credential = &state
            .by_reference
            .iter()
            .find(|(r, _)| r == string(&payload, "credentialReference"))
            .unwrap()
            .1;
        require(
            resource.is_none_or(|r| r.0.is_empty() || r == string(&payload, "resource")),
            "unauthenticated",
            "OAuth token belongs to another MCP resource.",
        )?;
        Ok(Some(IvyJson::object([
            (
                "principalId",
                IvyJson::String(credential.principal_id.clone()),
            ),
            (
                "credentialDigest",
                IvyJson::String(credential.digest.clone()),
            ),
            ("expiresAt", payload.get("expiresAt").unwrap().clone()),
        ])))
    }
    fn issue(&self, state: &State, payload: &IvyJson) -> IvyResult<JsString> {
        require(
            state.signing_key.is_some()
                && state.by_reference.iter().any(|(r, _)| {
                    Some(r)
                        == payload
                            .get("credentialReference")
                            .and_then(IvyJson::as_js_string)
                }),
            "unauthenticated",
            "Hive credential is no longer configured.",
        )?;
        let encoded = URL_SAFE_NO_PAD.encode(canonical(payload, MANAGEMENT_FRAME_BYTES)?);
        let signature = hmac_sha256(state.signing_key.as_ref().unwrap(), encoded.as_bytes());
        Ok(format!("ivyoa2.{encoded}.{}", URL_SAFE_NO_PAD.encode(signature)).into())
    }
    fn verify(&self, state: &State, token: &JsString, kind: &str) -> IvyResult<IvyJson> {
        let parts = token.0.split(|u| *u == 46).collect::<Vec<_>>();
        require(
            parts.len() == 3
                && parts[0] == JsString::from("ivyoa2").0
                && token.0.len() <= 8192
                && state.signing_key.is_some(),
            "unauthenticated",
            "Invalid OAuth credential.",
        )?;
        let encoded = JsString(parts[1].to_vec());
        let actual = decode_base64(&JsString(parts[2].to_vec()));
        require(
            verify_hmac_sha256(
                state.signing_key.as_ref().unwrap(),
                text(&encoded).as_bytes(),
                &actual,
            ),
            "unauthenticated",
            "Invalid OAuth credential.",
        )?;
        let value = parse_object(&encoded)?;
        let reference = value
            .get("credentialReference")
            .and_then(IvyJson::as_js_string);
        let resource = value.get("resource").and_then(IvyJson::as_js_string);
        let id = value.get("clientId").and_then(IvyJson::as_js_string);
        let valid = value.get("version") == Some(&IvyJson::Number(2.0))
            && value.get("type") == Some(&kind.into())
            && resource.is_some_and(|r| self.is_resource(r))
            && reference.is_some_and(|r| state.by_reference.iter().any(|(k, _)| k == r))
            && id.is_some_and(|id| !id.0.is_empty() && id.0.len() <= 4096)
            && value.get("scope") == Some(&"hive".into())
            && safe_integer(value.get("issuedAt"))
            && safe_integer(value.get("expiresAt"))
            && value.get("issuedAt").unwrap().as_f64().unwrap()
                <= (self.0.dependencies.clock)() + 60_000.0
            && value.get("expiresAt").unwrap().as_f64().unwrap() > (self.0.dependencies.clock)()
            && value.get("nonce").and_then(IvyJson::as_js_string).is_some();
        require(
            valid,
            "unauthenticated",
            "OAuth credential is expired or invalid.",
        )?;
        if kind == "refresh" {
            require(
                value
                    .get("familyId")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| {
                        v.0.len() == 43
                            && v.0
                                .iter()
                                .all(|u| matches!(u,65..=90|97..=122|48..=57|95|45))
                    })
                    && safe_integer(value.get("generation"))
                    && value.get("generation").unwrap().as_f64().unwrap() >= 1.0,
                "unauthenticated",
                "OAuth refresh credential is invalid.",
            )?;
        }
        Ok(value)
    }
    fn collect(&self, state: &mut State) {
        let now = (self.0.dependencies.clock)();
        state
            .pending
            .retain(|_, v| !(v.get("expiresAt").unwrap().as_f64().unwrap() <= now));
        state
            .codes
            .retain(|_, v| !(v.get("expiresAt").unwrap().as_f64().unwrap() <= now));
    }
    fn random(&self, length: usize) -> IvyResult<JsString> {
        Ok(URL_SAFE_NO_PAD
            .encode(self.0.dependencies.entropy.bytes(length)?)
            .into())
    }
}
impl crate::http_auth::OAuth for HiveOAuth {
    fn authenticate(
        &self,
        token: &JsString,
        resource: Option<&JsString>,
    ) -> IvyResult<Option<IvyJson>> {
        self.authenticate(token, resource)
    }
}
enum FamilyOperation {
    Create(IvyJson, Vec<JsString>),
    Rotate(IvyJson),
}
type PreparedToken = (
    JsString,
    JsString,
    JsString,
    JsString,
    JsString,
    FamilyOperation,
);
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn append(value: &JsString, suffix: &str) -> JsString {
    let mut value = value.clone();
    value.0.extend(suffix.encode_utf16());
    value
}
fn string<'a>(value: &'a IvyJson, key: &str) -> &'a JsString {
    value.get(key).unwrap().as_js_string().unwrap()
}
fn safe_integer(value: Option<&IvyJson>) -> bool {
    value
        .and_then(IvyJson::as_f64)
        .is_some_and(|v| v.is_finite() && v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0)
}
fn pkce(value: &JsString) -> bool {
    let units = &value.0;
    (43..=128).contains(&units.len())
        && units
            .iter()
            .all(|u| matches!(u,65..=90|97..=122|48..=57|46|95|126|45))
}
fn safe_redirect(value: Option<&JsString>) -> IvyResult<JsString> {
    require(
        value.is_some_and(|v| !v.0.is_empty() && v.0.len() <= 2048 && !v.0.contains(&92)),
        "invalid_arguments",
        "OAuth redirect URI is invalid.",
    )?;
    let url = parse_url(value.unwrap())?;
    let loopback = matches!(url.hostname.as_str(), "localhost" | "127.0.0.1" | "[::1]");
    require(
        (url.protocol == "https:" || (url.protocol == "http:" && loopback))
            && url.username.is_empty()
            && url.password.is_empty()
            && url.hash.is_empty(),
        "invalid_arguments",
        "OAuth redirects require HTTPS or an exact loopback HTTP host.",
    )?;
    Ok(url.href.into())
}
fn scope(value: Option<&JsString>) -> IvyResult<JsString> {
    let value = value
        .filter(|v| !v.0.is_empty())
        .cloned()
        .unwrap_or_else(|| "hive".into())
        .trim();
    require(
        value == JsString::from("hive"),
        "invalid_arguments",
        "OAuth scope is unsupported.",
    )?;
    Ok(value)
}
fn client(id: &JsString) -> IvyResult<IvyJson> {
    require(
        id.0.starts_with(&JsString::from("ivyc1.").0) && id.0.len() <= 4096,
        "invalid_arguments",
        "Unknown OAuth client.",
    )?;
    let value = parse_object(&JsString(id.0[6..].to_vec()))?;
    let mut valid = value.get("version") == Some(&IvyJson::Number(1.0));
    if valid {
        if let Some(redirects) = value.get("redirectUris").and_then(IvyJson::as_array) {
            valid = !redirects.is_empty() && redirects.len() <= 16;
            if valid {
                for redirect in redirects {
                    let Some(uri) = redirect.as_js_string() else {
                        valid = false;
                        break;
                    };
                    if safe_redirect(Some(uri))? != *uri {
                        valid = false;
                        break;
                    }
                }
            }
        } else {
            valid = false;
        }
    }
    valid = valid
        && value
            .get("clientName")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| v.0.len() <= 256);
    require(valid, "invalid_arguments", "Unknown OAuth client.")?;
    Ok(value)
}
fn reference(state: &State, digest: &JsString) -> IvyResult<JsString> {
    require(
        state.reference_key.is_some(),
        "service_unavailable",
        "OAuth is not initialized.",
    )?;
    let message = append(&"ivy/oauth/credential-reference/v2\0".into(), &text(digest));
    Ok(format!(
        "ivyr2.{}",
        URL_SAFE_NO_PAD.encode(hmac_sha256(
            state.reference_key.as_ref().unwrap(),
            text(&message).as_bytes()
        ))
    )
    .into())
}
fn index_credentials(state: &mut State) {
    state.by_reference.clear();
    if state.reference_key.is_none() {
        return;
    }
    for credential in &state.credentials {
        let key = reference(state, &credential.digest).unwrap();
        if let Some((_, value)) = state.by_reference.iter_mut().find(|(k, _)| *k == key) {
            *value = credential.clone();
        } else {
            state.by_reference.push((key, credential.clone()));
        }
    }
}
fn token_base(
    kind: &str,
    reference: &JsString,
    client_id: &JsString,
    resource: &JsString,
    scope: &JsString,
) -> IvyJson {
    IvyJson::object([
        ("version", IvyJson::Number(2.0)),
        ("type", kind.into()),
        ("credentialReference", IvyJson::String(reference.clone())),
        ("clientId", IvyJson::String(client_id.clone())),
        ("resource", IvyJson::String(resource.clone())),
        ("scope", IvyJson::String(scope.clone())),
    ])
}
fn parse_object(encoded: &JsString) -> IvyResult<IvyJson> {
    let value = parse(
        &String::from_utf8_lossy(&decode_base64(encoded)),
        usize::MAX,
    )
    .map_err(|_| IvyError::new("unauthenticated", "Invalid OAuth credential."))?;
    require(
        value.as_object().is_some(),
        "unauthenticated",
        "Invalid OAuth credential.",
    )?;
    Ok(value)
}
// Buffer.from(base64url) accepts both alphabets, ignores non-alphabet low bytes,
// stops at padding and ignores an incomplete final sextet and unused bits.
fn decode_base64(value: &JsString) -> Vec<u8> {
    let mut encoded = Vec::new();
    for unit in &value.0 {
        let byte = *unit as u8;
        if byte == b'=' {
            break;
        }
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => encoded.push(byte),
            b'+' => encoded.push(b'-'),
            b'/' => encoded.push(b'_'),
            _ => {}
        }
    }
    if encoded.len() % 4 == 1 {
        encoded.pop();
    }
    base64::engine::GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    )
    .decode(encoded)
    .expect("Filtered base64 has valid complete sextets")
}
