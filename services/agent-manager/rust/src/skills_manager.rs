//! Coalesced managed-skills application and current-layout bootstrap lifetime.
use crate::skills_files::{
    AppliedSkills, SkillsComparator, apply_skills, fs_path, regular_bytes, resolved_paths,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
pub struct SkillsSettings {
    pub state_home: JsString,
    pub root: JsString,
    pub host_id: JsString,
    pub hive_url: JsString,
    pub defaults: IvyJson,
    pub compare: Arc<SkillsComparator>,
}
#[derive(Clone, Debug)]
pub struct SkillsStatus {
    pub state: &'static str,
    pub target: JsString,
    pub desired_version: Option<String>,
    pub applied_version: Option<String>,
    pub observed_at: String,
    pub code: Option<String>,
    pub bytes: usize,
}
impl SkillsStatus {
    pub fn json(&self) -> IvyJson {
        IvyJson::object([
            ("state", self.state.into()),
            ("target", IvyJson::String(self.target.clone())),
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
    status: SkillsStatus,
    running: Option<watch::Receiver<bool>>,
}
struct Inner {
    settings: SkillsSettings,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct SkillsManager(Arc<Inner>);
pub struct SkillsRun(Option<watch::Receiver<bool>>);
impl SkillsRun {
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
fn update(status: &mut SkillsStatus, result: IvyResult<AppliedSkills>) {
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
struct Completion {
    owner: Arc<Inner>,
    sender: Option<watch::Sender<bool>>,
}
impl Completion {
    fn finish(&mut self, result: IvyResult<AppliedSkills>) {
        let mut state = self.owner.state.lock().unwrap_or_else(|e| e.into_inner());
        update(&mut state.status, result);
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
impl SkillsManager {
    pub fn new(settings: SkillsSettings) -> Self {
        let status = SkillsStatus {
            state: "pending",
            target: settings.root.clone(),
            desired_version: None,
            applied_version: None,
            observed_at: now(),
            code: None,
            bytes: 0,
        };
        Self(Arc::new(Inner {
            settings,
            state: Mutex::new(State {
                status,
                running: None,
            }),
        }))
    }
    pub fn status(&self) -> SkillsStatus {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    pub fn synchronize(&self, connection: Arc<dyn RpcClient>) -> SkillsRun {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = &state.running {
            return SkillsRun(Some(running.clone()));
        }
        let (sender, receiver) = watch::channel(false);
        state.running = Some(receiver.clone());
        let owner = self.0.clone();
        tokio::spawn(async move {
            let mut completion = Completion {
                owner: owner.clone(),
                sender: Some(sender),
            };
            let settings = &owner.settings;
            let result = apply_skills(
                settings.state_home.clone(),
                settings.root.clone(),
                connection,
                settings.defaults.clone(),
                settings.host_id.clone(),
                settings.hive_url.clone(),
                settings.compare.clone(),
            )
            .await;
            completion.finish(result);
        });
        SkillsRun(Some(receiver))
    }
    pub fn close(&self) -> SkillsRun {
        SkillsRun(
            self.0
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .running
                .clone(),
        )
    }
    pub async fn bootstrap(&self) -> IvyResult<()> {
        let paths = resolved_paths(
            self.0.settings.state_home.clone(),
            self.0.settings.root.clone(),
        )
        .await?;
        let present = tokio::task::spawn_blocking(move || {
            regular_bytes(&fs_path(&paths.state_path), 4 * 1024 * 1024).map(|bytes| bytes.is_some())
        })
        .await
        .map_err(|_| IvyError::internal())??;
        // An explicit import creates this current state before normal startup;
        // the runtime never reads legacy state locations or v1 ownership.
        if !present {
            self.synchronize(Arc::new(Bootstrap)).wait().await;
        }
        Ok(())
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
