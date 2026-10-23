use super::{Bindings, Startup};
use ivy_contracts::{IvyError, IvyResult, JsString};
use ivy_host_runtime::{
    bootstrap_restart::{BootstrapConfigurationRestarts, RestartContext},
    configuration_updater::update_credential,
    executor::{ExecutorOptions, NativeOfflineHiveInspector, PackageUpdateRuntime},
    package_updater::PackageClient,
    portable_native_files::NativeHelperLayout,
    prepare::PreparationRuntime,
};
use ivy_sdk::clock::SystemClock;
use std::{path::PathBuf, sync::Arc};

impl Startup {
    pub(super) fn identity(&self) -> IvyResult<(Option<String>, Option<JsString>)> {
        let own = self
            .own
            .as_ref()
            .map(|own| own.json().required("instanceId")?.as_str())
            .transpose()?;
        let credential = self
            .own
            .as_ref()
            .and_then(|own| own.json().get("credential"))
            .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
            .transpose()?;
        Ok((own, credential))
    }
}

impl Bindings {
    /// Assemble the native owners after startup admission and qualification of
    /// the caller's parser, locale and HTTP inputs. No transport default is chosen.
    /// Capture process_started_at before configuration admission, not here.
    pub fn native_for_startup(
        startup: &Startup,
        preparation: PreparationRuntime,
        distribution: PathBuf,
        job_launcher: PathBuf,
        mut options: ExecutorOptions,
        package_client: Option<Arc<dyn PackageClient>>,
        process_started_at: f64,
    ) -> IvyResult<Self> {
        let (own, credential) = startup.identity()?;
        options.own_instance_id = own;
        let updates = update_credential(
            &startup.host,
            options.own_instance_id.as_deref(),
            credential.as_ref(),
        )?;
        let packages = if updates.is_some_and(|value| !value.0.is_empty())
            && startup
                .host
                .get("packageUpdates")
                .is_some_and(|value| !value.is_null())
        {
            let client = package_client.ok_or_else(|| {
                IvyError::new(
                    "unsupported_runtime",
                    "Package updates require the qualified production runtime bindings.",
                )
            })?;
            Some(PackageUpdateRuntime::native(
                preparation.node.clone(),
                client,
                preparation.compare.clone(),
                distribution.clone(),
                job_launcher.clone(),
            )?)
        } else {
            None
        };
        let restarts = BootstrapConfigurationRestarts::new(
            RestartContext {
                own_instance_id: options.own_instance_id.as_deref().map(JsString::from),
                distribution: Some(distribution.clone()),
                process_started_at,
                platform: if cfg!(windows) {
                    "win32"
                } else {
                    std::env::consts::OS
                }
                .into(),
            },
            Arc::new(SystemClock),
            job_launcher.clone(),
        );
        let inspector = Arc::new(NativeOfflineHiveInspector::new(
            job_launcher.clone(),
            NativeHelperLayout::Target,
        ));
        Self::native(
            preparation,
            distribution,
            job_launcher,
            inspector,
            options,
            Some(restarts),
            packages,
        )
    }
}
