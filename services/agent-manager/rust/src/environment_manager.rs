//! Concrete environment ownership shared by pre-launch, reconciliation and close.
use crate::{
    claude_mcp::{ClaudeMcpManager, ClaudeMcpSettings},
    instructions_manager::{InstructionsManager, InstructionsSettings},
    instructions_source::EnglishHintComparator,
    inventory::InventoryClock,
    mcp_manager::{McpManager, McpSettings},
    reconciliation::{EnvironmentSynchronizers, EnvironmentWait},
    shutdown::ShutdownStep,
    skills_files::SkillsComparator,
    skills_manager::{SkillsManager, SkillsSettings},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::node_path::{self, Style};
use ivy_sdk::{rpc::RpcClient, service::ServiceFuture};
use std::sync::{Arc, Mutex};
use tokio::{sync::watch, task::JoinHandle};

pub struct EnvironmentManagerSettings {
    pub settings: IvyJson,
    pub data_root: JsString,
    pub host_id: JsString,
    pub hive_url: JsString,
    pub credential: JsString,
    /// Already loaded/validated project-owned defaults.
    pub defaults: IvyJson,
    pub english_hint_comparator: Arc<EnglishHintComparator>,
    pub skills_comparator: Arc<SkillsComparator>,
}
#[derive(Clone)]
enum Mcp {
    Codex(McpManager),
    Claude(ClaudeMcpManager),
}
impl Mcp {
    fn synchronize(&self, connection: Arc<dyn RpcClient>) -> EnvironmentWait {
        match self {
            Self::Codex(manager) => {
                let run = manager.synchronize(connection);
                Box::pin(run.wait())
            }
            Self::Claude(manager) => {
                let run = manager.synchronize(connection);
                Box::pin(run.wait())
            }
        }
    }
    fn bootstrap(&self) -> ServiceFuture<()> {
        match self {
            Self::Codex(manager) => {
                let manager = manager.clone();
                Box::pin(async move { manager.bootstrap().await })
            }
            Self::Claude(manager) => {
                let run = manager.bootstrap();
                Box::pin(async move {
                    run.wait().await;
                    Ok(())
                })
            }
        }
    }
    fn close(&self) -> ServiceFuture<()> {
        match self {
            Self::Codex(manager) => {
                let run = manager.close();
                Box::pin(async move {
                    run.wait().await;
                    Ok(())
                })
            }
            Self::Claude(manager) => {
                let run = manager.close();
                Box::pin(async move {
                    run.wait().await;
                    Ok(())
                })
            }
        }
    }
    fn status(&self) -> IvyJson {
        match self {
            Self::Codex(manager) => manager.status().json(),
            Self::Claude(manager) => manager.status().json(),
        }
    }
}
struct Managers {
    instructions: Arc<InstructionsManager>,
    mcp: Mcp,
    skills: SkillsManager,
}
impl Managers {
    fn synchronizers(self: &Arc<Self>) -> EnvironmentSynchronizers {
        let instructions = self.instructions.clone();
        let mcp = self.mcp.clone();
        let skills = self.skills.clone();
        EnvironmentSynchronizers {
            instructions: Arc::new(move |connection| {
                let run = instructions.synchronize(connection);
                Box::pin(run.wait())
            }),
            mcp: Arc::new(move |connection| mcp.synchronize(connection)),
            skills: Arc::new(move |connection| {
                let run = skills.synchronize(connection);
                Box::pin(run.wait())
            }),
        }
    }
    async fn bootstrap(&self) -> IvyResult<()> {
        let instructions = self.instructions.bootstrap();
        let mcp = self.mcp.bootstrap();
        let skills = self.skills.clone();
        // Promise.all rejects at the first failure while the other already
        // accepted bootstraps continue. Dropped JoinHandles do not abort them.
        let instructions = tokio::spawn(async move {
            instructions.wait().await;
            Ok(())
        });
        let mcp = tokio::spawn(mcp);
        let skills = tokio::spawn(async move { skills.bootstrap().await });
        tokio::try_join!(settle(instructions), settle(mcp), settle(skills))?;
        Ok(())
    }
}
async fn settle(handle: JoinHandle<IvyResult<()>>) -> IvyResult<()> {
    handle.await.map_err(|_| IvyError::internal())?
}
struct State {
    managers: Option<Arc<Managers>>,
    startup_at: f64,
}
struct Inner {
    settings: EnvironmentManagerSettings,
    state: Mutex<State>,
    now: Arc<InventoryClock>,
}
#[derive(Clone)]
pub struct ManagedEnvironment(Arc<Inner>);
pub struct EnvironmentStart(watch::Receiver<Option<IvyResult<()>>>);
impl EnvironmentStart {
    pub async fn wait(mut self) -> IvyResult<()> {
        loop {
            if let Some(result) = self.0.borrow_and_update().clone() {
                return result;
            }
            self.0.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}
pub struct EnvironmentStatus {
    pub instructions: IvyJson,
    pub mcp: IvyJson,
    pub skills: IvyJson,
}
impl EnvironmentStatus {
    pub fn json(self) -> IvyJson {
        IvyJson::object([
            ("instructions", self.instructions),
            (
                "environment",
                IvyJson::object([("mcp", self.mcp), ("skills", self.skills)]),
            ),
        ])
    }
}
pub struct EnvironmentShutdown {
    pub instructions: Arc<ShutdownStep>,
    pub mcp: Arc<ShutdownStep>,
    pub skills: Arc<ShutdownStep>,
}
impl ManagedEnvironment {
    pub fn new(settings: EnvironmentManagerSettings) -> Self {
        Self::with_clock(
            settings,
            Arc::new(|| chrono::Utc::now().timestamp_millis() as f64),
        )
    }
    pub fn with_clock(settings: EnvironmentManagerSettings, now: Arc<InventoryClock>) -> Self {
        Self(Arc::new(Inner {
            settings,
            now,
            state: Mutex::new(State {
                managers: None,
                startup_at: 0.0,
            }),
        }))
    }
    fn enabled(&self) -> bool {
        self.0.settings.settings.get("patcherEnabled") != Some(&IvyJson::Bool(false))
    }
    fn claude(&self) -> bool {
        self.0
            .settings
            .settings
            .get("appServer")
            .and_then(|s| s.get("mode"))
            == Some(&"claude-adapter".into())
    }
    fn managers(&self) -> IvyResult<Option<Arc<Managers>>> {
        Ok(self
            .0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .managers
            .clone())
    }
    fn create(&self, home: &JsString) -> IvyResult<Arc<Managers>> {
        let config = &self.0.settings;
        let style = Style::native();
        let skills_root = match config.settings.get("skillsRoot").filter(|v| !v.is_null()) {
            Some(value) => value
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?,
            None if self.claude() => node_path::join(
                style,
                &[
                    config.data_root.clone(),
                    "claude-config".into(),
                    "skills".into(),
                ],
            ),
            None => node_path::join(
                style,
                &[
                    node_path::dirname(style, home),
                    ".agents".into(),
                    "skills".into(),
                ],
            ),
        };
        let instructions = Arc::new(InstructionsManager::new(InstructionsSettings {
            home: node_path::filesystem_path(home),
            host_id: config.host_id.clone(),
            hive_url: config.hive_url.clone(),
            defaults: config.defaults.required("instructions")?.clone(),
            english_hint_comparator: config.english_hint_comparator.clone(),
        }));
        let mcp = if self.claude() {
            Mcp::Claude(ClaudeMcpManager::new(ClaudeMcpSettings {
                target: node_path::join(
                    style,
                    &[config.data_root.clone(), "claude-mcp.json".into()],
                ),
                host_id: config.host_id.clone(),
                hive_url: config.hive_url.clone(),
                credential: config.credential.clone(),
                defaults: config.defaults.required("mcp")?.clone(),
            })?)
        } else {
            Mcp::Codex(McpManager::new(McpSettings {
                home: node_path::filesystem_path(home),
                host_id: config.host_id.clone(),
                hive_url: config.hive_url.clone(),
                credential: config.credential.clone(),
                defaults: config.defaults.required("mcp")?.clone(),
            }))
        };
        let skills = SkillsManager::new(SkillsSettings {
            state_home: home.clone(),
            root: skills_root,
            host_id: config.host_id.clone(),
            hive_url: config.hive_url.clone(),
            defaults: config.defaults.required("skills")?.clone(),
            compare: config.skills_comparator.clone(),
        });
        Ok(Arc::new(Managers {
            instructions,
            mcp,
            skills,
        }))
    }
    /// Real native beforeLaunch callback: bootstrap every manager, then read
    /// authoritative overrides before allowing the native process to start.
    /// Accepted work continues if the native launch's observation is abandoned.
    pub fn before_launch(
        &self,
        home: JsString,
        bootstrap: Arc<dyn RpcClient>,
    ) -> IvyResult<EnvironmentStart> {
        let (sender, receiver) = watch::channel(None);
        if !self.enabled() {
            sender.send_replace(Some(Ok(())));
            return Ok(EnvironmentStart(receiver));
        }
        let managers = self.create(&home)?;
        self.0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .managers = Some(managers.clone());
        let owner = self.clone();
        tokio::spawn(async move {
            let result = async {
                managers.bootstrap().await?;
                managers.synchronizers().synchronize(bootstrap).await;
                if owner.claude() {
                    let status = managers.mcp.status();
                    if status.get("state") != Some(&IvyJson::from("applied"))
                        && status.get("state") != Some(&IvyJson::from("disabled"))
                    {
                        return Err(IvyError::new(
                            "native_environment_unavailable",
                            "Claude adapter MCP configuration was not applied.",
                        ));
                    }
                }
                owner
                    .0
                    .state
                    .lock()
                    .map_err(|_| IvyError::internal())?
                    .startup_at = (owner.0.now)();
                Ok(())
            }
            .await;
            sender.send_replace(Some(result));
        });
        Ok(EnvironmentStart(receiver))
    }
    /// Owners which did not invoke beforeLaunch attach lazily after launch,
    /// without an added bootstrap or a second manager trio.
    pub fn attach_after_launch(&self, home: Option<&JsString>) -> IvyResult<()> {
        if !self.enabled() || self.managers()?.is_some() {
            return Ok(());
        }
        if let Some(home) = home.filter(|home| !home.0.is_empty()) {
            let managers = self.create(home)?;
            self.0
                .state
                .lock()
                .map_err(|_| IvyError::internal())?
                .managers = Some(managers);
        }
        Ok(())
    }
    pub fn startup_at(&self) -> IvyResult<f64> {
        Ok(self
            .0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .startup_at)
    }
    pub fn synchronizers(&self) -> IvyResult<Option<EnvironmentSynchronizers>> {
        Ok(self.managers()?.map(|managers| managers.synchronizers()))
    }
    pub fn status(&self) -> IvyResult<Option<EnvironmentStatus>> {
        Ok(self.managers()?.map(|managers| EnvironmentStatus {
            instructions: managers.instructions.status().json(),
            mcp: managers.mcp.status(),
            skills: managers.skills.status().json(),
        }))
    }
    pub fn shutdown_steps(&self) -> EnvironmentShutdown {
        let instructions = self.clone();
        let mcp = self.clone();
        let skills = self.clone();
        EnvironmentShutdown {
            instructions: Arc::new(move || {
                let managers = instructions.managers();
                let run = managers
                    .as_ref()
                    .ok()
                    .and_then(|m| m.as_ref())
                    .map(|m| m.instructions.close());
                Box::pin(async move {
                    managers?;
                    if let Some(run) = run {
                        run.wait().await;
                    }
                    Ok(())
                })
            }),
            mcp: Arc::new(move || {
                let managers = mcp.managers();
                let run = managers
                    .as_ref()
                    .ok()
                    .and_then(|m| m.as_ref())
                    .map(|m| m.mcp.close());
                Box::pin(async move {
                    managers?;
                    if let Some(run) = run {
                        run.await?;
                    }
                    Ok(())
                })
            }),
            skills: Arc::new(move || {
                let managers = skills.managers();
                let run = managers
                    .as_ref()
                    .ok()
                    .and_then(|m| m.as_ref())
                    .map(|m| m.skills.close());
                Box::pin(async move {
                    managers?;
                    if let Some(run) = run {
                        run.wait().await;
                    }
                    Ok(())
                })
            }),
        }
    }
}
