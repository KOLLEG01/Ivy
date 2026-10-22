use super::*;
use crate::{accepted_configuration, target};

fn record(entry: &IvyJson) -> &IvyJson {
    field(entry, "record")
}
fn activation(entry: &IvyJson) -> &IvyJson {
    field(entry, "activation")
}
fn phase(entry: &IvyJson) -> IvyResult<String> {
    text(record(entry), "phase")
}
async fn advance(
    context: &OperationContext,
    entry: &IvyJson,
    next: &str,
    change: IvyJson,
) -> IvyResult<IvyJson> {
    context
        .journal
        .advance(
            &text(record(entry), "deploymentId")?,
            &phase(entry)?,
            next,
            change,
        )
        .await
}
async fn manifest_timeout(
    context: &OperationContext,
    candidate: &str,
    kind: &str,
    fallback: f64,
) -> IvyResult<f64> {
    let manifest = context.journal.manifest(candidate).await?;
    Ok(field(&manifest, kind)
        .get("timeoutMs")
        .and_then(IvyJson::as_f64)
        .unwrap_or(fallback))
}
fn candidate(entry: &IvyJson) -> IvyResult<String> {
    text(entry, "candidateId")
}
fn running(target: &IvyJson) -> bool {
    field(target, "desired") == &"running".into()
}
fn readiness(state: &str, message: JsString) -> IvyJson {
    IvyJson::object([
        ("state", state.into()),
        ("message", IvyJson::String(message)),
    ])
}

async fn installed(context: &OperationContext, target: &IvyJson) -> IvyResult<IvyJson> {
    let candidate = context
        .journal
        .candidate(&text(target, "candidateId")?)
        .await?;
    Ok(IvyJson::object([
        ("instanceId", context.instance_id.clone().into()),
        ("candidateId", target.required("candidateId")?.clone()),
        ("buildId", candidate.required("buildId")?.clone()),
        ("enabled", running(target).into()),
        ("installedAt", stamp(context.clock.as_ref())?),
    ]))
}
async fn materialize(
    context: &OperationContext,
    candidate: String,
    enabled: bool,
    basis: Option<IvyJson>,
) -> IvyResult<IvyJson> {
    Ok(target::materialize_target(
        context.journal.clone(),
        context.instance_id.clone().into(),
        candidate,
        enabled,
        basis,
        context.options.account_home.clone(),
    )
    .await?
    .target)
}

async fn rollback(
    context: &OperationContext,
    runtime: &dyn ActivationRuntime,
    mut entry: IvyJson,
) -> IvyResult<()> {
    let previous = selected(&entry, "previousCandidateId")
        .ok_or_else(|| {
            IvyError::new(
                "rollback_unavailable",
                "No previous candidate is retained for this deployment.",
            )
        })?
        .as_str()?;
    let mut target = context.journal.target(&context.instance_id).await?;
    if field(activation(&entry), "rollbackTargetRevision").is_null() {
        if target.as_ref().is_some_and(running) && !context.is_own_instance() {
            target = target.map(|t| target::stopped_target(&t));
            entry = advance(
                context,
                &entry,
                "rolling_back",
                IvyJson::object([("target", target.clone().unwrap())]),
            )
            .await?;
        }
        if !context.is_own_instance() {
            let timeout_candidate = target
                .as_ref()
                .map(|t| text(t, "candidateId"))
                .transpose()?
                .unwrap_or(candidate(&entry)?);
            let timeout =
                manifest_timeout(context, &timeout_candidate, "shutdown", 15_000.0).await?;
            wait_for(
                context,
                runtime,
                target.clone(),
                ObservationGoal::Drained,
                30_000.0 + timeout,
                false,
            )
            .await?;
        }
        let old = field(activation(&entry), "previousTarget");
        require(
            old.get("candidateId") == Some(&IvyJson::from(previous.clone()))
                && selected(activation(&entry), "previousConfigurationPath").is_some(),
            "rollback_unavailable",
            "The previous launch configuration is not retained for rollback.",
        )?;
        let path = config::path_value(activation(&entry).required("previousConfigurationPath")?)?;
        let host = context.config.clone();
        let basis = tokio::task::spawn_blocking(move || {
            accepted_configuration::read_accepted_instance_configuration(&host, &path)
        })
        .await
        .map_err(|_| IvyError::internal())??;
        runtime
            .compatibility(
                context,
                &previous,
                Some(basis.required("settings")?.clone()),
            )
            .await?;
        let next = materialize(
            context,
            previous.clone(),
            field(activation(&entry), "previousEnabled") == &true.into(),
            Some(basis),
        )
        .await?;
        entry = advance(
            context,
            &entry,
            "rolling_back",
            IvyJson::object([
                ("target", next.clone()),
                ("rollbackTargetRevision", next.required("revision")?.clone()),
            ]),
        )
        .await?;
        target = Some(next);
    }
    let target = target
        .filter(|t| t.get("candidateId") == Some(&IvyJson::from(previous)))
        .ok_or_else(|| {
            unknown(
                "outcome_unknown",
                "Rollback target no longer identifies its retained previous build.",
            )
        })?;
    let timeout = manifest_timeout(
        context,
        &text(&target, "candidateId")?,
        "readiness",
        30_000.0,
    )
    .await?;
    let observation = wait_for(
        context,
        runtime,
        Some(target.clone()),
        ObservationGoal::Desired(running(&target)),
        timeout + 15_000.0,
        running(&target),
    )
    .await?;
    advance(
        context,
        &entry,
        "rolled_back",
        IvyJson::object([
            (
                "observedBuild",
                field(field(&observation, "health"), "buildId").clone(),
            ),
            (
                "readiness",
                readiness(
                    "passed",
                    "Previous compatible binary restored; persistent data was retained.".into(),
                ),
            ),
            ("installed", installed(context, &target).await?),
        ]),
    )
    .await?;
    Ok(())
}

async fn drive(
    context: &OperationContext,
    runtime: &dyn ActivationRuntime,
    mut entry: IvyJson,
) -> IvyResult<()> {
    context
        .journal
        .use_configuration(context.config.clone())
        .await?;
    context.journal.instance(&context.instance_id).await?;
    if phase(&entry)? == "preparing" {
        let snapshot = selected(&entry, "sourceSnapshot").ok_or_else(|| {
            IvyError::new(
                "source_snapshot_required",
                "Accepted source input is missing.",
            )
        })?;
        let prepared = runtime
            .prepare(context, snapshot, &text(record(&entry), "componentId")?)
            .await?;
        entry = advance(
            context,
            &entry,
            "prepared",
            IvyJson::object([("candidateId", prepared.required("candidateId")?.clone())]),
        )
        .await?;
    }
    if phase(&entry)? == "prepared" {
        entry = advance(context, &entry, "checking", empty()).await?;
    }
    if phase(&entry)? == "checking" {
        let installed = context.journal.installed(&context.instance_id).await?;
        let current = context.journal.target(&context.instance_id).await?;
        require(
            installed
                .as_ref()
                .map(|i| field(i, "candidateId"))
                .unwrap_or(&IvyJson::Null)
                == field(&entry, "previousCandidateId"),
            "target_changed",
            "An earlier queued deployment changed the installed build.",
        )?;
        let current_id = current
            .as_ref()
            .map(|t| text(t, "candidateId"))
            .transpose()?;
        let installed_id = installed
            .as_ref()
            .map(|i| text(i, "candidateId"))
            .transpose()?;
        if replacement_needs_settled_observation(current_id.as_deref(), installed_id.as_deref()) {
            let timeout = manifest_timeout(
                context,
                current_id.as_deref().unwrap(),
                "readiness",
                30_000.0,
            )
            .await?;
            wait_for(
                context,
                runtime,
                current.clone(),
                ObservationGoal::Stopped,
                timeout + 15_000.0,
                false,
            )
            .await?;
        }
        if field(field(&entry, "request"), "action") != &"disable".into() {
            runtime
                .compatibility(context, &candidate(&entry)?, None)
                .await?;
        }
        require(
            json::canonical(current.as_ref().unwrap_or(&IvyJson::Null), usize::MAX)?
                == json::canonical(field(activation(&entry), "previousTarget"), usize::MAX)?,
            "target_changed",
            "Runtime target changed after this operation was accepted.",
        )?;
        require(
            !context.is_own_instance()
                || field(activation(&entry), "targetEnabled") == &true.into(),
            "invalid_arguments",
            "Use the OS bootstrap to take the host executor out of service.",
        )?;
        let change = match current.filter(|t| running(t) && !context.is_own_instance()) {
            Some(target) => IvyJson::object([("target", target::stopped_target(&target))]),
            None => empty(),
        };
        entry = advance(context, &entry, "draining", change).await?;
    }
    if phase(&entry)? == "draining" {
        let current = context.journal.target(&context.instance_id).await?;
        let selected = current
            .as_ref()
            .map(|t| text(t, "candidateId"))
            .transpose()?
            .unwrap_or(candidate(&entry)?);
        let timeout = match manifest_timeout(context, &selected, "shutdown", 15_000.0).await {
            Ok(value) => value,
            Err(error) if error.code == "invalid_arguments" => 15_000.0,
            Err(error) => return Err(error),
        };
        if !context.is_own_instance() {
            wait_for(
                context,
                runtime,
                current,
                ObservationGoal::Drained,
                30_000.0 + timeout,
                false,
            )
            .await?;
        }
        if field(field(&entry, "request"), "action") != &"disable".into() {
            runtime
                .compatibility(context, &candidate(&entry)?, None)
                .await?;
        }
        let next = materialize(
            context,
            candidate(&entry)?,
            field(activation(&entry), "targetEnabled") == &true.into(),
            None,
        )
        .await?;
        entry = advance(
            context,
            &entry,
            "activating",
            IvyJson::object([("target", next.clone())]),
        )
        .await?;
        if context.options.own_instance_id.is_none()
            && field(record(&entry), "componentId") == &"host-executor".into()
            && running(&next)
        {
            context.cancel.cancel();
            return Ok(());
        }
    }
    if phase(&entry)? == "activating" {
        let target = context.journal.target(&context.instance_id).await?;
        require(
            target.as_ref().and_then(|t| t.get("candidateId")) == entry.get("candidateId"),
            "outcome_unknown",
            "Activation target is not the accepted candidate.",
        )?;
        wait_for(
            context,
            runtime,
            target,
            ObservationGoal::Activation,
            45_000.0,
            true,
        )
        .await?;
        entry = advance(context, &entry, "verifying", empty()).await?;
    }
    if phase(&entry)? == "verifying" {
        let target = context
            .journal
            .target(&context.instance_id)
            .await?
            .filter(|t| t.get("candidateId") == entry.get("candidateId"))
            .ok_or_else(|| {
                unknown(
                    "outcome_unknown",
                    "Verification target is not the accepted candidate.",
                )
            })?;
        if running(&target) {
            wait_for(
                context,
                runtime,
                Some(target.clone()),
                ObservationGoal::ArtifactVerified,
                60_000.0,
                false,
            )
            .await?;
        }
        let timeout = manifest_timeout(context, &candidate(&entry)?, "readiness", 30_000.0).await?;
        let observation = wait_for(
            context,
            runtime,
            Some(target.clone()),
            ObservationGoal::Desired(running(&target)),
            timeout + 15_000.0,
            false,
        )
        .await?;
        let message = if running(&target) {
            "Actual launch, manifest readiness and current dependencies verified."
        } else {
            "Verified artifact installed; instance remains intentionally disabled."
        };
        entry = advance(
            context,
            &entry,
            "succeeded",
            IvyJson::object([
                (
                    "observedBuild",
                    field(field(&observation, "health"), "buildId").clone(),
                ),
                ("readiness", readiness("passed", message.into())),
                ("installed", installed(context, &target).await?),
            ]),
        )
        .await?;
    }
    if phase(&entry)? == "rolling_back" {
        rollback(context, runtime, entry).await?;
    }
    Ok(())
}

pub(super) async fn execute(
    context: &OperationContext,
    runtime: &dyn ActivationRuntime,
    original: IvyJson,
) -> IvyResult<()> {
    let id = text(record(&original), "deploymentId")?;
    let Err(failure) = drive(context, runtime, original).await else {
        return Ok(());
    };
    if context.cancel.is_cancelled() {
        return Ok(());
    }
    let mut entry = context.journal.get(&id).await?;
    if terminal(&phase(&entry)?) {
        return Ok(());
    }
    let mut error = failure.clone();
    let failed = || {
        IvyJson::object([
            ("errorCode", IvyJson::String(failure.code_js())),
            ("readiness", readiness("failed", failure.message_js())),
        ])
    };
    if ["preparing", "prepared", "checking"].contains(&phase(&entry)?.as_str()) {
        advance(context, &entry, "failed", failed()).await?;
        return Ok(());
    }
    if selected(&entry, "previousCandidateId").is_some() && phase(&entry)? != "rolling_back" {
        entry = advance(context, &entry, "rolling_back", failed()).await?;
        match rollback(context, runtime, entry).await {
            Ok(()) => return Ok(()),
            Err(_) if context.cancel.is_cancelled() => return Ok(()),
            Err(rollback_error) => error = rollback_error,
        }
    }
    entry = context.journal.get(&id).await?;
    let current = context.journal.target(&context.instance_id).await?;
    let mut change = IvyJson::object([
        ("errorCode", IvyJson::String(error.code_js())),
        (
            "readiness",
            readiness(
                "unknown",
                concat(&[
                    "Deployment failed: ".into(),
                    failure.code_js(),
                    ". Final failure: ".into(),
                    error.message_js(),
                    " The last target is requested stopped.".into(),
                ]),
            ),
        ),
    ]);
    if let Some(target) = current.filter(running) {
        change.insert("target", target::stopped_target(&target));
    }
    advance(context, &entry, "needs_attention", change).await?;
    Ok(())
}
