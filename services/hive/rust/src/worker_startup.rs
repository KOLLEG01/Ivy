//! Compose the original pre-Ready domains without a runtime-provider fallback.
use super::*;
use crate::{
    browser_push_store::{BrowserPushStore, Crypto},
    kernel::{Options, StartupFailure, StartupOptions, StartupPorts},
    store::Credential,
};
use std::rc::Rc;

impl KernelWorker {
    /// Consume the existing ServerOptions worker frame after typed configuration
    /// admission. Unknown server fields are ignored; dynamic JS object coercion
    /// and external configuration validation are not replaced here.
    pub fn open_worker_data(
        data: IvyJson,
        ports: StartupPorts,
        crypto: Rc<dyn Crypto>,
    ) -> Result<Self, StartupFailure> {
        let options = (|| {
            let text = |value: &IvyJson, key: &str| {
                value
                    .get(key)
                    .and_then(IvyJson::as_js_string)
                    .cloned()
                    .ok_or_else(IvyError::internal)
            };
            let optional_text = |key: &str| {
                data.get(key)
                    .filter(|value| !value.is_null())
                    .map(|value| value.as_js_string().cloned().ok_or_else(IvyError::internal))
                    .transpose()
            };
            let validate_messages = match data.get("validateMessages") {
                None | Some(IvyJson::Null) => false,
                Some(IvyJson::Bool(value)) => *value,
                _ => return Err(IvyError::internal()),
            };
            Ok(StartupOptions {
                filename: text(&data, "filename")?,
                ui_root: optional_text("uiRoot")?,
                credentials: data
                    .get("credentials")
                    .and_then(IvyJson::as_array)
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .map(|value| {
                        Ok(Credential {
                            principal_id: text(value, "principalId")?,
                            digest: text(value, "digest")?,
                        })
                    })
                    .collect::<IvyResult<Vec<_>>>()?,
                kernel: Options {
                    version: text(&data, "version")?,
                    build_id: text(&data, "buildId")?,
                    public_base_url: text(&data, "publicBaseUrl")?,
                    validate_messages,
                    reset_bootstrap_credential_digest: optional_text(
                        "resetBootstrapCredentialDigest",
                    )?,
                },
            })
        })()
        .map_err(StartupFailure::unowned)?;
        Self::open(options, ports, crypto)
    }

    pub fn open(
        options: StartupOptions,
        ports: StartupPorts,
        crypto: Rc<dyn Crypto>,
    ) -> Result<Self, StartupFailure> {
        let credentials = options.credentials.clone();
        let mut kernel = Kernel::open_retained(options, ports)?;
        let actions = (|| -> IvyResult<WorkerActions> {
            let push = BrowserPushStore::new(
                &kernel.parts.store,
                &kernel.options.public_base_url,
                crypto,
                kernel.clock.clone(),
            )?;
            let mut actions = WorkerActions::before_ready(&mut kernel, &credentials)?;
            actions.bind_browser_push(push);
            Ok(actions)
        })()
        .map_err(|error| StartupFailure::retain(error, &kernel.parts.store))?;
        let mut worker = Self::new(kernel, actions);
        worker.initialize_announcements = true;
        Ok(worker)
    }
}
