use super::*;
use crate::{
    node_runtime::NodeRuntime,
    portable_native_files::{NativeHelperLayout, component_native_helper},
};
use ivy_contracts::limits::MANAGEMENT_FRAME_BYTES;
use tokio::io::AsyncWriteExt;

/// Offline Hive's native owner is selected from the admitted candidate artifact.
/// The caller supplies the package layout; a missing payload has no fallback.
pub struct NativeOfflineHiveInspector {
    node: NodeRuntime,
    job_launcher: PathBuf,
    layout: NativeHelperLayout,
}
impl NativeOfflineHiveInspector {
    pub fn new(node: NodeRuntime, job_launcher: PathBuf, layout: NativeHelperLayout) -> Self {
        Self {
            node,
            job_launcher,
            layout,
        }
    }
}
impl OfflineHiveInspector for NativeOfflineHiveInspector {
    fn inspect(&self, request: OfflineInspection) -> ExecutorFuture<'_, IvyJson> {
        let node = self.node.clone();
        let job_launcher = self.job_launcher.clone();
        let layout = self.layout;
        Box::pin(async move {
            // Once accepted, the owner retains the deadline and child tree even
            // when the requesting activation waiter disappears.
            tokio::spawn(inspect(request, node, job_launcher, layout))
                .await
                .map_err(|_| IvyError::internal())?
        })
    }
}
async fn inspect(
    request: OfflineInspection,
    node: NodeRuntime,
    job_launcher: PathBuf,
    layout: NativeHelperLayout,
) -> IvyResult<IvyJson> {
    let admitted = config::instance_config(request.configuration_path, &node).await?;
    require(
        admitted.json().get("componentId") == Some(&"hive".into())
            && admitted.artifact_root() == request.artifact_root,
        "target_conflict",
        "Only the selected Hive artifact may inspect its own storage.",
    )?;
    let input = json::stringify(admitted.json(), MANAGEMENT_FRAME_BYTES)?;
    let contracts = json::stringify(&request.contracts, MANAGEMENT_FRAME_BYTES)?;
    let root = request.artifact_root;
    let selected = root.clone();
    let helper = tokio::task::spawn_blocking(move || {
        component_native_helper(
            &selected,
            if cfg!(windows) {
                "ivy-hive-inspect-storage.exe"
            } else {
                "ivy-hive-inspect-storage"
            },
            layout,
        )
    })
    .await
    .map_err(|_| IvyError::internal())??;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let command = process::Command {
        executable: "hive-inspector".into(),
        args: vec!["--contracts".into(), contracts.into()],
        cwd: None,
        timeout_ms: 10_000,
    };
    let mut child = process::spawn(
        &command,
        &root,
        &BTreeMap::from([("hive-inspector".into(), helper)]),
        LaunchContext {
            job_launcher: Some(job_launcher),
            ..Default::default()
        },
    )
    .await?;
    let completed = tokio::time::timeout_at(deadline, async {
        if let Some(mut stdin) = child.stdin.take() {
            // A rejected request may close stdin early. Its actual completion
            // remains authoritative; EPIPE never terminates the host owner.
            let _ = stdin.write_all(input.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }
        child.completion.wait().await
    })
    .await;
    let result = match completed {
        Ok(result) => result,
        Err(_) => {
            let _ = child.stop(Duration::from_millis(250)).await;
            return Err(unknown(
                "deadline_exceeded",
                "Command exceeded its finite deadline.",
            ));
        }
    };
    if !process::owned_process_tree_stopped(&child).await? {
        return Err(unknown(
            "outcome_unknown",
            "Offline Hive process retirement is not established.",
        ));
    }
    if result.exit_code != Some(0) || result.error_code.is_some() {
        // Keep the original Host runCommand failure contract. The owner emits
        // bounded redacted diagnostics; those are not a new public error API.
        return Err(unknown(
            result.error_code.as_deref().unwrap_or("command_failed"),
            "Command failed; bounded diagnostics record the outcome.",
        )
        .with_details(IvyJson::object([
            (
                "exitCode",
                result
                    .exit_code
                    .map_or(IvyJson::Null, |code| IvyJson::Number(code as f64)),
            ),
            (
                "signal",
                result.signal.clone().map_or(IvyJson::Null, IvyJson::from),
            ),
        ])));
    }
    let inspection = json::parse(&result.stdout, 65536).map_err(|_| IvyError::internal())?;
    validate_host("StorageInspection", &inspection)?;
    Ok(inspection)
}
