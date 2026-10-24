//! Local command composition. The Host runtime owns admission to durable work,
//! preparation, lifecycle, maintenance and private data; this shell owns argv
//! and the existing CliOutput/exit contract.
mod reset;
use ivy_cli::{
    arguments::{self, Arguments},
    output::CliResult,
};
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, JsString};
use ivy_host_runtime::{
    actions::{Action, LocalAction},
    artifact, config,
    host_actions::{HostActions, ProgressSink, RestoreRuntime},
    layout, native_account, native_path, native_plan,
    node_runtime::NodeRuntime,
    portable_http::PortableHttp,
    prepare::PreparationRuntime,
    storage_retention::RetentionBindings,
};
use ivy_sdk::clock::SystemClock;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// A packaged CLI resolves its own distribution, including an immutable release
/// selected by an installation link. Cargo invocations use the checkout cwd.
fn distribution() -> IvyResult<PathBuf> {
    let executable = std::env::current_exe().map_err(|_| IvyError::internal())?;
    for parent in executable.ancestors().skip(1).take(3) {
        if parent.file_name().is_some_and(|name| name == "native")
            && parent
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "dist")
        {
            return parent
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .ok_or_else(IvyError::internal);
        }
    }
    std::env::current_dir().map_err(|_| IvyError::internal())
}

pub async fn command(args: &[JsString], progress: Option<ProgressSink>) -> CliResult {
    let mut accepted = None;
    let result = async {
        let args = arguments::parse(args)?;
        let environment =
            std::env::var_os("IVY_HOST_CONFIG").map(|value| native_path::filesystem_name(&value));
        let path = args.config_path(environment.as_ref(), || {
            layout::default_host_config_path(
                &native_account::environment(),
                native_path::Style::native(),
                &native_account::os_homedir().map_err(|_| IvyError::internal())?,
            )
        })?;
        let paths = native_path::Context::current().map_err(|_| IvyError::internal())?;
        let config_path = native_path::to_native(&paths.resolve(&[path]));
        let distribution = distribution()?;
        execute(&args, config_path, distribution, progress, &mut accepted).await
    }
    .await;
    result.unwrap_or_else(|error| CliResult::failure(&error, accepted.as_ref()))
}

fn option(args: &Arguments, key: &str) -> Option<JsString> {
    args.values.get(key).and_then(J::as_js_string).cloned()
}
fn absolute(value: &JsString) -> IvyResult<PathBuf> {
    let paths = native_path::Context::current().map_err(|_| IvyError::internal())?;
    Ok(native_path::to_native(&paths.resolve(&[value.clone()])))
}
fn output(code: impl Into<JsString>, ok: bool, data: J) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("ok", ok.into()),
        ("code", J::String(code.into())),
        ("data", data),
    ])
}
fn deployment(data: J) -> IvyResult<J> {
    let record = data.required("record")?;
    let phase = record.required("phase")?.as_str()?;
    let mut value = output(
        format!("deployment_{phase}"),
        !matches!(
            phase.as_str(),
            "failed" | "needs_attention" | "rolled_back" | "rolling_back"
        ),
        data.clone(),
    );
    value.insert("deploymentId", record.required("deploymentId")?.clone());
    Ok(value)
}

async fn execute(
    args: &Arguments,
    config_path: PathBuf,
    distribution: PathBuf,
    progress: Option<ProgressSink>,
    accepted: &mut Option<JsString>,
) -> IvyResult<CliResult> {
    // Restore admits its explicit backup/hash before reading a fresh target.
    if args.command == "restore" {
        args.required("backup")?;
        args.required("backup-hash")?;
    }
    let input = if args.command == "restore" {
        J::Null
    } else {
        config::cli_json_file(config_path.clone(), config::CONFIGURATION_BYTES).await?
    };
    let needs_node = matches!(
        args.command.as_str(),
        "prepare"
            | "compact-preparations"
            | "backup"
            | "install-bootstrap"
            | "inspect-bootstrap"
            | "update-bootstrap"
            | "runtime-reset"
            | "runtime-reset-host"
    );
    let node = if needs_node {
        let configured = input
            .get("executables")
            .and_then(|v| v.get("node"))
            .and_then(J::as_js_string)
            .map(native_path::to_native);
        NodeRuntime::discover_optional(configured.as_deref()).await?
    } else {
        None
    };
    let compare = artifact::native_file_compare()?;
    let clock = Arc::new(SystemClock);
    let job = native_plan::native_binary_path(&distribution, "ivy-job");
    let actions = HostActions::native(
        node.clone(),
        compare.clone(),
        distribution.clone(),
        job.clone(),
    )?;
    let value = match args.command.as_str() {
        "status" => match option(args, "deployment").filter(|value| !value.0.is_empty()) {
            Some(id) => deployment(actions.deployment(config_path, id).await?)?,
            None => output("host_status", true, actions.status(config_path).await?),
        },
        "prepare" => output(
            "candidate_prepared",
            true,
            actions
                .prepare(
                    config_path,
                    args.required("component")?.clone(),
                    args.required("source")?.clone(),
                    PreparationRuntime {
                        node,
                        compare,
                        http: PortableHttp::new(Default::default())?,
                    },
                )
                .await?,
        ),
        "preparations" => output(
            "preparation_inventory",
            true,
            actions.preparation_inventory(config_path).await?,
        ),
        "compact-preparations" => output(
            "preparation_compaction",
            true,
            actions.compact_preparations(config_path, node).await?,
        ),
        "collect-storage" => {
            let data = actions
                .collect_storage(config_path, Arc::new(RetentionBindings { node, clock }))
                .await?;
            let state = data.required("status")?.required("state")?.as_str()?;
            output(
                format!("storage_retention_{state}"),
                state == "succeeded",
                data,
            )
        }
        "install-bootstrap" => {
            actions
                .install_bootstrap(config_path, args.required("candidate")?.clone())
                .await?
        }
        "inspect-bootstrap" => output(
            "bootstrap_snapshot",
            true,
            actions.inspect_bootstrap(config_path).await?,
        ),
        "update-bootstrap" | "bootstrap-status" => {
            let data = if args.command == "update-bootstrap" {
                // Keep malformed request files at the same CLI error boundary.
                config::cli_json_file(
                    absolute(args.required("request")?)?,
                    config::CONFIGURATION_BYTES,
                )
                .await?;
                actions
                    .update_bootstrap(config_path, args.required("request")?.clone())
                    .await?
            } else {
                actions
                    .bootstrap_status(config_path, args.required("operation-id")?.clone())
                    .await?
            };
            let phase = data.required("phase")?.as_str()?;
            output(
                format!("bootstrap_{phase}"),
                phase != "needs_attention",
                data,
            )
        }
        "backup" => output(
            "backup_completed",
            true,
            actions
                .backup(
                    config_path,
                    absolute(args.required("destination")?)?,
                    Arc::new(crate::backup::native_backup_bindings(
                        node,
                        compare,
                        distribution,
                        clock,
                    )?),
                )
                .await?,
        ),
        "restore" => output(
            "restore_completed",
            true,
            actions
                .restore(
                    config_path,
                    absolute(args.required("backup")?)?,
                    args.required("backup-hash")?.clone(),
                    RestoreRuntime::from_configuration(compare)?,
                )
                .await?,
        ),
        "runtime-reset" | "runtime-reset-host" => {
            let data =
                reset::run(args, config_path, distribution, job, node, compare, clock).await?;
            let code = if args.command == "runtime-reset" {
                if args.values.get("apply").and_then(J::as_bool) == Some(true) {
                    "runtime_reset_applied".to_owned()
                } else {
                    "runtime_reset_preview".to_owned()
                }
            } else {
                format!("runtime_reset_host_{}", args.required("phase")?.to_utf8()?)
            };
            output(code, true, data)
        }
        command => {
            let action = match command {
                "deploy" => Action::Deploy,
                "restart" => Action::Restart,
                "enable" => Action::Enable,
                "disable" => Action::Disable,
                "rollback" => Action::Rollback,
                _ => return Err(IvyError::internal()),
            };
            let result = actions
                .lifecycle(
                    config_path,
                    LocalAction {
                        action,
                        instance_id: args.required("instance")?.clone(),
                        operation_id: args.required("operation-id")?.clone(),
                        candidate_id: option(args, "candidate"),
                        source: option(args, "source"),
                        target_build: option(args, "to"),
                    },
                    args.wait_ms,
                    progress,
                )
                .await;
            *accepted = result.deployment_id;
            deployment(result.entry?)?
        }
    };
    CliResult::success(value)
}
