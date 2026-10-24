use super::*;
use crate::{artifact, native_catalog_compatibility, service_storage, storage_space, target};
use ivy_sdk::rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient};

impl NativeActivationRuntime {
    pub(super) async fn service_storage(
        &self,
        c: &OperationContext,
        plan: &IvyJson,
    ) -> IvyResult<()> {
        let target = c.journal.target(&c.instance_id).await?;
        let observations = c.journal.observations().await?;
        let observed = observations.get(&c.instance_id);
        let running = if let Some((target, observed)) =
            target.as_ref().zip(observed).filter(|(t, o)| {
                field(o, "state") == &"ready".into()
                    && o.get("targetRevision") == t.get("revision")
                    && o.get("candidateId") == t.get("candidateId")
                    && field(o, "health").get("launchId") == t.get("revision")
                    && field(field(o, "health"), "ready") == &true.into()
            }) {
            let candidate = c.journal.candidate(&text(target, "candidateId")?).await?;
            if field(observed, "health").get("buildId") == candidate.get("buildId") {
                let manifest = c.journal.manifest(&text(target, "candidateId")?).await?;
                manifest
                    .get("storage")
                    .filter(|s| {
                        s.get("minReadableFormat") == s.get("writeFormat")
                            && s.get("maxReadableFormat") == s.get("writeFormat")
                    })
                    .and_then(|s| s.get("writeFormat"))
                    .and_then(IvyJson::as_f64)
            } else {
                None
            }
        } else {
            None
        };
        service_storage::verify_service_storage(
            c.config.clone(),
            c.instance_id.clone().into(),
            plan.clone(),
            running,
        )
        .await
    }
    fn runtime_requirements(&self, manifest: &IvyJson) -> IvyResult<()> {
        artifact::verify_runtime_requirements(
            manifest,
            self.preparation
                .node
                .as_ref()
                .map(|node| node.version.as_str()),
            &self.platform,
            match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                other => other,
            },
        )
    }
    pub(super) async fn check_compatibility(
        &self,
        c: &OperationContext,
        id: &str,
        native_settings: Option<IvyJson>,
    ) -> IvyResult<()> {
        let candidate = c.journal.candidate(id).await?;
        let manifest = c.journal.manifest(id).await?;
        self.runtime_requirements(&manifest)?;
        self.service_storage(c, &manifest).await?;
        let entry = field(&manifest, "entrypoint")
            .get("args")
            .and_then(IvyJson::as_array)
            .and_then(|v| v.first())
            .and_then(IvyJson::as_js_string)
            .cloned();
        artifact::verify_launch_candidate_with_manifest(
            candidate.clone(),
            config::path_value(c.config.required("artifactRoot")?)?,
            entry,
            manifest.clone(),
        )
        .await?;
        let instance = c.journal.instance(&c.instance_id).await?;
        if manifest.get("componentId") == Some(&"agent-manager".into()) {
            let settings = match native_settings {
                Some(v) => v,
                None => crate::layout::agent_manager_account_settings(
                    instance.required("settings")?,
                    &c.options.account_home,
                    node_path::Style::native(),
                )?,
            };
            native_catalog_compatibility::verify_configured_native_catalog(
                &config::path_value(candidate.required("artifactRoot")?)?,
                &c.instance_id.clone().into(),
                &settings,
            )
            .await?;
        }
        if instance.get("engine") == Some(&"docker".into()) {
            self.docker
                .verify_container_image(
                    c.config.clone(),
                    instance.clone(),
                    candidate.clone(),
                    manifest.clone(),
                )
                .await?;
        }
        let root = config::path_value(candidate.required("artifactRoot")?)?;
        let commands = [
            command_value(manifest.required("entrypoint")?)?,
            command_value(field(&manifest, "readiness").required("command")?)?,
        ];
        let tools = executables(
            &c.config,
            &[&commands[0].executable, &commands[1].executable],
        )?;
        let command_root = root.clone();
        tokio::task::spawn_blocking(move || {
            for command in commands {
                process::resolve_command(&command, &command_root, &tools)?;
            }
            Ok::<(), IvyError>(())
        })
        .await
        .map_err(|_| IvyError::internal())??;
        if manifest.get("componentId") == Some(&"host-executor".into()) {
            let format = c.journal.storage_format().await?;
            let storage = field(&manifest, "storage");
            require(
                storage
                    .get("minReadableFormat")
                    .and_then(IvyJson::as_f64)
                    .is_some_and(|v| v <= format)
                    && storage
                        .get("maxReadableFormat")
                        .and_then(IvyJson::as_f64)
                        .is_some_and(|v| v >= format)
                    && storage.get("writeFormat").and_then(IvyJson::as_f64) == Some(format),
                "incompatible_storage",
                "Executor update must preserve the bootstrap-readable host journal format.",
            )?;
        }
        require(
            storage_space::available_storage_bytes(config::path_value(
                c.config.required("runtimeRoot")?,
            )?)
            .await?
                >= 256.0 * 1024.0 * 1024.0,
            "storage_pressure",
            "Insufficient free host space for guarded activation.",
        )?;
        let requirements = manifest.required("requirements")?;
        let contracts = IvyJson::Array(
            requirements
                .required("contracts")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(|r| {
                    let mut v = IvyJson::object([("key", field(r, "key").clone())]);
                    if let Some(scope) = r.get("readScope") {
                        v.insert("readScope", scope.clone());
                    }
                    v
                })
                .collect(),
        );
        let mut inspection = None;
        if manifest.get("kind") == Some(&"hive".into()) {
            let settings = instance.required("settings")?;
            validate_host("HiveSettings", settings)?;
            require(
                settings
                    .required("credentials")?
                    .as_array()
                    .is_some_and(|v| !v.is_empty()),
                "invalid_arguments",
                "Hive requires configured credentials.",
            )?;
            let observations = c.journal.observations().await?;
            let observed = observations.get(&c.instance_id);
            let current = c.journal.target(&c.instance_id).await?;
            if running_hive_can_be_inspected(observed, current.as_ref()) {
                let target = current.as_ref().unwrap();
                let live = config::json_file(
                    config::path_value(target.required("configPath")?)?,
                    config::CONFIGURATION_BYTES,
                )
                .await?;
                require(
                    live.get("instanceId") == Some(&c.instance_id.clone().into())
                        && live.get("hostId") == c.config.get("hostId"),
                    "configuration_changed",
                    "Live Hive configuration no longer belongs to its target.",
                )?;
                let endpoint = health::local_hive_endpoint(&live, instance.get("docker"))?;
                let token = field(&live, "settings")
                    .required("credentials")?
                    .as_array()
                    .and_then(|v| v.first())
                    .ok_or_else(IvyError::internal)?
                    .required("token")?
                    .as_str()?;
                let client = client(endpoint.as_str(), token)?;
                let status = client.request("system.status", empty(), options()).await?;
                require(
                    field(&status, "ready") == &true.into()
                        && status.get("buildId") == observed.and_then(|v| v.get("buildId")),
                    "service_not_ready",
                    "Live Hive identity does not match the runtime owner observation.",
                )?;
                inspection = Some(
                    client
                        .request(
                            "system.inspectStorage",
                            IvyJson::object([("contracts", contracts.clone())]),
                            options(),
                        )
                        .await?,
                );
            } else {
                // Linux has no runtime observation before its first target.
                // Only an uninstalled instance with no loaded or retained OS
                // owner may enter the normal candidate storage inspection.
                let first_install_stopped = if self.platform == "linux"
                    && instance.get("engine") == Some(&"process".into())
                    && observed.is_none()
                    && current.is_none()
                    && c.journal.installed(&c.instance_id).await?.is_none()
                {
                    let unit = crate::systemd::systemd_unit_name(
                        &c.config,
                        &c.instance_id.clone().into(),
                    )?;
                    let state = self
                        .systemd
                        .inspect_unit(c.config.clone(), unit.clone().into())
                        .await?;
                    state.load_state == JsString::from("not-found")
                        && state.active_state == JsString::from("inactive")
                        && state.sub_state == JsString::from("dead")
                        && state.main_pid.is_none()
                        && self
                            .systemd
                            .read_unit(c.config.clone(), unit.into())
                            .await?
                            .is_none()
                } else {
                    false
                };
                require(
                    first_install_stopped
                        || observed.is_some_and(|o| {
                            (fresh_observation(o, c.clock.utc_millis() as f64)
                                || stopped_target_is_conclusive(Some(o), current.as_ref()))
                                && ["idle", "stopped"]
                                    .iter()
                                    .any(|s| o.get("state") == Some(&IvyJson::from(*s)))
                                && field(o, "targetRevision")
                                    == current
                                        .as_ref()
                                        .map(|t| field(t, "revision"))
                                        .unwrap_or(&IvyJson::Null)
                        }),
                    "compatibility_unknown",
                    "Hive must be observed stopped before its own offline inspector opens storage.",
                )?;
                let staged = target::materialize_target(
                    c.journal.clone(),
                    c.instance_id.clone().into(),
                    id.into(),
                    false,
                    None,
                    c.options.account_home.clone(),
                )
                .await?;
                let value = self
                    .inspector
                    .inspect(OfflineInspection {
                        configuration_path: config::path_value(
                            staged.target.required("configPath")?,
                        )?,
                        artifact_root: root,
                        contracts: contracts.clone(),
                        host: c.config.clone(),
                    })
                    .await?;
                validate_host("StorageInspection", &value)?;
                inspection = Some(value);
            }
            let value = inspection.as_ref().unwrap();
            let storage = field(&manifest, "storage");
            let format = field(value, "format").as_f64();
            require(
                !storage.is_null()
                    && (value.get("exists") == Some(&false.into())
                        || value.get("exists") == Some(&true.into())
                            && format.is_some_and(|format| {
                                storage
                                    .get("minReadableFormat")
                                    .and_then(IvyJson::as_f64)
                                    .is_some_and(|min| format >= min)
                                    && storage
                                        .get("maxReadableFormat")
                                        .and_then(IvyJson::as_f64)
                                        .is_some_and(|max| format <= max)
                            })),
                "incompatible_storage",
                "Candidate cannot read the actual database format.",
            )?;
        } else if !field(requirements, "hiveProtocol").is_null()
            || contracts.as_array().is_some_and(|v| !v.is_empty())
        {
            let credential = instance
                .get("credential")
                .and_then(IvyJson::as_js_string)
                .filter(|value| !value.0.is_empty())
                .ok_or_else(|| {
                    IvyError::new(
                        "invalid_arguments",
                        "Hive compatibility requires the instance credential.",
                    )
                })?
                .to_utf8()?;
            let client: Arc<dyn RpcClient> =
                Arc::new(client(&text(&c.config, "publicBaseUrl")?, credential)?);
            let status = client.request("system.status", empty(), options()).await?;
            require(
                field(&status, "ready") == &true.into()
                    && status.get("hiveProtocol") == requirements.get("hiveProtocol"),
                "incompatible_protocol",
                "Current Hive protocol is unavailable or incompatible.",
            )?;
            inspection = Some(
                client
                    .request(
                        "system.inspectStorage",
                        IvyJson::object([("contracts", contracts.clone())]),
                        options(),
                    )
                    .await?,
            );
            if requirements.get("nativeCatalogs") == Some(&true.into()) {
                native_catalog_compatibility::verify_native_catalog_compatibility(
                    client,
                    &root,
                    manifest
                        .required("componentId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?,
                )
                .await?;
            }
        }
        if let Some(inspection) = inspection {
            for usage in inspection
                .required("contracts")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                let requirement = requirements
                    .required("contracts")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .find(|r| r.get("key") == usage.get("key"));
                require(
                    requirement.is_some_and(|r| {
                        usage
                            .get("versions")
                            .and_then(IvyJson::as_array)
                            .is_some_and(|versions| {
                                r.get("readVersions")
                                    .and_then(IvyJson::as_array)
                                    .is_some_and(|read| versions.iter().all(|v| read.contains(v)))
                            })
                    }),
                    "incompatible_contract",
                    "Candidate cannot read every stored current/historical version in its declared domain.",
                )?;
            }
        }
        Ok(())
    }
}
fn options() -> RequestOptions {
    RequestOptions {
        timeout: Duration::from_millis(5000),
        ..Default::default()
    }
}
fn client(endpoint: &str, credential: String) -> IvyResult<HiveClient> {
    HiveClient::new(
        endpoint,
        ClientOptions {
            credential: Some(credential),
            ..Default::default()
        },
    )
}
