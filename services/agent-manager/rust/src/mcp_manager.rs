//! MCP bootstrap, coalescing and status lifetime around filesystem application.
pub use crate::file_work::FileRun as McpRun;
use crate::{
    file_work::FileWork,
    mcp_files::{AppliedMcp, apply_mcp, regular_bytes},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{path::PathBuf, sync::Arc};

pub struct McpSettings {
    pub home: PathBuf,
    pub host_id: JsString,
    pub hive_url: JsString,
    pub credential: JsString,
    pub defaults: IvyJson,
    pub credentials: crate::mcp_credentials::McpCredentials,
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
struct Inner {
    settings: McpSettings,
    work: FileWork<McpStatus>,
}
#[derive(Clone)]
pub struct McpManager {
    inner: Arc<Inner>,
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
                work: FileWork::new(status),
            }),
        }
    }
    pub fn status(&self) -> McpStatus {
        self.inner.work.status()
    }
    /// Start immediately or join the running application. The first connection
    /// owns the coalesced work; later callers do not enqueue another application.
    pub fn synchronize(&self, connection: Arc<dyn RpcClient>) -> McpRun {
        self.inner.work.start(update, || {
            let inner = self.inner.clone();
            async move {
                let settings = &inner.settings;
                apply_mcp(
                    settings.home.clone(),
                    connection.as_ref(),
                    &settings.defaults,
                    &settings.host_id,
                    &settings.hive_url,
                    &settings.credential,
                    &settings.credentials,
                )
                .await
            }
        })
    }
    /// Wait for the current application; close does not permanently disable the
    /// manager or enqueue another synchronization.
    pub fn close(&self) -> McpRun {
        self.inner.work.close()
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
