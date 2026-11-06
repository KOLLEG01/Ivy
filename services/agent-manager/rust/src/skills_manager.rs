//! Coalesced managed-skills application and current-layout bootstrap lifetime.
pub use crate::file_work::FileRun as SkillsRun;
use crate::file_work::FileWork;
use crate::skills_files::{
    AppliedSkills, SkillsComparator, apply_skills, fs_path, regular_bytes, resolved_paths,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::sync::Arc;
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
struct Inner {
    settings: SkillsSettings,
    work: FileWork<SkillsStatus>,
}
#[derive(Clone)]
pub struct SkillsManager(Arc<Inner>);
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
            work: FileWork::new(status),
        }))
    }
    pub fn status(&self) -> SkillsStatus {
        self.0.work.status()
    }
    pub fn synchronize(&self, connection: Arc<dyn RpcClient>) -> SkillsRun {
        self.0.work.start(update, || {
            let owner = self.0.clone();
            async move {
                let settings = &owner.settings;
                apply_skills(
                    settings.state_home.clone(),
                    settings.root.clone(),
                    connection,
                    settings.defaults.clone(),
                    settings.host_id.clone(),
                    settings.hive_url.clone(),
                    settings.compare.clone(),
                )
                .await
            }
        })
    }
    pub fn close(&self) -> SkillsRun {
        self.0.work.close()
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
