//! AgentManager initialization, independent of the Voice client's catalog policy.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    codex_handshake::initialized_server_version,
    codex_rpc::{NativeRpc, RequestOptions},
    node_path, paths,
};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeMode {
    OwnedStdio,
    ExternalProxy,
    ClaudeAdapter,
}
impl NativeMode {
    pub fn connection_mode(self) -> &'static str {
        if self == Self::ExternalProxy {
            "external-proxy"
        } else {
            "owned-stdio"
        }
    }
}
pub struct AgentHandshake<'a> {
    pub client_version: &'a JsString,
    pub native_version: &'a JsString,
    pub expected_home: &'a Path,
    pub mode: NativeMode,
    pub daemon: Option<&'a IvyJson>,
}
pub struct NativeInitialization {
    pub initialized: IvyJson,
    pub native_home: PathBuf,
    pub actual_server_version: JsString,
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
async fn observed_home(value: &JsString) -> IvyResult<PathBuf> {
    let path = node_path::filesystem_path(value);
    tokio::task::spawn_blocking(move || paths::realpath(&path))
        .await
        .map_err(|_| start_failed())?
        .map_err(|_| start_failed())
}
fn start_failed() -> IvyError {
    // Original filesystem exceptions are wrapped only inside the initialized
    // launch boundary; native IvyError replies retain their original identity.
    IvyError::new("native_start_failed", "Public native startup failed.")
}

/// The process owner retires its child if this fails. Owned Codex retains its
/// configured version; only proxy/Claude handshakes parse the server user-agent.
pub async fn initialize_agent(
    rpc: &NativeRpc,
    settings: AgentHandshake<'_>,
) -> IvyResult<NativeInitialization> {
    let reply = rpc
        .request(
            "initialize".into(),
            IvyJson::object([
                (
                    "clientInfo",
                    IvyJson::object([
                        ("name", "ivy-agent-manager".into()),
                        ("title", "Ivy AgentManager".into()),
                        ("version", IvyJson::String(settings.client_version.clone())),
                    ]),
                ),
                (
                    "capabilities",
                    IvyJson::object([
                        ("experimentalApi", true.into()),
                        ("requestAttestation", false.into()),
                    ]),
                ),
            ]),
            RequestOptions {
                timeout_ms: 30_000,
                ..Default::default()
            },
        )
        .await?;
    let result = reply.get("result").ok_or_else(|| {
        IvyError::new(
            "native_initialization_failed",
            "Public native initialization returned an error.",
        )
    })?;
    let home = result.get("codexHome").and_then(IvyJson::as_js_string);
    require(
        result.as_object().is_some() && home.is_some(),
        "native_home_mismatch",
        "Initialized server home differs from its explicit expected assignment.",
    )?;
    let home = home.unwrap();
    require(
        observed_home(home).await? == settings.expected_home,
        "native_home_mismatch",
        "Initialized server home differs from its explicit expected assignment.",
    )?;
    let version = if settings.mode == NativeMode::OwnedStdio {
        settings.native_version.clone()
    } else {
        initialized_server_version(result.get("userAgent").unwrap_or(&IvyJson::Null))?
    };
    let claude_marker = JsString::from("claude-codex");
    require(
        version == *settings.native_version
            && (settings.mode != NativeMode::ClaudeAdapter
                || result
                    .get("userAgent")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|value| {
                        value
                            .0
                            .windows(claude_marker.0.len())
                            .any(|units| units == claude_marker.0)
                    }))
            && settings.daemon.is_none_or(|daemon| {
                daemon.get("appServerVersion") == Some(&IvyJson::String(version.clone()))
            }),
        "native_server_version_mismatch",
        "The connected external server version differs from the checked protocol catalog.",
    )?;
    rpc.notify("initialized".into(), IvyJson::Null).await?;
    Ok(NativeInitialization {
        initialized: result.clone(),
        native_home: observed_home(home).await?,
        actual_server_version: version,
    })
}
