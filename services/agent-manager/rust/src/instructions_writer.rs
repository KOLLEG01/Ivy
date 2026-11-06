//! Coalesced instruction output work; source retrieval/bootstrap belongs to its caller.
pub use crate::file_work::FileRun as InstructionsRun;
use crate::{
    file_work::FileWork,
    instructions_files::{AppliedInstructions, InstructionsSource, apply_instructions},
};
use ivy_contracts::{IvyJson, IvyResult, JsString};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone, Debug)]
pub struct InstructionsStatus {
    pub state: &'static str,
    pub home: PathBuf,
    pub desired_version: Option<String>,
    pub applied_version: IvyJson,
    pub observed_at: String,
    pub code: Option<String>,
    pub bytes: usize,
}
impl InstructionsStatus {
    pub fn json(&self) -> IvyJson {
        IvyJson::object([
            ("state", self.state.into()),
            (
                "home",
                IvyJson::String(ivy_host_runtime::node_path::from_native(&self.home)),
            ),
            (
                "desiredVersion",
                self.desired_version
                    .clone()
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null),
            ),
            ("appliedVersion", self.applied_version.clone()),
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
    home: PathBuf,
    host_id: JsString,
    hive_url: JsString,
    work: FileWork<InstructionsStatus>,
}
#[derive(Clone)]
pub struct InstructionsWriter {
    inner: Arc<Inner>,
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn update(status: &mut InstructionsStatus, result: IvyResult<AppliedInstructions>) {
    match result {
        Ok(AppliedInstructions::Unmanaged { desired_version }) => {
            status.state = "disabled";
            status.desired_version = Some(desired_version);
            status.code = None;
        }
        Ok(AppliedInstructions::Managed {
            home,
            desired_version,
            applied_version,
            active,
            bytes,
        }) => {
            status.state = if active { "applied" } else { "disabled" };
            status.home = home;
            status.desired_version = Some(desired_version);
            status.applied_version = applied_version;
            status.code = None;
            status.bytes = bytes;
        }
        Err(error) => {
            status.state = if ["target_conflict", "limit_exceeded", "contract_mismatch"]
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
impl InstructionsWriter {
    pub fn new(home: PathBuf, host_id: JsString, hive_url: JsString) -> Self {
        let status = InstructionsStatus {
            state: "pending",
            home: home.clone(),
            desired_version: None,
            applied_version: IvyJson::Null,
            observed_at: now(),
            code: None,
            bytes: 0,
        };
        Self {
            inner: Arc::new(Inner {
                home,
                host_id,
                hive_url,
                work: FileWork::new(status),
            }),
        }
    }
    pub fn status(&self) -> InstructionsStatus {
        self.inner.work.status()
    }
    /// One writer-owned operation. A joining caller never replaces its source;
    /// dropping any wait handle leaves the locked operation running.
    pub fn synchronize(&self, source: Arc<dyn InstructionsSource>) -> InstructionsRun {
        self.inner.work.start(update, || {
            let inner = self.inner.clone();
            async move {
                let progress = |version: &str| {
                    inner.work.progress(|status| {
                        status.desired_version = Some(version.to_owned());
                        status.observed_at = now();
                    });
                };
                apply_instructions(
                    inner.home.clone(),
                    &inner.host_id,
                    &inner.hive_url,
                    source.as_ref(),
                    &progress,
                )
                .await
            }
        })
    }
    pub fn close(&self) -> InstructionsRun {
        self.inner.work.close()
    }
}
