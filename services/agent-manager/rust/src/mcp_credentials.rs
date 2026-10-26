//! Endpoint-bound credentials enter at the protected host settings boundary.
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, agent_schema::validate_agent, hash::hash_json,
    whatwg_url::parse_url,
};
use std::collections::{BTreeMap, HashSet};

// Deliberately no Debug: tokens must never enter diagnostics or public status.
#[derive(Clone)]
pub struct McpCredential {
    pub url: String,
    pub bearer_token: String,
}
pub type McpCredentials = BTreeMap<String, McpCredential>;

pub fn read_credentials(value: Option<&Json>) -> IvyResult<McpCredentials> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let entries = value.as_object().ok_or_else(IvyError::internal)?;
    entries
        .iter()
        .map(|(name, value)| {
            validate_agent("McpCredential", value)?;
            Ok((
                name.to_utf8()?,
                McpCredential {
                    url: value.required("url")?.as_str()?,
                    bearer_token: value.required("bearerToken")?.as_str()?,
                },
            ))
        })
        .collect()
}

pub fn credentials_hash(credentials: &McpCredentials) -> IvyResult<String> {
    hash_json(&Json::object(credentials.iter().map(
        |(name, credential)| {
            (
                name.clone(),
                Json::object([
                    ("url", credential.url.clone().into()),
                    ("bearerToken", credential.bearer_token.clone().into()),
                ]),
            )
        },
    )))
}

pub struct ResolvedMcp {
    pub url: String,
    pub authorization: Option<String>,
}

pub fn resolve_mcp_server(
    server: &Json,
    hive_urls: &HashSet<String>,
    hive_credential: &JsString,
    credentials: &McpCredentials,
) -> IvyResult<ResolvedMcp> {
    let url = parse_url(&server.required("url")?.as_str()?.into())?;
    let loopback = ["localhost", "127.0.0.1", "[::1]"].contains(&url.hostname.as_str());
    if !(url.protocol == "https:" || url.protocol == "http:" && loopback)
        || !url.username.is_empty()
        || !url.password.is_empty()
        || !url.hash.is_empty()
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Managed remote MCP URLs require HTTPS.",
        ));
    }
    let authentication = server.required("authentication")?.as_str()?;
    let authorization = match authentication.as_str() {
        "bearer" => {
            let selected = server
                .get("credentialId")
                .map(Json::as_str)
                .transpose()?
                .and_then(|id| credentials.get(&id))
                .ok_or_else(|| {
                    IvyError::new(
                        "access_denied",
                        "External MCP credential is not provisioned on this host.",
                    )
                })?;
            if url.href != parse_url(&selected.url.as_str().into())?.href {
                return Err(IvyError::new(
                    "access_denied",
                    "External MCP credential is bound to another endpoint.",
                ));
            }
            // RFC 6750 b64token, with padding only at the end. Reject header injection.
            let token = selected.bearer_token.as_bytes();
            let body = token
                .iter()
                .position(|byte| *byte == b'=')
                .unwrap_or(token.len());
            if body == 0
                || token.len() > 16384
                || !token[..body]
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._~+/-".contains(byte))
                || !token[body..].iter().all(|byte| *byte == b'=')
            {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "External MCP bearer token is invalid.",
                ));
            }
            Some(format!("Bearer {}", selected.bearer_token))
        }
        "agent-manager" | "none" => {
            if server.get("credentialId").is_some() {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "Only bearer authentication may reference an external credential.",
                ));
            }
            if authentication == "agent-manager" {
                if !hive_urls.contains(&url.href) {
                    return Err(IvyError::new(
                        "access_denied",
                        "AgentManager credentials can only be sent to this Hive MCP resource.",
                    ));
                }
                Some(format!("Bearer {}", hive_credential.to_utf8()?))
            } else {
                None
            }
        }
        _ => {
            return Err(IvyError::new(
                "invalid_arguments",
                "Unknown MCP authentication mode.",
            ));
        }
    };
    Ok(ResolvedMcp {
        url: url.href,
        authorization,
    })
}
