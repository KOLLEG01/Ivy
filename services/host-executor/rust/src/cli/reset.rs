//! Assemble the existing Host reset phases, Hive clear owner and UI publisher.
use super::{Arguments, option};
use crate::reset::ResetClear;
use ivy_cli::reset_ui::{self, ResetUiBundle, ResetUiPublisher};
use ivy_contracts::{
    IvyError, IvyJson as J, IvyResult, JsString,
    hash::{digest, hash_json},
    published_contracts,
};
use ivy_host_runtime::{
    artifact::FileCompare,
    config,
    host_config::{self, HostContext},
    journal::HostJournal,
    layout::StorageContext,
    native_path,
    node_runtime::NodeRuntime,
    runtime_reset::{self, CoordinatedReset, ResetHost, ResetHostPhase, ResetPhaseArguments},
};
use ivy_sdk::clock::Clock;
use std::{path::PathBuf, sync::Arc, time::Duration};

fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field(value: &J, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn array(value: &J) -> IvyResult<&[J]> {
    value.as_array().ok_or_else(IvyError::internal)
}
fn settings(config: &J) -> IvyResult<J> {
    let values = array(config.required("instances")?)?
        .iter()
        .filter_map(|instance| {
            instance
                .get("settings")
                .and_then(|value| value.get("runtimeReset"))
        })
        .collect::<Vec<_>>();
    require(
        values.len() == 1,
        "invalid_arguments",
        "Runtime reset requires one configured coordinator and ui list.",
    )?;
    let value = values[0];
    require(
        value
            .get("coordinatorHostId")
            .and_then(J::as_js_string)
            .is_some()
            && value
                .get("uiIds")
                .and_then(J::as_array)
                .is_some_and(|ids| ids.iter().all(|id| id.as_js_string().is_some())),
        "invalid_arguments",
        "Runtime reset configuration is invalid.",
    )?;
    Ok(value.clone())
}

struct Reset {
    config: J,
    config_path: PathBuf,
    distribution: PathBuf,
    host: ResetHost,
    clear: ResetClear,
    storage: Arc<StorageContext>,
    node: Option<NodeRuntime>,
    clock: Arc<dyn Clock>,
}
impl Reset {
    async fn bundles(&self) -> IvyResult<Vec<ResetUiBundle>> {
        let settings = settings(&self.config)?;
        let ids = array(settings.required("uiIds")?)?
            .iter()
            .map(|id| id.as_js_string().cloned().ok_or_else(IvyError::internal))
            .collect::<IvyResult<Vec<_>>>()?;
        reset_ui::reset_ui_bundles(&self.distribution, &ids).await
    }
    fn contracts(bundles: &[ResetUiBundle]) -> IvyResult<Vec<J>> {
        let mut contracts = published_contracts::chat_contracts()?;
        contracts.extend(published_contracts::task_board_contracts()?);
        reset_ui::reset_bootstrap_contracts(bundles, &contracts)
    }
    async fn plan(&self) -> IvyResult<J> {
        runtime_reset::local_runtime_reset_plan(
            self.config.clone(),
            native_path::from_native(&self.config_path),
            self.distribution.clone(),
            self.storage.clone(),
        )
        .await
    }
    async fn phase(&self, name: &str, args: ResetPhaseArguments) -> IvyResult<J> {
        if matches!(name, "preview" | "clear") {
            let contracts = Self::contracts(&self.bundles().await?)?;
            let plan = self.plan().await?;
            if name == "preview" {
                return Ok(plan);
            }
            let id = args.reset_id.filter(|id| !id.0.is_empty());
            let epoch = args.runtime_epoch.filter(|id| !id.0.is_empty());
            require(
                id.is_some() && epoch.is_some(),
                "invalid_arguments",
                "Mutating runtime reset phases require reset and epoch identities.",
            )?;
            return self
                .clear
                .clear(self.config.clone(), id.unwrap(), epoch.unwrap(), contracts)
                .await;
        }
        let phase = match name {
            "prepare" => ResetHostPhase::Prepare,
            "complete" => ResetHostPhase::Complete,
            "bootstrap" => ResetHostPhase::Bootstrap,
            "release" => ResetHostPhase::Release,
            "confirm" => ResetHostPhase::Confirm,
            _ => {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "Unknown runtime reset phase.",
                ));
            }
        };
        self.host
            .run_phase(
                self.config.clone(),
                native_path::from_native(&self.config_path),
                phase,
                args,
                self.node.clone(),
                self.storage.clone(),
            )
            .await
    }
    async fn invoke(&self, host: J, name: &str, reset: &CoordinatedReset) -> IvyResult<()> {
        if host.get("hostId") == self.config.get("hostId") {
            self.phase(
                name,
                ResetPhaseArguments {
                    reset_id: Some(reset.reset_id.clone()),
                    runtime_epoch: Some(reset.runtime_epoch.clone()),
                    scope_roots: (name == "prepare").then(|| reset.scope_roots.clone()),
                    publisher_digest: (name == "prepare").then(|| reset.publisher_digest.clone()),
                },
            )
            .await?;
        } else {
            self.host
                .remote(
                    self.config.clone(),
                    host,
                    runtime_reset::reset_phase_arguments(&name.into(), reset),
                )
                .await?;
        }
        Ok(())
    }
    async fn restore_uis(
        &self,
        bundles: &[ResetUiBundle],
        credential: &JsString,
        reset: &CoordinatedReset,
    ) -> IvyResult<()> {
        let publisher = ResetUiPublisher::new(
            &field(&self.config, "publicBaseUrl")?,
            credential,
            &reset.runtime_epoch,
            &reset.reset_id,
            self.clock.as_ref(),
        )?;
        let end = self.clock.utc_millis() + 90_000;
        loop {
            match publisher.republish(bundles).await {
                Ok(_) => return Ok(()),
                Err(error) => {
                    if !matches!(
                        error.code.as_str(),
                        "outcome_unknown"
                            | "service_unavailable"
                            | "service_not_ready"
                            | "contract_not_found"
                    ) || self.clock.utc_millis() >= end
                    {
                        return Err(error);
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }
    async fn coordinated(&self, apply: bool, confirmation: Option<JsString>) -> IvyResult<J> {
        let settings = settings(&self.config)?;
        require(
            settings.get("coordinatorHostId") == self.config.get("hostId"),
            "access_denied",
            "Complete runtime reset must run on the configured coordinator host.",
        )?;
        let bundles = self.bundles().await?;
        Self::contracts(&bundles)?;
        let credential = reset_ui::reset_ui_credential(array(self.config.required("instances")?)?)?
            .ok_or_else(|| {
                IvyError::new(
                    "unauthenticated",
                    "Runtime reset needs one configured Hive credential to restore uis.",
                )
            })?;
        // The local planner admits the deployment host list before remote work.
        let local_plan = self.plan().await?;
        let selected = self.config.required("deployment")?;
        let hosts = array(selected.required("hosts")?)?;
        let local = hosts
            .iter()
            .find(|host| host.get("hostId") == self.config.get("hostId"))
            .ok_or_else(IvyError::internal)?;
        let mut previews = Vec::new();
        for host in hosts {
            previews.push(if host.get("hostId") == self.config.get("hostId") {
                local_plan.clone()
            } else {
                self.host
                    .remote(
                        self.config.clone(),
                        host.clone(),
                        vec!["--phase".into(), "preview".into()],
                    )
                    .await?
                    .ok_or_else(IvyError::internal)?
            });
        }
        let plans = runtime_reset::coordinated_runtime_reset_plans(&previews)?;
        let identity = selected.required("identity")?;
        if !apply {
            let uis = bundles
                .iter()
                .map(|bundle| {
                    Ok(J::object([
                        (
                            "uiId",
                            bundle
                                .definition
                                .required("metadata")?
                                .required("uiId")?
                                .clone(),
                        ),
                        (
                            "releaseId",
                            bundle.checked_bundle.release_id.as_str().into(),
                        ),
                    ]))
                })
                .collect::<IvyResult<Vec<_>>>()?;
            return Ok(J::object([
                ("schemaVersion", J::Number(1.0)),
                ("mode", "preview".into()),
                ("deploymentIdentity", identity.clone()),
                ("hosts", J::Array(plans)),
                ("uis", J::Array(uis)),
            ]));
        }
        require(
            confirmation.as_ref().map(|s| J::String(s.clone())).as_ref() == Some(identity),
            "invalid_arguments",
            "--confirm must exactly match the deployment identity.",
        )?;
        let plan = plans
            .iter()
            .find(|plan| plan.get("hostId") == self.config.get("hostId"))
            .ok_or_else(IvyError::internal)?;
        let previous = config::json_file(
            runtime_reset::reset_record_path(&self.config)?,
            config::CONFIGURATION_BYTES,
        )
        .await
        .ok();
        let retained =
            runtime_reset::retained_reset_identity(previous.as_ref(), &hash_json(plan)?.into());
        let reset = CoordinatedReset {
            reset_id: retained
                .as_ref()
                .map(|r| field(r, "resetId"))
                .transpose()?
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into()),
            runtime_epoch: retained
                .as_ref()
                .map(|r| field(r, "runtimeEpoch"))
                .transpose()?
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into()),
            scope_roots: runtime_reset::encode_scope_roots(
                &array(plan.required("scopeRootIds")?)?
                    .iter()
                    .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
                    .collect::<IvyResult<Vec<_>>>()?,
            )?,
            publisher_digest: digest(String::from_utf16_lossy(&credential.0)).into(),
        };
        let released = retained
            .as_ref()
            .and_then(|r| r.get("resumeFromReleased"))
            .and_then(J::as_bool)
            == Some(true);
        self.invoke(local.clone(), "prepare", &reset).await?;
        for host in hosts
            .iter()
            .filter(|host| host.get("hostId") != self.config.get("hostId"))
        {
            self.invoke(host.clone(), "prepare", &reset).await?;
        }
        if !released {
            for host in hosts {
                self.invoke(host.clone(), "clear", &reset).await?;
            }
        }
        for host in hosts {
            self.invoke(host.clone(), "complete", &reset).await?;
        }
        runtime_reset::bootstrap_reset_uis(
            hosts,
            |host| self.invoke(host, "bootstrap", &reset),
            |host| self.invoke(host, "prepare", &reset),
            self.restore_uis(&bundles, &credential, &reset),
        )
        .await?;
        let mut release_order = hosts
            .iter()
            .filter(|host| host.get("hostId") != self.config.get("hostId"))
            .cloned()
            .collect::<Vec<_>>();
        release_order.push(local.clone());
        runtime_reset::release_reset_hosts(
            &release_order,
            |host| self.invoke(host, "release", &reset),
            |host| self.invoke(host, "prepare", &reset),
            async { Ok(()) },
        )
        .await?;
        for host in &release_order {
            self.invoke(host.clone(), "confirm", &reset).await?;
        }
        Ok(J::object([
            ("schemaVersion", J::Number(1.0)),
            ("mode", "applied".into()),
            ("deploymentIdentity", identity.clone()),
            ("resetId", J::String(reset.reset_id)),
            ("runtimeEpoch", J::String(reset.runtime_epoch)),
            (
                "completedHosts",
                J::Array(
                    hosts
                        .iter()
                        .map(|host| host.required("hostId").cloned())
                        .collect::<IvyResult<Vec<_>>>()?,
                ),
            ),
        ]))
    }
}

pub(super) async fn run(
    args: &Arguments,
    config_path: PathBuf,
    distribution: PathBuf,
    job: PathBuf,
    node: Option<NodeRuntime>,
    compare: FileCompare,
    clock: Arc<dyn Clock>,
) -> IvyResult<J> {
    let config = host_config::host_config(config_path.clone()).await?;
    HostJournal::open(config.clone()).await?.close().await?;
    let account = HostContext::current()?;
    let storage = Arc::new(StorageContext {
        paths: account.paths,
        account_home: account.account_home,
        compare,
    });
    let reset = Reset {
        config,
        config_path,
        distribution: distribution.clone(),
        node,
        host: ResetHost::new(distribution.clone(), job.clone(), clock.clone())?,
        clear: ResetClear::new(distribution, job, storage.clone(), clock.clone())?,
        storage,
        clock,
    };
    if args.command == "runtime-reset-host" {
        reset
            .phase(
                &args.required("phase")?.to_utf8()?,
                ResetPhaseArguments {
                    reset_id: option(args, "reset-id"),
                    runtime_epoch: option(args, "runtime-epoch"),
                    scope_roots: option(args, "scope-roots"),
                    publisher_digest: option(args, "publisher-digest"),
                },
            )
            .await
    } else {
        reset
            .coordinated(
                args.values.get("apply").and_then(J::as_bool) == Some(true),
                option(args, "confirm"),
            )
            .await
    }
}
