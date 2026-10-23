//! Desktop policy binding to the canonical Host process-tree owner.
use super::{
    desktop::{DesktopHost, DesktopInvocation, WhatsAppDesktop},
    journal_owner::Journal,
    messages::Translator,
};
use ivy_contracts::{IvyError, IvyResult};
use ivy_host_runtime::{
    exec_file::{self, SpawnCommand, SpawnOptions},
    portable_native_files::NativeHelperLayout,
    process,
};
use ivy_sdk::service::ServiceFuture;
use std::{path::PathBuf, sync::Arc, time::Duration};

impl WhatsAppDesktop {
    pub fn for_component(
        journal: Journal,
        admitted_artifact: PathBuf,
        layout: NativeHelperLayout,
        translator: Translator,
    ) -> IvyResult<Self> {
        Ok(Self::new(
            journal,
            Arc::new(NativeDesktopHost::for_component(admitted_artifact, layout)?),
            translator,
        ))
    }
}

#[derive(Clone)]
pub struct NativeDesktopHost {
    artifact_root: PathBuf,
    job_launcher: Option<PathBuf>,
}
impl NativeDesktopHost {
    /// Admission of the immutable component/build precedes this helper binding.
    /// A missing target helper never selects the legacy layout implicitly.
    pub fn for_component(
        admitted_artifact: PathBuf,
        layout: NativeHelperLayout,
    ) -> IvyResult<Self> {
        let options = SpawnOptions::for_component(&admitted_artifact, layout)?;
        Ok(Self {
            artifact_root: admitted_artifact,
            job_launcher: options.job_launcher,
        })
    }

    async fn run_owned(self, invocation: DesktopInvocation) -> IvyResult<()> {
        if !invocation.windows_hide || invocation.shell || !invocation.ignored_stdio {
            return Err(IvyError::internal());
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(invocation.timeout_ms);
        let child = exec_file::spawn_ignored(
            SpawnCommand {
                executable: invocation.executable.into(),
                // Encoding only: filesystem spelling/namespace conversion must
                // never rewrite the fixed encoded program or another argument.
                args: invocation
                    .arguments
                    .iter()
                    .map(|argument| argument.to_utf8().map(Into::into))
                    .collect::<IvyResult<Vec<_>>>()?,
                cwd: None,
            },
            self.artifact_root,
            SpawnOptions {
                environment: None,
                job_launcher: self.job_launcher,
            },
        )
        .await
        .map_err(|_| IvyError::internal())?;
        // Finish an in-flight Host launch before handling the deadline: dropping
        // its future cannot serve as evidence that no process was created.
        let completion = if tokio::time::Instant::now() >= deadline {
            None
        } else {
            tokio::time::timeout_at(deadline, child.completion.wait())
                .await
                .ok()
        };
        let timed_out = completion.is_none();
        let result = match completion {
            Some(result) => result,
            None => child
                .stop(Duration::ZERO)
                .await
                .map_err(|_| IvyError::internal())?,
        };
        let retired = process::owned_process_tree_stopped(&child)
            .await
            .map_err(|_| IvyError::internal())?;
        if timed_out
            || !retired
            || result.error_code.is_some()
            || result.exit_code != Some(0)
            || result.signal.is_some()
        {
            return Err(IvyError::internal());
        }
        Ok(())
    }
}
impl DesktopHost for NativeDesktopHost {
    fn platform(&self) -> &'static str {
        match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            platform => platform,
        }
    }

    fn run(&self, invocation: DesktopInvocation) -> ServiceFuture<()> {
        let owner = self.clone();
        // The spawned process remains owned if its waiting caller is dropped.
        // In normal composition WhatsAppDesktop invokes this after its durable
        // attempt fence and retains the completion that publishes the result.
        let completion = tokio::spawn(owner.run_owned(invocation));
        Box::pin(async move { completion.await.map_err(|_| IvyError::internal())? })
    }
}
