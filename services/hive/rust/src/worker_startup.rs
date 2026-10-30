//! Compose the original pre-Ready domains without a runtime-provider fallback.
use super::*;
use crate::{
    browser_push_store::{BrowserPushStore, Crypto},
    kernel::{Options, StartupFailure, StartupOptions, StartupPorts},
    store::Credential,
};
use std::rc::Rc;

impl KernelWorker {
    /// Read storage settings from the admitted server configuration.
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
        let mut actions = (|| -> IvyResult<WorkerActions> {
            let clock = kernel.clock.clone();
            let push = BrowserPushStore::new(
                &kernel.parts.store,
                &kernel.options.public_base_url,
                crypto,
                Rc::new(move || clock() as i64),
            )?;
            let mut actions = WorkerActions::before_ready(&mut kernel, &credentials)?;
            actions.bind_browser_push(push);
            let pending = kernel.parts.store.metadata("storage_compaction_pending")?.unwrap_or_default();
            let prior = kernel.parts.registry.borrow().diagnostics(&kernel.parts.store, Some(&"current".into()))?
                .iter().any(|value| value.get("code") == Some(&"storage_compaction_pending".into())
                    && value.get("source") == Some(&"hive.storage".into()));
            if pending.is_empty() && !prior { return Ok(actions); }
            let now = (kernel.clock)();
            let at = ivy_sdk::clock::date_to_iso(now)?;
            let diagnostic = IvyJson::object([
                ("code", "storage_compaction_pending".into()), ("source", "hive.storage".into()),
                ("resource", IvyJson::object([] as [(&str, IvyJson); 0])),
                ("severity", if pending.is_empty() { "info" } else { "warning" }.into()),
                ("status", if pending.is_empty() { "resolved" } else { "current" }.into()),
                ("firstObservedAt", at.clone().into()), ("lastObservedAt", at.into()),
                ("message", if pending.is_empty() { "Incremental database storage reclamation is available.".to_owned() } else { pending }.into()),
            ]);
            kernel.parts.registry.borrow_mut().diagnostic(&mut kernel.parts.store, &diagnostic, kernel.runtime.as_ref(), now)?;
            Ok(actions)
        })()
        .map_err(|error| StartupFailure::retain(error, &kernel.parts.store))?;
        actions.initialize_announcements(&kernel)
            .map_err(|error| StartupFailure::retain(error, &kernel.parts.store))?;
        Ok(Self::new(kernel, actions))
    }
}
