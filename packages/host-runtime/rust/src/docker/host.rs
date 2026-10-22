use super::DockerHost;
use crate::{
    IvyError, IvyJson, IvyResult, config_file, linux_resources, node_path,
    process::{self, ProcessResult},
};
use futures_util::future::BoxFuture;
use ivy_contracts::JsString;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

pub struct NativeDockerHost {
    bootstrap_root: PathBuf,
    paths: node_path::Context,
}
impl NativeDockerHost {
    pub fn new(bootstrap_root: PathBuf) -> IvyResult<Self> {
        Ok(Self {
            bootstrap_root,
            paths: node_path::Context::current().map_err(|_| IvyError::internal())?,
        })
    }
}
impl DockerHost for NativeDockerHost {
    fn platform(&self) -> &str {
        if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(windows) {
            "win32"
        } else {
            "other"
        }
    }
    fn paths(&self) -> &node_path::Context {
        &self.paths
    }
    fn mkdir(&self, path: JsString) -> BoxFuture<'static, IvyResult<()>> {
        Box::pin(async move {
            tokio::fs::create_dir_all(node_path::filesystem_path(&path))
                .await
                .map_err(|_| IvyError::internal())
        })
    }
    fn configuration(&self, path: JsString) -> BoxFuture<'static, IvyResult<IvyJson>> {
        Box::pin(async move {
            config_file::json_file(
                node_path::to_native(&path),
                config_file::CONFIGURATION_BYTES,
            )
            .await
        })
    }
    fn verify_resources(
        &self,
        config: IvyJson,
        instance: JsString,
        scope: IvyJson,
    ) -> BoxFuture<'static, IvyResult<()>> {
        let root = self.bootstrap_root.clone();
        Box::pin(linux_resources::verify_linux_resource_membership(
            config, instance, scope, root,
        ))
    }
    fn execute(
        &self,
        config: IvyJson,
        args: Vec<JsString>,
        timeout_ms: f64,
        redact: Vec<JsString>,
    ) -> BoxFuture<'static, IvyResult<ProcessResult>> {
        let helper = crate::native_plan::native_binary_path(&self.bootstrap_root, "ivy-job");
        Box::pin(async move {
            let root = config_file::path_value(config.required("runtimeRoot")?)?;
            let executables = config
                .required("executables")?
                .as_object()
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(|(key, value)| Ok((key.to_utf8()?, config_file::path_value(value)?)))
                .collect::<IvyResult<BTreeMap<_, _>>>()?;
            // Node timers truncate finite delays and use 1ms outside their range.
            let timeout_ms =
                if !timeout_ms.is_finite() || !(1.0..2147483648.0).contains(&timeout_ms) {
                    1
                } else {
                    timeout_ms as u64
                };
            let command = process::Command {
                executable: "docker".into(),
                args: args
                    .iter()
                    .map(|v| node_path::to_native(v).into_os_string())
                    .collect(),
                cwd: None,
                timeout_ms,
            };
            process::run_command(
                &command,
                &root,
                &executables,
                process::LaunchContext {
                    job_launcher: Some(helper),
                    redact,
                    ..Default::default()
                },
                None,
            )
            .await
            .map(|v| v.as_ref().clone())
        })
    }
    fn sleep(&self) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(1000)))
    }
}
