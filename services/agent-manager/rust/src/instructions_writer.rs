//! Coalesced instruction output work; source retrieval/bootstrap belongs to its caller.
use crate::instructions_files::{AppliedInstructions, InstructionsSource, apply_instructions};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

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
struct State {
    status: InstructionsStatus,
    running: Option<watch::Receiver<bool>>,
}
struct Inner {
    home: PathBuf,
    host_id: JsString,
    hive_url: JsString,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct InstructionsWriter {
    inner: Arc<Inner>,
}
pub struct InstructionsRun {
    complete: Option<watch::Receiver<bool>>,
}
impl InstructionsRun {
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
struct Completion {
    inner: Arc<Inner>,
    sender: Option<watch::Sender<bool>>,
}
impl Completion {
    fn finish(&mut self, result: IvyResult<AppliedInstructions>) {
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
                state: Mutex::new(State {
                    status,
                    running: None,
                }),
            }),
        }
    }
    pub fn status(&self) -> InstructionsStatus {
        self.inner.state.lock().unwrap().status.clone()
    }
    /// One writer-owned operation. A joining caller never replaces its source;
    /// dropping any wait handle leaves the locked operation running.
    pub fn synchronize(&self, source: Arc<dyn InstructionsSource>) -> InstructionsRun {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(receiver) = &state.running {
            return InstructionsRun {
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
            let progress = |version: &str| {
                let mut state = inner.state.lock().unwrap();
                state.status.desired_version = Some(version.to_owned());
                state.status.observed_at = now();
            };
            let result = apply_instructions(
                inner.home.clone(),
                &inner.host_id,
                &inner.hive_url,
                source.as_ref(),
                &progress,
            )
            .await;
            completion.finish(result);
        });
        InstructionsRun {
            complete: Some(receiver),
        }
    }
    pub fn close(&self) -> InstructionsRun {
        InstructionsRun {
            complete: self.inner.state.lock().unwrap().running.clone(),
        }
    }
}
