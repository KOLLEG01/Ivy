use super::*;
use crate::artifact;
use futures_util::FutureExt;

impl NativeActivationRuntime {
    async fn container_observation(
        &self,
        c: &OperationContext,
        target: Option<&IvyJson>,
        state: &str,
        code: Option<JsString>,
        message: &str,
        health: Option<IvyJson>,
        build: Option<IvyJson>,
    ) -> IvyResult<()> {
        self.observation(c, target, state, code, message.into(), health, build, None)
            .await
    }
    async fn stop_container(
        &self,
        c: &OperationContext,
        target: Option<&IvyJson>,
        handles: &mut Handles,
    ) -> IvyResult<()> {
        if let Some(handle) = &handles.container {
            let result = handle
                .process
                .stop(timeout(&handle.manifest, "shutdown", 15000.0))
                .await?;
            handle.process.dispose();
            handles.container = None;
            if result.error_code.as_deref() == Some("outcome_unknown") {
                return Err(unknown(
                    "outcome_unknown",
                    "Docker did not establish a stopped container outcome.",
                ));
            }
            return Ok(());
        }
        let manifest = match target {
            Some(t) => Some(c.journal.manifest(&text(t, "candidateId")?).await?),
            None => None,
        };
        let stopped = self
            .docker
            .stop_container(
                c.config.clone(),
                c.instance_id.clone().into(),
                manifest
                    .as_ref()
                    .map(|m| timeout(m, "shutdown", 15000.0))
                    .unwrap_or(15000.0),
                None,
            )
            .await?;
        if stopped.as_ref().is_some_and(|s| {
            !["exited", "dead", "created"]
                .iter()
                .any(|v| s.get("state") == Some(&IvyJson::from(*v)))
        }) {
            return Err(unknown(
                "outcome_unknown",
                "Docker container stop remains unresolved.",
            ));
        }
        Ok(())
    }
    pub(super) async fn reconcile_container(
        &self,
        c: &OperationContext,
        instance: &IvyJson,
        target: Option<&IvyJson>,
        allow_start: bool,
        handles: &mut Handles,
    ) -> IvyResult<()> {
        require(
            instance.get("engine") == Some(&"docker".into()),
            "unsupported_runtime",
            "Only Docker instances use the direct Docker owner.",
        )?;
        let Some(target) = target else {
            self.stop_container(c, None, handles).await?;
            return self
                .container_observation(c, None, "idle", None, "", None, None)
                .await;
        };
        if field(target, "desired") == &"stopped".into() {
            self.stop_container(c, Some(target), handles).await?;
            return self
                .container_observation(
                    c,
                    Some(target),
                    "stopped",
                    None,
                    "Docker target is intentionally disabled.",
                    None,
                    Some(
                        c.journal
                            .candidate(&text(target, "candidateId")?)
                            .await?
                            .required("buildId")?
                            .clone(),
                    ),
                )
                .await;
        }
        if handles
            .container
            .as_ref()
            .is_some_and(|h| h.target.get("revision") != target.get("revision"))
            && allow_start
        {
            let old = handles.container.as_ref().unwrap().target.clone();
            self.stop_container(c, Some(&old), handles).await?;
        }
        if handles
            .container
            .as_ref()
            .is_some_and(|h| h.target.get("revision") != target.get("revision"))
        {
            return self
                .container_observation(
                    c,
                    Some(target),
                    "unknown",
                    Some("target_conflict".into()),
                    "A passive Docker observation cannot replace an active launch.",
                    None,
                    Some(
                        c.journal
                            .candidate(&text(target, "candidateId")?)
                            .await?
                            .required("buildId")?
                            .clone(),
                    ),
                )
                .await;
        }
        let config = self.instance(target).await?;
        let candidate = c.journal.candidate(&text(target, "candidateId")?).await?;
        let manifest = c.journal.manifest(&text(target, "candidateId")?).await?;
        let build = Some(candidate.required("buildId")?.clone());
        require(
            config.json().get("instanceId") == Some(&c.instance_id.clone().into())
                && config.json().get("hostId") == c.config.get("hostId")
                && config.json().get("artifactRoot") == candidate.get("artifactRoot"),
            "configuration_changed",
            "Docker target configuration no longer belongs to its selected release.",
        )?;
        if handles.container.is_none() && !allow_start {
            let Some(current) = self
                .docker
                .inspect_container(c.config.clone(), c.instance_id.clone().into())
                .await?
            else {
                return self
                    .container_observation(
                        c,
                        Some(target),
                        "failed",
                        Some("container_missing".into()),
                        "Docker owner has no container for the requested target.",
                        None,
                        build,
                    )
                    .await;
            };
            if current.get("candidateId") != candidate.get("candidateId")
                || current.get("launchId") != target.get("revision")
                || current.get("imageId") != candidate.get("dockerImage")
            {
                return self
                    .container_observation(
                        c,
                        Some(target),
                        "unknown",
                        Some("target_conflict".into()),
                        "Docker owner reports a different launch identity.",
                        None,
                        build,
                    )
                    .await;
            }
            let endpoint = health::local_hive_endpoint(config.json(), instance.get("docker"));
            match async {
                let endpoint = endpoint?;
                health::check_health_with_clock(
                    &config,
                    Some(endpoint.as_str()),
                    false,
                    c.clock.as_ref(),
                )
                .await
            }
            .await
            {
                Ok(health) => {
                    self.container_observation(
                        c,
                        Some(target),
                        if ["running", "restarting"]
                            .iter()
                            .any(|v| current.get("state") == Some(&IvyJson::from(*v)))
                        {
                            "ready"
                        } else {
                            "starting"
                        },
                        None,
                        "",
                        Some(health),
                        build,
                    )
                    .await
                }
                Err(error) => {
                    self.container_observation(
                        c,
                        Some(target),
                        if ["exited", "dead", "created"]
                            .iter()
                            .any(|v| current.get("state") == Some(&IvyJson::from(*v)))
                        {
                            "failed"
                        } else {
                            "starting"
                        },
                        Some(error.code_js()),
                        "Docker owner state is observed without starting or stopping it.",
                        None,
                        build,
                    )
                    .await
                }
            }?;
            return Ok(());
        }
        if handles.container.is_none() {
            self.container_observation(
                c,
                Some(target),
                "starting",
                Some("artifact_verification".into()),
                "Checking the selected Docker release.",
                None,
                build.clone(),
            )
            .await?;
            let entry = field(&manifest, "entrypoint")
                .get("args")
                .and_then(IvyJson::as_array)
                .and_then(|a| a.first())
                .and_then(IvyJson::as_js_string)
                .cloned();
            artifact::verify_launch_candidate_with_manifest(
                candidate.clone(),
                config::path_value(c.config.required("artifactRoot")?)?,
                entry,
                manifest.clone(),
            )
            .await?;
            let managed = self
                .docker
                .start_container(
                    c.config.clone(),
                    instance.clone(),
                    target.clone(),
                    config.json().clone(),
                    candidate.clone(),
                    manifest.clone(),
                    true,
                    None,
                )
                .await?;
            handles.container = Some(Container {
                target: target.clone(),
                process: managed,
                manifest: manifest.clone(),
                unready_since: c.clock.utc_millis() as f64,
                readiness_checked: false,
            });
            self.container_observation(
                c,
                Some(target),
                "starting",
                None,
                "Docker container accepted by the local executor.",
                None,
                build.clone(),
            )
            .await?;
        }
        let completed = handles
            .container
            .as_ref()
            .unwrap()
            .process
            .completion
            .wait()
            .now_or_never();
        if let Some(result) = completed {
            let result = result?;
            handles.container.take().unwrap().process.dispose();
            if result.error_code.as_deref() == Some("outcome_unknown") {
                self.container_observation(
                    c,
                    Some(target),
                    "unknown",
                    Some("outcome_unknown".into()),
                    "Docker container outcome is unknown; no replacement was started.",
                    None,
                    build,
                )
                .await?;
                return Err(unknown(
                    "outcome_unknown",
                    "Docker container outcome is unknown; reconcile the existing container.",
                ));
            }
            let failed = result.exit_code != Some(0) || result.error_code.is_some();
            return self
                .container_observation(
                    c,
                    Some(target),
                    if failed { "failed" } else { "exited" },
                    result
                        .error_code
                        .map(JsString::from)
                        .or_else(|| failed.then(|| "process_exited".into())),
                    if failed {
                        "Docker container exited before readiness."
                    } else {
                        "Docker container exited."
                    },
                    None,
                    build,
                )
                .await;
        }
        let mut health = None;
        let checked = async {
            let endpoint = health::local_hive_endpoint(config.json(), instance.get("docker"))?;
            health = Some(
                health::check_health_with_clock(
                    &config,
                    Some(endpoint.as_str()),
                    false,
                    c.clock.as_ref(),
                )
                .await?,
            );
            require(
                health.as_ref().and_then(|h| h.get("launchId")) == target.get("revision"),
                "service_not_ready",
                "Docker health belongs to a previous launch.",
            )?;
            let handle = handles.container.as_mut().unwrap();
            handle.process.verify().await?;
            if !handle.readiness_checked {
                self.run_command(
                    c,
                    field(&manifest, "readiness").required("command")?,
                    config.artifact_root().to_owned(),
                    env(
                        target.required("configPath")?,
                        target.get("revision"),
                        Some(endpoint.as_str()),
                    )?,
                )
                .await?;
                handle.readiness_checked = true;
            }
            Ok::<(), IvyError>(())
        }
        .await;
        match checked {
            Ok(()) => {
                self.container_observation(c, Some(target), "ready", None, "", health, build)
                    .await?;
                handles.container.as_mut().unwrap().unready_since = 0.0;
            }
            Err(error) => {
                if error.code == "outcome_unknown" {
                    self.observation(
                        c,
                        Some(target),
                        "unknown",
                        Some(error.code_js()),
                        error.message_js(),
                        health,
                        build,
                        None,
                    )
                    .await?;
                    return Err(error);
                }
                let limit = if instance.get("componentId") == Some(&"hive".into()) {
                    timeout(&manifest, "readiness", 30000.0).max(180000.0)
                } else {
                    timeout(&manifest, "readiness", 30000.0)
                };
                let handle = handles.container.as_mut().unwrap();
                handle.unready_since =
                    readiness_failure_start(handle.unready_since, c.clock.utc_millis() as f64);
                if readiness_expired(handle.unready_since, limit, c.clock.utc_millis() as f64) {
                    self.stop_container(c, Some(target), handles).await?;
                    self.container_observation(
                        c,
                        Some(target),
                        "failed",
                        Some("readiness_deadline".into()),
                        "Docker readiness deadline expired.",
                        health,
                        build,
                    )
                    .await?;
                } else {
                    self.container_observation(
                        c,
                        Some(target),
                        "starting",
                        Some("service_not_ready".into()),
                        "Docker readiness is not complete.",
                        health,
                        build,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}
