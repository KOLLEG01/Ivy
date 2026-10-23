//! Existing Claude MCP file ownership, independent of the provider adapter.
use crate::{
    environment::{EnvironmentKind, read_environment_document, select_environment_document},
    mcp_manager::McpStatus,
    mcp_render::hive_mcp_urls,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
    json::{parse, stringify},
    whatwg_url::parse_url,
};
use ivy_host_runtime::node_path::{self, Style};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

pub struct ClaudeMcpSettings {
    pub target: JsString,
    pub host_id: JsString,
    pub hive_url: JsString,
    pub credential: JsString,
    pub defaults: IvyJson,
}
struct State {
    status: McpStatus,
    last_hash: Option<String>,
    running: Option<watch::Receiver<bool>>,
}
struct Inner {
    settings: ClaudeMcpSettings,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct ClaudeMcpManager(Arc<Inner>);
pub struct ClaudeMcpRun(Option<watch::Receiver<bool>>);
impl ClaudeMcpRun {
    pub async fn wait(mut self) {
        if let Some(receiver) = self.0.as_mut() {
            while !*receiver.borrow_and_update() {
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        }
    }
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn internal<T>(result: Result<T, impl std::fmt::Debug>) -> IvyResult<T> {
    result.map_err(|_| IvyError::internal())
}
fn text<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a JsString> {
    value
        .required(name)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
struct Prepared {
    servers: IvyJson,
    authority: String,
    version: String,
    enabled: bool,
}
async fn prepare(settings: &ClaudeMcpSettings, connection: &dyn RpcClient) -> IvyResult<Prepared> {
    let global = read_environment_document(connection, EnvironmentKind::Mcp, None).await?;
    let host = read_environment_document(connection, EnvironmentKind::Mcp, Some(&settings.host_id))
        .await?;
    let desired = select_environment_document(&settings.defaults, global.as_ref(), host.as_ref())?;
    // Construct these even for a disabled document, as in the original manager.
    let allowed = hive_mcp_urls(&settings.hive_url)?;
    let mut servers = IvyJson::object([] as [(&str, IvyJson); 0]);
    let enabled = desired.document.get("enabled") == Some(&IvyJson::Bool(true));
    if enabled {
        for server in desired
            .document
            .required("servers")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            if server.get("enabled") != Some(&IvyJson::Bool(true)) {
                continue;
            }
            let url = parse_url(text(server, "url")?)?;
            let loopback = ["localhost", "127.0.0.1", "[::1]"].contains(&url.hostname.as_str());
            require(
                (url.protocol == "https:" || url.protocol == "http:" && loopback)
                    && url.username.is_empty()
                    && url.password.is_empty()
                    && url.hash.is_empty(),
                "invalid_arguments",
                "Managed remote MCP URLs require HTTPS.",
            )?;
            let authenticated = text(server, "authentication")? == &JsString::from("agent-manager");
            require(
                !authenticated || allowed.contains(&url.href),
                "access_denied",
                "AgentManager credentials may only reach this Hive MCP resource.",
            )?;
            let name = text(server, "name")?;
            require(
                servers.get_key(name).is_none(),
                "invalid_arguments",
                "Managed MCP server names must be unique.",
            )?;
            let mut value = IvyJson::object([("type", "http".into()), ("url", url.href.into())]);
            if authenticated {
                let mut bearer = JsString::from("Bearer ");
                bearer.0.extend_from_slice(&settings.credential.0);
                value.insert(
                    "headers",
                    IvyJson::object([("Authorization", IvyJson::String(bearer))]),
                );
            }
            servers.insert(name.clone(), value);
        }
    }
    let names = IvyJson::Array(
        servers
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, _)| IvyJson::String(name.clone()))
            .collect(),
    );
    Ok(Prepared {
        authority: hash_json(&IvyJson::object([
            ("hostId", IvyJson::String(settings.host_id.clone())),
            ("hiveUrl", IvyJson::String(settings.hive_url.clone())),
        ]))?,
        version: hash_json(&IvyJson::object([
            ("selection", desired.version.into()),
            ("servers", names),
        ]))?,
        servers,
        enabled,
    })
}
fn regular(path: &Path, maximum: u64, message: &str) -> IvyResult<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IvyError::internal()),
    };
    require(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= maximum,
        "target_conflict",
        message,
    )?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(IvyError::internal()),
    }
}
fn apply_files(owner: &Inner, prepared: Prepared) -> IvyResult<(Prepared, usize)> {
    let target = node_path::filesystem_path(&owner.settings.target);
    let mut state_path = target.as_os_str().to_os_string();
    state_path.push(".state.json");
    let state_path = std::path::PathBuf::from(state_path);
    let current = regular(
        &target,
        4 * 1024 * 1024,
        "Claude MCP configuration was replaced by another object.",
    )?;
    let state = regular(
        &state_path,
        4096,
        "Claude MCP ownership state was replaced.",
    )?
    .map(|bytes| {
        parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| IvyError::internal())
    })
    .transpose()?;
    let state = state.as_ref().filter(|value| !value.is_null());
    require(
        current.is_none() && state.is_none()
            || current.as_ref().is_some_and(|bytes| {
                state.is_some_and(|state| {
                    state.get("schemaVersion") == Some(&IvyJson::Number(1.0))
                        && state.get("authority")
                            == Some(&IvyJson::from(prepared.authority.clone()))
                        && state.get("appliedHash") == Some(&IvyJson::from(digest(bytes)))
                })
            }),
        "target_conflict",
        "Claude MCP configuration changed outside this AgentManager.",
    )?;
    let last_hash = owner.state.lock().unwrap().last_hash.clone();
    require(
        last_hash
            .as_ref()
            .is_none_or(|hash| current.as_ref().is_some_and(|bytes| &digest(bytes) == hash)),
        "target_conflict",
        "Claude MCP configuration changed outside AgentManager.",
    )?;
    require(
        stringify(&prepared.servers, usize::MAX)?.len() <= 4 * 1024 * 1024,
        "limit_exceeded",
        "Claude MCP configuration is too large.",
    )?;
    internal(ivy_store::atomic::atomic_json(&target, &prepared.servers))?;
    let saved = internal(fs::read(&target))?;
    let hash = digest(&saved);
    // Preserve the existing output-before-state crash window and the in-memory
    // hash update even when the following state publication fails (AM02).
    owner.state.lock().unwrap().last_hash = Some(hash.clone());
    internal(ivy_store::atomic::atomic_json(
        &state_path,
        &IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("authority", prepared.authority.clone().into()),
            ("appliedHash", hash.into()),
        ]),
    ))?;
    Ok((prepared, saved.len()))
}
struct Completion {
    owner: Arc<Inner>,
    sender: Option<watch::Sender<bool>>,
}
impl Completion {
    fn finish(&mut self, result: IvyResult<(Prepared, usize)>) {
        let mut state = self.owner.state.lock().unwrap();
        match result {
            Ok((prepared, bytes)) => {
                state.status.state = if prepared.enabled {
                    "applied"
                } else {
                    "disabled"
                };
                state.status.desired_version = Some(prepared.version.clone());
                state.status.applied_version = Some(prepared.version);
                state.status.code = None;
                state.status.bytes = bytes;
            }
            Err(error) => {
                state.status.state = if ["target_conflict", "invalid_arguments", "access_denied"]
                    .contains(&error.code.as_str())
                {
                    "conflict"
                } else {
                    "pending"
                };
                state.status.code = Some(error.code);
            }
        }
        state.status.observed_at = now();
        state.running = None;
        if let Some(sender) = self.sender.take() {
            sender.send_replace(true);
        }
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if self.sender.is_some() {
            self.finish(Err(IvyError::internal()));
        }
    }
}
impl ClaudeMcpManager {
    pub fn new(settings: ClaudeMcpSettings) -> IvyResult<Self> {
        require(
            node_path::is_absolute(Style::native(), &settings.target),
            "invalid_arguments",
            "Claude MCP configuration path must be absolute.",
        )?;
        let status = McpStatus {
            state: "pending",
            target: node_path::filesystem_path(&settings.target),
            desired_version: None,
            applied_version: None,
            observed_at: now(),
            code: None,
            bytes: 0,
        };
        Ok(Self(Arc::new(Inner {
            settings,
            state: Mutex::new(State {
                status,
                last_hash: None,
                running: None,
            }),
        })))
    }
    pub fn status(&self) -> McpStatus {
        self.0.state.lock().unwrap().status.clone()
    }
    pub fn synchronize(&self, connection: Arc<dyn RpcClient>) -> ClaudeMcpRun {
        let mut state = self.0.state.lock().unwrap();
        if let Some(running) = &state.running {
            return ClaudeMcpRun(Some(running.clone()));
        }
        let (sender, receiver) = watch::channel(false);
        state.running = Some(receiver.clone());
        let owner = self.0.clone();
        tokio::spawn(async move {
            let mut completion = Completion {
                owner: owner.clone(),
                sender: Some(sender),
            };
            let result = async {
                let prepared = prepare(&owner.settings, connection.as_ref()).await?;
                internal(tokio::task::spawn_blocking(move || apply_files(&owner, prepared)).await)?
            }
            .await;
            completion.finish(result);
        });
        ClaudeMcpRun(Some(receiver))
    }
    /// AM03 remains unchanged: bootstrap can replace an existing owned policy.
    pub fn bootstrap(&self) -> ClaudeMcpRun {
        self.synchronize(Arc::new(Bootstrap))
    }
    pub fn close(&self) -> ClaudeMcpRun {
        ClaudeMcpRun(self.0.state.lock().unwrap().running.clone())
    }
}
struct Bootstrap;
impl RpcClient for Bootstrap {
    fn request<'a>(&'a self, _: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async {
            Err(IvyError::new(
                "not_found",
                "No stored environment override.",
            ))
        })
    }
}
