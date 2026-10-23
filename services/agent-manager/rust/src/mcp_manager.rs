//! MCP bootstrap, coalescing and status lifetime around filesystem application.
use crate::mcp_files::{AppliedMcp, apply_mcp, regular_bytes};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

pub struct McpSettings {
    pub home: PathBuf,
    pub host_id: JsString,
    pub hive_url: JsString,
    pub credential: JsString,
    pub defaults: IvyJson,
}
#[derive(Clone, Debug)]
pub struct McpStatus {
    pub state: &'static str,
    /// Native target path; the service boundary serializes native path text.
    pub target: PathBuf,
    pub desired_version: Option<String>,
    pub applied_version: Option<String>,
    pub observed_at: String,
    pub code: Option<String>,
    pub bytes: usize,
}
impl McpStatus {
    pub fn json(&self) -> IvyJson {
        IvyJson::object([
            ("state", self.state.into()),
            (
                "target",
                IvyJson::String(ivy_host_runtime::node_path::from_native(&self.target)),
            ),
            (
                "desiredVersion",
                self.desired_version
                    .clone()
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "appliedVersion",
                self.applied_version
                    .clone()
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null),
            ),
            ("observedAt", self.observed_at.clone().into()),
            (
                "code",
                self.code
                    .clone()
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null),
            ),
            ("bytes", IvyJson::Number(self.bytes as f64)),
        ])
    }
}
struct State {
    status: McpStatus,
    running: Option<watch::Receiver<bool>>,
}
struct Inner {
    settings: McpSettings,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct McpManager {
    inner: Arc<Inner>,
}
pub struct McpRun {
    complete: Option<watch::Receiver<bool>>,
}
impl McpRun {
    pub async fn wait(mut self) {
        if let Some(receiver) = self.complete.as_mut() {
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
fn update(status: &mut McpStatus, result: IvyResult<AppliedMcp>) {
    match result {
        Ok(applied) => {
            status.state = if applied.applied {
                "applied"
            } else {
                "disabled"
            };
            status.target = applied.target;
            status.desired_version = Some(applied.desired_version.clone());
            status.applied_version = Some(applied.desired_version);
            status.code = None;
            status.bytes = applied.managed_bytes;
        }
        Err(error) => {
            status.state = if [
                "target_conflict",
                "limit_exceeded",
                "contract_mismatch",
                "access_denied",
                "invalid_arguments",
            ]
            .contains(&error.code.as_str())
            {
                "conflict"
            } else {
                "pending"
            };
            status.code = Some(error.code);
        }
    }
    status.observed_at = now();
}
// Dropping a caller's wait handle never cancels the manager-owned operation.
// If the operation task unwinds, clear its running slot and publish an error.
struct Completion {
    inner: Arc<Inner>,
    sender: Option<watch::Sender<bool>>,
}
impl Completion {
    fn finish(&mut self, result: IvyResult<AppliedMcp>) {
        let mut state = self.inner.state.lock().unwrap();
        update(&mut state.status, result);
        state.running = None;
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(true);
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
impl McpManager {
    pub fn new(settings: McpSettings) -> Self {
        let status = McpStatus {
            state: "pending",
            target: settings.home.join("config.toml"),
            desired_version: None,
            applied_version: None,
            observed_at: now(),
            code: None,
            bytes: 0,
        };
        Self {
            inner: Arc::new(Inner {
                settings,
                state: Mutex::new(State {
                    status,
                    running: None,
                }),
            }),
        }
    }
    pub fn status(&self) -> McpStatus {
        self.inner.state.lock().unwrap().status.clone()
    }
    /// Start immediately or join the running application. The first connection
    /// owns the coalesced work; later callers do not enqueue another application.
    pub fn synchronize(&self, connection: Arc<dyn RpcClient>) -> McpRun {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(receiver) = &state.running {
            return McpRun {
                complete: Some(receiver.clone()),
            };
        }
        let (sender, receiver) = watch::channel(false);
        state.running = Some(receiver.clone());
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut completion = Completion {
                inner: inner.clone(),
                sender: Some(sender),
            };
            let settings = &inner.settings;
            let result = apply_mcp(
                settings.home.clone(),
                connection.as_ref(),
                &settings.defaults,
                &settings.host_id,
                &settings.hive_url,
                &settings.credential,
            )
            .await;
            completion.finish(result);
        });
        McpRun {
            complete: Some(receiver),
        }
    }
    /// Wait for the current application; close does not permanently disable the
    /// manager or enqueue another synchronization.
    pub fn close(&self) -> McpRun {
        McpRun {
            complete: self.inner.state.lock().unwrap().running.clone(),
        }
    }
    pub async fn bootstrap(&self) -> IvyResult<()> {
        let path = self.inner.settings.home.join(".ivy-agent-mcp/state.json");
        let present =
            tokio::task::spawn_blocking(move || regular_bytes(&path).map(|v| v.is_some()))
                .await
                .map_err(|_| IvyError::internal())??;
        if !present {
            self.synchronize(Arc::new(Bootstrap)).wait().await;
        }
        Ok(())
    }
}
struct Bootstrap;
impl RpcClient for Bootstrap {
    fn request<'a>(&'a self, method: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "openai.getConfiguration" {
                Ok(IvyJson::object([
                    ("revision", IvyJson::Number(0.0)),
                    (
                        "configuration",
                        IvyJson::object([
                            ("schemaVersion", IvyJson::Number(1.0)),
                            ("appId", IvyJson::Null),
                        ]),
                    ),
                ]))
            } else {
                Err(IvyError::new(
                    "not_found",
                    "No stored environment override.",
                ))
            }
        })
    }
}
