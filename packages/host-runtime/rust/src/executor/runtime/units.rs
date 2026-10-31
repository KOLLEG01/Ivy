use super::*;
use crate::{linux_service_identity, runtime_owner::stop::StopPolicy, systemd};

fn inactive(state: &SystemdState) -> bool {
    ["inactive", "failed"]
        .iter()
        .any(|v| state.active_state == JsString::from(*v))
}
fn absent() -> SystemdState {
    SystemdState {
        load_state: "not-found".into(),
        active_state: "inactive".into(),
        sub_state: "dead".into(),
        main_pid: None,
    }
}
impl NativeActivationRuntime {
    async fn unit_observation(
        &self,
        c: &OperationContext,
        target: Option<&IvyJson>,
        owner: &SystemdState,
        state: &str,
        code: Option<JsString>,
        message: &str,
        health: Option<IvyJson>,
        build: Option<IvyJson>,
    ) -> IvyResult<()> {
        self.observation(
            c,
            target,
            state,
            code,
            message.into(),
            health,
            build,
            Some(owner),
        )
        .await
    }
    async fn stop_unit(
        &self,
        c: &OperationContext,
        target: Option<&IvyJson>,
        handles: &mut Handles,
    ) -> IvyResult<Option<SystemdState>> {
        let unit = systemd::systemd_unit_name(&c.config, &c.instance_id.clone().into())?;
        let definition = self
            .systemd
            .read_unit(c.config.clone(), unit.clone().into())
            .await?;
        handles.unit = None;
        if definition.is_none() {
            return Ok(None);
        }
        let current = self
            .systemd
            .inspect_unit(c.config.clone(), unit.clone().into())
            .await?;
        if inactive(&current) {
            return Ok(Some(current));
        }
        let policy = match target {
            Some(t) => {
                StopPolicy::for_manifest(&c.journal.manifest(&text(t, "candidateId")?).await?)
            }
            None => StopPolicy::Deadline(Duration::from_secs(15)),
        };
        self.systemd
            .stop_unit(c.config.clone(), unit.into(), policy)
            .await
            .map(Some)
    }
    pub(super) async fn reconcile_unit(
        &self,
        c: &OperationContext,
        instance: &IvyJson,
        target: Option<&IvyJson>,
        allow_start: bool,
        handles: &mut Handles,
    ) -> IvyResult<()> {
        require(
            self.platform == "linux" && instance.get("engine") == Some(&"process".into()),
            "unsupported_runtime",
            "Only Linux process instances use the direct systemd owner.",
        )?;
        require(
            instance.get("componentId") != Some(&"host-executor".into()),
            "restart_required",
            "The host executor is replaced through the OS bootstrap handoff.",
        )?;
        let unit = systemd::systemd_unit_name(&c.config, &c.instance_id.clone().into())?;
        let Some(target) = target else {
            let stopped = self
                .stop_unit(c, None, handles)
                .await?
                .unwrap_or_else(absent);
            return self
                .unit_observation(c, None, &stopped, "idle", None, "", None, None)
                .await;
        };
        if target.get("desired") == Some(&"stopped".into()) {
            let stopped = self
                .stop_unit(c, Some(target), handles)
                .await?
                .unwrap_or_else(absent);
            return self
                .unit_observation(
                    c,
                    Some(target),
                    &stopped,
                    "stopped",
                    None,
                    "Systemd target is intentionally disabled.",
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
            "Systemd target configuration no longer belongs to its selected release.",
        )?;
        require(
            selected(&manifest, "entrypoint").is_some(),
            "invalid_arguments",
            "A Linux process candidate needs an explicit entrypoint.",
        )?;
        let resources = self.resource_scope(c).await?;
        let rendered = systemd::render_systemd_unit(
            &c.config,
            &c.instance_id.clone().into(),
            target,
            &candidate,
            &manifest,
            resources.as_ref(),
        )?;
        let definition = self
            .systemd
            .read_unit(c.config.clone(), unit.clone().into())
            .await?;
        if handles
            .unit
            .as_ref()
            .is_some_and(|h| h.target.get("revision") != target.get("revision"))
            && !allow_start
        {
            let observed = self
                .systemd
                .inspect_unit(c.config.clone(), unit.clone().into())
                .await?;
            return self
                .unit_observation(
                    c,
                    Some(target),
                    &observed,
                    "unknown",
                    Some("target_conflict".into()),
                    "A passive systemd observation cannot replace an active launch.",
                    None,
                    build,
                )
                .await;
        }
        if definition.as_ref() != Some(&rendered.content) {
            require(
                allow_start,
                "target_changed",
                "Systemd definition differs from the accepted target; explicit activation is required.",
            )?;
            if definition.is_some() {
                let current = self
                    .systemd
                    .inspect_unit(c.config.clone(), unit.clone().into())
                    .await?;
                if !inactive(&current) {
                    self.systemd
                        .stop_unit(
                            c.config.clone(),
                            unit.clone().into(),
                            StopPolicy::for_manifest(&manifest),
                        )
                        .await?;
                }
            }
            if let Some(user) = linux_service_identity::linux_process_user(
                instance
                    .required("componentId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
                "linux",
            ) {
                linux_service_identity::ensure_linux_process_identity(
                    IvyJson::object([
                        ("componentId", instance.required("componentId")?.clone()),
                        ("user", user.into()),
                        ("configPath", target.required("configPath")?.clone()),
                        ("artifactRoot", candidate.required("artifactRoot")?.clone()),
                        (
                            "nodeExecutable",
                            if crate::native_plan::is_native_plan(&manifest) {
                                crate::layout::path_json(&crate::paths::direct_native_file(
                                    &crate::config::path_value(
                                        candidate.required("artifactRoot")?,
                                    )?,
                                    &manifest
                                        .required("entrypoint")?
                                        .required("executable")?
                                        .as_str()?,
                                )?)
                            } else {
                                c.config.required("executables")?.required("node")?.clone()
                            },
                        ),
                    ]),
                    config.json().clone(),
                    self.bootstrap_root.clone(),
                )
                .await?;
            }
            self.systemd
                .replace_unit(c.config.clone(), unit.clone().into(), rendered.content)
                .await?;
            self.systemd.reload(c.config.clone()).await?;
            self.systemd
                .start_unit(c.config.clone(), unit.clone().into())
                .await?;
            handles.unit = Some(Unit {
                target: target.clone(),
                unready_since: c.clock.utc_millis() as f64,
                readiness_checked: false,
            });
        } else if handles
            .unit
            .as_ref()
            .is_none_or(|h| h.target.get("revision") != target.get("revision"))
        {
            handles.unit = Some(Unit {
                target: target.clone(),
                unready_since: c.clock.utc_millis() as f64,
                readiness_checked: false,
            });
        }
        let mut observed = self
            .systemd
            .inspect_unit(c.config.clone(), unit.clone().into())
            .await?;
        if inactive(&observed) {
            if !allow_start {
                return self
                    .unit_observation(
                        c,
                        Some(target),
                        &observed,
                        if observed.active_state == JsString::from("failed") {
                            "failed"
                        } else {
                            "exited"
                        },
                        Some("systemd_not_running".into()),
                        "Systemd owner state is observed without starting the unit.",
                        None,
                        build,
                    )
                    .await;
            }
            self.systemd
                .start_unit(c.config.clone(), unit.clone().into())
                .await?;
            observed = self
                .systemd
                .inspect_unit(c.config.clone(), unit.clone().into())
                .await?;
        }
        if observed.active_state != JsString::from("active")
            || !["running", "listening", "exited"]
                .iter()
                .any(|v| observed.sub_state == JsString::from(*v))
        {
            return self
                .unit_observation(
                    c,
                    Some(target),
                    &observed,
                    if observed.active_state == JsString::from("failed") {
                        "failed"
                    } else {
                        "starting"
                    },
                    Some("systemd_starting".into()),
                    "Systemd has not reported a running process yet.",
                    None,
                    build,
                )
                .await;
        }
        let mut health = None;
        let checked = async {
            health = Some(
                health::check_health_with_clock(&config, None, false, c.clock.as_ref()).await?,
            );
            require(
                health.as_ref().and_then(|h| h.get("launchId")) == target.get("revision"),
                "service_not_ready",
                "Health belongs to a previous systemd launch.",
            )?;
            let handle = handles.unit.as_mut().unwrap();
            if !handle.readiness_checked {
                self.run_command(
                    c,
                    field(&manifest, "readiness").required("command")?,
                    config.artifact_root().to_owned(),
                    env(target.required("configPath")?, target.get("revision"), None)?,
                )
                .await?;
                handle.readiness_checked = true;
            }
            Ok::<(), IvyError>(())
        }
        .await;
        match checked {
            Ok(()) => {
                self.unit_observation(c, Some(target), &observed, "ready", None, "", health, build)
                    .await?;
                handles.unit.as_mut().unwrap().unready_since = 0.0;
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
                        Some(&observed),
                    )
                    .await?;
                    return Err(error);
                }
                let handle = handles.unit.as_mut().unwrap();
                handle.unready_since =
                    readiness_failure_start(handle.unready_since, c.clock.utc_millis() as f64);
                if readiness_expired(
                    handle.unready_since,
                    timeout(&manifest, "readiness", 30000.0),
                    c.clock.utc_millis() as f64,
                ) {
                    self.stop_unit(c, Some(target), handles).await?;
                    observed.active_state = "inactive".into();
                    observed.sub_state = "dead".into();
                    observed.main_pid = None;
                    self.unit_observation(
                        c,
                        Some(target),
                        &observed,
                        "failed",
                        Some("readiness_deadline".into()),
                        "Systemd readiness deadline expired.",
                        health,
                        build,
                    )
                    .await?;
                } else {
                    self.unit_observation(
                        c,
                        Some(target),
                        &observed,
                        "starting",
                        Some("service_not_ready".into()),
                        "Systemd readiness is not complete.",
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
